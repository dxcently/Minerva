//! The loop (triage-toolkit.md §7–§8):
//!
//!   reset → boot → wait_ready → baseline read → agent works within the time box
//!   → agent stopped → authoritative read → VM down → one Row.
//!
//! The authoritative points are read over the console *after* the agent is
//! stopped (§7.4.1), so a guest the agent locked itself out of is still scored.

use crate::agent::{AgentRunner, RunCtx};
use crate::clock::Clock;
use crate::config::BenchConfig;
use crate::report::{Row, Timeline};
use crate::score::Score;
use crate::vm::{ScoreReader, VmControl};
use std::io;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    Solved,
    TimeBox,
}

impl StopReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            StopReason::Solved => "solved",
            StopReason::TimeBox => "time_box",
        }
    }
}

pub fn run_benchmark(
    cfg: &BenchConfig,
    vm: &dyn VmControl,
    scorer: &dyn ScoreReader,
    agent: &dyn AgentRunner,
    clock: &dyn Clock,
) -> io::Result<Row> {
    // A previous run that failed part-way leaves its guest up, and a reset
    // under a live qemu would pull its overlay out from under it: stop first
    // (a no-op when nothing runs).
    vm.stop()?;
    vm.reset()?;
    vm.boot()?;
    vm.wait_ready(clock, cfg.boot_timeout_s)?;

    let mut timeline = Timeline::new();
    let baseline = read_until_some(scorer, clock, cfg)?;
    timeline.push(clock.elapsed_s(), baseline.earned);

    let ctx = RunCtx {
        ssh_host: "127.0.0.1".into(),
        ssh_port: 2222,
        brain_model: cfg.brain_model.clone(),
        blind: cfg.feedback_blind,
    };

    agent.start(&ctx)?;
    let reason = work_loop(cfg, scorer, agent, clock, &ctx, &mut timeline)?;
    agent.stop()?;

    // Authoritative, after the agent can no longer touch the guest, and after
    // the scorer has had time to rescore the agent's last change.
    clock.sleep_s(cfg.score_settle_s);
    let (final_score, score_source) = match scorer.read()? {
        Some(s) => (s, "read"),
        None => (
            Score { earned: timeline.peak(), total: cfg.score_total, penalties: None },
            "peak_fallback",
        ),
    };
    timeline.push(clock.elapsed_s(), final_score.earned);
    vm.stop()?;

    Ok(Row {
        image: cfg.image.clone(),
        sha: cfg.image_sha.clone(),
        // The model the agent actually drove, not the configured default: a
        // Noop baseline calls nothing and must not claim `brain_model`.
        model: agent.model().unwrap_or_else(|| "none".into()),
        graphs_snapshot: cfg.graphs_snapshot.clone(),
        mode: cfg.mode().into(),
        points: final_score.earned,
        total: final_score.total,
        peak_points: timeline.peak(),
        forensics_k: 0,
        forensics_of: 7,
        // Unknown on a peak fallback (that reading carried no penalties token).
        penalties: final_score.penalties.unwrap_or(0),
        escalations_by_tier: [0; 4],
        leak_hits: 0,
        tokens: agent.tokens(),
        wall_s: clock.elapsed_s(),
        stop_reason: reason,
        score_source,
        timeline,
    })
}

fn work_loop(
    cfg: &BenchConfig,
    scorer: &dyn ScoreReader,
    agent: &dyn AgentRunner,
    clock: &dyn Clock,
    ctx: &RunCtx,
    timeline: &mut Timeline,
) -> io::Result<StopReason> {
    loop {
        if clock.elapsed_s() >= cfg.time_box_s {
            return Ok(StopReason::TimeBox);
        }
        let _ = agent.step(ctx)?;
        if let Some(s) = scorer.read()? {
            timeline.push(clock.elapsed_s(), s.earned);
            if s.solved() {
                return Ok(StopReason::Solved);
            }
        }
        clock.sleep_s(cfg.poll_interval_s);
    }
}

/// The first read after boot can come back before the scorer has run; retry
/// within the boot timeout rather than record a missing baseline.
fn read_until_some(scorer: &dyn ScoreReader, clock: &dyn Clock, cfg: &BenchConfig) -> io::Result<Score> {
    let deadline = clock.elapsed_s() + cfg.boot_timeout_s;
    loop {
        if let Some(s) = scorer.read()? {
            return Ok(s);
        }
        if clock.elapsed_s() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "scorer produced no reading"));
        }
        clock.sleep_s(cfg.poll_interval_s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::NoopRunner;
    use crate::clock::test_support::FakeClock;
    use crate::vm::test_support::FakeHarness;
    use crate::vm::Vm;

    fn cfg(time_box_s: u64) -> BenchConfig {
        let mut c = BenchConfig::default();
        c.score_cmd = vec!["score".into()];
        c.vm_cmd = vec!["vm".into()];
        c.time_box_s = time_box_s;
        c.poll_interval_s = 30;
        c
    }

    #[test]
    fn noop_agent_runs_to_the_time_box_at_baseline() {
        let c = cfg(300);
        let vm = Vm::new(FakeHarness::with_scores(&["0 / 256"]), &c);
        let clock = FakeClock::new();
        let row = run_benchmark(&c, &vm, &vm, &NoopRunner, &clock).unwrap();
        assert_eq!(row.stop_reason, StopReason::TimeBox);
        assert_eq!(row.points, 0);
        assert!(row.wall_s >= 300);
        // Honesty: a baseline run drove no model and read its final score.
        assert_eq!(row.model, "none");
        assert_eq!(row.score_source, "read");
    }

    #[test]
    fn a_final_read_that_fails_is_flagged_as_a_peak_fallback() {
        let c = cfg(60);
        // The score answers during the loop (so peak climbs) then goes silent,
        // so the authoritative read falls back to the high-water mark.
        let vm = Vm::new(FakeHarness::with_scores(&["10 / 256", ""]), &c);
        let clock = FakeClock::new();
        let row = run_benchmark(&c, &vm, &vm, &NoopRunner, &clock).unwrap();
        assert_eq!(row.score_source, "peak_fallback");
        assert_eq!(row.points, row.peak_points);
        assert_eq!(row.penalties, 0); // unknown on a fallback, recorded as 0
    }

    #[test]
    fn stops_before_reset_and_settles_before_the_final_read() {
        let c = cfg(60);
        let vm = Vm::new(FakeHarness::with_scores(&["0 / 256"]), &c);
        let clock = FakeClock::new();
        let row = run_benchmark(&c, &vm, &vm, &NoopRunner, &clock).unwrap();
        let calls = vm.harness().calls.borrow().clone();
        assert_eq!(&calls[..3], &["vm down", "vm reset", "vm up"]);
        assert_eq!(calls.last().map(String::as_str), Some("vm down"));
        assert!(row.wall_s >= 60 + c.score_settle_s);
    }

    #[test]
    fn a_rising_score_reaches_solved_and_stops() {
        let c = cfg(7200);
        // Score climbs each poll; once it hits the total the loop stops.
        let scores = ["0 / 256", "40 / 256", "180 / 256", "256 / 256"];
        let vm = Vm::new(FakeHarness::with_scores(&scores), &c);
        let clock = FakeClock::new();
        let row = run_benchmark(&c, &vm, &vm, &NoopRunner, &clock).unwrap();
        assert_eq!(row.stop_reason, StopReason::Solved);
        assert_eq!(row.points, 256);
        assert_eq!(row.peak_points, 256);
        assert!(row.wall_s < 7200, "should stop early on solve, not run the box");
    }

    #[test]
    fn timeline_records_the_climb() {
        let c = cfg(7200);
        let scores = ["0 / 256", "40 / 256", "256 / 256"];
        let vm = Vm::new(FakeHarness::with_scores(&scores), &c);
        let clock = FakeClock::new();
        let row = run_benchmark(&c, &vm, &vm, &NoopRunner, &clock).unwrap();
        let earned: Vec<i64> = row.timeline.points.iter().map(|&(_, p)| p).collect();
        assert_eq!(earned.first(), Some(&0));
        assert!(earned.contains(&40));
        assert_eq!(earned.last(), Some(&256));
    }
}

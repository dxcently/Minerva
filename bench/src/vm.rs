//! The VM under the loop: lifecycle verbs and reading the scorer, both through
//! a `Harness` so a run drives real qemu in the field and canned output in
//! tests. The real harness shells to the configured command
//! (`vm_cmd`/`score_cmd`, e.g. `wsl bin/botforge-vm.sh …`); it never talks to
//! the guest's ssh for the score, which is read over the console so it survives
//! a lockout (triage-toolkit.md §7.4.1, invariant d).

use crate::clock::Clock;
use crate::score::{parse_score, Score};
use std::io;
use std::process::Command;

/// Runs one argv and returns its stdout, or an error on non-zero exit.
pub trait Harness {
    fn run(&self, argv: &[String]) -> io::Result<String>;
}

pub trait VmControl {
    fn reset(&self) -> io::Result<()>;
    fn boot(&self) -> io::Result<()>;
    fn wait_ready(&self, clock: &dyn Clock, timeout_s: u64) -> io::Result<()>;
    fn stop(&self) -> io::Result<()>;
}

pub trait ScoreReader {
    fn read(&self) -> io::Result<Option<Score>>;
}

pub struct ShellHarness;

impl Harness for ShellHarness {
    fn run(&self, argv: &[String]) -> io::Result<String> {
        let (cmd, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
        let out = Command::new(cmd).args(args).output()?;
        if !out.status.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("`{}` exited {}: {}", cmd, out.status, String::from_utf8_lossy(&out.stderr).trim()),
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

pub struct Vm<H: Harness> {
    harness: H,
    vm_cmd: Vec<String>,
    score_cmd: Vec<String>,
    ready_cmd: Vec<String>,
    score_total: i64,
    poll_interval_s: u64,
}

impl<H: Harness> Vm<H> {
    pub fn new(harness: H, cfg: &crate::config::BenchConfig) -> Self {
        Self {
            harness,
            vm_cmd: cfg.vm_cmd.clone(),
            score_cmd: cfg.score_cmd.clone(),
            ready_cmd: cfg.ready_cmd.clone(),
            score_total: cfg.score_total,
            poll_interval_s: cfg.poll_interval_s,
        }
    }

    fn verb(&self, verb: &str) -> io::Result<String> {
        let mut argv = self.vm_cmd.clone();
        argv.push(verb.to_string());
        self.harness.run(&argv)
    }
}

impl<H: Harness> VmControl for Vm<H> {
    fn reset(&self) -> io::Result<()> {
        self.verb("reset").map(|_| ())
    }

    fn boot(&self) -> io::Result<()> {
        self.verb("up").map(|_| ())
    }

    fn wait_ready(&self, clock: &dyn Clock, timeout_s: u64) -> io::Result<()> {
        if self.ready_cmd.is_empty() {
            return Ok(());
        }
        let deadline = clock.elapsed_s() + timeout_s;
        loop {
            if self.harness.run(&self.ready_cmd).is_ok() {
                return Ok(());
            }
            if clock.elapsed_s() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("guest not ready within {timeout_s}s"),
                ));
            }
            clock.sleep_s(self.poll_interval_s);
        }
    }

    fn stop(&self) -> io::Result<()> {
        self.verb("down").map(|_| ())
    }
}

impl<H: Harness> ScoreReader for Vm<H> {
    fn read(&self) -> io::Result<Option<Score>> {
        let out = self.harness.run(&self.score_cmd)?;
        Ok(parse_score(&out, self.score_total))
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    /// A harness that replays queued outputs per verb, so a test scripts a whole
    /// run: the score command's queue is what the loop will observe over time.
    #[derive(Default)]
    pub struct FakeHarness {
        pub scores: RefCell<VecDeque<String>>,
        pub ready_after: RefCell<u32>,
        pub calls: RefCell<Vec<String>>,
    }

    impl FakeHarness {
        pub fn with_scores(scores: &[&str]) -> Self {
            Self {
                scores: RefCell::new(scores.iter().map(|s| s.to_string()).collect()),
                ready_after: RefCell::new(0),
                calls: RefCell::new(vec![]),
            }
        }
    }

    impl Harness for FakeHarness {
        fn run(&self, argv: &[String]) -> io::Result<String> {
            let joined = argv.join(" ");
            self.calls.borrow_mut().push(joined.clone());
            if joined.contains("score") {
                let mut q = self.scores.borrow_mut();
                // Hold the last reading once the queue drains.
                let s = if q.len() > 1 { q.pop_front().unwrap() } else { q.front().cloned().unwrap_or_default() };
                return Ok(s);
            }
            if joined.contains("ready") {
                let mut n = self.ready_after.borrow_mut();
                if *n == 0 {
                    return Ok("ok".into());
                }
                *n -= 1;
                return Err(io::Error::new(io::ErrorKind::Other, "not yet"));
            }
            Ok(String::new())
        }
    }
}

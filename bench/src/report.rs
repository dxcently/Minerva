//! One run, one row (triage-toolkit.md §8). Serialised to JSON by hand so the
//! crate stays dependency-free; the row is flat and the only string that can
//! carry a quote is a model or image name, so escaping stays simple.

use crate::run::StopReason;

#[derive(Debug, Clone)]
pub struct Timeline {
    /// (elapsed seconds, earned points) samples, in order.
    pub points: Vec<(u64, i64)>,
}

impl Timeline {
    pub fn new() -> Self {
        Self { points: vec![] }
    }
    pub fn push(&mut self, t: u64, earned: i64) {
        self.points.push((t, earned));
    }
    pub fn peak(&self) -> i64 {
        self.points.iter().map(|&(_, p)| p).max().unwrap_or(0)
    }

    /// Area under the score curve, in points·seconds (trapezoidal over the
    /// recorded samples). Rewards a score reached early over the same score
    /// reached late: speed and height in one number. Zero for a flat baseline.
    pub fn auc_points_s(&self) -> i64 {
        self.points
            .windows(2)
            .map(|w| {
                let (t0, p0) = w[0];
                let (t1, p1) = w[1];
                (t1.saturating_sub(t0)) as i64 * (p0 + p1) / 2
            })
            .sum()
    }

    /// Seconds from the start until the score first rose above the baseline
    /// (the first sample), or `-1` if it never did.
    pub fn time_to_first_point_s(&self) -> i64 {
        let Some(&(_, base)) = self.points.first() else { return -1 };
        self.points
            .iter()
            .find(|&&(_, p)| p > base)
            .map(|&(t, _)| t as i64)
            .unwrap_or(-1)
    }

    /// Seconds until the peak was first reached — when the run stopped
    /// improving. `-1` for an empty timeline.
    pub fn time_to_peak_s(&self) -> i64 {
        let peak = self.peak();
        self.points
            .iter()
            .find(|&&(_, p)| p == peak)
            .map(|&(t, _)| t as i64)
            .unwrap_or(-1)
    }
}

impl Default for Timeline {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct Row {
    pub image: String,
    pub sha: String,
    pub model: String,
    pub graphs_snapshot: String,
    pub mode: String,
    pub points: i64,
    pub total: i64,
    pub peak_points: i64,
    pub forensics_k: u32,
    pub forensics_of: u32,
    pub penalties: i64,
    pub escalations_by_tier: [u32; 4],
    pub leak_hits: u32,
    pub tokens: u64,
    pub wall_s: u64,
    pub stop_reason: StopReason,
    /// How the authoritative `points` were obtained: `"read"` when the scorer
    /// answered at the final read, `"peak_fallback"` when it did not and the
    /// row fell back to the timeline's high-water mark. A reader must not treat
    /// a fallback number as a measured final score.
    pub score_source: &'static str,
    pub timeline: Timeline,
}

impl Row {
    pub fn to_json(&self) -> String {
        let mut o = String::from("{");
        push_str(&mut o, "image", &self.image, true);
        push_str(&mut o, "sha", &self.sha, false);
        push_str(&mut o, "model", &self.model, false);
        push_str(&mut o, "graphs_snapshot", &self.graphs_snapshot, false);
        push_str(&mut o, "mode", &self.mode, false);
        push_num(&mut o, "points", self.points);
        push_num(&mut o, "total", self.total);
        push_num(&mut o, "peak_points", self.peak_points);
        push_num(&mut o, "forensics_k", self.forensics_k as i64);
        push_num(&mut o, "forensics_of", self.forensics_of as i64);
        push_num(&mut o, "penalties", self.penalties);
        push_raw(&mut o, "escalations_by_tier", &tier_array(&self.escalations_by_tier));
        push_num(&mut o, "leak_hits", self.leak_hits as i64);
        push_num(&mut o, "tokens", self.tokens as i64);
        push_num(&mut o, "wall_s", self.wall_s as i64);
        push_str(&mut o, "stop_reason", self.stop_reason.as_str(), false);
        push_str(&mut o, "score_source", self.score_source, false);
        // Reward primitives off the timeline: speed and timing, so training can
        // weight a fast climb, not only the final height (points, above).
        push_num(&mut o, "auc_points_s", self.timeline.auc_points_s());
        push_num(&mut o, "time_to_first_point_s", self.timeline.time_to_first_point_s());
        push_num(&mut o, "time_to_peak_s", self.timeline.time_to_peak_s());
        push_raw(&mut o, "timeline", &timeline_array(&self.timeline));
        o.push('}');
        o
    }
}

fn tier_array(t: &[u32; 4]) -> String {
    format!("[{},{},{},{}]", t[0], t[1], t[2], t[3])
}

fn timeline_array(tl: &Timeline) -> String {
    let mut s = String::from("[");
    for (i, (t, p)) in tl.points.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("[{},{}]", t, p));
    }
    s.push(']');
    s
}

fn push_str(o: &mut String, key: &str, val: &str, first: bool) {
    if !first {
        o.push(',');
    }
    o.push_str(&format!("\"{}\":\"{}\"", key, escape(val)));
}

fn push_num(o: &mut String, key: &str, val: i64) {
    o.push_str(&format!(",\"{}\":{}", key, val));
}

fn push_raw(o: &mut String, key: &str, raw: &str) {
    o.push_str(&format!(",\"{}\":{}", key, raw));
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> Row {
        Row {
            image: "botforge-practice-v2".into(),
            sha: "abc".into(),
            model: "ollama:deepseek-v4.1-flash".into(),
            graphs_snapshot: "lib@1".into(),
            mode: "blind".into(),
            points: 40,
            total: 256,
            peak_points: 44,
            forensics_k: 2,
            forensics_of: 7,
            penalties: 0,
            escalations_by_tier: [3, 2, 1, 0],
            leak_hits: 0,
            tokens: 12345,
            wall_s: 7200,
            stop_reason: StopReason::TimeBox,
            score_source: "read",
            timeline: {
                let mut t = Timeline::new();
                t.push(0, 0);
                t.push(60, 40);
                t
            },
        }
    }

    #[test]
    fn json_has_every_field_and_arrays() {
        let j = row().to_json();
        assert!(j.starts_with('{') && j.ends_with('}'));
        assert!(j.contains("\"points\":40"));
        assert!(j.contains("\"escalations_by_tier\":[3,2,1,0]"));
        assert!(j.contains("\"timeline\":[[0,0],[60,40]]"));
        assert!(j.contains("\"stop_reason\":\"time_box\""));
    }

    #[test]
    fn quotes_in_names_are_escaped() {
        let mut r = row();
        r.model = "weird\"name".into();
        assert!(r.to_json().contains("weird\\\"name"));
    }

    #[test]
    fn peak_tracks_the_high_water_mark() {
        let mut t = Timeline::new();
        t.push(0, 0);
        t.push(30, 44);
        t.push(60, 40);
        assert_eq!(t.peak(), 44);
    }

    #[test]
    fn area_rewards_the_faster_climb_at_equal_final_score() {
        // Both reach 100 by t=100, but A gets there at t=10 and holds.
        let mut fast = Timeline::new();
        fast.push(0, 0);
        fast.push(10, 100);
        fast.push(100, 100);
        let mut slow = Timeline::new();
        slow.push(0, 0);
        slow.push(90, 0);
        slow.push(100, 100);
        assert_eq!(fast.points.last(), slow.points.last()); // same final score
        assert!(fast.auc_points_s() > slow.auc_points_s()); // fast wins on area
        assert_eq!(fast.time_to_first_point_s(), 10);
        assert_eq!(slow.time_to_first_point_s(), 100);
    }

    #[test]
    fn area_and_timings_of_a_flat_baseline_are_zero_and_minus_one() {
        let mut t = Timeline::new();
        t.push(0, 0);
        t.push(300, 0);
        assert_eq!(t.auc_points_s(), 0);
        assert_eq!(t.time_to_first_point_s(), -1); // never rose
        assert_eq!(t.time_to_peak_s(), 0); // peak is the baseline, reached at t=0
    }

    #[test]
    fn area_is_the_trapezoid_sum() {
        // 0..60 rising 0→40 is a triangle (1200), 60..120 flat at 40 is 2400.
        let mut t = Timeline::new();
        t.push(0, 0);
        t.push(60, 40);
        t.push(120, 40);
        assert_eq!(t.auc_points_s(), 1200 + 2400);
        assert_eq!(t.time_to_peak_s(), 60);
    }
}

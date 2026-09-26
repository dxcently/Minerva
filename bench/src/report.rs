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
}

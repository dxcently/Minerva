//! Bench settings (triage-toolkit.md §7.6) plus the operational knobs the loop
//! needs. Parsed from a small `key = value` file so the crate stays
//! dependency-free; unknown keys are an error, not a silent typo.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct BenchConfig {
    // §7.6 recorded settings.
    pub brain_model: String,
    pub feedback_blind: bool,
    pub time_box_s: u64,
    pub repeats: u32,

    // Operational.
    pub poll_interval_s: u64,
    pub boot_timeout_s: u64,
    pub score_total: i64,

    // The VM harness: `vm_cmd <verb>` runs a lifecycle verb (reset/up/down);
    // `score_cmd` reads the scorer over the console. Split on spaces.
    pub vm_cmd: Vec<String>,
    pub score_cmd: Vec<String>,
    pub ready_cmd: Vec<String>,

    // §7.4 honesty checks.
    pub allowed_tools: Vec<String>,
    pub deny_paths: Vec<String>,

    // Recorded into the report row for provenance.
    pub image: String,
    pub image_sha: String,
    pub graphs_snapshot: String,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            brain_model: "ollama:deepseek-v4.1-flash".into(),
            feedback_blind: true,
            time_box_s: 7200,
            repeats: 3,
            poll_interval_s: 30,
            boot_timeout_s: 300,
            score_total: 256,
            vm_cmd: vec!["botforge-vm.sh".into()],
            score_cmd: vec![],
            ready_cmd: vec![],
            allowed_tools: vec!["fs".into(), "ssh".into(), "vm_read".into()],
            deny_paths: vec!["/opt/aeacus".into(), "ScoringReport.html".into()],
            image: "botforge-practice-v2".into(),
            image_sha: String::new(),
            graphs_snapshot: String::new(),
        }
    }
}

impl BenchConfig {
    pub fn from_str(text: &str) -> Result<Self, String> {
        let mut cfg = BenchConfig::default();
        for (n, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            let (key, val) = line
                .split_once('=')
                .ok_or_else(|| format!("line {}: not `key = value`", n + 1))?;
            let key = key.trim();
            let val = val.trim();
            match key {
                "brain_model" => cfg.brain_model = val.into(),
                "feedback" => cfg.feedback_blind = parse_feedback(val, n + 1)?,
                "time_box_s" => cfg.time_box_s = parse_u64(key, val, n + 1)?,
                "repeats" => cfg.repeats = parse_u64(key, val, n + 1)? as u32,
                "poll_interval_s" => cfg.poll_interval_s = parse_u64(key, val, n + 1)?,
                "boot_timeout_s" => cfg.boot_timeout_s = parse_u64(key, val, n + 1)?,
                "score_total" => cfg.score_total = parse_u64(key, val, n + 1)? as i64,
                "vm_cmd" => cfg.vm_cmd = words(val),
                "score_cmd" => cfg.score_cmd = words(val),
                "ready_cmd" => cfg.ready_cmd = words(val),
                "allowed_tools" => cfg.allowed_tools = commas(val),
                "deny_paths" => cfg.deny_paths = commas(val),
                "image" => cfg.image = val.into(),
                "image_sha" => cfg.image_sha = val.into(),
                "graphs_snapshot" => cfg.graphs_snapshot = val.into(),
                other => return Err(format!("line {}: unknown key `{}`", n + 1, other)),
            }
        }
        cfg.validate()
    }

    fn validate(self) -> Result<Self, String> {
        if self.poll_interval_s == 0 {
            return Err("poll_interval_s must be > 0".into());
        }
        if self.score_total <= 0 {
            return Err("score_total must be > 0".into());
        }
        if self.score_cmd.is_empty() {
            return Err("score_cmd is required (how to read the scorer)".into());
        }
        Ok(self)
    }

    /// For the report's `mode` column.
    pub fn mode(&self) -> &'static str {
        if self.feedback_blind {
            "blind"
        } else {
            "feedback"
        }
    }

    /// Deterministic dump for a report footer / provenance line.
    pub fn recorded(&self) -> BTreeMap<&'static str, String> {
        let mut m = BTreeMap::new();
        m.insert("brain_model", self.brain_model.clone());
        m.insert("feedback", self.mode().to_string());
        m.insert("time_box_s", self.time_box_s.to_string());
        m.insert("repeats", self.repeats.to_string());
        m
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(i) => &line[..i],
        None => line,
    }
}

fn words(val: &str) -> Vec<String> {
    val.split_whitespace().map(|s| s.to_string()).collect()
}

fn commas(val: &str) -> Vec<String> {
    val.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_u64(key: &str, val: &str, line: usize) -> Result<u64, String> {
    val.parse::<u64>()
        .map_err(|_| format!("line {}: `{}` wants a whole number, got `{}`", line, key, val))
}

fn parse_feedback(val: &str, line: usize) -> Result<bool, String> {
    match val {
        "blind" => Ok(true),
        "feedback" => Ok(false),
        other => Err(format!("line {}: feedback is `blind` or `feedback`, got `{}`", line, other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_doc() {
        let cfg = BenchConfig::default();
        assert_eq!(cfg.brain_model, "ollama:deepseek-v4.1-flash");
        assert!(cfg.feedback_blind);
        assert_eq!(cfg.time_box_s, 7200);
        assert_eq!(cfg.repeats, 3);
        assert_eq!(cfg.score_total, 256);
    }

    #[test]
    fn parses_overrides_and_lists() {
        let text = "\
# a run
score_cmd = wsl botforge-vm.sh score
feedback = feedback
time_box_s = 600
allowed_tools = fs, ssh, vm_read
deny_paths = /opt/aeacus, ScoringReport.html
";
        let cfg = BenchConfig::from_str(text).unwrap();
        assert_eq!(cfg.score_cmd, vec!["wsl", "botforge-vm.sh", "score"]);
        assert!(!cfg.feedback_blind);
        assert_eq!(cfg.time_box_s, 600);
        assert_eq!(cfg.allowed_tools, vec!["fs", "ssh", "vm_read"]);
        assert_eq!(cfg.deny_paths.len(), 2);
    }

    #[test]
    fn unknown_key_is_rejected() {
        let err = BenchConfig::from_str("score_cmd = x\nnope = 1").unwrap_err();
        assert!(err.contains("unknown key"));
    }

    #[test]
    fn score_cmd_is_required() {
        let err = BenchConfig::from_str("time_box_s = 60").unwrap_err();
        assert!(err.contains("score_cmd"));
    }

    #[test]
    fn zero_poll_interval_is_rejected() {
        let err = BenchConfig::from_str("score_cmd = x\npoll_interval_s = 0").unwrap_err();
        assert!(err.contains("poll_interval_s"));
    }
}

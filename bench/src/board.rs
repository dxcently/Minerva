//! The board: one round's record of what was found and what was done, kept as
//! an append-only JSONL file and rendered two ways — a short digest that fits
//! jev's context budget, and a fuller view for the brain.
//!
//! Every entry is a fact about the round, never an instruction: the brain
//! reads the board to decide, and a later retrain reads it to label.

use crate::jev::{head, CONTEXT_BYTES};
use crate::json::quote;
use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// Something a scout looked at. `id` is how a later fix cites it.
    Finding { id: String, role: String, item: String, detail: String },
    /// An action the loop ran, and what the score did after it.
    Done { kind: String, cmd: String, ok: bool, delta: i64, cites: Vec<String> },
}

impl Entry {
    pub fn to_json(&self) -> String {
        match self {
            Entry::Finding { id, role, item, detail } => format!(
                r#"{{"t":"finding","id":{},"role":{},"item":{},"detail":{}}}"#,
                quote(id),
                quote(role),
                quote(item),
                quote(detail)
            ),
            Entry::Done { kind, cmd, ok, delta, cites } => {
                let c: Vec<String> = cites.iter().map(|s| quote(s)).collect();
                format!(
                    r#"{{"t":"done","kind":{},"cmd":{},"ok":{},"delta":{},"cites":[{}]}}"#,
                    quote(kind),
                    quote(cmd),
                    ok,
                    delta,
                    c.join(",")
                )
            }
        }
    }
}

#[derive(Debug, Default)]
pub struct Board {
    pub entries: Vec<Entry>,
    /// Where each entry is appended as it is added; `None` keeps it in memory.
    pub log: Option<PathBuf>,
}

impl Board {
    pub fn new(log: Option<PathBuf>) -> Self {
        Self { entries: vec![], log }
    }

    pub fn add(&mut self, e: Entry) -> io::Result<()> {
        if let Some(path) = &self.log {
            let mut f = OpenOptions::new().create(true).append(true).open(path)?;
            writeln!(f, "{}", e.to_json())?;
        }
        self.entries.push(e);
        Ok(())
    }

    /// Every id any action cites, which is what makes a finding closed.
    fn cited_ids(&self) -> HashSet<&str> {
        let mut cited: HashSet<&str> = HashSet::new();
        for e in &self.entries {
            if let Entry::Done { cites, .. } = e {
                cited.extend(cites.iter().map(|c| c.as_str()));
            }
        }
        cited
    }

    /// Findings no action has cited yet: what is still open, in the order they
    /// were found.
    pub fn open_findings(&self) -> Vec<&Entry> {
        let cited = self.cited_ids();
        self.entries
            .iter()
            .filter(|e| match e {
                Entry::Finding { id, .. } => !cited.contains(id.as_str()),
                Entry::Done { .. } => false,
            })
            .collect()
    }

    /// The board's own digest, with no goal to lead it: how many findings are
    /// open and how many actions ran, then every open finding as `id item`,
    /// cut to jev's context budget. Small enough to sit in jev's menu context
    /// whole; [`Board::digest_goal`] is the chooser's version, which spends its
    /// first bytes on the round's target instead.
    pub fn digest(&self) -> String {
        let open = self.open_findings();
        let done = self
            .entries
            .iter()
            .filter(|e| matches!(e, Entry::Done { .. }))
            .count();
        let mut s = format!("open {} done {}", open.len(), done);
        for e in &open {
            if let Entry::Finding { id, item, .. } = e {
                s.push_str("; ");
                s.push_str(id);
                s.push(' ');
                s.push_str(item);
            }
        }
        head(&s, CONTEXT_BYTES).to_string()
    }

    /// One line per entry, in order, the whole round read top to bottom — what
    /// the brain reads. A finding is marked `[open]` or `[cited]` as of now, so
    /// the brain does not have to reconstruct which findings the actions closed
    /// by scanning their `cites`. Unbounded on purpose — the brain's window is
    /// not jev's. [`Board::for_brain`] is the same history with the open
    /// findings restated under a heading.
    pub fn brain_view(&self) -> String {
        let cited = self.cited_ids();
        let mut lines: Vec<String> = Vec::with_capacity(self.entries.len());
        for e in &self.entries {
            match e {
                Entry::Finding { id, role, item, detail } => lines.push(format!(
                    "finding {id} [{}] {role} {item}: {detail}",
                    if cited.contains(id.as_str()) { "cited" } else { "open" }
                )),
                Entry::Done { kind, cmd, ok, delta, cites } => lines.push(format!(
                    "done {kind} ok={ok} delta={delta} cites={} :: {cmd}",
                    cites.join(",")
                )),
            }
        }
        lines.join("\n")
    }

    /// The chooser's digest: the goal first, then one line per open finding
    /// and per action taken, as many as fit in jev's context budget. This is
    /// the `choose(context, …)` context, so `goal` comes first and is the one
    /// thing never dropped — jevlike reads the first ~`CONTEXT_BYTES` bytes
    /// reliably, and a finding written in front of the goal is a finding it
    /// will not see.
    ///
    /// `goal` is the round's target, passed in rather than stored: the board
    /// holds facts about the round, and the loop is what knows what the round
    /// is for. [`Board::digest`] is the goal-less summary for when the caller
    /// has no target to lead with.
    pub fn digest_goal(&self, goal: &str) -> String {
        const PREFIX: &str = "goal: ";
        let mut out = String::from(PREFIX);
        out.push_str(head(goal, CONTEXT_BYTES - PREFIX.len()));

        let mut seen: Vec<String> = vec![out.clone()];
        for (key, line) in self.digest_candidates() {
            // A line that repeats never appears twice: two identical labels in
            // a chooser's menu are a near-tie against itself, which the
            // accept/reject gate reads as no answer. Half the budget is the
            // most one line may take, so one long item cannot crowd out every
            // other finding.
            if seen.iter().any(|s| *s == key || *s == line) {
                continue;
            }
            let line = head(&line, CONTEXT_BYTES / 2);
            if out.len() + 1 + line.len() > CONTEXT_BYTES {
                break;
            }
            out.push('\n');
            out.push_str(line);
            seen.push(key);
            seen.push(line.to_string());
        }
        // Belt and braces: `head` is already how every piece above was cut,
        // so this only matters if a goal carries a byte offset the caller's
        // own arithmetic missed.
        head(&out, CONTEXT_BYTES).to_string()
    }

    /// The fuller view for the brain: the whole history in order, with each
    /// open finding restated so the brain does not have to reconstruct which
    /// ones are still open by scanning the actions' `cites`. Unbounded on
    /// purpose — the brain's window is not jev's.
    pub fn for_brain(&self) -> String {
        let open = self.open_findings();
        let actions = self.entries.iter().filter(|e| matches!(e, Entry::Done { .. })).count();
        let mut out = format!(
            "board: {} entries -- {} open finding(s), {} action(s)\n",
            self.entries.len(),
            open.len(),
            actions
        );
        out.push_str("open findings:\n");
        for e in &open {
            if let Entry::Finding { id, role, item, detail } = e {
                out.push_str(&format!("  {id} [{role}] {item} -- {detail}\n"));
            }
        }
        out.push_str("history:\n");
        for e in &self.entries {
            match e {
                Entry::Finding { id, role, item, detail } => {
                    out.push_str(&format!("  found {id} [{role}] {item} -- {detail}\n"))
                }
                Entry::Done { kind, cmd, ok, delta, cites } => out.push_str(&format!(
                    "  did {kind} `{cmd}` {} {delta:+} citing [{}]\n",
                    if *ok { "ok" } else { "failed" },
                    cites.join(", ")
                )),
            }
        }
        out
    }

    /// The digest's candidate lines, most valuable first: the open findings
    /// (what the chooser picks from) before the actions (what has already been
    /// tried, worth the room only if there is room). Each carries a key so a
    /// repeated entry is dropped by [`Board::digest_goal`] whichever way it
    /// repeats — a finding restated under the same id, or the same command run
    /// twice.
    fn digest_candidates(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for e in self.open_findings() {
            if let Entry::Finding { id, role, item, .. } = e {
                out.push((format!("f{id}"), format!("{id} [{role}] {item}")));
            }
        }
        for e in &self.entries {
            if let Entry::Done { kind, cmd, ok, delta, cites: _ } = e {
                out.push((
                    format!("d{kind}{cmd}{ok}{delta}"),
                    format!(
                        "did {kind} {cmd} {} {delta:+}",
                        if *ok { "ok" } else { "failed" }
                    ),
                ));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(id: &str, role: &str, item: &str, detail: &str) -> Entry {
        Entry::Finding {
            id: id.into(),
            role: role.into(),
            item: item.into(),
            detail: detail.into(),
        }
    }

    fn done(cmd: &str, delta: i64, cites: &[&str]) -> Entry {
        Entry::Done {
            kind: "fix".into(),
            cmd: cmd.into(),
            ok: true,
            delta,
            cites: cites.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The board the samples below are rendered from: four findings, one
    /// already cited by an action.
    fn sample() -> Board {
        let mut b = Board::new(None);
        b.add(finding("f1", "files", "/etc/shadow is world-readable", "mode 0644, owner root:root"))
            .unwrap();
        b.add(done("chmod 600 /etc/shadow", 8, &["f1"])).unwrap();
        b.add(finding("f2", "ssh", "sshd_config has PermitRootLogin yes", "line 41, no AllowUsers"))
            .unwrap();
        b.add(finding("f3", "services", "telnetd listening on :23", "inetd.conf line 12"))
            .unwrap();
        b
    }

    #[test]
    fn open_findings_are_the_uncited_ones() {
        let b = sample();
        let ids: Vec<&str> = b
            .open_findings()
            .iter()
            .map(|e| match e {
                Entry::Finding { id, .. } => id.as_str(),
                _ => panic!("an action is not a finding"),
            })
            .collect();
        assert_eq!(ids, vec!["f2", "f3"]);
    }

    #[test]
    fn digest_goal_stays_within_the_context_budget() {
        let b = sample();
        let d = b.digest_goal("raise the score on botforge-practice-v2, 0/256 now, 256/256 is the target");
        assert!(
            d.len() <= CONTEXT_BYTES && d.len() <= 192,
            "digest is {} bytes: {d:?}",
            d.len()
        );
        // The goal is what jevlike reliably reads, so it is at the front.
        assert!(d.starts_with("goal: raise the score"), "digest: {d:?}");
    }

    #[test]
    fn digest_goal_survives_a_goal_longer_than_the_whole_budget() {
        let b = sample();
        let goal = "target ".repeat(100);
        let d = b.digest_goal(&goal);
        assert!(d.len() <= CONTEXT_BYTES, "digest is {} bytes", d.len());
        // Multi-byte characters at the cut: `head` lands on a boundary.
        let d = b.digest_goal(&"é".repeat(400));
        assert!(d.len() <= CONTEXT_BYTES && d.is_char_boundary(d.len()));
    }

    #[test]
    fn digest_goal_never_repeats_an_entry() {
        let mut b = sample();
        // The same finding restated under the same id, and the same fix run
        // twice: a chooser's menu must not carry either twice.
        b.add(finding("f2", "ssh", "sshd_config has PermitRootLogin yes", "line 41, no AllowUsers"))
            .unwrap();
        b.add(done("chmod 600 /etc/shadow", 8, &["f1"])).unwrap();
        let d = b.digest_goal("target");
        assert_eq!(d.matches("f2 [ssh]").count(), 1, "digest: {d:?}");
        assert_eq!(d.matches("chmod 600 /etc/shadow").count(), 1, "digest: {d:?}");
    }

    #[test]
    fn for_brain_carries_history_and_the_open_findings() {
        let b = sample();
        let v = b.for_brain();
        assert!(v.contains("board: 4 entries -- 2 open finding(s), 1 action(s)"), "view: {v}");
        assert!(v.contains("  found f1 [files]"), "view: {v}");
        assert!(v.contains("  did fix `chmod 600 /etc/shadow` ok +8 citing [f1]"), "view: {v}");
        // Findings open *and* in history: the brain gets both readings.
        assert_eq!(v.matches("f2 [ssh]").count(), 2, "view: {v}");
    }

    #[test]
    fn open_findings_drops_a_cited_id_and_keeps_the_uncited_in_order() {
        let mut b = Board::new(None);
        b.add(finding("f1", "files", "one", "d1")).unwrap();
        b.add(finding("f2", "ssh", "two", "d2")).unwrap();
        b.add(finding("f3", "services", "three", "d3")).unwrap();
        b.add(finding("f4", "net", "four", "d4")).unwrap();
        // One action cites two of them at once, one cites a fourth.
        b.add(done("chmod 600 /etc/shadow", 8, &["f1", "f3"])).unwrap();
        let ids: Vec<&str> = b
            .open_findings()
            .iter()
            .map(|e| match e {
                Entry::Finding { id, .. } => id.as_str(),
                _ => panic!("an action is not a finding"),
            })
            .collect();
        assert_eq!(ids, vec!["f2", "f4"]);
        // A citation of an id that was never a finding opens nothing: f5 stays
        // in the board as a finding of its own and f2 is still uncited.
        b.add(done("rm /tmp/x", 0, &["f5"])).unwrap();
        let ids: Vec<&str> = b
            .open_findings()
            .iter()
            .map(|e| match e {
                Entry::Finding { id, .. } => id.as_str(),
                _ => panic!("an action is not a finding"),
            })
            .collect();
        assert_eq!(ids, vec!["f2", "f4"]);
    }

    #[test]
    fn digest_is_bounded_and_counts_the_round() {
        let mut b = Board::new(None);
        for i in 0..50 {
            b.add(finding(
                &format!("f{i}"),
                "files",
                "this item is long enough that fifty of them cannot fit in one jev context",
                &"detail ".repeat(20),
            ))
            .unwrap();
        }
        let d = b.digest();
        assert!(d.len() <= CONTEXT_BYTES, "digest is {} bytes: {d:?}", d.len());
        assert!(d.starts_with("open 50 done 0"), "digest: {d:?}");
        // Nothing was done yet, so the summary is the count and open findings.
        let mut b = Board::new(None);
        b.add(finding("f1", "ssh", "PermitRootLogin yes", "line 41")).unwrap();
        b.add(done("sed -i s/yes/no/ sshd_config", 4, &["f1"])).unwrap();
        assert_eq!(b.digest(), "open 0 done 1");
    }

    #[test]
    fn brain_view_marks_cited_and_open() {
        let b = sample();
        let v = b.brain_view();
        let lines: Vec<&str> = v.lines().collect();
        assert_eq!(lines.len(), 4, "one line per entry: {v:?}");
        // f1 is cited by the action on line 2; f2 and f3 are not.
        assert_eq!(
            lines[0],
            "finding f1 [cited] files /etc/shadow is world-readable: mode 0644, owner root:root"
        );
        assert_eq!(lines[1], "done fix ok=true delta=8 cites=f1 :: chmod 600 /etc/shadow");
        assert_eq!(
            lines[2],
            "finding f2 [open] ssh sshd_config has PermitRootLogin yes: line 41, no AllowUsers"
        );
        assert_eq!(
            lines[3],
            "finding f3 [open] services telnetd listening on :23: inetd.conf line 12"
        );
        assert_eq!(lines.join("\n"), v);
    }

    #[test]
    fn add_appends_one_json_line_per_entry() {
        let path = std::env::temp_dir().join(format!("board-test-{}.jsonl", std::process::id()));
        // A file left by an earlier run would be appended to, and the count
        // below would then be somebody else's.
        let _ = std::fs::remove_file(&path);
        {
            let mut b = Board::new(Some(path.clone()));
            b.add(finding("f1", "files", "/etc/shadow is world-readable", "mode 0644")).unwrap();
            b.add(done("chmod 600 /etc/shadow", 8, &["f1"])).unwrap();
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "file: {text:?}");
        assert!(lines[0].starts_with(r#"{"t":"finding","id":"f1","#), "line: {}", lines[0]);
        assert_eq!(
            lines[1],
            r#"{"t":"done","kind":"fix","cmd":"chmod 600 /etc/shadow","ok":true,"delta":8,"cites":["f1"]}"#
        );
        // Each line parses as one JSON value, so the append-only log is a log.
        for line in &lines {
            assert!(crate::json::parse(line).is_ok(), "line: {line}");
        }
        std::fs::remove_file(&path).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn to_json_escapes_a_quote_in_detail() {
        let j = finding("f9", "files", r#"a "quoted" item"#, r#"detail with " inside"#).to_json();
        assert!(j.contains(r#"detail with \" inside"#), "json: {j}");
        assert!(!j.contains(r#"detail with " inside"#), "json: {j}");
        assert_eq!(
            j,
            r#"{"t":"finding","id":"f9","role":"files","item":"a \"quoted\" item","detail":"detail with \" inside"}"#
        );
        // What it escapes, the parser reads back.
        let back = crate::json::parse(&j).unwrap();
        assert_eq!(back.get("detail").and_then(|v| v.as_str()), Some(r#"detail with " inside"#));
        assert_eq!(back.get("item").and_then(|v| v.as_str()), Some(r#"a "quoted" item"#));
    }

    /// The samples the log quotes, printed with `cargo test -- --nocapture`.
    #[test]
    fn prints_the_samples() {
        let b = sample();
        let d = b.digest_goal("score 256/256 on botforge-practice-v2 (baseline 0/256)");
        eprintln!(
            "--- digest_goal({} bytes, budget {}) ---\n{d}\n--- digest({} bytes) ---\n{}\n--- for_brain({} bytes) ---\n{}\n--- brain_view ---\n{}",
            d.len(),
            CONTEXT_BYTES,
            b.digest().len(),
            b.digest(),
            b.for_brain().len(),
            b.for_brain(),
            b.brain_view()
        );
        assert!(d.len() <= CONTEXT_BYTES);
        assert!(b.digest().len() <= CONTEXT_BYTES);
    }
}

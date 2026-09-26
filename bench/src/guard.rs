//! Two checks that keep a run honest (triage-toolkit.md §7.4).
//!
//! The agent is root in the guest, so these are detection, not prevention: a
//! run that trips them is invalidated, not silently allowed to continue.

/// Tools present in the agent's config that the benchmark profile does not
/// allow (web, browser, lookup, swarm — anything that could fetch an answer).
/// Returns the disallowed names; empty means the config is clean.
pub fn disallowed_tools<'a>(present: &'a [String], allowed: &[String]) -> Vec<&'a str> {
    present
        .iter()
        .filter(|t| !allowed.iter().any(|a| a.eq_ignore_ascii_case(t)))
        .map(|s| s.as_str())
        .collect()
}

/// Deny-paths named in a script the agent wants to run (the scorer's report and
/// engine dir). A hit is a leak attempt: the script is refused and counted.
pub fn deny_path_hits<'a>(script: &str, deny: &'a [String]) -> Vec<&'a str> {
    deny.iter()
        .filter(|p| !p.is_empty() && script.contains(p.as_str()))
        .map(|s| s.as_str())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn allowlist_is_case_insensitive_and_flags_extras() {
        let present = v(&["fs", "ssh", "Web", "browser"]);
        let allowed = v(&["fs", "ssh"]);
        let bad = disallowed_tools(&present, &allowed);
        assert_eq!(bad, vec!["Web", "browser"]);
    }

    #[test]
    fn clean_config_has_no_findings() {
        let present = v(&["fs", "ssh"]);
        let allowed = v(&["ssh", "fs", "vm_read"]);
        assert!(disallowed_tools(&present, &allowed).is_empty());
    }

    #[test]
    fn deny_path_in_script_is_caught() {
        let deny = v(&["/opt/aeacus", "ScoringReport.html"]);
        let hits = deny_path_hits("cat /opt/aeacus/scoring.dat", &deny);
        assert_eq!(hits, vec!["/opt/aeacus"]);
    }

    #[test]
    fn empty_deny_entry_never_matches() {
        let deny = v(&[""]);
        assert!(deny_path_hits("anything at all", &deny).is_empty());
    }
}

//! What the loop can do on a round, typed: the brain's JSON reply becomes one
//! of these or is refused. Two rules the round depends on live here as code,
//! not as prompt text the model may ignore:
//!
//!   - a command naming a deny path (the scorer's own files) is refused at
//!     parse time — the loop competes, it does not read the answer key;
//!   - a fix that can cut the loop's own hands (sshd, the firewall, sudoers)
//!     is marked `lockout_risk` and `order_for_execution` moves every such
//!     fix after the safe ones, keeping relative order — measured on the
//!     image: removing NOPASSWD or hardening sshd ends root/ssh for the
//!     rest of the round, so those go last.

use crate::guard::deny_path_hits;
use crate::json::Json;
use std::io;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Enumerate one area of the box and let the chooser pick what to inspect.
    Scout { role: String },
    /// A root command that changes the box. `cites` names the board findings
    /// it acts on, so a later score rise can be credited back to them.
    Fix { cmd: String, cites: Vec<String>, note: String },
    /// A read-only check of something already changed.
    Verify { cmd: String },
    /// The brain is done with this round.
    Stop { why: String },
}

/// Why a fix could end the loop's own access, when it could.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockoutRisk {
    Ssh,
    Firewall,
    Sudo,
}

impl Action {
    /// Parse the brain's reply. `deny` is the scorer's file list: any command
    /// naming one is refused here, before it could run.
    pub fn from_json(v: &Json, deny: &[String]) -> io::Result<Action> {
        let s = |k: &str| v.get(k).and_then(Json::as_str).map(str::to_string);
        let kind = s("action").ok_or_else(|| bad("reply has no `action`"))?;
        let action = match kind.as_str() {
            "scout" => Action::Scout { role: s("role").ok_or_else(|| bad("scout needs `role`"))? },
            "fix" => Action::Fix {
                cmd: s("cmd").ok_or_else(|| bad("fix needs `cmd`"))?,
                cites: match v.get("cites") {
                    Some(Json::Arr(a)) => a.iter().filter_map(Json::as_str).map(str::to_string).collect(),
                    _ => vec![],
                },
                note: s("note").unwrap_or_default(),
            },
            "verify" => Action::Verify { cmd: s("cmd").ok_or_else(|| bad("verify needs `cmd`"))? },
            "stop" => Action::Stop { why: s("why").unwrap_or_default() },
            other => return Err(bad(&format!("unknown action `{other}`"))),
        };
        if let Some(cmd) = action.command() {
            let hits = deny_path_hits(cmd, deny);
            if !hits.is_empty() {
                return Err(bad(&format!("command names a deny path {:?}: refused", hits)));
            }
        }
        Ok(action)
    }

    /// The shell command this action would run on the box, if any.
    pub fn command(&self) -> Option<&str> {
        match self {
            Action::Fix { cmd, .. } | Action::Verify { cmd } => Some(cmd),
            _ => None,
        }
    }

    /// A fix that could cut the loop off from the box. Verify and scout only
    /// read, so they never carry this.
    pub fn lockout_risk(&self) -> Option<LockoutRisk> {
        let Action::Fix { cmd, .. } = self else { return None };
        let c = cmd.to_ascii_lowercase();
        let has = |needles: &[&str]| needles.iter().any(|n| c.contains(n));
        if has(&["sshd_config", "sshd", "/etc/ssh/", "ssh.service"]) {
            Some(LockoutRisk::Ssh)
        } else if has(&["ufw", "iptables", "nftables", "nft ", "firewalld"]) {
            Some(LockoutRisk::Firewall)
        } else if has(&["sudoers", "nopasswd", "visudo", "/etc/sudo"]) {
            Some(LockoutRisk::Sudo)
        } else {
            None
        }
    }
}

/// Safe fixes first, lockout-risk fixes after them, each group in its given
/// order. Non-fix actions keep their place among the safe ones.
pub fn order_for_execution(actions: Vec<Action>) -> Vec<Action> {
    let (risky, safe): (Vec<Action>, Vec<Action>) = actions.into_iter().partition(|a| a.lockout_risk().is_some());
    safe.into_iter().chain(risky).collect()
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::parse;

    fn deny() -> Vec<String> {
        vec!["/opt/aeacus".into(), "ScoringReport.html".into()]
    }

    #[test]
    fn parses_each_kind() {
        let a = Action::from_json(&parse(r#"{"action":"scout","role":"users"}"#).unwrap(), &deny()).unwrap();
        assert_eq!(a, Action::Scout { role: "users".into() });
        let a = Action::from_json(
            &parse(r#"{"action":"fix","cmd":"passwd -l guest","cites":["users:guest"],"note":"lock it"}"#).unwrap(),
            &deny(),
        )
        .unwrap();
        assert_eq!(
            a,
            Action::Fix { cmd: "passwd -l guest".into(), cites: vec!["users:guest".into()], note: "lock it".into() }
        );
        let a = Action::from_json(&parse(r#"{"action":"verify","cmd":"passwd -S guest"}"#).unwrap(), &deny()).unwrap();
        assert_eq!(a, Action::Verify { cmd: "passwd -S guest".into() });
        let a = Action::from_json(&parse(r#"{"action":"stop","why":"done"}"#).unwrap(), &deny()).unwrap();
        assert_eq!(a, Action::Stop { why: "done".into() });
    }

    #[test]
    fn unknown_or_incomplete_replies_are_refused() {
        assert!(Action::from_json(&parse(r#"{"action":"dance"}"#).unwrap(), &deny()).is_err());
        assert!(Action::from_json(&parse(r#"{"action":"fix"}"#).unwrap(), &deny()).is_err());
        assert!(Action::from_json(&parse(r#"{"why":"no action key"}"#).unwrap(), &deny()).is_err());
    }

    #[test]
    fn a_command_naming_the_scorer_is_refused_at_parse() {
        let v = parse(r#"{"action":"verify","cmd":"cat /opt/aeacus/scoring.conf"}"#).unwrap();
        let err = Action::from_json(&v, &deny()).unwrap_err();
        assert!(err.to_string().contains("deny path"));
    }

    #[test]
    fn lockout_risks_are_detected_only_on_fixes() {
        let fix = |cmd: &str| Action::Fix { cmd: cmd.into(), cites: vec![], note: String::new() };
        assert_eq!(fix("sed -i 's/^PermitRootLogin.*/PermitRootLogin no/' /etc/ssh/sshd_config").lockout_risk(), Some(LockoutRisk::Ssh));
        assert_eq!(fix("ufw enable").lockout_risk(), Some(LockoutRisk::Firewall));
        assert_eq!(fix("rm /etc/sudoers.d/mford").lockout_risk(), Some(LockoutRisk::Sudo));
        assert_eq!(fix("passwd -l guest").lockout_risk(), None);
        // A verify that merely reads sshd_config is not a lockout.
        assert_eq!(Action::Verify { cmd: "grep PermitRootLogin /etc/ssh/sshd_config".into() }.lockout_risk(), None);
    }

    #[test]
    fn execution_order_puts_lockout_fixes_last_and_keeps_relative_order() {
        let fix = |cmd: &str| Action::Fix { cmd: cmd.into(), cites: vec![], note: String::new() };
        let ordered = order_for_execution(vec![
            fix("rm /etc/sudoers.d/mford"),
            fix("passwd -l guest"),
            Action::Verify { cmd: "passwd -S guest".into() },
            fix("ufw enable"),
            fix("chmod 600 /etc/shadow"),
        ]);
        let cmds: Vec<&str> = ordered.iter().filter_map(Action::command).collect();
        assert_eq!(
            cmds,
            vec!["passwd -l guest", "passwd -S guest", "chmod 600 /etc/shadow", "rm /etc/sudoers.d/mford", "ufw enable"]
        );
    }
}

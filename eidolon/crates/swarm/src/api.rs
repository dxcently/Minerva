//! What the two built-in tools actually do, kept out of the Rune host.
//!
//! The host's job is to cross the language boundary; the wording of a
//! roster and the rules for resolving a name are harness behaviour and
//! belong somewhere they can be read and tested. Both functions return the
//! string the model sees.

use anyhow::Result;

use crate::inbox::Envelope;
use crate::presence::{Delivery, Peer, Presence};

/// The literal `to` that means "everyone working on this repository".
pub const CHANNEL: &str = "channel";

/// The roster, as prose. Sessions on this project come first and are
/// marked as such, because "who else is in this repo" is the question
/// nearly every use of this tool is really asking; anything else the
/// operator has open is listed after, since knowing it exists is
/// occasionally the point and mistaking it for a colleague never is.
pub fn roster(p: &Presence) -> String {
    let cwd = p.meta().cwd.display().to_string();
    roster_of(p.peers(), &p.channel_key(), Some((p.id(), &cwd)))
}

/// The same, for a caller that is not itself a session — `eidolon peers`,
/// which answers "who is running" without registering in order to ask.
pub fn roster_of(peers: Vec<Peer>, key: &std::path::Path, me: Option<(&str, &str)>) -> String {
    if peers.is_empty() {
        return match me {
            Some((id, cwd)) => format!(
                "No other sessions are running. You are `{id}`, working in {}.",
                tilde(cwd)
            ),
            None => "No sessions are running.".to_string(),
        };
    }
    let (here, elsewhere): (Vec<Peer>, Vec<Peer>) = peers
        .into_iter()
        .partition(|q| q.meta.repo.as_ref().unwrap_or(&q.meta.cwd).as_path() == key);

    let mut out = match me {
        Some((id, _)) => format!("You are `{id}`.\n"),
        None => String::new(),
    };
    if !here.is_empty() {
        out.push_str(&format!(
            "\n{} on this project (reachable as `{CHANNEL}`):\n",
            plural(here.len(), "session")
        ));
        for q in &here {
            out.push_str(&row(q));
        }
    }
    if !elsewhere.is_empty() {
        out.push_str(&format!(
            "\n{} elsewhere:\n",
            plural(elsewhere.len(), "session")
        ));
        for q in &elsewhere {
            out.push_str(&row(q));
        }
    }
    out
}

fn row(q: &Peer) -> String {
    let doing = if q.meta.title.trim().is_empty() {
        "(nothing asked yet)".to_string()
    } else {
        format!("\"{}\"", one_line(&q.meta.title))
    };
    format!(
        "  {}  {}  {}  {}  {doing}\n",
        q.meta.id,
        tilde(&q.meta.cwd.display().to_string()),
        q.meta.model,
        if q.meta.busy { "busy" } else { "idle" },
    )
}

/// Send `text` to one peer, or to every session on this project.
///
/// An unknown name is an error that *lists who is there*, for the same
/// reason `Dispatcher::unknown_tool` lists the tools: a model that guessed
/// a session id, or an operator working from a stale roster, turns a dead
/// end into one retry rather than a shrug.
///
/// `wake` is three-valued, and the third value is what makes a swarm move.
/// `None` is *nobody said*, and the answer then depends on who is being
/// addressed: a **direct message wakes**, because one agent naming another
/// is one agent asking it for something, and a note that waits for an
/// operator who is watching a different pane is a note that arrives an
/// hour late; a **fan-out does not**, because "everyone on this
/// repository" is an announcement, and an announcement that starts a turn
/// in every session at once is how a swarm burns its credits on
/// acknowledgements. Neither is a property of the kind, only its default:
/// `Some(true)` on a fan-out wakes everyone it reaches, which is what an
/// announcement that everyone must act on now wants.
///
/// Waking means *time-sensitive*, to a recipient in any state: a session
/// whose turn is in flight takes the message at its next safe point and
/// steers with it — it may not settle without reading it — and an idle
/// one starts a turn on it. A message that does not wake waits in the
/// recipient's inbox for its next turn boundary: context for the next
/// turn, never an interruption of the one running.
pub fn send(p: &Presence, to: &str, text: &str, wake: Option<bool>) -> Result<String> {
    if text.trim().is_empty() {
        anyhow::bail!("a message needs something in it");
    }
    if to.trim().is_empty() {
        anyhow::bail!(
            "say who to send to: a session id from `peers`, or `{CHANNEL}` for everyone on this project"
        );
    }

    if to == CHANNEL {
        let members = p.channel();
        if members.is_empty() {
            anyhow::bail!(
                "no other session is working on this project, so there is nobody on the channel"
            );
        }
        let key = p.channel_key().display().to_string();
        let env = p
            .envelope(text)
            .on_channel(&key)
            .waking(wake.unwrap_or(false));
        return Ok(deliver_all(&members, &env));
    }

    let Some(peer) = p.find(to) else {
        anyhow::bail!("{}", unknown_name(p.peers(), to));
    };
    let env = p.envelope(text).waking(wake.unwrap_or(true));
    Ok(match peer.deliver(&env)? {
        Delivery::Delivered => format!("delivered to {}", peer.meta.id),
        Delivery::Queued => format!(
            "written to {}'s inbox, but it is not answering; it will read it on recovery",
            peer.meta.id
        ),
    })
}

/// Send `text` as an outside caller — `eidolon send`, which has a roster
/// and no presence of its own.
///
/// The same rules as [`send`]: an unknown name is an error that lists who
/// is there, a direct message wakes by default and a channel fan-out does
/// not — and waking steers a recipient mid-turn rather than only starting
/// an idle one. The envelope is marked external, so the recipient's model
/// is told this is a tool injecting text — never its operator, never a
/// colleague session — which is what makes the CLI safe to hand to an
/// automation.
pub fn send_external(
    root: &std::path::Path,
    cwd: &std::path::Path,
    from: &str,
    to: &str,
    text: &str,
    wake: Option<bool>,
) -> Result<String> {
    if text.trim().is_empty() {
        anyhow::bail!("a message needs something in it");
    }
    if to.trim().is_empty() {
        anyhow::bail!(
            "say who to send to: a session id from `eidolon peers`, or `{CHANNEL}` for everyone on this project"
        );
    }
    if from.trim().is_empty() {
        anyhow::bail!("say who the message is from: --from NAME");
    }

    let peers = crate::presence::scan(root);
    if to == CHANNEL {
        let key = crate::presence::channel_key_of(cwd);
        let members: Vec<crate::presence::Peer> = peers
            .iter()
            .filter(|q| q.meta.repo.as_ref().unwrap_or(&q.meta.cwd).as_path() == key)
            .cloned()
            .collect();
        if members.is_empty() {
            anyhow::bail!("no session is working on this project, so there is nobody on the channel");
        }
        let env = Envelope::new(from, &cwd.display().to_string(), text)
            .on_channel(&key.display().to_string())
            .external()
            .waking(wake.unwrap_or(false));
        return Ok(deliver_all(&members, &env));
    }

    let Some(peer) = crate::presence::find_in(&peers, to) else {
        anyhow::bail!("{}", unknown_name(peers, to));
    };
    let env = Envelope::new(from, &cwd.display().to_string(), text)
        .external()
        .waking(wake.unwrap_or(true));
    Ok(match peer.deliver(&env)? {
        Delivery::Delivered => format!("delivered to {}", peer.meta.id),
        Delivery::Queued => format!(
            "written to {}'s inbox, but it is not answering; it will read it on recovery",
            peer.meta.id
        ),
    })
}

/// Fan a message out, and say what became of it. A peer that is not
/// answering still has the message on disk, so an unanswered ring is a
/// *report*, not a failure — saying "sent" would be a lie and failing the
/// whole fan-out over one wedged session would be worse.
fn deliver_all(members: &[crate::presence::Peer], env: &Envelope) -> String {
    let mut delivered = Vec::new();
    let mut queued = Vec::new();
    for q in members {
        match q.deliver(env) {
            Ok(Delivery::Delivered) => delivered.push(q.meta.id.clone()),
            Ok(Delivery::Queued) => queued.push(q.meta.id.clone()),
            Err(e) => queued.push(format!("{} ({e:#})", q.meta.id)),
        }
    }
    let mut out = String::new();
    if !delivered.is_empty() {
        out.push_str(&format!("delivered to {}", delivered.join(", ")));
    }
    if !queued.is_empty() {
        if !out.is_empty() {
            out.push_str("; ");
        }
        out.push_str(&format!(
            "queued for {} (not answering, will read it on recovery)",
            queued.join(", ")
        ));
    }
    out
}

/// The error for a name that is nobody: a dead end turned into one retry
/// by saying who *is* there.
fn unknown_name(peers: Vec<Peer>, to: &str) -> String {
    let names: Vec<String> = peers.into_iter().map(|q| q.meta.id).collect();
    if names.is_empty() {
        format!("there is no session `{to}`, and no other session is running at all")
    } else {
        format!(
            "there is no session `{to}`. Running right now: {}.",
            names.join(", ")
        )
    }
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn one_line(text: &str) -> String {
    let flat: String = text
        .replace('\n', " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if flat.chars().count() > 60 {
        flat.chars().take(59).collect::<String>() + "…"
    } else {
        flat
    }
}

/// Shorten a path under `$HOME` for display. The roster shows `~/eidolon`
/// rather than `/home/noah/eidolon`, and so does the vault label a session
/// attributes its writes with — one rule for both, since both answer "which
/// tree is this session in".
pub fn tilde(path: &str) -> String {
    match std::env::var_os("HOME") {
        Some(h) => match path.strip_prefix(&*h.to_string_lossy()) {
            Some(rest) => format!("~{rest}"),
            None => path.to_string(),
        },
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_name_says_who_is_there_instead() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = Presence::register(&root, &cwd.join("a.eid"), &cwd, "mock", "a").unwrap();

        let e = send(&a, "eidolon-nope", "hello", None)
            .unwrap_err()
            .to_string();
        assert!(e.contains("no session `eidolon-nope`"), "{e}");
        assert!(e.contains("no other session is running"), "{e}");

        assert!(
            send(&a, "", "hello", None)
                .unwrap_err()
                .to_string()
                .contains("say who to send to")
        );
        assert!(
            send(&a, CHANNEL, "  ", None)
                .unwrap_err()
                .to_string()
                .contains("needs something in it")
        );
        assert!(
            send(&a, CHANNEL, "hi", None)
                .unwrap_err()
                .to_string()
                .contains("nobody on the channel")
        );
    }

    #[test]
    fn a_lone_session_says_so_rather_than_printing_an_empty_table() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = Presence::register(&root, &cwd.join("a.eid"), &cwd, "mock", "a").unwrap();
        let r = roster(&a);
        assert!(r.contains("No other sessions are running"), "{r}");
        assert!(r.contains(a.id()), "it still says who you are: {r}");
    }

    /// A presence with no served socket is not live, so these tests ring
    /// real doorbells — the same way the CLI's `build` does — rather than
    /// registering and wondering why nobody is listed. The notification
    /// receiver is forgotten rather than dropped: a real session's driver
    /// holds it, and a dropped one makes every ring read as "no longer
    /// taking messages".
    async fn registered(root: &std::path::Path, cwd: &std::path::Path, name: &str) -> std::sync::Arc<Presence> {
        let p =
            Presence::register(root, &cwd.join(format!("{name}.eid")), cwd, "mock", name).unwrap();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        std::mem::forget(rx);
        crate::socket::serve(&p.socket_path(), tx, tokio_util::sync::CancellationToken::new())
            .await
            .unwrap();
        p
    }

    #[tokio::test]
    async fn an_outside_sender_reaches_a_session_and_is_marked_external() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = registered(&root, &cwd, "a").await;

        let out = tokio::task::spawn_blocking({
            let (root, cwd, to) = (root.clone(), cwd.clone(), a.id().to_string());
            move || send_external(&root, &cwd, "aoide", &to, "rebase finished", None)
        })
        .await
        .unwrap()
        .unwrap();
        assert!(out.contains("delivered to"), "{out}");
        let mail = crate::inbox::drain(&a.inbox_dir());
        assert_eq!(mail.len(), 1);
        assert_eq!(mail[0].from, "aoide");
        assert!(mail[0].external, "the recipient must frame it as a tool's");
        assert!(mail[0].wake, "a direct message still wakes by default");

        // A unique prefix is enough, as for a session's own `send`.
        tokio::task::spawn_blocking({
            let (root, cwd, to) = (root.clone(), cwd.clone(), a.id()[..6].to_string());
            move || send_external(&root, &cwd, "aoide", &to, "again", Some(false))
        })
        .await
        .unwrap()
        .unwrap();
        let mail = crate::inbox::drain(&a.inbox_dir());
        assert_eq!(mail.len(), 1);
        assert!(!mail[0].wake, "the sender can queue instead of wake");

        let e = tokio::task::spawn_blocking({
            let (root, cwd) = (root.clone(), cwd.clone());
            move || send_external(&root, &cwd, "aoide", "eidolon-nope", "hi", None)
        })
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
        assert!(e.contains("no session `eidolon-nope`"), "{e}");
        assert!(e.contains(a.id()), "the error says who is there: {e}");
    }

    #[tokio::test]
    async fn an_outside_channel_send_reaches_the_project_and_wakes_nobody() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let cwd = tmp.path().join("proj");
        std::fs::create_dir_all(&cwd).unwrap();
        let a = registered(&root, &cwd, "a").await;
        let b = registered(&root, &cwd, "b").await;

        let out = tokio::task::spawn_blocking({
            let (root, cwd) = (root.clone(), cwd.clone());
            move || send_external(&root, &cwd, "aoide", CHANNEL, "heads up", None)
        })
        .await
        .unwrap()
        .unwrap();
        assert!(out.contains(a.id()), "{out}");
        assert!(out.contains(b.id()), "{out}");
        for p in [&a, &b] {
            let mail = crate::inbox::drain(&p.inbox_dir());
            assert_eq!(mail.len(), 1);
            assert!(mail[0].external);
            assert!(!mail[0].wake, "a fan-out does not wake by default");
            assert_eq!(
                mail[0].channel.as_deref(),
                Some(cwd.display().to_string().as_str())
            );
        }
    }
}

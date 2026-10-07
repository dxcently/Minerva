//! The agent gate: how a chat's own tools reach the perch, and how the perch knows which chat (slice AG).
//!
//! A door's agent calls `/hub/term/a/<verb>` and `/hub/mail/a/<verb>` through plugin tools. Those two prefixes are the only
//! paths whose gate 1 is not the master token or the login cookie: they take a **scoped** token instead ([`Scope`], one per
//! prefix, so a leaked mail token opens no terminal), and a master bearer or a cookie never authenticates there. `serve` asks
//! [`agent_scope`] about the raw path, and the dispatch in `term` and `mail` asks the same predicate ([`verb`]), so a path that is
//! routed as an agent route has always been through [`authenticate`], and the reverse.
//!
//! This is the seam; the gate is AG's. [`authenticate`] is a stub that refuses everything with 401, which is the fail-closed
//! answer: a perch whose agent gate is not built has no agent route open to any caller, and the 401 is what a caller with no
//! credential gets anywhere else behind the gate.

pub mod attrib;
pub mod token;

use hyper::body::Incoming;
use hyper::{Request, Response};

use crate::serve::{BoxBody, Ctx};

/// What the agent gate keeps: the two tokens and where they were written. Filled by [`start`]; `Default` is how [`Ctx`] gets one
/// before the hook runs (nothing here can be given the options at that point), so a field is interior-mutable or a `OnceLock`.
#[derive(Default)]
pub struct State {}

/// Which scoped token a path is behind. One token per scope, so a leaked mail token opens no terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Term,
    Mail,
}

/// The chat an authenticated agent call came from, found from the socket it arrived on (AG's `attrib`). `serve` puts it into the
/// request's extensions when [`authenticate`] lets a call through, and an agent route reads it from there: no route reads a chat id
/// out of a body, a query or a header, because the caller chooses all of those.
#[derive(Clone, Debug)]
pub struct Caller {
    pub chat: String,
}

/// The two agent namespaces, as `serve` sees a path before anything is parsed: `/hub/term/a/<verb>` and `/hub/mail/a/<verb>`, and
/// nothing else. `/hub/term/a`, `/hub/term/a/`, `/hub/term/a/x/y` and every percent-encoded spelling are not agent paths, so they get
/// today's master gate and then a 404 from the module's dispatch, never an agent route.
pub fn agent_scope(path: &str) -> Option<Scope> {
    if let Some(rest) = path.strip_prefix("/hub/term/") {
        return verb(rest).map(|_| Scope::Term);
    }
    if let Some(rest) = path.strip_prefix("/hub/mail/") {
        return verb(rest).map(|_| Scope::Mail);
    }
    None
}

/// `a/<verb>` as `term` and `mail` see the rest of their path: the verb when it is exactly that. A verb is lower case letters, digits,
/// `_` and `-`, starting with a letter, at most 32: no `/`, no `.`, no `%`.
pub fn verb(rest: &str) -> Option<&str> {
    let verb = rest.strip_prefix("a/")?;
    let mut chars = verb.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_lowercase());
    (first_ok && verb.len() <= 32 && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')).then_some(verb)
}

/// Gate 1 for an agent path: the scoped token for `scope`, in constant time, and the chat the socket belongs to. `Err` is the
/// answer to send (401, 403 or 404), boxed like the tree's, and nothing else is read from the request first. Stub: 401 for everyone.
pub fn authenticate(ctx: &Ctx, req: &Request<Incoming>, scope: Scope) -> Result<Caller, Box<Response<BoxBody>>> {
    token::authenticate(ctx, req, scope)
}

/// After the bind, before the boot lines (lib.rs): mint the scoped tokens and write the files the flags name. `Err` refuses the start.
pub fn start(ctx: &Ctx) -> anyhow::Result<()> {
    token::start(ctx)
}

/// After the drain, beside the perch's own token file's removal (lib.rs): take away the files [`start`] wrote. **Compare before
/// unlink**: a file is removed only if it still holds what *this* perch wrote into it (the scoped token for a token file, the url for
/// the url file), exactly as `sessions::drop_token_file` does for a door's. A path a flag named is where a *later* perch, or the
/// operator, may have put a newer file by now; a bare `remove_file` would take its credential away from whoever holds it. A file that
/// is gone, unreadable or someone else's is left alone and mentioned at debug level, never an error: this runs on the way out.
///
/// Also called when [`start`] failed part-way, so it removes what exists and is ours, and is quiet about the rest.
pub fn on_exit(ctx: &Ctx) {
    token::on_exit(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_verb_directly_under_the_two_prefixes_is_an_agent_path() {
        for (path, scope) in [
            ("/hub/term/a/list", Scope::Term),
            ("/hub/term/a/whoami", Scope::Term),
            ("/hub/term/a/term-read_2", Scope::Term),
            ("/hub/mail/a/send", Scope::Mail),
            ("/hub/mail/a/whoami", Scope::Mail),
        ] {
            assert_eq!(agent_scope(path), Some(scope), "{path}");
        }
        for not in [
            "/hub/term/a",
            "/hub/term/a/",
            "/hub/term/a/x/y",
            "/hub/term/a/List",
            "/hub/term/a/1st",
            "/hub/term/a/x.y",
            "/hub/term/a/%6cist",
            "/hub/term/%61/list",
            "/hub/terma/list",
            "/hub/term/t1/a/list",
            "/hub/term/events",
            "/hub/term",
            "/hub/mail/a",
            "/hub/mail/a/",
            "/hub/mail/nodes",
            "/hub/mail/b/send",
            "/hub/notes/a/list",
            "/hub/sessions/a/list",
            "/hub/a/list",
            "/s/a/list",
            "//hub/term/a/list",
            "/hub/term/a/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "",
        ] {
            assert_eq!(agent_scope(not), None, "{not:?} is not an agent path");
        }
    }

    #[test]
    fn the_verb_predicate_is_the_one_both_dispatches_use() {
        assert_eq!(verb("a/list"), Some("list"));
        assert_eq!(verb("a/"), None);
        assert_eq!(verb("a"), None);
        assert_eq!(verb("t1/a/list"), None);
        assert_eq!(verb("a/list/"), None);
        assert_eq!(verb(&format!("a/{}", "x".repeat(32))), Some("xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"));
        assert_eq!(verb(&format!("a/{}", "x".repeat(33))), None);
    }
}

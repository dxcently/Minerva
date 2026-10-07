//! The scoped agent tokens, their files, and the gate that checks them (slice AG).
//!
//! **Start.** With `--term-token-file` and `--mail-token-file` the perch mints one token per scope (256 bits each, fresh every start,
//! never the master token) and writes it `0600` to the file the flag names, in a directory that is `0700`; with `--agent-url-file` it
//! writes `http://<bound>/` beside them. Each file is written atomically, then **read back** the way a door's token file is read
//! ([`crate::spawn::read_token_file`]: no symlink, a regular file, ours, owner-only) and compared with what was written, so a start that
//! cannot make a private file refuses rather than serving with a credential another user could read. A flag not given writes nothing,
//! mints nothing, and leaves its scope with no token: nothing opens it. No token is in argv, in the environment, in a log line or in an
//! error: every message here names a path and never a value.
//!
//! **Exit.** Each file is removed only if it still holds what this perch put there ([`on_exit`]); a crash leaves the old, now rotated,
//! file, which opens nothing and is replaced by the next start.
//!
//! **The gate.** [`authenticate`] is the answer table in [`super`]. The comparison is constant time ([`crate::gate::token_ok`]) and
//! only ever against the scope's own token, so a scoped token is good for its own prefix and nowhere else.

use std::net::SocketAddr;
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use hyper::body::Incoming;
use hyper::header::{AUTHORIZATION, COOKIE, HOST};
use hyper::{HeaderMap, Method, Request, Response, StatusCode};

use super::{attrib, Caller, Scope};
use crate::serve::{json, problem, status, BoxBody, Ctx, Peer};
use crate::{gate, login, spawn};

/// The minted tokens, by scope. `None` is a scope nobody can open: its flag was not given.
#[derive(Default)]
pub(super) struct Tokens {
    term: Option<String>,
    mail: Option<String>,
}

impl Tokens {
    fn of(&self, scope: Scope) -> Option<&str> {
        match scope {
            Scope::Term => self.term.as_deref(),
            Scope::Mail => self.mail.as_deref(),
        }
    }
}

/// A file this perch wrote, what it put in it and which file it is (device and inode, from the descriptor it was read back through):
/// the compare-before-unlink key. Content alone is not one: the url file of two perches on one port holds the same bytes.
pub(super) struct Written {
    path: PathBuf,
    content: String,
    id: spawn::FileId,
}

/// The answer a caller gets when the socket it called from is not a live door of this perch. The words are the agent's to read: it is
/// the body of the tool's error, so it says what to do and not why it was refused.
fn not_a_door(scope: Scope) -> &'static str {
    match scope {
        Scope::Term => "term tools work in chats this Minerva opened",
        Scope::Mail => "mail tools work in chats this Minerva opened",
    }
}

// ---------------------------------------------------------------------------
// The files
// ---------------------------------------------------------------------------

/// Mint a term token and a mail token and write each `0600` to the file its flag names, and the url, when the flags are given. `master`
/// is the perch's own token file, which was written a moment ago and which none of these may be.
pub fn start(ctx: &Ctx, master: &Path) -> Result<()> {
    let opts = &ctx.opts;
    let named: Vec<(&str, &Path)> = [
        ("--term-token-file", opts.term_token_file.as_deref()),
        ("--mail-token-file", opts.mail_token_file.as_deref()),
        ("--agent-url-file", opts.agent_url_file.as_deref()),
    ]
    .into_iter()
    .filter_map(|(flag, path)| path.map(|path| (flag, path)))
    .collect();

    // Before anything is written. An absolute path, because a relative one names a file by where the perch happened to be started, and
    // the directory step below would otherwise be asked to tighten the working directory. Three different files, because one file
    // named twice holds one secret and not two.
    for (flag, path) in &named {
        anyhow::ensure!(path.is_absolute(), "{flag} {} is not an absolute path", path.display());
    }
    for (at, (flag, path)) in named.iter().enumerate() {
        if let Some((other, _)) = named[at + 1..].iter().find(|(_, other)| other == path) {
            anyhow::bail!("{flag} and {other} name the same file, {}", path.display());
        }
    }
    // Not the perch's own token file, by any spelling: writing a scoped token there would replace the master token with it, and the
    // launcher that reads that file would hold a credential that opens only the agent paths (and, at the exit, neither would be taken
    // back). Looked at before anything is written, with the directories resolved; and again after, by identity.
    for (flag, path) in &named {
        anyhow::ensure!(!same_place(path, master), "{flag} {} is the perch's own token file", path.display());
    }

    let mut tokens = Tokens::default();
    if let Some(path) = &opts.term_token_file {
        let token = harnox::crypto::random_token();
        put(ctx, path, &token)?;
        tokens.term = Some(token);
    }
    if let Some(path) = &opts.mail_token_file {
        let token = harnox::crypto::random_token();
        put(ctx, path, &token)?;
        tokens.mail = Some(token);
    }
    if let Some(path) = &opts.agent_url_file {
        // No newline: a tool reads this file as one line, and a trailing one is an empty second.
        put(ctx, path, &format!("http://{}/", ctx.bound))?;
    }

    // Every file, read back as a reader would find it. After all three are written, so that two flags that reached one file through
    // different spellings (a link, `a/../b`) are caught: the second write replaced the first, and the first no longer reads back.
    let written = ctx.agent.written.lock().unwrap();
    for Written { path, content, id } in written.iter() {
        let (found, now) = spawn::read_token_file_id(path).with_context(|| format!("reading back {}", path.display()))?;
        anyhow::ensure!(
            gate::token_ok(&found, content) && now == *id,
            "{} does not hold what was just written to it (two flags reaching one file, or another process writing there)",
            path.display()
        );
    }
    // And no agent file is the perch's own, whichever way it was spelled: the same device and inode as the master token file.
    if let Ok(meta) = std::fs::symlink_metadata(master) {
        let master_id = (meta.dev(), meta.ino());
        if let Some(Written { path, .. }) = written.iter().find(|written| written.id == master_id) {
            anyhow::bail!("{} is the perch's own token file", path.display());
        }
    }
    drop(written);
    ctx.agent.tokens.set(tokens).map_err(|_| anyhow::anyhow!("the agent gate was started twice"))?;
    Ok(())
}

/// Whether two paths name the same place for a file about to be written: equal, or the same name in the same directory once the
/// directories are resolved (`a/../b/f` and `b/f`, a link to a directory). A directory that does not exist yet cannot be the master
/// token file's.
fn same_place(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    let resolved = |path: &Path| Some(std::fs::canonicalize(path.parent()?).ok()?.join(path.file_name()?));
    matches!((resolved(a), resolved(b)), (Some(a), Some(b)) if a == b)
}

/// One file: its directory private, the content written atomically at `0600`, read back, and recorded as ours at once (with the file's
/// identity), so a `start` that fails on the next step still takes this one back in [`on_exit`].
fn put(ctx: &Ctx, path: &Path, content: &str) -> Result<()> {
    let dir = path.parent().with_context(|| format!("{} has no directory", path.display()))?;
    private_dir(dir)?;
    harnox::fs::write_atomic_0600(path, content.as_bytes()).with_context(|| format!("writing {}", path.display()))?;
    let (found, id) = spawn::read_token_file_id(path).with_context(|| format!("reading back {}", path.display()))?;
    anyhow::ensure!(gate::token_ok(&found, content), "{} does not hold what was just written to it", path.display());
    ctx.agent.written.lock().unwrap().push(Written { path: path.to_path_buf(), content: content.to_string(), id });
    Ok(())
}

/// The directory a secret goes in. **An existing directory is never chmod'ed**: it is somebody's, and tightening it would change what
/// they meant it to be. It must be ours and not writable by group or others (so nobody else can rename or replace a file in it), or the
/// start is refused with a sentence. A missing one is made `0700`, every level of it. Then the chain above it: every ancestor must be
/// owned by us or root and, if group- or other-writable at all, sticky (`/tmp`), because whoever can rename a directory on the path can
/// put another directory, and their own files, where this one was.
fn private_dir(dir: &Path) -> Result<()> {
    let ours = unsafe { libc::getuid() };
    if !dir.exists() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let meta = std::fs::metadata(dir).with_context(|| format!("checking {}", dir.display()))?;
    anyhow::ensure!(meta.is_dir(), "{} is not a directory", dir.display());
    crate::owned_by(meta.uid(), ours).with_context(|| format!("checking {}", dir.display()))?;
    let mode = meta.mode() & 0o7777;
    anyhow::ensure!(
        mode & 0o022 == 0,
        "{} is writable by group or others (mode {mode:o}); a token file is not put where someone else can replace it",
        dir.display()
    );
    let real = std::fs::canonicalize(dir).with_context(|| format!("resolving {}", dir.display()))?;
    for above in real.ancestors().skip(1) {
        let meta = std::fs::metadata(above).with_context(|| format!("checking {}", above.display()))?;
        let mode = meta.mode() & 0o7777;
        anyhow::ensure!(
            mode & 0o022 == 0 || mode & 0o1000 != 0,
            "{} (above {}) is writable by group or others and not sticky (mode {mode:o}); someone else could swap the directory below it",
            above.display(),
            dir.display()
        );
        anyhow::ensure!(
            meta.uid() == ours || meta.uid() == 0,
            "{} (above {}) is owned by uid {}, neither ours ({ours}) nor root",
            above.display(),
            dir.display(),
            meta.uid()
        );
    }
    Ok(())
}

/// Remove each file this perch wrote, only while it is still the same file and still holds what this perch put there (compare before
/// unlink: see [`super::on_exit`], and `sessions::drop_token_file`). The read refuses a link, a FIFO, a file that is not ours and one
/// that is not owner-only, and all of those are left in place: nothing here is anything but a debug line.
pub fn on_exit(ctx: &Ctx) {
    let written = std::mem::take(&mut *ctx.agent.written.lock().unwrap());
    for Written { path, content, id } in written {
        match spawn::read_token_file_id(&path) {
            Ok((found, now)) if now == id && gate::token_ok(&found, &content) => {
                if let Err(e) = std::fs::remove_file(&path) {
                    tracing::debug!(error = %e, path = %path.display(), "perch: an agent file was not removed");
                }
            }
            Ok(_) => tracing::debug!(path = %path.display(), "perch: an agent file is not the file this perch wrote (another file, or other bytes); left in place"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::debug!(error = %e, path = %path.display(), "perch: an agent file did not read; left in place"),
        }
    }
}

// ---------------------------------------------------------------------------
// The gate
// ---------------------------------------------------------------------------

/// `Authorization: Bearer <x>`, spelled as `fetch` and the tools write it. The first header, as the master gate reads it.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(AUTHORIZATION).and_then(|h| h.to_str().ok()).and_then(|value| value.strip_prefix("Bearer "))
}

/// Whether `presented` is `want`, in constant time. A scope with no token (`None`) is never matched, and neither is an empty one: an
/// empty bearer against an empty token would otherwise be a match for a scope nobody started.
fn matches(presented: &str, want: Option<&str>) -> bool {
    want.is_some_and(|want| !want.is_empty() && gate::token_ok(presented, want))
}

/// The master credential, from either place a browser or a launcher holds it: the perch token as a bearer, or the login cookie (which
/// counts only on a request that says it is `same-origin`, exactly as it does behind the master gate). It never authenticates here; it
/// is only recognised, so that its holder is told "not here" and a stranger is told nothing.
fn master_presented(ctx: &Ctx, headers: &HeaderMap, presented: Option<&str>) -> bool {
    if presented.is_some_and(|presented| gate::token_ok(presented, &ctx.token)) {
        return true;
    }
    if headers.get("sec-fetch-site").and_then(|h| h.to_str().ok()) != Some("same-origin") {
        return false;
    }
    // The name serve.rs gives the cookie (`cookie_name`): the port is in it because cookies are not isolated by port.
    let name = format!("minerva_{}", ctx.bound.port());
    headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|h| h.to_str().ok())
        .filter_map(|header| login::cookie_value(header, &name))
        .fold(false, |ok, value| ok | ctx.logins.session_ok(value))
}

/// The scoped token for `scope` in constant time, then the chat the socket belongs to ([`super::attrib`]). See the table in [`super`].
/// Nothing is looked up in `/proc` for a caller that did not first present the scope's token.
pub fn authenticate(ctx: &Ctx, req: &Request<Incoming>, scope: Scope) -> Result<Caller, Box<Response<BoxBody>>> {
    let headers = req.headers();
    let presented = bearer(headers);
    let own = ctx.agent.tokens.get().and_then(|tokens| tokens.of(scope));
    if !presented.is_some_and(|presented| matches(presented, own)) {
        let code = if master_presented(ctx, headers, presented) { StatusCode::NOT_FOUND } else { StatusCode::UNAUTHORIZED };
        return Err(Box::new(status(code)));
    }

    // Only the kernel's word about where the connection came from: `Peer` is an extension the accept loop sets and a header cannot.
    let chat = req.extensions().get::<Peer>().and_then(|Peer(peer)| attrib::chat_of_hinted(&ctx.registry, &ctx.agent.hints, *peer, ctx.bound));
    let Some(chat) = chat else {
        tracing::debug!(?scope, "perch: an agent call from a socket no live door owns");
        return Err(Box::new(problem(StatusCode::FORBIDDEN, not_a_door(scope))));
    };

    // `whoami` is the gate's own verb: the answer is the attribution, so it is made here and no module has to. It still owes the
    // gates `serve` would have run between this one and a route (a stop under way, the `Host`, fetch metadata, the method), in order.
    if req.uri().path().rsplit('/').next() == Some("whoami") {
        return Err(Box::new(whoami(ctx, req, &chat)));
    }
    Ok(Caller { chat })
}

/// `POST /hub/<scope>/a/whoami` -> `{"chat": <id>}`, for a caller the gate has already attributed.
fn whoami(ctx: &Ctx, req: &Request<Incoming>, chat: &str) -> Response<BoxBody> {
    whoami_answer(ctx.stop.is_cancelled(), req.headers(), req.method(), ctx.bound, chat)
}

/// The gates `serve` runs between gate 1 and a route, in its order (a stop under way, the `Host`, fetch metadata, the method), then the
/// answer. Apart from the request so each gate can be asked on its own.
fn whoami_answer(stopping: bool, headers: &HeaderMap, method: &Method, bound: SocketAddr, chat: &str) -> Response<BoxBody> {
    if stopping {
        return status(StatusCode::SERVICE_UNAVAILABLE);
    }
    if !gate::host_ok(headers.get(HOST).and_then(|h| h.to_str().ok()), bound) {
        return status(StatusCode::FORBIDDEN);
    }
    if !gate::sec_fetch_ok(headers.get("sec-fetch-site").and_then(|h| h.to_str().ok())) {
        return status(StatusCode::FORBIDDEN);
    }
    if method != Method::POST {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    }
    json(StatusCode::OK, &serde_json::json!({ "chat": chat }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scope_with_no_token_opens_for_nobody_and_an_empty_one_for_no_one() {
        assert!(matches("abc", Some("abc")));
        assert!(!matches("abd", Some("abc")));
        assert!(!matches("abc", Some("abcd")), "a prefix is not the token");
        assert!(!matches("abc", None), "no flag, no token, nothing matches");
        assert!(!matches("", None));
        assert!(!matches("", Some("")), "an empty bearer is not an empty token's match");
    }

    #[test]
    fn the_bearer_is_the_exact_scheme_and_nothing_else() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(AUTHORIZATION, value.parse().unwrap());
            headers
        };
        assert_eq!(bearer(&with("Bearer abc")), Some("abc"));
        assert_eq!(bearer(&with("Bearer ")), Some(""));
        assert_eq!(bearer(&with("bearer abc")), None, "the scheme is spelled as fetch spells it");
        assert_eq!(bearer(&with("Basic abc")), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[test]
    fn a_scope_takes_its_own_token_only() {
        let tokens = Tokens { term: Some("t".into()), mail: None };
        assert_eq!(tokens.of(Scope::Term), Some("t"));
        assert_eq!(tokens.of(Scope::Mail), None);
    }

    #[test]
    fn the_refusal_names_the_scope_and_never_a_value() {
        assert_eq!(not_a_door(Scope::Term), "term tools work in chats this Minerva opened");
        assert_eq!(not_a_door(Scope::Mail), "mail tools work in chats this Minerva opened");
    }

    #[test]
    fn a_missing_directory_is_made_owner_only_and_an_existing_one_is_judged_and_never_chmoded() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = tempfile::tempdir().unwrap();
        let mode_of = |dir: &Path| std::fs::metadata(dir).unwrap().permissions().mode() & 0o7777;
        // Missing, and several levels deep: every level made is `0700`.
        let deep = base.path().join("a").join("b").join("c");
        private_dir(&deep).unwrap();
        for dir in [base.path().join("a"), base.path().join("a/b"), deep.clone()] {
            assert_eq!(mode_of(&dir), 0o700, "{}", dir.display());
        }
        // There already and not writable by others: accepted as it is, whatever else it allows (0755 is the common home directory).
        std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&deep).unwrap();
        assert_eq!(mode_of(&deep), 0o755, "an existing directory is not chmod'ed");
        // Writable by group or by others: refused with a sentence, and left exactly as it was.
        for loose in [0o775, 0o757, 0o777, 0o770 | 0o002] {
            std::fs::set_permissions(&deep, std::fs::Permissions::from_mode(loose)).unwrap();
            let why = private_dir(&deep).unwrap_err().to_string();
            assert!(why.contains("writable by group or others") && why.contains(&deep.display().to_string()), "{loose:o}: {why}");
            assert_eq!(mode_of(&deep), loose, "{loose:o} is left as it was");
        }
        // A file where the directory should be.
        let file = base.path().join("file");
        std::fs::write(&file, "x").unwrap();
        assert!(private_dir(&file).is_err());
        assert!(private_dir(&file.join("below")).is_err());
    }

    #[test]
    fn a_directory_below_one_someone_else_can_swap_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let base = tempfile::tempdir().unwrap();
        let above = base.path().join("above");
        let dir = above.join("agent");
        std::fs::create_dir_all(&dir).unwrap();
        // The directory itself is ours and 0700; the one above it is not safe to trust.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&above, std::fs::Permissions::from_mode(0o777)).unwrap();
        let why = private_dir(&dir).unwrap_err().to_string();
        assert!(why.contains("not sticky") && why.contains("above"), "{why}");
        // Sticky, as `/tmp` is: the one above can add names, and not replace or rename another's.
        std::fs::set_permissions(&above, std::fs::Permissions::from_mode(0o1777)).unwrap();
        private_dir(&dir).unwrap();
        // Through a link: the chain that counts is the one the directory really lives in.
        let link = base.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        std::fs::set_permissions(&above, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(private_dir(&link).is_err(), "a link does not hide what is above its target");
        std::fs::set_permissions(&above, std::fs::Permissions::from_mode(0o700)).unwrap();
        private_dir(&link).unwrap();
    }

    #[test]
    fn two_spellings_of_one_place_are_one_place() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("d");
        std::fs::create_dir(&dir).unwrap();
        let plain = dir.join("x");
        assert!(same_place(&plain, &plain));
        assert!(same_place(&plain, &dir.join("..").join("d").join("x")), "a `..` that comes back");
        let link = base.path().join("link");
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(same_place(&plain, &link.join("x")), "a link to the directory");
        assert!(!same_place(&plain, &dir.join("y")), "another name");
        assert!(!same_place(&plain, &base.path().join("x")), "another directory");
        assert!(!same_place(&plain, &base.path().join("missing").join("x")), "a directory that is not there is nobody's yet");
    }

    #[test]
    fn whoami_runs_the_gates_serve_would_have_in_serves_order() {
        let bound: SocketAddr = "127.0.0.1:4477".parse().unwrap();
        let headers = |host: Option<&str>, site: Option<&str>| {
            let mut headers = HeaderMap::new();
            if let Some(host) = host {
                headers.insert(HOST, host.parse().unwrap());
            }
            if let Some(site) = site {
                headers.insert("sec-fetch-site", site.parse().unwrap());
            }
            headers
        };
        let code = |stopping: bool, headers: &HeaderMap, method: &Method| whoami_answer(stopping, headers, method, bound, "chat-a").status();
        let good = headers(Some("127.0.0.1:4477"), None);
        assert_eq!(code(false, &good, &Method::POST), StatusCode::OK);
        assert_eq!(code(false, &headers(Some("localhost:4477"), Some("same-origin")), &Method::POST), StatusCode::OK);
        assert_eq!(code(false, &headers(Some("127.0.0.1:4477"), Some("none")), &Method::POST), StatusCode::OK);
        // Each gate on its own.
        assert_eq!(code(true, &good, &Method::POST), StatusCode::SERVICE_UNAVAILABLE, "a stop under way");
        assert_eq!(code(false, &headers(Some("evil.example"), None), &Method::POST), StatusCode::FORBIDDEN, "a Host that is not this perch's");
        assert_eq!(code(false, &headers(None, None), &Method::POST), StatusCode::FORBIDDEN, "no Host at all");
        assert_eq!(code(false, &headers(Some("127.0.0.1:4477"), Some("cross-site")), &Method::POST), StatusCode::FORBIDDEN, "cross-site");
        assert_eq!(code(false, &headers(Some("127.0.0.1:4477"), Some("same-site")), &Method::POST), StatusCode::FORBIDDEN, "same-site");
        assert_eq!(code(false, &good, &Method::GET), StatusCode::METHOD_NOT_ALLOWED, "GET");
        assert_eq!(code(false, &good, &Method::PUT), StatusCode::METHOD_NOT_ALLOWED, "PUT");
        // The order: the stop beats everything, then the Host, then fetch metadata, then the method.
        let all_bad = headers(Some("evil.example"), Some("cross-site"));
        assert_eq!(code(true, &all_bad, &Method::GET), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(code(false, &all_bad, &Method::GET), StatusCode::FORBIDDEN);
        assert_eq!(code(false, &headers(Some("127.0.0.1:4477"), Some("cross-site")), &Method::GET), StatusCode::FORBIDDEN);
    }
}

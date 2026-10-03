# src/ — the perch's nine modules

One job each, and the reason for the split is the tests: everything a unit test has to
reach is a plain function here, not a closure inside `main`.

```text
main.rs    argv, SIGINT/SIGTERM -> CancellationToken, the runtime `run` needs
lib.rs     bind, the runtime dir, the token mint and its removal on exit
gate.rs    loopback_only, token_ok (subtle), host_ok, sec_fetch_ok
serve.rs   accept loop, the gate order, the route match, the H1b session routes, shutdown
files.rs   the static rules, ported from eidolon/crates/web/src/files.rs
tree.rs    GET /hub/tree: parse_query, the deny list, containment, the listing, the 4-listing slots
sessions.rs  the doors we own, the list (kept + ours + roster), the pane count, the ordered stop
spawn.rs     the argv, the fork, the boot lines, the door's token file, PDEATHSIG
watch.rs     the perch's own /api/events stream to a door, and closing it before SIGTERM; reads only until `hello` (capped at 1 MiB), holds no transcript
proxy.rs     GET|POST /s/<id>/api/<route> to that session's door, panes, the hub-stream-end frame
bin/fake_eidolon.rs   the stand-in door, built only with `--features test-bins`
```

`lib.rs` is the crate's whole surface (`Options`, `run`) and `main.rs` does nothing but
parse argv, install the signal handlers and hand over. `files.rs` is a **port, not a
dependency**: the door's containment rules and their tests came across, because the perch
knows a door only as a binary and HTTP, and must not link an `eidolon-*` crate.

Hand-formatted, no `cargo fmt`: a line that reads better unbroken stays unbroken, and the
comments carry the *why* of a gate rather than its restatement.

**H1b is lifecycle**: spawn, resume, stop and list, the watcher that makes a door `live`, and
the close-then-signal order a stop needs.

**`proxy.rs` is H1c, the browser-facing half.** `GET|POST /s/<id>/api/<route>` is rebuilt, not
forwarded. The path is a **shape, not an allowlist** (`^[a-z][a-z0-9-]{0,31}(/[a-z0-9-]{1,32})*$`
after `/s/<id>/api/`, any `%` refused), so a route the door grows later passes with no change
here. A malformed path is a bare 404; a session that is not live is 503 `dead`, 409 (`kept`,
or the state it is between) or 404, with no connection made. The perch writes the door's
Bearer into the request it sends *to that door*, rewrites `Host` to `127.0.0.1:<door port>`,
and passes **only `Content-Type` and `Accept`**: no `Cookie`, `Origin`, `Referer`, `Sec-*`,
`Last-Event-ID`, nor the page's own `Authorization`. The answer carries the door's status and
`content-type` plus `cache-control: no-store`; a door's 401 becomes a 502, so the page never
concludes its own token is bad. A POST body streams to the door through `Limited` at 8 MiB,
uncollected (a declared `Content-Length` over the cap is a 413 before any connect), and a POST
has a 60 s head deadline where a GET stream has none. A GET of `events` is a **pane**, counted
per door while its body lives: its frames pass through as bytes, unbuffered, and it always
ends with one frame of the perch's own, `hub-stream-end` (`door` is `live`, `stopping` or
`dead`, plus the exit when known). Each pane has a task of its own that owns the upstream
socket and the count and is cancelled by a stop, so a page that has stopped reading cannot
hold a stop up.

**The stop order** is cancel the panes, close the watcher, wait for both, then SIGTERM.
`serve::run` bounds the graceful shutdown at 3 s (`RESPONSE_DRAIN`, above a pane's 2 s drain
and well under the door's 10 s), so the perch's token file is removed on that path too.

**Deferred to H2**: the idle reaper, `/hub/events`, `/hub/mesh` (the mesh feed is
H3), and telling "goodbye, then exit" from a crash: the `goodbye` frame is not parsed, so that
door's pane ends as `dead` with `code:0`, not `stopping` (its list row is `kept`: code 0 is clean).

**H2 notes** (post-H1c review, `logs/agents/perch-post-h1c.review.md`). `dead(id, pid)` is keyed
by pid as well as id, like `forget`, so a stop's late `dead` cannot mark a newer door of the same
id. `GET /hub/tree` re-checks the cwd against `--root` on every request (`Registry::tree_cwd`) and
judges the cwd's own segments below that root; listings hold a `tree::Slots` permit (4) and scan
at most 100 000 entries. **Open for H2, not fixed (review finding 6):** a recycled pid makes a
stale roster row read `live-elsewhere` (`kill(pid, 0)`, as upstream's `pid_live`), so resume,
`/hub/tree` and `/s` refuse with 409 until the stale `meta.json` is deleted by hand. The cure is to
also require the process start time in `/proc/<pid>/stat` to be earlier than the row's `meta.json`
mtime.

**Unit tests live here, in `#[cfg(test)] mod tests`** — they need private items
(`gated_path`, `decode`, `owned_by`, `in_gated`, `set_owner_only`), which moving them out
would break. Anything that needs a socket or the real binary is in [`../tests/`](../tests/).

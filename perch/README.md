# perch — the Minerva front door

**One loopback process in front of several `eidolon web` doors.** The browser UI in
[`webui/term/`](../webui/term/) is served by the perch instead of by a door, so one page
can tile many sessions, and each session's own door token stays inside the machine —
the page ever holds only the perch's token.

`perch` is the binary. Conceptually it is `minerva perch`; there is no `minerva`
multiplexer binary yet, so it is invoked as `perch` on its own. The design it implements
is [`docs/design/hub.md`](../docs/design/hub.md), which still calls it "the hub" — the
prose rename is a separate human edit, not a code change.

| | |
|---|---|
| **Loopback only** | It refuses any bind that is not `127.0.0.0/8` or `::1`, before a listener exists, and there is no flag that widens it. Its token is a file on this machine, not a credential for a network. |
| **MIT** | Same license as the rest of Minerva. It depends on `harnox` (MIT, a sibling checkout) and on no `eidolon-*` crate at all: a door is a binary, two stdout lines, a `0600` token file and HTTP, and nothing more. No `aoide-*` crate either — those are GPL-3.0. |
| **Unix only** | It needs `$XDG_RUNTIME_DIR`, `0600` files and signals. A non-Unix build fails at `compile_error!` rather than at the first `chmod`. |

## Run it

The one command, from WSL (starts or reuses the perch on port 47820 and opens the web UI in
the Windows browser; the token goes from its `0600` file into a `0600` redirect file and never
through argv, stdout or a URL you type):

```sh
bin/perch-open.sh              # start or reuse, open the browser
bin/perch-open.sh --no-browser # print the redirect file's Windows path instead
bin/perch-open.sh --status     # up or down, url, pid, session counts by state
bin/perch-open.sh --stop
```

It needs the binary at `~/perch-target/release/perch` (`MINERVA_PERCH_BIN` to override):
`cd perch && CARGO_TARGET_DIR=$HOME/perch-target cargo build --release`. `MINERVA_PERCH_PORT`,
`MINERVA_UI_DIR` and `MINERVA_OPEN_TTL` (seconds the redirect file lives, default 60) are optional.
It never passes `--sessions-dir`, so the perch's default stays equal to the door's own.

By hand:

```sh
cd perch
cargo build --release
# the UI bundle it serves is webui/term/ in this repository
target/release/perch --bind 127.0.0.1:0 --ui-dir ../webui/term
# …and, for the session lifecycle (H1b), the flags a door needs:
target/release/perch --bind 127.0.0.1:0 --ui-dir ../webui/term \
  --root "$HOME" --eidolon eidolon --sessions-dir "$HOME/.local/share/eidolon/sessions"
```

It prints exactly two lines and then serves:

```text
listening on http://127.0.0.1:41231/
token file: /run/user/1000/minerva/perch-41231.token
```

The **token is never printed**, never in argv, never in the environment, never in a URL
and never in a log. Read it from the file the second line names (mode `0600`), and give
it to the page as `Authorization: Bearer <token>`. `$XDG_RUNTIME_DIR` is required: the
perch will not fall back to a shared temp directory, because another user could
pre-create the directory its secret goes in.

## What this slice is

This is **H1c** on top of H1a's front door and H1b's session lifecycle. H1a was the process,
its one secret, the gates every request passes and the static UI; H1b was the session
lifecycle behind those gates — spawn a door, resume a kept log, stop a live one, list them —
plus the perch's *own* watcher stream to every door it owns. **H1c is the proxy**: the page
reaches a session's door only through `/s/<id>/api/<route>`, with the door's Bearer injected
by the perch, and a stop cancels every pane and closes the watcher **before** it signals the
door.

What is **not** here, on purpose: the idle reaper, `/hub/events` (H2) and
`/hub/mesh` (H3).

```text
  /            the UI bundle from --ui-dir: no token, `Host` checked
  /hub/sessions                 GET the list, POST to spawn a new session   (H1b)
  /hub/sessions/<id>/resume     POST: a door on that kept log                (H1b)
  /hub/sessions/<id>/stop       POST: cancel panes, close watcher, SIGTERM   (H1b, H1c)
  /s/<id>/api/<route>           GET|POST, proxied to that session's door     (H1c)
  /hub/tree?session=<id>&path=<rel>   GET one directory of a live cwd     (H2)
  /hub/events, /hub/mesh                   ->  404                           (H2/H3)
  any other /s/...              token-gated  ->  404
```

An **unauthenticated** request to `/hub` or `/s` is a 401; an authenticated one to a route
that is not there is a 404. The proxied path is a shape, not a list of routes
(`^[a-z][a-z0-9-]{0,31}(/[a-z0-9-]{1,32})*$`), so a route the door grows needs no perch change.
Only `Content-Type` and `Accept` cross to the door, a POST body streams under the 8 MiB cap,
and a pane (`GET .../api/events`) ends with a `hub-stream-end` frame saying whether the door
is `live`, `stopping` or `dead`. A door's 401 reaches the page as a 502.

**A clean exit is `kept`, not `dead`.** A door that exits with code 0, or by SIGTERM after a stop was
asked of it, leaves no row of ours: the list shows its log as `kept` and `/s/<id>/...` answers 409
`kept`. A SIGKILL, any other code or signal, or a spawn that never came up stays `dead` (503, never
connected to). The roster (`live-elsewhere`) counts a row only while its pid is alive (`kill(pid, 0)`,
`EPERM` is alive); a crashed door's leftover `meta.json` is ignored, never deleted.

### The gates, in order (hub.md §10.2)

| # | Gate | Refusal |
|---|---|---|
| 0 | loopback-only bind, at start | no start |
| 1 | `Authorization: Bearer` on `/hub`, `/hub/*`, `/s`, `/s/*` — never a query parameter, constant time | 401, empty body |
| 2 | `Host` is the bound `ip:port` (or literally `localhost:port`) | 403 |
| 3 | `Sec-Fetch-Site`, when present, is `same-origin` or `none` | 403 |
| 4 | method and path shape; `OPTIONS` is 405 everywhere | 404 / 405 |
| 5 | body read through `Limited` at 8 MiB | 413 |

A query string on `/s/...` is 400 (`/hub` keeps its query strings — `/hub/tree?…` is
one). No `Access-Control-*` header is ever written, and `OPTIONS` never gets a preflight
answer. Static responses carry `nosniff`, `no-referrer`, `no-cache` and the bundle's CSP
on refusals as well as 200s; every response carries `nosniff` and `no-referrer`.

### `GET /hub/tree` (H2)

`/hub/tree?session=<id>&path=<rel>` lists **one directory** under a **live** session's cwd
(the `cwd` its `hello` named, canonicalized when the request is served). Names and kinds only:
no file contents, no link target, no absolute path.

```text
  query    exactly `session` and `path` (optional = the root), once each, decoded once   400
  session  not an id / unknown                                                           404
           kept, live-elsewhere, dead, starting, stopping                                409 {"state": "..."}
  path     NUL, `\`, absolute, `.`/`..`/empty segment, > 4096 B                          404
           any segment on the deny list or shaped like an NTFS 8.3 alias (`~` + digit),
           or outside the cwd after canonicalize,
           or not a directory, or a cwd outside every --root / under a denied name       404
  503      four listings already running (a short JSON reason, not a queue)
  200      {"path":"src","entries":[{"name","kind":"file","size"}|{"name","kind":"dir"}|
           {"name","kind":"link"}|{"name","kind":"other"}],"skipped":0,"truncated":false}
```

Deny list (case-insensitive, whole name; matched on the asked path and again on the canonical
path, so a link inside the tree cannot launder one): `.git`, `.env`, `.env.*`, `*.key`,
`*.pem`, `*.p12`, `*.pfx`, `*.kdbx`, `id_*`, `*secret*`, `*.token`, `.netrc`, `.ssh`, `.gnupg`,
`.aoide`, `.envrc`, `.pgpass`, `.npmrc`, `.pypirc`, `auth.json`, `creds.json`, `*credentials*`. A
denied entry is dropped from a listing silently. A path segment (or a segment of the cwd below
its `--root`) containing `~` then a digit is refused too: on NTFS (`/mnt/c`) `GIT~1` is `.git`.
The cwd is checked against `--root` on every request, so a resumed session whose log named a
directory outside it lists nothing, and a cwd that is itself a denied name (`~/.ssh`) lists nothing.
Kinds come from `d_type` (a link is `link` and never followed, even one that points out of the
tree); only the kept regular files are `lstat`ed, for their size. At most 100 000 entries are
scanned per directory (more is `truncated`) and 4 listings run at once. Asking for a link as a
`path` canonicalizes it, and a target outside the cwd is a 404.
Non-UTF-8 names are counted in `skipped`. Dirs first, then by name (byte order); 2 000 entries
at most, then `truncated: true`. There is no config knob yet.

## Layout

```text
src/main.rs     argv, SIGINT/SIGTERM -> CancellationToken, the runtime `run` needs
src/lib.rs      bind, the runtime dir, the token mint and its removal on exit
src/gate.rs     loopback_only, token_ok (subtle), host_ok, sec_fetch_ok
src/serve.rs    accept loop, the gate order, the route match, the session routes, shutdown
src/files.rs    the static rules, ported from eidolon/crates/web/src/files.rs
src/tree.rs     GET /hub/tree: one directory under a live cwd, names and kinds only
src/sessions.rs the door table, the list, the id regex, the roster scan, the pane count, the ordered stop
src/spawn.rs    argv, the fork (one long-lived thread, PDEATHSIG), the boot lines, the token file
src/watch.rs    the perch's own /api/events stream to a door, and the handle that closes it
src/proxy.rs    /s/<id>/api/<route>: the path shape, the rebuilt request, panes, hub-stream-end
src/bin/fake_eidolon.rs   the stand-in door the tests drive, `--features test-bins` only
tests/http.rs   a real perch on 127.0.0.1:0, over a real socket, against a real child door
```

`src/files.rs` is a **port, not a dependency**: `eidolon/crates/web/src/files.rs` is the
source of the containment rules (one percent-decode, no `.`/`..`/empty segments, no NUL
or backslash, canonicalize under a root canonicalized once, regular files only, 32 MiB)
and its tests came with it. The perch must not link `eidolon-web`, so the rules live here
too.

`cargo test` starts real servers and speaks HTTP/1.1 to them by hand; the token is read
out of the file the child printed, and the child runs at `RUST_LOG=trace` so a token that
reached a log line would fail the suite.

## Still to come (not in H1c)

- **H2** — the idle reaper, the running/parked inference off the frames the H1b watcher
  already reads (a bounded line reader: the watcher itself keeps nothing after `hello`), `/hub/events`, and classifying a door that says `goodbye` and
  exits as `stopping` rather than `dead` (the frame is not parsed today).
- **H3** — `/hub/mesh`, `/hub/mesh/events`.

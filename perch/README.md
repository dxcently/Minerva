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

```sh
cd perch
cargo build --release
# the UI bundle it serves is webui/term/ in this repository
target/release/perch --bind 127.0.0.1:0 --ui-dir ../webui/term
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

This is **H1a**, and H1a is only the front door: the process, its one secret, the gates
every request passes, and the static UI. Nothing behind the gates is built yet.

```text
  /            the UI bundle from --ui-dir: no token, `Host` checked
  /hub/...     token-gated, no handler yet  ->  404   (H1b: sessions, spawn, stop, tree)
  /s/...       token-gated, no handler yet  ->  404   (H1c: the proxy and its SSE)
```

An **unauthenticated** request to `/hub` or `/s` is a 401; an authenticated one is a
404. Being able to tell those two apart is the whole point of this slice.

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

## Layout

```text
src/main.rs     argv, SIGINT/SIGTERM -> CancellationToken, the runtime `run` needs
src/lib.rs      bind, the runtime dir, the token mint and its removal on exit
src/gate.rs     loopback_only, token_ok (subtle), host_ok, sec_fetch_ok
src/serve.rs    accept loop, the gate order, the route match, shutdown
src/files.rs    the static rules, ported from eidolon/crates/web/src/files.rs
tests/http.rs   a real perch on 127.0.0.1:0, over a real socket
```

`src/files.rs` is a **port, not a dependency**: `eidolon/crates/web/src/files.rs` is the
source of the containment rules (one percent-decode, no `.`/`..`/empty segments, no NUL
or backslash, canonicalize under a root canonicalized once, regular files only, 32 MiB)
and its tests came with it. The perch must not link `eidolon-web`, so the rules live here
too.

`cargo test` starts real servers and speaks HTTP/1.1 to them by hand; the token is read
out of the file the child printed, and the child runs at `RUST_LOG=trace` so a token that
reached a log line would fail the suite.

## Still to come (not in H1a)

- **H1b** — spawn, resume and stop `eidolon web` doors, the session list, the idle
  reaper, `/hub/tree`, `/hub/mesh`. Shutdown grows a step *before* its drain: close the
  watcher and pane streams, then SIGTERM the children, wait, and only then stop the
  listener — a door holding an open `/api/events` stream ignores SIGTERM for minutes, and
  a SIGKILL would leave its token file behind.
- **H1c** — the `/s/<id>/api/*` proxy with the door's Bearer injected, SSE passed through
  unbuffered, and the `hub-stream-end` frame that tells the page a door is gone.

# The hub: one loopback process in front of many doors

**Written 2026-09-25. Design only: no code, no other file changed.** Subject: a Minerva-owned `minerva hub` binary that lets the browser UI (`web-ui.md`) tile several eidolon sessions without touching upstream. Decided before this doc (by the user): option A, a hub, in Rust, upstream untouched.

**Labels** (the four `web-door-security.md` uses). **observed**: I read the code; file:line given. **reported**: someone else measured it; I quote. **reasoned**: follows from observed facts by the argument written next to it. **hypothesis**: believed, not measured; the note says what would measure it.

**Citation base.** Door code: the working tree of `eidolon-door/eidolon` on branch `web-launch` (`22021b9`), `crates/web/src/*` unless another path is named. Aoide: `Minerva/Aoide/pkgs/aoide/...` and the wiki page named. No cargo was run for this doc.

---

## 0. The answer in one paragraph

The hub is a second loopback server that owns the browser's only credential. It spawns `eidolon web` doors as children, reads each door's token from its `0600` file, and proxies `/s/<id>/api/*` to that door with the door's Bearer injected, streaming SSE through unbuffered. It adds four things the door does not have: a session list, spawn/resume/stop with an idle reaper, a read-only directory listing under a session's cwd, and a read-only Aoide mesh feed. It needs **no door change for H1** (reasoned, section 13). Its token is strictly more powerful than any door token: it is every door token at once, plus the power to start agents.

---

## 1. Primitives and relations

| Primitive | What it is | Owned by |
|---|---|---|
| **page** | the UI bundle from `web-ui.md`, served by the hub, same-origin | Minerva |
| **hub token** | 256 bits, per hub run, `0600` file; the page's only secret | hub |
| **door** | one `eidolon web` process = one session, loopback, own token (lib.rs:202) | upstream |
| **door token** | per door run, `$XDG_RUNTIME_DIR/eidolon/web-<port>.token` (lib.rs:148) | door; hub reads it, page never sees it |
| **session id** | the log file's stem inside the sessions dir, e.g. `ctf-pwn-3` | hub derives it |
| **pane** | one open `/s/<id>/api/events` stream from the page | page |
| **watcher** | the hub's own SSE stream to each live door (not a pane) | hub |
| **mesh** | Aoide's node roster, caches and event feed, read-only | Aoide |

```text
 browser tab (page, holds HUB token only)
   |  same origin: http://127.0.0.1:<hub>/
   v
 +------------------------ minerva hub ------------------------+
 | gates: loopback bind > token > Host > Sec-Fetch > shape > cap |
 |                                                               |
 |  /            static UI  (door's files.rs rules)              |
 |  /hub/...     sessions, tree, mesh      (hub's own routes)    |
 |  /s/<id>/api/* --proxy-->  +Bearer <door token>, Host rewrite |
 +------|------------------|---------------------|---------------+
        | child            | child               | read-only
        v                  v                     v
   eidolon web A      eidolon web B         aoided.sock (ping, subscribe)
   127.0.0.1:P1       127.0.0.1:P2          ~/.aoide/state/*.json
   web-P1.token       web-P2.token          `aoide node list --json`
        \                  /                `aoide mesh --json`
         sessions dir: *.eid logs (kept sessions = logs, no process)
```

Relations: a pane belongs to one session; a session has 0 or 1 door; a door has 0..n panes plus exactly 1 watcher while live; the hub has 0..n doors and 1 mesh reader.

---

## 2. Process

| Item | Design | Basis |
|---|---|---|
| Binary | `minerva hub [--bind 127.0.0.1:0] [--ui-dir DIR] [--root DIR]... [--idle 30m] [--eidolon PATH] [--sessions-dir DIR]` | proposed |
| Bind | loopback only (127.0.0.0/8, `::1`), refused before a listener exists, no widening flag | copies lib.rs:137 |
| Token | `harnox::crypto::random_token()` after bind; `harnox::fs::write_atomic_0600` to `$XDG_RUNTIME_DIR/minerva/hub-<port>.token`; mode re-checked after write, start refused if not owner-only | copies lib.rs:158, :202 |
| Runtime dir | **refuse to start without `XDG_RUNTIME_DIR`**; check the dir is owned by our uid and mode `& 077 == 0` | reasoned: eidolon falls back to the shared temp dir (presence.rs:109-114) and ignores a failed `chmod 0700` (presence.rs:130, `let _ =`), so a dir another user pre-created stays theirs |
| stdout | `listening on http://<addr>/` and `token file: <path>`; never the token | copies lib.rs:209-210 |
| Shutdown | SIGINT/SIGTERM: stop accepting, SIGTERM every child, wait up to 10 s each, SIGKILL leftovers and delete their token files, drain SSE (`server-graceful`), remove own token file | door does the same for itself (main.rs:660, lib.rs:253) |
| Hub crash | children get `PR_SET_PDEATHSIG(SIGTERM)` so they do not outlive the hub | **hypothesis**: PDEATHSIG fires when the *forking thread* exits, and tokio's blocking-pool threads exit when idle; spawn from one dedicated long-lived thread. Measure: kill -9 the hub in a test and assert children gone |
| Platform | Unix only, like the door (lib.rs:159 `std::os::unix`) | observed |

Static serving: the door's rules, ported (not depended on): one percent-decode, no `.`/`..`/empty segments, no NUL or backslash, join to a root canonicalized once, canonicalize, must still start with the root, regular files only, 32 MiB cap (files.rs:1-19, :40, :63-80). Headers on every static response, refusals included: `nosniff`, `no-referrer`, `no-cache`, and the door's exact CSP `default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'` (serve.rs:369, :379). Statics need no token (the page loads before it has one), but unlike the door they do need the `Host` check (the door does that too, serve.rs:309).

---

## 3. Route table

| Method | Path | Token | Does | Answers |
|---|---|---|---|---|
| GET, HEAD | `/`, `/<file>` (not `/hub`, `/s`) | no | static UI from `--ui-dir` | 200 / 404 / 405 / 413 |
| GET | `/hub/sessions` | yes | list kept + live (section 4) | 200 `[{id, state, title?, cwd?, busy?, panes, mtime}]` |
| POST | `/hub/sessions` | yes | spawn a new session `{cwd, model?}` | 201 `{id}` / 422 cwd outside roots / 503 spawn failed |
| POST | `/hub/sessions/<id>/resume` | yes | spawn a door on a kept log | 200 / 409 live here or elsewhere / 404 |
| POST | `/hub/sessions/<id>/stop` | yes | SIGTERM that door | 202 / 409 not live |
| GET | `/hub/events` | yes | SSE: `session-state {id, state}` whenever the list changes | stream |
| GET | `/hub/tree?session=<id>&path=<rel>` | yes | one directory under the live session's cwd (section 7) | 200 / 404 / 409 not live |
| GET | `/hub/mesh` | yes | Aoide snapshot (section 8) | 200 `{available, nodes, mesh, cache, graph}` |
| GET | `/hub/mesh/events` | yes | SSE: Aoide events, filtered | stream |
| GET, POST | `/s/<id>/api/<route>` | yes | proxy to that door (section 5); today: `events`, `say`, `cancel`, `answer`, `launch` (lib.rs:6-20) | the door's status, except 401 -> 502 |
| other | anything under `/hub`, `/s` | yes | nothing | 401 before, 404/405 after the token |

The token gate covers the bare `/hub`, `/s` and everything under them, before path parsing, so an unauthenticated caller cannot read this table off status codes (the door's `in_api` rule, serve.rs:206, :240).

---

## 4. Sessions

**Where the list comes from.**

| Source | Gives | Basis |
|---|---|---|
| sessions dir, `*.eid` | kept sessions: stem, mtime | observed main.rs:868-876 (`eidolon sessions` reads the same); default `data_local_dir/eidolon/sessions`, overridable in config (config.rs:587-593). The hub takes `--sessions-dir` and defaults to the same path; **hypothesis** it matches the member's config, since the hub does not parse eidolon's `config.toml` |
| hub children | live doors this hub owns: pid, port, door token (memory only) | proposed |
| swarm roster, `$XDG_RUNTIME_DIR/eidolon/*/meta.json` | live sessions of any consumer (TUI, other doors): `pid`, `log`, `cwd`, `busy`, `title` | observed presence.rs:33-53. Read by scanning, the hub never registers (Q2) |

**Id.** The log stem, `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`, resolved only as `<sessions dir>/<id>.eid`. The browser never sends a path to a log. Logs outside the sessions dir are not listed.

**States.**

```text
            spawn/resume                stream up, hello read
   kept ---------------> starting ------------------------> live
    ^                       | startup timeout / exit           |  stop, idle, hub quit
    |                       v                                  v
    +------------------- dead(exit) <----- child exit ----- stopping
    |                                                          |
    +--------------------------- exited cleanly --------------+
   live-elsewhere: roster has meta.log == this log and pid is not our child (read-only in the list)
```

**Spawn sequence** (new and resume differ only in argv).

| # | Step | Why |
|---|---|---|
| 1 | new: canonicalize `cwd`, must be under a `--root` (default `$HOME`); resume: `<sessions dir>/<id>.eid` must exist | the hub token should not start agents in `/` |
| 2 | refuse 409 if the roster or our children already hold that log | **observed**: no `flock`/`try_lock` in `crates/core/src/session*`; two writers on one log is unguarded upstream |
| 3 | argv: `eidolon web --bind 127.0.0.1:0` + `--session <path>` or `--cwd <dir>` [+ `-m <model>`] (main.rs:325-358). **No `--ui-dir`**: the door serves no statics | the door mints its own token, so no token ever enters argv or env (reasoned) |
| 4 | env inherited unchanged, stdin `/dev/null`, stdout piped, stderr to the hub's log | the hub adds nothing secret |
| 5 | read stdout until both lines (lib.rs:209-210), 20 s timeout -> kill, `dead` | the *printed* token path names the port; scanning the dir for a new `web-*.token` would race other doors starting (reasoned). Fragile if upstream rewords the lines (door change W3) |
| 6 | the token path must be `<runtime>/eidolon/web-<port>.token` with the port from line 1; open `O_NOFOLLOW`, check owner uid and `& 077 == 0`, read, hold in memory | never trust a path a child printed without checking it |
| 7 | open the watcher stream; `live` after `hello` | also gives `hello.session` (the log path, hence the id for a new session) and `hello.cwd` (stream.rs:59-72) |

**Resume can spend.** A door that opens a log whose last message is unanswered starts a turn by itself (lib.rs:225 `finish_unsettled`; driver.rs:478 test `..._registers_a_running_turn_for_an_orphaned_message`; observed). So the hub **never auto-restarts** a door, and the page must label Resume as "may continue the last turn".

**Stop.** SIGTERM (the door drains, removes its token, main.rs:660) -> 10 s -> SIGKILL and the hub deletes the token file. `eidolon door quiesce` was considered and rejected: it journals a `quiesced` marker naming a destination (main.rs:396-417) that reads as a hop to another machine, which an idle stop is not.

**Idle reaper** (every 60 s, per live child the hub owns):

```text
if panes(id) > 0                          -> last_watched = now; keep
if now - last_watched < idle (30 min)     -> keep
if watcher not caught-up                  -> keep      (unknown = keep)
if running                                -> keep      (turn-state frames, stream.rs:74, :177)
if parked                                 -> keep      (inferred, below)
if pending asks > 0                       -> keep      (an ask frame not yet ask-settled)
else                                      -> stop (as above), state kept
```

| Signal | How the hub reads it today | Label |
|---|---|---|
| running | watcher's `turn-state {running}`; but `hello` has no `running` and `turn-state` only fires on change (stream.rs:59-72, :177), so a door that is mid-turn when the watcher connects looks idle until the next change | observed; web-ui gap 1 |
| parked / timer armed | not exposed by any route. Inferred: the last `turn-settled` (replayed or live) has `stop_reason: "waiting"` (wire.rs:163-167) and no `trigger-fired` or `turn-state running` after it | reasoned; wrong only toward *keeping* a door (a park cleared without firing, wait.rs:883, still reads as parked) |
| busy (cross-check) | roster `meta.busy` (presence.rs:52), when the swarm is on | observed |

**Accepted loss.** A stopped session leaves the swarm roster, so peer mail and doorbell rings no longer reach it until resumed (reasoned from presence.rs `deregister`).

---

## 5. Proxy: `/s/<id>/api/<route>`

**Path.** `<route>` must match `^[a-z][a-z0-9-]{0,31}(/[a-z0-9-]{1,32})*$`, no `%`, no query string; method GET or POST. A shape check, not an allowlist of five names: new door routes (web-ui M3's `/api/commands`) pass without a hub change. The trade: the hub token grants whatever the door grows. It already grants everything the door grants, so this adds no new class of power (reasoned).

**Request rewrite.**

| Header / part | page -> hub | hub -> door |
|---|---|---|
| `Authorization` | `Bearer <hub token>`, checked, removed | `Bearer <door token>` |
| `Host` | must be the hub's | `127.0.0.1:<door port>`; the door checks `Host` on POSTs against its bound address (serve.rs:244) |
| `Content-Type`, `Accept` | kept | kept |
| everything else (`Cookie`, `Origin`, `Referer`, `Sec-*`, `Last-Event-ID` until the door honours it) | dropped | not sent |
| query string | refused (400) | none; the door's `?token=` form (serve.rs:145-158) is never used |
| body | read through `Limited` at 8 MiB, then sent | door caps at 8 MiB too (serve.rs:87, :408) |

**Response.** Status passes through, except **door 401 -> hub 502** `{"error":"door refused the hub"}`, so the page never concludes its own token is bad. Headers kept: `content-type`, `cache-control`; added: `nosniff`, `no-referrer`, `cache-control: no-store`. No `Access-Control-*`, ever.

**Transport.** One fresh TCP connection per request to `127.0.0.1:<port>`, `hyper::client::conn::http1`, 2 s connect timeout, 60 s response-head timeout for POSTs (`/api/launch` spawns under `spawn_blocking`), none for SSE. No pooling: loopback connects are cheap and a pool would outlive a dead door (reasoned).

**SSE passthrough.** Each upstream `Frame<Bytes>` becomes a hub frame as it arrives: no buffering, no re-chunking, no parsing. Backpressure is the door's own: a slow tab stops the hub reading, the door's broadcast subscriber lags, the door sends `lagged` and ends the stream (stream.rs:149-156). The page reconnects and gets a fresh hello + replay (reasoned).

**When the upstream stream ends**, the hub appends one frame of its own and ends the page's stream:

```text
\n\ndata: {"type":"hub-stream-end","session":"<id>","door":"live|stopping|dead","exit":"code:1|signal:9|null"}\n\n
```

The leading blank line terminates any half-written door frame, whose JSON the page then fails to parse and drops (web-ui must ignore unparseable frames). The `hub-` prefix keeps the name out of the door's namespace; the page ignores unknown types, so the door's protocol number is untouched (lib.rs:84-90).

| `door` | Cause | Page does |
|---|---|---|
| `live` | door ended the stream: `lagged`, or a reconnect-worthy reset | reconnect now |
| `stopping` | idle stop, `/stop`, hub quitting, or the door's own `goodbye` (stream.rs:93) | show "stopped", offer Resume |
| `dead` | child exited or startup failed | show the exit, offer Resume (no auto-restart, section 4) |

While `dead`, `/s/<id>/api/*` answers 503 `{"state":"dead"}` and the hub opens **no** connection to the old port (next section, attacker E).

---

## 6. Mirrors

Several panes on one session = several proxied `/api/events` streams to one door. The door serves any number of streams, each with its own subscribe + replay (stream.rs:104-139, observed). The hub adds nothing but a pane count per id for the reaper. Two panes answering one ask: the second `answer` gets the door's 409 (lib.rs:13-16), passed through. Cost: each pane replays the whole branch on connect (web-ui gap 10); **hypothesis** fine below a few thousand records; measure a 2 000-record branch times 4 panes.

---

## 7. Project tree: `GET /hub/tree?session=<id>&path=<rel>`

Live sessions only (the root is `hello.cwd` from the watcher, the reopen-time cwd, main.rs:1536-1543); kept -> 409.

| Step | Rule | Refusal |
|---|---|---|
| 1 | decode the query once; `path` empty means the root | 400 malformed escape |
| 2 | no NUL, no backslash, not absolute, no `.`/`..`/empty segments, <= 4096 bytes | 404 |
| 3 | every segment tested against the deny list below | 404 (same as missing: no oracle) |
| 4 | join to the canonical root, canonicalize, must `starts_with(root)` | 404: this is where a symlink out of the tree dies |
| 5 | must be a directory; `read_dir`; entries via `symlink_metadata` (links are never followed) | 404 |
| 6 | drop denied names; sort dirs first then by name; cap 2 000 | `truncated: true` |

Deny list (case-insensitive, whole name): `.git` (hidden entirely), `.env`, `.env.*`, `*.key`, `*.pem`, `*.p12`, `*.pfx`, `*.kdbx`, `id_*`, `*secret*`, `*.token`, `.netrc`, `.ssh`, `.gnupg`, `.aoide`, `credentials*`. A config knob can add names, never remove these.

Response: `{"path":"src","entries":[{"name":"main.rs","kind":"file","size":1234},{"name":"lib","kind":"dir"},{"name":"out","kind":"link"}],"skipped":0,"truncated":false}`. Links show no target (a target string can name paths outside the root). Non-UTF-8 names are counted in `skipped`. **No file contents, ever.** File viewing belongs to the door or `/api/launch` (editor window).

Accepted: a TOCTOU swap between canonicalize and `read_dir` needs write access as the operator's own user, which is out of scope (the door's files.rs:14-16 accepts the same).

---

## 8. Aoide feed

**Snapshot, `GET /hub/mesh`** (cached 5 s so N panes cost one read):

| Part | Source | Handling |
|---|---|---|
| `nodes` | `aoide node list --json` | fixed argv, no browser input, 5 s timeout, stdout capped 1 MiB. **hypothesis**: it reads caches and does not probe the network ("read-only", Node-Federation.md:203-205); measure with the network off |
| `mesh` | `aoide mesh --json` | same; declared vs registered drift (Node-Federation.md:210-213) |
| `registry` | `~/.aoide/state/nodes.json` | projected through an allowlist: `name, url, verified, allows, autogate, addedAt`. **Dropped**: `tokenFile` (a path to a secret), `bearerSecret` (a secret's name), `pubkey` (Node-Federation.md:55-89) |
| `cache` | `~/.aoide/state/node-cache/*.json` | at most 64 files, 2 MiB each; remote-authored, so rendered as text only |
| `graph` | `~/.aoide/state/stage/graph.json` | 2 MiB cap |
| `available` | false when `aoide` is not on PATH and no state dir exists | the club's Windows members may have neither |

**Events, `GET /hub/mesh/events`.** One upstream subscription, fanned out to every browser stream through a bounded broadcast; a lagging browser gets `lagged` and a closed stream, the door's pattern.

| Preference | Source | Basis |
|---|---|---|
| 1 | `$XDG_RUNTIME_DIR/aoide/aoided.sock` (0600, daemon.rs:25, :690): send `{"v":0,"op":"subscribe","classes":["audit","gate"]}`; the connection becomes one-way; hang up to stop | observed daemon.rs:1043-1049, :1112-1160; empty classes deliver nothing (default deny, daemon.rs:1097-1103) |
| 2 | tail `$XDG_RUNTIME_DIR/aoide/events.jsonl`, filter the same classes; the file is 1 MiB truncate-in-place, so a length below the read offset means restart at 0 | observed daemon.rs:78-83 |

Classes: `audit`, `gate` only (Q7). Excluded: `notification` (forwarded OS notifications, untrusted payload, audit.rs:88-89), `secret` (names of secrets and consumers, audit.rs:90-98), `rice`, `content` (not mesh).

**Allowed aoided ops: `ping` and `subscribe`. Nothing through `dispatch`**, read-only or not. Reason: `dispatch` runs the same `cli::dispatch::dispatch` as every other door with "no daemon-specific permission table" (daemon.rs:60-65, :1051-1075), so a hub allowlist would be the only thing between the browser and every aoide command, one bug from a write. Read-only snapshots go through the fixed-argv CLI calls above. If a dispatch path is ever wanted, the rule is: a compiled-in list of exact command paths (`node list`, `mesh`, `graph`) with `--json` and no argument taken from a request.

**Never:** proxy Aoide's A2A door (`127.0.0.1:8710`) to the browser; run `node pull`, `mesh pair`, `pair`, `spawn`, `send`, `allow`, `node status` (its `--json` carries the full registry row, Node-Federation.md:208-209).

---

## 9. Launcher

`/api/launch` goes through the door, per session: `POST /s/<id>/api/launch {"window","path"?}`, proxied like any other route. The window opens on the door's machine, which is the hub's and the browser's (lib.rs:17-20; launch.rs:1-10). The hub has no launcher of its own and no second copy of `[terminal]` config. Reported working in WSL2 with `wt.exe` (web-launch commit `22021b9` message).

---

## 10. Security

### 10.1 Threat model

| # | Attacker | Can | Hub's answer |
|---|---|---|---|
| A | hostile page in the operator's browser | fire cross-origin requests at `127.0.0.1:<hub>` | token in a header only (a cross-site simple request cannot set one); no CORS headers; `frame-ancestors 'none'`; `Sec-Fetch-Site` check |
| B | DNS rebinding | make its own name resolve to 127.0.0.1 | `Host` check on **every** request, SSE and statics included (stricter than the door, which skips it on `/api/events`, lib.rs:33-36) |
| C | another local OS user | connect to loopback ports, read world-readable files and `/proc/*/cmdline` | token only in `0600` files under an owner-checked runtime dir; no token in argv, env, URLs or logs |
| D | a compromised extension in the UI | run script in the page's origin | **nothing stops it.** It holds the hub token: it can type into every session, approve every ask, start and resume agents. See 10.4 |
| E | another local user racing a dead door's port | bind the freed port and receive what the hub sends there | a child's exit flips its state to `dead` before any new upstream connect; the door token died with the door. **Residual**: a request already in flight at the instant of exit (reasoned; narrow) |
| - | same-uid attacker | reads every token file | out of scope, as for the door (web-door-security.md section 1) |

### 10.2 Gates, in the order a request meets them

| # | Gate | Refusal | Basis |
|---|---|---|---|
| 0 | loopback-only bind, at start | no start | lib.rs:137 |
| 1 | token on `/hub`, `/hub/*`, `/s`, `/s/*`: `Authorization: Bearer` only, never a query parameter; constant time (`subtle`) | 401, empty body | serve.rs:165, :240; the page uses fetch-based SSE (web-ui.md section 9, M1), so the door's `?token=` escape hatch is not needed |
| 2 | `Host` equals the bound `ip:port` (or `localhost:port`, Q8) | 403 | serve.rs:118 |
| 3 | `Sec-Fetch-Site`, when present, is `same-origin` or `none` | 403 | new; a same-origin page always passes, curl sends none |
| 4 | method and path shape | 404 / 405 | section 3 |
| 5 | body `Limited` at 8 MiB | 413 | serve.rs:87, :408 |
| 6 | per-route: id regex, cwd under `--root`, tree containment, session state | 404 / 409 / 422 | sections 4, 7 |

### 10.3 What the hub token grants

**Everything every door token grants, across every session the hub can reach, at once**: read every transcript, send and steer, cancel, answer any ask (including policy approvals), open terminal/editor/file windows. **Plus**: start a new agent in any directory under `--root`, resume any kept session (which may continue a turn and spend, section 4), stop sessions, list directory names under any live session's cwd, read the mesh snapshot and feed. Leaking it is leaking the operator's whole bench.

### 10.4 Must never happen

| Never | Enforced by | Test |
|---|---|---|
| a token in argv or env (hub's, a child's, a browser's) | the door mints its own; hub passes none | read `/proc/<pid>/cmdline` and `environ` of hub and children |
| a token in a URL | Bearer header only; query strings refused on `/s` | request with `?token=` -> 400/401 |
| a token in a log or stdout | no header, body or query logged; the hub never prints its token | run at `RUST_LOG=trace`, grep all output for both tokens |
| a door token reaching the page | stripped response headers; hub never serializes it | grep every response body in the suite |
| CORS headers, an `OPTIONS` answer | none written; `OPTIONS` is 405 | assert absent |
| a listing outside a session cwd, or of a denied name | section 7 | symlink-out, `..`, `%2e%2e`, `.env` cases |
| two doors on one log | section 4 step 2 | spawn twice -> 409 |
| a dispatch to aoided, or a proxy to `:8710` | no code path | grep in review; mutation below |

**Accepted risks.** (1) Attacker D is total; the remedy is an extension sandbox (sandboxed iframes fed by `postMessage`), a web-ui M4 decision, not a hub one (web-ui.md Q9). (2) `localhost` in `Host` if Q8 says yes. (3) Tree TOCTOU (section 7). (4) Stopped sessions miss peer mail. (5) The PDEATHSIG hypothesis in section 2.

---

## 11. Crate layout and dependencies

```text
Minerva/hub/                  own Cargo.toml + Cargo.lock; not an eidolon workspace member
  src/main.rs                 clap argv, signals -> CancellationToken, run
  src/lib.rs                  Hub::run(opts, cancel)
  src/gate.rs                 loopback_only, token_ok, host_ok, sec_fetch_ok
  src/serve.rs                accept loop, gate order, route match on (Method, path)
  src/files.rs                static rules, ported from crates/web/src/files.rs with its tests
  src/sessions.rs             registry, states, list, roster scan
  src/spawn.rs                argv, stdout parse, token-file checks, PDEATHSIG
  src/watch.rs                watcher streams, running/parked inference, idle reaper
  src/proxy.rs                /s/<id>/api/*, rewrite tables, hub-stream-end
  src/tree.rs                 /hub/tree
  src/mesh.rs                 snapshot + aoided subscribe / events.jsonl tail
  src/bin/fake_eidolon.rs     test double, required-features = ["test-bins"]
  tests/http.rs               real HTTP against a hub on 127.0.0.1:0
```

| Dep | Why | Already in eidolon's graph |
|---|---|---|
| `tokio` (rt-multi-thread, net, process, signal, fs, time, io-util, sync, macros), `tokio-util` | runtime, children, signals, cancellation | yes, 1.53 / 0.7 (eidolon Cargo.toml:41-42) |
| `hyper` 1 (`server`, `client`, `http1`), `hyper-util` (`tokio`, `server`, `server-graceful`, `http1`), `http-body-util`, `bytes` | the door's exact stack, plus hyper's `client` feature for the proxy | yes (crates/web/Cargo.toml) |
| `async-stream`, `futures-util` | SSE bodies | yes |
| `serde`, `serde_json`, `anyhow`, `tracing`, `tracing-subscriber`, `clap` 4 | the usual | yes (Cargo.toml:37-59) |
| `subtle` 2 | constant-time token compare | yes |
| `harnox` (git tag v0.3.8, no default features) | `random_token`, `write_atomic_0600`: one mint of a secret, the door's own reason | yes (Cargo.toml:35). Noah's crate, **not** eidolon |
| `libc` | `prctl(PR_SET_PDEATHSIG)`, `getuid` | in the lock, 0.2.189 |
| dev: `tempfile` | test dirs | yes |

No axum, no reqwest, no dependency on any `eidolon-*` crate: the hub knows the door only as a binary, a stdout contract, a token file and HTTP.

---

## 12. Test plan

Door style: real sockets, real child processes, no mocked HTTP layer; every gate proven by a mutation that turns a test red.

**`fake_eidolon`**: accepts `web --bind --session|--cwd`, binds `127.0.0.1:0`, writes a `0600` `web-<port>.token` under `$XDG_RUNTIME_DIR/eidolon`, prints the door's two lines, serves `/api/events` from a script (hello, replayed frames, `turn-state`, `turn-settled waiting`, `goodbye`) and records every request's method, path, headers and body to a file the test reads. Knobs by env: `FAKE_SILENT` (never prints), `FAKE_MODE=0644`, `FAKE_EXIT_AFTER_MS`, `FAKE_LOCK_PORT`.

| Area | Cases |
|---|---|
| gates | no token / wrong / prefix / `?token=` -> 401; bad `Host` on GET, POST, SSE, static -> 403; `Sec-Fetch-Site: cross-site` -> 403; 8 MiB + 1 -> 413 and the fake saw nothing; `OPTIONS` -> 405; no `Access-Control-*` anywhere |
| spawn | argv recorded by the fake contains no token; startup timeout -> `dead`; `0644` door token -> refused; token path outside the runtime dir -> refused; second spawn on one log -> 409; cwd outside `--root` -> 422 |
| proxy | the fake saw `Bearer <door token>`, `Host: 127.0.0.1:<its port>`, no `Cookie`/`Origin`; door 401 -> 502; SSE frames arrive before the fake ends the stream (timeout-guarded, no sleeps); fake exits mid-frame -> page stream ends with `hub-stream-end dead`; later request -> 503 and the fake's port gets no connection |
| mirrors | 3 panes, each gets hello + replay; pane count drives the reaper |
| idle | `--idle 2s`: stopped when unwatched; kept while `running`, while parked (`turn-settled waiting`), while an ask is pending, while a pane is open |
| tree | `..`, `%2e%2e`, absolute, NUL, symlink out -> 404; `.env`, `id_rsa`, `.git` absent; 2 001 entries -> `truncated` |
| mesh | fixture state dir: `tokenFile`/`bearerSecret` absent from output; a fake `aoided.sock` receives exactly one `subscribe` with `["audit","gate"]` and never `dispatch` |
| secrets | whole suite at `RUST_LOG=trace`: neither token appears in captured stdout/stderr |
| shutdown | SIGTERM the hub: children gone, all token files gone; kill -9 the hub: children gone (PDEATHSIG hypothesis) |

**Mutation checks** (each must turn a named test red): remove the token gate; remove the `Host` check on SSE; drop the Authorization rewrite; forward `Cookie`; remove `Limited`; remove `starts_with(root)` in the tree; remove the `running` guard in the reaper; send one `dispatch` in `mesh.rs`.

Run in WSL2 (ext4 target dir), like the door's reported numbers.

---

## 13. Milestones (matching web-ui.md section 9)

| H | With UI | Scope | Door needs | Done when |
|---|---|---|---|---|
| **H1** | M1 | process, token, statics, gates; spawn / resume / stop one session; proxy incl. SSE and `hub-stream-end`; `/hub/sessions` | **nothing** (reasoned: existing flags main.rs:325-358, existing stdout lines lib.rs:209-210, existing routes) | web-ui M1's done-when passes through `/s/<id>/`, and the secrets tests in section 12 are green |
| **H2** | M2 | several sessions tiled; mirrors; idle reaper; `/hub/events`; `/hub/tree`; launcher passthrough | W1 wanted (exact `running`) | two sessions in two panes plus a mirror, one idles out and resumes from its log |
| **H3** | M3 | `/hub/mesh`, `/hub/mesh/events`; commands pass through unchanged | the registry, for the page, not the hub | mesh roster and live `gate` events on screen with Aoide running; nothing when it is not |
| **H4** | M4 | nothing new in the hub; decide the extension sandbox (10.4) | nothing | an extension renders without holding the hub token, or the risk is written down as accepted |

---

## 14. Door changes the hub would want (none required for H1)

| # | Change | Why the hub wants it | Size |
|---|---|---|---|
| W1 | `running: bool` in `hello` (web-ui gap 1) | reaper reads a fact instead of waiting for a transition | one field, additive |
| W2 | `parked: bool` (or the waiting condition) in `hello` and a frame when it changes, from `Parks::is_armed` (wait.rs:839) | replaces the `stop_reason: waiting` inference | additive |
| W3 | a machine-readable ready line, e.g. `{"listening":"127.0.0.1:P","token_file":"..."}`, or `--ready-fd N` | the hub parses human `println!`s today | small |
| W4 | (core, not door) an advisory lock on an open session log | two writers on one log are unguarded (section 4 step 2); the hub's roster check misses swarm-off consumers | small, upstream decision |

---

## 15. Open questions

| # | Question | Default if unanswered |
|---|---|---|
| Q1 | **Windows members.** The hub and doors run in WSL2 (both Unix-only). Does a Windows browser reach WSL2 loopback reliably: default localhost forwarding or `networkingMode=mirrored`? Who opens the browser: `minerva hub --open` via `wslview`/`explorer.exe`, with web-ui option B's `0600` redirect file, which Windows must then read through `\\wsl.localhost\...` (**hypothesis**, untested)? The launcher already reaches `wt.exe` (reported). | WSL2 + mirrored mode documented; test on one member's box before H1 ships |
| Q2 | Should the hub be an eidolon swarm peer? For: doorbell, appears in `door list`. Against: it would publish a `meta.json` for a session it is not, and reading the roster needs no registration (`swarm::scan` is a directory read, presence.rs:341) | **no**, read-only roster scan |
| Q3 | Idle timeout default | 30 min, `--idle` flag, `0` disables |
| Q4 | Naming. "hub" collides with `aoide node hub <name>` (Node-Federation.md:193-197) in the same club's vocabulary. Candidates: `minerva hub`, `minerva desk`, `minerva perch` (owl, fits the mascot) | `minerva hub` until someone picks |
| Q5 | Doors started by hand (not our children): adopt them (read their token file, proxy) or list as `live-elsewhere` only? | list only; adopting means proxying a door whose lifetime the hub does not own |
| Q6 | `--root` default for new sessions | `$HOME` |
| Q7 | Mesh event classes: `audit` + `gate`, or also `secret` (names only)? | `audit`, `gate` |
| Q8 | Accept `Host: localhost:<port>` as well as the IP? A Windows browser over WSL forwarding may send it; the door refuses it (serve.rs:118-132 compares the IP string) | accept `localhost` literally; no other names |
| Q9 | Should `/hub/tree` also serve kept sessions (root from the log's start record)? | no, live only |

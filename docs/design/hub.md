# The perch: one loopback process in front of many doors

**Written 2026-09-25. Design only: no code, no other file changed.** Subject: a Minerva-owned `minerva perch` binary that lets the browser UI (`web-ui.md`) tile several eidolon sessions without touching upstream. Decided before this doc (by the user): option A, a perch, in Rust, upstream untouched.

**Labels** (the four `web-door-security.md` uses). **observed**: I read the code; file:line given. **reported**: someone else measured it; I quote. **reasoned**: follows from observed facts by the argument written next to it. **hypothesis**: believed, not measured; the note says what would measure it.

**Citation base.** Door code: the working tree of `eidolon-door/eidolon` on branch `web-launch` (`22021b9`), `crates/web/src/*` unless another path is named. Aoide: `Minerva/Aoide/pkgs/aoide/...` and the wiki page named. No cargo was run for this doc.

---

## 0. The answer in one paragraph

The perch is a second loopback server that owns the browser's only credential. It spawns `eidolon web` doors as children, reads each door's token from its `0600` file, and proxies `/s/<id>/api/*` to that door with the door's Bearer injected, streaming SSE through unbuffered. It adds four things the door does not have: a session list, spawn/resume/stop with an idle reaper, a read-only directory listing under a session's cwd, and a read-only Aoide mesh feed. It needs **no door change for H1** (reasoned, section 13). Its token is strictly more powerful than any door token: it is every door token at once, plus the power to start agents.

---

## 1. Primitives and relations

| Primitive | What it is | Owned by |
|---|---|---|
| **page** | the UI bundle from `web-ui.md`, served by the perch, same-origin | Minerva |
| **perch token** | 256 bits, per perch run, `0600` file; the page's only secret | perch |
| **door** | one `eidolon web` process = one session, loopback, own token (lib.rs:202) | upstream |
| **door token** | per door run, `$XDG_RUNTIME_DIR/eidolon/web-<port>.token` (lib.rs:148) | door; perch reads it, page never sees it |
| **session id** | the log file's stem inside the sessions dir, e.g. `ctf-pwn-3` | perch derives it |
| **pane** | one open `/s/<id>/api/events` stream from the page | page |
| **watcher** | the perch's own SSE stream to each live door (not a pane) | perch |
| **mesh** | Aoide's node roster, caches and event feed, read-only | Aoide |

```text
 browser tab (page, holds PERCH token only)
   |  same origin: http://127.0.0.1:<perch>/
   v
 +------------------------ minerva perch ------------------------+
 | gates: loopback bind > token > Host > Sec-Fetch > shape > cap |
 |                                                               |
 |  /            static UI  (door's files.rs rules)              |
 |  /hub/...     sessions, tree, mesh      (perch's own routes)  |
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

Relations: a pane belongs to one session; a session has 0 or 1 door; a door has 0..n panes plus exactly 1 watcher while live; the perch has 0..n doors and 1 mesh reader.

---

## 2. Process

| Item | Design | Basis |
|---|---|---|
| Binary | `minerva perch [--bind 127.0.0.1:0] [--ui-dir DIR] [--root DIR]... [--idle 30m] [--eidolon PATH] [--sessions-dir DIR] [--eidolon-config FILE] [--home DIR]` | built; the last two are the Minerva home (below) |
| Bind | loopback only (127.0.0.0/8, `::1`), refused before a listener exists, no widening flag | copies lib.rs:137 |
| Token | `harnox::crypto::random_token()` after bind; `harnox::fs::write_atomic_0600` to `$XDG_RUNTIME_DIR/minerva/perch-<port>.token`; mode re-checked after write, start refused if not owner-only | copies lib.rs:158, :202 |
| Runtime dir | **refuse to start without `XDG_RUNTIME_DIR`**; check the dir is owned by our uid and mode `& 077 == 0` | reasoned: eidolon falls back to the shared temp dir (presence.rs:109-114) and ignores a failed `chmod 0700` (presence.rs:130, `let _ =`), so a dir another user pre-created stays theirs |
| stdout | `listening on http://<addr>/` and `token file: <path>`; never the token | copies lib.rs:209-210 |
| Shutdown | SIGINT/SIGTERM: stop accepting, SIGTERM every child, wait up to 10 s each, SIGKILL leftovers and delete their token files, drain SSE (`server-graceful`, bounded to 3 s so a page that has stopped reading cannot hold the quit), remove own token file | door does the same for itself (main.rs:660, lib.rs:253) |
| Perch crash | children get `PR_SET_PDEATHSIG(SIGTERM)` so they do not outlive the perch | **hypothesis**: PDEATHSIG fires when the *forking thread* exits, and tokio's blocking-pool threads exit when idle; spawn from one dedicated long-lived thread. Measure: kill -9 the perch in a test and assert children gone |
| Platform | Unix only, like the door (lib.rs:159 `std::os::unix`) | observed |

Static serving: the door's rules, ported (not depended on): one percent-decode, no `.`/`..`/empty segments, no NUL or backslash, join to a root canonicalized once, canonicalize, must still start with the root, regular files only, 32 MiB cap (files.rs:1-19, :40, :63-80). Headers on every static response, refusals included: `nosniff`, `no-referrer`, `no-cache`, and the door's exact CSP `default-src 'self'; connect-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'` (serve.rs:369, :379). Statics need no token (the page loads before it has one), but unlike the door they do need the `Host` check (the door does that too, serve.rs:309).

---

## 3. Route table

| Method | Path | Token | Does | Answers |
|---|---|---|---|---|
| GET, HEAD | `/`, `/<file>` (not `/hub`, `/s`) | no | static UI from `--ui-dir` | 200 / 404 / 405 / 413 |
| POST | `/hub/code` | yes, **the bearer token only** (a cookie is a 403) | mint a login code: random, single use, 30 s, at most 4 outstanding (section 10.5) | 200 `{code}` |
| POST | `/hub/redeem` | **no** (host, fetch-metadata and a 1 KiB body cap still apply) | trade a code `{code}` for the session cookie (section 10.5) | 204 + `Set-Cookie` / 403 |
| GET | `/hub/sessions` | yes | list kept + live (section 4) | 200 `[{id, state, title, origin?, cwd?, busy?, panes, mtime}]` |
| GET | `/hub/sessions/<id>/log` | yes | a saved chat as eidolon's `log --json` lines, read-only (section 4, "Titles and saved chats") | 200 `application/x-ndjson` / 404 / 413 over 16 MiB / 504 / 502 |
| POST | `/hub/sessions` | yes | spawn a new session `{cwd, model?}` | 201 `{id}` / 422 cwd outside roots / 503 spawn failed |
| POST | `/hub/sessions/<id>/resume` | yes | spawn a door on a kept log | 200 / 409 live here or elsewhere / 404 |
| POST | `/hub/sessions/<id>/stop` | yes | SIGTERM that door | 202 / 409 not live |
| POST | `/hub/sessions/<id>/restart` | yes | `{model?, yolo?}`: stop the door, resume the same log with `-m` / `--yolo`, only between turns (section "Model and mode") | 200 `{id, model, yolo, restarted}` / 400 / 404 / 409 turn running, starting, not running here / 503 `{error, reverted}` |
| GET | `/hub/models` | yes | the models `eidolon [--config C] models` lists, cached 10 min | 200 `[{key, name?, note?}]` / 504 / 502 |
| GET | `/hub/events` | yes | SSE: `session-state {id, state}` whenever the list changes | stream |
| GET | `/hub/tree?session=<id>&path=<rel>` | yes | one directory under the live session's cwd (section 7) | 200 / 404 / 409 not live |
| GET, POST | `/hub/projects` | yes | the named folders in `<home>/projects.json`; POST `{path, name?}` adds one (section "Projects and the folder picker") | 200 `[{name, path, created}]` / 201 new, 200 existing / 400 name / 422 path outside the roots / 503 no home |
| DELETE | `/hub/projects/<n>` | yes | forget entry `n`; never the folder | 204 / 404 |
| GET | `/hub/dirs?path=<abs>` | yes | the subfolders of one folder under the roots; no path is the roots | 200 `{path, label, parent, crumbs, dirs, truncated}` / 404 / 503 |
| POST | `/hub/dirs` | yes | `{parent, name}`: make one folder inside a root | 201 `{path, name}` / 400 name / 409 exists / 422 outside |
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
| sessions dir, `*.eid` | kept sessions: stem, mtime | observed main.rs:868-876 (`eidolon sessions` reads the same); default `data_local_dir/eidolon/sessions`, overridable in config (config.rs:587-593). The perch takes `--sessions-dir` and defaults to the same path; with `--home` the perch generates the config the doors run with and lists the same `<home>/sessions` (see "Minerva home"), so the two cannot differ |
| perch children | live doors this perch owns: pid, port, door token (memory only) | proposed |
| swarm roster, `$XDG_RUNTIME_DIR/eidolon/*/meta.json` | live sessions of any consumer (TUI, other doors): `pid`, `log`, `cwd`, `busy`, `title` | observed presence.rs:33-53. Read by scanning, the perch never registers (Q2) |

**Id.** The log stem, `^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$`, resolved only as `<sessions dir>/<id>.eid`. The browser never sends a path to a log. Logs outside the sessions dir are not listed.

**States.**

```text
            spawn/resume                stream up, hello read
   kept ---------------> starting ------------------------> live
    ^                       | startup timeout / exit           |  stop, idle, perch quit
    |                       v                                  v
    +------------------- dead(exit) <----- child exit ----- stopping
    |                                                          |
    +--------------------------- exited cleanly --------------+
    +<-- live --exit 0 on its own, no stop asked (kept; a start that exits 0 is dead)
   live-elsewhere: roster has meta.log == this log and pid is not our child (read-only in the list)
```

**Spawn sequence** (new and resume differ only in argv).

| # | Step | Why |
|---|---|---|
| 1 | new: canonicalize `cwd`, must be under a `--root` (default `$HOME`); resume: `<sessions dir>/<id>.eid` must exist | the perch token should not start agents in `/` |
| 2 | refuse 409 if the roster or our children already hold that log | **observed**: no `flock`/`try_lock` in `crates/core/src/session*`; two writers on one log is unguarded upstream |
| 3 | argv: `eidolon web --bind 127.0.0.1:0` + `--session <path>` or `--cwd <dir>` [+ `-m <model>`] (main.rs:325-358). **No `--ui-dir`**: the door serves no statics | the door mints its own token, so no token ever enters argv or env (reasoned) |
| 4 | env inherited unchanged, stdin `/dev/null`, stdout piped, stderr to the perch's log | the perch adds nothing secret |
| 5 | read stdout until both lines (lib.rs:209-210), 20 s timeout -> kill, `dead` | the *printed* token path names the port; scanning the dir for a new `web-*.token` would race other doors starting (reasoned). Fragile if upstream rewords the lines (door change W3) |
| 6 | the token path must be `<runtime>/eidolon/web-<port>.token` with the port from line 1; open `O_NOFOLLOW`, check owner uid and `& 077 == 0`, read, hold in memory | never trust a path a child printed without checking it |
| 7 | open the watcher stream; `live` after `hello` | also gives `hello.session` (the log path, hence the id for a new session) and `hello.cwd` (stream.rs:59-72) |

**Resume can spend.** A door that opens a log whose last message is unanswered starts a turn by itself (lib.rs:225 `finish_unsettled`; driver.rs:478 test `..._registers_a_running_turn_for_an_orphaned_message`; observed). So the perch **never auto-restarts** a door, and the page must label Resume as "may continue the last turn".

**Stop.** SIGTERM (the door drains, removes its token, main.rs:660) -> 10 s -> SIGKILL and the perch deletes the token file. `eidolon door quiesce` was considered and rejected: it journals a `quiesced` marker naming a destination (main.rs:396-417) that reads as a hop to another machine, which an idle stop is not.

**Idle reaper** (every 60 s, per live child the perch owns):

```text
if panes(id) > 0                          -> last_watched = now; keep
if now - last_watched < idle (30 min)     -> keep
if watcher not caught-up                  -> keep      (unknown = keep)
if running                                -> keep      (turn-state frames, stream.rs:74, :177)
if parked                                 -> keep      (inferred, below)
if pending asks > 0                       -> keep      (an ask frame not yet ask-settled)
else                                      -> stop (as above), state kept
```

| Signal | How the perch reads it today | Label |
|---|---|---|
| running | watcher's `turn-state {running}`; but `hello` has no `running` and `turn-state` only fires on change (stream.rs:59-72, :177), so a door that is mid-turn when the watcher connects looks idle until the next change | observed; web-ui gap 1 |
| parked / timer armed | not exposed by any route. Inferred: the last `turn-settled` (replayed or live) has `stop_reason: "waiting"` (wire.rs:163-167) and no `trigger-fired` or `turn-state running` after it | reasoned; wrong only toward *keeping* a door (a park cleared without firing, wait.rs:883, still reads as parked) |
| busy (cross-check) | roster `meta.busy` (presence.rs:52), when the swarm is on | observed |

**Accepted loss.** A stopped session leaves the swarm roster, so peer mail and doorbell rings no longer reach it until resumed (reasoned from presence.rs `deregister`).

---

## 5. Proxy: `/s/<id>/api/<route>`

**Path.** `<route>` must match `^[a-z][a-z0-9-]{0,31}(/[a-z0-9-]{1,32})*$`, no `%`, no query string; method GET or POST. A shape check, not an allowlist of five names: new door routes (web-ui M3's `/api/commands`) pass without a perch change. The trade: the perch token grants whatever the door grows. It already grants everything the door grants, so this adds no new class of power (reasoned).

**Request rewrite.**

| Header / part | page -> perch | perch -> door |
|---|---|---|
| `Authorization` | `Bearer <perch token>`, checked, removed | `Bearer <door token>` |
| `Host` | must be the perch's | `127.0.0.1:<door port>`; the door checks `Host` on POSTs against its bound address (serve.rs:244) |
| `Content-Type`, `Accept` | kept | kept |
| everything else (`Cookie`, `Origin`, `Referer`, `Sec-*`, `Last-Event-ID` until the door honours it) | dropped | not sent |
| query string | refused (400) | none; the door's `?token=` form (serve.rs:145-158) is never used |
| body | read through `Limited` at 8 MiB, then sent | door caps at 8 MiB too (serve.rs:87, :408) |

**Response.** Status passes through, except **door 401 -> perch 502** `{"error":"door refused the perch"}`, so the page never concludes its own token is bad. Headers kept: `content-type`, `cache-control`; added: `nosniff`, `no-referrer`, `cache-control: no-store`. No `Access-Control-*`, ever.

**Transport.** One fresh TCP connection per request to `127.0.0.1:<port>`, `hyper::client::conn::http1`, 2 s connect timeout, 60 s response-head timeout for POSTs (`/api/launch` spawns under `spawn_blocking`), none for SSE. No pooling: loopback connects are cheap and a pool would outlive a dead door (reasoned).

**SSE passthrough.** Each upstream `Frame<Bytes>` becomes a perch frame as it arrives: no buffering, no re-chunking, no parsing. Backpressure is the door's own: a slow tab stops the perch reading, the door's broadcast subscriber lags, the door sends `lagged` and ends the stream (stream.rs:149-156). The page reconnects and gets a fresh hello + replay (reasoned).

**When the upstream stream ends**, the perch appends one frame of its own and ends the page's stream:

```text
\n\ndata: {"type":"hub-stream-end","session":"<id>","door":"live|stopping|dead","exit":"code:1|signal:9|null"}\n\n
```

The leading blank line terminates any half-written door frame, whose JSON the page then fails to parse and drops (web-ui must ignore unparseable frames). The `hub-` prefix keeps the name out of the door's namespace; the page ignores unknown types, so the door's protocol number is untouched (lib.rs:84-90).

| `door` | Cause | Page does |
|---|---|---|
| `live` | door ended the stream: `lagged`, or a reconnect-worthy reset | reconnect now |
| `stopping` | idle stop, `/stop`, perch quitting, or the door's own `goodbye` (stream.rs:93) | show "stopped", offer Resume |
| `dead` | child exited or startup failed | show the exit, offer Resume (no auto-restart, section 4) |

While `dead`, `/s/<id>/api/*` answers 503 `{"state":"dead"}` and the perch opens **no** connection to the old port (next section, attacker E).

---

## 6. Mirrors

Several panes on one session = several proxied `/api/events` streams to one door. The door serves any number of streams, each with its own subscribe + replay (stream.rs:104-139, observed). The perch adds nothing but a pane count per id for the reaper. Two panes answering one ask: the second `answer` gets the door's 409 (lib.rs:13-16), passed through. Cost: each pane replays the whole branch on connect (web-ui gap 10); **hypothesis** fine below a few thousand records; measure a 2 000-record branch times 4 panes.

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

Deny list (case-insensitive, whole name): `.git` (hidden entirely), `.env`, `.env.*`, `*.key`, `*.pem`, `*.p12`, `*.pfx`, `*.kdbx`, `id_*`, `*secret*`, `*.token`, `.netrc`, `.ssh`, `.gnupg`, `.aoide`, `.envrc`, `.pgpass`, `.npmrc`, `.pypirc`, `auth.json`, `creds.json`, `*credentials*` (a substring). A config knob can add names, never remove these. A segment shaped like an NTFS 8.3 alias (`~` then a digit, so `GIT~1` for `.git`) is refused as a path segment like a denied name, and the cwd is held to the same rules below its `--root`.

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

**Allowed aoided ops: `ping` and `subscribe`. Nothing through `dispatch`**, read-only or not. Reason: `dispatch` runs the same `cli::dispatch::dispatch` as every other door with "no daemon-specific permission table" (daemon.rs:60-65, :1051-1075), so a perch allowlist would be the only thing between the browser and every aoide command, one bug from a write. Read-only snapshots go through the fixed-argv CLI calls above. If a dispatch path is ever wanted, the rule is: a compiled-in list of exact command paths (`node list`, `mesh`, `graph`) with `--json` and no argument taken from a request.

**Never:** proxy Aoide's A2A door (`127.0.0.1:8710`) to the browser; run `node pull`, `mesh pair`, `pair`, `spawn`, `send`, `allow`, `node status` (its `--json` carries the full registry row, Node-Federation.md:208-209).

---

## 9. Launcher

`/api/launch` goes through the door, per session: `POST /s/<id>/api/launch {"window","path"?}`, proxied like any other route. The window opens on the door's machine, which is the perch's and the browser's (lib.rs:17-20; launch.rs:1-10). The perch has no launcher of its own and no second copy of `[terminal]` config. Reported working in WSL2 with `wt.exe` (web-launch commit `22021b9` message).

---

## 10. Security

### 10.1 Threat model

| # | Attacker | Can | Perch's answer |
|---|---|---|---|
| A | hostile page in the operator's browser | fire cross-origin requests at `127.0.0.1:<perch>` | token in a header only (a cross-site simple request cannot set one); no CORS headers; `frame-ancestors 'none'`; `Sec-Fetch-Site` check |
| B | DNS rebinding | make its own name resolve to 127.0.0.1 | `Host` check on **every** request, SSE and statics included (stricter than the door, which skips it on `/api/events`, lib.rs:33-36) |
| C | another local OS user | connect to loopback ports, read world-readable files and `/proc/*/cmdline` | token only in `0600` files under an owner-checked runtime dir; no token in argv, env, URLs or logs |
| D | a compromised extension in the UI | run script in the page's origin | **nothing stops it.** It holds the perch token: it can type into every session, approve every ask, start and resume agents. See 10.4 |
| E | another local user racing a dead door's port | bind the freed port and receive what the perch sends there | a child's exit flips its state to `dead` before any new upstream connect; the door token died with the door. **Residual**: a request already in flight at the instant of exit (reasoned; narrow) |
| - | same-uid attacker | reads every token file | out of scope, as for the door (web-door-security.md section 1) |

### 10.2 Gates, in the order a request meets them

| # | Gate | Refusal | Basis |
|---|---|---|---|
| 0 | loopback-only bind, at start | no start | lib.rs:137 |
| 1 | token on `/hub`, `/hub/*`, `/s`, `/s/*`: `Authorization: Bearer`, or the session cookie of 10.5 on a `Sec-Fetch-Site: same-origin` request; never a query parameter; constant time (`subtle`). `POST /hub/redeem` alone is exempt; `POST /hub/code` takes the bearer token only (cookie: 403). Every gated answer carries `Content-Security-Policy: default-src 'none'; sandbox` | 401, empty body | serve.rs:165, :240; the page uses fetch-based SSE (web-ui.md section 9, M1), so the door's `?token=` escape hatch is not needed |
| 2 | `Host` equals the bound `ip:port` (or `localhost:port`, Q8) | 403 | serve.rs:118 |
| 3 | `Sec-Fetch-Site`, when present, is `same-origin` or `none`. **The cookie** counts as a credential (gate 1) only when it is present and `same-origin`; a bearer request keeps "absent passes" | 403 (cookie without it: the gate 1 401) | new; a same-origin page always passes, curl sends none |
| 4 | method and path shape | 404 / 405 | section 3 |
| 5 | body `Limited` at 8 MiB | 413 | serve.rs:87, :408 |
| 6 | per-route: id regex, cwd under `--root`, tree containment, session state | 404 / 409 / 422 | sections 4, 7 |

### 10.3 What the perch token grants

**Everything every door token grants, across every session the perch can reach, at once**: read every transcript, send and steer, cancel, answer any ask (including policy approvals), open terminal/editor/file windows. **Plus**: start a new agent in any directory under `--root`, resume any kept session (which may continue a turn and spend, section 4), stop sessions, list directory names under any live session's cwd, read the mesh snapshot and feed. Leaking it is leaking the operator's whole bench.

### 10.5 Login: code, redeem, cookie (2026-10-05)

```text
hoot -> wsl.exe -> perch-open.sh --code
          curl -H @tokenfile  POST /hub/code      -> {"code": c}      (30 s, one use)
        prints  http://127.0.0.1:<port>/#code=c   -> hoot opens it in the browser
page:   strips #code=c from the URL, POST /hub/redeem {"code": c}
          -> 204  Set-Cookie: minerva_<port>=<secret>; HttpOnly; SameSite=Strict; Path=/
          -> 403  (wrong, used or expired: one empty answer)
then:   every fetch and stream carries the cookie by itself; no token is in page script at all
```

**The cookie is not the token.** Each redeem mints its own random secret, kept in memory only (at
most 32; the oldest is dropped). The token file stays the single master credential: a cookie
that leaks from a browser profile can be dropped by restarting the perch, and never
reveals what the launcher reads. A restart forgets every cookie, which is the revocation.

**Threat notes.**

- The code is in the launched URL, so it can appear in browser argv, history and a process list. That is harmless: it works once and dies in 30 s, and the page spends it in the first second.
- Cookies are **not isolated by port** on `127.0.0.1`: another loopback server (any port, any local user's process that can bind one) is sent this cookie by the browser. That is why the name carries the port (two perches do not overwrite each other's login) and why the cookie is `HttpOnly` (page script, including a compromised extension's, cannot read it). It does not stop a hostile *server* on another port; that case is attacker D in 10.1 and is out of reach of any loopback web login. The gate backstop is gate 3: a request from another port on the same host is `Sec-Fetch-Site: same-site`, which is refused, and `SameSite=Strict` keeps cross-site pages from sending it at all. A browser that sends no fetch metadata would be admitted by "absent passes" (right for curl and the bearer, wrong for an ambient cookie), so the cookie authenticates only on a request that says `same-origin`; the page's own `fetch` always does.
- `POST /hub/code` accepts the bearer token only. A script in the page's origin cannot read the HttpOnly cookie, but could otherwise call the route with it, read the code (same-origin responses are readable) and hand it to an accomplice that redeems it for a session of its own.
- A reload or a new tab has no `#code=` and no token in it, and is signed in by the cookie alone: the page probes `GET /hub/sessions` once and boots on a 200 (webui/term/README.md).
- Redeem is the one open `/hub` route. It keeps gates 2 (Host), 3 (fetch metadata) and 5 (the body cap, 1 KiB here), compares in constant time against every outstanding code, and answers every failure with the same empty 403. Codes are 256 bits; no rate limit is applied.
- No `Secure` flag: the origin is plain-http loopback (the operator's decision D3), so the cookie is sent over unencrypted loopback; the threat is a local process, not a network.
- The cookie has no `Max-Age`: it ends with the browser session or the perch, whichever is first.
- The proxy never forwards the cookie (it rebuilds the door request from five fields).

### 10.4 Must never happen

| Never | Enforced by | Test |
|---|---|---|
| a token in argv or env (perch's, a child's, a browser's) | the door mints its own; perch passes none | read `/proc/<pid>/cmdline` and `environ` of perch and children |
| a token in a URL | Bearer header only; query strings refused on `/s` | request with `?token=` -> 400/401 |
| a token in a log or stdout | no header, body or query logged; the perch never prints its token | run at `RUST_LOG=trace`, grep all output for both tokens |
| a door token reaching the page | stripped response headers; perch never serializes it | grep every response body in the suite |
| CORS headers, an `OPTIONS` answer | none written; `OPTIONS` is 405 | assert absent |
| a listing outside a session cwd, or of a denied name | section 7 | symlink-out, `..`, `%2e%2e`, `.env` cases |
| two doors on one log | section 4 step 2 | spawn twice -> 409 |
| a dispatch to aoided, or a proxy to `:8710` | no code path | grep in review; mutation below |

**Accepted risks.** (1) Attacker D is total; the remedy is an extension sandbox (sandboxed iframes fed by `postMessage`), a web-ui M4 decision, not a perch one (web-ui.md Q9). (2) `localhost` in `Host` if Q8 says yes. (3) Tree TOCTOU (section 7). (4) Stopped sessions miss peer mail. (5) The PDEATHSIG hypothesis in section 2.

---

## 11. Crate layout and dependencies

```text
Minerva/perch/                own Cargo.toml + Cargo.lock; not an eidolon workspace member
  src/main.rs                 clap argv, signals -> CancellationToken, run
  src/lib.rs                  Perch::run(opts, cancel)
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
  tests/http.rs               real HTTP against a perch on 127.0.0.1:0
```

| Dep | Why | Already in eidolon's graph |
|---|---|---|
| `tokio` (rt-multi-thread, net, process, signal, fs, time, io-util, sync, macros), `tokio-util` | runtime, children, signals, cancellation | yes, 1.53 / 0.7 (eidolon Cargo.toml:41-42) |
| `hyper` 1 (`server`, `client`, `http1`), `hyper-util` (`tokio`, `server`, `server-graceful`, `http1`), `http-body-util`, `bytes` | the door's exact stack, plus hyper's `client` feature for the proxy | yes (crates/web/Cargo.toml) |
| `async-stream`, `futures-util` | SSE bodies | yes |
| `serde`, `serde_json`, `anyhow`, `tracing`, `tracing-subscriber`, `clap` 4 | the usual | yes (Cargo.toml:37-59) |
| `toml` 0.8 (parse + display only) | the Minerva home's generated config: the member's `config.toml` is parsed and re-written, and an explicit `--eidolon-config`'s `sessions_dir` is read, with a real parser instead of a line scan | yes; third party, no `eidolon-*` |
| `subtle` 2 | constant-time token compare | yes |
| `harnox` (git tag v0.3.8, no default features) | `random_token`, `write_atomic_0600`: one mint of a secret, the door's own reason | yes (Cargo.toml:35). Noah's crate, **not** eidolon |
| `libc` | `prctl(PR_SET_PDEATHSIG)`, `getuid` | in the lock, 0.2.189 |
| dev: `tempfile` | test dirs | yes |

No axum, no reqwest, no dependency on any `eidolon-*` crate: the perch knows the door only as a binary, a stdout contract, a token file and HTTP.

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
| shutdown | SIGTERM the perch: children gone, all token files gone; kill -9 the perch: children gone (PDEATHSIG hypothesis) |

**Mutation checks** (each must turn a named test red): remove the token gate; remove the `Host` check on SSE; drop the Authorization rewrite; forward `Cookie`; remove `Limited`; remove `starts_with(root)` in the tree; remove the `running` guard in the reaper; send one `dispatch` in `mesh.rs`.

Run in WSL2 (ext4 target dir), like the door's reported numbers.

---

## 13. Milestones (matching web-ui.md section 9)

| H | With UI | Scope | Door needs | Done when |
|---|---|---|---|---|
| **H1** | M1 | process, token, statics, gates; spawn / resume / stop one session; proxy incl. SSE and `hub-stream-end`; `/hub/sessions` | **nothing** (reasoned: existing flags main.rs:325-358, existing stdout lines lib.rs:209-210, existing routes) | web-ui M1's done-when passes through `/s/<id>/`, and the secrets tests in section 12 are green |
| **H2** | M2 | several sessions tiled; mirrors; idle reaper; `/hub/events`; `/hub/tree`; launcher passthrough | W1 wanted (exact `running`) | two sessions in two panes plus a mirror, one idles out and resumes from its log |
| **H3** | M3 | `/hub/mesh`, `/hub/mesh/events`; commands pass through unchanged | the registry, for the page, not the perch | mesh roster and live `gate` events on screen with Aoide running; nothing when it is not |
| **H4** | M4 | nothing new in the perch; decide the extension sandbox (10.4) | nothing | an extension renders without holding the perch token, or the risk is written down as accepted |

---

## 14. Door changes the perch would want (none required for H1)

| # | Change | Why the perch wants it | Size |
|---|---|---|---|
| W1 | `running: bool` in `hello` (web-ui gap 1) | reaper reads a fact instead of waiting for a transition | one field, additive |
| W2 | `parked: bool` (or the waiting condition) in `hello` and a frame when it changes, from `Parks::is_armed` (wait.rs:839) | replaces the `stop_reason: waiting` inference | additive |
| W3 | a machine-readable ready line, e.g. `{"listening":"127.0.0.1:P","token_file":"..."}`, or `--ready-fd N` | the perch parses human `println!`s today | small |
| W4 | (core, not door) an advisory lock on an open session log | two writers on one log are unguarded (section 4 step 2); the perch's roster check misses swarm-off consumers | small, upstream decision |

---

## 15. Open questions

| # | Question | Default if unanswered |
|---|---|---|
| Q1 | **Windows members.** The perch and doors run in WSL2 (both Unix-only). Does a Windows browser reach WSL2 loopback reliably: default localhost forwarding or `networkingMode=mirrored`? Who opens the browser: `minerva perch --open` via `wslview`/`explorer.exe`, with web-ui option B's `0600` redirect file, which Windows must then read through `\\wsl.localhost\...` (**hypothesis**, untested)? The launcher already reaches `wt.exe` (reported). | WSL2 + mirrored mode documented; test on one member's box before H1 ships |
| Q2 | Should the perch be an eidolon swarm peer? For: doorbell, appears in `door list`. Against: it would publish a `meta.json` for a session it is not, and reading the roster needs no registration (`swarm::scan` is a directory read, presence.rs:341) | **no**, read-only roster scan |
| Q3 | Idle timeout default | 30 min, `--idle` flag, `0` disables |
| Q4 | Naming. "hub" collides with `aoide node hub <name>` (Node-Federation.md:193-197) in the same club's vocabulary. Candidates: `minerva hub`, `minerva desk`, `minerva perch` (owl, fits the mascot) | **RESOLVED 2026-09-27: `minerva perch`.** The crate, binary, token file and prose in this doc use `perch`; the `/hub/*` route namespace and the `hub-stream-end` marker stay (wire contract shared with webui/term and the perch's own gate) |
| Q5 | Doors started by hand (not our children): adopt them (read their token file, proxy) or list as `live-elsewhere` only? | list only; adopting means proxying a door whose lifetime the perch does not own |
| Q6 | `--root` default for new sessions | `$HOME` |
| Q7 | Mesh event classes: `audit` + `gate`, or also `secret` (names only)? | `audit`, `gate` |
| Q8 | Accept `Host: localhost:<port>` as well as the IP? A Windows browser over WSL forwarding may send it; the door refuses it (serve.rs:118-132 compares the IP string) | accept `localhost` literally; no other names |
| Q9 | Should `/hub/tree` also serve kept sessions (root from the log's start record)? | no, live only |

## Minerva home (2026-10-05)

UI sessions live in `~/Minerva` (`MINERVA_HOME`), not eidolon's shared sessions dir, so the page lists only its own chats and a new chat gets a real default model instead of `mock`. `bin/perch-open.sh` creates `sessions/`, `scratch/`, `projects.json` and `minerva.toml` (`default_model`) when missing and starts the perch with `--home`. **The perch generates `<home>/config.toml` at every start**: eidolon's `--config` replaces `~/.config/eidolon/config.toml` outright (`Config::load` reads that one file), so the member's own config is parsed (a TOML parser; every key and table kept, an empty table when there is none), `sessions_dir` is forced to the absolute `<home>/sessions` (eidolon does not expand `~` in that key, and a relative one is read from each door's cwd), `default_model` is set only when absent (from `<home>/minerva.toml`, else `chatgpt:gpt-5.6-luna`), and the file is written atomically, mode `0600`, under a "generated, edit the other files" comment. The files beside the config (`providers/`, `policy.rn`, `secrets`) are `config_dir()`-relative and shared as before. Every door is `eidolon --config <home>/config.toml web ...`, new or resumed, and the perch lists the same `<home>/sessions`. `--home` conflicts with `--eidolon-config` and `--sessions-dir`; an explicit `--eidolon-config` alone has its `sessions_dir` read with the same parser and a relative or `~` value refuses the start. A new session with no `cwd` works in a fresh `<home>/scratch/<YYYYMMDD-HHMMSS>/` made per chat (`-2`, `-3` on a collision), so two chats never share files. The old shared `~/.local/share/eidolon/sessions` chats are listed read-only (see "Titles, saved chats and the second source" below). The perch lives in WSL: it dies if WSL idles out, and the fix is `vmIdleTimeout=-1` under `[wsl2]` in `%UserProfile%\.wslconfig` (a `systemd --user` unit is the unbuilt alternative).

## Titles, saved chats and the second source (2026-10-05)

**Titles (D4 reversed).** The perch names every row, from eidolon's outputs only (it never decodes the `EIDO` log): the roster `meta.json` `title` for a door that is live, else the last column of `eidolon [--config <home config>] sessions` (`first_asked`, the first message flattened, cut to 60 characters by that command), else `<project folder> · HH:MM` (`scratch · HH:MM` under `<home>/scratch`, `chat · HH:MM` when the perch never saw the cwd, which is inside the journal). Cached by log path: a titled log is final, an untitled one is asked about again only when its mtime moved, and a listing that changes nothing runs nothing (a failed run is not retried for 30 s). Bounded: 10 s, 4 MiB. Cold list over 245 logs, two sources: 47 ms; warm: 2 ms.

**Saved chats are readable.** `GET /hub/sessions/<id>/log` runs `eidolon [--config C] log --json <path>` (15 s, 16 MiB) and returns its lines, one record each (`{id, parent, ts_ms, kind}`; `kind` is a variant name or a one-key object). It starts nothing; the page maps the records onto the same row model a live stream feeds (`webui/term/core/saved.js`), draws them without a composer, and offers `[ continue this chat ]`, which is the resume route of section 3 and the only thing that spends. Over the cap is a 413 and the page says so; continue still works.

**Two sources.** The Minerva home's `sessions/` is the one the UI owns. Under `--home` the perch also lists eidolon's own sessions dir as it resolves without the Minerva config (the member's `sessions_dir`, else `~/.local/share/eidolon/sessions`) as read-only rows `origin: "other"`, state `kept`; an id both have is the home's. Their `log` runs without `--config`; resuming one is allowed and starts a door with the home's config on that log in place (the page says so under the action). While running it is an ordinary live row (no `origin`), and after it stops it is `other` again. The page folds these under `older chats (N)` at the bottom of the sidebar and shows them nowhere else. Nothing in that dir is ever written, moved or deleted by the perch. This closes the "old shared dir behind a fold" deferral in "Minerva home" above.

## Model and mode (2026-10-06)

The composer's `model` and `mode` menus are real. A running door cannot change either (`eidolon web` takes `-m` and the global
`--yolo` at start; the door's routes are `events`, `say`, `answer`, `launch`, `cancel`), so `POST /hub/sessions/<id>/restart`
is **stop the door cleanly, resume the same log with the new flags**: the id, the cwd (it is in the log) and `--config` are
kept, the pane reconnects and replays, and the page adds one transcript line (`switched to <model> - the chat restarted
between turns, nothing lost`). It is refused while a turn runs: the perch reads `turn-state` off the stream it holds to each
door (`watch.rs`, bounded line scan, no transcript kept) and answers 409 `wait for the turn to end`. That stream is now kept for
a resumed door as for a new one; before, a resume dropped it. When the new flags do not come up (an unknown model), the chat
is started again on the old ones and the 503 says `reverted`. A request for the flags the door already has restarts nothing.
`GET /hub/models` is `eidolon models` parsed (`provider:model` rows, name, last column as the note), 10 min, one answer per
process. **Effort and mic are not offered**: `web` has no effort flag, and nothing in the page or perch records or
transcribes audio. The perch's routes after this slice: `/hub/models`, `/hub/sessions/<id>/restart`.

## Projects and the folder picker (2026-10-06)

**Model.** A project is a folder with a name: `<home>/projects.json`, `[{name, path, created}]`, written atomically with mode `0600`, one entry per path. A chat is in a project when its cwd is the project's path or inside it (the deepest project wins). The perch also keeps `<home>/chat-cwds.json` (`{id: cwd}`), because a saved chat's cwd is inside the journal the perch never decodes; saved rows of `GET /hub/sessions` carry `cwd` when this perch started or resumed the chat. A folderless chat is a fresh `<home>/scratch/<stamp>/` each time; the stamp is a directory name and is never shown (the title fallback is `new chat · HH:MM` for scratch and `<project> · HH:MM` in a project; the pane frame shows the title and the project's name only).

**Routes.** `GET /hub/projects`; `POST /hub/projects {path, name?}` (path a directory under a root and passing the picker's rules, else 422; name trimmed, at most 100 characters, no control characters; the same path twice returns the existing entry); `DELETE /hub/projects/<n>` forgets entry `n` (0-based) and touches no file. `DELETE` is allowed for that one prefix only; every other gated method but GET and POST stays 405.

**The picker (D8: in-page).** `GET /hub/dirs?path=<abs>` lists subdirectories only; with no path it lists the roots, labelled (the Windows profile as `C:\Users\<you>`). `POST /hub/dirs {parent, name}` makes one folder. Safety is section 7's, applied to a path rather than a session cwd: absolute, no `.`, `..`, NUL or backslash (the page converts a pasted `C:\...` or `\\wsl.localhost\...` path first); canonicalized and under a root, so a link that leaves them is refused; every segment below the root against the deny list and the 8.3 `~digit` shape, on the canonical remainder and on the spelling asked; entries are real directories (`d_type`, never followed: a link is not offered), and dot-folders, denied names and aliases are left out; the scan stops at 100 000 entries and 2 000 are answered (`truncated`); at most 4 listings run at once (503). Every refusal is the same empty 404, so a denied folder cannot be told from a missing one. A new folder's name has no `/`, `\`, `..`, control character or leading dot, is at most 100 characters, and is not a denied or 8.3-shaped name; `create_dir` (one level) never reuses anything (409 on an existing name); the parent is re-checked on every call (422).

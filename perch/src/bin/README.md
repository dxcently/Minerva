# src/bin/ — the stand-ins

Test doubles, and nothing else lives here. Each is built **only** with `--features test-bins`
(`required-features` in `../../Cargo.toml`), so a plain `cargo build` ships no test double and a
plain `cargo test` compiles none; the integration tests that need one are
`#[cfg(feature = "test-bins")]` and reach the binary through `CARGO_BIN_EXE_<name>`.

| File | Stands in for | State |
|---|---|---|
| `fake_eidolon.rs` | `eidolon`: the door (`web`), `resume`, the read-only `sessions`, `log --json [--after N]` and `models`, and the seams below | built |
| `fake_eidolon/workflow.rs` | `eidolon [--config C] workflow run\|resume ...` | built: a case per start (`FAKE_WORKFLOW_CASE`, a letter or a file read at each start), `_STEP_MS`, `_LINGER_MS`, `_HOLD_MS`, `_ENV`, and `_DEAF` (ignores SIGTERM, holds a child, for the SIGKILL path) |
| `fake_eidolon/meta.rs` | `eidolon [--config C] tools\|plugins\|policy-rows` | stub until R3: says so on stderr, exits 2 |
| `fake_eidolon/asks.rs` | the door's open asks: `FAKE_ASKS` (ids unique per process; a replay of the model's calls and the turn markers), `FAKE_LAG` (a `lagged` frame, no loss), `409` for a second answer and `422` for one not offered | built (A1a) |
| `fake_eidolon/agent.rs` | an agent's tool call into the perch: `FAKE_AGENT_CALL` | AG: after a say, the fake door calls the perch from its own socket (and from an `sh -c curl` child, which is refused); honours http_proxy and no_proxy as reqwest does |
| `fake_aoide.rs` | the `aoide` CLI | stub until R2a: says so on stderr, exits 99 (the code "anything else" gets) |
| `fake_minerva.rs` | the installer's `minerva` | stub until R4: says so on stderr, exits 2 |

`fake_eidolon.rs` is the only file that calls the seams: `workflow` and `meta` are dispatched by subcommand, after the global flags
(`--config`, `--yolo`) are parsed into a `Cmd`, and each `run(&Cmd) -> i32` is the process's exit code; the `asks` and `agent` hooks are called
after a `say` is accepted, when an `/api/answer` has passed its checks, and when an `/api/events` stream has been sent `hello`. The owning slice
fills its own file and never edits `fake_eidolon.rs`. The submodules sit in `fake_eidolon/` and are declared with `#[path]`, because a bin's root
file looks for its modules beside itself, where `src/bin/<name>.rs` would be built as a bin of its own.

## fake_eidolon

`fake_eidolon.rs` is a test double for `eidolon web`.

It models the door **as of eidolon `0194f2b`** (the contract the H1c proxy is written
against): the argv `web --bind ADDR (--cwd DIR | --session PATH) [-m MODEL]`, the two boot
lines, a `0600` token file it mints and never prints, and the `/api` routes `events`, `say`,
`answer` (with the ask `note`), `cancel` and `launch`, each behind the door's Bearer and, on a
POST, its `Host` check. Request bodies are read by `Content-Length` or chunked and refused
past 8 MiB. It is hand-parsed HTTP/1.1 with `Connection: close` replies and no `hyper`, so
"hold the stream open" and "the peer closed" are explicit. When the real door's contract
moves, this file moves with it, and the tests that fail are the ones to read.

Knobs, by environment (they reach it because a door's environment is inherited unchanged):

| Knob | Does |
|---|---|
| `FAKE_LOG` | file every request is appended to: method, path, `auth=`, a `hdr ...` line, then `body-len=N`; `<FAKE_LOG>.body` holds the last body |
| `FAKE_SESSIONS_DIR` | where a new session's log is written (the door's own default otherwise) |
| `FAKE_SILENT` | never print the boot lines (the startup timeout) |
| `FAKE_MODE=0644` | write the token file world-readable (the perch must refuse it) |
| `FAKE_TOKEN_ELSEWHERE` | write the token file outside `<runtime>/eidolon` |
| `FAKE_EXIT_AFTER_MS` | send half a frame to every open stream, then exit with `FAKE_EXIT_CODE` (default 1) |
| `FAKE_EXIT_CODE` | the code `FAKE_EXIT_AFTER_MS` exits with; 0 is a door that stops on its own, cleanly |
| `FAKE_EXIT_BEFORE_BOOT=<code>` | exit with that code before any boot line (0: a clean exit while `starting`, which is a failed spawn) |
| `FAKE_IGNORE_SIGTERM` | never exit on SIGTERM (the SIGKILL path) |
| `FAKE_REFUSE=<path>` | answer 401 on that path even with the right token |
| `FAKE_LATE_PANE_MS` | hold back the head of the third and later `/api/events` streams (the watcher is the first) by this long, counted as open meanwhile: a pane still being set up when a stop lands |

SIGTERM is answered the way the real door answers it: while any stream is open it does not
exit (hyper's `GracefulShutdown` waits an SSE connection out), and it logs
`sigterm streams=N` with the number it saw. That line is what makes "the perch closed the
streams first" something a test can read. It also logs `sigterm-received streams=N` a beat
(20 ms) after the signal lands, before the 500 ms window in which streams may still close:
that line is the one that says the streams were closed *before* the signal, not merely soon after.

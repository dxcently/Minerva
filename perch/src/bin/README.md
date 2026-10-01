# src/bin/ — the stand-in door

`fake_eidolon.rs` is a test double for `eidolon web`, and nothing else lives here. It is
built **only** with `--features test-bins` (`required-features` in `../../Cargo.toml`), so a
plain `cargo build` ships no test double and a plain `cargo test` compiles none; the
integration tests that need a door are `#[cfg(feature = "test-bins")]` and reach the binary
through `CARGO_BIN_EXE_fake_eidolon`.

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
| `FAKE_EXIT_AFTER_MS` | send half a frame to every open stream, then exit 1 |
| `FAKE_IGNORE_SIGTERM` | never exit on SIGTERM (the SIGKILL path) |
| `FAKE_REFUSE=<path>` | answer 401 on that path even with the right token |
| `FAKE_LATE_PANE_MS` | hold back the head of the third and later `/api/events` streams (the watcher is the first) by this long, counted as open meanwhile: a pane still being set up when a stop lands |

SIGTERM is answered the way the real door answers it: while any stream is open it does not
exit (hyper's `GracefulShutdown` waits an SSE connection out), and it logs
`sigterm streams=N` with the number it saw. That line is what makes "the perch closed the
streams first" something a test can read. It also logs `sigterm-received streams=N` a beat
(20 ms) after the signal lands, before the 500 ms window in which streams may still close:
that line is the one that says the streams were closed *before* the signal, not merely soon after.

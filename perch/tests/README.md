# tests/ — a real perch over a real socket

`manual_policy.rs` checks the manual mode's policy: eidolon's own `policy.rn` plus the wrapper, compiled through Rune and asked about every tool and shell leaf, then (when `eidolon` is on `PATH`, with `XDG_CONFIG_HOME`, `HOME` and `XDG_RUNTIME_DIR` in temp dirs) the real `eidolon policy` and a headless `-m mock` turn under each config.

`http.rs` is cargo's integration test for this crate, and it is the only test that proves
the parts a unit test cannot reach: the argv, the two boot lines, the `0600` token file a
supervisor actually gets, and every gate as it is met by a request.

- The **binary** is spawned as a child with `--bind 127.0.0.1:0`, so the port is the one
  the first boot line names; the token is read out of the file the second line names,
  never out of the child's output.
- Requests are **hand-written HTTP/1.1** on a `TcpStream`, because `Request<Incoming>`
  cannot be built outside a connection — which is also why each gate has a case here and
  not in `src/`.
- The child runs at **`RUST_LOG=trace`** for every test, not just the secrets one: the loud
  path is the one a token could leak through.
- Every response every test looks at is checked for an `Access-Control-*` header before the
  test sees it, so a later route cannot quietly add one.

The **H1b** tests in the same file drive a real child process: `--eidolon` points at
`src/bin/fake_eidolon.rs`, a stand-in door that speaks the door's contract (two boot lines, a
`0600` token file it mints itself, `/api/events` behind that token, and SIGTERM that does
*not* end it while a stream is open, as hyper's `GracefulShutdown` does not). It is a `[[bin]]`
with `required-features = ["test-bins"]`, so those tests are `#[cfg(feature = "test-bins")]`
and are run with `cargo test --features test-bins`; `cargo test` alone still passes everything
that needs no door.

The **H1c** tests (same file, same feature) drive the proxy against that stand-in. They cover:
the rebuilt request (the door's Bearer and rewritten `Host` arrive, `Cookie` and the page's own
`Authorization` do not); the door's current contract passing with no route-specific code; the
8 MiB body cap, which holds on a chunked stream, and a body that reaches the door while it is
still arriving; a door's 401 becoming a 502; pane frames arriving as they happen, several panes
counted and a closed tab not; a stop closing its panes **before** the SIGTERM lands (read off
the fake's `sigterm-received streams=0`, including a pane still being set up, via
`FAKE_LATE_PANE_MS`); a pane that has stopped reading holding up neither a stop nor a perch
shutdown; exactly one `hub-stream-end` frame per pane, `stopping` or `dead` as the cause says;
a session that is not live refused without a connection; a clean stop listing the session `kept` (and a SIGKILL or crash `dead`); a stale roster row (dead pid) not blocking a resume; and a rejected new session leaving no
door token file behind.

The **H2 tree** tests (same file, same feature) build a fixture under a live session's cwd (`.env`, `id_rsa`, `.git`, links out and in, a non-UTF-8 name) and cover the listing, every path escape and deny-list spelling (404), the 2 000 cap, the session states (409) and the strict query (400).

The post-review tree tests add: NTFS 8.3 aliases (`GIT~1`, `a/SSH~1/x`, escaped, through a link) as the same empty 404; a session whose cwd is `.ssh`, `GIT~1` or under `.git`/`.gnupg` listing nothing; a resumed session whose cwd is outside `--root` listing nothing; denied, aliased and missing answers byte-identical (headers, `date` apart); a burst of 48 over a 40 000-entry directory seeing 503s and then free slots; and a door that exits 0 on its own while live ending `kept` (`FAKE_EXIT_AFTER_MS` with `FAKE_EXIT_CODE=0`) but `dead` while starting (`FAKE_EXIT_BEFORE_BOOT=0`). FIFO, dangling-link, `0000`-directory, deleted-cwd, `.git`-file, scan-bound, `dead(id, pid)` and `HelloScan` byte-at-a-time and 2 MiB cases are unit tests in `src/tree.rs`, `src/sessions.rs` and `src/watch.rs`.

Directories are `tempfile`'s, under `/tmp` rather than the 9p mount this repository lives
on: the `0600` and `0700` checks need a filesystem that keeps its promises.

## The harness, and the file each slice owns

The perch child, the requests and answers, the stand-in door's log and the SSE pane reader that `http.rs` is written with live in
[`common/`](common/) (`mod common;` in each file), so a later slice starts a perch the way `http.rs` does without copying 600 lines. `http.rs`
keeps the cases of the slices that are built. Every slice that is not yet built has **one file here, with one pin** that says what its seam does
today (the empty 404 behind the gate, the 401 of an agent path, or a fake that refuses); the slice replaces the pin with its own tests and edits
no other test file.

| File | Slice | The pin today |
|---|---|---|
| `runs.rs` | R1a | built: start, park, answer, cancel, abandon, adopt, the health gate, against the fake's `workflow` (`FAKE_WORKFLOW_*`) |
| `runs_log.rs` | R1b | built: `log?after` while a run steps, one child per unchanged journal, the query refusals, the graphs list and file, names outside `--run-graphs` |
| `aoide.rs` | R2a | `fake_aoide` refuses every argv with exit 99 |
| `mesh.rs` | R2b | `/hub/mesh` and `/hub/mesh/events` are a 404 behind the gate (pinned in `http.rs` until this file took them over) |
| `meta.rs` | R3 | `/hub/meta/...` is a 404 behind the gate; the fake's `tools`, `plugins` and `policy-rows` refuse |
| `install.rs` | R4 | done: the notice, keys list and inspect, the arm (bearer only, never a page), add and remove spending it once, listed/mismatch/unknown/updated/Nix answers, writes that outlive the client, the child's env list |
| `eidolon_config.rs` | SA2 | done: a door and every other eidolon call started with `--config` see `EIDOLON_CONFIG` naming that file (a shell shim in front of the stand-in records it); a door with no config has the name removed even when the perch inherited one |
| `agent.rs` | AG | done: scoped tokens, attribution to a door (its children refused), files on exit by identity, refused starts leave nothing |
| `term.rs` | T1b | done: create (size, title, a chat's remembered folder, a listed chat with none starts in `$HOME`, an unknown chat 404), input (octet-stream only, 415/413 before the body, 503 once a shell that reads nothing has its queue full), resize (clamp, 400 shapes), hold, close (a background `sleep` is gone), cwd 422, text (digits given once; a count over 400 reads 400 lines; 7000 characters at most, the end kept), the 17th a 409, an ended terminal read back from `terms.json` (409 for input, resize and hold), ids as the empty 404 and a wrong method as 405 with `Allow`, no environment or token in an answer, the gate (401; a bearer cross-site 403; a cookie cross-site 401; a cookie same-origin 200) |
| `term_stream.rs` | T1c | done: the roster first; a join resumes from a `since` seq the ring holds (term-out from that seq, no reset) and rebuilds a screen when the ring has moved on (`snap` true, DECCKM kept, a reset first); a join after a region the shell set rebuilds the region (`CSI 2;10 r`); an exact replay (`snap` false, `seq` = data length); a seq past the end is a reset; a `since` seq the ring has passed is a snapshot reset at the same end, and one it holds resumes with the bytes from it (no reset); a live client the ring passes while it reads nothing is reset with a snapshot at the shell's end; a lagged change channel is listed again and the terminal that left is closed; a burst of one-byte writes is flushed in a few frames; a pager (`less`, skipped when absent) is rebuilt with its rows and alternate screen; live output and a replay in frames of at most 32 KiB, a replay's chunks at its seq; open, close, size, title, hold and exit announced; the keep-alive knob; every frame a JSON object with a string `type`; a SIGTERM ends the stream with `hub-stream-end` `stopping` inside 3 s; the gate (401 with no credential), 405 with `Allow: GET`, 400 for a malformed or repeated `since` |
| `term_agent.rs` | T2 | `/hub/term/a/<verb>` is a 404 for the master bearer |
| `notes.rs` | N1 | `/hub/notes[...]` is a 404 behind the gate |
| `mail.rs` | M2 | `/hub/mail[...]` is a 404 behind the gate |
| `mail_agent.rs` | M3 | `/hub/mail/a/send` is a 404 for the master bearer; `/hub/sessions/<id>/mailsend` a 404 |
| `allow.rs` | A1b | `/hub/sessions/<id>/allow` and `/revoke` are a 404 behind the gate |
| `watch.rs` | A1a | none: A1a is built and the file holds its tests (the watcher against the stand-in's `FAKE_ASKS` and `FAKE_LAG`; its `409` for a second answer and `422` for one not offered) |
| `guard.rs` | G1 | `perch::guard::status()` is off and says "not built yet"; `refuses_term_text` refuses nothing |

Two files are not stubs. `deps_smoke.rs` runs a real `/bin/sh` through `portable-pty` into an `alacritty_terminal` screen and reads it back as
plain text (the terminal host's two dependencies, doing what T1a assumes; the probe a scout wrote, kept). `gpl_boundary.rs` parses `Cargo.lock`
and fails on any package named `aoide*`: the perch is MIT and those crates are GPL-3.0. `live/` holds `fake-perch.sh`, the live check a person runs
in a browser (see its README).

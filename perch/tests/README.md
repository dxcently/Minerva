# tests/ — a real perch over a real socket

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
a session that is not live refused without a connection; and a rejected new session leaving no
door token file behind.

Directories are `tempfile`'s, under `/tmp` rather than the 9p mount this repository lives
on: the `0600` and `0700` checks need a filesystem that keeps its promises.

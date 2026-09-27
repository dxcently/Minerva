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

Directories are `tempfile`'s, under `/tmp` rather than the 9p mount this repository lives
on: the `0600` and `0700` checks need a filesystem that keeps its promises.

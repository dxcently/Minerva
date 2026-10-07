# tests/common/ — the harness every test file shares

`mod.rs` is what the top half of `tests/http.rs` was: the perch as a child process, hand-written HTTP/1.1 requests and answers, the
stand-in door's log, and the SSE pane reader. A test file says `mod common;` and `use common::*;`. Cargo does not build this folder as a test of
its own, and each test crate compiles its own copy and uses part of it, so the module allows dead code.

| Item | What it is |
|---|---|
| `Perch` | the child: `start()`, `start_hub(knobs)`, `start_hub_member(args, member_config)`, `stop()`, `get`/`post`/`post_json`, the session helpers (`list`, `wait_state`, `keep_a_log`), the stand-in door's log (`fake_records`, `fake_helpers`, `fake_last_start`, `fake_argv`) |
| `Perch::builder()` | `Builder`: `ui_dir`, `hub`, `arg`/`args`, `env`/`envs`, `member_config`, `port` (0, the kernel's pick, by default), `hold(guard)` (keeps a temp dir or stub alive as long as the perch), `start()` |
| `Perch::assert_not_built(method, path)` | the pin a seam gets until its slice builds it: 401 without a credential, the empty 404 with the token |
| `Request`, `Answer`, `send_oversize` | one request on the wire, and the answer parsed (every answer is checked for a CORS header) |
| `Pane` | one open SSE response read incrementally: `open(perch, id)` for `/s/<id>/api/events`, `open_path(perch, path)` for any gated stream |
| `fake_eidolon()`, `fake_aoide()`, `fake_minerva()` | the compiled stand-ins' paths, `None` without `--features test-bins` |
| `live`, `since`, `wait_record`, `proc_of`, `wait_gone`, `dechunk`, ... | small helpers the `http.rs` cases are written with |

The perch is always started with its runtime dir, XDG dirs and sessions dir in fresh temp dirs and `RUST_LOG=trace`, so no test reads or writes the
operator's, and the loud path (the one a token could leak through) is the one every test runs on.

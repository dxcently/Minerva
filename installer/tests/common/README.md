# tests/common/ — the fixture environment

`mod.rs`, shared by `cli.rs` and `perch_open.rs`:

```text
Env      a temp HOME and XDG_RUNTIME_DIR holding DECOY tokens at the default paths (perch-<port>.token for the stub port and 47820, the
         agent token files); `cmd()` gives a Command with a CLEARED environment and the curl shim first on PATH
Stub     an HTTP server on 127.0.0.1:<port> that answers one fixed status/body and records every request (line, headers, body);
         `start_slow` holds each answer back (a perch that is slow to reply); refuses port 47820; stops when dropped
shim     `curl` first on PATH: logs argv, the mode and contents of each `-H @file`, refuses any call aimed at :47820
port_lock  tests that use the stub port, or ask what is on it, take turns: a flock on minerva-installer-stub.lock in the temp dir, the same
         file for every target dir, so it holds across threads, test binaries, `cargo test` runs and worktrees; the system lets go of it if the holder dies
```

# src/ — the perch's five modules

One job each, and the reason for the split is the tests: everything a unit test has to
reach is a plain function here, not a closure inside `main`.

```text
main.rs    argv, SIGINT/SIGTERM -> CancellationToken, the runtime `run` needs
lib.rs     bind, the runtime dir, the token mint and its removal on exit
gate.rs    loopback_only, token_ok (subtle), host_ok, sec_fetch_ok
serve.rs   accept loop, the gate order, the route match, shutdown
files.rs   the static rules, ported from eidolon/crates/web/src/files.rs
```

`lib.rs` is the crate's whole surface (`Options`, `run`) and `main.rs` does nothing but
parse argv, install the signal handlers and hand over. `files.rs` is a **port, not a
dependency**: the door's containment rules and their tests came across, because the perch
knows a door only as a binary and HTTP, and must not link an `eidolon-*` crate.

Hand-formatted, no `cargo fmt`: a line that reads better unbroken stays unbroken, and the
comments carry the *why* of a gate rather than its restatement.

**Unit tests live here, in `#[cfg(test)] mod tests`** — they need private items
(`gated_path`, `decode`, `owned_by`, `in_gated`, `set_owner_only`), which moving them out
would break. Anything that needs a socket or the real binary is in [`../tests/`](../tests/).

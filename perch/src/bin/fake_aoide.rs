//! `fake_aoide` — a stand-in for the `aoide` CLI, for the perch's tests. **A stub until slice R2a.**
//!
//! R2a fills this file: it logs its argv and `AOIDE_SESSION_ID` as JSON lines to `FAKE_AOIDE_LOG`, accepts only the argv shapes the perch's
//! runner builds (`--version`, `mesh --json`, `mail outbox --json`, `mail read ...`, `mail send ...`), replays scripted output from a fixture
//! directory, and exits 99 for anything else. Until then every argv is "anything else": the stub says so on stderr and exits 99, so a
//! test that reaches it early fails loudly instead of reading an empty answer as a quiet mesh.
//!
//! Built only with `--features test-bins` (`required-features` in Cargo.toml), so a normal `cargo build` ships no test double.

fn main() {
    eprintln!("fake_aoide: not built yet (slice R2a); refusing every argv");
    std::process::exit(99);
}

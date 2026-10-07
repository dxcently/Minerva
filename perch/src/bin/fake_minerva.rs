//! `fake_minerva` — a stand-in for the installer's `minerva` binary, for the perch's tests. **A stub until slice R4.**
//!
//! R4 fills this file: it logs argv and stdin, answers `status --json` (installed, not installed and a nix-managed mode) and the `keys`
//! verbs (`list`, `inspect -`, `add - --confirm <FP>`, `remove <id> --confirm <FP>`, each with `--json`) over a fixture keys dir, and
//! refuses anything else, so a route that ever ran `update` would fail a test. Until then it refuses every argv, loudly, with the exit
//! code a bad argument gets.
//!
//! Built only with `--features test-bins` (`required-features` in Cargo.toml), so a normal `cargo build` ships no test double.

fn main() {
    eprintln!("fake_minerva: not built yet (slice R4); refusing every argv");
    std::process::exit(2);
}

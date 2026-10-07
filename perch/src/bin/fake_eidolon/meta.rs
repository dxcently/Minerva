//! `eidolon [--config C] tools | plugins | policy-rows`: the read-only commands behind the loaded and diff tabs. **A stub until slice R3.**
//!
//! R3 fills [`run`]: each command prints its stock text on stdout, records `helper <argv> config=<bool>` to `$FAKE_LOG` the way the other
//! helpers do (so a test can count the perch's calls and see that `--live` was never passed), and exits 0. `fake_eidolon.rs` calls it for
//! these three subcommands and nothing else, after the global flags are parsed into a [`Cmd`]; the return value is the process's exit code.

use super::Cmd;

/// Answers `tools`, `plugins` and `policy-rows`. The stub refuses them, loudly, so a route that reaches the fake before R3 fails a test
/// instead of rendering an empty table.
pub fn run(cmd: &Cmd) -> i32 {
    eprintln!("fake_eidolon: `{}` is not built yet (slice R3)", cmd.name);
    2
}

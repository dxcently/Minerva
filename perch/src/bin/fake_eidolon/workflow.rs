//! `eidolon [--config C] workflow run|resume ...`: the stand-in for the workflow runner. **A stub until slice R1a.**
//!
//! R1a fills [`run`]: it replays the journals F0 captured (`tests/fixtures/jev/`) into the run's own session dir one step at a time
//! (`FAKE_WORKFLOW_STEP_MS` apart), parks and resumes on `--answer=<text>`, journals `WorkflowEnd` on SIGTERM, and prints the last line and
//! the exit code a real run does. `fake_eidolon.rs` calls it for the `workflow` subcommand and nothing else, after the global flags are
//! parsed into a [`Cmd`]; the return value is the process's exit code.

use super::Cmd;

/// Answers `workflow ...`. The stub refuses it, loudly: a perch that starts a run before R1a would otherwise see a door-less fake fail with
/// a message about the `web` subcommand.
pub fn run(cmd: &Cmd) -> i32 {
    eprintln!("fake_eidolon: `{}` is not built yet (slice R1a)", cmd.name);
    2
}

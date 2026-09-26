//! The benchmark loop for a defender agent on a scored practice image.
//!
//! `minerva bench` (this crate, standalone until the hub exists) resets a VM to
//! a frozen baseline, boots it, lets an agent work for a time box, reads the
//! scorer over the qemu console, and writes one report row. The loop
//! (`run::run_benchmark`) is generic over four seams so it runs against fakes in
//! tests and against real qemu + eidolon/JEV in the field:
//!
//!   VmControl    reset / boot / wait_ready / stop      (vm.rs → the harness)
//!   ScoreReader  read the scorer over the console      (vm.rs, score.rs parses)
//!   AgentRunner  drive eidolon + JEV + the brain       (agent.rs; Noop for now)
//!   Clock        elapsed time and the poll sleep       (clock.rs)
//!
//! Nothing here reaches the answer key or the writeup; the agent runs as a
//! competitor. See docs/design/triage-toolkit.md §7–§8.

pub mod agent;
pub mod clock;
pub mod config;
pub mod guard;
pub mod report;
pub mod run;
pub mod score;
pub mod vm;

pub use agent::{AgentRunner, NoopRunner};
pub use clock::{Clock, SystemClock};
pub use config::BenchConfig;
pub use report::Row;
pub use run::{run_benchmark, StopReason};
pub use score::Score;
pub use vm::{ScoreReader, Vm, VmControl};

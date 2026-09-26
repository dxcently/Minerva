//! The agent seam: how the loop drives the defender.
//!
//! The real runner starts eidolon + JEV pointed at the brain model, lets JEV's
//! triage graphs work the guest over ssh, and stops on demand. None of that is
//! wired here yet, so this crate ships `NoopRunner`: it drives nothing, which is
//! exactly the honest baseline — an agent that does nothing leaves the image at
//! its frozen 0/256. A real runner drops in behind the same trait.

use std::io;

/// What the loop tells the agent about the run it is inside.
#[derive(Debug, Clone)]
pub struct RunCtx {
    pub ssh_host: String,
    pub ssh_port: u16,
    pub brain_model: String,
    pub blind: bool,
}

/// Whether a step left more to do; the loop keeps polling the score regardless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    Working,
    Idle,
}

pub trait AgentRunner {
    fn start(&self, ctx: &RunCtx) -> io::Result<()>;
    fn step(&self, ctx: &RunCtx) -> io::Result<StepOutcome>;
    fn stop(&self) -> io::Result<()>;
    /// Tokens spent so far, if the runner tracks them (report §8).
    fn tokens(&self) -> u64 {
        0
    }
}

pub struct NoopRunner;

impl AgentRunner for NoopRunner {
    fn start(&self, _ctx: &RunCtx) -> io::Result<()> {
        Ok(())
    }
    fn step(&self, _ctx: &RunCtx) -> io::Result<StepOutcome> {
        Ok(StepOutcome::Idle)
    }
    fn stop(&self) -> io::Result<()> {
        Ok(())
    }
}

//! The guard rows (slice G1): the deny rows the perch adds to the policies it generates, and what it tells the page about them.
//! Stub until G1: nothing is guarded, and [`status`] says so.

use std::path::PathBuf;

use serde::Serialize;

/// Whether the guard rows are in the config a chat starts with, which file holds them, and why not when they are off. The loaded tab
/// shows it through `/hub/meta/policy`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct GuardStatus {
    pub on: bool,
    pub file: Option<PathBuf>,
    pub why: String,
}

/// The guard's status as of the last start.
pub fn status() -> GuardStatus {
    GuardStatus { on: false, file: None, why: "not built yet".to_string() }
}

/// The guard's sentence when `term_send` text names something an agent may not run (the same word check as the policy's), else `None`.
pub fn refuses_term_text(_text: &str) -> Option<&'static str> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stub_guards_nothing_and_says_so() {
        let status = status();
        assert!(!status.on);
        assert_eq!(status.file, None);
        assert_eq!(status.why, "not built yet");
        assert_eq!(refuses_term_text("aoide mail send x"), None);
    }
}

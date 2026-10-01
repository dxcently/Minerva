//! A terminal, editor or file manager opened over HTTP, as the TUI's `T`
//! chords open one.
//!
//! **The operator's act, not the model's**, so it does not go through the
//! [`eidolon_core::dispatch::Dispatcher`], for `crates/tui/src/launch.rs`'s
//! reason. The door is loopback-only, so the window opens on the browser's
//! machine. Only configured or scanned programs run, as argv, never a shell.
//!
//! [`Launch`] is a seam, as `crate::driver::Presence` is: `crates/cli` fills it
//! with `eidolon_tui::launch::Launcher`, so this crate does not depend on the TUI.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    Terminal,
    Editor,
    Files,
}

impl Window {
    pub(crate) fn parse(name: &str) -> Option<Window> {
        match name {
            "terminal" => Some(Window::Terminal),
            "editor" => Some(Window::Editor),
            "files" => Some(Window::Files),
            _ => None,
        }
    }
}

pub trait Launch: Send + Sync {
    /// The command line as spawned: the only way to tell a window that opened
    /// on another workspace from one that never opened. `path` means what it
    /// means to the TUI; `None` is the session's directory.
    fn open(&self, window: Window, path: Option<&str>) -> anyhow::Result<String>;
}

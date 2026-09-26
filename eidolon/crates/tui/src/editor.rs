//! **The draft, in your editor** — readline's `C-x C-e`, for the prompt.
//!
//! A draft that has outgrown the prompt is still the prompt's: this hands
//! the whole terminal to `$EDITOR` (or `[terminal] editor`, or the first
//! of `hx`, `nvim`, `vi`, … that exists — [`crate::launch::Launcher`]'s
//! resolution, the same one `T H` uses), on a file holding the draft, and
//! reads the file back when the editor leaves. One undo step takes the
//! buffer back to what it was, the way `u` does after a paste.
//!
//! ## In this terminal, not another window
//!
//! [`crate::launch`] opens windows, deliberately, and one of its reasons
//! is that a window cannot be read back from. A draft can be had no other
//! way: an editor in a second window writing a file this one is supposed
//! to pick up someday is not a round trip, it is a race with no finish
//! line. So the terminal is handed over instead — the modes the ui loop
//! pushed are popped, raw mode and the alternate screen are left, and the
//! editor runs in the foreground of the tty the harness was launched
//! from, exactly as a shell hands its foreground over.
//!
//! That is safe because of what the two-thread split already guarantees:
//! **nothing but the ui thread ever writes to the terminal.** A turn
//! streaming into the driver publishes to the bus, the bus lands in the
//! channel, and the channel is unbounded — so the transcript simply
//! buffers while the operator writes, and the first frame back drains it.
//! The terminal belongs to the editor alone for as long as it wants it.
//!
//! ## Why the ui loop does the standing down
//!
//! `edit_prompt` (the command) cannot do any of this: `exec` holds the
//! state and the keymap, and the terminal — the ratatui `Terminal` whose
//! diff buffer still believes last frame is on screen — is the ui loop's.
//! So the command only asks, the way `reload_ui` asks, and the loop calls
//! [`edit_prompt`] here with the terminal in hand. Coming back, the frame
//! is stale twice over (the alternate screen is blank and may be any
//! size), which is what `Terminal::clear` is for: the next draw repaints
//! everything rather than diffing against a fiction.
//!
//! The file is a per-process scratch name in the temp directory, mode
//! `0600` — a prompt is not for the other users of a machine — and is
//! removed as soon as it has been read.

use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use ratatui::crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::state::UiState;

/// Take the draft into the editor and back. Called on the ui thread, from
/// the ui loop, with the terminal that loop owns; blocks for as long as
/// the editor is open, and says why on the status line when any part of
/// the round trip did not happen.
pub fn edit_prompt(state: &mut UiState, terminal: &mut ratatui::DefaultTerminal) {
    let Some(argv) = editor_argv(state) else {
        return;
    };
    let path = draft_path();
    if let Err(e) = stage(&path, state.prompt.text()) {
        state.alert(format!("cannot stage the draft: {e:#}"));
        return;
    }
    if let Err(e) = stand_down() {
        state.alert(format!("cannot hand over the terminal: {e:#}"));
        let _ = std::fs::remove_file(&path);
        return;
    }
    let edited = run(&argv, &path);
    // Back, whether or not the editor ran: the terminal is already down,
    // and a harness that stayed in a shell because `hx` did not start
    // would be a poor way to fail.
    if let Err(e) = stand_up() {
        state.alert(format!("cannot take the terminal back: {e:#}"));
    }
    let _ = terminal.clear();
    let read = std::fs::read_to_string(&path);
    let _ = std::fs::remove_file(&path);
    match (edited, read) {
        // The editor said nothing was kept — say why, and leave the draft
        // as it stands rather than reading back whatever was left behind.
        (Err(e), _) => state.alert(format!("{e:#}")),
        (Ok(()), Err(e)) => state.alert(format!("cannot read the draft back: {e:#}")),
        (Ok(()), Ok(text)) => {
            let text = without_final_newline(text);
            // Unchanged is not a change: no undo step for nothing.
            if text != state.prompt.text() {
                state.prompt.set_text(&text);
            }
        }
    }
}

/// The editor as `argv`, or `None` and an alert naming where to say.
/// Interactive by nature — it is about to own this terminal — so the same
/// resolution `T H` uses is the right one: whatever the operator reaches
/// their other tools with.
fn editor_argv(state: &mut UiState) -> Option<Vec<String>> {
    let argv = state.launcher.editor();
    if argv.is_empty() {
        state.alert("no editor found; set `$EDITOR`, or `[terminal] editor` in config.toml");
        return None;
    }
    Some(argv)
}

/// The scratch file the draft round-trips through: one per process, so a
/// helix that remembers files can find it again, and gone the moment it
/// has been read.
fn draft_path() -> PathBuf {
    std::env::temp_dir().join(format!("eidolon-draft-{}.md", std::process::id()))
}

/// Write the draft, with one newline of our own on the end and the mode a
/// private file wears. The newline is *ours* — not the editor's and not
/// the draft's — which is what makes [`without_final_newline`] able to
/// take exactly it back off: the round trip is the identity, even for a
/// draft that itself ends in a newline, which arrives in the editor as
/// the blank last line it really is.
fn stage(path: &Path, text: &str) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(path)?;
    f.write_all(text.as_bytes())?;
    f.write_all(b"\n")
}

/// Run the editor on the file, in the foreground, inheriting the tty.
/// This is the one blocking call in the harness that is allowed to take
/// as long as the operator does.
fn run(argv: &[String], path: &Path) -> Result<()> {
    let Some((bin, args)) = argv.split_first() else {
        bail!("no editor to run");
    };
    let status = Command::new(bin)
        .args(args)
        .arg(path)
        .status()
        .with_context(|| format!("running {bin}"))?;
    if !status.success() {
        bail!("{bin} came back {status} — the draft stands as it was");
    }
    Ok(())
}

/// Hand the terminal over. The three input modes go first — the same
/// order the panic hook pops them in, for the same reason — then raw mode
/// and the alternate screen, which is what `ratatui::init` took and what
/// `ratatui::try_restore` would take back.
fn stand_down() -> Result<()> {
    execute!(
        std::io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        PopKeyboardEnhancementFlags
    )?;
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Take it back: raw mode and the alternate screen, then the three modes
/// the ui loop pushed at startup, in its own order. The screen is blank
/// and possibly the wrong size now; the caller clears, and the next draw
/// repaints the world.
fn stand_up() -> Result<()> {
    enable_raw_mode()?;
    execute!(
        std::io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    Ok(())
}

/// The one final newline comes off, and no more: an editor that saves
/// with a trailing newline (helix does) must not leave the draft looking
/// like it ends in a blank line, and a draft that *meant* one keeps it.
fn without_final_newline(text: String) -> String {
    let mut s = text;
    if s.ends_with('\n') {
        s.pop();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_editor_final_newline_comes_off_and_only_it() {
        assert_eq!(without_final_newline("hello\n".into()), "hello");
        assert_eq!(without_final_newline("hello".into()), "hello");
        assert_eq!(without_final_newline("two\nlines\n".into()), "two\nlines");
        // A blank line the draft meant to end on is content, not the
        // editor's habit, and survives.
        assert_eq!(without_final_newline("two\n\n".into()), "two\n");
        assert_eq!(without_final_newline(String::new()), "");
    }

    #[test]
    fn the_draft_round_trips_through_the_scratch_file() {
        let path =
            std::env::temp_dir().join(format!("eidolon-draft-test-{}.md", std::process::id()));
        let round = |text: &str| {
            stage(&path, text).unwrap();
            without_final_newline(std::fs::read_to_string(&path).unwrap())
        };
        // The identity, at every shape the draft comes in: empty, one
        // line, many, ending in its own newline.
        assert_eq!(round(""), "");
        assert_eq!(round("hello"), "hello");
        assert_eq!(round("a draft\ntwo lines"), "a draft\ntwo lines");
        assert_eq!(round("ends in one\n"), "ends in one\n");
        // What the editor is actually handed for that last one: the
        // draft's newline as a blank last line, and ours on the end.
        stage(&path, "ends in one\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "ends in one\n\n");
        std::fs::remove_file(&path).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn the_scratch_file_is_for_this_user_only() {
        use std::os::unix::fs::PermissionsExt;
        let path =
            std::env::temp_dir().join(format!("eidolon-draft-mode-{}.md", std::process::id()));
        stage(&path, "private").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(mode & 0o777, 0o600, "a prompt is not for the other users");
    }
}

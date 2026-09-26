//! The project instructions a session reads from its working directory:
//! `CLAUDE.md` or `AGENTS.md`, whichever [`ProjectInstructions`] asks for.
//!
//! These files are a convention every coding harness now shares — a
//! directory states how it wants to be worked on, and the harness puts
//! that in front of the model without being asked. Which file (or neither)
//! is a setting and not a command, because it is a property of the
//! *machine* rather than of a session: `[project] instructions` in
//! `config.toml`, read once at launch, held for the session's life — the
//! same place every other posture is decided.
//!
//! **Nothing here is cached and nothing here is journaled.** Like the
//! persona beside it, the file is re-read at the start of every turn and
//! the *setting*, not the body, is what a session carries — so an edit is
//! live on the next turn (these files are edited mid-conversation, often
//! by the model itself, which is the case a cached copy gets wrong in the
//! way that matters) and a log never holds a snapshot of instructions that
//! have since moved on. The cost of that liveness is one deliberate cache
//! miss per edit, paid on the prefix the block sits in, and no more.
//!
//! **Reading the file is not a tool call** — the same rule an attached
//! image follows. The name came from the config, which is the operator's
//! own voice, and not from the model; the day the model chooses the file
//! is the day it has to become a dispatched call.
//!
//! Nothing here reaches a driver backend: the Claude CLI composes its own
//! prompt and reads `CLAUDE.md` itself, so splicing the file again would
//! be the same instructions said twice. The setting is the harness's own
//! loop's.

use serde::Deserialize;
use std::path::Path;

/// The bound on a project instructions file, in characters.
///
/// A runaway's backstop and not a fit check: the file is the directory's
/// own instructions and is meant to be read whole, and truncating it would
/// leave rules half-obeyed — which is worse than a large prompt, because
/// it looks like obedience. But nothing bounds a file the model itself may
/// be appending its progress to, so the block says where it was cut rather
/// than cutting silently, and `:context` shows the size the whole time.
const MAX_CHARS: usize = 100_000;

/// Which project instructions file a session reads from its working
/// directory.
///
/// ```toml
/// [project]
/// instructions = "auto"     # auto | claude | agents | off
/// ```
///
/// `auto` takes `AGENTS.md` when the directory holds one and falls back to
/// `CLAUDE.md` — one file, never both, because two instructions blocks are
/// one prompt arguing with itself. `AGENTS.md` is the vendor-neutral name
/// the convention has settled on and the one a directory being migrated to
/// it carries; `CLAUDE.md` stays as the fallback so a directory that grew
/// up under the older name keeps its instructions. A directory that means
/// one file in particular names it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProjectInstructions {
    /// `AGENTS.md`, falling back to `CLAUDE.md`.
    #[default]
    Auto,
    /// `CLAUDE.md` only.
    Claude,
    /// `AGENTS.md` only.
    Agents,
    /// Read neither — the honest way out for a directory whose
    /// instructions are nobody's business but the operator's, or a machine
    /// where the prompt is spent on the work alone.
    Off,
}

impl ProjectInstructions {
    /// The filenames to try, in order of preference.
    fn candidates(&self) -> &'static [&'static str] {
        match self {
            ProjectInstructions::Auto => &["AGENTS.md", "CLAUDE.md"],
            ProjectInstructions::Claude => &["CLAUDE.md"],
            ProjectInstructions::Agents => &["AGENTS.md"],
            ProjectInstructions::Off => &[],
        }
    }
}

/// A project instructions file, resolved from the working directory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// The file's name — `CLAUDE.md` or `AGENTS.md` — which is all a
    /// report needs: the directory it was read from is the one the
    /// session is running in.
    pub file: String,
    /// The system-prompt block. Empty when nothing resolved.
    pub block: String,
}

impl Resolved {
    pub fn is_empty(&self) -> bool {
        self.block.is_empty()
    }
}

/// Resolve the setting against a working directory: the first candidate
/// file the directory holds, read whole.
///
/// **Forgiving, like the persona resolved beside it.** A directory holding
/// no candidate is the ordinary case and resolves to nothing. So does a
/// file that exists but will not read — the setting named a convention,
/// not a file the operator typed, and a permission problem must cost the
/// prompt and not the turn. It is said (to the log, not the screen) so a
/// directory that *means* to have instructions can be seen not to. An
/// empty file resolves to nothing rather than to an empty heading: an
/// instruction to instruct nothing is worse than silence.
pub fn resolve(setting: ProjectInstructions, cwd: &Path) -> Resolved {
    for name in setting.candidates() {
        let path = cwd.join(name);
        if !path.is_file() {
            continue;
        }
        return match std::fs::read_to_string(&path) {
            Ok(text) => Resolved {
                file: name.to_string(),
                block: block(name, &text),
            },
            Err(e) => {
                tracing::warn!("reading {} failed: {e}", path.display());
                Resolved::default()
            }
        };
    }
    Resolved::default()
}

/// The prompt block for one file: a heading, one line saying where the
/// text came from and that it is current, and the text.
fn block(name: &str, text: &str) -> String {
    let text = text.trim();
    if text.is_empty() {
        return String::new();
    }
    let body = if text.chars().count() > MAX_CHARS {
        let kept: String = text.chars().take(MAX_CHARS).collect();
        format!("{kept}\n[…truncated at {MAX_CHARS} characters]")
    } else {
        text.to_string()
    };
    format!(
        "## About this project\n\n\
         {name}, from the working directory, re-read each turn — this is its \
         current content:\n\n{body}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, text) in files {
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    #[test]
    fn auto_prefers_agents_and_falls_back_to_claude() {
        let both = dir_with(&[
            ("CLAUDE.md", "claude's rules\n"),
            ("AGENTS.md", "agents' rules\n"),
        ]);
        let r = resolve(ProjectInstructions::Auto, both.path());
        assert_eq!(r.file, "AGENTS.md");
        assert!(r.block.contains("agents' rules"));
        assert!(!r.block.contains("claude's rules"), "one file, never both");

        let one = dir_with(&[("CLAUDE.md", "claude's rules\n")]);
        let r = resolve(ProjectInstructions::Auto, one.path());
        assert_eq!(r.file, "CLAUDE.md");
        assert!(r.block.contains("claude's rules"));
    }

    #[test]
    fn a_directory_holding_neither_resolves_to_nothing() {
        let dir = dir_with(&[("README.md", "not instructions\n")]);
        for setting in [
            ProjectInstructions::Auto,
            ProjectInstructions::Claude,
            ProjectInstructions::Agents,
        ] {
            assert!(resolve(setting, dir.path()).is_empty(), "{setting:?}");
        }
    }

    #[test]
    fn naming_a_file_takes_that_file_alone() {
        let dir = dir_with(&[
            ("CLAUDE.md", "claude's rules\n"),
            ("AGENTS.md", "agents' rules\n"),
        ]);
        let r = resolve(ProjectInstructions::Agents, dir.path());
        assert_eq!(r.file, "AGENTS.md");
        assert!(r.block.contains("agents' rules"));

        let r = resolve(ProjectInstructions::Claude, dir.path());
        assert_eq!(r.file, "CLAUDE.md");
    }

    #[test]
    fn off_reads_nothing_even_when_the_files_are_there() {
        let dir = dir_with(&[("CLAUDE.md", "claude's rules\n")]);
        assert!(resolve(ProjectInstructions::Off, dir.path()).is_empty());
    }

    #[test]
    fn the_block_names_the_file_and_says_it_is_current() {
        let dir = dir_with(&[("AGENTS.md", "# House style\nBe terse.\n")]);
        let r = resolve(ProjectInstructions::Auto, dir.path());
        assert!(r.block.contains("AGENTS.md"), "{}", r.block);
        assert!(r.block.contains("re-read each turn"));
        assert!(r.block.contains("Be terse."));
    }

    #[test]
    fn an_empty_file_is_silence_rather_than_an_empty_heading() {
        let dir = dir_with(&[("CLAUDE.md", "   \n")]);
        assert!(resolve(ProjectInstructions::Auto, dir.path()).is_empty());
    }

    #[test]
    fn a_file_over_the_bound_is_cut_where_it_says_it_is() {
        let text = "x".repeat(MAX_CHARS + 10);
        let dir = dir_with(&[("CLAUDE.md", &text)]);
        let r = resolve(ProjectInstructions::Auto, dir.path());
        assert_eq!(
            r.block.chars().count(),
            block("CLAUDE.md", &text).chars().count()
        );
        assert!(r.block.contains("truncated at"));
        assert!(!r.block.contains(&"x".repeat(MAX_CHARS + 1)));
    }

    #[test]
    fn the_setting_reads_as_config_words() {
        // `config.toml` reaches this through toml's serde bridge, so the
        // words themselves are checked in `eidolon-cli`'s config test;
        // what is checked here is the Deserialize impl both share.
        let ok = |s: &str| serde_json::from_str::<ProjectInstructions>(s).unwrap();
        assert_eq!(ok("\"auto\""), ProjectInstructions::Auto);
        assert_eq!(ok("\"claude\""), ProjectInstructions::Claude);
        assert_eq!(ok("\"agents\""), ProjectInstructions::Agents);
        assert_eq!(ok("\"off\""), ProjectInstructions::Off);
        assert!(serde_json::from_str::<ProjectInstructions>("\"nope\"").is_err());
    }
}

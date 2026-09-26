//! **Another window**: a terminal, a file manager, an editor, another
//! harness — opened where this session is looking.
//!
//! The operator's helix and yazi keymaps already agree that `T T` is a
//! terminal here and `T R` is a file manager here, so a third tool in the
//! same rotation that did not answer those chords would be the one place
//! the hands had to stop and think. This is the harness's half of that
//! agreement, plus the half only the harness can offer: it knows what the
//! model just touched, so `T H` in trace mode opens the editor **on that
//! file** rather than on the directory.
//!
//! ## What is spawned, and what is not
//!
//! A new window, detached — never a program in *this* terminal. Both of
//! the other two tools spawn (`shell --orphan 'foot -D .'`,
//! `:run-shell-command foot -D .`), and suspending a harness whose turn is
//! still streaming to hand the terminal to an editor is a different and
//! much larger feature: the driver thread keeps running, the bus keeps
//! publishing, and the alternate screen has to come back to a transcript
//! that grew while it was gone. A second window has none of those
//! problems and is what the muscle memory already expects.
//!
//! Nothing here is a tool call and nothing here goes through
//! [`eidolon_core::dispatch::Dispatcher`], because none of it is the
//! model's: a program named in `config.toml` is launched because the
//! operator pressed a key, which is the same act as typing it at a shell.
//! The chokepoint exists to adjudicate what the *model* asks for, and the
//! model cannot reach the TUI's keymap or its `:` line.
//!
//! ## Why the resolution is lazy
//!
//! Working out which terminal emulator this machine has means stat-ing a
//! handful of names across `PATH`, and the launch budget is ten
//! milliseconds to a painted prompt (see the invariant in `AGENTS.md`).
//! So [`Launcher`] holds only what the config said, and the scan happens
//! on the first press of `T` — which is a key the operator has just
//! pressed, and where a hundred `stat` calls are invisible. A session
//! that never opens a window never pays for it at all.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

/// The terminal emulators worth guessing at, in the order they are
/// preferred, with the arguments that put a new window in a directory and
/// introduce a program to run in it.
///
/// Order is load-bearing twice over. It is the preference when several are
/// installed, and it is what keeps `$TERM` matching honest: kitty and
/// ghostty both announce themselves as `xterm-`something, so both have to
/// be tried before `xterm` is.
const EMULATORS: &[(&str, &[&str])] = &[
    ("foot", &["-D", "{dir}"]),
    ("ghostty", &["--working-directory={dir}", "-e"]),
    ("kitty", &["--directory", "{dir}"]),
    ("wezterm", &["start", "--cwd", "{dir}", "--"]),
    ("alacritty", &["--working-directory", "{dir}", "-e"]),
    ("st", &["-e"]),
    ("xterm", &["-e"]),
];

/// The emulators whose `$TERM` distinctively names them, and the only
/// ones the `$TERM` guess is allowed to match.
///
/// `st` and `xterm` are left out on purpose. `xterm-256color` is what
/// half the terminals in the world claim to be — this session is reading
/// it from a foot window right now — so matching it to the literal
/// `xterm` binary would answer "which terminal am I in?" with the one the
/// operator is demonstrably not looking at. Both stay in [`EMULATORS`],
/// because as a *last* resort an xterm that exists is better than no
/// window at all.
const TERM_SAYS: &[&str] = &["foot", "ghostty", "kitty", "wezterm", "alacritty"];

/// Editors to look for when neither the config nor `$VISUAL`/`$EDITOR`
/// says. Helix first, because this harness is written beside one.
const EDITORS: &[&str] = &["hx", "helix", "nvim", "vim", "vi", "nano"];

/// File managers, likewise. **Not `ya`**: on a machine with yazi, `ya` is
/// its command-line helper (`ya emit`, `ya pack`) and would open nothing
/// — while in the operator's own nushell `ya` is a *function* wrapping
/// `yazi --cwd-file`, which is a shell's business and not something the
/// harness can guess at. Reproduce that with
/// `files = ["nu", "-e", "ya \"{path}\""]` — see [`with_path`].
const MANAGERS: &[&str] = &["yazi", "lf", "ranger", "nnn", "broot"];

/// What `[terminal]` in `config.toml` said, if anything.
///
/// Every field is an argv rather than a string, because a command line is
/// not a string: `command = "foot -D {dir}"` would need a shell to take
/// apart, and a shell is one more thing between a keystroke and a window
/// that can go wrong with a space in a path. An empty vector means the
/// operator did not say, and [`Launcher`] works it out.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launcher {
    /// The emulator and its arguments. `{dir}` is replaced with the
    /// directory the window should open in; the program to run, when
    /// there is one, is appended after these.
    pub terminal: Vec<String>,
    /// The editor, and the file manager. Both are handed a path — see
    /// [`with_path`] for where it lands.
    pub editor: Vec<String>,
    pub files: Vec<String>,
}

impl Launcher {
    /// Open `program` (empty: whatever the terminal runs on its own,
    /// which is the login shell) in a new window at `dir`.
    ///
    /// Answers with the command line as spawned, which is what the
    /// transcript says — a window that opened somewhere off-screen, or
    /// on the wrong workspace, is indistinguishable from one that never
    /// opened, and the operator should be able to read what was tried.
    pub fn open(&self, dir: &Path, program: &[String]) -> Result<String> {
        // A terminal emulator with no display server to draw on starts,
        // fails, and dies without a word — the same trap the clipboard
        // helpers set, and the same guard. Saying so is the useful
        // answer for a session over ssh into something plain.
        if std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("DISPLAY").is_none() {
            bail!("no display server: there is nowhere to open a window");
        }
        let term = self.terminal();
        let Some((bin, args)) = term.split_first() else {
            bail!("no terminal emulator found; set `[terminal] command` in config.toml");
        };
        let mut argv: Vec<String> = args
            .iter()
            .map(|a| a.replace("{dir}", &dir.to_string_lossy()))
            .collect();
        // A trailing exec-introducer is the emulator's "and now the
        // program": `alacritty -e` with nothing after it is an error, not
        // a bare shell. One rule, applied to a detected argv and a
        // configured one alike, so an operator writing `["alacritty",
        // "--working-directory", "{dir}", "-e"]` gets both behaviours
        // from the one line.
        if program.is_empty() {
            if matches!(argv.last().map(String::as_str), Some("-e" | "--")) {
                argv.pop();
            }
        } else {
            argv.extend(program.iter().cloned());
        }
        // `current_dir` as well as the `{dir}` argument: `st` and `xterm`
        // have no flag for it and inherit ours, and an emulator that does
        // have one is being told the same thing twice.
        let mut child = Command::new(bin)
            .args(&argv)
            .current_dir(dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning {bin}"))?;
        // Reaped off the UI thread, as the clipboard helpers are: the
        // window outlives the keystroke by design, and waiting on it here
        // would freeze the harness for as long as the operator kept it
        // open.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(std::iter::once(bin.clone())
            .chain(argv)
            .collect::<Vec<_>>()
            .join(" "))
    }

    /// The terminal emulator, as configured or as guessed.
    ///
    /// The guess reads the environment and never asks the terminal, for
    /// the reason `[images] inline = "auto"` does not: the honest way to
    /// ask is a round-trip on something that may not answer.
    pub fn terminal(&self) -> Vec<String> {
        if !self.terminal.is_empty() {
            return self.terminal.clone();
        }
        // `$TERMINAL` is the convention a user who has already had this
        // argument with another program will have set.
        if let Some(t) = env_argv("TERMINAL") {
            let stem = Path::new(&t[0])
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let args = EMULATORS
                .iter()
                .find(|(bin, _)| *bin == stem)
                .map_or(&[][..], |(_, a)| *a);
            return t
                .into_iter()
                .chain(args.iter().map(|s| (*s).to_string()))
                .collect();
        }
        // `$TERM` names the emulator we are *running inside*, which is
        // the one the operator most likely wants another window of:
        // `foot`, `foot-extra`, `xterm-kitty`, `xterm-ghostty`. Only the
        // names that mean it — see [`TERM_SAYS`].
        if let Ok(term) = std::env::var("TERM")
            && let Some(e) = EMULATORS
                .iter()
                .find(|(bin, _)| TERM_SAYS.contains(bin) && term.contains(bin) && on_path(bin))
        {
            return argv(e);
        }
        EMULATORS
            .iter()
            .find(|(bin, _)| on_path(bin))
            .map(argv)
            .unwrap_or_default()
    }

    /// The editor: what was configured, else `$VISUAL`, else `$EDITOR`,
    /// else the first of [`EDITORS`] on `PATH`.
    pub fn editor(&self) -> Vec<String> {
        pick(&self.editor, &["VISUAL", "EDITOR"], EDITORS)
    }

    /// The file manager. No environment variable is consulted, because
    /// there is no convention for one worth guessing at.
    pub fn files(&self) -> Vec<String> {
        pick(&self.files, &[], MANAGERS)
    }
}

/// Configured, else the first environment variable that is set, else the
/// first candidate on `PATH`.
fn pick(configured: &[String], vars: &[&str], candidates: &[&str]) -> Vec<String> {
    if !configured.is_empty() {
        return configured.to_vec();
    }
    for v in vars {
        if let Some(a) = env_argv(v) {
            return a;
        }
    }
    candidates
        .iter()
        .find(|b| on_path(b))
        .map(|b| vec![(*b).to_string()])
        .unwrap_or_default()
}

/// An environment variable as an argv. `EDITOR="hx -w"` is a command line
/// and not a filename, and splitting on whitespace is the whole of what
/// the convention supports — a path with a space in it has never worked
/// there and is not going to start here.
fn env_argv(name: &str) -> Option<Vec<String>> {
    let v = std::env::var(name).ok()?;
    let parts: Vec<String> = v.split_whitespace().map(str::to_string).collect();
    (!parts.is_empty()).then_some(parts)
}

fn argv((bin, args): &(&str, &[&str])) -> Vec<String> {
    std::iter::once((*bin).to_string())
        .chain(args.iter().map(|s| (*s).to_string()))
        .collect()
}

/// Is this program runnable? A name is looked up on `PATH`; anything with
/// a separator in it is a path already and is asked directly.
fn on_path(bin: &str) -> bool {
    if bin.contains('/') {
        return Path::new(bin).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// Hand `path` to a program that opens things.
///
/// Appended, which is what `["hx"]` and `["yazi"]` want — but appending
/// is wrong the moment the program is reached through a shell. The
/// operator's `T R` is `["nu", "-e", "ya"]`, where `ya` is a nushell
/// function; a fourth argv element there is not an argument to `ya` but a
/// *script file* for nushell to run, so the window opens on the wrong
/// thing and says nothing about it. `{path}` is how such a command says
/// where the path belongs — `["nu", "-e", "ya \"{path}\""]` — and an argv
/// that mentions it is never appended to.
pub fn with_path(argv: Vec<String>, path: &str) -> Vec<String> {
    if argv.iter().any(|a| a.contains("{path}")) {
        return argv
            .into_iter()
            .map(|a| a.replace("{path}", path))
            .collect();
    }
    argv.into_iter()
        .chain(std::iter::once(path.to_string()))
        .collect()
}

/// The path a tool call's input names, if it names one.
///
/// The three keys are what the built-in tools and the common conventions
/// use — `path` for `fs::{read,write,edit}` and `search::grep`, the other
/// two for tools written against other harnesses' habits. Anything else
/// is a tool the harness cannot know the shape of, and the answer is
/// honestly nothing rather than a guess at the first string in the
/// object.
pub fn subject(input: &str) -> Option<PathBuf> {
    let v: serde_json::Value = serde_json::from_str(input).ok()?;
    ["path", "file_path", "file"]
        .iter()
        .find_map(|k| v.get(k).and_then(serde_json::Value::as_str))
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Where a window aimed at `path` should open, and what to hand the
/// program: the file itself for an editor or a file manager, and its
/// directory for the window.
///
/// A path that is a directory is both answers at once, and a path whose
/// parent does not exist falls back to `cwd` — a call that wrote to
/// `out/report.md` before `out/` existed should not open a window
/// nowhere.
pub fn aim(cwd: &Path, path: Option<&Path>) -> (PathBuf, Option<PathBuf>) {
    let Some(p) = path else {
        return (cwd.to_path_buf(), None);
    };
    let full = if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    };
    if full.is_dir() {
        return (full, None);
    }
    let dir = full
        .parent()
        .filter(|d| d.is_dir())
        .map_or_else(|| cwd.to_path_buf(), Path::to_path_buf);
    (dir, Some(full))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_configured_terminal_is_taken_whole_and_gets_the_directory() {
        let l = Launcher {
            terminal: ["alacritty", "--working-directory", "{dir}", "-e"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
            ..Launcher::default()
        };
        assert_eq!(
            l.terminal()[2],
            "{dir}",
            "substitution happens at spawn, not at resolve"
        );
    }

    /// The `$TERM` guess only answers for the names that mean it, and
    /// the two that share a prefix with something generic do not stop it
    /// reaching the right one.
    /// A plain program is handed the path; one reached through a shell
    /// says where the path goes and is not appended to.
    #[test]
    fn a_program_reached_through_a_shell_says_where_the_path_belongs() {
        let argv = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            with_path(argv(&["hx"]), "/tmp/x.rs"),
            argv(&["hx", "/tmp/x.rs"])
        );
        // The operator's own `T R`. Appending would have handed nushell a
        // script to run instead of yazi a directory to open.
        assert_eq!(
            with_path(argv(&["nu", "-e", "ya \"{path}\""]), "/tmp/a dir"),
            argv(&["nu", "-e", "ya \"/tmp/a dir\""]),
            "the quotes are the shell's, and the path goes inside them"
        );
        // Named in the middle, for a program that takes the path before
        // its own flags.
        assert_eq!(
            with_path(argv(&["hx", "{path}", "--vsplit"]), "/tmp/x"),
            argv(&["hx", "/tmp/x", "--vsplit"])
        );
    }

    #[test]
    fn a_generic_term_is_not_evidence_of_which_terminal_this_is() {
        let says = |term: &str| {
            EMULATORS
                .iter()
                .find(|(bin, _)| TERM_SAYS.contains(bin) && term.contains(bin))
                .map(|(b, _)| *b)
        };
        assert_eq!(says("foot-extra"), Some("foot"));
        assert_eq!(
            says("xterm-kitty"),
            Some("kitty"),
            "and not xterm, which is not in the list at all"
        );
        assert_eq!(says("xterm-ghostty"), Some("ghostty"));
        // The two that would otherwise swallow everything.
        assert_eq!(says("xterm-256color"), None);
        assert_eq!(says("st-256color"), None);
        // Inside tmux nothing is claimed, and the `PATH` scan decides.
        assert_eq!(says("tmux-256color"), None);
        assert_eq!(says("screen"), None);
    }

    #[test]
    fn the_path_comes_out_of_a_tool_call_under_any_of_its_names() {
        assert_eq!(
            subject(r#"{"path":"src/main.rs"}"#),
            Some(PathBuf::from("src/main.rs"))
        );
        assert_eq!(
            subject(r#"{"file_path":"/tmp/x"}"#),
            Some(PathBuf::from("/tmp/x"))
        );
        // `path` wins when a tool somehow carries both.
        assert_eq!(
            subject(r#"{"file":"b","path":"a"}"#),
            Some(PathBuf::from("a"))
        );
        // A shell command names no file, and neither does a blank one.
        assert_eq!(subject(r#"{"command":"ls -la"}"#), None);
        assert_eq!(subject(r#"{"path":""}"#), None);
        assert_eq!(subject("not json at all"), None);
    }

    #[test]
    fn a_window_opens_in_the_directory_and_the_program_gets_the_file() {
        let cwd = std::env::current_dir().unwrap();
        // Nothing under the cursor: the session's own directory, and no
        // argument for the program.
        assert_eq!(aim(&cwd, None), (cwd.clone(), None));
        // A directory is where to open and nothing to open *on*.
        let (dir, file) = aim(&cwd, Some(Path::new("src")));
        assert_eq!((dir, file), (cwd.join("src"), None));
        // A relative file resolves against the session's directory, and
        // the window opens beside it.
        let (dir, file) = aim(&cwd, Some(Path::new("src/lib.rs")));
        assert_eq!(dir, cwd.join("src"));
        assert_eq!(file, Some(cwd.join("src/lib.rs")));
        // A file in a directory that was never created still opens
        // somewhere real.
        let (dir, _) = aim(&cwd, Some(Path::new("no/such/place/report.md")));
        assert_eq!(dir, cwd);
    }

    #[test]
    fn an_editor_from_the_environment_may_carry_arguments() {
        // SAFETY: single-threaded test, and the variable is read back
        // immediately below.
        unsafe { std::env::set_var("EIDOLON_TEST_EDITOR", "hx -w") };
        assert_eq!(
            env_argv("EIDOLON_TEST_EDITOR"),
            Some(vec!["hx".into(), "-w".into()])
        );
        unsafe { std::env::set_var("EIDOLON_TEST_EDITOR", "   ") };
        assert_eq!(
            env_argv("EIDOLON_TEST_EDITOR"),
            None,
            "whitespace is not a command"
        );
        unsafe { std::env::remove_var("EIDOLON_TEST_EDITOR") };
    }
}

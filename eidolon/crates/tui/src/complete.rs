//! What can be typed next on the `:` line.
//!
//! The command line is the prompt with a colon in it, and until now that
//! was true of its completion too: `tab` matched the *whole* line against
//! the command names, so `:model son` matched nothing, `:attach ` offered
//! nothing, and completing `:att` wrote `:attach` with the cursor jammed
//! against a name that cannot be run alone. A command line that stops
//! helping at the space is a command line that helps with the easy half.
//!
//! So the line is read in two spots — the **name**, and the **argument**
//! — and both are completed from the same table. Which spot the cursor is
//! in is a question about the text (is there a space yet?), asked here and
//! answered once, the way [`crate::state::UiState::command_line`] answers
//! "am I on a command line at all?".
//!
//! ## One list
//!
//! A [`Suggestion`] carries **the whole line body it would write**, not
//! just the word: rewriting `:exclude 12,` to `:exclude 12,14` and
//! rewriting `:mod` to `:model ` are then the same operation, and `tab`
//! needs to know nothing about which spot it is in. The popup and the
//! cycle read one list, so what is drawn and what `tab` writes cannot
//! come apart — the failure the command registry itself exists to
//! prevent, one level down.
//!
//! A row with an empty [`Suggestion::insert`] is a row that is only
//! *saying what goes here*: the usage hint for `:note`, `:launch`,
//! `:dispatch`, where the argument is prose and there is nothing to
//! offer. Such a row is produced **only when there is nothing to
//! insert**, which is what keeps the popup's index and the cycle's index
//! the same number.
//!
//! ## What fills what
//!
//! [`crate::command::Fill`] on the command's row, and everything it names
//! is answerable from `UiState` — see the note there. The two that are
//! not free are the filesystem and the sessions directory, and both are
//! plain `read_dir`s capped at [`CAP`]: a listing per keystroke is what a
//! shell does, and the expensive reading of session *logs* stays where it
//! was, in the picker.

use crate::command::{self, Fill};
use crate::state::UiState;

/// How many candidates one source may offer. The popup shows twelve and
/// `tab` walks the rest; past this the list has stopped being an answer
/// and the operator wants the picker.
const CAP: usize = 200;

/// One row of the completion popup, and one stop on the `tab` cycle.
pub struct Suggestion {
    /// The whole `:` line body this row would write — the name, whatever
    /// of the argument is already settled, and the candidate in place of
    /// the token being typed. Empty when the row is only saying what goes
    /// here; see the module note for why that is never mixed in with real
    /// candidates.
    pub insert: String,
    /// The row's left column.
    pub label: String,
    /// The row's right column.
    pub help: String,
}

impl Suggestion {
    fn new(insert: String, label: impl Into<String>, help: impl Into<String>) -> Suggestion {
        Suggestion {
            insert,
            label: label.into(),
            help: help.into(),
        }
    }
}

/// The popup over a half-typed command line.
pub struct Completion {
    /// What the popup calls itself: what has been typed while the name is
    /// still being typed, and the command's *usage line* once it is
    /// settled — so that the thing the operator now owes is on screen in
    /// the same words `:help` uses.
    pub title: String,
    pub rows: Vec<Suggestion>,
}

/// The line, split at the first whitespace: the name, and the rest with
/// its leading space kept. `None` while there is no space yet, which is
/// the name spot.
fn split(line: &str) -> Option<(&str, &str)> {
    line.find(char::is_whitespace)
        .map(|i| (&line[..i], &line[i..]))
}

/// Where the value being typed begins in `rest` — the argument with its
/// leading whitespace still on the front.
///
/// **An argument runs to the end of the line.** It is not cut at spaces,
/// which is what this used to do: `fau:Ministral 3 14B` is a real model
/// key and `my shot.png` is a real filename, and completing the last
/// word of either matched nothing and then wrote the value in a second
/// time on top of the first. The space after the *name* is a separator;
/// no space after it is.
///
/// The one separator inside an argument is the comma, for the two fills
/// that are lists of record ids — `:exclude 12,14` should complete the
/// third without rewriting the first two.
fn value(rest: &str, list: bool) -> usize {
    let start = rest.len() - rest.trim_start().len();
    match list {
        true => rest.rfind(',').map_or(start, |i| i + 1),
        false => start,
    }
}

/// What could come next on `line` — the `:` line's body, colon stripped.
///
/// `None` when nothing about the line resolves: a name that matches no
/// command, or an argument to a command that does not exist. A popup
/// that offered rows for a name the harness has never heard of would be
/// promising something `ret` will refuse.
pub fn complete(state: &UiState, line: &str) -> Option<Completion> {
    let Some((name, rest)) = split(line) else {
        // Still in the name. This is the search the `:` line has always
        // had, with one addition: a command that takes an argument
        // completes to `name ` with the space already there, so the next
        // `tab` is the argument's rather than a retype of the space.
        let found = command::matches_in(line, &state.script_commands);
        if found.is_empty() {
            return None;
        }
        // The group leads the description here, as it did on the command
        // palette this replaced: browsing the vocabulary is now something
        // the `:` line does, and `motion` or `launch` in front of the
        // sentence is what tells you which part of the harness a name
        // you have never seen belongs to.
        let rows = found
            .iter()
            .map(|e| {
                let insert = if e.takes_arg() {
                    format!("{} ", e.name())
                } else {
                    e.name().to_string()
                };
                Suggestion::new(
                    insert,
                    e.usage().trim_start_matches(':'),
                    format!("{} · {}", e.group(), e.help()),
                )
            })
            .collect();
        return Some(Completion {
            title: format!(":{line}"),
            rows,
        });
    };
    let entry = command::lookup_in(name, &state.script_commands)?;
    let fill = entry.fill();
    let (prefix, stem) = rest.split_at(value(rest, fill.list()));
    let title = entry.usage();
    let rows: Vec<Suggestion> = candidates(state, fill, stem)
        .into_iter()
        .map(|(value, help)| Suggestion::new(format!("{name}{prefix}{value}"), value, help))
        .collect();
    if !rows.is_empty() {
        return Some(Completion { title, rows });
    }
    // Nothing to offer, so say what goes here instead. This is the whole
    // popup for `:note`, `:launch` and `:dispatch`, whose argument is
    // prose — and it is also what an argument with no matches looks like,
    // which is the honest answer: the shape is still what it was.
    let label = entry.hint().unwrap_or("no argument").to_string();
    Some(Completion {
        title,
        rows: vec![Suggestion::new(String::new(), label, entry.help())],
    })
}

/// The values for one [`Fill`], filtered by what has been typed of one.
fn candidates(state: &UiState, fill: Fill, stem: &str) -> Vec<(String, String)> {
    match fill {
        Fill::Free => Vec::new(),
        Fill::Path => paths(&state.cwd, stem, false),
        Fill::Dir => paths(&state.cwd, stem, true),
        // Every mode `enter_mode` can reach: the ones the prompt can be
        // in, and the one-line modes, which it opens rather than enters.
        Fill::Mode => narrow(
            stem,
            state
                .modes
                .rows()
                .filter(|d| d.kind.is_some() || crate::state::MiniKind::parse(&d.name).is_some())
                .map(|d| (d.name.clone(), d.label.clone()))
                .collect(),
        ),
        Fill::Model => narrow(stem, state.model_keys.clone()),
        Fill::Provider => narrow(
            stem,
            state
                .provider_keys
                .iter()
                .map(|p| (p.clone(), String::new()))
                .collect(),
        ),
        Fill::Session => sessions(state, stem),
        Fill::Attachment => narrow(
            stem,
            state
                .attachments
                .iter()
                .map(|a| (a.name.clone(), a.label()))
                .collect(),
        ),
        Fill::Record => records(state, stem, false),
        Fill::Struck => records(state, stem, true),
        Fill::Pin => narrow(
            stem,
            state
                .pins
                .iter()
                .map(|p| (p.clone(), String::new()))
                .collect(),
        ),
        Fill::Persona => narrow(stem, state.persona_keys.clone()),
        Fill::Surface => narrow(
            stem,
            state
                .dispatch
                .surfaces()
                .iter()
                .map(|s| (s.clone(), String::new()))
                .collect(),
        ),
        Fill::Words(w) => narrow(
            stem,
            w.iter()
                .map(|s| ((*s).to_string(), String::new()))
                .collect(),
        ),
    }
}

/// Filter a list of candidates by what has been typed, best first: the
/// ones it prefixes, shortest first, then the ones that merely contain
/// it. The same ranking [`command::matches_in`] uses on the names, for
/// the same reason — the least you could have meant by what you typed.
fn narrow(stem: &str, items: Vec<(String, String)>) -> Vec<(String, String)> {
    let q = stem.to_lowercase();
    let (mut head, mut tail): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
    for (value, help) in items {
        let v = value.to_lowercase();
        if v.starts_with(&q) {
            head.push((value, help));
        } else if v.contains(&q) || help.to_lowercase().contains(&q) {
            tail.push((value, help));
        }
    }
    if !q.is_empty() {
        head.sort_by_key(|(v, _)| v.chars().count());
    }
    head.extend(tail);
    head.truncate(CAP);
    head
}

/// The filesystem, under whatever directory the stem names.
///
/// The written form of that directory is kept rather than resolved — a
/// stem of `~/Doc` completes to `~/Documents/` and not to the home
/// directory spelled out — because the operator is editing a line of
/// text and a completion that rewrites what they already typed reads as
/// the wrong candidate having been chosen.
fn paths(cwd: &str, stem: &str, dirs_only: bool) -> Vec<(String, String)> {
    let (written, typed) = match stem.rfind('/') {
        Some(i) => (&stem[..=i], &stem[i + 1..]),
        None => ("", stem),
    };
    let base = if written.is_empty() {
        std::path::PathBuf::from(cwd)
    } else {
        let expanded = eidolon_tools::image::expand_tilde(written);
        if expanded.is_absolute() {
            expanded
        } else {
            std::path::Path::new(cwd).join(expanded)
        }
    };
    let typed_lower = typed.to_lowercase();
    let mut out: Vec<(String, bool)> = Vec::new();
    for e in std::fs::read_dir(&base).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // A dotfile is offered once the dot is typed and not before,
        // which is every shell's rule and what keeps `:attach ` in a
        // repository from opening on `.git`.
        if name.starts_with('.') && !typed.starts_with('.') {
            continue;
        }
        if !name.to_lowercase().starts_with(&typed_lower) {
            continue;
        }
        let dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if dirs_only && !dir {
            continue;
        }
        out.push((name, dir));
    }
    // Directories first — a path is walked down before it is finished —
    // and then by name, case-insensitively, so the list reads the way
    // `ls` does rather than in whatever order the filesystem hands them
    // over.
    out.sort_by(|(a, ad), (b, bd)| {
        bd.cmp(ad)
            .then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    out.truncate(CAP);
    out.into_iter()
        .map(|(name, dir)| {
            // A directory completes with its slash, so the next `tab`
            // reads it rather than offering the same name again.
            (
                format!("{written}{name}{}", if dir { "/" } else { "" }),
                if dir {
                    "directory".to_string()
                } else {
                    String::new()
                },
            )
        })
        .collect()
}

/// The session logs, newest first, by filename alone.
///
/// Deliberately without opening any of them: a summary costs a full
/// replay of the log ([`crate::sessions`]), and this list is rebuilt on
/// every keystroke. What the names cannot say — what a session was
/// about — is what the picker is for, and a bare `:resume` opens it.
fn sessions(state: &UiState, stem: &str) -> Vec<(String, String)> {
    let mut files: Vec<(std::path::PathBuf, std::time::SystemTime)> =
        std::fs::read_dir(&state.sessions_dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "eid"))
            .map(|p| {
                let t = p
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                (p, t)
            })
            .collect();
    files.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
    let q = stem.to_lowercase();
    files
        .into_iter()
        .map(|(p, t)| (p.display().to_string(), t))
        .filter(|(path, _)| {
            let p = path.to_lowercase();
            p.starts_with(&q) || p.rsplit('/').next().is_some_and(|n| n.contains(&q))
        })
        .map(|(path, t)| {
            let here = path == state.session_path;
            (
                path,
                if here {
                    "this session".to_string()
                } else {
                    crate::sessions::ago(t)
                },
            )
        })
        .take(CAP)
        .collect()
}

/// Record ids on the branch, newest first: the ones still in the context
/// for `:exclude`, and the struck ones for `:restore`.
///
/// Each carries the first line of what it says, because an id is a
/// number and a number is not something an operator recognises. The
/// transcript is where that line comes from — `records` maps an entry to
/// the record it was drawn from, which is the same map `:exclude` uses
/// to strike one.
fn records(state: &UiState, stem: &str, struck: bool) -> Vec<(String, String)> {
    let mut rows: Vec<(u64, String)> = state
        .records
        .iter()
        .filter(|(_, id)| state.struck.contains(id) == struck)
        .map(|(&entry, &id)| {
            let text = state
                .transcript
                .get(entry)
                .map(|e| e.text())
                .unwrap_or_default();
            (
                id,
                text.lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .chars()
                    .take(60)
                    .collect(),
            )
        })
        .collect();
    rows.sort_by_key(|(id, _)| std::cmp::Reverse(*id));
    rows.dedup_by_key(|(id, _)| *id);
    rows.into_iter()
        .map(|(id, line)| (id.to_string(), line))
        .filter(|(id, _)| id.starts_with(stem))
        .take(CAP)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> UiState {
        let mut st = UiState::new("m".into(), "/tmp/s.eid".into(), "/tmp".into());
        st.modes = crate::modes::ModeTable::default();
        st
    }

    #[test]
    fn a_name_completes_with_the_space_its_argument_needs() {
        let st = state();
        let c = complete(&st, "attac").expect("attach is a command");
        assert_eq!(
            c.rows[0].insert, "attach ",
            "a required argument leaves the space behind it"
        );
        assert_eq!(c.rows[0].label, "attach PATH");
        let c = complete(&st, "compac").expect("compact is a command");
        assert_eq!(
            c.rows[0].insert, "compact",
            "and one that takes nothing does not"
        );
        assert!(complete(&st, "zzz").is_none());
    }

    #[test]
    fn the_argument_is_completed_from_the_rows_own_source() {
        let mut st = state();
        st.model_keys = vec![
            ("fau:openai/gpt-5".into(), "GPT-5".into()),
            ("deepseek:deepseek-v4-pro".into(), "DeepSeek".into()),
        ];
        // Past the space, the name is settled and the popup is the
        // argument's — titled with the usage, so the shape is on screen.
        let c = complete(&st, "model deep").expect("model takes a model");
        assert_eq!(c.title, ":model [KEY]");
        assert_eq!(c.rows.len(), 1);
        assert_eq!(
            c.rows[0].insert, "model deepseek:deepseek-v4-pro",
            "the whole line, not the word"
        );
        // An empty stem is every value.
        assert_eq!(complete(&st, "model ").unwrap().rows.len(), 2);
    }

    #[test]
    fn a_list_argument_completes_its_last_element() {
        let mut st = state();
        st.records = [(0usize, 12u64), (1, 14)].into_iter().collect();
        st.transcript = vec![
            crate::state::Entry::User("first".into()),
            crate::state::Entry::User("second".into()),
        ];
        let c = complete(&st, "exclude 12,1").expect("exclude takes ids");
        let inserts: Vec<&str> = c.rows.iter().map(|r| r.insert.as_str()).collect();
        assert_eq!(
            inserts,
            ["exclude 12,14", "exclude 12,12"],
            "the comma is a separator, so 12 is kept"
        );
        assert_eq!(
            c.rows[0].help, "second",
            "an id is a number; the line beside it is what it is recognised by"
        );
        // Struck records are `restore`'s list and not `exclude`'s.
        st.struck.insert(14);
        assert_eq!(complete(&st, "exclude ").unwrap().rows.len(), 1);
        assert_eq!(
            complete(&st, "restore ").unwrap().rows[0].insert,
            "restore 14"
        );
    }

    /// A value with a space in it is one value. `fau:Ministral 3 14B` is
    /// a real model key and `my shot.png` is a real filename, and
    /// completing only the last word of either offered nothing and then
    /// wrote the value in a second time on top of the first.
    #[test]
    fn a_value_with_a_space_in_it_is_still_one_value() {
        let mut st = state();
        st.model_keys = vec![(
            "fau:Ministral 3 14B".into(),
            "Ministral 3 14B (FAU free)".into(),
        )];
        assert_eq!(
            complete(&st, "model Minis").unwrap().rows[0].insert,
            "model fau:Ministral 3 14B"
        );
        // Reached by its own prefix, and completing a settled value is
        // the same value rather than the value written twice.
        let c = complete(&st, "model fau:Ministral 3").unwrap();
        assert_eq!(c.rows[0].insert, "model fau:Ministral 3 14B");
        assert_eq!(
            complete(&st, "model fau:Ministral 3 14B").unwrap().rows[0].insert,
            "model fau:Ministral 3 14B"
        );
        // The leading space is the separator and is kept, however much
        // of it there is; nothing inside the value is.
        assert_eq!(
            complete(&st, "model   Min").unwrap().rows[0].insert,
            "model   fau:Ministral 3 14B"
        );
    }

    #[test]
    fn prose_gets_the_shape_rather_than_a_guess() {
        let st = state();
        // Nothing fills a note, so the row says what goes there and
        // inserts nothing — the one case where a row is not a candidate,
        // and the reason `tab` refuses an empty insert.
        let c = complete(&st, "note anything at all").expect("note is a command");
        assert_eq!(c.title, ":note [TEXT]");
        assert_eq!(c.rows.len(), 1);
        assert!(c.rows[0].insert.is_empty());
        assert_eq!(c.rows[0].label, "[TEXT]");
        // Same shape when a real source simply has no match.
        let c = complete(&st, "model nothing-like-this").unwrap();
        assert!(c.rows[0].insert.is_empty());
    }

    #[test]
    fn paths_come_off_the_filesystem_the_way_they_were_written() {
        let dir = std::env::temp_dir().join(format!("eidolon-complete-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("pictures")).unwrap();
        std::fs::write(dir.join("shot.png"), b"x").unwrap();
        std::fs::write(dir.join(".hidden"), b"x").unwrap();
        let mut st = state();
        st.cwd = dir.display().to_string();
        let c = complete(&st, "attach ").unwrap();
        let labels: Vec<&str> = c.rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            ["pictures/", "shot.png"],
            "directories first, and no dotfile until the dot is typed"
        );
        assert_eq!(complete(&st, "attach .").unwrap().rows[0].label, ".hidden");
        // The written half of the path is kept, so completing inside a
        // directory does not rewrite the way it was reached.
        let c = complete(&st, "attach pictures/").unwrap();
        assert!(
            c.rows[0].insert.is_empty(),
            "an empty directory has nothing to offer"
        );
        std::fs::write(dir.join("pictures").join("a.png"), b"x").unwrap();
        assert_eq!(
            complete(&st, "attach pictures/a").unwrap().rows[0].insert,
            "attach pictures/a.png"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

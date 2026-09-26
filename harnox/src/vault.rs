//! The vault conventions both harnesses speak: markdown-note frontmatter, the
//! shape of Mneme's `list_notes` reply, and the **persona** layout under
//! [`PERSONA_FOLDER`].
//!
//! Everything here is pure and synchronous. What is deliberately *not* here is
//! the vault fetch: Melete reaches Mneme through `mneme_auth` +
//! `attachments::call_mneme_function`, the harness through
//! `eidolon_remote::Mneme::rpc`, and neither client belongs in this crate. It
//! is the seam [`crate::policy`] already uses — that module keeps the shell
//! decomposition algebra and leaves the leaf Allow/Flag/Deny table to the
//! consumer; this one keeps the *convention* and leaves the I/O.
//!
//! ## Why the persona layout is shared rather than copied
//!
//! A persona is a note in the operator's vault, and the vault is owned by
//! neither harness. Two copies of [`is_persona_entry`] is a persona that lists
//! in one surface's picker and not the other's, or one that resolves to a
//! different note on each — a divergence with no symptom until somebody
//! notices they are talking to two subtly different characters. The prompt
//! wording in [`persona_prefix`] is shared for the sharper form of the same
//! reason: it is the sentence that turns a note into a voice, and one
//! character rendered by two wordings is two characters.

/// The inner text of a leading `---`-delimited YAML frontmatter block, or
/// `None` when the note has none. Tolerates a leading BOM or newline.
pub fn frontmatter_block(note: &str) -> Option<&str> {
    let t = note.trim_start_matches('\u{feff}').trim_start_matches('\n');
    let rest = t.strip_prefix("---")?;
    // The opening fence must be its own line.
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let end = rest.find("\n---")?;
    Some(&rest[..end])
}

/// `text` with a leading `---`-fenced frontmatter block removed, so a caller
/// never mistakes frontmatter keys for body content. Returns `text` unchanged
/// when there is no complete block to strip.
pub fn strip_frontmatter(text: &str) -> &str {
    let t = text.trim_start_matches('\u{feff}').trim_start_matches('\n');
    let Some(rest) = t.strip_prefix("---\n") else { return text };
    match rest.find("\n---") {
        Some(i) => {
            let after = &rest[i + "\n---".len()..];
            after.trim_start_matches(|c| c != '\n').strip_prefix('\n').unwrap_or(after)
        }
        None => text,
    }
}

/// One frontmatter field's value, by key, case-insensitively. `None` when
/// there is no frontmatter, no such key, or the value is empty.
///
/// A line scan rather than a YAML parse: the fields read through here are
/// scalars written by hand, and pulling a YAML dependency into the crate to
/// read `name:` would be the sort of over-building the vault's own personas
/// have opinions about. A value that needs YAML's quoting or block scalars is
/// out of scope by construction — see [`persona_name`], whose whole job is one
/// short string.
pub fn frontmatter_field<'a>(note: &'a str, key: &str) -> Option<&'a str> {
    let fm = frontmatter_block(note)?;
    fm.lines().find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(key).then(|| v.trim())
    }).filter(|v| !v.is_empty())
}

/// Parse Mneme's `list_notes` reply — `"N note(s):\n- path\n..."` — into
/// relative vault paths.
///
/// A bullet strip and nothing more: the call is always folder-scoped, so
/// every bullet in the reply is already in scope and no prefix filter is
/// needed.
pub fn note_list_paths(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("- "))
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect()
}

/// The vault folder personas live under — user-editable notes, one per
/// persona: frontmatter carrying at least a `name`, and a markdown body that
/// becomes the system-prompt prefix when pinned.
///
/// A persona is either a **flat note** (`wiki/personalities/Foo.md`) or a
/// **folder** whose [`PERSONA_ENTRY`] is the note
/// (`wiki/personalities/Foo/index.md`). The folder form exists so a persona
/// can carry sibling files — reference material for other work — without them
/// reaching a turn: only the entry file is ever read, and wikilinks out of it
/// are deliberately not followed. Anything a live conversation needs belongs
/// in the entry file, however long it gets.
pub const PERSONA_FOLDER: &str = "wiki/personalities";

/// The note inside a persona folder that *is* the persona. Every other file
/// in the folder is a sibling: neither listed as a persona nor spliced into a
/// turn.
pub const PERSONA_ENTRY: &str = "index.md";

/// The vault paths that could hold the persona pinned as `pin`, in the order
/// they should be tried.
///
/// A pin is whatever the operator gave — a frontmatter `name`, or a full vault
/// path. The bare name goes first (it covers flat notes, and Mneme also
/// resolves it through a folder entry's `aliases:`), then the folder entry
/// path explicitly, so the folder form keeps working for a persona whose
/// frontmatter carries no alias. A pin that already looks like a path gets
/// only itself: joining [`PERSONA_FOLDER`] onto it would address nonsense.
pub fn persona_note_candidates(pin: &str) -> Vec<String> {
    if pin.contains('/') {
        return vec![pin.to_string()];
    }
    vec![pin.to_string(), format!("{PERSONA_FOLDER}/{pin}/{PERSONA_ENTRY}")]
}

/// Whether a path listed under [`PERSONA_FOLDER`] names a persona, as opposed
/// to a sibling file inside a persona folder.
///
/// `list_notes` recurses, so a folder-form persona lists every file it holds;
/// without this filter each sibling would show up in a picker as its own
/// character. Accepts a flat note directly under the folder, or a folder's
/// [`PERSONA_ENTRY`]; rejects everything else, including anything nested
/// deeper than one folder.
pub fn is_persona_entry(path: &str) -> bool {
    let Some(rel) = path.strip_prefix(&format!("{PERSONA_FOLDER}/")) else { return false };
    match rel.split_once('/') {
        None => !rel.is_empty(),
        Some((folder, rest)) => !folder.is_empty() && rest == PERSONA_ENTRY,
    }
}

/// A persona note's frontmatter `name`, which is the character's identity and
/// the key anything per-persona is addressed by.
///
/// It is the note's answer and not the pin string on purpose: a session that
/// pinned `μέλι` and one that pinned `wiki/personalities/μέλι/index.md` are
/// talking to the same character, and only the note can say what they both
/// mean.
pub fn persona_name(note: &str) -> Option<String> {
    frontmatter_field(note, "name").map(str::to_string)
}

/// The system-prompt prefix a pinned persona contributes: the note's body
/// under one instruction. Empty when the body is — a persona note that is all
/// frontmatter says nothing, and an instruction to embody nothing is worse
/// than silence.
///
/// `body` is expected to have been through [`strip_frontmatter`]; the
/// frontmatter is the machinery by which the note is found and keyed, and
/// spending prompt on it would tell the model its own filing system.
pub fn persona_prefix(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return String::new();
    }
    format!(
        "Embody the persona below for this conversation — let it shape your voice and \
         character while you do the work:\n{body}\n\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_is_read_and_stripped() {
        let note = "---\nname: μέλι\ntagline: a muse\n---\n\n## Who she is\n\nbody\n";
        assert_eq!(frontmatter_field(note, "name"), Some("μέλι"));
        // Case-insensitive on the key, and untouched on the value.
        assert_eq!(frontmatter_field(note, "NAME"), Some("μέλι"));
        assert_eq!(strip_frontmatter(note).trim(), "## Who she is\n\nbody");
        assert_eq!(persona_name(note).as_deref(), Some("μέλι"));
    }

    #[test]
    fn a_note_without_frontmatter_survives_both() {
        let note = "just a body, no fences\n";
        assert_eq!(frontmatter_block(note), None);
        assert_eq!(persona_name(note), None);
        // Unchanged rather than emptied: there was nothing to strip.
        assert_eq!(strip_frontmatter(note), note);
    }

    #[test]
    fn an_unterminated_fence_strips_nothing() {
        // Better to send frontmatter as body than to swallow the whole note.
        let note = "---\nname: x\nno closing fence\n";
        assert_eq!(strip_frontmatter(note), note);
    }

    #[test]
    fn an_empty_field_reads_as_absent() {
        assert_eq!(frontmatter_field("---\nname:\n---\nbody\n", "name"), None);
    }

    #[test]
    fn a_bare_pin_tries_the_flat_note_then_the_folder_entry() {
        assert_eq!(
            persona_note_candidates("μέλι"),
            vec!["μέλι".to_string(), "wiki/personalities/μέλι/index.md".to_string()]
        );
    }

    #[test]
    fn a_path_pin_gets_only_itself() {
        let pin = "wiki/personalities/μέλι/index.md";
        assert_eq!(persona_note_candidates(pin), vec![pin.to_string()]);
    }

    #[test]
    fn only_entries_are_personas() {
        // The real vault's one persona, folder-form with four siblings.
        assert!(is_persona_entry("wiki/personalities/μέλι/index.md"));
        assert!(!is_persona_entry("wiki/personalities/μέλι/journal.md"));
        assert!(!is_persona_entry("wiki/personalities/μέλι/appearance.md"));
        // The flat form.
        assert!(is_persona_entry("wiki/personalities/Foo.md"));
        // Nested deeper than one folder, and outside the folder entirely.
        assert!(!is_persona_entry("wiki/personalities/μέλι/sub/index.md"));
        assert!(!is_persona_entry("wiki/projects/Eidolon/index.md"));
        assert!(!is_persona_entry("wiki/personalities/"));
    }

    #[test]
    fn a_listing_becomes_paths() {
        let reply = "5 note(s):\n- wiki/personalities/μέλι/index.md\n- wiki/personalities/μέλι/canon.md\n";
        let paths = note_list_paths(reply);
        assert_eq!(paths.len(), 2);
        assert_eq!(paths[0], "wiki/personalities/μέλι/index.md");
        // And the pair composes into the picker's one row per persona.
        assert_eq!(paths.iter().filter(|p| is_persona_entry(p)).count(), 1);
    }

    #[test]
    fn an_empty_body_earns_no_instruction() {
        assert_eq!(persona_prefix("   \n  "), "");
        assert_eq!(persona_prefix(strip_frontmatter("---\nname: x\n---\n")), "");
    }

    #[test]
    fn a_body_is_wrapped_once_and_ends_clear_of_what_follows() {
        let p = persona_prefix("She reads history compulsively.");
        assert!(p.starts_with("Embody the persona below"));
        assert!(p.contains("She reads history compulsively."));
        assert!(p.ends_with("\n\n"), "the prefix must not run into the system prompt");
    }
}

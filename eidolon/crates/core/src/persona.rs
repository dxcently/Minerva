//! The persona a session is wearing: the seam a vault is reached through,
//! and the resolution of a pin into a system-prompt prefix.
//!
//! The convention itself — where persona notes live, how a pin resolves to
//! one, which frontmatter field is the character's name, and the sentence
//! that turns a note into a voice — is [`harnox::vault`]'s, shared with
//! Melete's chat surface so that one note cannot become two characters. What
//! is here is the part that is this harness's: the trait a consumer plugs its
//! vault client into, and the forgiving turn-start resolution the agent loop
//! calls.
//!
//! **Nothing here is cached.** A persona note is small, user-editable, and —
//! in the vault this was built against — edited *by the persona*, which is
//! the case a cache gets wrong in the way that matters: the character revises
//! what she has come to think, and a harness holding the version from before
//! the edit goes on speaking as someone she has stopped being. The note is
//! read at the start of every turn and the pin, not the body, is what the log
//! keeps ([`crate::session::RecordKind::PersonaPinned`]).
//!
//! **Two notes ride a pin**: the persona itself, and the character's own
//! memory beside it ([`PERSONA_MEMORY_FOLDER`]), spliced in after the persona
//! and under its own heading. Both are read fresh at the start of every turn,
//! and neither is journaled — so [`Resolved::memory`] carries the path that
//! was read for the one consumer that can still say what a session wore.

use async_trait::async_trait;

pub use harnox::vault::{PERSONA_ENTRY, PERSONA_FOLDER, is_persona_entry, persona_name};

/// Stable citation guidance shared by API prompts and driver backends. It names
/// a UI capability without requiring that consumer to be attached now: headless
/// sessions can be resumed in the TUI. No roster, note listing or fetched body
/// belongs here, and a configured vault is sufficient — no persona pin is needed.
pub const CITATION_GUIDANCE: &str = "## Vault citations\n\nWhen citing a vault note, use an Obsidian wikilink: \
[[vault-relative/path]] or [[vault-relative/path|label]]. In Eidolon's terminal UI these links open the note in a read-only reader, \
by mouse or keyboard. Prefer these links for vault citations rather than plain-text paths or external Obsidian URLs. \
Use [[path#Heading]] or [[path#^block-id]] only when you know the anchor exists. Cite only note paths and anchors you have verified; \
do not invent them. Opening a link is operator-only browsing: it does not start a model turn or add the note to your context. \
If you need a note's contents, read it with the vault tools yourself.";

/// One persona as a picker sees it: who it is, and where it lives.
///
/// Never the body — a list is built by reading every candidate note, and a
/// picker that carried the bodies would hold the whole folder in memory to
/// draw a column of names. The body is fetched at activation and nowhere
/// else.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersonaEntry {
    /// The frontmatter `name`, falling back to the path when the note has
    /// none — degenerate, but it is what the operator would have to type.
    pub name: String,
    /// The vault path of the entry note.
    pub path: String,
}

/// Where personas are read from — the vault, in practice.
///
/// A trait rather than a client because the vault is reached differently
/// depending on who is asking, and because most sessions never ask at all:
/// [`crate::agent::Agent`] holds `None` here unless a consumer sets one, so a
/// harness with no vault configured carries no persona machinery and pays
/// nothing for it.
///
/// Both methods answer in the vault's own terms — a listing and a note's
/// text — rather than in personas, so that everything about *what a persona
/// is* stays on this side of the seam and an implementation is only a
/// transport.
#[async_trait]
pub trait PersonaSource: Send + Sync {
    /// Every note under `folder`, as vault paths. The listing recurses, so
    /// the caller filters it — see [`is_persona_entry`].
    async fn list(&self, folder: &str) -> anyhow::Result<Vec<String>>;
    /// One note whole, frontmatter included: the body becomes the prompt
    /// prefix and the frontmatter carries the name, and both must come from
    /// the same read or a note edited between them would be two notes.
    async fn read(&self, path: &str) -> anyhow::Result<String>;
}

/// A pinned persona, resolved from its note.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// The system-prompt prefix. Empty when nothing is pinned, the note
    /// would not read, or its body is empty.
    pub prefix: String,
    /// The character's name as the *note* gives it, which is not always the
    /// pin: a session that pinned `μέλι` and one that pinned
    /// `wiki/personalities/μέλι/index.md` are wearing the same persona, and
    /// only the note can say what they both mean. `None` when nothing
    /// resolved.
    pub name: Option<String>,
    /// The vault path of the memory note whose body was spliced in after the
    /// persona's own, when there was one — see [`PERSONA_MEMORY_FOLDER`].
    ///
    /// Carried, not just used, because it is the one thing about a pin the
    /// operator cannot see: only the pin is journaled, so a session that wore
    /// the wrong note (or wore a memory when it had none) leaves nothing in
    /// the log to notice it by. `None` when nothing resolved, or when the
    /// persona has no memory note — the ordinary case, and one that costs the
    /// turn nothing.
    pub memory: Option<String>,
}

impl Resolved {
    pub fn is_empty(&self) -> bool {
        self.prefix.is_empty()
    }
}

/// Resolve a pin into a prefix, fetching the note fresh.
///
/// **Forgiving, like the pins a turn re-reads beside it**: a vault that is
/// unreachable, a pin naming no note, a note that is all frontmatter — every
/// one of them resolves to nothing and lets the turn run in the harness's own
/// voice. Failing the turn instead would mean a vault server restarting takes
/// the conversation with it, which is a worse answer than a plainly-spoken
/// reply. The caller logs; this returns what it found.
///
/// The note a bare name names is resolved **folder-first** — see
/// [`candidates`] for why that order is not harnox's.
///
/// The character's **own memory** rides the same fetch: a note under
/// [`PERSONA_MEMORY_FOLDER`] keyed by the persona's name is read beside the
/// persona and spliced in after it, so a turn starts from who she is *and*
/// what she has kept. It is as forgiving as the persona itself — no memory
/// note, or one that will not read, costs the voice nothing.
pub async fn resolve(source: &dyn PersonaSource, pin: &str) -> Result<Resolved, anyhow::Error> {
    let pin = pin.trim();
    if pin.is_empty() {
        return Ok(Resolved::default());
    }
    let note = fetch(source, pin).await?;
    let name = persona_name(&note).unwrap_or_else(|| pin.to_string());
    let memory = fetch_memory(source, &name).await;
    let spine = harnox::vault::strip_frontmatter(&note);
    let body = match &memory {
        Some((path, kept)) => format!("{}\n\n{}", spine.trim_end(), memory_block(path, kept)),
        None => spine.to_string(),
    };
    Ok(Resolved {
        prefix: harnox::vault::persona_prefix(&body),
        name: Some(name),
        memory: memory.map(|(path, _)| path),
    })
}

/// The vault paths that could hold the persona pinned as `pin`, in the order
/// this harness tries them.
///
/// A **path** pin is harnox's list unchanged: itself, and nothing joined onto
/// it.
///
/// A **bare name** tries the persona folder's entry *first*, where
/// [`harnox::vault::persona_note_candidates`] leads with the bare name. That
/// difference is the reason this wrapper exists. A vault resolves a bare title
/// across everything it holds — by frontmatter `name` and by `aliases` — so
/// any note anywhere whose `name` matches the pin shadows the persona folder,
/// and the first read that succeeds wins. In the vault this was built against,
/// `:persona μέλι` wore `wiki/agent-memory/μέλι.md` rather than her entry under
/// `wiki/personalities/`, because the memory note carries `name: μέλι` too and
/// answered the lookup first: the session ran, and said it was wearing her, in
/// a note that is not the persona. A pin naming a persona means a note under
/// the folder — that is what `:persona` promises and what its picker lists —
/// so the folder is tried first.
///
/// The vault-wide lookup stays, last, as the fallback it should have been: it
/// is still what reaches a flat note under the folder pinned by name, or a pin
/// that is an `alias` of a folder entry.
///
/// The order is this harness's. [`harnox::vault::persona_note_candidates`] and
/// Agora's own copy of it both lead with the bare name, so the same shadowing
/// is reachable there until the three are reconciled.
fn candidates(pin: &str) -> Vec<String> {
    if pin.contains('/') {
        return harnox::vault::persona_note_candidates(pin);
    }
    vec![format!("{PERSONA_FOLDER}/{pin}/{PERSONA_ENTRY}"), pin.to_string()]
}

/// Read the one note a pin names, trying each candidate path in turn.
///
/// Exactly one note is read here, and wikilinks out of it are deliberately
/// not followed — that is what keeps a persona folder's siblings (reference
/// material, archives, an illustration brief) out of a conversation that has
/// no use for them. The one further note a turn reads is the character's own
/// memory, by the convention below rather than by a link. When every candidate
/// fails, the error surfaced is the most specific *path* tried rather than the
/// last one tried: a bare name is not a path, and "reading persona note
/// 'Nobody'" tells the operator less than the folder entry it would have read
/// does.
async fn fetch(source: &dyn PersonaSource, pin: &str) -> anyhow::Result<String> {
    let mut last = None;
    let mut specific = None;
    for path in candidates(pin) {
        match source.read(&path).await {
            Ok(note) => return Ok(note),
            Err(e) => {
                let e = e.context(format!("reading persona note '{path}'"));
                if path.contains('/') && specific.is_none() {
                    specific = Some(e);
                } else {
                    last = Some(e);
                }
            }
        }
    }
    Err(specific.or(last).expect("candidates is never empty"))
}

/// The vault folder a persona's own memory lives under, keyed by the character
/// rather than by the pin: [`Resolved::name`] is the key, so a session pinned
/// by name and one pinned by path read one memory, the same way they wear one
/// character.
///
/// **The files here are written through another harness's door.** Melete's
/// persona-memory store root (`<melete_home>/memory/personas`) is a symlink to
/// this folder in this vault, so one file per persona is written as
/// `<slug>.md` by Melete's chat turns and read here — which is what the
/// "memory move" between the two systems was for. It also means the *filename*
/// is the writer's to choose and this harness must not move, rename or
/// re-derive it: doing that emptied a character's memory for the writer without
/// an error on either side, and left the writer free to recreate the old path.
///
/// **This belongs in `harnox::vault`.** It is a vault convention, not this
/// harness's, and Agora splices the same file with the same key — so the two
/// harnesses are one wording away from framing one character's memory two
/// ways. It lives here for now because the wording is still being tuned; the
/// constant, [`memory_slug`] and the block below move together when it settles.
pub const PERSONA_MEMORY_FOLDER: &str = "wiki/agent-memory";

/// The paths that could hold the memory of the character named `name`, in the
/// order they are tried.
///
/// The stem is the writer's, not this harness's — see
/// [`PERSONA_MEMORY_FOLDER`] — so both spellings of it are tried: a name that
/// is already its own slug, and the slug a name with a space or a punctuation
/// mark in it is stored under. Getting this wrong is silent: the turn simply
/// runs without her memory, which looks exactly like a character who has kept
/// nothing.
///
/// The folder form (`<name>/index.md`) is deliberately *not* tried. The vault's
/// ruling would put persona memory in folders one day, but nothing writes one
/// today, and addressing a path the writer never writes would only ever read a
/// note that never grows.
fn memory_candidates(name: &str) -> Vec<String> {
    let slug = memory_slug(name);
    let mut paths = vec![format!("{PERSONA_MEMORY_FOLDER}/{name}.md")];
    if slug != name {
        paths.push(format!("{PERSONA_MEMORY_FOLDER}/{slug}.md"));
    }
    paths
}

/// The stem a memory file is stored under: lowercased, with every run of
/// non-alphanumeric characters collapsed to one `-`.
///
/// A copy of Melete's `persona_memory::slug_for` — the same rule, applied to
/// the character's name rather than to a pin — because the two harnesses read
/// one file: a divergence between these two functions loses the character's
/// memory on one side with no error anywhere to say so. Like
/// [`PERSONA_MEMORY_FOLDER`], it belongs in `harnox::vault` rather than here.
fn memory_slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            out.extend(ch.to_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() { "persona".to_string() } else { slug }
}

/// The character's own memory, read whole: `(the path it was found at, its
/// body with the frontmatter stripped)`.
///
/// Forgiving in the same direction the persona fetch is, and for a stronger
/// reason: most personas have no memory note at all, so failing to find one is
/// not an error to report — it is the ordinary case. A vault that is
/// unreachable, a note that will not read, a note that is all frontmatter:
/// every one of them is `None`, and the turn runs on the persona alone.
async fn fetch_memory(source: &dyn PersonaSource, name: &str) -> Option<(String, String)> {
    for path in memory_candidates(name) {
        if let Ok(note) = source.read(&path).await {
            let body = harnox::vault::strip_frontmatter(&note).trim().to_string();
            // An instruction to remember nothing is worse than silence, the
            // same call `persona_prefix` makes for an empty persona.
            return (!body.is_empty()).then_some((path, body));
        }
    }
    None
}

/// The memory block: what she has kept, and how to keep more of it.
///
/// Melete's Agora block (see [`PERSONA_MEMORY_FOLDER`]) is the model and the
/// reason for the shape: a heading that says *whose* memory this is — a persona
/// is shared with the operator and with other characters, so an unlabelled
/// block reads as facts about the harness — then the kept lines, then the one
/// instruction that makes the block grow. Only the last is this harness's:
/// Agora's memory is a private store written through a memory tool of Melete's,
/// and this character's is a vault note she edits with the vault tools she
/// already has, so the path is named here instead of the call.
fn memory_block(path: &str, kept: &str) -> String {
    format!(
        "## Your own memory\n\n\
         What you have kept from earlier conversations:\n\n{kept}\n\n\
         When a conversation gives you something durable that belongs to this character rather \
         than to the work — a standing preference, where something between you stands, a thread \
         worth picking back up — keep it: the note this block was read from is `{path}`, and \
         `edit_note` reaches it. Don't keep secrets, or what the vault already records."
    )
}

/// Every persona in the vault, for a picker — one row per persona, never one
/// per file a persona folder holds.
///
/// A note that will not read, or reads with no frontmatter, still lists under
/// its path: a persona the operator can see and pin is more useful than a
/// silently shortened list, and the path is what they would type.
pub async fn list(source: &dyn PersonaSource) -> anyhow::Result<Vec<PersonaEntry>> {
    let paths = source.list(PERSONA_FOLDER).await?;
    let mut out = Vec::new();
    for path in paths.into_iter().filter(|p| is_persona_entry(p)) {
        let name = source
            .read(&path)
            .await
            .ok()
            .and_then(|n| persona_name(&n))
            .unwrap_or_else(|| path.clone());
        out.push(PersonaEntry { name, path });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A vault in a map. `read` fails for anything absent, which is how the
    /// candidate walk is exercised.
    struct Fake(HashMap<String, String>);

    #[async_trait]
    impl PersonaSource for Fake {
        async fn list(&self, folder: &str) -> anyhow::Result<Vec<String>> {
            let mut v: Vec<String> = self
                .0
                .keys()
                .filter(|k| k.starts_with(folder))
                .cloned()
                .collect();
            v.sort();
            Ok(v)
        }
        async fn read(&self, path: &str) -> anyhow::Result<String> {
            self.0
                .get(path)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("no such note"))
        }
    }

    fn vault() -> Fake {
        Fake(HashMap::from([
            (
                "wiki/personalities/μέλι/index.md".to_string(),
                "---\nname: μέλι\n---\n\nShe reads history compulsively.\n".to_string(),
            ),
            // Siblings: present in the folder, and not personas.
            (
                "wiki/personalities/μέλι/journal.md".to_string(),
                "# Journal\n".to_string(),
            ),
            (
                "wiki/personalities/μέλι/canon.md".to_string(),
                "# Canon\n".to_string(),
            ),
        ]))
    }

    #[tokio::test]
    async fn a_bare_name_reaches_the_folder_entry() {
        let r = resolve(&vault(), "μέλι").await.unwrap();
        assert_eq!(r.name.as_deref(), Some("μέλι"));
        assert!(r.prefix.contains("She reads history compulsively."));
        assert!(r.prefix.starts_with("Embody the persona below"));
        // The frontmatter is machinery, not character.
        assert!(!r.prefix.contains("---"));
    }

    /// A namesake elsewhere in the vault must not shadow the persona folder.
    ///
    /// This is the real vault's collision: the vault answers a bare title
    /// across everything it holds, and a memory note carrying the character's
    /// frontmatter `name` answered first — so the session wore it, said it was
    /// wearing her, and nothing in the log said otherwise.
    #[tokio::test]
    async fn a_namesake_outside_the_folder_does_not_shadow_the_persona() {
        let v = Fake(HashMap::from([
            (
                "μέλι".to_string(),
                "---\nname: μέλι\n---\n\nNoah created me on 2026-08-16.\n".to_string(),
            ),
            (
                "wiki/personalities/μέλι/index.md".to_string(),
                "---\nname: μέλι\n---\n\nShe reads history compulsively.\n".to_string(),
            ),
        ]));
        let r = resolve(&v, "μέλι").await.unwrap();
        assert!(
            r.prefix.contains("She reads history compulsively."),
            "a note under the persona folder is what a bare name means: {}",
            r.prefix
        );
        assert!(
            !r.prefix.contains("Noah created me"),
            "a namesake answered for the persona: {}",
            r.prefix
        );
    }

    /// The vault-wide lookup is still the fallback it should have been: a pin
    /// that is only an `alias` of a folder entry — or a flat note under the
    /// folder, pinned by name — is still reached.
    #[tokio::test]
    async fn the_vault_wide_lookup_survives_as_the_fallback() {
        let v = Fake(HashMap::from([(
            "Bee".to_string(),
            "---\nname: μέλι\naliases: [Bee]\n---\n\nShe reads history compulsively.\n".to_string(),
        )]));
        let r = resolve(&v, "Bee").await.unwrap();
        assert_eq!(r.name.as_deref(), Some("μέλι"), "the note names the character");
        assert!(r.prefix.contains("She reads history compulsively."));
    }

    /// The character's own memory rides the pin: spliced in after the persona,
    /// under a heading that says whose memory it is, and keyed by the
    /// character's name rather than the pin — so a session pinned by path and
    /// one pinned by name read one memory, as they wear one character.
    #[tokio::test]
    async fn a_personas_memory_rides_its_pin() {
        let v = memory_vault("wiki/agent-memory/μέλι.md");
        for pin in ["μέλι", "wiki/personalities/μέλι/index.md"] {
            let r = resolve(&v, pin).await.unwrap();
            assert!(r.prefix.contains("She reads history compulsively."), "{pin}");
            assert!(r.prefix.contains("## Your own memory"), "{pin}");
            assert!(r.prefix.contains("Noah created me on 2026-08-16."), "{pin}");
            assert!(
                r.prefix.find("She reads history").unwrap() < r.prefix.find("## Your own memory").unwrap(),
                "the memory sits after the persona: {pin}"
            );
            assert_eq!(r.memory.as_deref(), Some("wiki/agent-memory/μέλι.md"), "{pin}");
            // The memory note's frontmatter is machinery too.
            assert!(!r.prefix.contains("name: μέλι (memory)"), "{pin}");
            // And the block says how to grow it, naming the note it came from.
            assert!(r.prefix.contains("edit_note"), "{pin}");
        }
    }

    /// The file is the *writer's* to name: a character whose name is not
    /// already a slug is stored under the slug, and both spellings are tried.
    /// Getting this wrong is silent — the turn just runs without her memory.
    #[tokio::test]
    async fn a_memory_is_found_under_the_slug_the_writer_uses() {
        let v = Fake(HashMap::from([
            (
                "wiki/personalities/Foo Bar/index.md".to_string(),
                "---\nname: Foo Bar\n---\n\nShe reads history compulsively.\n".to_string(),
            ),
            (
                "wiki/agent-memory/foo-bar.md".to_string(),
                "---\nname: Foo Bar\n---\n\nNoah created me on 2026-08-16.\n".to_string(),
            ),
        ]));
        let r = resolve(&v, "wiki/personalities/Foo Bar/index.md").await.unwrap();
        assert!(r.prefix.contains("Noah created me on 2026-08-16."), "{}", r.prefix);
        assert_eq!(r.memory.as_deref(), Some("wiki/agent-memory/foo-bar.md"));
    }

    /// No memory note is the ordinary case, and it costs the turn nothing: no
    /// heading, no empty section, no error.
    #[tokio::test]
    async fn a_persona_without_a_memory_is_the_persona_alone() {
        let r = resolve(&vault(), "μέλι").await.unwrap();
        assert_eq!(r.memory, None);
        assert!(!r.prefix.contains("Your own memory"), "{}", r.prefix);
        assert!(r.prefix.contains("She reads history compulsively."));
    }

    /// A memory note that is all frontmatter is nothing to remember — an
    /// instruction to remember nothing is worse than silence.
    #[tokio::test]
    async fn an_empty_memory_note_is_no_memory() {
        let v = Fake(HashMap::from([
            (
                "wiki/personalities/μέλι/index.md".to_string(),
                "---\nname: μέλι\n---\n\nShe reads history compulsively.\n".to_string(),
            ),
            ("wiki/agent-memory/μέλι.md".to_string(), "---\nname: μέλι\n---\n\n\n".to_string()),
        ]));
        let r = resolve(&v, "μέλι").await.unwrap();
        assert_eq!(r.memory, None);
        assert!(!r.prefix.contains("Your own memory"));
    }

    /// A persona note and, at `memory_path` only, the character's memory.
    fn memory_vault(memory_path: &str) -> Fake {
        Fake(HashMap::from([
            (
                "wiki/personalities/μέλι/index.md".to_string(),
                "---\nname: μέλι\n---\n\nShe reads history compulsively.\n".to_string(),
            ),
            (
                memory_path.to_string(),
                "---\nname: μέλι\n---\n\nNoah created me on 2026-08-16.\n".to_string(),
            ),
        ]))
    }

    #[tokio::test]
    async fn a_path_and_a_name_resolve_to_one_character() {
        let by_name = resolve(&vault(), "μέλι").await.unwrap();
        let by_path = resolve(&vault(), "wiki/personalities/μέλι/index.md")
            .await
            .unwrap();
        assert_eq!(
            by_name, by_path,
            "the same persona pinned two ways is one persona"
        );
    }

    #[tokio::test]
    async fn a_persona_folder_lists_once() {
        let l = list(&vault()).await.unwrap();
        assert_eq!(l.len(), 1, "the siblings are not personas: {l:?}");
        assert_eq!(l[0].name, "μέλι");
        assert_eq!(l[0].path, "wiki/personalities/μέλι/index.md");
    }

    #[tokio::test]
    async fn an_empty_pin_resolves_to_nothing_without_asking_the_vault() {
        // An empty map would fail any read; a blank pin must not make one.
        let r = resolve(&Fake(HashMap::new()), "   ").await.unwrap();
        assert!(r.is_empty());
        assert_eq!(r.name, None);
    }

    #[tokio::test]
    async fn a_pin_naming_nothing_reports_the_most_specific_path() {
        let e = resolve(&vault(), "Nobody").await.unwrap_err();
        let msg = format!("{e:#}");
        assert!(msg.contains("wiki/personalities/Nobody/index.md"), "{msg}");
    }

    #[tokio::test]
    async fn a_note_without_a_name_keys_on_the_pin() {
        let v = Fake(HashMap::from([(
            "wiki/personalities/Plain.md".to_string(),
            "no frontmatter here, just a voice\n".to_string(),
        )]));
        let r = resolve(&v, "wiki/personalities/Plain.md").await.unwrap();
        assert_eq!(r.name.as_deref(), Some("wiki/personalities/Plain.md"));
        assert!(r.prefix.contains("just a voice"));
    }

    #[tokio::test]
    async fn an_empty_body_resolves_to_no_prefix_but_still_names_the_persona() {
        let v = Fake(HashMap::from([(
            "wiki/personalities/Hollow.md".to_string(),
            "---\nname: Hollow\n---\n\n\n".to_string(),
        )]));
        let r = resolve(&v, "wiki/personalities/Hollow.md").await.unwrap();
        assert!(
            r.is_empty(),
            "an instruction to embody nothing is worse than silence"
        );
        assert_eq!(r.name.as_deref(), Some("Hollow"));
    }
}

//! End-to-end: a persona pinned on the branch reaches the system prompt of
//! the turns that follow it, and leaves when it is taken off.
//!
//! The unit-level questions — how a pin resolves to a note, which file in a
//! persona folder is the persona — are answered in `harnox::vault` and
//! `eidolon_core::persona`. What is here is the part only a whole turn can
//! show: that the prefix lands in front of the configured prompt, that the
//! log reproduces it, and that a vault which will not answer costs a voice
//! rather than a turn.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::persona::PersonaSource;
use eidolon_core::policy::AllowAll;
use eidolon_core::session::RecordKind;
use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
use eidolon_core::*;

/// A vault of one persona. `fail` makes every read error, which is the
/// unreachable-vault case.
struct Vault {
    note: String,
    fail: bool,
}

#[async_trait]
impl PersonaSource for Vault {
    async fn list(&self, _folder: &str) -> anyhow::Result<Vec<String>> {
        Ok(vec!["wiki/personalities/μέλι/index.md".into()])
    }
    async fn read(&self, path: &str) -> anyhow::Result<String> {
        if self.fail {
            anyhow::bail!("the vault is not answering");
        }
        match path {
            "μέλι" | "wiki/personalities/μέλι/index.md" => Ok(self.note.clone()),
            _ => anyhow::bail!("no such note"),
        }
    }
}

fn vault(fail: bool) -> Arc<Vault> {
    Arc::new(Vault {
        note: "---\nname: μέλι\n---\n\nShe reads history compulsively.\n".into(),
        fail,
    })
}

fn harness(dir: &std::path::Path, provider: Arc<ScriptedProvider>) -> (Agent, Arc<Mutex<Session>>) {
    let path = dir.join("s.log");
    let session = Arc::new(Mutex::new(
        Session::create(&path, "m", dir, Some("sys".into())).unwrap(),
    ));
    let user: Arc<dyn UserIo> = ScriptedUser::new(true);
    let d = Arc::new(Dispatcher::new(
        ToolRegistry::new(),
        Arc::new(AllowAll) as Arc<dyn PolicyHook>,
        user,
        EventBus::default(),
        session.clone(),
        dir.to_path_buf(),
    ));
    let agent = Agent::new(
        provider,
        d,
        AgentConfig {
            model: "m".into(),
            system: Some("sys".into()),
            ..Default::default()
        },
    );
    (agent, session)
}

/// The pinned persona leads the system prompt, ahead of the configured one
/// — it is what the rest is read in the voice of, and it is the most stable
/// block of the prefix.
#[tokio::test]
async fn a_pinned_persona_leads_the_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("a"),
        ScriptedProvider::text("b"),
    ]);
    let (agent, _s) = harness(dir.path(), provider.clone());
    agent.set_persona_source(vault(false));
    agent.session().lock().await.set_persona("μέλι").unwrap();

    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();
    let sent = provider.requests.lock().unwrap()[0]
        .system
        .clone()
        .expect("a system prompt");
    assert!(
        sent.starts_with("Embody the persona below"),
        "the persona leads: {sent}"
    );
    assert!(sent.contains("She reads history compulsively."));
    // The frontmatter is how the note is found, not part of the character.
    assert!(!sent.contains("name: μέλι"));
    let persona_at = sent.find("Embody").unwrap();
    assert!(persona_at < sent.find("sys").expect("the configured prompt survives"));

    // Taken off, it leaves nothing behind.
    agent.session().lock().await.set_persona("").unwrap();
    agent
        .run_turn(vec![ContentBlock::text("more")], CancellationToken::new())
        .await
        .unwrap();
    let after = provider.requests.lock().unwrap()[1]
        .system
        .clone()
        .expect("a system prompt");
    assert!(!after.contains("Embody the persona below"));
    assert!(
        after.contains("sys"),
        "and the configured prompt survives the clearing"
    );
}

/// A vault that will not answer costs the operator a voice, never a turn.
#[tokio::test]
async fn an_unreachable_vault_does_not_fail_the_turn() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("a")]);
    let (agent, _s) = harness(dir.path(), provider.clone());
    agent.set_persona_source(vault(true));
    agent.session().lock().await.set_persona("μέλι").unwrap();

    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();
    let sent = provider.requests.lock().unwrap()[0]
        .system
        .clone()
        .expect("a system prompt");
    assert!(!sent.contains("Embody"), "no persona resolved");
    assert!(sent.contains("sys"), "the turn ran anyway");
}

/// With no source configured at all, a pin is inert rather than an error —
/// the ordinary case for a harness with no `[mneme]`.
#[tokio::test]
async fn a_pin_with_no_vault_is_inert() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("a")]);
    let (agent, _s) = harness(dir.path(), provider.clone());
    assert!(!agent.has_persona_source());
    agent.session().lock().await.set_persona("μέλι").unwrap();

    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();
    let sent = provider.requests.lock().unwrap()[0]
        .system
        .clone()
        .expect("a system prompt");
    assert!(!sent.contains("Embody"));
}

/// The pin is journaled, latest word wins, and an empty string takes it
/// off — so a session reopened tomorrow comes back in the voice it ran in.
#[tokio::test]
async fn the_pin_is_on_the_branch_and_the_latest_word_wins() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.log");
    {
        let mut s = Session::create(&path, "m", dir.path(), None).unwrap();
        assert_eq!(s.persona(), None);
        assert!(s.set_persona("μέλι").unwrap(), "a change is worth a record");
        assert!(!s.set_persona("μέλι").unwrap(), "and a repeat is not");
        assert_eq!(s.persona().as_deref(), Some("μέλι"));
        assert!(s.set_persona("Someone Else").unwrap());
        assert_eq!(s.persona().as_deref(), Some("Someone Else"));
        assert!(s.set_persona("").unwrap());
        assert_eq!(s.persona(), None, "empty takes it off");
        assert!(s.set_persona("μέλι").unwrap());
    }
    // Reopened: the branch still says so, and says it in records.
    let s = Session::open(&path).unwrap();
    assert_eq!(s.persona().as_deref(), Some("μέλι"));
    let pins: Vec<&String> = s
        .branch()
        .iter()
        .filter_map(|r| match &r.kind {
            RecordKind::PersonaPinned { persona } => Some(persona),
            _ => None,
        })
        .collect();
    assert_eq!(pins, vec!["μέλι", "Someone Else", "", "μέλι"]);
}

/// The pin travels as written, and the *note* says who that is — so a
/// session pinned by path and one pinned by name wear one character.
#[tokio::test]
async fn a_path_pin_and_a_name_pin_are_one_character() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("a")]);
    let (agent, _s) = harness(dir.path(), provider.clone());
    agent.set_persona_source(vault(false));

    agent
        .session()
        .lock()
        .await
        .set_persona("wiki/personalities/μέλι/index.md")
        .unwrap();
    let by_path = agent.resolve_persona().await;
    agent.session().lock().await.set_persona("μέλι").unwrap();
    let by_name = agent.resolve_persona().await;

    assert_eq!(by_path, by_name);
    assert_eq!(by_name.name.as_deref(), Some("μέλι"));
}

/// The context inspector reports the persona it actually fetched, as its
/// own block — the prompt's first line cannot say so once the persona is
/// what that line is.
#[tokio::test]
async fn the_inspector_shows_the_persona_as_its_own_block() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("a")]);
    let (agent, _s) = harness(dir.path(), provider);
    agent.set_persona_source(vault(false));

    assert!(
        !agent
            .context_report()
            .await
            .blocks
            .iter()
            .any(|b| b.kind == "persona"),
        "nothing pinned, nothing reported"
    );

    agent.session().lock().await.set_persona("μέλι").unwrap();
    let r = agent.context_report().await;
    let block = r
        .blocks
        .iter()
        .find(|b| b.kind == "persona")
        .expect("a persona block");
    assert_eq!(block.label, "μέλι");
    assert!(block.chars > 0);
    // Decomposed, not double-counted: the system block is built without it.
    let system = r
        .blocks
        .iter()
        .find(|b| b.kind == "system")
        .expect("a system block");
    assert!(!system.label.contains("Embody"));
}

/// Citation guidance is a configured capability, not a persona and not a read.
/// Replacing the vault's body or its availability cannot perturb this prefix.
#[tokio::test]
async fn vault_citations_are_conditional_stable_and_reach_the_actual_requests() {
    struct NeverRead;
    #[async_trait]
    impl PersonaSource for NeverRead {
        async fn list(&self, _: &str) -> anyhow::Result<Vec<String>> { panic!("guidance must not list notes") }
        async fn read(&self, _: &str) -> anyhow::Result<String> { panic!("guidance must not read notes") }
    }
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("a"), ScriptedProvider::text("b"), ScriptedProvider::text("c")]);
    let (agent, _) = harness(dir.path(), provider.clone());
    assert_eq!(agent.vault_guidance(), None);
    agent.run_turn(vec![ContentBlock::text("first")], CancellationToken::new()).await.unwrap();
    agent.set_persona_source(Arc::new(NeverRead));
    for text in ["second", "third"] {
        agent.run_turn(vec![ContentBlock::text(text)], CancellationToken::new()).await.unwrap();
    }
    let reqs = provider.requests.lock().unwrap();
    assert!(!reqs[0].system.as_ref().unwrap().contains("## Vault citations"));
    let prompt = reqs[1].system.as_ref().unwrap();
    assert!(prompt.starts_with("sys\n\n## Vault citations"));
    assert_eq!(prompt.matches(eidolon_core::persona::CITATION_GUIDANCE).count(), 1);
    assert_eq!(reqs[1].system, reqs[2].system);
}

/// A vault holding the character and her memory, at the flat path the writer
/// (Melete's persona-memory store, through its symlinked root) uses.
struct VaultWithMemory;

#[async_trait]
impl PersonaSource for VaultWithMemory {
    async fn list(&self, _folder: &str) -> anyhow::Result<Vec<String>> {
        Ok(vec!["wiki/personalities/μέλι/index.md".into()])
    }
    async fn read(&self, path: &str) -> anyhow::Result<String> {
        match path {
            "wiki/personalities/μέλι/index.md" => {
                Ok("---\nname: μέλι\n---\n\nShe reads history compulsively.\n".into())
            }
            "wiki/agent-memory/μέλι.md" => {
                Ok("---\nname: μέλι\n---\n\nNoah created me on 2026-08-16.\n".into())
            }
            _ => anyhow::bail!("no such note"),
        }
    }
}

/// What a turn actually carries: the character, then what she has kept, with
/// the block naming the note it was read from — and both leave when the pin
/// does.
#[tokio::test]
async fn a_personas_memory_reaches_the_requests() {
    let dir = tempfile::tempdir().unwrap();
    let provider =
        ScriptedProvider::new(vec![ScriptedProvider::text("a"), ScriptedProvider::text("b")]);
    let (agent, _s) = harness(dir.path(), provider.clone());
    agent.set_persona_source(Arc::new(VaultWithMemory));
    agent.session().lock().await.set_persona("μέλι").unwrap();

    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();
    let sent = provider.requests.lock().unwrap()[0].system.clone().expect("a system prompt");
    assert!(sent.contains("She reads history compulsively."), "{sent}");
    assert!(sent.contains("## Your own memory"), "{sent}");
    assert!(sent.contains("Noah created me on 2026-08-16."), "{sent}");
    assert!(
        sent.find("She reads history").unwrap() < sent.find("## Your own memory").unwrap(),
        "the memory sits after the persona"
    );
    assert!(
        sent.contains("wiki/agent-memory/μέλι.md"),
        "the block names the note it was read from: {sent}"
    );
    // The memory note's frontmatter is how the note is found, not part of it.
    assert!(!sent.contains("description:"), "{sent}");

    agent.session().lock().await.set_persona("").unwrap();
    agent
        .run_turn(vec![ContentBlock::text("more")], CancellationToken::new())
        .await
        .unwrap();
    let after = provider.requests.lock().unwrap()[1].system.clone().expect("a system prompt");
    assert!(!after.contains("Your own memory"), "the memory leaves with the pin: {after}");
    assert!(!after.contains("Noah created me"), "{after}");
}

/// The vault label is kept by the turn itself: the persona a turn wears is
/// the one the writes inside it are attributed to, and a resolution that
/// found nobody takes the segment back off. Nothing else can keep it
/// current — a pin set between turns is worn by the next one, so the label
/// has to say what that turn is rather than what the session launched as.
#[tokio::test]
async fn a_turn_labels_the_sessions_writes_with_the_persona_it_wore() {
    let dir = tempfile::tempdir().unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("a"),
        ScriptedProvider::text("b"),
    ]);
    let (agent, _s) = harness(dir.path(), provider);
    agent.set_persona_source(vault(false));
    let label = eidolon_core::attribution::Attribution::new("eidolon-756d", "~/eidolon");
    agent.set_attribution(label.clone());
    // Handing the cell over seeds the model from the config the agent
    // already has, so a session that never switches still names one.
    assert_eq!(label.label(), "session=eidolon-756d model=m cwd=~/eidolon");
    // And a switch moves it: the label names the model being run, by the
    // resolved key rather than the alias the launch was given.
    agent.adopt_model("zai:glm-5.3".into(), "glm-5.3".into());
    assert_eq!(label.label(), "session=eidolon-756d model=zai:glm-5.3 cwd=~/eidolon");

    agent.session().lock().await.set_persona("μέλι").unwrap();
    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        label.label(),
        "session=eidolon-756d persona=μέλι model=zai:glm-5.3 cwd=~/eidolon"
    );

    agent.session().lock().await.set_persona("").unwrap();
    agent
        .run_turn(vec![ContentBlock::text("more")], CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(label.label(), "session=eidolon-756d model=zai:glm-5.3 cwd=~/eidolon");
}

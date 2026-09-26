//! End-to-end: the working directory's own instructions reach the system
//! prompt of every turn, and only the system prompt.
//!
//! The unit-level questions — which file `auto` picks, what an empty file
//! resolves to — are answered in `eidolon_core::project`. What is here is
//! the part only a whole turn can show: that the block lands between the
//! configured prompt and the session note, that it is re-read rather than
//! remembered, and that none of it is ever journaled.

use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use eidolon_core::policy::AllowAll;
use eidolon_core::project::ProjectInstructions;
use eidolon_core::testing::{ScriptedProvider, ScriptedUser};
use eidolon_core::*;

fn harness(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    instructions: ProjectInstructions,
) -> Agent {
    let session = Arc::new(Mutex::new(
        Session::create(&dir.join("s.log"), "m", dir, Some("sys".into())).unwrap(),
    ));
    let user: Arc<dyn UserIo> = ScriptedUser::new(true);
    let d = Arc::new(Dispatcher::new(
        ToolRegistry::new(),
        Arc::new(AllowAll) as Arc<dyn PolicyHook>,
        user,
        EventBus::default(),
        session,
        dir.to_path_buf(),
    ));
    Agent::new(
        provider,
        d,
        AgentConfig {
            model: "m".into(),
            system: Some("sys".into()),
            project_instructions: instructions,
            ..Default::default()
        },
    )
}

fn system_of(provider: &ScriptedProvider, n: usize) -> String {
    provider.requests.lock().unwrap()[n]
        .system
        .clone()
        .expect("a system prompt")
}

/// The file reaches the system prompt between the configured prompt and
/// the session note — the harness's voice, the directory's instructions,
/// the operator's standing note — and names where it came from, so the
/// model can cite and edit the file rather than a rumour of it.
#[tokio::test]
async fn the_directories_instructions_sit_in_the_system_prompt() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "Ship the parser first.\n").unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("done"),
        ScriptedProvider::text("done"),
    ]);
    let agent = harness(dir.path(), provider.clone(), ProjectInstructions::Auto);
    agent
        .session()
        .lock()
        .await
        .set_session_note("we are debugging the parser")
        .unwrap();

    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();

    let sent = system_of(&provider, 0);
    let file_at = sent.find("Ship the parser first.").expect("the file");
    let note_at = sent.find("we are debugging the parser").expect("the note");
    assert!(sent.starts_with("sys"), "the configured prompt still leads");
    assert!(file_at < note_at, "instructions above the session note");
    assert!(sent.contains("CLAUDE.md"), "and the file is named: {sent}");

    // And it is nowhere in the branch: the log keeps the setting's effects
    // out of its bytes, or every turn would grow by the file again.
    let journaled = agent.session().lock().await.messages();
    assert!(
        !journaled
            .iter()
            .any(|m| m.text().contains("Ship the parser first.")),
        "the contents must never be journaled"
    );
}

/// Re-read each turn, so an edit is live on the next request and
/// yesterday's instructions are not still in the prompt behind it —
/// the same liveness the persona's note is re-read for.
#[tokio::test]
async fn an_edit_is_live_on_the_next_turn() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("AGENTS.md");
    std::fs::write(&file, "RULE_ONE\n").unwrap();
    let provider = ScriptedProvider::new(vec![
        ScriptedProvider::text("done"),
        ScriptedProvider::text("done"),
    ]);
    let agent = harness(dir.path(), provider.clone(), ProjectInstructions::Agents);

    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();
    std::fs::write(&file, "RULE_CHANGED\n").unwrap();
    agent
        .run_turn(vec![ContentBlock::text("again")], CancellationToken::new())
        .await
        .unwrap();

    let second = system_of(&provider, 1);
    assert!(second.contains("RULE_CHANGED"), "re-read each turn");
    assert!(!second.contains("RULE_ONE"), "and nothing lingers");
}

/// A file the setting does not name is not read, and a directory holding
/// nothing says nothing — `off` costs no prompt and no journal.
#[tokio::test]
async fn off_and_an_empty_directory_say_nothing() {
    for (instructions, expected) in [
        (ProjectInstructions::Off, None),
        (ProjectInstructions::Agents, Some("agents' rules")),
        (ProjectInstructions::Claude, Some("claude's rules")),
        (ProjectInstructions::Auto, Some("agents' rules")),
    ] {
        // A directory holding nothing: the ordinary directory, whatever
        // the setting says.
        let empty = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(vec![ScriptedProvider::text("done")]);
        let agent = harness(empty.path(), provider.clone(), instructions);
        agent
            .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            system_of(&provider, 0),
            "sys",
            "nothing added: {instructions:?}"
        );

        // A directory holding both files: each setting reads its own, and
        // `off` reads neither.
        let both = tempfile::tempdir().unwrap();
        std::fs::write(both.path().join("CLAUDE.md"), "claude's rules\n").unwrap();
        std::fs::write(both.path().join("AGENTS.md"), "agents' rules\n").unwrap();
        let provider = ScriptedProvider::new(vec![ScriptedProvider::text("done")]);
        let agent = harness(both.path(), provider.clone(), instructions);
        agent
            .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
            .await
            .unwrap();
        match expected {
            Some(rules) => assert!(
                system_of(&provider, 0).contains(rules),
                "{instructions:?} reads its own file"
            ),
            None => assert_eq!(system_of(&provider, 0), "sys", "off reads neither"),
        }
    }
}

/// `:context` shows the block as its own row — a large instructions file
/// is a question about the prompt the operator will want the size of
/// before the model has read it.
#[tokio::test]
async fn the_context_report_sizes_it_as_its_own_block() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("CLAUDE.md"), "Be terse.\n").unwrap();
    let provider = ScriptedProvider::new(vec![ScriptedProvider::text("done")]);
    let agent = harness(dir.path(), provider.clone(), ProjectInstructions::Auto);
    agent
        .run_turn(vec![ContentBlock::text("hi")], CancellationToken::new())
        .await
        .unwrap();

    let r = agent.context_report().await;
    let project = r
        .blocks
        .iter()
        .find(|b| b.kind == "project")
        .expect("a project block");
    assert_eq!(project.label, "CLAUDE.md");
    assert!(project.chars > 0);
    // The system block is the prompt decomposed, not counted twice: it is
    // built with the project block left out, exactly as it is with the
    // persona, and the two blocks reassemble into the string the wire saw.
    let system = r.blocks.iter().find(|b| b.kind == "system").unwrap();
    let sent = system_of(&provider, 0);
    assert_eq!(
        sent.chars().count(),
        system.chars + project.chars + "\n\n".chars().count(),
        "system + project + the join between them is the prompt the wire saw"
    );
}

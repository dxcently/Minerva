//! The vault as a source of personas: [`eidolon_core::persona::PersonaSource`]
//! over the Mneme client.
//!
//! A transport and nothing else. Everything about *what a persona is* —
//! which folder, flat note or folder entry, which frontmatter field is the
//! name, the sentence that turns a note into a voice — lives in
//! `harnox::vault` and is shared with Melete's chat surface, so that one note
//! in the vault cannot become two different characters depending on which
//! harness read it. What is here is `read_note` and `list_notes`.

use async_trait::async_trait;
use eidolon_core::persona::PersonaSource;
use eidolon_remote::Mneme;

/// Personas read out of the operator's Obsidian vault through Mneme.
pub struct VaultPersonas(Mneme);

impl VaultPersonas {
    pub fn new(mneme: Mneme) -> Self {
        VaultPersonas(mneme)
    }
}

#[async_trait]
impl PersonaSource for VaultPersonas {
    async fn list(&self, folder: &str) -> anyhow::Result<Vec<String>> {
        let reply = self
            .0
            .rpc("list_notes", serde_json::json!({ "folder": folder }))
            .await?;
        Ok(harnox::vault::note_list_paths(&reply))
    }

    async fn read(&self, path: &str) -> anyhow::Result<String> {
        // `title` takes a full vault path as readily as a bare name, which
        // is what lets one pin be either — see
        // `harnox::vault::persona_note_candidates`.
        self.0
            .rpc("read_note", serde_json::json!({ "title": path }))
            .await
    }
}

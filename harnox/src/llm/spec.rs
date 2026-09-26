//! Provider configuration: the few fields that distinguish one endpoint
//! from another, and the constructor that turns them into a [`Provider`].
//!
//! ```text
//! Provider { base_url, token, wire: anthropic | openai, auth }
//!   anthropic  → https://api.anthropic.com           bearer (setup-token) or api-key
//!   kimi       → https://api.moonshot.ai/anthropic   api-key   — same code path
//!   qwen       → https://dashscope…/compatible-mode/v1          — adapter
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::anthropic::Anthropic;
use super::credentials::TokenSource;
use super::openai::OpenAiCompat;
use super::provider::Provider;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Wire {
    Anthropic,
    OpenAi,
    Gemini,
    Codex,
}

/// How the credential travels. Only meaningful on the Anthropic wire; the
/// OpenAI wire is always a bearer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthStyle {
    /// `x-api-key: …` — an API key.
    #[default]
    ApiKey,
    /// `Authorization: Bearer …` — a `claude setup-token` bearer, sent with
    /// the OAuth beta header.
    Bearer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderSpec {
    pub name: String,
    pub wire: Wire,
    pub base_url: String,
    #[serde(default)]
    pub token_file: Option<PathBuf>,
    #[serde(default)]
    pub token_env: Option<String>,
    #[serde(default)]
    pub auth: AuthStyle,
    /// Model-name globs this provider serves, e.g. `["claude-*"]`. The
    /// first provider whose list matches a requested model wins.
    #[serde(default)]
    pub models: Vec<String>,
    /// Optional project ID for the Gemini wire (Antigravity).
    #[serde(default)]
    pub project: Option<String>,
}

impl ProviderSpec {
    pub fn token_source(&self) -> TokenSource {
        match (&self.token_file, &self.token_env) {
            (Some(f), _) => {
                if self.wire == Wire::Codex {
                    TokenSource::CodexRefresh { path: f.clone() }
                } else if self.wire == Wire::Gemini || f.extension().is_some_and(|e| e == "db" || e == "sqlite") {
                    TokenSource::GoogleRefresh {
                        path: f.clone(),
                        client_id: None,
                        client_secret: None,
                    }
                } else {
                    TokenSource::File(f.clone())
                }
            }
            (None, Some(e)) => TokenSource::Env(e.clone()),
            (None, None) => TokenSource::None,
        }
    }

    pub fn serves(&self, model: &str) -> bool {
        self.models.iter().any(|g| glob_match(g, model))
    }
}

pub fn build_provider(spec: &ProviderSpec) -> Arc<dyn Provider> {
    match spec.wire {
        Wire::Anthropic => Arc::new(Anthropic::new(&spec.name, &spec.base_url, spec.token_source(), spec.auth)),
        Wire::OpenAi => Arc::new(OpenAiCompat::new(&spec.name, &spec.base_url, spec.token_source())),
        Wire::Codex => Arc::new(super::codex::Codex::new(&spec.name, &spec.base_url, spec.token_source())),
        Wire::Gemini => unimplemented!("Gemini requires HTTP provider construction directly"),
    }
}

/// `*` matches any run of characters; nothing else is special.
pub fn glob_match(pattern: &str, s: &str) -> bool {
    fn go(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some(a), Some(b)) if a == b => go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), s.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_match("claude-*", "claude-sonnet-5"));
        assert!(!glob_match("claude-*", "qwen3"));
        assert!(glob_match("*", "anything"));
        assert!(!glob_match("claude-*", ""));
    }

    #[test]
    fn spec_parses_with_defaults_and_routes_by_model() {
        let spec: ProviderSpec = serde_json::from_str(
            r#"{"name":"kimi","wire":"anthropic","base_url":"https://api.moonshot.ai/anthropic","token_file":"~/.config/kimi/token","models":["kimi-*"]}"#,
        )
        .unwrap();
        assert_eq!(spec.auth, AuthStyle::ApiKey);
        assert!(matches!(spec.token_source(), TokenSource::File(_)));
        assert!(spec.serves("kimi-k2"));
        assert!(!spec.serves("claude-sonnet-5"));
        assert_eq!(build_provider(&spec).name(), "kimi");
    }
}

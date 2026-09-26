//! The shared Rust foundation under Mneme, Melete, and the interactive
//! harness.
//!
//! One crate, feature-gated, rather than several: the pieces that are
//! genuinely common (an OAuth 2.1 server that was maintained twice, ~97%
//! identical, in Mneme and Melete; the browserless OAuth client that Melete
//! runs against Mneme and the harness will run against both) do not split
//! along one clean line — provider-credential acquisition is "auth" by
//! mechanism and "LLM" by purpose, and the OAuth *server* and *client* already
//! have different consumer sets. Features let a module move between groups
//! with a one-line change; crates would make that a coordinated version bump
//! across three repos.
//!
//! ## What is always compiled
//!
//! The core is small and synchronous: [`crypto`] (OS randomness, SHA-256, the
//! encodings every bearer-shaped secret is built from), [`fs`] (atomic `0600`
//! writes), [`env`] (env-var lookup with deprecated aliases), [`html`] (one
//! escape, one place to audit), [`time`], and [`vault`] — the operator's
//! markdown vault: note frontmatter, the shape of a `list_notes` reply, and
//! the persona layout that Melete's chat surface and the harness both have to
//! read *identically* or end up talking to two subtly different characters.
//!
//! [`vault`] sits here rather than behind a feature although its consumer set
//! is the featured one's, because what earns a gate in this crate is the
//! dependencies a module drags in, and that one has none — it is string
//! handling over notes. A `vault = []` feature would buy nothing and add a
//! name every consumer has to remember to name.
//!
//! ## Features
//!
//! | Feature | Module | Consumers |
//! |---|---|---|
//! | `oauth-server` | [`oauth_server`] | Mneme, Melete |
//! | `oauth-client` | [`oauth_client`] | Melete, harness |
//! | `secrets` | [`secrets`] | Melete (harness likely) |
//! | `provider-auth` | [`setup_token`] | Melete, harness |
//! | `llm` | [`llm`] | harness (Melete once its own provider client is deleted) |
//! | `bitcode` | derives on [`llm::message`] | harness |
//! | `claude-cli` | [`claude_cli`] | Melete (harness, if setup-token bearers bill differently from the CLI) |
//! | `policy` | [`policy`] | the harness, Melete |
//!
//! `full` turns them all on. The default set is empty: a consumer names what
//! it uses, and Mneme — which contains no LLM code at all — never compiles a
//! provider layer.
//!
//! **Invariant: features are additive.** A feature adds modules (or, for
//! `bitcode`, trait impls); it never changes the behaviour of a module another
//! feature already compiles. Cargo unifies features across a build, so
//! anything else would let one consumer's choice reconfigure another's.
//!
//! ## Errors
//!
//! `anyhow::Result` throughout, `.context()` on I/O, `bail!` for early exits —
//! the convention both consumers already hold. No custom error enum: the
//! crate's callers render errors, they don't match on them.

pub mod crypto;
pub mod env;
pub mod fs;
pub mod html;
pub mod time;
pub mod vault;

#[cfg(feature = "oauth-server")]
pub mod oauth_server;

#[cfg(feature = "oauth-client")]
pub mod oauth_client;

#[cfg(feature = "secrets")]
pub mod secrets;

#[cfg(feature = "provider-auth")]
pub mod setup_token;

#[cfg(feature = "llm")]
pub mod llm;

#[cfg(feature = "claude-cli")]
pub mod claude_cli;

#[cfg(feature = "policy")]
pub mod policy;

#[cfg(test)]
pub(crate) mod testutil {
    use std::path::PathBuf;

    /// A fresh, unique scratch directory under the system temp dir. `prefix`
    /// identifies the owning test module so leftover dirs are attributable.
    pub fn tempdir(prefix: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir().join(format!(
            "harnox-{prefix}-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }
}

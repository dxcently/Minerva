//! Whether a model's *prerequisite* is here — asked without using it.
//!
//! A picker draws every model the catalog knows before anyone has picked
//! one, and the useful thing to say beside a row is whether this machine
//! could reach it at all: "no key here" in the list costs one line, and the
//! same fact discovered during the turn costs a `401` after the operator has
//! committed to the model. Answering it means looking at what a definition's
//! credential source *is*, never at what it says. The store is asked
//! presence ([`SecretStore::has`]) and never the value; a token file is
//! stat'd and, for a refresh shape, parsed for structure, and never
//! rendered; a driver's executable is looked for on `PATH` and never
//! spawned; no lane of a token pool is claimed. Nothing here mints, refreshes
//! or goes to the network — a probe that did would be a bug rather than a
//! slow path, which is the same promise [`harnox::llm`]'s own credential
//! probe makes for the sources it owns.
//!
//! Two things are deliberately not said. **Validity**: a source being
//! present is not a token being good, and only the endpoint shown the token
//! can say the second. A probe that claimed it would draw every
//! unauthenticated local endpoint as broken and every revoked key as fine
//! until the turn that used it. **Login state**: `claude` on `PATH` is a
//! driver that can start, not a person who has logged in.
//!
//! ## Two channels, never one
//!
//! [`Catalog::readiness`] answers `Err` when the *spec* does not resolve —
//! an unknown provider, a bare id two providers both serve — and
//! `Ok(Readiness::Unavailable { .. })` when the spec is fine and the
//! credential is not. The two have different fixes (a misspelled `:model`
//! versus `eidolon secret set`), and a caller that received them through one
//! channel would print a sentence about a credential for a typo.
//!
//! ## The order is `token()`'s order
//!
//! Custodied secret, pool, file, variable, inline placeholder, nothing —
//! the precedence [`ProviderDef::token`] reads them in, so a probe and the
//! request that follows it agree about *which* source is in play (with the
//! one deliberate exception the pool section below spells out). Each source
//! has its own way of being unusable — absent, unreadable, empty, malformed
//! — and the failure is reported as that source's own problem rather than as
//! a generic "no credential", because the four send the operator to four
//! different fixes.
//!
//! ## The pool is answered from declared lanes, and never claims one
//!
//! A pool is never *claimed* here: reporting which lanes exist must not
//! leave a session holding one (see [`CredentialSource::SecretPool`]), so a
//! probe asks only whether any declared lane is in the store, and the lane a
//! session ends up on stays resolution's business. A pool with no lane
//! filled is [`UnavailableReason::SecretAbsent`] naming every lane — the
//! one place this *knowingly* differs from `token()`, which would fall
//! through to a source declared beside the pool (a script's placeholder,
//! say). A pool is a definition's statement about where its key lives, and a
//! patch that names a pool replaces every other source with it
//! ([`ProviderDef::patched`]), so "fill one of these lanes" is the sentence
//! that gets a session started; a placeholder standing in for one would draw
//! a ready row over a request that cannot authenticate. A single named
//! secret *does* fall through, exactly as `token()` says it does — the store
//! is a key's preferred home, not its only one — and the secret is named in
//! the diagnostic only when nothing anywhere held the credential.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use harnox::llm::{SourceKind, SourceProblem, SourceState, TokenSource};
use harnox::secrets::SecretStore;

use crate::catalog::{CLAUDE_CLI, Catalog, Resolved};
use crate::def::{ProviderDef, Wire};

/// `provider:id` identity plus the structural state, so a diagnostic never
/// has to be re-attributed by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelReadiness {
    /// `provider:id`, spelled as the picker's rows and the session log
    /// spell it — `claude-cli:` with an empty tail is the CLI's own
    /// default model, and `mock` has neither half.
    pub key: String,
    /// The provider half of [`Self::key`].
    pub provider: String,
    /// The model half of [`Self::key`], empty when the spec named none.
    pub model: String,
    pub state: Readiness,
}

/// The structural state of one model's prerequisite. See the module docs for
/// what is deliberately not claimed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Readiness {
    /// No credential is declared and none is needed (incl. `mock`).
    NoCredentialNeeded,
    /// A source is structurally present. Validity is NOT verified.
    Present(CredentialSource),
    /// A driver backend's executable was found on PATH. Login state is
    /// deliberately UNVERIFIED: executable presence is not proof of a login.
    DriverExecutable { binary: String },
    /// Nothing usable, with a safe diagnostic.
    Unavailable {
        reason: UnavailableReason,
        diagnostic: String,
    },
}

/// Which source a definition's credential would come from, as far as it can
/// be seen from outside it. Every variant is metadata — a name, a path, a
/// shape — because the value itself is the one thing a probe never holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    /// The store holds it — presence only, value NEVER read.
    Secret { name: String },
    /// Declared lanes; NO lane claimed.
    SecretPool { lanes: Vec<String> },
    File { path: PathBuf, shape: SourceKind },
    Env { name: String },
    /// The definition's placeholder token.
    Inline,
}

/// Why nothing usable was found, as a value a caller can branch on. Every
/// arm names something the operator can act on, which is why the file and
/// variable cases are separate arms rather than one "absent".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnavailableReason {
    SecretAbsent,
    NoSecretStore,
    FileMissing,
    FileUnreadable,
    FileEmpty,
    FileMalformed,
    EnvUnset,
    EnvEmpty,
    DriverMissing,
}

impl ProviderDef {
    /// Structural readiness of this one definition (the picker iterates
    /// providers and wants exactly this form). No store value is read, no
    /// lane is claimed, no network, no spawn.
    ///
    /// The order is [`ProviderDef::token`]'s, so the probe and the request
    /// that follows it agree about which source is in play: the single
    /// named secret, the pool, the file, the variable, the inline
    /// placeholder, nothing. A single named secret the store does not hold is
    /// not the answer while another source is declared beside it — the store
    /// is the *preferred* home for a key, not the only one — so a definition
    /// naming both reaches whichever the operator filled in, and the
    /// secret's absence is named only when nothing anywhere held the
    /// credential. A pool does not fall through; see the module docs.
    ///
    /// "Nothing at all declared" is [`Readiness::NoCredentialNeeded`] and
    /// not a failure: that is `Ok(None)` from `token()`, and the request it
    /// precedes goes out with no credential, which is the correct thing to
    /// do against an unauthenticated local endpoint.
    pub fn readiness(&self, store: Option<&SecretStore>) -> Readiness {
        if let Some(name) = &self.token_secret {
            if store.is_some_and(|s| s.has(name)) {
                return Readiness::Present(CredentialSource::Secret { name: name.clone() });
            }
            return self.after_the_store(store, Some(name));
        }
        if !self.token_secrets.is_empty() {
            // Any declared lane will do: a pool is ready when *a* key of it
            // is here, and which lane this session will take is resolution's
            // business, not a probe's — asking properly would mean taking
            // one. No lane present is the pool's own verdict and not a
            // fall-through: the pool *is* this definition's statement about
            // where its key lives (a patch naming one replaces every other
            // source, see `ProviderDef::patched`), so a placeholder the
            // script left beside it is not a key the operator filled in, and
            // "fill one of these lanes" is the sentence that gets a session
            // started. See the module docs for the one place this differs
            // from `token()`.
            if store.is_some_and(|s| self.token_secrets.iter().any(|lane| s.has(lane))) {
                return Readiness::Present(CredentialSource::SecretPool {
                    lanes: self.token_secrets.clone(),
                });
            }
            let named = self.token_secrets.join("`, `");
            return match store {
                Some(_) => Readiness::Unavailable {
                    reason: UnavailableReason::SecretAbsent,
                    diagnostic: format!(
                        "provider `{}`: no lane of its token pool (`{named}`) is in the store; \
                         run `eidolon secret set <lane>`",
                        self.name
                    ),
                },
                // The store's absence, not the lanes': with no store
                // configured no lane could have been found, and saying the
                // secrets are absent would send the operator to look for
                // keys that had nowhere to live.
                None => Readiness::Unavailable {
                    reason: UnavailableReason::NoSecretStore,
                    diagnostic: format!(
                        "provider `{}`: its token pool (`{named}`) lives in the secret store, and \
                         none is configured",
                        self.name
                    ),
                },
            };
        }
        self.after_the_store(store, None)
    }

    /// Everything `token()` reads after the store, in its order: the file,
    /// the variable, the inline placeholder, and — when a single secret was
    /// named and the store did not hold it — the sentence that says so.
    ///
    /// Exactly one of the first three is read, as `token()` reads exactly
    /// one: a definition naming both a file and a variable reaches the file,
    /// and a probe that tried all three would describe a credential the
    /// request will not send.
    ///
    /// `secret` is not a fallback target here; it is the name the definition
    /// asked the store for, kept so the last branch can name it instead of
    /// saying "no credential" about a definition that declared one.
    fn after_the_store(&self, store: Option<&SecretStore>, secret: Option<&str>) -> Readiness {
        if let Some(f) = &self.token_file {
            return match self.file_source(f).probe() {
                SourceState::Present(shape) => Readiness::Present(CredentialSource::File {
                    path: f.clone(),
                    shape,
                }),
                SourceState::Unusable { reason, diagnostic } => Readiness::Unavailable {
                    reason: file_reason(reason),
                    diagnostic: self.attributed(diagnostic),
                },
                // A file source has no such answer; if harnox ever gave one,
                // the truth would be that this definition asks for nothing.
                SourceState::NotRequired => Readiness::NoCredentialNeeded,
            };
        }
        if let Some(name) = &self.token_env {
            return match TokenSource::Env(name.clone()).probe() {
                SourceState::Present(_) => Readiness::Present(CredentialSource::Env {
                    name: name.clone(),
                }),
                SourceState::Unusable { reason, diagnostic } => Readiness::Unavailable {
                    reason: env_reason(reason),
                    diagnostic: self.attributed(diagnostic),
                },
                SourceState::NotRequired => Readiness::NoCredentialNeeded,
            };
        }
        if self.token.is_some() {
            return Readiness::Present(CredentialSource::Inline);
        }
        match secret {
            // The wording is `token()`'s, so the picker's row and the failed
            // turn tell the operator to run the same command.
            Some(name) => match store {
                Some(_) => Readiness::Unavailable {
                    reason: UnavailableReason::SecretAbsent,
                    diagnostic: format!(
                        "provider `{}`: no key for secret `{name}`; run `eidolon secret set {name}`",
                        self.name
                    ),
                },
                None => Readiness::Unavailable {
                    reason: UnavailableReason::NoSecretStore,
                    diagnostic: format!(
                        "provider `{}` needs secret `{name}` but no secret store is configured",
                        self.name
                    ),
                },
            },
            None => Readiness::NoCredentialNeeded,
        }
    }

    /// The source a `token_file` means — the choice `token()` makes, for the
    /// reasons it gives there: the Gemini wire's file is a Google refresh
    /// credential whatever it is called, a `.db`/`.sqlite` is one whatever
    /// the wire (a bare token file is never a SQLite database), the Codex
    /// wire's is a ChatGPT refresh credential, and anything else is the
    /// bearer itself.
    fn file_source(&self, path: &Path) -> TokenSource {
        let google = self.wire == Wire::Gemini
            || path.extension().is_some_and(|e| e == "db" || e == "sqlite");
        if self.wire == Wire::Codex {
            TokenSource::CodexRefresh {
                path: path.to_path_buf(),
            }
        } else if google {
            TokenSource::GoogleRefresh {
                path: path.to_path_buf(),
                client_id: None,
                client_secret: None,
            }
        } else {
            TokenSource::File(path.to_path_buf())
        }
    }

    /// harnox's own sentence with this definition's name in front of it: a
    /// diagnostic travels to a renderer that no longer has the definition in
    /// hand, and "credential file /x does not exist" alone does not say
    /// whose file it is.
    fn attributed(&self, diagnostic: String) -> String {
        format!("provider `{}`: {diagnostic}", self.name)
    }
}

impl Catalog {
    /// `Err` = the spec does not resolve (unknown provider, ambiguous bare
    /// id): a RESOLUTION error, deliberately not a credential one.
    /// `Ok(Readiness::Unavailable{..})` = a credential error. The two never
    /// share a channel.
    ///
    /// Resolution is the ordinary [`Catalog::resolve`] minus its side effect
    /// ([`Catalog::resolve_unclaimed`]), so a key the picker would draw and a
    /// key the session would start on answer the same way — including the
    /// error a bare id served by two providers gets, which is a question for
    /// the operator rather than a credential verdict — while a probe takes no
    /// lane of a token pool: a picker asks this once per row per frame, and a
    /// claim per ask would spread one session over the pool and hold lanes
    /// for processes that never send a request.
    pub fn readiness(&self, spec: &str) -> anyhow::Result<ModelReadiness> {
        Ok(match self.resolve_unclaimed(spec, None)? {
            // The mock is the harness's own scripted provider: it needs no
            // account because it goes nowhere, and a row claiming otherwise
            // would be asking for a key to reach a fixture.
            Resolved::Mock => ModelReadiness {
                key: "mock".into(),
                provider: "mock".into(),
                model: String::new(),
                state: Readiness::NoCredentialNeeded,
            },
            Resolved::ClaudeCli { model, binary, .. } => {
                let model = model.unwrap_or_default();
                let binary = binary.unwrap_or_else(|| "claude".to_string());
                let key = format!("{CLAUDE_CLI}:{model}");
                let state = driver_readiness(&key, &binary);
                ModelReadiness {
                    key,
                    provider: CLAUDE_CLI.into(),
                    model,
                    state,
                }
            }
            Resolved::Http { provider, model, .. } => {
                let name = provider.name();
                // The *declared* definition, not the provider resolution
                // built: a pooled resolution carries a claimed lane as its
                // `token_secret`, and readiness is a question about the
                // definition the lane came out of.
                let def = self
                    .providers()
                    .iter()
                    .find(|p| p.def.name == name)
                    .with_context(|| format!("no provider named `{name}`"))?
                    .def
                    .clone();
                ModelReadiness {
                    key: format!("{}:{model}", def.name),
                    provider: def.name.clone(),
                    model,
                    state: def.readiness(self.store()),
                }
            }
        })
    }
}

/// A driver backend's readiness: is its executable here?
///
/// Executable presence is the whole of what can be said without spawning it
/// (see [`Readiness::DriverExecutable`]): `claude` installed and logged out
/// draws exactly like one installed and logged in, and claiming otherwise
/// would put a reassuring row in front of a turn that cannot start.
fn driver_readiness(key: &str, binary: &str) -> Readiness {
    if on_path(binary) {
        return Readiness::DriverExecutable {
            binary: binary.to_string(),
        };
    }
    // Which of the two lookups failed is the actionable half of the
    // sentence: a configured path that is not there is one typo, a name
    // that is nowhere on PATH is an install.
    let diagnostic = if binary.contains('/') {
        format!("model `{key}`: driver executable `{binary}` does not exist")
    } else {
        format!("model `{key}`: no `{binary}` on PATH")
    };
    Readiness::Unavailable {
        reason: UnavailableReason::DriverMissing,
        diagnostic,
    }
}

/// Is this program there? A name is looked up along `PATH`; anything with a
/// separator in it is a path already and is asked directly — the same test
/// `eidolon-tui`'s launcher makes when it picks a terminal, and for the same
/// reason: a bare name and a path are different questions, and answering the
/// first with the second (or the reverse) finds nothing or the wrong thing.
///
/// Stat only, on purpose: whether the file is *executable* is a claim only a
/// spawn could settle, and this must not spawn.
fn on_path(bin: &str) -> bool {
    if bin.contains('/') {
        return Path::new(bin).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
}

/// harnox's file problem in this crate's words. Total on both sides on
/// purpose: a shape added to one enum is a compile error here rather than a
/// silent "unreadable".
fn file_reason(problem: SourceProblem) -> UnavailableReason {
    match problem {
        SourceProblem::Missing => UnavailableReason::FileMissing,
        SourceProblem::Unreadable => UnavailableReason::FileUnreadable,
        SourceProblem::Empty => UnavailableReason::FileEmpty,
        SourceProblem::Malformed => UnavailableReason::FileMalformed,
    }
}

/// The same for a variable, whose two shapes that cannot occur are paired
/// with the ones that would send the operator to the same place: `Env`
/// answers missing or empty and nothing else (see [`harnox::llm`]), so a
/// variable that cannot be read is not there for this purpose, and one whose
/// content is not a credential is not usable as one.
fn env_reason(problem: SourceProblem) -> UnavailableReason {
    match problem {
        SourceProblem::Missing | SourceProblem::Unreadable => UnavailableReason::EnvUnset,
        SourceProblem::Empty | SourceProblem::Malformed => UnavailableReason::EnvEmpty,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::catalog::ClaudeCliEntry;
    use crate::script::ProviderScript;

    /// A store rooted in a fresh temp directory, holding `names`.
    fn store_with(names: &[(&str, &str)]) -> (tempfile::TempDir, Arc<SecretStore>) {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SecretStore::at(tmp.path().join("secrets")));
        for (name, value) in names {
            store.set(name, value, None, vec![]).unwrap();
        }
        (tmp, store)
    }

    fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    /// The precedence is `token()`'s: the store's answer first, then the
    /// variable beside it — because a key's preferred home is not its only
    /// one — then the file when a definition names both a file and a
    /// variable, and "nothing declared" is no credential rather than a
    /// failure.
    #[test]
    fn the_store_is_preferred_and_what_stands_beside_it_still_answers() {
        let var = "EIDOLON_TEST_READINESS_ENV";
        unsafe { std::env::set_var(var, "sk-env") };
        let (tmp, store) = store_with(&[("readiness-held", "sk-store")]);
        let file = write(tmp.path(), "token", "sk-file\n");

        let def = |secret: Option<&str>, env: Option<&str>, token_file: Option<&Path>| ProviderDef {
            name: "p".into(),
            wire: Wire::OpenAi,
            base_url: "http://x".into(),
            token_file: token_file.map(Path::to_path_buf),
            token_env: env.map(String::from),
            token_secret: secret.map(String::from),
            ..Default::default()
        };

        assert_eq!(
            def(Some("readiness-held"), Some(var), None).readiness(Some(&store)),
            Readiness::Present(CredentialSource::Secret {
                name: "readiness-held".into()
            })
        );
        // The store does not hold the named secret: the variable beside it is
        // the credential, not an error.
        assert_eq!(
            def(Some("nothing-holds-this"), Some(var), None).readiness(Some(&store)),
            Readiness::Present(CredentialSource::Env { name: var.into() })
        );
        // A file and a variable both declared: `token()` reads the file, so
        // the probe reports the file.
        assert_eq!(
            def(None, Some(var), Some(&file)).readiness(Some(&store)),
            Readiness::Present(CredentialSource::File {
                path: file.clone(),
                shape: SourceKind::PlainToken
            })
        );
        // A secret named and nothing beside it, with no store configured.
        assert!(matches!(
            def(Some("readiness-held"), None, None).readiness(None),
            Readiness::Unavailable { reason: UnavailableReason::NoSecretStore, ref diagnostic }
                if diagnostic.contains("run `eidolon secret set`") || diagnostic.contains("readiness-held")
        ));
        // A secret named, a store configured, and the store does not hold it.
        assert!(matches!(
            def(Some("nothing-holds-this"), None, None).readiness(Some(&store)),
            Readiness::Unavailable { reason: UnavailableReason::SecretAbsent, ref diagnostic }
                if diagnostic.contains("nothing-holds-this")
        ));
        // Nothing declared is not a failure.
        assert_eq!(
            def(None, None, None).readiness(Some(&store)),
            Readiness::NoCredentialNeeded
        );
        assert_eq!(
            ProviderDef::default().readiness(None),
            Readiness::NoCredentialNeeded
        );

        unsafe { std::env::remove_var(var) };
    }

    /// An unauthenticated local endpoint declares no source at all, and a
    /// resolved row for it is ready rather than unavailable — the request it
    /// precedes really does go out, with no credential.
    #[test]
    fn an_endpoint_with_no_auth_needs_no_credential() {
        let def = ProviderDef {
            name: "local".into(),
            wire: Wire::OpenAi,
            base_url: "http://127.0.0.1:11434/v1".into(),
            ..Default::default()
        };
        let mut c = Catalog::new();
        c.add_def(def, None, "test");
        let ready = c.readiness("local:any-model").unwrap();
        assert_eq!(ready.key, "local:any-model");
        assert_eq!(ready.provider, "local");
        assert_eq!(ready.model, "any-model");
        assert_eq!(ready.state, Readiness::NoCredentialNeeded);
    }

    /// Every way a file source can fail is named as itself — absent, empty,
    /// unreadable, a directory — because "no credential" is the one sentence
    /// that cannot be acted on.
    #[test]
    fn a_files_problems_are_named_one_by_one() {
        let tmp = tempfile::tempdir().unwrap();
        let def = |path: &Path| ProviderDef {
            name: "p".into(),
            wire: Wire::OpenAi,
            base_url: "http://x".into(),
            token_file: Some(path.to_path_buf()),
            ..Default::default()
        };
        let reason = |path: &Path| match def(path).readiness(None) {
            Readiness::Unavailable { reason, diagnostic } => {
                assert!(
                    diagnostic.contains(&path.display().to_string()),
                    "the diagnostic names the path: {diagnostic}"
                );
                reason
            }
            other => panic!("expected Unavailable, got {other:?}"),
        };

        assert_eq!(reason(&tmp.path().join("absent")), UnavailableReason::FileMissing);
        let empty = write(tmp.path(), "empty", "\n");
        assert_eq!(reason(&empty), UnavailableReason::FileEmpty);
        // A directory is not a credential, and saying so needs no read.
        assert_eq!(reason(tmp.path()), UnavailableReason::FileUnreadable);

        // Mode 000, asserted only where it actually bites — root reads it
        // anyway, and a test that pretended otherwise would fail as root.
        let locked = write(tmp.path(), "locked", "sk-file\n");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read_to_string(&locked).is_err() {
            assert_eq!(reason(&locked), UnavailableReason::FileUnreadable);
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o600)).unwrap();

        // A variable that is not set, and one that is set and blank.
        let var = "EIDOLON_TEST_READINESS_UNSET";
        unsafe { std::env::remove_var(var) };
        let env = |name: &str| ProviderDef {
            name: "p".into(),
            wire: Wire::OpenAi,
            base_url: "http://x".into(),
            token_env: Some(name.into()),
            ..Default::default()
        };
        assert_eq!(
            env(var).readiness(None),
            Readiness::Unavailable {
                reason: UnavailableReason::EnvUnset,
                diagnostic: format!("provider `p`: credential env var {var} is not set")
            }
        );
        unsafe { std::env::set_var(var, "   ") };
        assert_eq!(
            env(var).readiness(None),
            Readiness::Unavailable {
                reason: UnavailableReason::EnvEmpty,
                diagnostic: format!("provider `p`: credential env var {var} is empty")
            }
        );
        unsafe { std::env::remove_var(var) };
    }

    /// A refresh file is present in *shape*, not merely by existing: a
    /// file with no refresh token in it is malformed, which is the
    /// difference between a row that says "ready" and a turn that fails on
    /// the first mint. Both wires, both ways.
    #[test]
    fn a_refresh_file_is_present_only_when_it_holds_a_refresh_token() {
        let tmp = tempfile::tempdir().unwrap();
        let def = |wire: Wire, path: &Path| ProviderDef {
            name: "p".into(),
            wire,
            base_url: "http://x".into(),
            token_file: Some(path.to_path_buf()),
            ..Default::default()
        };

        let google = write(tmp.path(), "creds.json", r#"{"refresh_token":"1//rt","client_id":"c"}"#);
        assert_eq!(
            def(Wire::Gemini, &google).readiness(None),
            Readiness::Present(CredentialSource::File {
                path: google.clone(),
                shape: SourceKind::GoogleRefresh
            })
        );
        // Present, readable, non-empty — and no refresh token: malformed.
        let bare = write(tmp.path(), "bare.json", r#"{"client_id":"x"}"#);
        assert!(matches!(
            def(Wire::Gemini, &bare).readiness(None),
            Readiness::Unavailable { reason: UnavailableReason::FileMalformed, .. }
        ));

        let codex = write(
            tmp.path(),
            "chatgpt.json",
            r#"{"refresh_token":"rt","account_id":"acc_1"}"#,
        );
        assert_eq!(
            def(Wire::Codex, &codex).readiness(None),
            Readiness::Present(CredentialSource::File {
                path: codex.clone(),
                shape: SourceKind::CodexRefresh
            })
        );
        let codex_bare = write(tmp.path(), "chatgpt-bare.json", r#"{"account_id":"acc_1"}"#);
        assert!(matches!(
            def(Wire::Codex, &codex_bare).readiness(None),
            Readiness::Unavailable { reason: UnavailableReason::FileMalformed, .. }
        ));
    }

    /// A custodied secret beside a broken refresh file is ready: the store's
    /// key is the one the request sends, so the file's shape is not the
    /// answer. With the store empty, the file is the source in play and its
    /// own problem is what is named.
    #[test]
    fn a_custodied_secret_outranks_a_broken_refresh_file() {
        let tmp = tempfile::tempdir().unwrap();
        // The store's own directory must outlive the probe: dropping it takes
        // `secrets.enc` with it, and `has` then answers false about a key
        // this test just set.
        let (_store_dir, store) = store_with(&[("readiness-key", "sk-store")]);
        let empty_store = SecretStore::at(tmp.path().join("no-such-store"));
        let broken = write(tmp.path(), "creds.json", r#"{"client_id":"x"}"#);
        let def = |secret: &str| ProviderDef {
            name: "p".into(),
            wire: Wire::Gemini,
            base_url: "http://x".into(),
            token_file: Some(broken.clone()),
            token_secret: Some(secret.into()),
            ..Default::default()
        };

        assert_eq!(
            def("readiness-key").readiness(Some(&store)),
            Readiness::Present(CredentialSource::Secret {
                name: "readiness-key".into()
            })
        );
        assert!(matches!(
            def("readiness-key").readiness(Some(&empty_store)),
            Readiness::Unavailable { reason: UnavailableReason::FileMalformed, ref diagnostic }
                if diagnostic.contains("creds.json")
        ));
    }

    /// A pool is probed without taking a lane: the same definition a
    /// resolution has already claimed from reports the pool, and the
    /// registry's claim directories are exactly as the resolution left them.
    /// A probe that claimed would hold a key on behalf of a session that
    /// asked nothing.
    #[test]
    fn a_pool_is_probed_without_claiming_a_lane() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        // One lane filled, so the pool is ready; which lane is *not* the
        // probe's to decide.
        let (_store_dir, store) = store_with(&[("pool-gw2", "sk-lane2")]);
        let lanes = vec![
            "pool-gw".to_string(),
            "pool-gw2".to_string(),
            "pool-gw3".to_string(),
        ];

        let mut c = Catalog::new().with_store(store.clone());
        let src = r#"pub fn provider() { #{ name: "pool-gw", wire: "openai", base_url: "http://x", token: "inline", token_secrets: ["pool-gw", "pool-gw2", "pool-gw3"], models: [ #{ id: "m" } ] } }"#;
        let (def, script) = ProviderScript::load("pool-gw", src).unwrap();
        assert_eq!(def.token_secrets, lanes);
        c.add_def(def, Some(script), "test");

        let holder = eidolon_swarm::Presence::register(
            &root,
            &tmp.path().join("a.eid"),
            &root,
            "mock",
            "a",
        )
        .unwrap();
        let c = c.with_keypool(root.clone(), holder.id());

        // Resolution claims a lane — that is its job, and the backend it hands
        // out is the one the session will send with.
        let Resolved::Http { claim: Some(claimed), .. } = c.resolve("pool-gw:m", None).unwrap() else {
            panic!("a pooled resolution claims a lane")
        };
        assert_eq!(claimed.secret, "pool-gw");

        let claimed_lanes = |root: &Path| -> Vec<String> {
            let dir = root.join(eidolon_swarm::keypool::POOL_DIR).join("pool-gw");
            let mut rows: Vec<String> = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.filter_map(Result::ok)
                        .map(|e| {
                            let holder = std::fs::read_to_string(e.path().join("holder"))
                                .unwrap_or_default()
                                .trim()
                                .to_string();
                            format!("{}={holder}", e.file_name().to_string_lossy())
                        })
                        .collect()
                })
                .unwrap_or_default();
            rows.sort();
            rows
        };
        let before = claimed_lanes(&root);
        assert_eq!(before.len(), 1, "the resolution holds one lane: {before:?}");

        // The probe: the declared definition, as a picker would read it.
        let declared = c
            .providers()
            .iter()
            .find(|p| p.def.name == "pool-gw")
            .unwrap()
            .def
            .clone();
        assert_eq!(
            declared.readiness(Some(&store)),
            Readiness::Present(CredentialSource::SecretPool {
                lanes: lanes.clone()
            })
        );
        assert_eq!(
            claimed_lanes(&root),
            before,
            "probing a pool must not claim, release or rewrite a lane"
        );

        // With no lane filled the pool is unavailable, and the lanes are
        // named: the operator is told which secrets to fill, not that the
        // provider is a mystery. The placeholder token the script left beside
        // the pool does not stand in for a lane — see the module docs on the
        // one place this knowingly differs from `token()`.
        let empty = SecretStore::at(tmp.path().join("empty-store"));
        let Readiness::Unavailable { reason, diagnostic } = declared.readiness(Some(&empty)) else {
            panic!("a pool no lane of which is filled is unavailable")
        };
        assert_eq!(reason, UnavailableReason::SecretAbsent);
        for lane in &lanes {
            assert!(diagnostic.contains(lane), "{diagnostic}");
        }
        // And with no store configured at all, that is what is said.
        assert!(matches!(
            declared.readiness(None),
            Readiness::Unavailable { reason: UnavailableReason::NoSecretStore, .. }
        ));
    }

    /// The same property through the public API the picker actually calls: a
    /// `Catalog::readiness` answer claims nothing, proven by a *second*
    /// holder still finding the first lane free afterwards — it would land on
    /// the second lane if the probe had quietly held the first, and a probe
    /// that takes a lane is a bug the operator meets as their sessions
    /// spreading over the pool.
    #[test]
    fn a_catalog_probe_holds_no_lane_of_a_pool() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("run");
        let (_store_dir, store) = store_with(&[("pool-gw2", "sk-lane2")]);
        let lanes = vec![
            "pool-gw".to_string(),
            "pool-gw2".to_string(),
            "pool-gw3".to_string(),
        ];
        let src = r#"pub fn provider() { #{ name: "pool-gw", wire: "openai", base_url: "http://x", token: "inline", token_secrets: ["pool-gw", "pool-gw2", "pool-gw3"], models: [ #{ id: "m" } ] } }"#;
        // A catalog per holder, since the registry root and the holder are
        // what a catalog is built with.
        let catalog = |holder: &str| -> Catalog {
            let mut c = Catalog::new().with_store(store.clone());
            let (def, script) = ProviderScript::load("pool-gw", src).unwrap();
            c.add_def(def, Some(script), "test");
            c.with_keypool(root.clone(), holder)
        };
        let register = |name: &str| {
            eidolon_swarm::Presence::register(
                &root,
                &tmp.path().join(format!("{name}.eid")),
                &root,
                "mock",
                name,
            )
            .unwrap()
        };

        let first = register("a");
        let ready = catalog(first.id()).readiness("pool-gw:m").unwrap();
        assert_eq!(ready.key, "pool-gw:m");
        assert_eq!(
            ready.state,
            Readiness::Present(CredentialSource::SecretPool {
                lanes: lanes.clone()
            }),
            "the probe answers from the declared lanes"
        );

        // A second, different holder resolving for real takes the first lane
        // — untouched, because nothing above held it.
        let second = register("b");
        let Resolved::Http { claim: Some(claimed), .. } = catalog(second.id()).resolve("pool-gw:m", None).unwrap() else {
            panic!("a real resolution claims a lane")
        };
        assert_eq!(
            claimed.secret, "pool-gw",
            "the probe left the first lane free for the next session"
        );
    }

    /// A driver backend is judged by its executable and nothing else: an
    /// absent one is missing, one on `PATH` is present, and presence is not
    /// a login — the state says only what can be seen without starting it.
    #[test]
    fn a_driver_backend_is_probed_by_its_executable_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let catalog = |binary: &str| {
            Catalog::new().with_claude(Some(ClaudeCliEntry {
                binary: Some(binary.into()),
                models: vec!["sonnet".into()],
                tools: Default::default(),
            }))
        };

        let absent = tmp.path().join("no-such-claude");
        let ready = catalog(&absent.display().to_string())
            .readiness("claude-cli:sonnet")
            .unwrap();
        assert_eq!(ready.key, "claude-cli:sonnet");
        assert_eq!(ready.provider, "claude-cli");
        assert_eq!(ready.model, "sonnet");
        assert!(
            matches!(
                ready.state,
                Readiness::Unavailable { reason: UnavailableReason::DriverMissing, ref diagnostic }
                    if diagnostic.contains(&absent.display().to_string())
            ),
            "{:?}",
            ready.state
        );

        // A bare name is looked up along `PATH`, never in the working
        // directory — a `claude` beside the repo is not an installed CLI.
        let named = catalog("eidolon-test-driver-that-is-not-installed")
            .readiness("sonnet")
            .unwrap();
        assert_eq!(named.key, "claude-cli:sonnet", "a bare id resolves there too");
        assert!(matches!(
            named.state,
            Readiness::Unavailable { reason: UnavailableReason::DriverMissing, ref diagnostic }
                if diagnostic.contains("on PATH")
        ));

        let real = write(tmp.path(), "claude", "#!/bin/sh\n");
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            catalog(&real.display().to_string())
                .readiness("claude-cli:sonnet")
                .unwrap()
                .state,
            Readiness::DriverExecutable {
                binary: real.display().to_string()
            }
        );
    }

    /// The mock needs nothing, and says so through the catalog's own form.
    #[test]
    fn the_mock_needs_no_credential() {
        let ready = Catalog::new().readiness("mock").unwrap();
        assert_eq!(ready.key, "mock");
        assert_eq!(ready.provider, "mock");
        assert_eq!(ready.model, "");
        assert_eq!(ready.state, Readiness::NoCredentialNeeded);
    }

    /// A resolvable spec is never an error and an unresolvable one never
    /// borrows the credential channel: a caller printing `Err` for a typo
    /// must not be printing a sentence about a key.
    #[test]
    fn resolution_errors_and_credential_errors_never_share_a_channel() {
        let tmp = tempfile::tempdir().unwrap();
        let mut c = Catalog::new();
        c.add_def(
            ProviderDef {
                name: "p".into(),
                wire: Wire::OpenAi,
                base_url: "http://x".into(),
                token_file: Some(tmp.path().join("nowhere")),
                ..Default::default()
            },
            None,
            "test",
        );
        let e = match c.readiness("nope:gpt") {
            Err(e) => e.to_string(),
            Ok(_) => panic!("an unknown provider is a resolution error"),
        };
        assert!(!e.contains("credential"), "{e}");
        assert!(matches!(
            c.readiness("p:any-model").unwrap().state,
            Readiness::Unavailable { reason: UnavailableReason::FileMissing, .. }
        ));
    }

    /// Nothing in a diagnostic is a byte of the credential, whether the
    /// state is present or unavailable: the file that holds a secret is
    /// named by its path, and a parse failure reports the shape it expected
    /// rather than the text it read.
    #[test]
    fn a_diagnostic_carries_no_byte_of_the_credential() {
        const SENTINEL: &str = "SUPERSECRET-readiness-sentinel";
        let tmp = tempfile::tempdir().unwrap();

        let plain = write(tmp.path(), "token", &format!("{SENTINEL}\n"));
        let present = ProviderDef {
            name: "p".into(),
            wire: Wire::OpenAi,
            base_url: "http://x".into(),
            token_file: Some(plain),
            ..Default::default()
        }
        .readiness(None);
        assert!(matches!(present, Readiness::Present(_)), "{present:?}");
        assert!(!format!("{present:?}").contains(SENTINEL));

        // The sentinel in the *contents* of a file whose shape is wrong: a
        // parse diagnostic is where a careless message would print it.
        let broken = write(tmp.path(), "creds.json", &format!(r#"{{"client_id":"{SENTINEL}"}}"#));
        let def = ProviderDef {
            name: "p".into(),
            wire: Wire::Gemini,
            base_url: "http://x".into(),
            token_file: Some(broken),
            ..Default::default()
        };
        let state = def.readiness(None);
        assert!(
            matches!(state, Readiness::Unavailable { reason: UnavailableReason::FileMalformed, .. }),
            "{state:?}"
        );
        assert!(!format!("{state:?}").contains(SENTINEL), "{state:?}");

        // And through the catalog's form, which carries identity beside it.
        let mut c = Catalog::new();
        c.add_def(def, None, "test");
        let ready = c.readiness("p:any-model").unwrap();
        assert!(!format!("{ready:?}").contains(SENTINEL), "{ready:?}");
        assert!(format!("{ready:?}").contains("any-model"));
    }
}

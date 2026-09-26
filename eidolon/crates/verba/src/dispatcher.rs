//! The classifier child and its line protocol. A port of Melete's
//! `verba.rs` onto `tokio::process`.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// One classification, as `verba-volantia dispatch` prints it.
///
/// `Default` is for tests and for a verdict that never arrived: an empty one
/// abstains, which is the safe direction and the honest one.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct Verdict {
    pub utterance: String,
    /// The delexicalised form classified (`read the note $a`).
    pub delex: String,
    /// A function name, or `none` / `UNK`.
    pub intent: String,
    pub intent_prob: f64,
    pub margin: f64,
    pub threshold: Option<f64>,
    pub accept: Option<bool>,
    /// Corpus slot name → value. A `Value` and not a `String` because the
    /// surface has list-valued slots (`refs`, `subtract_refs` arrive as JSON
    /// arrays) and a typed map is the only shape that decodes every verdict
    /// the binary can print — a `Vec` under a `String` map fails the whole
    /// line, which the dispatcher reads as a dead child.
    #[serde(default)]
    pub slots: BTreeMap<String, Value>,
    #[serde(default)]
    pub candidates: Vec<IntentScore>,
    /// Slot bindings the classifier itself found contradictory. Non-empty
    /// means **do not dispatch**: the model's own two readings of the line
    /// disagree, which is the one case a fast local resolver must not guess
    /// through. Untyped on purpose — the channel only ever asks whether it is
    /// empty, and v1 does not want to grow a parser for a shape the binary is
    /// still free to change.
    #[serde(default)]
    pub conflicts: Vec<serde_json::Value>,
    /// Text in the command that no slot claimed — a second instruction, a
    /// remark, a question. Non-null means **do not dispatch**: executing the
    /// first clause while silently dropping the rest is the failure mode this
    /// field exists to prevent. (`None` and an empty string are both "nothing
    /// left over"; the gate treats them the same.)
    #[serde(default)]
    pub trailing_editorial_text: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct IntentScore {
    pub intent: String,
    pub score: f64,
}

impl Verdict {
    /// Calibrated accept on a real intent. Everything else abstains.
    pub fn accepted(&self) -> bool {
        self.accept == Some(true) && self.intent != "none" && self.intent != "UNK"
    }
}

/// Per-request ceiling. A warm child answers in milliseconds; a cold one
/// loads weights in ~150ms. Hitting this means it is wedged: tear down,
/// respawn next call.
const CLASSIFY_TIMEOUT: Duration = Duration::from_secs(5);

struct Child {
    proc: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
}

pub struct VerbaDispatcher {
    binary: String,
    weights_dir: String,
    child: Mutex<Option<Child>>,
    timeout: Duration,
}

impl VerbaDispatcher {
    pub fn new(binary: impl Into<String>, weights_dir: impl Into<String>) -> Self {
        VerbaDispatcher {
            binary: binary.into(),
            weights_dir: weights_dir.into(),
            child: Mutex::new(None),
            timeout: CLASSIFY_TIMEOUT,
        }
    }

    pub fn weights_dir(&self) -> &str {
        &self.weights_dir
    }

    fn spawn(&self) -> anyhow::Result<Child> {
        let mut proc = tokio::process::Command::new(&self.binary)
            .args(["dispatch", "--out", &self.weights_dir])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("spawning {} (the verba-volantia binary)", self.binary))?;
        let stdin = proc.stdin.take().context("verba stdin")?;
        let stdout = BufReader::new(proc.stdout.take().context("verba stdout")?);
        Ok(Child {
            proc,
            stdin,
            stdout,
        })
    }

    /// Classify one utterance. Respawns a dead child once.
    pub async fn classify(&self, utterance: &str) -> anyhow::Result<Verdict> {
        let line = sanitize(utterance)?;
        let mut slot = self.child.lock().await;
        for attempt in 0..2 {
            let alive = match slot.as_mut() {
                Some(c) => c.proc.try_wait().map(|s| s.is_none()).unwrap_or(false),
                None => false,
            };
            if !alive {
                *slot = Some(self.spawn()?);
            }
            let child = slot.as_mut().expect("spawned above");
            if let Err(e) = child.stdin.write_all(format!("{line}\n").as_bytes()).await {
                *slot = None;
                if attempt == 0 {
                    tracing::debug!(error = %e, "verba write failed; respawning");
                    continue;
                }
                return Err(e).context("writing to verba dispatch");
            }
            match tokio::time::timeout(self.timeout, read_verdict(&mut child.stdout)).await {
                Ok(Ok(Some(v))) => return Ok(v),
                Ok(Ok(None)) => {
                    *slot = None;
                    if attempt == 0 {
                        continue;
                    }
                    bail!("verba dispatch closed stdout");
                }
                Ok(Err(e)) => {
                    *slot = None;
                    return Err(e);
                }
                Err(_) => {
                    *slot = None;
                    bail!(
                        "verba dispatch produced no verdict within {:?}",
                        self.timeout
                    );
                }
            }
        }
        unreachable!("the second attempt always returns")
    }
}

async fn read_verdict(
    out: &mut BufReader<tokio::process::ChildStdout>,
) -> anyhow::Result<Option<Verdict>> {
    let mut l = String::new();
    loop {
        l.clear();
        let n = out
            .read_line(&mut l)
            .await
            .context("reading from verba dispatch")?;
        if n == 0 {
            return Ok(None);
        }
        if let Ok(v) = serde_json::from_str::<Verdict>(l.trim()) {
            return Ok(Some(v));
        }
    }
}

fn sanitize(text: &str) -> anyhow::Result<String> {
    let s: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() {
        bail!("empty utterance");
    }
    if !s.chars().any(char::is_alphanumeric) {
        bail!("utterance has no classifiable tokens");
    }
    Ok(s)
}

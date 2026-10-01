//! `vv_gate_eval` — the real consumer gate, driven over recorded verdicts.
//!
//! ```text
//! stdin  (JSONL) {"id": <string|number>, "verdict": {<wire verdict>}, "blocks": {"a": "<bytes>"}}
//! stdout (JSONL) {"id": …, "gate": "<Gate::as_str()>", "call": {"function": …, "args": {…}} | null}
//! ```
//!
//! This is the smallest honest adapter between an independently frozen
//! verdict/gold layer and the **consumer's own decision**. It calls
//! [`eidolon_verba::command::gate`] — the decision itself — and nothing else:
//! no dispatcher, no tool, no spawn, no network, no execution, no clock. `gate`
//! is pure and synchronous, so a run is exactly as reproducible as its input
//! file. (The channel reaches that decision through `gate_command`, which
//! refuses an unreadable payload *before* the verdict is consulted at all — see
//! what this cannot decide, below.)
//!
//! `blocks` is **supplied data**: the parsed form (`letter → payload bytes`)
//! that [`gate`] itself takes. The channel's `!!a` fence scanner — `marked`,
//! `marked_block`, `Command`, `tool_command`, all private — is **not**
//! exercised here, and a green run says nothing about it.
//!
//! What this does not decide, and cannot:
//!
//! * the payload-shape refusal: since 2026-09-29 a hole fenced twice, or a body
//!   carrying a line that is exactly `!!`, is refused by `gate_command` ahead of
//!   any verdict — and that path is crate-private, so a `payload_shape` refusal
//!   is invisible from here, exactly as the fence scanner is (below). A green
//!   run is not coverage of the refusals that never consult a verdict;
//! * `eidolon_core::policy::Approval` and the dispatcher's own decision (the
//!   `vv` tool declares `Approval::Mutating`) — those need an attached
//!   [`Dispatcher`](eidolon_core::dispatch::Dispatcher) and a real call;
//! * execution: no vault is contacted, so no server outcome is measured and a
//!   `pass` is a **consumer decision, not a certified call**;
//! * anything about a checkpoint: the verdicts are inputs. Where they came
//!   from — which checkpoint, which run — is the caller's provenance, not this
//!   program's claim.
//!
//! It reads no gold and writes no verdict. The caller joins this output to
//! independently frozen expectations by `id` (and whatever source hash its own
//! run manifest recorded); nothing here compares, scores or judges.
//!
//! Input discipline. Every line must parse and validate; the first bad line is
//! reported with its line number and the run exits nonzero. No line is silently
//! skipped (blank lines are skipped explicitly, and carry no request). All
//! parsing and validation happens before any output, so a malformed input file
//! cannot produce a partial result that looks complete.
//!
//! Confirm insertion is *observed*, not modelled: for the one intent the gate
//! confirms (`delete_note`), the `confirm: true` argument appears in `call.args`
//! because that is what the gate returned.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufRead, Write};

use anyhow::{Context, Result, anyhow, bail};
use eidolon_verba::command::gate;
use eidolon_verba::dispatcher::Verdict;
use serde_json::{Value, json};

/// One validated request line.
struct Request {
    id: Value,
    verdict: Verdict,
    blocks: BTreeMap<char, String>,
}

fn main() -> std::process::ExitCode {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());
    match run(stdin.lock(), &mut out) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("vv_gate_eval: {err:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// Read every request, then emit one verdict per request.
///
/// The two phases are the point: validation failures are reported before a
/// single output line exists.
fn run<R: BufRead, W: Write>(reader: R, out: &mut W) -> Result<()> {
    let mut requests: Vec<Request> = Vec::new();
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for (i, line) in reader.lines().enumerate() {
        let n = i + 1;
        let line = line.with_context(|| format!("line {n}: cannot read this line"))?;
        if line.trim().is_empty() {
            continue; // a blank line carries no request
        }
        let request = parse_line(&line, n)?;
        let key = serde_json::to_string(&request.id).unwrap_or_else(|_| "<?>".into());
        if !ids.insert(key.clone()) {
            bail!("line {n}: duplicate id {key} — one id, one request");
        }
        requests.push(request);
    }
    for request in &requests {
        emit(request, out)?;
    }
    out.flush().context("cannot flush stdout")
}

fn parse_line(line: &str, n: usize) -> Result<Request> {
    let value: Value = serde_json::from_str(line)
        .with_context(|| format!("line {n}: not a JSON object"))?;
    let object = value
        .as_object()
        .ok_or_else(|| anyhow!("line {n}: the envelope must be a JSON object"))?;
    let id = object
        .get("id")
        .cloned()
        .ok_or_else(|| anyhow!("line {n}: the envelope needs an `id`"))?;
    if !(id.is_string() || id.is_number()) {
        bail!("line {n}: `id` must be a string or a number, got {}", kind_of(&id));
    }
    let verdict_value = object
        .get("verdict")
        .ok_or_else(|| anyhow!("line {n}: the envelope needs a `verdict`"))?;
    let verdict: Verdict = serde_json::from_value(verdict_value.clone()).with_context(|| {
        format!(
            "line {n}: the verdict does not decode — the wire's required fields are \
             utterance, delex, intent, intent_prob and margin (a missing `accept` is \
             not a decode error: it reads as no acceptance, which the gate labels an \
             abstention)"
        )
    })?;
    let blocks = parse_blocks(object.get("blocks"), n)?;
    Ok(Request { id, verdict, blocks })
}

/// `{"a": "bytes"}` → `{'a': "bytes"}`. A key is the hole letter the line named,
/// so it must be the one character `gate` reads back out of a `$a` slot.
fn parse_blocks(raw: Option<&Value>, n: usize) -> Result<BTreeMap<char, String>> {
    let Some(raw) = raw else {
        return Ok(BTreeMap::new());
    };
    let map = raw
        .as_object()
        .ok_or_else(|| anyhow!("line {n}: `blocks` must be an object of letter → bytes"))?;
    let mut blocks = BTreeMap::new();
    for (key, value) in map {
        let mut chars = key.chars();
        let letter = match (chars.next(), chars.next()) {
            (Some(c), None) if c.is_ascii_alphanumeric() => c,
            _ => bail!(
                "line {n}: block key {key:?} must be one ASCII letter or digit — the hole \
                 letter a `$a` slot names"
            ),
        };
        let bytes = value.as_str().ok_or_else(|| {
            anyhow!("line {n}: block {key:?} must be a JSON string of payload bytes")
        })?;
        blocks.insert(letter, bytes.to_string());
    }
    Ok(blocks)
}

fn emit<W: Write>(request: &Request, out: &mut W) -> Result<()> {
    let decision = gate(&request.verdict, &request.blocks);
    let call = decision.call.map(|(function, args)| {
        json!({ "function": function, "args": Value::Object(args) })
    });
    let line = json!({
        "id": request.id,
        "gate": decision.gate.as_str(),
        "call": call,
    });
    writeln!(out, "{line}").context("cannot write a result line")
}

fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Run the adapter over an input string; the output lines, or the error.
    fn eval(input: &str) -> Result<Vec<Value>> {
        let mut out: Vec<u8> = Vec::new();
        run(Cursor::new(input.as_bytes()), &mut out)?;
        let text = String::from_utf8(out).expect("utf-8 output");
        Ok(text
            .lines()
            .map(|l| serde_json::from_str(l).expect("one JSON object per line"))
            .collect())
    }

    fn verdict(intent: &str, accept: bool, slots: Value) -> Value {
        json!({
            "utterance": "…",
            "delex": "…",
            "intent": intent,
            "intent_prob": 0.9,
            "margin": 0.5,
            "threshold": 0.3,
            "accept": accept,
            "slots": slots,
        })
    }

    #[test]
    fn a_read_passes_and_is_reconciled_to_mnemes_names() {
        let input = json!({
            "id": "r1",
            "verdict": verdict("read_note", true, json!({"note": "Groceries"})),
        })
        .to_string();
        let out = eval(&input).expect("clean input");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["id"], json!("r1"));
        assert_eq!(out[0]["gate"], json!("pass"));
        assert_eq!(out[0]["call"]["function"], json!("read_note"));
        assert_eq!(out[0]["call"]["args"]["title"], json!("Groceries"));
        assert!(out[0]["call"]["args"].get("note").is_none(), "the corpus name is gone");
    }

    #[test]
    fn accept_is_consulted_before_conflicts() {
        // `accept: false` wins even with conflicts present — the gate's own
        // precedence, and the labelling the harvest counts.
        let mut v = verdict("read_note", false, json!({"note": "Groceries"}));
        v["conflicts"] = json!(["note"]);
        let out = eval(&json!({"id": 1, "verdict": v}).to_string()).expect("clean input");
        assert_eq!(out[0]["gate"], json!("abstain"));
        assert_eq!(out[0]["call"], Value::Null);

        // …and with accept true the conflict is the label.
        let mut v = verdict("read_note", true, json!({"note": "Groceries"}));
        v["conflicts"] = json!(["note"]);
        let out = eval(&json!({"id": 2, "verdict": v}).to_string()).expect("clean input");
        assert_eq!(out[0]["gate"], json!("conflict"));

        // `none` is not a command, whatever the margin said.
        let out = eval(&json!({"id": 3, "verdict": verdict("none", true, json!({}))}).to_string())
            .expect("clean input");
        assert_eq!(out[0]["gate"], json!("abstain"));
    }

    #[test]
    fn a_payload_must_be_carried_and_the_blocks_must_match() {
        let envelope = |slots: Value, blocks: Option<Value>| {
            let mut e = json!({"id": "w1", "verdict": verdict("append_to_note", true, slots)});
            if let Some(blocks) = blocks {
                e["blocks"] = blocks;
            }
            e.to_string()
        };
        // Inferred words: the tagger decided the payload. Refused.
        let out = eval(&envelope(json!({"note": "Log", "content": "some words"}), None))
            .expect("clean input");
        assert_eq!(out[0]["gate"], json!("payload_undeclared"));
        // Declared as `$a`, no block supplied. Refused.
        let out = eval(&envelope(json!({"note": "Log", "content": "$a"}), None))
            .expect("clean input");
        assert_eq!(out[0]["gate"], json!("payload_unbound"));
        // Declared and carried: dispatched.
        let out = eval(&envelope(
            json!({"note": "Log", "content": "$a"}),
            Some(json!({"a": "carried"})),
        ))
        .expect("clean input");
        assert_eq!(out[0]["gate"], json!("pass"));
        assert_eq!(out[0]["call"]["args"]["content"], json!("carried"));
        // A block nothing asked for — on a read, and as an extra on a write.
        let out = eval(&json!({
            "id": "r2",
            "verdict": verdict("read_note", true, json!({"note": "Log"})),
            "blocks": {"a": "orphan"},
        })
        .to_string())
        .expect("clean input");
        assert_eq!(out[0]["gate"], json!("block_orphan"));
        let out = eval(&envelope(
            json!({"note": "Log", "content": "$a"}),
            Some(json!({"a": "carried", "b": "extra"})),
        ))
        .expect("clean input");
        assert_eq!(out[0]["gate"], json!("block_orphan"));
        // The same letter on two declared holes cannot be two payloads.
        let out = eval(&json!({
            "id": "w2",
            "verdict": verdict(
                "edit_note",
                true,
                json!({"note": "Log", "target": "$a", "content": "$a"}),
            ),
            "blocks": {"a": "one block"},
        })
        .to_string())
        .expect("clean input");
        assert_eq!(out[0]["gate"], json!("payload_unbound"));
    }

    #[test]
    fn payload_bytes_arrive_exactly() {
        // Newlines, quotes, a backslash and a tab: the bytes are the payload,
        // byte for byte, and only the JSON string encoding carries them here.
        let payload = "first line\nhe said \"stop\" \\ and a tab\tend";
        let input = json!({
            "id": "b1",
            "verdict": verdict("append_to_note", true, json!({"note": "Log", "content": "$a"})),
            "blocks": {"a": payload},
        })
        .to_string();
        let out = eval(&input).expect("clean input");
        assert_eq!(out[0]["call"]["args"]["content"], json!(payload));
        assert_eq!(
            out[0]["call"]["args"]["content"].as_str().expect("a string"),
            payload
        );
    }

    #[test]
    fn typed_args_and_list_order() {
        // `similar_notes.k` is a uint32 on the wire.
        let out = eval(&json!({
            "id": "t1",
            "verdict": verdict("similar_notes", true, json!({"ref": "Log", "k": "5"})),
        })
        .to_string())
        .expect("clean input");
        assert_eq!(out[0]["call"]["args"]["k"], json!(5));
        assert!(out[0]["call"]["args"]["k"].is_number());

        // `edit_note.replace_all` is a bool.
        let out = eval(&json!({
            "id": "t2",
            "verdict": verdict(
                "edit_note",
                true,
                json!({"note": "Log", "target": "$a", "content": "$b", "replace_all": "true"}),
            ),
            "blocks": {"a": "old", "b": "new"},
        })
        .to_string())
        .expect("clean input");
        assert_eq!(out[0]["gate"], json!("pass"));
        assert_eq!(out[0]["call"]["args"]["replace_all"], json!(true));
        assert!(out[0]["call"]["args"]["replace_all"].is_boolean());
        assert_eq!(out[0]["call"]["args"]["old_str"], json!("old"));
        assert_eq!(out[0]["call"]["args"]["new_str"], json!("new"));

        // A list-valued slot keeps the order the verdict bound, and an
        // unlisted intent passes its slots through unchanged.
        let out = eval(&json!({
            "id": "t3",
            "verdict": verdict(
                "semantic_centroid",
                true,
                json!({"refs": ["$a", "$b", "$a"]}),
            ),
        })
        .to_string())
        .expect("clean input");
        assert_eq!(out[0]["call"]["args"]["refs"], json!(["$a", "$b", "$a"]));

        // `delete_note` is confirmed by the gate itself — observed in the args,
        // never restated here.
        let out = eval(&json!({
            "id": "t4",
            "verdict": verdict("delete_note", true, json!({"note": "Log"})),
        })
        .to_string())
        .expect("clean input");
        assert_eq!(out[0]["gate"], json!("pass"));
        assert_eq!(out[0]["call"]["args"]["confirm"], json!(true));
    }

    #[test]
    fn malformed_input_fails_with_a_line_number() {
        let good = json!({
            "id": "ok",
            "verdict": verdict("read_note", true, json!({"note": "Log"})),
        })
        .to_string();

        // Not JSON at all.
        let err = eval(&format!("{good}\nnot json\n")).expect_err("must fail");
        assert!(format!("{err:#}").contains("line 2"), "{err:#}");
        // Missing `id`.
        let err = eval(&json!({"verdict": verdict("read_note", true, json!({}))}).to_string())
            .expect_err("must fail");
        assert!(format!("{err:#}").contains("line 1"), "{err:#}");
        // Duplicate id, reported with both lines.
        let err = eval(&format!("{good}\n{good}\n")).expect_err("must fail");
        let text = format!("{err:#}");
        assert!(text.contains("line 2") && text.contains("duplicate id"), "{text}");
        // A verdict that does not decode: `margin` is one of the five fields the
        // crate's own test pins as load-bearing.
        let mut v = verdict("read_note", true, json!({"note": "Log"}));
        v.as_object_mut().expect("object").remove("margin");
        let err = eval(&json!({"id": "x", "verdict": v}).to_string()).expect_err("must fail");
        assert!(format!("{err:#}").contains("line 1"), "{err:#}");
        // …while a missing `accept` is not a decode failure: it is no acceptance,
        // and the gate calls that an abstention. Worth pinning, because a silent
        // `accept` default is exactly how a command could be swallowed.
        let mut v = verdict("read_note", true, json!({"note": "Log"}));
        v.as_object_mut().expect("object").remove("accept");
        let out = eval(&json!({"id": "x", "verdict": v}).to_string()).expect("decodes");
        assert_eq!(out[0]["gate"], json!("abstain"));
        // A block key that is not one letter.
        let err = eval(&json!({
            "id": "x",
            "verdict": verdict("append_to_note", true, json!({"note": "L", "content": "$a"})),
            "blocks": {"aa": "bytes"},
        })
        .to_string())
        .expect_err("must fail");
        assert!(format!("{err:#}").contains("block key"), "{err:#}");
        // Blocks that are not strings.
        let err = eval(&json!({
            "id": "x",
            "verdict": verdict("append_to_note", true, json!({"note": "L", "content": "$a"})),
            "blocks": {"a": 7},
        })
        .to_string())
        .expect_err("must fail");
        assert!(format!("{err:#}").contains("payload bytes"), "{err:#}");
        // `blocks` that is not an object.
        let err = eval(&json!({
            "id": "x",
            "verdict": verdict("read_note", true, json!({"note": "L"})),
            "blocks": ["a"],
        })
        .to_string())
        .expect_err("must fail");
        assert!(format!("{err:#}").contains("line 1"), "{err:#}");
    }

    #[test]
    fn nothing_is_emitted_when_any_line_is_bad() {
        let good = json!({
            "id": "ok",
            "verdict": verdict("read_note", true, json!({"note": "Log"})),
        })
        .to_string();
        let mut out: Vec<u8> = Vec::new();
        let err = run(Cursor::new(format!("{good}\n{{}}\n").as_bytes()), &mut out)
            .expect_err("must fail");
        assert!(format!("{err:#}").contains("line 2"), "{err:#}");
        assert!(out.is_empty(), "no partial output: {:?}", String::from_utf8_lossy(&out));
    }

    #[test]
    fn blank_lines_carry_no_request_and_ids_may_be_numbers() {
        let input = format!(
            "\n{}\n   \n{}\n",
            json!({"id": 1, "verdict": verdict("read_note", true, json!({"note": "A"}))}),
            json!({"id": 2, "verdict": verdict("read_note", true, json!({"note": "B"}))}),
        );
        let out = eval(&input).expect("clean input");
        assert_eq!(out.len(), 2, "one result per request, blank lines ignored");
        assert_eq!(out[0]["id"], json!(1));
        assert_eq!(out[1]["call"]["args"]["title"], json!("B"));
    }
}

//! Terminal plumbing for the headless CLI: an event printer and a stdin
//! `UserIo`. Nothing here is the TUI; it is the minimum a consumer needs.

use async_trait::async_trait;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use eidolon_core::UsageExt;
use eidolon_core::event::Event;
use eidolon_core::user::{Choice, UserIo};

/// One line describing a record, for `:log`, `:tree` and `eidolon log`.
pub fn record_line(r: &eidolon_core::session::Record) -> String {
    use eidolon_core::session::RecordKind::*;
    match &r.kind {
        SessionStart { model, cwd, .. } => format!("start model={model} cwd={cwd}"),
        // An attached image is named. `eidolon log` is the answer to "what
        // is actually on this branch", and a message that arrived with a
        // screenshot reading exactly like one that did not is the log
        // being quietly lossy about the thing hardest to reconstruct.
        UserMessage(m) => {
            let images: Vec<String> = m
                .images()
                .map(|(_, alt)| alt.unwrap_or("image").to_string())
                .collect();
            match images.is_empty() {
                true => format!("user: {}", preview(&m.text())),
                false => format!("user [{}]: {}", images.join(", "), preview(&m.text())),
            }
        }
        AssistantMessage(m) => {
            let tools: Vec<String> = m.tool_uses().map(|(_, n, _)| n.to_string()).collect();
            if tools.is_empty() {
                format!("assistant: {}", preview(&m.text()))
            } else {
                format!("assistant: {} [{}]", preview(&m.text()), tools.join(", "))
            }
        }
        ToolResult {
            tool_use_id,
            content,
            is_error,
        } => format!(
            "result [{tool_use_id}]{}: {}",
            if *is_error { " ERROR" } else { "" },
            preview(content)
        ),
        TurnSettled { stop_reason, usage } => {
            // `cached=` only when there were hits: the dogfood over the
            // token pool reads per-lane hit rates straight out of this
            // line, and a `cached=0` on every cold turn would bury the
            // signal in noise the journal does not actually contain.
            let mut s = format!(
                "settled {stop_reason:?} in={} out={}",
                eidolon_core::usage::human(usage.total_input()),
                eidolon_core::usage::human(usage.output_tokens)
            );
            if usage.cache_read_input_tokens > 0 {
                s.push_str(&format!(
                    " cached={}",
                    eidolon_core::usage::human(usage.cache_read_input_tokens)
                ));
            }
            s
        }
        // What a turn that never settled had already spent. Written only
        // when there was a cost to write, so its absence beside a
        // `cancelled` means the backend never said — not that the turn
        // was free.
        TurnSpend { usage, calls } => format!(
            "spent in={} out={} over {calls} call{}",
            eidolon_core::usage::human(usage.total_input()),
            eidolon_core::usage::human(usage.output_tokens),
            if *calls == 1 { "" } else { "s" }
        ),
        // The wrap-up nudge: the harness told the model the turn was near
        // its limit, with this many model calls to go.
        TurnBudget { calls_left } => format!("wrap-up: {calls_left} calls left"),
        // The context, which `settled` above deliberately does not carry:
        // its `in=` is the turn's total across every model call it made.
        ContextSize { tokens } => format!("context {}", eidolon_core::usage::human(*tokens)),
        // The harness's answers to the commands the model wrote in its own
        // prose: one record, one entry per command, and this renders the
        // first of them with the count — the full text is on the branch,
        // and `eidolon log` is a row per record, not a transcript.
        CommandResults { lines } => match lines.first() {
            Some(first) => format!("answers {}: {}", lines.len(), preview(first)),
            None => "answers".to_string(),
        },
        // The pace, said the one way it is said anywhere: `pace_note` is
        // shared with the live event printer and the TUI's usage page.
        TurnPace {
            timing,
            output_tokens,
        } => match eidolon_core::usage::pace_note(timing, *output_tokens) {
            Some(note) => format!("pace {note}"),
            // A record is written only when a call was timed, so no note
            // means the turn finished inside the clock's own resolution —
            // said as such rather than as a rate of zero.
            None => format!("pace under a millisecond over {} calls", timing.calls),
        },
        AskUser { prompt, answer, .. } => format!("ask {prompt:?} → {answer:?}"),
        Cancelled => "cancelled".into(),
        ModelChanged { model } => format!("model → {model}"),
        Compacted {
            replaced_messages,
            summary,
        } => format!(
            "compacted {replaced_messages} messages: {}",
            preview(summary)
        ),
        BackendSession { backend, id } => format!("{backend} session {id}"),
        UserToolCall {
            tool_use_id,
            name,
            input,
            utterance,
        } => match utterance
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        {
            Some(u) => format!(
                "dispatch [{tool_use_id}] {name} {}: {}",
                preview(&input.0),
                preview(u)
            ),
            None => format!("dispatch [{tool_use_id}] {name} {}", preview(&input.0)),
        },
        Note { text } => format!("note: {text}"),
        PeerMessage {
            from,
            from_cwd,
            channel,
            text,
        } => match channel {
            Some(c) => format!("peer {from} ({from_cwd}) → {c}: {}", preview(text)),
            None => format!("peer {from} ({from_cwd}): {}", preview(text)),
        },
        // An outside caller — `eidolon send` — so the one-line ledger
        // says so: the sender is a tool, not a session, and a log read
        // back later must not blur that into peer traffic.
        ExternalMessage { from, channel, text } => match channel {
            Some(c) => format!("external {from} → {c}: {}", preview(text)),
            None => format!("external {from}: {}", preview(text)),
        },
        // A park the session armed has resolved and the turn is about to
        // continue on it. Its own line, because the headless ledger is read
        // back later and "why did this session wake" has no other answer.
        TriggerFired { condition, outcome, .. } => {
            format!("woke: {} — waiting on {}", preview(outcome), preview(condition))
        }
        // The gate's own answer. `structural` is spelled out rather than
        // printed as a bool because it is the whole difference between a
        // rule a table edit could silence and a shape no edit can.
        // Says which way it went and which record, because a log read
        // back later has to explain why a turn sent less than it holds.
        SessionNote { text } => match text.trim() {
            "" => "session note cleared".to_string(),
            t => format!("session note: {}", preview(t)),
        },
        Pinned { path, pinned } => {
            format!("{} {path}", if *pinned { "pinned" } else { "unpinned" })
        }
        // The pin as pinned, which is what the record holds — a log says
        // what was asked for, and what the note resolved to on any given
        // turn is not on the branch to be read back.
        PersonaPinned { persona } => match persona.trim() {
            "" => "persona cleared".to_string(),
            p => format!("persona: {p}"),
        },
        Excluded { target, excluded } => {
            format!(
                "{} #{target} from the replay",
                if *excluded { "struck" } else { "restored" }
            )
        }
        // What happened leads, and the classifier's reason follows it in
        // brackets, because the two can pull against each other: the reason
        // is the classifier's fixed vocabulary and says things like
        // "blocked" about a shape the gate then *asked* about. This line
        // used to read `Approved (shape, not merits): unsafe command in
        // sequence blocked`, and a session that read its own log back
        // through `eidolon log` took that for a call the gate had refused
        // without telling it, and wrote a turn around the idea. A question
        // raised about a command's shape is not a verdict on its merits,
        // and the line says which it was before it says why.
        PolicyVerdict {
            tool_use_id,
            tool,
            reason,
            structural,
            outcome,
            note,
        } => {
            use eidolon_core::session::PolicyOutcome::*;
            let asked = if *structural {
                "asked about its shape"
            } else {
                "asked"
            };
            let by = note
                .as_deref()
                .map(|n| format!(": {n}"))
                .unwrap_or_default();
            let what = match outcome {
                Refused => format!("refused ({reason})"),
                Approved => format!("{asked} ({reason}) — approved by you"),
                Declined => format!("{asked} ({reason}) — declined by you"),
                Judged => format!("{asked} ({reason}) — allowed by the judge{by}"),
                Yolo => format!("{asked} ({reason}) — waived, yolo"),
            };
            format!("policy [{tool_use_id}] {tool} {what}")
        }
    }
}

/// Reads one line from stdin with terminal echo disabled — real hidden
/// input, for a secret typed at an interactive terminal. Restores echo on
/// every exit path (including an error), and prints the newline the
/// suppressed echo swallowed so the next line of output starts fresh.
pub fn read_hidden_line() -> std::io::Result<String> {
    use std::io::BufRead;

    // SAFETY: `tcgetattr`/`tcsetattr` on stdin's fd (0) with a stack-local
    // `termios` we initialize before use; both are standard libc calls with
    // no aliasing or lifetime hazards here.
    let mut term = std::mem::MaybeUninit::<libc::termios>::uninit();
    let had_termios = unsafe { libc::tcgetattr(0, term.as_mut_ptr()) } == 0;
    if had_termios {
        let mut hidden = unsafe { term.assume_init() };
        hidden.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &hidden) };
    }

    let mut s = String::new();
    let result = std::io::stdin().lock().read_line(&mut s);

    if had_termios {
        let original = unsafe { term.assume_init() };
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &original) };
    }
    // The echoed newline never made it to the terminal while ECHO was off.
    eprintln!();

    result?;
    let trimmed_len = s.trim_end_matches(['\n', '\r']).len();
    s.truncate(trimmed_len);
    Ok(s)
}

pub fn read_prompt(prompt: &str) -> Option<String> {
    use std::io::{BufRead, Write};
    eprint!("{prompt}");
    std::io::stderr().flush().ok()?;
    let mut s = String::new();
    match std::io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(s),
    }
}

pub fn preview(s: &str) -> String {
    let one: String = s.chars().take(100).collect();
    let one = one.replace('\n', "⏎");
    if s.chars().count() > 100 {
        format!("{one}…")
    } else {
        one
    }
}

pub async fn print_events(mut rx: broadcast::Receiver<Event>) {
    use std::io::Write;
    let mut out = std::io::stdout();
    loop {
        match rx.recv().await {
            Ok(ev) => match ev {
                Event::TextDelta(t) => {
                    let _ = out.write_all(t.as_bytes());
                    let _ = out.flush();
                }
                Event::ThinkingDelta(_) => {}
                Event::ToolUseStart { name, .. } => eprintln!("\n→ {name}"),
                Event::ToolCallFinished { call, output, .. } => {
                    eprintln!(
                        "  ↳ {} {}{}",
                        call.name,
                        if output.is_error { "ERROR " } else { "" },
                        preview(&output.content)
                    );
                }
                Event::TurnSettled { usage, timing, .. } => {
                    let _ = out.write_all(b"\n");
                    let _ = out.flush();
                    // The pace, when there was one to measure — printed as
                    // the turn settles, where the record beside it
                    // (`eidolon log`, and the same line on a resume) carries
                    // the same reading for anyone reading it later.
                    if let Some(note) = timing
                        .and_then(|t| eidolon_core::usage::pace_note(&t, usage.output_tokens))
                    {
                        eprintln!("[{note}]");
                    }
                }
                Event::Compacted {
                    replaced_messages, ..
                } => eprintln!("\n[compacted {replaced_messages} messages]"),
                // A peer's — or an outside caller's — mail, shown the
                // moment it is journaled rather than only when the next
                // turn reads it: a long-lived headless session otherwise
                // learns of a note an hour late, and a `--no-wake` message
                // is never shown at all.
                Event::PeerMessage {
                    from,
                    channel,
                    external,
                    text,
                    ..
                } => {
                    let kind = if external { "external" } else { "peer" };
                    match channel {
                        Some(_) => eprintln!("\n[{kind} {from} on the channel: {}]", preview(&text)),
                        None => eprintln!("\n[{kind} {from}: {}]", preview(&text)),
                    }
                }
                Event::Error(e) => eprintln!("\n[error] {e}"),
                Event::Cancelled { .. } => eprintln!("\n[cancelled]"),
                // The harness's answers to commands the model wrote in its
                // prose. There is no call for them to appear beside — the
                // reply that asked has already been printed and the
                // continuation is about to be — so they are printed here,
                // where they happened, exactly as the model reads them.
                Event::CommandResults { lines, .. } => {
                    for line in lines {
                        eprintln!("{line}");
                    }
                }
                _ => {}
            },
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

pub struct StdinUser;

fn read_line(prompt: String) -> Option<String> {
    use std::io::{BufRead, Write};
    eprint!("{prompt} ");
    std::io::stderr().flush().ok()?;
    let mut s = String::new();
    std::io::stdin().lock().read_line(&mut s).ok()?;
    let s = s.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

#[async_trait]
impl UserIo for StdinUser {
    async fn choose(
        &self,
        prompt: &str,
        options: &[Choice],
        cancel: &CancellationToken,
    ) -> Option<String> {
        let labels: Vec<String> = options.iter().map(|c| c.label.clone()).collect();
        let p = format!("\n? {prompt} [{}]\n>", labels.join("/"));
        let answer = tokio::select! {
            r = tokio::task::spawn_blocking(move || read_line(p)) => r.ok().flatten()?,
            _ = cancel.cancelled() => return None,
        };
        let a = answer.to_lowercase();
        labels
            .iter()
            .find(|l| l.to_lowercase() == a)
            .or_else(|| labels.iter().find(|l| l.to_lowercase().starts_with(&a)))
            .cloned()
    }
}

# Eidolon

An interactive coding harness in Rust — the thing at the keyboard. A
headless agent core (one dispatch chokepoint, a binary session log that
resumes by replay) with a terminal UI as one consumer beside headless
`run`/`chat` modes. Built to replace pi for daily interactive use after
the layer that frustrated — rendering, keymaps, layout — turned out to be
the layer pi exposes worst.

The premise that shapes the code: **everything the user touches is a
script.** Built-in tools, the policy table, and the UI layout itself are
Rune scripts on the same contract as anything a user writes, wrapping Rust
primitives — so the harness is reshaped by editing a script, never by
fighting a privileged renderer. Under that, every tool call — model, script,
operator, or question — routes through one policy gate and one journal,
whichever backend is speaking: Anthropic, OpenAI-compatible, Gemini, and
Codex wires over HTTP, or the Claude CLI driven as a first-class backend.

## Try it

```sh
cargo run -- tui --provider mock     # no credentials, no network; esc, space q quits
```

For a real turn, put a key in the secret store (over stdin, never argv)
and pick a model:

```sh
cargo run -- secret set deepseek
cargo run -- tui                      # space m opens the model picker
```

Providers ship as small declarations — DeepSeek, z.ai, OpenRouter,
Antigravity — and a `[[providers]]` patch in `config.toml` or a script in
`~/.config/eidolon/providers/` adds or adjusts any endpoint speaking those
wires.

## Where the documentation lives

- **`AGENTS.md`** — orientation for agents working in this repo: commands,
  load-bearing invariants, the source map, and links into the vault.
- **Module doc comments** (`//!` / `///`) — the technical reference, beside
  the code it describes.
- **The operator's vault** (reached through Mneme) — design rationale,
  plans, status, and history under `wiki/projects/Eidolon/`. The index note
  there is this project's status page.

# AGENTS.md

**Eidolon** — an interactive Rust coding harness: headless core, dispatch
chokepoint, binary session log, event bus, and consumers on top. Crates use
`eidolon-`, the binary `eidolon`, scripts `eidolon::*`.

This file is the short entry point, not the manual. It carries the
cross-harness `AGENTS.md` name, which the harness's own
`[project] instructions = "auto"` reads first and Claude Code reads
natively; a legacy `CLAUDE.md` is still honored as the fallback. Detailed
architecture, rationale, plans, and status belong in the **vault**, not
another repo doc or an expanded crate-level comment. Keep this file short;
update and link the relevant vault note instead of appending a feature
narrative here.

## Working rules

- Read relevant code before changing it; preserve unrelated work.
- The tree is hand-formatted at roughly 140 columns. **Do not run
  `rustfmt` or `cargo fmt`.** Use `anyhow::Result` and `.context()` on IO.
- Check `peers` before editing, claim files with `send`, and announce changed
  paths when done. Coordinate shared files and persistent-format changes.
- Verify changes with appropriate terminating checks; report what actually
  ran and what remains unverified. Batch independent tool calls.
- A harnox change is a tag first, never a bare rev in `flake.nix`: `[patch]`
  and the Nix input both hide the pin, so only `nix build` compiles what a git
  consumer resolves. Run it before pushing (`.githooks/pre-push`, active once
  per clone via `git config core.hooksPath .githooks`).
- Configuration is read-only to the harness. On this machine Nix generates
  `~/.config/eidolon/config.toml` and provider files: edit the generator,
  not the generated file or store symlink. Do not add another config writer.

## Commands

```sh
cargo build
cargo clippy --all-targets
cargo test
cargo run -- run --provider mock "hello"       # no credentials/network
cargo run -- chat --provider mock              # headless REPL
cargo run -- tui --provider mock               # default UI; esc, space q quits
cargo run -- run --image shot.png "describe it"
cargo run -- run --provider claude-cli "…"      # real Claude CLI driver
cargo run -- models                            # live catalog; primes cache
cargo run -- tools                             # registered manifests
cargo run -- tools --live                      # refresh remote tool listing
cargo run -- log <session.eid>                  # inspect a real session
cargo run -- worktree <branch>                 # second checkout/session
cargo run -- secret set NAME < key.txt         # never a key in argv
```

Diagnostics are opt-in. The drawing commands (`tui`, and no subcommand)
write no `tracing` output at all: stderr is the terminal they are drawing
on. `EIDOLON_LOG=/tmp/eidolon.log` gives them a subscriber writing there,
and every headless command logs to stderr as before.
`EIDOLON_WIRE_LOG=/tmp/wire.log` records request
bodies/raw SSE; `EIDOLON_MCP_LOG=/tmp/mcp.log` records MCP/remote JSON-RPC.
Keep secrets out of both. `EIDOLON_BOOT_LOG=/tmp/boot.log cargo run --release`
records startup marks: read the gap column when the first frame regresses.

## Load-bearing invariants

- **One chokepoint.** Model, script, operator, and question tools reach
  `Dispatcher`. `dispatch` decides/runs/journals/publishes; `adjudicate`
  decides for a backend hook; `execute` decides/runs when the backend's
  stream is the journal. No execution bypass or second policy path.
- **The log is truth.** Journal before publishing; resume is replay.
  Operator calls need `UserToolCall` before execution. Publish turn
  boundaries and stop reasons; never infer a settle from rendered text.
- **Journaled enums append at the actual end.** Bitcode encodes variant
  order (`RecordKind`, harnox `ContentBlock`). Count variants, agree ordering
  with concurrent editors, update `crates/core/tests/pins.rs` fixtures and
  exhaustive ordinal match, and decode a previous-binary log. Fresh-log
  round trips alone cannot detect historical corruption.
- **Inject messages only at safe points.** Steers, peers, and budget nudges
  must ride completed tool results, never interrupt owed results in replay.
  Queue semantics belong in core, not the TUI. Keep peer rosters out of the
  cached system prefix.
- **Fail closed without bricking attended work.** The classifier's named
  denials stand; structural unfamiliarity asks. Judge and yolo can promote
  Ask, never Deny. Keep human/model/waived outcomes distinct in the ledger.
- **First frame never waits on network.** Cached discovery, parallel Rune
  warm-up, one default-UI compile, draw before poll, no terminal probing.
  Budget <100 ms; release baseline roughly 10 ms. Discuss regressions.
- **One shared message/provider model.** `../harnox` owns wire clients,
  canonical messages, and shared vault/policy conventions; core re-exports.
  `model` is the bare wire id, `model_key` the journaled catalog key.
- **Unknown is not zero.** Missing prices are unpriced, missing spend is
  unknown, interrupted measured spend is a labelled floor. Shared pricing
  arithmetic includes all cache rates. Never make partial totals look final.
- **Stable prefix, live inputs.** Persona/project instruction bodies are
  re-read per turn, never journaled. Tool discovery derives from the branch;
  no separate persistent reached-tool state. Never retry published output.
- **Scriptable does not mean unrestricted.** Built-ins wrap Rust primitives
  on the user-script contract; Rune gets `json` and `eidolon::*`, not
  unrestricted fs/http/process. Never expose credentials to scripts, tools,
  or logs; Rust injects headers (a script's `api_request` carries its endpoint
  and names a secret, and never the value). Never disable Claude's tools
  without the replacement MCP registry in the same invocation.
- **The UI is a view.** Folding/search/trace and image placement share layout
  decisions; journal conversation facts, not keystroke notices. Normal/select
  are Helix-modal; text-entry modes also support readline. No slash commands.

## Vault references — read for the task, not all at startup

These are **vault-relative paths**, not files in this checkout. Read via
`mneme_rpc` (`read_note`, `args: {"title": "<path>"}`), or Mneme dispatch.
A wikilink does not load its target automatically. If the vault is unavailable,
say so and inspect code; do not invent a reference's contents.

- [[wiki/projects/Eidolon/Core and Integration Contracts]] — core loop,
  replay and enum incident, steering/peers, prompts/personas, spend, deferred
  tools, policy/judge/yolo, Rune/assets, startup, providers/caching/credentials,
  Claude driver, remote tools, Verba, and swarm. Read the relevant sections
  before changing these contracts.
- [[wiki/projects/Eidolon/Interactive UI Contracts]] — ownership, modes,
  keys/commands/completion, fields/minibuffers/editor, clusters/trace/search,
  tags/copying, notices/pages/usage/quota, images, dispatch, and session adoption.
- [[wiki/projects/Eidolon/Agent Operating Guidance]] — Mneme/Melete workflows,
  peer replies, retry/reporting discipline, and the failure trackers behind them.
- [[wiki/projects/Eidolon/index]] — project design and historical decisions.
- [[wiki/projects/Eidolon/Eidolon Build Log]] — chronological implementation record.
- [[wiki/projects/Eidolon/Eidolon TODOs]] — pending work, not runtime guarantees.
- [[wiki/projects/Eidolon/Tool Extensions (design)]] — the two paths a Rune
  tool takes — a command wrapper and an API wrapper: the primitives, what the
  gate sees, what the log keeps, and what a shareable file means. Read before
  writing either kind.
- [[wiki/projects/Eidolon/Context Controls (design)]] — pins, exclusion, compaction.
- [[wiki/projects/Eidolon/The Command Safety Classifier (design)]] — gate design.
- [[wiki/projects/Eidolon/Images in Chat]] — attachment and graphics rationale.
- [[wiki/projects/Eidolon/Eidolon Startup Benchmark]] — startup measurements.
- [[wiki/projects/Eidolon/Eidolon Swarm Coordination (idea)]] — original swarm design.
- [[wiki/projects/Eidolon/Eidolon Terminal Integrations]] — terminal integration plans.

Historical notes can describe superseded designs. The contract references
summarize the extracted implementation; code and tests decide what runs today.

## Source map

| Path | Owns |
|---|---|
| `crates/core` | Agent, session/replay, events, dispatch, policy/user seams |
| `crates/tools` | Rust file/shell/search/web/image primitives |
| `crates/rune` | Rune host, scripted built-ins, leaf policy table |
| `crates/cli` | Binary, config readers, shipped asset reconciliation |
| `crates/tui` | Interactive consumer; Rust widgets plus Rune UI tables |
| `crates/providers` | Definitions/scripts, catalog, HTTP wrapper, cost/quota |
| `crates/claude` | Claude CLI driver, hook/MCP bridges, transcript handoff |
| `crates/remote` | Mneme/Melete OAuth clients and tool registration |
| `crates/swarm` | Runtime session registry, maildir delivery, channel |
| `crates/verba` | Client-side classifier bundles; execution stays dispatched |
| `../harnox` | Shared message/wire, credential, policy algebra, vault conventions |

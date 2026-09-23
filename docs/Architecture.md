# Architecture

## The box

AMD Ryzen AI 7 PRO 350 (8 cores, Zen 5 / Zen 5c), Radeon 860M iGPU (RDNA 3.5,
~4 GB carve-out reported, shares system memory), 64 GB LPDDR5X, Windows 11 Pro.
There is an XDNA 2 NPU; nothing in the stack uses it.

**The box is memory-bandwidth-bound** — measured, not assumed. A thread sweep
on the 27B dense model is flat and then falls: 4 threads 2.65 tok/s, 8 → 2.71,
16 → 2.20. Effective bandwidth during decode is ~40 GB/s.

The consequence that shapes every model decision: what matters is bytes touched
per token, so **active** parameters, not total, set the speed.

## Serving

`llama.cpp` (own build in `llama.cpp/`, Vulkan backend `ggml-vulkan.dll`
present; ROCm is not an option on gfx1152) runs `llama-server` in **router
mode**. `models.ini` declares every model, each section becomes an entry in
`GET /v1/models`, and Open WebUI, OpenCode and eidolon's `hoot` provider all
read that one list. Global settings: 16 K context, jinja templates, flash
attention, 8 threads.

Measured here (llama-bench; tg32 = generation tok/s, pp128 = prompt tok/s):

| model | CPU tg | Vulkan tg | CPU pp | Vulkan pp | verdict |
|---|---|---|---|---|---|
| Qwen3.8-27B UD-Q4_K_M | 2.58 | 1.93 | 15.6 | 33.3 | stay on CPU; Vulkan only if prompt-heavy |
| Bonsai-2-27B PTQ1_0 | 0.29 | 0.79 | 16.0 | 27.0 | slow path, avoid |
| Bonsai-2-27B Q2_0 | 0.50 | **3.34** | 16.2 | 35.5 | Vulkan, 99 layers |
| Bonsai-8B Q1_0 | 10.56 | 10.27 (±7) | 18.5 | 31.1 | CPU; fastest model on the box |

Two more measured facts: Vulkan roughly **doubles prompt processing
everywhere**; and Qwen with its MTP draft head (speculative decoding,
`spec-type = draft-mtp`) goes 1.83 → 3.07 tok/s on the same prompt, 1.68× on
code at 84 % acceptance. Qwen's thinking mode is off by default in the router
because at ~2.7 tok/s it burns the budget before answering.

## The jev sidecar

`jev/server.py` (FastAPI, service port assigned by eidolon) serves the two models llama.cpp cannot: they
are not causal LMs and have no GGUF path.

- **jevlike** — a one-pass N-way chooser: context + N text options → one
  probability per option, one forward pass, no decode. Byte-encoder trained
  from scratch; the checkpoint on this box is `runs/synthetic.pt`, **169 KB,
  trained on synthetic data**. Imported from the sibling
  `cms-agent/models/jevlike` checkout, not vendored. Instant on CPU.
- **openjev** — Qwen3.5-4B fine-tuned as a 3-way NLI cross-encoder
  (`Qwen3_5ForSequenceClassification`): premise + hypothesis → contradiction /
  entailment / neutral. ~9 GB safetensors, CPU float32, one pass. Loaded
  through the vendor's own `OpenJevCrossEncoder`, because the head was trained
  on **one templated string** (`config.nli_template`), not the tokenizer's pair
  encoding.

As of 2026-09-18 the sidecar still exposes both as fake chat models on an
OpenAI surface. The decision is that they become **tools**, not models — an
extension, phase 3.

## The face

Open WebUI, installed with `uv tool`, launched by `bin/hoot.ps1` (`hoot up`,
`hoot serve`, `hoot webui`, `hoot jev`, `hoot bench`, `hoot setup`,
`hoot status`). The skin lives in `webui/`:

- `custom.css` — palette, type, borders, the chat frame, ~19 id-anchored icon
  masks, the Notes/Workspace nav icons (anchored on `href`).
- `loader.js` — **generated** by `build-loader.py` from `icon-map-full.json` +
  `dinkie-bodies.json`: a runtime swapper that fingerprints every inline
  `<svg>` by normalised inner markup and replaces 165 of Open WebUI's 272 icons
  with **Dinkie Icons** (MIT, pixel grid). A `MutationObserver` coalesced to
  one pass per frame survives Svelte re-renders.
- `brand/` — the owl in every slot Open WebUI reads.

`hoot up` copies `webui/` into the installed package on every launch, so an
upgrade that blanks the hooks heals on the next start.

## The core: eidolon

Rust workspace (`eidolon/`, remote `noah427/eidolon`; shared crate `harnox`,
remote `noah427/harnox`). Crates:

| crate | what |
|---|---|
| `core` | agent loop, dispatch chokepoint, journal, event bus, `ipc` |
| `cli` | the `eidolon` binary |
| `tui` | the terminal interface, itself scripted in Rune (`ui.rn`) |
| `web` | the verification surface (`eidolon web`) |
| `tools` | the Rust primitives behind the built-in tools |
| `providers` | the model catalog; providers are Rune scripts |
| `rune` | the script host, the policy table, the extension model |
| `claude` | drives the Claude Code CLI as a backend, policy kept eidolon's via a hook socket |
| `swarm` | presence + doorbell between concurrent sessions — "the directory is the registry" |
| `remote` | Mneme / Melete clients |
| `verba` | Verba Volantia: free text resolved to a tool call, no model turn |

Everything the operator changes is a **Rune 0.14 script**: tools (`tools/*.rn`),
providers (`providers/*.rn`), the policy leaf table (`policy.rn`), the TUI
(`ui.rn`), and now extensions (`extensions/<name>/`). Scripts see only `json`
and `eidolon` — no `http`, `fs`, `process` — so every effect goes through a
host primitive, behind the policy gate, into the session log. Built-ins are
seeded into the user's tools directory so "the file the operator edits is the
file the model runs."

The `hoot` provider (`%APPDATA%\eidolon\providers\hoot.rn`) points eidolon at
the router: `hoot:Qwen3.8-27B-UD-Q4_K_M`, `hoot:bonsai-8b`,
`hoot:bonsai-2-27b`.

### The extension host

Landed 2026-09-18. An extension is a directory under
`%APPDATA%\eidolon\extensions\` with an `extension.rn` in it: a namespaced
bundle of Rune tools, optionally served by a long-running process of its own.

```
extensions/jev/
  extension.rn        manifest: name, description, service, tools
  tools/choose.rn     an ordinary tool script — registers as jev_choose
  service.py
```

The joint between the two halves is one host primitive,
`eidolon::service_call(method, args)`. A script names a **method**; the loader
owns the address, the port and the bearer token, all in Rust. A script cannot
name a URL — it cannot even name which extension it belongs to, because that is
bound to the thread the call runs on. Same shape as `search`, same reason.

Services are **machine-scoped**: started once, recorded at
`<cache>/eidolon/extensions/<name>.json`, adopted by every later session. Two
chats do not mean two copies of a model in memory.

Full guide: [`eidolon/docs/extensions.md`](../eidolon/docs/extensions.md).

### Credentials

Paths that exist today: API keys via `eidolon secret set NAME < key.txt`
(stdin only, never argv — `deepseek`, `zai`, `openrouter`, `ollama`,
`brave_search`); `eidolon google-login` (Antigravity/Gemini, OAuth refresh
token → `antigravity.json`); `eidolon chatgpt-login` (device flow →
`chatgpt.json`).

As of 2026-09-18 none are stored: every cloud provider is dark, only `hoot` is
live. A credentials page served by eidolon is phase 2 — it cannot live in Open
WebUI, and a form must POST the value in a body.

## The vault

Mneme serves two vaults (`magi`, `necoconeco`). `eidolon/AGENTS.md` cites
twelve design notes under `wiki/projects/Eidolon/`; **none exist in either**
(checked 2026-09-18). The decision was not to mint an Eidolon wiki: Minerva gets
the wiki, eidolon is an entity inside it. Until that mint lands, this `docs/`
directory is the record.

## Components, in brief

**Open WebUI** — the face. 0.11.3, `uv tool` install, compiled Svelte.
Supported hooks `custom.css` + `loader.js`. Has Tools, Automations (rrule),
Terminals (proxy), Skills; no graph editor. Forwards the chat id as a header
when `ENABLE_FORWARD_USER_INFO_HEADERS=True`.

**llama.cpp router** — own build with Vulkan; `llama-server` router mode on
`:8080`; `models.ini` is the model list every client reads; MTP speculative
decoding for Qwen.

**jev** — the sidecar and its two one-pass models. jevlike is an independent
starter with the input/output shape of TypeSafe's commercial Jev. Becoming
tools `choose` / `entail`; the learned transition function of Minerva's automation
statecharts.

**The models on disk** — Qwen3.8-27B-UD-Q4_K_M (18 GB, + MTP draft 1.3 GB, +
mmproj); Ternary-Bonsai-2-27B Q2_0 "legacy" (7.2 GB, live), PQ2_0 (6.8 GB,
dead), PTQ1_0 (6.2 GB, only its mmproj used); Bonsai-8B-Q1_0 (1.1 GB); openjev
(8.5 GB). Candidates: gpt-oss-20b, Qwen3-Coder-30B-A3B-Instruct,
Qwen3-30B-A3B-Instruct-2507.

**Dinkie Icons** — atelierAnchor, MIT, 1198 pixel-grid emoji-style icons via the
Iconify API; 165 mapped into Open WebUI; no UI chrome glyphs by design.
Companion candidate: pixelarticons.

**Melete and Mneme** — already have their own wiki (`04 ◈ GNOSIS/Melete-Mneme/`).
In Minerva: Mneme is where the wiki will live and where eidolon's `mneme_rpc` tool
reads; Melete is the job harness and the intended scheduler for remote triage
boxes. Link, do not duplicate.

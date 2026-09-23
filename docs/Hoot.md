# Minerva

A local AI workstation for the FAU Cybersecurity Club: one box runs the models,
one chat app fronts them, and one agent core does the work. Nothing leaves the
machine unless a cloud provider is deliberately logged in.

Minerva is its own project; `hoot` (`bin/hoot.ps1`) remains its launcher and CLI control. **eidolon is its core**, the way OpenClaw is a core
with skins and extensions around it — eidolon appears here as a component, not
as the subject.

| | |
|---|---|
| [Architecture](Architecture.md) | the box, the serving layer, the face, the core, the vault |
| [Decisions](Decisions.md) | what was decided, why, and what it replaced |
| [Build-Log](Build-Log.md) | what was actually built and verified, by day |
| [Tasklist](Tasklist.md) | open work, in order of value |
| [eidolon/docs/extensions.md](../eidolon/docs/extensions.md) | how to write an extension |

## Three layers, three processes

| layer | what | process / port |
|---|---|---|
| **face** | Open WebUI 0.11.3, re-skinned as *Minerva* (owl, phosphor green, pixel-grid icons) | `uv tool` install, `bin/hoot.ps1` launches |
| **models** | llama.cpp `llama-server` in router mode; the eidolon-managed `jev` extension for the two non-causal models | `:8080` router; jev service port assigned dynamically |
| **core** | **eidolon** — a Rust coding-agent harness: dispatch chokepoint, policy gate, journaled sessions, Rune extension scripts | `eidolon` binary; sessions in `%LOCALAPPDATA%\eidolon\sessions`, config in `%APPDATA%\eidolon` |

## The direction

Fixed 2026-09-18: **Open WebUI is the face, eidolon is the agent.** Chats in
the app summon eidolon sessions on the box and track their state, the way the
Codex app summons `codex` sessions.

Every extension — models-as-tools, browser automation, schedulers, credential
logins — is an **eidolon extension**, togglable from eidolon or from the webui.
Open WebUI is never extended in place: it is compiled Svelte, and the only
supported hooks are two files it ships empty (`custom.css`, `loader.js`).

The name is the club's owl, Minerva's own bird. It is the logo (`webui/owl-green.svg`), the tab
icon, and `WEBUI_NAME`.

## Repository layout

```
bonsai2/
  bin/hoot.ps1         the launcher: up, serve, shim, webui, bench, setup, status
  models.ini           the router's model list, with a bench line per model
  models/              the GGUFs and openjev's safetensors
  llama.cpp/           own build, Vulkan backend present
  jev/server.py        the sidecar (becoming an extension — phase 3)
  webui/               the skin: custom.css, loader.js and its build inputs
  providers/           Rune provider scripts seeded into %APPDATA%\eidolon
  eidolon/             the core — its own git repository (noah427/eidolon)
  harnox/              shared crate (noah427/harnox)
  docs/                this record
```

## Standing constraints

- Never a key in argv, in a script, or in a log (`EIDOLON_WIRE_LOG`,
  `EIDOLON_MCP_LOG`). `eidolon secret set NAME < key.txt` reads stdin.
- Do not run `rustfmt` / `cargo fmt` in `eidolon/`; hand-format.
- Open WebUI's LICENSE clause 4 permits altered branding only for **50 or fewer
  end users in a rolling 30 days**. A personal box, or one install per club
  member, is well inside that; one shared instance in front of more than fifty
  people is not.
- Nothing from the vault's `03 ◆ CASPER/` leaves the vault.

## Picking this up cold

[`HANDOFF.md`](../HANDOFF.md) in the repo root is the entry point for an agent joining this work: what is running, the three different git
situations, the protected files, the conventions, the measured baselines, and the task list as done / needed / pending.

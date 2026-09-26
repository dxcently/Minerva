# Minerva

A local AI workstation: one box serves the models, one chat app fronts them, and
one agent core does the work. Nothing leaves the machine unless a cloud provider
is deliberately logged in.

**Minerva** is the product. **`hoot`** — `bin/hoot.ps1` — is the Windows
launcher for its model server, and it keeps that name and filename so existing
configs and provider ids (`providers/hoot.rn`) go on working.

## Three pieces

| piece | what it is | where |
|---|---|---|
| **webui/term** | the face — a terminal-style web UI, served by `eidolon web` | `webui/term/`, WSL |
| **eidolon** | the agent core: a Rust coding-agent harness, and the sole extension host | `eidolon/`, WSL |
| **jev** | an eidolon-managed extension — browser automation, graph-driven navigation | `jev/`, WSL, service port assigned dynamically |

A local llama.cpp router (upstream `ggml-org/llama.cpp`, Vulkan build) serves
the models on `:8080`. It is the one piece that runs natively on Windows;
everything else runs in WSL. Open WebUI was retired on 2026-09-26 — its skin is
kept in [`webui/archive/open-webui/`](webui/archive/open-webui/README.md).

## Launch it

On Windows, the model server:

```powershell
hoot setup              # fetch upstream llama.cpp (win-vulkan-x64)
hoot up                 # start the router, backgrounded
hoot status             # is it up, and what it serves
hoot stop               # stop it
```

In WSL, eidolon and the face: see [`webui/term/README.md`](webui/term/README.md).

The launcher's other commands:

| command | what it does |
|---|---|
| `hoot serve [-Port N] [-Bind ADDR]` | start llama-server (router mode), foreground |
| `hoot models` | list models from `models.ini` / the API |
| `hoot get <quant>` | download a Qwen3.8-27B quant |
| `hoot setup [-Tag T] [-Force]` | pin a llama.cpp release / replace the current build |
| `hoot edit` | open `models.ini` |
| `hoot which` | show which llama.cpp binaries are in use |

## Where to read more

| | |
|---|---|
| [`docs/Hoot.md`](docs/Hoot.md) | the project's own front door: layers, direction, layout, constraints |
| [`docs/Architecture.md`](docs/Architecture.md) | the box, the serving layer, the face, the core, the vault |
| [`docs/Decisions.md`](docs/Decisions.md) | what was decided, why, and what it replaced |
| [`docs/Build-Log.md`](docs/Build-Log.md) | what was actually built and verified, by day |
| [`HANDOFF.md`](HANDOFF.md) | entry point for an agent joining this work |

`docs/Hoot.md` keeps its filename so links elsewhere in the tree keep resolving,
even though the product it describes is Minerva.

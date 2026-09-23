# Minerva

A local AI workstation: one box serves the models, one chat app fronts them, and
one agent core does the work. Nothing leaves the machine unless a cloud provider
is deliberately logged in.

**Minerva** is the product. **`hoot`** is its launcher and CLI control —
`bin/hoot.ps1` — and it keeps that name, its filename, and its commands so
existing configs, provider ids, and scripts go on working.

## Three pieces

| piece | what it is | where |
|---|---|---|
| **Open WebUI** | the face — the chat UI, re-skinned as Minerva | `webui/`, `:3000` |
| **eidolon** | the agent core: a Rust coding-agent harness, and the sole extension host | `eidolon/`, `:8085` |
| **jev** | an eidolon-managed extension — browser automation, graph-driven navigation | `jev/`, service port assigned dynamically |

A local llama.cpp router serves the models on `:8080`; Ollama is on `:11434`.

## Launch it

```powershell
hoot up                 # serve + shim + webui, backgrounded
hoot status             # what is up, and what is loaded
hoot stop               # stop everything
```

The launcher's other commands, unchanged:

| command | what it does |
|---|---|
| `hoot serve [-Port N] [-Bind ADDR]` | start llama-server (router mode) |
| `hoot shim [-ShimPort N]` | start `eidolon serve` (the agent shim) |
| `hoot webui [-WebuiPort N]` | start Open WebUI wired to both |
| `hoot models` | list models from `models.ini` / the API |
| `hoot get <quant>` | download a Qwen3.8-27B quant |
| `hoot bench [-Model NAME]` | benchmark a local model |
| `hoot setup` | re-run the installer |
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

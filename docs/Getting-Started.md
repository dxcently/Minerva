# Getting started with Minerva

This guide takes a club member from a fresh Windows 11 machine to a working agent. Parts 1–4 are required; everything after that is optional.

## Contents

| Part | Topic | Required? |
|---|---|---|
| [0](#part-0-how-minerva-is-laid-out) | How Minerva is laid out | read first |
| [1](#part-1-wsl) | WSL | required |
| [2](#part-2-clone-and-build) | Clone and build | required |
| [3](#part-3-a-brain-ollama-pro-or-local-models) | A brain: Ollama Pro or local models | required |
| [4](#part-4-run-it) | Run it | required |
| [5](#part-5-jev-the-chooser-optional) | jev, the chooser | optional |
| [6](#part-6-aoide-core-optional) | Aoide core | optional |
| [7](#part-7-a-free-coding-helper-opencode-on-the-same-ollama-plan) | A free coding helper on the same Ollama plan | optional |
| [8](#part-8-when-something-is-wrong) | When something is wrong | reference |

Status is as of 2026-09-26. Anything marked **untested** has not yet been run end to end on a club machine. If you are the first to run one of those steps, fix this page.

**Tested on:** WSL Ubuntu 26.04.1 with Python 3.14.4 and rustc 1.98.1. The numbers in `models.ini` were tuned on the GPU of the PC that built this stack and its Prism llama.cpp build. On other hardware they are a starting point to re-tune.

---

## Part 0: how Minerva is laid out

```
 Windows 11 PC
 ├─ native Windows ─ llama-server (hoot)      OPTIONAL: only for local models on your GPU
 │                    http://127.0.0.1:8080/v1
 └─ WSL2 (Ubuntu) ── everything else
     ├─ eidolon ........ the agent (Rust)      eidolon/  + harnox/  (vendored)
     ├─ plugins ........ browser, subagent, jev  eidolon-plugins/  (vendored; installed into eidolon)
     ├─ webui/term ..... the browser face      served by `eidolon web`
     ├─ jev ............ the chooser (Python)  jev/
     └─ aoide .......... the mesh node (Rust)  aoide-core/  (optional)

 the brain = ONE of:
   (a) Ollama Pro, a cloud API key ....... nothing to download
   (b) local GGUF models + llama-server .. a GPU and a model download
```

Why it's split this way: eidolon upstream is Unix-only, so it runs in WSL. The GPU is easiest to use from native Windows, so the model server stays there. Everything the repo needs is vendored (see [VENDORED.md](../VENDORED.md)), so a clone builds with no other checkouts.

**What you download, depending on your choices:**

| You want | You install | You download |
|---|---|---|
| The agent with a cloud brain | WSL, a Rust toolchain | nothing (you paste an API key) |
| The agent with local models | the above, plus a llama.cpp build (hoot fetches it) | GGUF weights (12–20 GB each) |
| jev's `choose` | a Python venv | nothing (its 169 KB model ships in the repo) |
| jev's `entail` | the same venv | openjev weights (~9 GB, one script) |

---

## Part 1: WSL

In **PowerShell as Administrator**:

```powershell
wsl --install -d Ubuntu
```

Reboot when asked, then open **Ubuntu** from the Start menu and create your Linux user. Every command from here on runs in that Ubuntu terminal unless it says PowerShell.

In Ubuntu:

```bash
sudo apt update
sudo apt install -y build-essential git curl python3-venv
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
. "$HOME/.cargo/env"
```

- The crates need Rust 1.88 or newer (edition 2024; the perch uses let-chains, stable from 1.88). rustup's stable channel is newer than that.
- `python3-venv` is only needed for jev (Part 5), but it's cheap to install now.

**Fresh-WSL prerequisites.** Do these before Part 2. The first two are required; the last two are only for building the installer.

1. **systemd on.** The perch refuses to start without `$XDG_RUNTIME_DIR`, and systemd provides it. In Ubuntu, create `/etc/wsl.conf` with:

   ```ini
   [boot]
   systemd=true
   ```

   Then in PowerShell run `wsl --shutdown` and open Ubuntu again.

2. **WSL kept alive.** In `%UserProfile%\.wslconfig` (create it if missing) add:

   ```ini
   [wsl2]
   vmIdleTimeout=-1
   ```

   Then run `wsl --shutdown` once. Without it the perch stops when WSL idles out (see Part 8).

3. **Only for a release installer bundle:** `sudo apt install -y musl-tools` (the static musl build needs `musl-gcc`).

4. **Only for building `minerva-install.exe` on Windows:** Visual Studio Build Tools with the "Desktop development with C++" workload.

---

## Part 2: clone and build

Clone into your **Linux home**, not into `/mnt/c/...`.

```bash
git clone https://github.com/dxcently/Minerva ~/src/Minerva
cd ~/src/Minerva/eidolon
cargo build --release --locked
mkdir -p ~/.local/bin
ln -sf ~/src/Minerva/eidolon/target/release/eidolon ~/.local/bin/eidolon
```

**Why the Linux home?** eidolon keeps its sockets and `0600` token files on the filesystem. The Windows drive under `/mnt/c` handles Unix permissions poorly and is several times slower to build on.

- The first build takes about 3 minutes and downloads crates from crates.io.
- `~/.local/bin` is on Ubuntu's `PATH` from your next login. Open a new terminal, or run `export PATH="$HOME/.local/bin:$PATH"`.

Check it runs, with no key and no network:

```bash
eidolon tui --provider mock        # a scripted stand-in model; Esc then space q quits
```

---

## Part 3: a brain (Ollama Pro or local models)

### Option A: Ollama Pro, the $20/month cloud plan (recommended)

Ollama Pro costs **$20/month**. It gives you Ollama's larger cloud models over an API, with a monthly usage allowance and a few concurrent requests. Plans and limits change, so check [ollama.com/pricing](https://ollama.com/pricing) for the current figures.

1. Sign in at [ollama.com](https://ollama.com) and subscribe to **Pro**.
2. Go to **Settings → Keys → Add API Key**, name it `eidolon`, and copy the key.
3. Store it in eidolon's secret store:

   ```bash
   eidolon secret set ollama
   ```

   This reads the key from **stdin**: paste it and press Enter. Never pass a key as a command argument, because arguments land in shell history and in `/proc`.

4. Pick a model and start:

   ```bash
   eidolon models                      # the ollama: rows are your plan's models
   eidolon tui --model ollama:deepseek-v4.1-flash
   ```

   There is no `config.toml` by default, so eidolon has no default model: **name the model on every call** with `--model` (or `-m`), e.g. `eidolon run -m ollama:deepseek-v4.1-flash "…"`. Without it the turn has no backend.

   Inside the TUI, `space m` opens the model picker. eidolon's built-in Ollama provider lists DeepSeek V4.1 Flash, DeepSeek V4 Pro, GLM 5.3 and 5.3 Flash, Kimi K2.7 Code, Qwen 3.5 397B, GPT-OSS 120B and Nemotron 3 Super (`eidolon/crates/providers/builtin/ollama.rn`). Its header records the per-token prices it estimates spend with.

**Watch your usage.** Every eidolon turn draws on the same monthly allowance. So does opencode, if you set it up in Part 7. The Ollama dashboard shows what has been spent.

### Option B: local models on your GPU (native Windows)

Use this if you have a capable GPU (the club's EVO-X2s qualify) and would rather not use a cloud key.

**B1. Get the Windows side of the repo.**

Model files are large and should sit on the Windows disk. Use either a second clone on Windows (needs [Git for Windows](https://git-scm.com/download/win)) or the GitHub ZIP:

```powershell
git clone https://github.com/dxcently/Minerva $HOME\Minerva
cd $HOME\Minerva
```

**B2. Fetch llama.cpp.**

```powershell
bin\hoot.cmd setup          # upstream ggml-org/llama.cpp, win-vulkan-x64, into llama.cpp\
```

- Your GPU driver must provide Vulkan. Current AMD and NVIDIA drivers do.
- `hoot setup -Tag <tag>` pins a specific llama.cpp release.

**B3. Put weights where `models.ini` expects them.**

Each `[section]` in `models.ini` names a `models/<folder>/<file>.gguf` path. Download the matching GGUF from Hugging Face into that folder. `hoot get <quant>` fetches one Qwen3.8-27B quant for you.

**B4. Start the server and check it.**

```powershell
bin\hoot.cmd up             # starts the router in the background, waits for its API
bin\hoot.cmd status
```

**B5. Let WSL reach it.** *(untested)*

By default WSL's `127.0.0.1` is its own, so it can't see the Windows server. Create or edit `%UserProfile%\.wslconfig`:

```ini
[wsl2]
networkingMode=mirrored
```

Then run `wsl --shutdown` in PowerShell and reopen Ubuntu. Check from Ubuntu:

```bash
curl -s http://127.0.0.1:8080/v1/models
```

If that returns JSON, WSL can reach the server. Some machines also need a Hyper-V firewall rule (see `docs/design/range.md`).

**B6. Tell eidolon about the server.**

```bash
mkdir -p ~/.config/eidolon/providers
cp ~/src/Minerva/providers/hoot.rn ~/.config/eidolon/providers/
eidolon models                      # hoot: rows appear
eidolon tui --model hoot:Qwen3-Coder-30B-A3B
```

---

## Part 4: run it

### The terminal UI

```bash
eidolon tui
```

Useful keys:

- `space m`: models
- `space r`: sessions
- Ctrl-C: cancel the current turn
- Ctrl-C twice at the prompt: quit

### The browser UI (webui/term)

In Windows PowerShell, type:

```powershell
hoot
```

The first time, `hoot` walks you through a short setup wizard (default model, which folders a chat may work in; press Enter to accept each default) and saves your answers in `~/Minerva/minerva.toml`. `hoot init` runs the wizard again. Build it once in WSL: `cd setup && CARGO_TARGET_DIR=$HOME/setup-target cargo build --release` (until it is built, a first `hoot` goes on with the defaults).

Then `hoot` starts (or reuses) the Minerva server in WSL and opens your browser already signed in: a one-time link that works once and dies after 30 seconds, so there is no password and no token to copy. `hoot status web` says whether it is up and `hoot stop web` stops it (your chats are kept). The first run needs the server built once, in WSL: `cd perch && CARGO_TARGET_DIR=$HOME/perch-target cargo build --release`. `hoot` does not start the local-model server; `hoot up` does that.

To serve one bare `eidolon web` session by hand instead (no chat list, one session):

```bash
eidolon web --bind 127.0.0.1:47811 --ui-dir ~/src/Minerva/webui/term
#   listening on http://127.0.0.1:47811/
#   token file: /run/user/<uid>/eidolon/web-47811.token
```

Then, in your **Windows** browser, open `http://127.0.0.1:47811/#token=<contents of the token file>`. Windows can reach WSL's loopback by default. The page moves the token out of the URL once it loads.

`webui/term` is milestone M1. The door's attachment and launch routes it uses are in the vendored eidolon (upstream master). See [`webui/term/README.md`](../webui/term/README.md) for keys and layout.

---

## Part 5: jev, the chooser (optional)

jev scores a menu of options in one pass (`choose`), checks whether one statement follows from another (`entail`), and runs jev graphs as Rune workflows from the `jev` plugin (for example `eidolon workflow run ~/.config/eidolon/plugins/jev selftest`).

> **Status:** jev loads into eidolon as the `jev` plugin (`eidolon-plugins/jev`): `jev_choose` and `jev_entail` call this service on `127.0.0.1:8091` with the bearer token in `~/.config/eidolon/jev.token`, and the automation graphs run as workflows. Install it, and the `browser` and `subagent` plugins, with `eidolon plugins install dxcently/eidolon-plugins browser subagent jev --ref 7a0ff8ed288717fad7bdbd8e6163f22c3afa7a6a` and then `eidolon plugins trust <name>` for each; each plugin's README (in `eidolon-plugins/<name>/README.md`) has the rest (the browser service, the token grants).

Set it up:

```bash
cd ~/src/Minerva
python3 -m venv jev/.venv
jev/.venv/bin/pip install -r jev/requirements.txt    # CPU-only torch, ~1 GB
jev/.venv/bin/python jev/test_server.py              # 33 tests; `choose` uses the shipped checkpoint
```

Then, to start the service for the plugin:

```bash
mkdir -p ~/.config/eidolon && umask 077
head -c 24 /dev/urandom | base64 > ~/.config/eidolon/jev.token
for v in jev_choose jev_entail; do
  eidolon plugins grant $v file:~/.config/eidolon/jev.token
done
cd ~/src/Minerva/jev
EIDOLON_SERVICE_PORT=8091 EIDOLON_SERVICE_TOKEN="$(cat ~/.config/eidolon/jev.token)" \
  HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1 .venv/bin/python -u server.py
```

For `entail`, download the openjev weights:

```bash
jev/get-openjev.sh                                   # ~9 GB into models/openjev, pinned revision
(cd jev && .venv/bin/python test_server.py)
```

- **No local GPU and no download?** `jev/server.py` serves only `/health`, `choose` and `entail` on `127.0.0.1:8091`, and both `choose` and `entail` need the weights; there is no hosted option.

---

## Part 6: Aoide core (optional)

`aoide-core/` is the mesh node: `aoide` (the CLI) and `aoided` (the daemon). It connects Minerva machines to each other. You only need it on machines joining the club mesh.

```bash
cd ~/src/Minerva/aoide-core
cargo build --release -p aoide-cli     # target/release/aoide and aoided
```

- The folder is GPL-3.0 (`aoide-core/LICENSE`); the rest of the repo is MIT.
- Its test suite has 4 known failures that also fail on Aoide's own `main` (3 mail tests in `aoide-client`, 1 in `aoide-conductor`).

---

## Part 7: a free coding helper (opencode) on the same Ollama plan

[opencode](https://opencode.ai) is a free, open-source terminal coding agent. It's useful for reading, fixing and extending Minerva itself. Point it at your Ollama Pro plan and it costs nothing beyond the $20.

> **Untested on a club machine** as of 2026-09-26. The steps follow opencode's and Ollama's own documentation.

### 7.1 Install opencode (Ubuntu)

```bash
curl -fsSL https://opencode.ai/install | bash
```

### 7.2 Connect it to Ollama Cloud

Make a **second** API key at ollama.com (**Settings → Keys**) named `opencode`. With one key per tool, you can revoke either without breaking the other.

opencode's docs say a cloud model must also be pulled with the Ollama CLI before opencode can use it. So install the CLI and sign in:

```bash
curl -fsSL https://ollama.com/install.sh | sh
ollama signin
ollama pull gpt-oss:20b-cloud        # swap in any model from ollama.com/search?c=cloud
```

Then start opencode inside the repo:

```bash
cd ~/src/Minerva
opencode
```

1. Type `/connect`, search for **Ollama Cloud**, and paste the `opencode` key.
2. Type `/models` and pick the cloud model you pulled.

**Shortcut.** Ollama's docs also offer a one-command path, `ollama launch opencode`, where Ollama handles the sign-in for you.

### 7.3 Or use your local models

The repo's `opencode.json` already declares a `hoot` provider for the Windows model server from Part 3 Option B. With `hoot up` running and B5 done, `/models` lists `hoot/Qwen3-Coder-30B-A3B` and `hoot/gpt-oss-20b`.

### 7.4 Working rules for any coding agent in this repo

- Read [`VENDORED.md`](../VENDORED.md) first. `eidolon/`, `harnox/` and `aoide-core/` are upstream snapshots; fix those upstream, not here.
- Decisions go in `docs/Decisions.md`, open work in `docs/Tasklist.md`.
- Never put a key in a file, a command argument or a log. Keys go in `eidolon secret set` or opencode's `/connect`.

---

## Part 8: when something is wrong

| Symptom | Likely cause | Fix |
|---|---|---|
| `cargo build` is very slow, or fails with permission errors | The repo is under `/mnt/c` | Clone into `~/src/Minerva` (Part 2). |
| `eidolon: command not found` | `~/.local/bin` isn't on `PATH` yet | Open a new terminal, or `export PATH="$HOME/.local/bin:$PATH"`. |
| `python3 -m venv` says `ensurepip is not available` | `python3-venv` isn't installed | `sudo apt install -y python3-venv` |
| `curl 127.0.0.1:8080` fails from WSL | NAT networking | Set `networkingMode=mirrored` (B5), then `wsl --shutdown`. |
| `hoot up` exits at once | A `models.ini` path is missing, or the llama.cpp build lacks an option | `hoot status`; check the paths in `models.ini`. `spec-type` and `reasoning` were only tested on the old build, so comment them out to test. |
| An Ollama model returns 401/403 | The key is wrong or revoked, or the plan doesn't include that model | Rerun `eidolon secret set ollama`; check the plan. |
| An Ollama model returns 429 | Too many concurrent requests or the allowance is used up | Wait, or run fewer agents at once. |
| The web UI says "this link has expired" or "not signed in" | The one-time link was used or is older than 30 seconds, or the server restarted | Type `hoot` again. (A bare `eidolon web` door still takes `#token=...`.) |
| The web page says "not answering" a while after `hoot`, and `hoot status web` says down | The Minerva server (the perch) runs inside WSL and stops when WSL idles out with no window open | In `%UserProfile%\.wslconfig` add `[wsl2]` and `vmIdleTimeout=-1`, run `wsl --shutdown` once, then `hoot`. (A `systemd --user` unit would also work; not set up.) |

Anything not covered here: ask in the club channel, and add the answer to this table.

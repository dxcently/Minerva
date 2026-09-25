# Triage toolkit: structured problem-solving on JEV, benchmarked on a practice image

**2026-09-25. Design only: no code, no downloads, no VM started.** Evidence labels follow `triage.md`:

| Label | Meaning |
|---|---|
| **observed** | Read in code or on a page; the source is named. |
| **reported** | Someone else's claim, not reproduced. |
| **reasoned** | Argued from observed facts. |
| **hypothesis** | Believed but not measured; says what would measure it. |

**Scope is defensive only.** The agent investigates and remediates a practice VM the operator owns, on a network with no route out.

---

## 0. The answer in one paragraph

Build a thin toolkit on Minerva's own JEV: six primitives, and every competition or incident-response workflow is a JEV graph plus an exercise manifest over them. The primitives are `target`, `adapter`, `case`, `remediation`, `script` and `exercise`.

The work splits three ways:

- **Any eidolon model plans.** It picks the next question, writes graphs, and writes helper scripts.
- **JEV executes and decides.** It holds state, budgets and `choose` points. Every effect goes through eidolon's gate.
- **Adapters are the only way to reach a target.** They are argv-only wrappers over standard CLI tools. A target is a named endpoint on an isolated network.

**Neither Laya nor OpenJEV replaces JEV.** Both make one decision at a time; neither is an executor. OpenJEV is already JEV's guard checker. Laya is a candidate to replace jevlike as the chooser, because it claims calibrated probabilities; that needs testing.

**Graph rendering uses `@xyflow/system`** (the framework-free core React Flow is built on), vendored once as one ESM file, with nodes drawn in Preact + htm. `@xyflow/react` through `preact/compat` is the fallback, used only if a spike passes. Graphs reach chat two ways: a renderer for `jev_run`/`jev_resume` tool lines, and a fenced `jev` block in model text. That takes one new read-only hub route, `GET /hub/jev/graphs/<id>`, and no new frame type.

**The benchmark is BotForge**, an OVA built for VMware. It runs under QEMU/KVM inside WSL2 with no outbound network and a throwaway overlay per run. A separate process, run as a separate Linux user, holds the answers and does the scoring. During a benchmark run the agent never has web tools. Generalization is checked against the author's Fedora image and against held-out questions.

---

## 1. What exists (observed)

### JEV is already the executor

- It runs an XState-subset graph. Each state runs observe, check, choose, act.
- The interpreter "never executes anything". The Rune driver dispatches each request through eidolon's gate (`docs/design/automation.md`, "The model in one page", §2).
- Budgets (`steps`, `actions`, `wall_s`, `escalations`) and the warrant block live in `meta.jev` (`extensions/jev/graphs/ctf-juice-recon.json`).
- The service methods are `automation.lint`, `start`, `step`, `status`, `answer`, `order`, `stop`, `runs` and `warrant` (`jev/server.py:635-643`).

### The chooser and checker today

- **jevlike** is the chooser: a 169 KB byte-model trained on synthetic data (`docs/Architecture.md`, "The jev sidecar").
- **openjev** is the checker: Qwen3.5-4B as a 3-way NLI cross-encoder, about 9 GB (same source).
- The chooser is not calibrated: 4.48% against 3.62% chance (`triage.md` §4.1, citing S47).

### Swapping the chooser for a Jev-family model (`jev/automation/systemone.py`)

The jev service reads one chooser from its environment. With `JEV_CHOOSER` unset, jevlike stays the chooser. Otherwise the spec is `systemone:<base_url>#<model>`. It sends every menu as one `choice` question to `POST <base_url>/v1/systemone`. The wire format is TypeSafe's (https://docs.typesafe.ai/api.md), which jev.cpp's `--system-one` server also speaks (https://github.com/thomasgauthier/jev.cpp, `tools/server/README.md`).

| variable | local jev.cpp | hosted Jev API |
|---|---|---|
| `JEV_CHOOSER` | `systemone:http://127.0.0.1:8081#autojev` | `systemone:https://api.typesafe.ai#jev-latest` |
| `JEV_CHOOSER_ALLOW_REMOTE` | unset | `1` (opt-in: the context leaves the box) |
| `JEV_CHOOSER_KEY_FILE` | unset, unless the server runs with `--api-key` | path to a file holding only the key |
| `JEV_CHOOSER_TIMEOUT_S` | default `10`, one deadline from connect to last byte | same |

The local server is started with `llama-server -m <autojev|decider|jevk5>.gguf --system-one --host 127.0.0.1 --port 8081`.

- **Refused at service start.** Plain `http` to any non-loopback host, a remote host without the opt-in, credentials or a query in the URL, and a missing `#model`.
- **Park, never pick.** A timeout, a non-2xx reply, a redirect, a reply that isn't JSON or repeats a key, a score missing or extra for an option, a NaN, inf or out-of-range probability, probabilities that don't sum to 1, an unreadable key file, and any unexpected error all raise `run.ChooserUnavailable`. The choice then parks with `why: "chooser unavailable: ..."`, and the menu shows `p: 0` because eidolon's runs view drops an option whose `p` isn't a number. A refused connection is retried once, and nothing else is retried.
- **The key is read per call**, from the file only. It goes into the `Authorization` header and nowhere else: never into the spec, the errors, the escalation or the decision log.
- **Decision rows gain `chooser`.** It is `{spec, model, served_model, latency_ms}`, where `model` is the one asked for and `served_model` is the reply's own name, capped at 128 characters. On a park it is `{spec, model, latency_ms, error}` and the row's `probs` is `null`, because nothing was scored. It is `null` for jevlike, where `ckpt` already names the model. Older rows simply lack the key. An HTTP chooser's own pick has `source: "chooser"`; jevlike's is still `"jevlike"`.

### Triage rulings to reuse (`triage.md` §§2-6)

- Typed evidence goes through `capture` into `context.case`.
- A `reads` warrant clause (proposed, not built).
- "The graph is the chain": no control flow inside command strings.
- A mechanical ending can decide on its own. An interpretive ending parks instead of picking.
- One graph per platform, with shared labels.

### SSH is the way into other boxes (`ssh-triage.md` §1)

- Commands go as `exec("ssh", argv)`, never through a local shell.
- `BatchMode=yes` and `StrictHostKeyChecking=yes`.
- The host allow-list comes from config, never from model input.
- Read-only by default. Write actions over ssh are "a separate `.rn` file with its own policy rule" (§0 non-goals).

### The web UI

- `webui/term/` vendors preact 10.29.8, @preact/signals 2.11.2 and htm 3.1.1, with no build step (`webui/term/vendor/VERSIONS`).
- The door's CSP is `default-src 'self'` (`web-ui.md` P4).
- Renderers load through `manifest.json` + `import()`, because an inline importmap would break the CSP (`web-ui.md` §10).
- The `graph` inspector tab states that the jev graphs from the archived line are out (`web-ui.md` §8). This plan brings them back as a renderer in chat, not in the inspector.

### The earlier graph library

- It was `@xyflow/svelte`, built once with Vite and embedded with `include_str!`.
- Size: 335,430 B of JS and 15,678 B of CSS, from 13 MIT, ISC and BSD-3 packages.
- Two build settings mattered: `cssCodeSplit: false` and a `define` for `process.env.NODE_ENV`.
- The CSP was checked in a browser (`docs/Build-Log.md` ~1827-1850):
  - `setAttribute('style')`: blocked.
  - An injected `<style>` element: blocked.
  - A CSSOM write: works.

### The pieces this plan builds on

- **`webui/graph-adapter.js`** already turns a JEV graph into xyflow-shaped `{nodes, edges}`:
  - node types `state` and `stateGroup`, each with a `position`;
  - edges with `markerEnd: {type: "arrowclosed"}`;
  - its own `layout()`;
  - `choose` nodes drawn as four bands.
- **`jev_run` already returns** `outcome`, `final_state`, `output` and `path`, or an escalation (`extensions/jev/tools/run.rn`, manifest description). That is enough to highlight a run on its graph.
- **`bin/ctf-lab.ps1`** is the pattern for a local lab: a pinned container bound to `127.0.0.1` with `--cap-drop ALL`. Its paired graph, `ctf-juice-recon`, lists its literal commands in the warrant.

---

## 2. Primitives

**The rule for adding a primitive:** it must be something a JEV graph can't express, and it must carry a safety or scoring meaning. Anything else is a graph or an adapter file.

| name | what | why it has to be a primitive | what it is NOT |
|---|---|---|---|
| **target** | A named execution endpoint from operator config: `{name, transport: ssh\|local-ro, host alias, user, key ref, network: isolated\|loopback, class: practice\|club-box}`. The model names a target. It never supplies a host string. | It's where the "no external systems" boundary becomes concrete. Allow-listing by name is already ruled (`ssh-triage.md` §1). Only a `practice` target may be changed by a remediation. | Not a hostname field, not a network scanner, and not a way to add hosts at runtime. |
| **adapter** | A typed wrapper over one CLI tool, as one `.rn` tool file or a table row. It has an argv template, a class (`read` or `change`), a timeout, and `capture` patterns that turn output into typed `case` keys. It runs on a target through `exec`/ssh, argv only. | It gives the gate a real command to classify, and it gives the case typed facts (`triage.md` §3). A new tool is a new file, so the toolkit grows without core changes. | Not a free-form shell. No loops, `if` or `cd` inside it (`triage.md` §2.6, §5). |
| **case** | The run's evidence store: typed keys, questions with draft answers and the evidence each rests on, and findings. It is the existing `context.case`, lifted to exercise level. | The planner, executor, report and scorer all need the same structured state. Free-text notes can't carry a chain of evidence (`triage.md` §3.3). | Not the transcript, and never raw output blobs. |
| **remediation** | One change as a unit: `{finding, precheck (adapter, read), change (adapter, change), postcheck (read), rollback note}`. It only counts if the postcheck passes. | Scoring rewards changes that are verified and penalizes breakage. The postcheck is JEV's test for "done" on a mechanical case. | Not a bare command. Never allowed on a target that isn't `class: practice`, unless that one change is explicitly approved. |
| **script** | A helper the model writes, like an analyst's one-off: `{lang: sh\|python, body, purpose, class: read\|change, sha256}`. It is copied to the target and run there through the adapter layer. | It fills gaps in the adapter set without widening the gate. The hash is approved, the body is logged in the case, and it runs on the target only. | Never run on the host or in WSL. Not persistent: becoming a tool means a human PR promotes it to an adapter. It can't make network calls, because the target has no route out. |
| **exercise** | A manifest: `{id, targets, time box, question files (paths on target), scenario README path, deny-paths, scorer handle (opaque)}`. | It binds the time box, scope and deny list for one run. It is the unit the benchmark loop resets and scores. | It never holds answers or scoring rules. The scorer handle is a name the harness resolves outside the agent's reach. |

**Deliberately not primitives:**

| Need | Where it lives instead |
|---|---|
| The time box | JEV's `wall_s` and `steps` budget. |
| A question loop | A graph with `choose` over `lines` of the question list. |
| ATT&CK lookup | An adapter that reads a local `enterprise-attack.json`. |
| The scorer | Outside the toolkit, on purpose (§4). |

### The three roles (reasoned)

- **LLM planner.** Any eidolon model, over hoot or cloud providers. It:
  - reads the exercise README and the questions;
  - proposes hypotheses;
  - picks or writes graphs;
  - writes `script`s;
  - answers the interpretive escalations JEV parks on (the Judge's seam, `triage.md` §2.6).
- **JEV executor.** It:
  - runs the graph and enforces the budgets;
  - calls `capture`;
  - runs each remediation's precheck, change and postcheck;
  - writes the decision log;
  - parks whenever a decision is interpretive.
- **Adapters.** The only way to affect a target. They go through eidolon's dispatcher, and the policy table classifies each one.

---

## 3. Architecture

```
            operator (approves, answers parks)
                        |
   +--------------------v---------------------+      WSL2 user: minerva
   |  eidolon session (gate, journal)         |
   |                                          |
   |  LLM planner  <-- escalations --  JEV executor (jev service
   |  (plan, graphs, scripts) --graph/order--> + run.rn driver)
   |                                          |   case / budget / choose
   |                           dispatch every action through the gate
   |                                          v
   |               tool adapters (argv only; target by NAME)
   |               vm_read.rn  vm_change.rn  vm_script.rn  attack_lookup.rn
   +-------------------------|-----------------+
                             | ssh, key=agent_key, 127.0.0.1:2222 only
                             v
   +-----------------------------------------------+
   | QEMU/KVM in WSL2: BotForge overlay.qcow2      |  user-mode net, restrict=on:
   |  (base image read-only, overlay per run)      |  no outbound; only the hostfwd
   |  in-VM scoring engine -> scoring report       |  ssh port is exposed
   +-----------------------------------------------+
                             ^
                             | ssh, key=scorer_key (separate account)
   +-------------------------|------------------------------+
   | SCORER (WSL2 user: minerva-scorer, home 0700)          |
   |  answers/ (writeup-derived key, never under minerva's  |
   |  reach), reads scoring report + Forensics files        |
   |  after the run ends -> results.jsonl                   |
   +--------------------------------------------------------+
   bench.sh (as minerva-scorer): reset overlay -> boot -> start agent run ->
   wait (time box) -> stop agent -> score -> destroy overlay
```

### Policy boundary (reasoned; new rows in `policy.rn`)

| surface | rule |
|---|---|
| **where commands run** | Adapters run only on a named `target`. In an exercise profile, the agent's local `bash` is limited to reads the table already classes as read-only, inside the exercise workspace. `script` runs on a target only. A local `python` or `sh -c` stays `FLAG` (observed row). |
| **network** | `web_fetch`, `web_search` and `browser_*` are off in the benchmark profile, since they could fetch the writeup. The VM uses QEMU `restrict=on`, so the guest has no route out. `vm_*` adapters accept only target names that resolve to `127.0.0.1:<hostfwd port>`. Any other host is refused by the tool before dispatch, not by the model. |
| **reads on a target** | `vm_read` classifies the remote argv with the same harnox leaf table: read-only composes per stage (`triage.md` §2.1). Read-only commands are covered by a `reads` warrant; anything else asks. |
| **changes on a target** | `vm_change`, and `vm_script` with `class: change`, are allowed only when the target is `class: practice` **and** a base snapshot exists. Under the exercise warrant they are capped by `actions`; outside it, each one asks. The three refusals on the merits still stand: privilege escalation on the host, power-off, and destroying a filesystem. `sudo` inside the practice VM is expected, so the privilege refusal applies to the host, not the target. |
| **deny-paths** | The exercise manifest has a `deny` list (the scoring engine's directory and report path, filled in by the operator). `vm_read` refuses those paths whatever the command, as a prefix check on argv paths. This is advisory only, because a script could still reach them, so §4 doesn't rely on it. |
| **approvals** | Once per run: the exercise warrant (graph ids, target, `reads: true`, change cap, wall time). Per item: any new `script` hash, and any change outside the warrant. |

---

## 4. Benchmark: BotForge, and answer hygiene

### 4.1 What the page says (observed; https://tirefire.org/posts/botforge-practice/, fetched 2026-09-25, post dated 2026-01-17)

- **The image:** Linux Mint 22, shipped as an OVA at `https://downloads.tirefire.org/botforge-practice-v2.ova`, MD5 `c5ef4ab19fd6cd9f79139ba6db2b65b9`.
- **Stated requirements:** "VMware Workstation, Player, or Fusion", 4 GB RAM (8 GB recommended), 20 GB disk. **The download size is not stated.**
- **The questions:** seven forensics questions (`Forensics1.txt`–`Forensics7.txt`) on the Desktop, each citing ATT&CK technique IDs.
- **The scenario:** a README on the Desktop describes it and lists the authorized users.
- **Scoring:** fixing a vulnerability shows a hint on the scoring report.
- **Page advice:** "Take a snapshot before you start."
- **Login credentials** are published on the page. They belong in operator config, not in this doc.
- **The official writeup** is `github.com/christopher-gholmieh/BotForge-Writeup`. It was **not opened** while writing this doc, so it isn't in this doc's context.
- **Not stated anywhere on the page:**
  - the scoring engine's name or location;
  - how the report is opened;
  - whether it needs internet;
  - the answer format;
  - a license or redistribution terms.

**The same author also published a Fedora 37 practice image** (https://tirefire.org/posts/fedora-practice/, 2022-11-26):

- It ships as a `.zip`, and the hypervisor isn't stated.
- It has "a lot more forensics questions than most images".
- Its writeup is by a different author (`github.com/Poarthan/mr.robot.sh-writeup`).

The site index also lists older posts; they weren't enumerated.

### 4.2 Hypervisor on Windows 11 + WSL2 (reasoned; the operator runs every install)

| option | how | caveats | verdict |
|---|---|---|---|
| **QEMU/KVM inside WSL2** | Convert the OVA's VMDK to qcow2 once with `qemu-img convert`. Each run gets an overlay qcow2. Networking is `-netdev user,restrict=on,hostfwd=tcp:127.0.0.1:2222-:22`. | Needs `/dev/kvm` in WSL2 through nested virtualization (**hypothesis**: check with `ls -l /dev/kvm`). The guest may expect VMware devices and need `e1000` plus SATA/AHCI (**hypothesis**, measured at first boot). Measure boot time under nesting. | **The default.** Isolation is one flag, reset means deleting the overlay, the agent and scorer are already in WSL, and Windows networking is untouched. |
| VMware Workstation Pro (free for personal use) | The supported path: import the OVA, use a host-only VMnet, snapshot and revert with `vmrun`. | Coexists with Hyper-V through WHP, which is slower. WSL2 in NAT mode may not reach VMnet1; `networkingMode=mirrored` probably fixes that (**hypothesis**). `vmrun` is a Windows exe called from WSL. | The fallback if QEMU won't boot the guest. |
| VirtualBox 7 | Import the OVA. | Also runs on WHP, which is known to be slow. VMware-built OVAs sometimes need storage controller fixes. | Not recommended. |
| Hyper-V | Convert the disk to VHDX. | Needs a disk conversion and a different NIC model, and has no clean OVA import. | Not recommended. |

### 4.3 How the agent reaches the VM (reasoned; the footprint is measured)

Mint's desktop edition probably doesn't ship `openssh-server` (**hypothesis**; check at first boot). Installing it and adding keys changes the image, and the scoring engine might penalize an extra network service. So the operator prepares the image once:

1. Boot the base image and log in on the console.
2. Install `openssh-server` from a `.deb` copied in offline.
3. Add `agent_key` to the scenario's main user.
4. Add `scorer_key` to a separate `scorer` account that can only read the report path.
5. Run the scoring engine once and **record the baseline score**.
6. Freeze the result as `base-harness.qcow2`.

If step 5 shows sshd being penalized, switch to a serial-console getty on `ttyS0` over a unix socket, as a `vm_serial` transport on `target`. The footprint is recorded in `exercise.harness_footprint`, so results say exactly what was changed.

### 4.4 Answer hygiene: the scorer can't leak (reasoned)

1. **A separate process run by a separate Unix user.**
   - `minerva-scorer` owns `~minerva-scorer/answers/`: mode 0700, on ext4 inside WSL, not under `/mnt/c`.
   - The agent runs as `minerva`. The policy table refuses host `sudo` (observed class: privilege escalation).
   - The scorer is started by `bench.sh` running as `minerva-scorer`. It is never an eidolon tool.
2. **The operator types the answers by hand** into a small `answers.toml`, with the accepted forms for each question. The writeup itself is never cloned anywhere `minerva` can read.
3. **No way to look anything up.**
   - In the benchmark profile, web tools, `browser_*`, `mneme_rpc` and the swarm/peer tools are off.
   - `bench.sh` generates the run's eidolon config and diffs it against an allow-list before starting.
4. **The in-VM report is a feedback channel.** It shows hints, which turns the benchmark into hill-climbing.
   - In **bench mode**, the report path and the engine directory are on the exercise deny list. Results record every `vm_read` that touched a deny-path (a leak-attempt counter).
   - In **practice mode**, the agent may read the report, and results are labelled that way.
   - The agent is root in the guest, so a script could still read the engine's files. Bench mode therefore also refuses `vm_script` bodies that contain deny-path strings, and the scorer checks the case log for engine paths after the run.
   - A hit invalidates the run. The goal is detection, not perfect prevention.
5. **What gets scored:**
   - (a) The Forensics answers the agent wrote into `ForensicsN.txt` on the target, compared by the scorer against `answers.toml`.
   - (b) The points on the in-VM scoring report, read by the scorer over `scorer_key` **after** the agent is stopped.
   - (c) The penalties from that report.
   - (d) Wall time and the number of approvals.

   Each run is one row in `results.jsonl`: `{image, sha, model, graphs@sha, mode, forensics k/7, points, penalties, leak_hits, wall_s}`.

### 4.5 Not overfitting to one image (reasoned)

- **Held-out image.** Fedora 37 from the same site (a different distro, package manager and writeup author). It is scored only at milestones, never while tuning.
- **Held-out questions.** Within BotForge, tune on questions 1–4 and report 5–7 separately. Do the same with the scoring-report categories.
- **An image-string lint.** CI greps the toolkit, graphs and prompts for strings specific to this image (its usernames, filenames, hostnames). The operator keeps that list on the scorer side, and a hit fails CI.
- **Home-made canaries.** An operator-run script plants 3–5 known misconfigurations in a clean Ubuntu cloud image, with answers written by the club. This grows the benchmark without depending on anyone else's writeup.
- **Variance and speed.**
  - Run each configuration three times.
  - The local 27B model runs at about 2.6 tok/s (observed, `Architecture.md`), which may not fit a time box.
  - Record which model ran on every row.

### 4.6 Setup the operator runs (the agent runs none of this)

1. **Windows:** `wsl --update`, and make sure nested virtualization is on. Then, in WSL, check that `/dev/kvm` exists.
2. **WSL packages (apt):** `qemu-system-x86`, `qemu-utils`, `openssh-client`. Optional: `libguestfs-tools`, for inspecting the disk offline.
3. **The image:**
   - Download `botforge-practice-v2.ova` (size not stated; plan on about 20 GB of disk) and verify its MD5.
   - Unpack it with `tar`.
   - Convert the disk with `qemu-img convert -O qcow2` to `botforge-base.qcow2`.
4. **First boot on the console:**
   - install `openssh-server` offline;
   - create the keys and the `scorer` account;
   - run the engine and record the baseline;
   - freeze `base-harness.qcow2` read-only.
5. **The scorer user:** create `minerva-scorer`, and type in `answers.toml` while reading the writeup on Windows, outside WSL.
6. **ATT&CK data:** download MITRE's `enterprise-attack.json` (github.com/mitre/cti) into the exercise's read-only data directory.
7. **Later, Fedora:** download from the author's mirror, verify the MD5, and repeat steps 3–5.
8. **Optional, once:** build the xyflow bundle (§5). Use Node + Vite in a throwaway directory, then delete `node_modules`, as was done for D3.

**Reset between runs.** Delete the overlay and create a fresh one on the base, which is never written:

```sh
rm overlay.qcow2 && qemu-img create -f qcow2 -b base-harness.qcow2 -F qcow2 overlay.qcow2
```

That is the snapshot the page recommends.

---

## 5. Graph rendering

### 5.1 The pick: `@xyflow/system` with Preact + htm; `@xyflow/react` via `preact/compat` only after a spike

| route | what you vendor | size | fit |
|---|---|---|---|
| **A. `@xyflow/react` via `preact/compat`** | A one-time Vite library build with `react`/`react-dom` aliased to `preact/compat`. Output is ESM that imports the already-vendored `preact.js` by relative path (no importmap, since the CSP forbids an inline one). Also vendor `preact/compat` and React Flow's CSS. | **Hypothesis:** about 200–350 KB of JS, similar to the 335 KB Svelte Flow bundle. Measure it in the spike. | You get React Flow's components (MiniMap, Controls, NodeResizer). The risk: React Flow's store relies on `useSyncExternalStore`, React 18 batching and `forwardRef` semantics that compat only imitates (**hypothesis**). If the bundle pulls in a second Preact copy, hooks break without any error. |
| **B. `@xyflow/system`** (the pick) | A one-time Vite build of `@xyflow/system` and its d3 dependencies (`d3-zoom`, `d3-drag`, `d3-selection`, `d3-interpolate`) as one ESM file. Nodes, edges and handles are written in Preact + htm and fed by `graph-adapter.js`. | **Hypothesis:** about 60–100 KB of JS. | This is the shared core React Flow and Svelte Flow both run on: pan and zoom, dragging, connections, edge-path math. Minerva writes its own node components, which it wants anyway: the four-band `choose` node, square frames, monospace text, all from theme variables. There's no default React Flow look to strip off and no compat layer. The cost is roughly 400–600 lines of Preact for nodes, edges, selection and fit-view (reasoned). |
| C. No library | SVG from `graph-adapter.js`'s own `layout()`, with hand-written pan and zoom. | ~0 | Enough for read-only graphs in chat, not for an editor. It's the fallback if B's bundle fails under the CSP. |

**The call is B.** It gives React Flow at the engine level, keeps a single virtual DOM, and fits the no-build rule better: one frozen file, pinned in `vendor/VERSIONS` with its sha512 like the others.

**Take A only if** the spike (milestone T6a) shows compat handling React Flow's store with no second Preact copy, and a bundle under 350 KB.

**CSP (observed and reasoned):**

- **Scripts** are covered by `script-src 'self'`: the bundle is a local file.
- **Styles** are covered by `style-src 'self'`:
  - xyflow and Preact set node transforms and sizes through CSSOM (`el.style.*`, `setProperty`).
  - The Svelte Flow check showed that CSSOM writes work under this CSP, while `setAttribute('style')` and an injected `<style>` are blocked (observed, Build-Log).
  - Preact turns a string `style` prop into `style.cssText`. That is believed to count as CSSOM but is **not verified**, so the rule is object-form styles only.
- **Library CSS** ships as a vendored `.css` link, built with `cssCodeSplit: false` (observed from the Svelte build).
- **Arrow markers** are inline SVG, not `data:` images, so `img-src` doesn't matter.

**Restyling (reasoned):**

- `border-radius: 0` and the monospace cell font.
- 1px frames with the title cut into the border.
- Colours from the `theme.css` variables, and a grid that snaps to `--cw`/`--ch`.
- Edges use `getSmoothStepPath` with `borderRadius: 0`, giving square, terminal-style edges.
- No gradients.

### 5.2 Where graph output in chat comes from

1. **`jev_run`, `jev_resume` and `jev_runs` tool lines.**
   - A renderer, `renderers/jev_run.js`, is registered in `manifest.json` (`web-ui.md` §10).
   - It parses the tool output (`path`, `final_state`, `outcome`, escalation `options`) as JSON, never as HTML.
   - It fetches the graph document from a **new read-only hub route**, `GET /hub/jev/graphs/<id>`, which returns `{id, version, sha256, doc}`.
   - The route only accepts `id` matching `^[a-z0-9][a-z0-9-]{0,63}$`, and only serves files in the configured `extensions/jev/graphs/` directory. It follows no symlinks out, with the same containment rules as `/hub/tree` (`hub.md` §7).
   - The run's path is drawn highlighted, and a parked state glows `--yellow`.
2. **Graphs the model writes.** A fenced block with language `jev` in assistant text. `core/markdown.js` hands the fence to the same component, which runs `JSON.parse` and shows a read-only view. The page doesn't lint; linting happens when the model calls the jev lint path.
3. **Live step progress (later).** This needs a door frame type, a sibling of the `park-tick` (gap 7 in `web-ui.md`), because `jev_run` only returns at the end or at a park. Deferred: **no new frame type now**.

"Similar to how Melete's UI renders graphs" is **reported** by the user. Melete's UI wasn't examined.

---

## 6. Substrates evaluated (fetched 2026-09-25)

| candidate | what it actually is | license | maturity (evidence) | fit against Minerva JEV |
|---|---|---|---|---|
| **Laya** — huggingface.co/convaiinnovations/laya; code at github.com/NandhaKishorM/laya | A **decision model**, non-autoregressive ("System 1"). ModernBERT-large, 421M parameters, English, 512 context; there is also a 322M multilingual variant on mmBERT-base. It answers `choice`, `score` and `noul` (yes/no) questions in one forward pass, with probabilities it says are calibrated. It's trained with "RLCD" against strictly proper scoring rules. It claims to be about 7.8× faster than TypeSafe Jev (observed, model card). | Apache-2.0 (observed) | It has a pip package, `laya-serve` (FastAPI), an MCP server, tests and `BENCHMARKS.md` (observed via a fetch summary). Reported CPU time is about 193–464 ms per decision. The star and commit counts came from a summarizing fetch and are **unverified**. It makes single decisions only: no state machine and no multi-step orchestration. | **Not a substrate.** It is a candidate to replace **jevlike** behind `choose`. Calibration is exactly what jevlike lacks (S47). **Test:** replay S47's calibration check on a triage decision log, Laya against jevlike, on CPU within this box's latency budget. At 421M parameters against 169 KB, it should load once as a machine-scoped extension service. |
| **OpenJEV** — huggingface.co/AlexWortega/openjev | Real. Qwen3.5 as a 3-way NLI **cross-encoder**: premise + hypothesis → entailment, contradiction or neutral. Checkpoints: 0.8B v2s, 2B v5, 4B v5 (recommended), 4B v2 (images), and a 35B-A3B MoE. It loads through a custom `OpenJevCrossEncoder` (observed, card). | MIT (observed) | The card reports held-out scores: MNLI 0.896, RAGTruth AUROC 0.932, HaluBench AUROC 0.937. Download counts aren't tracked. Other projects with the same name (tellsiddh/jev "open-jev", ZefanCai/Open-Jev-9B, cappuch/openjev.cpp) are **unverified**. | **Already inside Minerva** as `openjev`, JEV's guard checker (`Architecture.md`). Not an executor. **Action:** check which checkpoint the box runs against v5. 2B v5 might be a cheaper checker than the ~9 GB 4B (hypothesis). |
| **Minerva JEV** — `jev/automation/`, `extensions/jev/` | A statechart executor: an XState subset, observe/check/choose/act, budgets, warrants, parking and escalation, and a decision log. Every effect is dispatched through eidolon's gate (observed, `automation.md`). | the repo's | Five graphs shipped, and the live end-to-end trial J1 ran (observed, Build-Log). `reads` and `capture` are proposed, not built (`triage.md`). | **The substrate.** It is the only one of the three that executes anything, and the only one wired into the gate. The toolkit needs `capture` and `reads` built first. |

---

## 7. Milestones

| # | what | done when |
|---|---|---|
| **T0** | Operator setup (§4.6), plus the harness footprint and baseline | `bench.sh reset && bench.sh up` boots the overlay. `ssh -p 2222 -i agent_key` works. The scorer reads the baseline report. The operator runs `curl` from the guest and it fails, proving there's no outbound network. Boot time is recorded. |
| **T1** | Prerequisites from `triage.md`: `capture`, the `reads` envelope key, `Ruling.read_only` | `triage.md` spec steps 1–5 pass, including the empty-match lint. |
| **T2** | The `target`, `adapter` (`vm_read.rn`), `case` and `exercise` primitives, plus the policy rows | A JEV graph lists the VM's users and diffs them against the README's authorized list, unattended under a `reads` warrant, with typed keys in the case. Naming a host that isn't in config is refused before dispatch, and a test says so by name. |
| **T3** | Scorer and loop: `bench.sh`, `minerva-scorer`, `results.jsonl` | A null agent run produces a baseline row. The leak tests pass: `minerva` can't read the answers directory; the benchmark config has no web, browser or mneme tools; a planted deny-path read shows `leak_hits: 1`. |
| **T4** | Forensics: the planner, plus a question-loop graph that writes answers into `ForensicsN.txt` | Three bench-mode runs on BotForge give a k/7 score with its spread, split between tuned questions (1–4) and held-out ones (5–7). |
| **T5** | `remediation` and `vm_change.rn` | Points beat the baseline with zero penalties across three runs. Every change in the case has a passing postcheck and a rollback note. |
| **T6a** | Graph spike | B's bundle is built and pinned, with no CSP errors in the console and its size recorded. A's spike has run, and the pass or fail verdict against §5.1's gate is written down. |
| **T6b** | Graph renderer and `GET /hub/jev/graphs/<id>` | A `jev_run` in chat draws its graph with the path highlighted. A fenced `jev` block renders. Tests show the hub route refuses `../`, symlinks and bad ids. |
| **T7** | `script` primitive | A read helper the model wrote runs on the target after one hash approval, and its body is in the case. A request to run it locally is refused by name. A second run with the same hash asks nothing. |
| **T8** | Generalization | Fedora 37 is scored with no toolkit change. The image-string lint passes. The canary image is scored. |

T0 and T6a/T6b can run in parallel with T1–T2.

---

## 8. Risks

| Risk | Detail | Mitigation |
|---|---|---|
| The OVA may not boot under QEMU | It may depend on VMware-specific drivers. | Fall back to VMware Workstation with mirrored WSL networking (§4.2). |
| The access path changes the image | sshd and the added keys might be scored as findings. | Measure at baseline (§4.3). Fall back to a serial console. |
| The scoring engine is unknown | It might need internet, a clock or a GUI session, or detect tampering. | Found at T0. If it phones home, `restrict=on` breaks it, and that gets recorded as part of the footprint. |
| Leaks through the engine's files inside the VM | The agent is root in the guest, so the deny list is only advisory. | Detection plus invalidating the run is the real control (§4.4). |
| Local model speed | Qwen 27B runs at about 2.6 tok/s, against a time box. Every cloud provider is dark today (`Architecture.md`). | Results name the model. A club member with an API key changes the numbers. |
| Chooser calibration | jevlike is at chance (S47). | Interpretive decisions must park and go to the LLM, not be picked. Test Laya. |
| Compat fragility (route A) | React Flow's store may not work through `preact/compat`. | That's why A sits behind a spike. |
| No stated license for the image or writeup | Redistribution terms are unknown. | Nothing from either is redistributed or committed. `answers.toml` stays local to the scorer user. |
| Maintenance | The toolkit must survive long stretches unmaintained. | Six primitives, `.rn` adapter files, one frozen vendored bundle, no runtime build. Rebuilds are rare and recorded in `vendor/VERSIONS`. |

---

## 9. Open questions (default in the right column)

| # | question | default |
|---|---|---|
| Q1 | React Flow's components (A) or xyflow's engine (B)? | B, and run A's spike anyway |
| Q2 | Hypervisor | QEMU/KVM in WSL2; VMware if the guest won't boot |
| Q3 | How the agent gets into the VM | sshd + keys added to the base image, measured at baseline; serial console if it's penalized |
| Q4 | May the agent read the in-VM scoring report? | No in bench mode; yes in practice mode, with results labelled |
| Q5 | Which models to benchmark | The local Qwen 27B as the floor, plus any cloud model a member supplies a key for. Always reported per model |
| Q6 | Remediation approvals in bench mode | Covered by the exercise warrant up to an `actions` cap; anything above the cap asks |
| Q7 | When to test Laya as the chooser | After T4, on the triage decision log T4 produces |
| Q8 | Which openjev checkpoint | Keep the current one; test 2B v5 as a cheaper checker at T4 |
| Q9 | A frame type for live progress | Defer; the tool-line renderer and hub route come first |
| Q10 | Where the canary images come from | A club-written script over an Ubuntu cloud image, run by the operator |

### Files an implementation will touch

- `jev/automation/graph.py`: lint for `reads` and `capture`.
- `extensions/jev/tools/run.rn`: the driver, and the output shape the renderer reads.
- `webui/graph-adapter.js`: JEV graph → xyflow nodes and edges.
- `webui/term/renderers/`: the renderer manifest hook.
- `docs/design/hub.md`: add `GET /hub/jev/graphs/<id>`.

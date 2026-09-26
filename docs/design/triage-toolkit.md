# Triage toolkit: a competitor agent on JEV, from a fresh Aeacus image to solved

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

Run an agent as a competitor against a fresh Aeacus-scored practice image and take it from **0/256 to solved** using only general-purpose tools: eidolon's generic tools and JEV's generic triage graphs. Nothing BotForge-specific is written from the writeup. The only thing added to the image is the agent's ssh key (`mford`); the agent reads `ReadMe.txt` in the VM like any competitor, and problems like broken `sudo` are its job to fix.

The toolkit is thin: six primitives on Minerva's own JEV — `target`, `adapter`, `case`, `remediation`, `script`, `exercise` — and every workflow is a JEV graph plus an exercise manifest over them.

The work splits by cognition, not by vendor:

- **DeepSeek is the brain (§4).** It reads findings, decides where to go next, and authors *new* JEV graphs/scripts when none in the library fits.
- **JEV is the hands (§4).** It runs the triage graphs over ssh, acts, and records. A Jev-family chooser handles the mechanical single-object choices.

Escalation follows standard SOC tiers tagged with MITRE **D3FEND** tactics (§5), from read-only recon (T0) to irreversible access-path changes under an incident commander (T3); four invariants protect the agent's own lifeline at every tier. Findings are tagged **ATT&CK**, actions **D3FEND** (§6). Every new graph a run authors is validated and auto-kept in a shared library; each run records the library snapshot it started from, so runs stay comparable.

The runner is **`minerva bench`** (§7), a Rust binary: reset → boot → start eidolon + JEV with DeepSeek → poll Aeacus over the qemu console → stop at the time box or when solved → report. **Graph rendering (§9)** uses `@xyflow/system` in Preact + htm. Generalization is checked against the author's Fedora image and held-out questions (§7.5).

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

### The pieces this plan builds on

- **`webui/graph-adapter.js`** already turns a JEV graph into xyflow-shaped `{nodes, edges}`: node types `state` and `stateGroup` with `position`; edges with `markerEnd: {type: "arrowclosed"}`; its own `layout()`; `choose` nodes drawn as four bands.
- **`jev_run` already returns** `outcome`, `final_state`, `output` and `path`, or an escalation (`extensions/jev/tools/run.rn`, manifest description). That is enough to highlight a run on its graph.
- **`bin/ctf-lab.ps1`** is the pattern for a local lab: a pinned container bound to `127.0.0.1` with `--cap-drop ALL`. Its paired graph, `ctf-juice-recon`, lists its literal commands in the warrant.

---

## 2. Primitives

**The rule for adding a primitive:** it must be something a JEV graph can't express, and it must carry a safety or scoring meaning. Anything else is a graph or an adapter file.

| name | what | why it has to be a primitive | what it is NOT |
|---|---|---|---|
| **target** | A named execution endpoint from operator config: `{name, transport: ssh\|local-ro, host alias, user, key ref, network: isolated\|loopback, class: practice\|club-box}`. The model names a target. It never supplies a host string. | It's where the "no external systems" boundary becomes concrete. Allow-listing by name is already ruled (`ssh-triage.md` §1). Only a `practice` target may be changed by a remediation. | Not a hostname field, not a network scanner, and not a way to add hosts at runtime. |
| **adapter** | A typed wrapper over one CLI tool, as one `.rn` tool file or a table row. It has an argv template, a class (`read` or `change`), a timeout, and `capture` patterns that turn output into typed `case` keys. It runs on a target through `exec`/ssh, argv only. | It gives the gate a real command to classify, and it gives the case typed facts (`triage.md` §3). A new tool is a new file, so the toolkit grows without core changes. | Not a free-form shell. No loops, `if` or `cd` inside it (`triage.md` §2.6, §5). |
| **case** | The run's evidence store: typed keys, questions with draft answers and the evidence each rests on, and findings. Findings carry ATT&CK tags and actions carry D3FEND tags (§6). It is the existing `context.case`, lifted to exercise level. | The planner, executor, report and scorer all need the same structured state. Free-text notes can't carry a chain of evidence (`triage.md` §3.3). | Not the transcript, and never raw output blobs. |
| **remediation** | One change as a unit: `{finding, precheck (adapter, read), change (adapter, change), postcheck (read), rollback note}`. It only counts if the postcheck passes. | Scoring rewards changes that are verified and penalizes breakage. The postcheck is JEV's test for "done" on a mechanical case. | Not a bare command. Never allowed on a target that isn't `class: practice`, unless that one change is explicitly approved. |
| **script** | A helper the model writes, like an analyst's one-off: `{lang: sh\|python, body, purpose, class: read\|change, sha256}`. It is copied to the target and run there through the adapter layer. | It fills gaps in the adapter set without widening the gate. The hash is approved, the body is logged in the case, and it runs on the target only. | Never run on the host or in WSL. Not persistent: becoming a tool means a human PR promotes it to an adapter. It can't make network calls beyond the run's apt cache (§7). |
| **exercise** | A manifest: `{id, targets, time box, question files (paths on target), scenario README path, deny-paths, library snapshot, scorer handle (opaque)}`. | It binds the time box, scope, library snapshot and deny list for one run. It is the unit `minerva bench` resets and scores. | It never holds answers or scoring rules. The scorer handle is a name the harness resolves outside the agent's reach. |

**Deliberately not primitives:**

| Need | Where it lives instead |
|---|---|
| The time box | JEV's `wall_s` and `steps` budget. |
| A question loop | A graph with `choose` over `lines` of the question list. |
| ATT&CK / D3FEND lookup | An adapter reading `crates/attack`'s local MITRE data (§6). |
| The escalation ladder | A policy over tiers, not a data type (§5). |
| The graph library | A validated, auto-kept store (§6), not a new data type. |
| The scorer | Outside the toolkit, on purpose (§7). |

---

## 3. Architecture

```
        operator (approves parks at T3+; off for benchmark runs)
                    |
+-------------------v---------------------------+  WSL2 user: minerva
|  eidolon session (gate, journal)              |
|                                               |
|   DeepSeek = BRAIN         JEV = HANDS         |
|   (analyst / commander)    (jev service +      |
|      ^        |            run.rn driver)      |
|      |        | next graph+params, or a        |
| findings,     | new *validated* graph          |
| evidence,     v          |                     |
| parked   Jev-family chooser (T1 only)          |
| decisions               |                      |
|                         | dispatch every       |
|                         v action via the gate  |
|      tool adapters (argv only; target by NAME) |
|      vm_read.rn  vm_change.rn  vm_script.rn     |
|      attack_lookup.rn                           |
+-------------------------|----------------------+
                          | ssh, key=mford, 127.0.0.1:2222 -> guest :22
                          v
+------------------------------------------------------+
| QEMU/KVM in WSL2: BotForge overlay.qcow2             | user-mode net:
|  (base read-only, overlay per run)                   |  restrict=on;
|  Aeacus scoring engine -> ScoringReport (in-VM)      |  egress = apt cache
+------------------------------------------------------+  only (guestfwd)
                          ^ qemu console (NOT ssh)
                          |
   minerva bench (Rust; runs as its own user):
     reset overlay -> boot -> start eidolon + JEV w/ DeepSeek ->
     poll Aeacus via console -> stop at time box or solved -> report.
   Answer key (writeup T-IDs, forensics) stays HERE, never in the guest.
```

### Architecture rule (reasoned; decided 2026-09-25)

- **Rust core, Rune extensions.** The `minerva` binary is the Rust core: bench runner and hub timeline. Extensions and scripts are Rune. The monorepo carries a README per folder. **The web UI and JEV (Python) are the two exceptions** to the Rust rule.
- **`minerva bench` is the runner.** One command: reset → boot → start eidolon + JEV with DeepSeek → poll Aeacus over the qemu console → stop at the time limit or when solved → report. It replaces the earlier shell `bench.sh`.
- **MITRE data lives in a crate.** `crates/attack` carries both ATT&CK and D3FEND from MITRE's official data, with the required attribution (§6).

### Policy boundary (reasoned; new rows in `policy.rn`)

| surface | rule |
|---|---|
| **where commands run** | Adapters run only on a named `target`. In an exercise profile the agent's local `bash` is limited to reads the table already classes as read-only, inside the exercise workspace. `script` runs on a target only. A local `python` or `sh -c` stays `FLAG` (observed row). |
| **network** | **Setup:** no network, no apt (§7.3). **Run:** QEMU `restrict=on`, with a single `guestfwd` egress to a WSL apt cache — the VM reaches *only* that mirror, so "packages updated" / "automatic security updates" issues are earnable and nothing else is reachable. `web_fetch`, `web_search` and `browser_*` are off in the benchmark profile (they could fetch the writeup). `vm_*` adapters accept only target names resolving to `127.0.0.1:<hostfwd port>`; any other host is refused before dispatch. |
| **reads on a target** | `vm_read` classifies the remote argv with the same harnox leaf table: read-only composes per stage (`triage.md` §2.1). Read-only commands are covered by a `reads` warrant; anything else asks. |
| **changes on a target** | `vm_change`, and `vm_script` with `class: change`, are allowed only when the target is `class: practice` **and** a base snapshot exists. Under the exercise warrant they are capped by `actions`; outside it, each one asks. The three refusals on the merits still stand: privilege escalation on the host, power-off, and destroying a filesystem. `sudo` inside the practice VM is expected (fixing broken sudo is the agent's job), so the privilege refusal applies to the host, not the target. Access-path changes are T3 and bound by the invariants (§5). |
| **deny-paths** | The exercise manifest has a `deny` list (Aeacus's directory and report path). `vm_read` refuses those paths whatever the command, as a prefix check on argv paths. Advisory only — a root script could still reach them — so §7 doesn't rely on it. |
| **approvals** | Once per run: the exercise warrant (graph ids, target, `reads: true`, change cap, wall time, library snapshot). Per item: any new `script` hash, any change outside the warrant, and any T3 action (operator off for benchmark runs — T3 then blocks or auto-parks). |

---

## 4. Roles: the brain / hands loop

Two roles, split by cognition. Neither is a model name for its own sake: DeepSeek happens to be the brain, JEV is always the hands.

```
        DeepSeek (BRAIN)                         JEV (HANDS)
   reads findings / evidence /       up:   findings, evidence,
   parked decisions                        parked decisions
   decides where to go next        ------------------------->
   authors a NEW graph/script      down:  next graph + params,
   when none in the library fits   <-------  OR a new validated graph
                                          runs graphs over ssh, acts,
                                          records, holds budgets +
                                          `choose` points; parks the
                                          interpretive ones back up
```

- **DeepSeek = brain.** It reads the case, orders the work by severity (§5), decides the next graph and its params, answers interpretive parks, and — when nothing in the library fits — authors a new JEV graph or `script`. Local `ollama:deepseek-v4.1-flash` first; the DeepSeek API later, to compare (§7.4).
- **JEV = hands.** It runs the chosen graph, enforces budgets, calls `capture`, runs each remediation's precheck/change/postcheck, writes the decision log, and parks whenever a decision is interpretive.
- **Jev-family chooser.** For T1 mechanical single-object choices only (§5). Which model is open (§12, Q7).
- **Adapters** remain the only way to affect a target (§2).

---

## 5. Escalation ladder and invariants

**Framing (reported).** There is **no public Arete escalation ladder** to copy. The tiers below are standard SOC tiers, tagged with MITRE **D3FEND** tactics. The "roles, escalation paths, decision frameworks" framing is taken from Arete's incident-response-management page (https://areteir.com, IR management), not from a published ladder.

| tier | who | scope | D3FEND tactics |
|---|---|---|---|
| **T0** | JEV graphs, no chooser | read-only recon / baselines | Model, Detect |
| **T1** | JEV + Jev-family chooser | reversible single-object fixes, rollback armed | Harden |
| **T2** | DeepSeek analyst | ambiguous findings, multi-step fixes, new graphs, removing non-critical things | Harden, Isolate, Evict |
| **T3** | DeepSeek as incident commander (+human optional; **off for benchmark runs**) | irreversible or access-path changes: ssh, sudo, PAM, firewall, deleting users/packages, anything the ReadMe calls critical | Isolate, Evict, Restore |

**Triggers (any one escalates one tier):** confidence below floor; impact class (Isolate/Evict, or an access path); irreversible; conflicts with the ReadMe; two failures; tier time box exceeded.

**Rule: decide at the tier allowed, execute at the lowest safe tier.**

**Severity orders the work; the ladder decides who acts.** The hunt order and per-tactic severities (Persistence / Privilege-Escalation backdoors SEV1 first, hygiene SEV3 last), and the D3FEND-tactic → tier mapping, are the generic chain in `attack-chain.md` (§"How triage uses it"); this doc doesn't repeat them. The one toolkit-specific rule: anything touching the agent's own access (ssh, sudo, PAM, firewall) is **T3** with the dead-man rollback armed (invariants below), whatever its D3FEND tactic.

### Invariants at every tier

| # | invariant | mechanism |
|---|---|---|
| **a** | Own the lifeline | Host ssh must survive. Before any firewall / sshd change, confirm the rule that keeps the agent's connection is ordered **first**. |
| **b** | Dead-man switch | Risky changes (`ufw`, `sshd`, PAM, `sudoers`) auto-roll back after **2 min** unless JEV reconnects and confirms. |
| **c** | Keep a way back to root | Re-check `sudo -v` right after each `sudoers` / PAM change, rollback armed. |
| **d** | Lockout = run ends, counted | Aeacus is read over the qemu console, not ssh, so the score survives a lost lifeline; the run still ends and counts as a lockout. |

---

## 6. Tagging and the library

### Tagging (reasoned; ground truth is scorer-only)

Findings are tagged ATT&CK, actions D3FEND, from `crates/attack` (MITRE data, ATT&CK Enterprise **v19.2**, 15 tactics, with the required attribution). The tactic set, tag paths (static / chosen / free), ID validation, and the build paths for the data are the generic **`attack-chain.md`** — this doc doesn't repeat them. Use its v19.2 tactic names (e.g. **Stealth** TA0005, **Defense Impairment** TA0112), never the pre-v19 "Defense Evasion".

The one toolkit-specific point: **ground truth is scorer-only.** The writeup's per-vuln technique IDs are the ground truth for tagging accuracy; they live on the scorer side, the answer key **never reachable by the agent** (§7.4). Each run also records the ATT&CK/D3FEND versions it tagged against, so cross-release runs aren't compared as equals.

### Library: auto-keep (decided 2026-09-25)

- **Every new graph a run authors is validated and kept automatically** in the shared library — schema, tools used, and invariant/policy checks all pass, or it is not kept. (User's choice: auto-keep, not human-gated.)
- **Bookkeeping so runs stay interpretable:**
  - Each run records the **library snapshot** (hash/version) it started from.
  - Reports compare runs at the **same snapshot** only (§8).
  - An **empty-library control run** can be run any time to measure the fresh-image baseline against a full library.

*How a new graph is validated in detail is open (§12, Q9).*

---

## 7. Benchmark: BotForge + Aeacus, and answer hygiene

**Goal (decided 2026-09-25):** from a fresh BotForge image, Aeacus-scored, **256 pts across 61 issues (7 of them forensics)**, to solved. No preconfigured tools beyond what's on the image plus the general-purpose ones; only the `mford` key is added. The agent reads `ReadMe.txt` in-VM like a competitor. Nothing BotForge-specific is written from the writeup.

### 7.1 What the page says (observed; https://tirefire.org/posts/botforge-practice/, fetched 2026-09-25, post dated 2026-01-17)

- **The image:** Linux Mint 22, shipped as an OVA at `https://downloads.tirefire.org/botforge-practice-v2.ova`, MD5 `c5ef4ab19fd6cd9f79139ba6db2b65b9`.
- **Stated requirements:** "VMware Workstation, Player, or Fusion", 4 GB RAM (8 GB recommended), 20 GB disk. The download size is not stated.
- **The scorer is Aeacus** (observed from the image, 2026-09-25): a local scoring engine that shows a hint on its report when a vulnerability is fixed. **256 pts / 61 issues, incl. 7 forensics** (`Forensics1.txt`–`Forensics7.txt` on the Desktop, each citing ATT&CK technique IDs).
- **The scenario:** a README on the Desktop describes it and lists the authorized users.
- **Page advice:** "Take a snapshot before you start."
- **Login credentials** are published on the page. They belong in operator config, not in this doc.
- **The official writeup** is `github.com/christopher-gholmieh/BotForge-Writeup`. It is used **only by the scorer** (T-ID ground truth, §6; forensics answers, §7.4) and is **never** cloned anywhere the agent can read.
- **Not stated on the page:** how the report is opened; whether it needs internet; the answer format; a license or redistribution terms.

**The same author also published a Fedora 37 practice image** (https://tirefire.org/posts/fedora-practice/, 2022-11-26): ships as a `.zip`, hypervisor not stated, "a lot more forensics questions than most images", writeup by a different author (`github.com/Poarthan/mr.robot.sh-writeup`). Used as the held-out image (§7.5).

### 7.2 Hypervisor on Windows 11 + WSL2 (reasoned; the operator runs every install)

| option | how | caveats | verdict |
|---|---|---|---|
| **QEMU/KVM inside WSL2** | Convert the OVA's VMDK to qcow2 once with `qemu-img convert`. Each run gets an overlay qcow2. Networking is `-netdev user,restrict=on,guestfwd=...`(apt cache)`,hostfwd=tcp:127.0.0.1:2222-:22`. | Needs `/dev/kvm` in WSL2 via nested virtualization (**hypothesis**: `ls -l /dev/kvm`). The guest may want `e1000` + SATA/AHCI (**hypothesis**, first boot). | **The default.** Isolation is one flag, reset is deleting the overlay, agent and scorer are in WSL, Windows networking is untouched. |
| VMware Workstation Pro (free personal) | Import the OVA, host-only VMnet, snapshot/revert with `vmrun`. | Coexists with Hyper-V via WHP (slower). WSL2 NAT may not reach VMnet1; `networkingMode=mirrored` probably fixes it (**hypothesis**). `vmrun` is a Windows exe from WSL. | Fallback if QEMU won't boot the guest. |
| VirtualBox 7 | Import the OVA. | Runs on WHP (slow). VMware OVAs sometimes need controller fixes. | Not recommended. |
| Hyper-V | Convert to VHDX. | Disk conversion + different NIC, no clean OVA import. | Not recommended. |

### 7.3 Access path and baseline hygiene (observed today; decided 2026-09-25)

**The lesson.** The first base got **3 pts pre-awarded** because `apt install openssh-server` upgraded ssh (13.14→13.19) and removed the `:2222` backdoor listener — the setup step was silently solving a scored issue and mutating the image. The base was **rebuilt from the original disk**.

**The fix.** Setup adds **only the `mford` key**, delivered through a qemu `guestfwd` channel (10.0.2.100) — **no network, no apt, no added service**. The image's own sshd serves the run over `hostfwd` on `127.0.0.1:2222 → :22`. **The baseline must read 0/256 before the base is frozen.**

Setup once:

1. Boot the base from the original disk on the console.
2. Deliver `mford`'s key via `guestfwd` (10.0.2.100). Nothing else is installed or changed.
3. Run Aeacus once and **confirm the baseline reads 0/256**. If it doesn't, the image was mutated — rebuild from the original disk.
4. Freeze the result read-only.

The footprint (key-only) is recorded in `exercise.harness_footprint`, so results say exactly what was added.

### 7.4 Answer hygiene: nothing leaks to the agent (reasoned)

1. **Aeacus scores in-VM; `minerva bench` reads it over the qemu console, not ssh.** The score survives a lockout (invariant d).
2. **The answer key never reaches the guest.** The writeup-derived T-ID ground truth (§6) and the forensics answers live with `minerva bench` on the host side. The agent runs as a competitor; it never has a path to them.
3. **No way to look anything up.** In the benchmark profile, web / `browser_*` / `mneme_rpc` / swarm tools are off; the VM's only egress is the apt cache (§3). `minerva bench` diffs the run's eidolon config against an allow-list before starting.
4. **Two feedback modes:**
   - **Blind (first):** the agent cannot read the Aeacus ScoringReport. Report path + engine dir are on the deny list; every `vm_read` that touches a deny-path is counted (a leak-attempt counter); `vm_script` bodies containing deny-path strings are refused.
   - **Feedback (later):** the agent may read the report (hill-climbing), and results are labelled that way.
   - The agent is root in the guest, so the deny list is advisory; **detection + invalidating the run** is the real control.
5. **What gets scored:** (a) Aeacus points read over the console after the agent is stopped; (b) Aeacus penalties; (c) the 7 forensics answers written into `ForensicsN.txt`, graded against the key; (d) tagging accuracy vs the T-ID ground truth; plus ladder metrics, tokens and wall time (§8).

### 7.5 Not overfitting to one image (reasoned)

- **Held-out image.** Fedora 37 from the same site (different distro, package manager, writeup author). Scored only at milestones, never while tuning.
- **Held-out questions.** Within BotForge, tune on forensics 1–4, report 5–7 separately; same split for the Aeacus categories.
- **An image-string lint.** CI greps the toolkit, graphs and prompts for strings specific to this image (usernames, filenames, hostnames). The list stays on the scorer side; a hit fails CI. This is what "nothing BotForge-specific" means mechanically.
- **Home-made canaries.** An operator script plants 3–5 known misconfigurations in a clean Ubuntu cloud image, answers written by the club — grows the benchmark without anyone's writeup.

### 7.6 Bench settings (defaults; decided 2026-09-25)

| setting | default | later |
|---|---|---|
| VM egress | apt cache on WSL only (VM reaches nothing else) | — |
| lockouts | counted | — |
| brain model | local `ollama:deepseek-v4.1-flash` | DeepSeek API, to compare |
| feedback | blind (agent can't read the ScoringReport) | feedback mode |
| time box | 2 h per run | — |
| repeats | 3 runs per setup | — |

### 7.7 Operator setup and reset (the agent runs none of this)

1. **Windows:** `wsl --update`; nested virtualization on; in WSL, check `/dev/kvm`.
2. **WSL packages (apt):** `qemu-system-x86`, `qemu-utils`, `openssh-client`. Optional `libguestfs-tools`. Stand up the **apt cache mirror** the run's `guestfwd` points at.
3. **The image:** download `botforge-practice-v2.ova`, verify MD5, `tar` unpack, `qemu-img convert -O qcow2` to `botforge-base.qcow2`.
4. **First boot:** deliver the `mford` key via `guestfwd`; run Aeacus and confirm **0/256**; freeze `base-harness.qcow2` read-only (§7.3).
5. **Answer key:** the operator types the forensics answers and records the writeup T-IDs on the scorer side, reading the writeup on Windows, outside WSL.
6. **MITRE data:** load ATT&CK + D3FEND into `crates/attack` (§6), with attribution.
7. **Later, Fedora:** repeat 3–5.
8. **Optional, once:** build the xyflow bundle (§9) with Node + Vite in a throwaway dir, then delete `node_modules`.

**Reset between runs.** Delete the overlay, create a fresh one on the never-written base:

```sh
rm overlay.qcow2 && qemu-img create -f qcow2 -b base-harness.qcow2 -F qcow2 overlay.qcow2
```

---

## 8. Report

Each run is one row; reports compare rows **at the same library snapshot** (§6). Contents:

- score over time; penalties; forensics k/7;
- tagging accuracy (ATT&CK + D3FEND, vs the T-ID ground truth);
- ladder metrics: escalations per tier, justified vs not;
- tokens; wall time; the library snapshot (hash/version);
- **grouped by ATT&CK tactic.**

Row shape: `{image, sha, model, graphs@snapshot, mode, forensics k/7, points, penalties, tag_acc, escalations_by_tier, leak_hits, tokens, wall_s}`.

---

## 9. Graph rendering

### 9.1 The pick: `@xyflow/system` with Preact + htm; `@xyflow/react` via `preact/compat` only after a spike

| route | what you vendor | size | fit |
|---|---|---|---|
| **A. `@xyflow/react` via `preact/compat`** | A one-time Vite library build with `react`/`react-dom` aliased to `preact/compat`. Output is ESM importing the vendored `preact.js` by relative path (no importmap, CSP forbids an inline one). Also vendor `preact/compat` and React Flow's CSS. | **Hypothesis:** ~200–350 KB JS, near the 335 KB Svelte Flow bundle. Measure in the spike. | You get React Flow's components. Risk: React Flow's store leans on `useSyncExternalStore`, React 18 batching and `forwardRef` that compat only imitates (**hypothesis**). A second Preact copy breaks hooks silently. |
| **B. `@xyflow/system`** (the pick) | A one-time Vite build of `@xyflow/system` and its d3 deps (`d3-zoom`, `d3-drag`, `d3-selection`, `d3-interpolate`) as one ESM file. Nodes/edges/handles in Preact + htm, fed by `graph-adapter.js`. | **Hypothesis:** ~60–100 KB JS. | The shared core React Flow and Svelte Flow both run on. Minerva writes its own node components (the four-band `choose` node, square frames, monospace, theme vars) — no default look to strip, no compat layer. Cost: ~400–600 lines of Preact (reasoned). |
| C. No library | SVG from `graph-adapter.js`'s `layout()`, hand-written pan/zoom. | ~0 | Read-only graphs in chat only, not an editor. Fallback if B fails under the CSP. |

**The call is B.** React Flow at the engine level, one virtual DOM, one frozen file pinned in `vendor/VERSIONS` with its sha512. **Take A only if** the spike (milestone **M6a**) shows compat handling the store with no second Preact copy and a bundle under 350 KB.

**CSP (observed and reasoned):**

- **Scripts** covered by `script-src 'self'`: the bundle is a local file.
- **Styles** covered by `style-src 'self'`: xyflow and Preact set node transforms/sizes via CSSOM (`el.style.*`, `setProperty`). The Svelte Flow check showed CSSOM writes work under this CSP, while `setAttribute('style')` and an injected `<style>` are blocked (observed, Build-Log). Preact turns a string `style` prop into `style.cssText` — believed CSSOM but **not verified**, so the rule is object-form styles only.
- **Library CSS** ships as a vendored `.css` link, built `cssCodeSplit: false` (observed).
- **Arrow markers** are inline SVG, not `data:`, so `img-src` doesn't matter.

**Restyling (reasoned):** `border-radius: 0`, monospace cell font, 1px frames with the title cut into the border, colours from `theme.css`, grid snapped to `--cw`/`--ch`, `getSmoothStepPath` with `borderRadius: 0`, no gradients.

### 9.2 Where graph output in chat comes from

1. **`jev_run`, `jev_resume`, `jev_runs` tool lines.** `renderers/jev_run.js`, registered in `manifest.json` (`web-ui.md` §10), parses tool output (`path`, `final_state`, `outcome`, escalation `options`) as JSON, never HTML. It fetches the graph from a **new read-only hub route**, `GET /hub/jev/graphs/<id>`, returning `{id, version, sha256, doc}`. The route accepts `id` matching `^[a-z0-9][a-z0-9-]{0,63}$`, serves only files in `extensions/jev/graphs/`, follows no symlinks out, same containment as `/hub/tree` (`hub.md` §7). The run's path is drawn highlighted; a parked state glows `--yellow`.
2. **Graphs the model writes.** A fenced `jev` block in assistant text; `core/markdown.js` hands the fence to the same component (`JSON.parse`, read-only view). The page doesn't lint; linting happens on the jev lint path.
3. **Live step progress (later).** Needs a door frame type, sibling of `park-tick` (gap 7 in `web-ui.md`). Deferred: **no new frame type now**.

"Similar to how Melete's UI renders graphs" is **reported** by the user; Melete's UI wasn't examined.

---

## 10. Substrates evaluated (fetched 2026-09-25)

| candidate | what it actually is | license | maturity (evidence) | fit against Minerva JEV |
|---|---|---|---|---|
| **Laya** — huggingface.co/convaiinnovations/laya; github.com/NandhaKishorM/laya | A **decision model**, non-autoregressive ("System 1"). ModernBERT-large, 421M params, English, 512 ctx; also a 322M multilingual variant on mmBERT-base. Answers `choice`, `score`, `noul` (yes/no) in one forward pass, probabilities it says are calibrated. Trained "RLCD" against strictly proper scoring rules. Claims ~7.8× faster than TypeSafe Jev (observed, model card). | Apache-2.0 (observed) | pip package `laya-serve` (FastAPI), MCP server, tests, `BENCHMARKS.md` (observed via fetch summary). ~193–464 ms per decision on CPU. Star/commit counts from a summarizing fetch, **unverified**. Single decisions only. | **Not a substrate.** Candidate to replace **jevlike** behind `choose` — i.e. the **T1 Jev-family chooser** (§5). Calibration is exactly what jevlike lacks (S47). **Test:** replay S47's calibration check on a triage decision log, Laya vs jevlike, on CPU within this box's latency budget. |
| **OpenJEV** — huggingface.co/AlexWortega/openjev | Real. Qwen3.5 as a 3-way NLI **cross-encoder** (entailment / contradiction / neutral). Checkpoints: 0.8B v2s, 2B v5, 4B v5 (recommended), 4B v2 (images), 35B-A3B MoE. Custom `OpenJevCrossEncoder` (observed, card). | MIT (observed) | Card reports MNLI 0.896, RAGTruth AUROC 0.932, HaluBench AUROC 0.937. Same-name projects (tellsiddh, ZefanCai, cappuch) **unverified**. | **Already inside Minerva** as the guard checker (`Architecture.md`). Not an executor. **Action:** confirm the box's checkpoint; test 2B v5 as a cheaper checker at M4 (hypothesis). |
| **Minerva JEV** — `jev/automation/`, `extensions/jev/` | A statechart executor: XState subset, observe/check/choose/act, budgets, warrants, parking, escalation, decision log. Every effect goes through eidolon's gate (observed, `automation.md`). | the repo's | Five graphs shipped; live end-to-end trial J1 ran (observed, Build-Log). `reads` and `capture` proposed, not built (`triage.md`). | **The substrate** — the hands (§4). The only one that executes and the only one wired to the gate. Needs `capture` and `reads` built first. |

---

## 11. Milestones

Ladder tiers are T0–T3 (§5); milestones are **M#**, to keep them distinct.

| # | what | done when |
|---|---|---|
| **M0** | Operator setup (§7.7): key-only footprint via `guestfwd`, apt cache, baseline **0/256** | `minerva bench reset && up` boots the overlay; `ssh -p 2222 -i mford` works; Aeacus over the console reads **0/256** on the frozen base; a `curl` to anything but the apt cache from the guest fails; boot time recorded. |
| **M1** | `triage.md` prerequisites: `capture`, the `reads` envelope key, `Ruling.read_only` | `triage.md` spec steps 1–5 pass, incl. the empty-match lint. |
| **M2** | `target`, `adapter` (`vm_read.rn`), `case`, `exercise`, plus the policy rows and **ATT&CK/D3FEND tagging** (`crates/attack`) | A graph lists the VM's users, diffs them against the README's authorized list unattended under a `reads` warrant, tags findings ATT&CK with validated IDs. Naming a host not in config is refused before dispatch, and a test says so by name. |
| **M3** | `minerva bench` (Rust) + the loop: reset → boot → DeepSeek + JEV → poll Aeacus over the console → report | A null-agent run produces a baseline row. Leak tests pass: the answer key is unreachable by the agent; the benchmark config has no web/browser/mneme tools; a planted deny-path read shows `leak_hits: 1`. |
| **M4** | Forensics + the ladder: DeepSeek brain, T1 Jev chooser, a question-loop graph writing `ForensicsN.txt` | Three blind runs on BotForge give a k/7 score with spread, split tuned (1–4) vs held-out (5–7). Ladder metrics recorded. Laya-vs-jevlike chooser check and OpenJEV 2B v5 checker check run here. |
| **M5** | `remediation` + `vm_change.rn`, under the invariants (§5) | Points beat baseline with zero penalties across three runs. Every change has a passing postcheck and a rollback note; a dead-man rollback fires in a forced-lockout test and the run counts. |
| **M6a** | Graph spike | B's bundle built and pinned, no CSP errors, size recorded. A's spike has run; the pass/fail verdict against §9.1's gate is written down. |
| **M6b** | Graph renderer + `GET /hub/jev/graphs/<id>` | A `jev_run` in chat draws its graph with the path highlighted; a fenced `jev` block renders; tests show the hub route refuses `../`, symlinks and bad ids. |
| **M7** | `script` primitive + library auto-keep | A read helper DeepSeek wrote runs on the target after one hash approval, its body in the case, is **validated and auto-kept** in the library, and the run records the snapshot. A request to run it locally is refused by name. A second run with the same hash asks nothing. |
| **M8** | Generalization | Fedora 37 scored with no toolkit change; the image-string lint passes; the canary image scored. An empty-library control run is recorded for comparison. |

M0 and M6a/M6b can run in parallel with M1–M2.

---

## 12. Risks

| Risk | Detail | Mitigation |
|---|---|---|
| Setup mutates the image | An install can silently solve a scored issue (the ssh-upgrade / `:2222` backdoor case: 3 free pts). | Key-only via `guestfwd`, no apt at setup; **baseline must read 0/256** before freeze (§7.3); rebuild from the original disk on any drift. |
| The OVA may not boot under QEMU | Possible VMware-driver dependence. | Fall back to VMware Workstation, mirrored WSL networking (§7.2). |
| The access path shows up in scoring | An added service/key could be a finding. | Add only the `mford` key; confirm 0/256 baseline; footprint recorded (§7.3). |
| Aeacus is unknown | It might need a clock, a GUI, or detect tampering. | Found at M0; read over the console; `restrict=on` egress limited to the apt cache. |
| Leaks through in-VM files | The agent is root, so the deny list is advisory. | Answer key never in the guest; detection + run invalidation is the control (§7.4). |
| Own-lifeline loss | A firewall/sshd/PAM change can lock the agent out mid-run. | The four invariants (§5): lifeline-first ordering, 2-min dead-man rollback, `sudo -v` recheck, lockout counted. |
| Brain-model speed / availability | Local models are slow against a 2 h box; DeepSeek API may be needed to hit the score. | Results name the model; local `deepseek-v4.1-flash` first, API to compare (§7.6). |
| Chooser calibration | jevlike is at chance (S47). | Interpretive decisions park to DeepSeek, never picked; test Laya as the T1 chooser (§10). |
| Compat fragility (route A) | React Flow's store may not survive `preact/compat`. | A sits behind the M6a spike. |
| No stated license for image or writeup | Redistribution terms unknown. | Nothing from either is redistributed or committed; the writeup is used only by the scorer, off the agent's path. |
| Maintenance | Must survive long stretches unmaintained. | Six primitives, `.rn` adapters, one frozen vendored bundle, no runtime build; auto-kept library keeps graphs without human gating; rebuilds recorded in `vendor/VERSIONS`. |

---

## 13. Open questions (default in the right column)

| # | question | default |
|---|---|---|
| Q1 | React Flow's components (A) or xyflow's engine (B)? | B, and run A's spike anyway |
| Q2 | Hypervisor | QEMU/KVM in WSL2; VMware if the guest won't boot |
| Q3 | How the agent gets into the VM | image's own sshd over `hostfwd`; only the `mford` key added, via `guestfwd`; baseline 0/256 |
| Q4 | May the agent read the Aeacus report? | No in blind mode; yes in feedback mode, results labelled |
| Q5 | Which brain model | local `deepseek-v4.1-flash` as the floor; DeepSeek API to compare; always reported per model |
| Q6 | Remediation approvals in bench mode | Covered by the exercise warrant up to the `actions` cap; T3 blocks (operator off) |
| **Q7** | **Which Jev-family model for the T1 chooser?** | **Test Laya vs jevlike on the M4 decision log; keep jevlike until Laya wins on calibration + latency** |
| **Q8** | **DeepSeek API key handling** | **A key file (`*_KEY_FILE`), read per call, never in spec/logs/errors — same discipline as `JEV_CHOOSER_KEY_FILE` (§1)** |
| **Q9** | **How is a new graph validated before auto-keep?** | **Schema + tools-used + invariant/policy checks pass, or it is not kept; detail TBD (§6)** |
| Q10 | Which openjev checkpoint | Keep the current one; test 2B v5 as a cheaper checker at M4 |
| Q11 | A frame type for live progress | Defer; the tool-line renderer and hub route come first |
| Q12 | Where the canary images come from | A club-written script over an Ubuntu cloud image, run by the operator |

### Files an implementation will touch

- `crates/minerva/` — the Rust core: `minerva bench` runner and hub timeline.
- `crates/attack/` — ATT&CK + D3FEND data and lookup, with attribution.
- `jev/automation/graph.py` — lint for `reads` and `capture`.
- `extensions/jev/tools/run.rn` — the driver, and the output shape the renderer reads.
- `webui/graph-adapter.js` — JEV graph → xyflow nodes and edges.
- `webui/term/renderers/` — the renderer manifest hook.
- `docs/design/hub.md` — add `GET /hub/jev/graphs/<id>`.

---

## 14. Sources

- **ATT&CK + D3FEND** — the tactic set, severities and data build paths live in `attack-chain.md` (its Sources list has the MITRE URLs and the ATT&CK terms-of-use / attribution link). ATT&CK Enterprise v19.2.
- **Arete IR management** — https://areteir.com (IR management page; the "roles, escalation paths, decision frameworks" framing — there is no public Arete ladder to copy, §5).
- **BotForge** — https://tirefire.org/posts/botforge-practice/; writeup `github.com/christopher-gholmieh/BotForge-Writeup` (scorer-only).
- **Fedora 37 (held-out)** — https://tirefire.org/posts/fedora-practice/; writeup `github.com/Poarthan/mr.robot.sh-writeup`.
- **Chooser wire format** — https://docs.typesafe.ai/api.md; jev.cpp https://github.com/thomasgauthier/jev.cpp.
- **Laya** — huggingface.co/convaiinnovations/laya; github.com/NandhaKishorM/laya. **OpenJEV** — huggingface.co/AlexWortega/openjev.

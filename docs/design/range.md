# The range: the club's cyber range, phased from Windows mini-PCs to a Proxmox lab

**Written 2026-09-25. Plan only: no code, no installs, no downloads, no network change, no commit.** This is the club's foundational range doc. It is meant to be decided, section by section, not read as an encyclopedia.

It builds on the user's range research artifact (Proxmox + Ludus, the hardware split, content, isolation, Nix). That research is cited here as **reported**, because it was given to this doc in condensed form, not re-derived.

**Labels** (the same four `triage-toolkit.md` and `hub.md` use):

| Label | Meaning |
|---|---|
| **observed** | Read in code, config or a fetched page; the source is named. |
| **reported** | Someone else's claim (the research doc, the user, a third-party review), not reproduced here. |
| **reasoned** | Argued from observed facts; the argument is next to it. |
| **hypothesis** | Believed, not measured; says what would measure it. |

**Decided** marks a user decision already made on 2026-09-25:

- Nix is the authoring core, plain generated files run the range, and there is an eject path.
- Every physical host runs Proxmox.
- Minerva is the interface.
- Mneme has tiers.
- Commands flow down only.
- The human-yes list stands.

---

## 0. The answer in one paragraph

Build the range in four phases, each behind a gate, so nothing done today on Windows breaks the Proxmox end state.

- **Phase 0 (now)** runs on the Windows EVO-X2s over a private club LAN with no public IP. Each box gets a NixOS-WSL distro:
  - one box hosts the management plane: the new **club Mneme**, the **Minerva hub**, the planner agents and Aoide;
  - two boxes keep Windows-native **Hoot** as the AI plane;
  - the rest run Linux practice images (BotForge) under QEMU/KVM inside WSL, each isolated on its own box.
- **Gate 0.5** is university IT sign-off. Before it, nothing vulnerable leaves a single isolated box.
- **Phase 1** inventories, labels and racks the Dells. It puts Proxmox on the R730 (core VMs) and Proxmox Backup Server plus a QDevice on the R330, then moves the management plane onto NixOS VMs.
- **Phase 2** backs up and wipes the range EVO-X2s into **independent single-node Ludus/Proxmox servers**, a failure domain separate from the core.
- **Phase 3** needs a static IP (or an approved tunnel) and adds the OPNsense edge, the web DMZ and member accounts.

**Nix authors everything and plain files run it.** One flake declares the Proxmox objects (terranix → Tofu), the Ludus ranges, the Ansible inventory, the installer answer files, a fenced set of host units, the NixOS guests and the Aoide peer list. It renders them to committed plain files under `generated/`. CI fails if those drift. Daily operations use plain `tofu`, `ludus` and `ansible` on the generated files, and the club can eject from Nix by editing them directly.

**Agents run the fleet in a loop:** observe → plan → PR → CI → a human merges → apply → verify → record. Humans keep the rules, the merge button, the human-yes asks (answered inline in Minerva) and the physical work. **Commands flow down only** (MGMT → RANGE): range agents report into a quarantine vault and never hold a path up, a flake write, or a door token.

---

## 1. Why Nix over the alternatives (decision record)

**Verdict: (B) Nix is the authoring core, the runtime is plain generated files, and there is an eject path.**

| Criterion | (A) NixOS everywhere, including the hypervisor | **(B) Nix authors, plain files run** | (C) No Nix: Tofu HCL + Ansible + Ludus |
|---|---|---|---|
| One source of truth | yes | **yes**: the IP plan, people, peers and ranges are declared once and rendered into each format | no: the IP plan and host list are repeated across HCL, Ansible vars, Ludus YAML and the Aoide peer list, and kept consistent by hand |
| Reproducibility and rollback | best | NixOS guests: generations + PBS. Proxmox objects: `tofu plan` against committed JSON. Ranges: Ludus redeploy. | Tofu state + Ansible idempotence; no guest generations |
| Aoide mesh | native | **native**: Aoide ships NixOS modules (observed, `Aoide/modules/nucleus/aoided.nix`) | the aoided and a2a units get re-implemented in Ansible |
| Agents maintaining it | typed, but no Windows story | **typed eval + CI** (flake check, render diff, VM tests, `tofu plan`) catch agent mistakes before a human sees the PR | untyped YAML/HCL; errors surface at apply time |
| Bus factor | worst: every recovery needs Nix | **eject path**: `generated/` is plain and editable, daily ops never touch Nix, NixOS guests restore from PBS | best |
| Windows and Ludus | **fails**: microvm.nix can't boot Windows, proxmox-nixos is experimental, and Ludus is locked to Debian/Proxmox (reported, research doc) | works: Proxmox and Ludus stay stock; Nix only renders their inputs | works |
| Build effort | very high (rebuild Ludus) | medium: a thin render layer (terranix for Tofu, `pkgs.formats.yaml`/`toml` for Ludus and the answer files) | low at first; grows with every duplicated fact |

**Why B's costs don't flip the answer (reasoned):**

- **Bus factor.** Ejecting is always one step away: delete the render step, and the committed `generated/` files become the source. Daily operations (reset a range, deploy a comp, restore a VM) already use plain tools on plain files. Only *changing the design* needs Nix.
- **Nix is hard to read.** CI is the reviewer that doesn't need a Nix expert:
  - `nix flake check`;
  - the render diff;
  - NixOS VM tests for Mneme, the hub and aoided (Aoide already has a VM boot test, observed at `Aoide/tests/vm-boot.nix`);
  - `tofu plan` attached to the PR;
  - Ludus config validation.

  A merging officer reads a Tofu plan and a YAML diff, not Nix.
- **C's saving is a false economy here.** The facts that must agree across formats (addresses, people, peers, ranges) are exactly what a range churns, and C keeps them consistent by hand.

**Eject triggers.** Any one of these starts the "ejecting from Nix" runbook page (§21):

1. The club no longer wants to own the flake itself.
2. No second person can read Nix, **and** agent maintenance proves unreliable. Measure the first-pass CI rate of agent Nix PRs in Phase 1; the default threshold is below 60 % over a term (Q13).
3. The render keeps drifting because people hand-edit `generated/`: more than 3 render-diff failures caused by hand edits in a term.

---

## 2. Primitives and relations

| Primitive | What it is | Owned by |
|---|---|---|
| **asset** | One physical thing with a permanent ID (`cc-mini03`) and one Mneme note | club (human-entered) |
| **zone** | A trust segment: MGMT, RANGE, AI, SVC, WEB-DMZ, OOB, STORAGE | the IP/VLAN plan |
| **host** | A physical Proxmox node (or a Windows box in Phase 0) | the flake → rendered installer, Tofu, fenced units |
| **guest** | A VM or LXC: either a NixOS guest (ours) or range content (attacker-authored) | the flake (Colmena) / Ludus |
| **range** | One Ludus-deployed topology for one user or team, on its own 10.x segment | Ludus YAML rendered from the flake |
| **peer** | An Aoide instance in a Linux/WSL agent guest; it has a `nodes.json` | Aoide; its peer list comes from the flake |
| **target** | Anything an agent *acts on* by name: a practice VM, an AD box, the Ludus API | triage-toolkit `target` |
| **session** | One eidolon agent run: one door, shown in Minerva | eidolon / hub |
| **vault** | A Mneme vault directory with a trust tier | club Mneme |
| **ask** | An eidolon policy question, answered inline in Minerva | eidolon / hub |
| **PR** | A proposed flake change; the only way the design changes | the forge; merged by a human |
| **person** | One identity (officer or member) declared once and rendered everywhere | `nucleus/people.nix` |
| **bible** | One of six human-readable reference docs | §16 |

```text
 officer ── Minerva page ── hub (MGMT) ── doors ── eidolon sessions (planner, reviewer, keeper)
    │ merge                   │                         │ PRs to the flake     │ commands DOWN only
    ▼                         │ read-only mesh feed     ▼                      ▼ (Aoide, ssh, Ludus API)
  forge ◄── CI (flake check, render diff, VM tests, tofu plan, ludus validate)   Aoide peers in RANGE ── targets
    │ merged main                                                                 │ reports (data)
    ▼ render → generated/ → apply (colmena, tofu, ludus, ansible)                 ▼
 club Mneme:  shared (read by all) ◄── promotion (human yes) ── inbox / quarantine (written by agents)
 Melete (public wiki server) ── audited jump, fixed job names ──► MGMT job queue (never into RANGE)
```

**Relations:**

- An asset hosts 0..n guests.
- A range belongs to one Ludus server and has 1..n guests plus at most one agent peer.
- A peer belongs to exactly one zone. A zone's peers may command only zones below it (§10).
- Every design change is a PR. Every PR is merged by a human.
- Every session is visible in Minerva, but only MGMT sessions have a writable door.

---

## 3. Current reality, and what it rules out

| Fact | Label | Consequence |
|---|---|---|
| No static IP and no public IP; "a more limited" network exists | reported (user) | No inbound anything. Remote access and public hosting wait (§13). **Open: does the lab LAN have outbound internet at all?** (Q1) |
| Only the 8 EVO-X2s exist, all on Windows | reported (user) | Phase 0 is Windows + WSL2: no Proxmox, no Ludus. |
| The R730 and R330 aren't up | reported (user) | No ECC or iDRAC tier, no shared storage, no PBS. The management plane lives on a mini-PC for now. |
| Native Windows Aoide doesn't build (aoide-storage, 10 errors); the WSL2 build works | reported (brief) | Every Phase 0 Aoide peer is a WSL2 distro. |
| eidolon upstream is unix-only (Windows port closed 2026-09-22) | reported (memory note) | eidolon, the door and the hub run in WSL too. |
| Aoide's A2A door is JSON-RPC over plain HTTP, off by default, bound to loopback by default | observed: `Aoide/CONTRACTS.md` §6 "Security posture"; `modules/nucleus/aoided.nix:77-126` | The transport (ssh or WireGuard) must encrypt cross-host traffic. |
| With no bearer set, loopback is trusted unconditionally. With `tokenFile`/`bearerSecret` set, a caller without the token is coerced to `Unknown`. | observed: CONTRACTS.md §6, amendment 2026-08-19 | **Every** peer sets a token before any tunnel exists (§10). |
| The CSRF fix (refuse `Origin`, require JSON content-type) is on `fix/a2a-browser-origin` | observed: the branch exists locally (`git branch` in `Aoide/`), with a worktree at `Projects/Aoide-a2a-origin`. Uncommitted per the brief. | It's a prerequisite of Phase 0's mesh step. |
| Aoide's flake assembles hosts with a single `username = "khoa"` | observed: `Aoide/flake.nix` (`mkHost`) | The range flake's guest builder needs a people list (§19.3). |
| There is **no club Mneme**. The Mneme on the user's box serves personal vaults: `magi`, `necoconeco`, `rook`. | observed: `list_vaults`; reported (user) | A new, separate instance gets created. Lab agents are never pointed at the personal one (§11). |
| Aoide's `mneme` dendrite always starts a second userspace `tailscaled-mneme` with a public `:443` Funnel, and takes one `vaultPath` | observed: `Aoide/modules/dendrites/mneme.nix:48, 101-110` | The club Mneme needs its own dendrite with no Funnel and several instances (§11). |
| Mneme has no attachment/upload function (44 functions: notes, canvas, versions, trash, share, semantic) | observed: `RPC list` | Photos and captures go in by path convention (§16.2). |
| Melete runs on the public wiki server. Its tools include `run_code_task`, `schedule_*`, `graph_view`, `secret_set`, `ssh_exec`, `verba_dispatch`. | observed (the connector tool surface in this session); reported (user) | Melete is internet-facing, so treat it as compromisable. It gets an audited jump with fixed job names (§10.5). |
| "Hoot" is the club's local AI workstation stack: a llama.cpp router with `models.ini`, Open WebUI, and eidolon's `hoot` provider | observed: `docs/Architecture.md`; reported (user) | Reuse it as the AI plane; don't build a new one. |

---

## 4. Zones

### 4.1 End state (Phase 3)

```text
                     internet
                        │ static IP (or approved outbound tunnel, §13)
               ┌────────┴─────────┐
               │ OPNsense (VM on R730, 2 NICs)   WireGuard for officers/members
               └──┬─────┬─────┬───┘
   WEB-DMZ v40 ───┘     │     └─── campus: NO bridge, NO route (WAN uplink only)
   K3s on NixOS VMs     │
   club site, member    │ MGMT v10 (NixOS VMs on R730)
   containers; no       │  ┌─────────────────────────────────────────────┐
   resident agent;      │  │ hub + per-person accounts  club Mneme        │
   Flux pulls merged    │  │ planner/reviewer/keeper    (officer, agent)  │
   main                 │  │ Aoide peers (top)          scorer (own acct) │
                        │  │ forge + CI runner, jump, log sink, caches    │
                        │  └───┬───────────────┬──────────────┬──────────┘
                        │      │ down only     │ inference     │ one-way push (ro replica)
          ┌─────────────┘      ▼               ▼               ▼          one-way pull (quarantine)
          │        RANGE v100-v199          AI v20           SVC v30
          │        6 × EVO-X2, each an      2 × EVO-X2       range-facing Mneme
          │        independent Ludus/PVE    Proxmox + LXC    (quarantine rw, shared ro
          │        server; per-team 10.x    llama-server     replica); no route to MGMT
          │        ranges; one agent peer   (Hoot stack),
          │        per range; NO egress     inference port only
          │        (Ludus testing mode)
          │
   OOB v99: iDRAC R730/R330, switch mgmt      STORAGE v50: R730 NFS, PBS traffic
   (MGMT keeper reads only)                   R330: PBS + corosync-qnetd
```

### 4.2 Phase 0 (now)

```text
   campus network / Wi-Fi (Windows Update, Nix fetches, forge, IF allowed; Q1)
        │ each Windows host only; never bridged into the lab LAN or the VMs
 ┌──────┴───────────────────────────────────────────────────────────────┐
 │ club lab switch (unmanaged OK): flat P0-MGMT 10.66.10.0/24, static   │
 └──┬──────────────┬──────────────┬──────────────┬──────────────────────┘
    │              │              │              │
 cc-mini01      cc-mini07/08   cc-mini02..06   (one unit on loan as
 Windows        Windows        Windows          yomi-strix; not in the lab)
 └ NixOS-WSL    └ Hoot native  └ NixOS-WSL
   "mgmt-0":      llama.cpp      "rng-0N": Aoide peer (range tier),
   club Mneme,    Vulkan,        JEV + adapters, scorer user,
   hub, planner,  --api-key,     QEMU/KVM practice VMs
   reviewer,      lab-LAN port     └ -netdev user,restrict=on
   keeper, CI     only (P0-AI)       (no route out, ssh hostfwd only)
   runner, Aoide                 (P0-RANGE: one isolated box each)
   (P0-MGMT)
```

Phase 0 has no VLANs. The zone boundary is the box: vulnerable guests exist only inside one EVO-X2's WSL with `restrict=on`, which is triage-toolkit's isolation (observed, `triage-toolkit.md` §4.2). The lab LAN carries agent and management traffic only.

---

## 5. Phases

`G` = gate (prerequisite). **Throwaway** = work expected to be discarded. **Deps** = what Minerva and the flake must have built (§8.5, §19).

| Phase | Gates | Builds | Throwaway | Deps | Done when |
|---|---|---|---|---|---|
| **0: Windows bench** (now) | **G0a:** the CSRF fix is merged, and `tokenFile` is set on every peer. **G0b:** a club lab switch with static RFC1918 addressing that doesn't overlap campus (ask IT which 10.x is free; placeholder `10.66.0.0/16`). **G0c:** `nestedVirtualization=true` gives `/dev/kvm` in WSL on one box (hypothesis; §6). | **0a:** club Mneme on mgmt-0 (new instance, club vaults, bible skeletons, first asset notes). **0b:** the range flake skeleton + render + CI on mgmt-0; NixOS-WSL on each box from it. **0c:** the Aoide mesh over the lab LAN (§10). **0d:** Hoot on cc-mini07/08, Windows-native. **0e:** the BotForge loop T0–T5 on rng-02. **0f:** the Phase 0 mock comp (§14). | The WSL distros (their config survives in the flake; state is exported). Hoot's Windows launcher (`hoot.ps1`), though `models.ini` survives. The Windows installs. Flat addressing *inside* P0-MGMT survives as the MGMT subnet. | Hub H1 + web-ui M1–M3 (the mesh tab); triage-toolkit T0–T5; people.nix with per-officer accounts on mgmt-0; flake check + render diff in CI | An officer, on-site, opens Minerva on mgmt-0 and runs a BotForge exercise on rng-02 through Aoide. The score lands in `results.jsonl`, the report in quarantine, and a promotion ask appears in the officer's pane. An agent PR to the flake passes CI and an officer merges it. |
| **G0.5: university IT sign-off** | Written authorization covering: scope (vulnerable VMs, AD labs, attack tooling on an isolated network), the isolation design (§4.1), no campus bridging, Windows eval licensing, and any overlay or tunnel (§13) | The signed document, filed in `docs/range/` | none | none | IT has signed. Until then, no vulnerable workload spans more than one box and nothing touches campus. |
| **1: Servers up** | **G1:** power and space (circuits per §15.3); the Dells physically present | **1a** inventory servers. **1b** inventory network gear. **1c** label and document (§16.4). **1d** decide hardware roles (§15.1). **1e** rack and cable (§15.3). **1f** bibles v1 (§16). **1g** Proxmox on the R730 from a rendered `answer.toml`; internal-only lab network (MGMT, OOB, STORAGE VLANs if the switch allows); terranix → `tofu apply` for the core VMs. **1h** PBS + corosync-qnetd on the R330. **1i** move mgmt-0 onto NixOS VMs on the R730 (Colmena), restored from the mgmt-0 export. | mgmt-0 on cc-mini01 (config survives; the box returns to the pool). Any hand-edited switch config not yet captured in the flake. | Hub on the R730 mgmt VM; health-capture adapters (§16.3); Mneme tier instances (§11); Melete jump (§10.5); NixOS VM tests + `tofu plan` in CI; forge + runner in MGMT | 1a–1c: every asset note has every schema field filled, or `unknown` with a reason. 1g–1i: mgmt VMs back up nightly to PBS, a **test restore** has passed, and the render diff is clean. The first-pass CI rate of agent PRs is being measured (eject trigger 2). |
| **2: Range cluster** | G0.5 signed. Phase 1 done. **G2a:** a lab switch that is VLAN-capable, or physically separate from campus. **G2b:** each EVO-X2 backed up (§7). **G2c:** a one-box Ludus pilot passes. | **2a** back up. **2b** pilot Ludus on cc-mini02. **2c** convert range nodes one at a time (Proxmox via the Ludus install; fenced host units via system-manager). **2d** AI nodes: Proxmox + inference LXC (hypothesis) or bare metal. **2e** content: GOAD, AD + Elastic, CALDERA/Atomic/Sliver, Security Onion, Splunk Attack Range, DVWA/Juice Shop (reported list), as Nix-authored ranges rendered to Ludus YAML. **2f** comp templates + scoring. | Windows on the converted boxes (one image is kept, of cc-mini02 only; §7). The Phase 0 QEMU practice pipeline (base images carry over). | Ludus adapters (CLI/API/LudusMCP as triage-toolkit adapters); range `target` class; read-only mirrored panes; the scorer VM; T6–T8; Ludus validation in CI | Two teams run a cloned comp topology at once on two nodes. A CALDERA operation runs. Injects fire from a JEV schedule. The scorer posts results. A range is redeployed from `generated/ludus/` with plain `ludus` alone. |
| **3: Edge and web** | **G3a:** a static IP, **or** an outbound tunnel IT approved (§13). **G3b:** IT sign-off extended to internet-facing hosting. | **3a** OPNsense edge on the R730 (terranix). **3b** WEB-DMZ: K3s on NixOS VMs, Flux from the repo. **3c** remote officer and member access over WireGuard on OPNsense (peers rendered from people.nix). **3d** per-member identity everywhere (§19.3). | The interim remote path, if one was used (§13). | Member accounts and profiles; DMZ change asks | A member off-campus reaches their own Ludus range and their own Minerva session over WireGuard. The club site serves from the DMZ. A DMZ change went PR → CI → merge → an officer yes → Flux. |
| **4: CTF hosting** (gated, optional) | Its own IT sign-off, a separate VLAN and host, and Q2 = yes | Internet-facing challenge containers on a dedicated node, with no route to any other zone | none | none | Only if Q2 says yes. Sketch: §13.3. |

**What no early phase may lock in (reasoned):**

- Aoide peers are addressed by URL (observed, CONTRACTS.md §7 "Topology-blind by design"). Moving from LAN to overlay or VLAN addresses is a peer-list edit plus a render, never a protocol change.
- Vault directories are plain files, so moving Mneme is rsync plus a service move.
- The MGMT subnet keeps its Phase 0 numbers.
- Hoot's `models.ini` is the model list on any OS (observed, `Architecture.md` "Serving").
- The one Phase 0 choice that would break later is putting *range* content on the lab LAN before Gate 0.5, and this plan forbids it.

---

## 6. Phase 0 on Windows, in detail

| Choice | Pick | Alternatives weighed | Label |
|---|---|---|---|
| Linux side | **NixOS-WSL** on every box, built from the range flake | Ubuntu-WSL + home-manager can't consume Aoide's NixOS modules (`systemd.user.services`, `networking.firewall`, observed in `modules/nucleus/aoided.nix`). Ubuntu-WSL + system-manager supports Ubuntu/Debian (observed, system-manager README) but again isn't the NixOS modules. | reasoned. **Hypothesis:** NixOS-WSL runs Aoide's modules unchanged apart from desktop/hardware bits. Build `mgmt-0` and start `aoide-a2a` in WSL. |
| Non-Nix recovery | `wsl --export` tarball of each distro, kept on the lab share; the build guide says "`wsl --import`, done" | none | reasoned (§19 rule) |
| Practice images | **QEMU/KVM inside WSL2**, `restrict=on`, an overlay per run (triage-toolkit's pick) | **Hyper-V, Private switch:** native and better for Windows guests, but WSL peers can't reach a Private switch, and an Internal switch touches the host. Keep it for hands-on Windows images with no agent. **VMware Workstation:** the fallback if an OVA won't boot. | observed (triage-toolkit §4.2). `/dev/kvm` needs `nestedVirtualization=true` in `.wslconfig` on Windows 11 (reported, Microsoft WSL issue tracker). **Hypothesis** until `ls -l /dev/kvm` on an EVO-X2. |
| WSL networking | `networkingMode=mirrored`, so the WSL peer binds the box's lab-LAN address | NAT + `netsh portproxy`: every hop looks like it came from the host, the same misattribution the Aoide contract warns about | reported (Microsoft WSL docs). **Hypothesis:** mirrored mode needs a Hyper-V firewall inbound rule for 8710/22. Measure it. |
| WSL memory | `.wslconfig memory=48GB` on range boxes | the default cap is 50 % of RAM (reported, Microsoft docs) | hypothesis: leave ~12 GB for Windows |
| AI plane | **Hoot, Windows-native** on cc-mini07/08: the llama.cpp Vulkan build, the `llama-server` router + `models.ini`, `--api-key`, bound to the lab-LAN address | ROCm on gfx1151: reports split (Vulkan decodes faster, ROCm prefills faster). Inference inside WSL: goes through the GPU paravirtualization layer, unmeasured. | observed (Hoot's shape, `Architecture.md`); reported (github.com/ggml-org/llama.cpp/discussions/20856; AMDVLK issue #420: allocations over ~2 GiB fail on AMDVLK but not RADV). **Hypothesis** for this hardware: measure `llama-bench` Vulkan vs HIP on one box. |
| UMA carve-out | Range boxes: the smallest in BIOS. AI boxes: the largest. | none | reasoned. `yomi-strix` says "unified 32 GB pool … 24 GiB GPU-mappable" (observed, `Aoide/hosts/yomi-strix/default.nix`), which doesn't match the 64 GB spec. Inventory 1c records the BIOS UMA setting and the RAM the OS sees. |
| Where the club Mneme lives | **mgmt-0: NixOS-WSL on cc-mini01**, lab LAN only, **no Funnel**. The vault dirs are git repos, pushed nightly to a second box. | a member's laptop (bus factor); Melete's public server (internet-facing, and range data must never go there) | reasoned |
| Where it moves | Phase 1i: the `mgmt-mneme-01` NixOS VM on the R730, backed up to PBS | none | reasoned |
| Forge + CI in Phase 0 | the repo on its existing host; a CI runner on mgmt-0 for jobs needing the lab (Q8) | none | reasoned; the runner reaches the forge only if mgmt-0's Windows host has internet (Q1) |
| Hub access | Officers on-site: the local browser, or `ssh -L` from a laptop on the lab LAN to their own hub port on mgmt-0 | none in Phase 0 | observed: the hub is loopback-only by design (`hub.md` §2) |

---

## 7. The Phase 2 wipe: what gets backed up first

| Item on each EVO-X2 | Keep? | How | Label |
|---|---|---|---|
| WSL distro state: `~/.aoide/state` (nodes.json, keys) | **No keys**: regenerate and re-pair. Keep `nodes.json` for reference. | Export the file. Re-pair through Minerva (an officer ask). | reasoned: moved keys are worse than fresh pairing |
| eidolon session journals, `results.jsonl`, case logs | yes | copy to the MGMT store, then PBS | reasoned |
| Scorer `answers.toml` | yes, scorer-only | copy as the scorer user to the scorer VM, mode 0700 | observed need (triage-toolkit §4.4) |
| `base-harness.qcow2` (practice images with the harness footprint) | yes | copy to R730 storage; later a Proxmox template | reasoned: it holds operator prep work |
| Hoot GGUF models | yes (saves bandwidth) | copy to R730 storage | reasoned |
| Windows install + OEM key | **one full image** (cc-mini02, the pilot) as a rollback. Record the key state in the asset note for all boxes. | a Clonezilla-style image to R730 storage (operator step) | **hypothesis:** the OEM key is in firmware. Check `wmic path SoftwareLicensingService get OA3xOriginalProductKey` before the wipe. |
| BIOS settings | yes | photographed into the asset note | reasoned |
| The loaned unit (`yomi-strix`) | **not ours to wipe** | it returns first, then gets converted | reported (user) |

---

## 8. The harness: Minerva as the interface

### 8.1 Interaction model

Members and officers work only through Minerva sessions: the hub, the doors, eidolon, JEV and the triage-toolkit primitives. Every change the range makes is one of three things:

- a tool call adjudicated by eidolon's single chokepoint (observed, `permissions.md` "The model in one page", `dispatch.rs:380-434`);
- a JEV run under a warrant;
- a merged PR applied by the fleet loop (§12).

**The human-yes list is the set of calls whose policy verdict is an ask, plus the merge button.** An ask renders inline in the pane of the session that raised it (observed, `web-ui.md` §7.1). Nobody answers from a chat app or a side channel.

| Action | Default | Mechanism | Ask appears in | Who may answer |
|---|---|---|---|---|
| Deploy, reset or snapshot a range; revert a practice VM | **automatic** | an exercise or range **warrant** (graph ids, target, `actions` cap, `wall_s`), the shape observed in `triage-toolkit.md` §3 | none; journaled | the warrant is approved once, by an officer |
| Apply a merged change to NixOS guests (`colmena apply`), non-firewall Proxmox objects (`tofu apply`), or ranges (`ludus`) | **automatic** after merge | a keeper warrant over "apply merged `main`" | none; journaled | none. The merge was the yes. |
| Patch MGMT guests (a flake input bump) | **automatic** after merge | PR → CI → merge → Colmena; rollback = the previous generation | none | the merge |
| Patch Proxmox hosts | security updates automatic; a kernel or PVE major version asks | Ansible check-mode diff in the keeper session | the keeper's pane | officer |
| **Merge any flake PR** | human | forge branch protection: required CI + one officer review; agents can't merge | the forge PR page, linked from the planner's pane | officer (paths in §19.4 need a second officer) |
| Anything touching WEB-DMZ | human yes | FLAG on apply when the plan touches DMZ paths | the keeper's pane, with the diff | officer |
| Firewall, SDN, egress, OPNsense | human yes | FLAG on `tofu apply` when the plan touches `firewall`/`sdn`/`egress` resources | the keeper's pane, with the plan | officer |
| Mneme promotion (inbox or quarantine → shared) | human yes | FLAG on `mneme_promote` (a reviewer tool, §11) | the reviewer's pane, with the diff | officer; a member may propose |
| New mesh peer (pair accept) | human yes | Aoide's pairing prompt (observed, web-ui.md §9: "Pair accept is `y`, a real authorization") | the mesh tab | officer |
| Range-node reinstall (expendable) | human yes | FLAG on `host_reinstall` for a `class: range` host | the keeper's pane | officer |
| Wipes of core, PBS datastores, vaults or template stores | human yes, **two officers** | FLAG plus a second ask in a different officer's hub. That's a gap: eidolon has no two-person rule today (reasoned). | both officers' panes | two officers |
| Secrets set or rotated | human yes | FLAG on `secret_set`-shaped tools | the keeper's pane | officer |
| Melete requests for a job *not* on the automatic list | human yes | jump queue → an ask (§10.5) | the planner's pane | officer |

**Session modes** (the observed ladder, `permissions.md`):

- MGMT sessions run in `table` mode, with warrants for bounded automation.
- **`unattended` (yolo) is forbidden** for any session whose tool set includes a human-yes tool.
- Range executor sessions have a warrant, and a policy table that **denies** (never asks) anything outside it. So they never need a human reply (reasoned: a range session asking up would be an upward channel).

### 8.2 Where the hub runs, and who reaches it

| Phase | Hub location | Remote reach | Identity |
|---|---|---|---|
| 0 | mgmt-0 (NixOS-WSL on cc-mini01), loopback | on-site only: the local browser, or `ssh -L` over the lab LAN | **one Unix account per officer**, from people.nix, each running their own hub |
| 1–2 | `mgmt-hub-01` NixOS VM on the R730, loopback | `ssh -L` to your own account over the lab LAN; later the overlay if §13 picks one for officers | per-officer accounts |
| 3 | same | WireGuard on OPNsense → `ssh -L` to your own account | per-member accounts. Member eidolon profiles **deny** human-yes tools, so those asks never reach a member's pane. |
| never | WEB-DMZ, a public address, a tunnel endpoint | none | none |

**Gap (observed, `hub.md` §10.3):** the hub token is "every door token at once, plus the power to start agents", so each hub has one operator. Per-user accounts with per-user hubs close the gap without a hub change (reasoned). They don't let an officer answer an ask in a *member's* session. That needs hub roles (Q7), and the roles come from the same people.nix (§19.3).

### 8.3 Placement per zone, and range sessions in the UI

- **MGMT:** the hub, the planner, reviewer and keeper, the scorer (its own account), the forge runner, and the top Aoide peer. Only MGMT sessions have doors the hub proxies for writing.
- **RANGE:** one executor peer per range: eidolon + JEV + adapters, with its own local door.
  - The hub **never holds a range door token for writing**.
  - Nothing on the range side holds a MGMT token, a forge token, or a Mneme write credential outside its quarantine (reasoned: any of those is a path up).
- **How a range session appears:** as a **read-only mirrored pane**, fed by MGMT pulling downward:
  - Aoide's `graphSummary`/node cache (observed: CONTRACTS.md §7; web-ui.md §9 "Peer sessions openable as panes");
  - the range agent's report notes in quarantine.

  The pane accepts no `say`, `answer` or `cancel`, and every byte renders as text (observed rule P5, web-ui.md §1). Proposed hub rule: `/s/<id>/api/*` refuses every non-GET route for a session whose origin is a RANGE peer.
- **AI:** not an Aoide peer. It's an OpenAI-compatible endpoint (the `hoot:<model>` provider, observed in `Architecture.md`), reachable on the inference port only.

### 8.4 Melete next to Minerva

Melete schedules and runs club jobs (reported). Minerva is the human interface.

1. **The fleet loop's schedule** (§12): Melete fires fixed job names through the jump. MGMT runs them, and each run is an ordinary MGMT session visible in Minerva.
2. **Club jobs that don't touch the range** (wiki upkeep): an MGMT session polls Melete read-only through eidolon's `remote` crate (observed, `Architecture.md` crates table: "Mneme / Melete clients"; `classify_melete` exists in `policy.rn`, observed in `ssh-triage.md` §3).
3. **How runs render:** a run's `graph_view` output is drawn by the triage-toolkit graph renderer (`@xyflow/system` + Preact, the pick observed in `triage-toolkit.md` §5.1). **Hypothesis:** Melete's graph shape maps onto `graph-adapter.js` through a thin adapter. Measure by feeding one `graph_view` result through it.

### 8.5 What Minerva must build, per phase

| Phase | Harness work | Source of the requirement |
|---|---|---|
| 0 | hub H1 (spawn, list, proxy, redirect-file token); web-ui M1–M3 (panes, asks, tiling, mesh tab); triage-toolkit T0–T5 (`target`, `adapter`, `case`, `exercise`, `remediation`, scorer loop); policy rows for the human-yes list; forge adapters (`pr_open`, `pr_status`; no merge tool exists) | hub.md §13, web-ui.md §12, triage-toolkit.md §7 |
| 1 | health-capture adapters; the `mneme_promote` tool; Melete jump intake; Colmena/Tofu/Ansible adapters that show the plan/diff in the ask | §11, §16.3, §10.5 |
| 2 | Ludus adapters (`ludus_validate`, `ludus_deploy`, `ludus_reset`, `ludus_snapshot`, `ludus_conn_info`, `ludus_destroy`) over the CLI/API or LudusMCP (tools reported: `validate_range_config`, `set_range_config`, `deploy_range`, `get_connection_info`, `destroy_range`); range `target` class; multi-host `exercise`; read-only mirrored panes; triage-toolkit T6–T8 | §8.3, §14 |
| 3 | member accounts and deny-profiles; hub roles (Q7); DMZ change asks | §8.2 |

---

## 9. Capabilities: who may do what

| Capability | MGMT planner | MGMT reviewer | MGMT keeper | RANGE executor | Melete | Officer | Member |
|---|---|---|---|---|---|---|---|
| **Write to the flake (push `agent/*` branches, open PRs)**: the most powerful capability in the range | yes | yes (review comments, promotion PRs) | yes (bumps, drift fixes) | **never** | **never** | yes | yes (PRs, reviewed like agents') |
| Merge to `main` | no | no | no | no | no | **yes** | no |
| Apply merged `main` (automatic categories) | no | no | yes, under warrant | no | no | yes | no |
| Answer human-yes asks | no | no | no | no | no | yes | no (denied by profile) |
| Aoide spawn/message down | yes | no | yes | no (nothing below it) | no | via sessions | own ranges only (Phase 3) |
| Mneme write | inbox | inbox | inbox | own quarantine | none | shared | inbox (proposals) |
| Forge token | yes (scoped) | yes (scoped) | yes (scoped) | **none** | **none** | own | own |

**Why flake write is gated this hard (reasoned):** whoever edits the flake controls every host on the next apply.

- A range agent can be prompt-injected by attacker-authored content, so it gets no forge credential at all.
- A MGMT planner that has read range reports is **tainted** (§10.4). Its PRs carry a `tainted-origin` label from the session journal and need a second officer review.
- CI plus a human merge sit between any agent and every host. Agents never self-merge. Branch protection enforces that at the forge, not by agent compliance.

---

## 10. Aoide: the mesh, and who may command whom

### 10.1 The knobs (observed, CONTRACTS.md §6–§7)

| Knob | Where | Meaning |
|---|---|---|
| `aoide.a2a.enable`, `bindAddress`, `port` (8710) | NixOS option (observed, `modules/nucleus/aoided.nix:101-126`) | Off by default; bound to loopback by default. |
| `tokenFile` / `bearerSecret` | NixOS option | Once set, Spawn needs the token and loopback stops being trusted. |
| `nodes.json` entry: `verified`, `pubkey`, `allows ⊆ {read, spawn, message}`, `autogate`, `tokenFile` | the **receiving** side's registry | Spawn and Message need `verified`, the capability in `allows`, and a **signature**-rung caller. Inject (`message/send` into a session) auto-delivers only from `autogate` nodes; otherwise it waits in `pending.json` for a human. |
| `pairing.defaultGrant` | `config.toml`, default `["read"]` | What a freshly paired node gets. |
| `discoveryAdvertise` | NixOS option; opens UDP 8711 | LAN advertisement. **Off in the lab**: peers come from the flake. |

**The direction rule, made mechanical.** A grant lives in the *receiver's* registry. So "commands flow down" means two things:

- RANGE peers register MGMT with `spawn` and `message`.
- **MGMT doesn't register RANGE peers at all.** A range caller then resolves to no node, and its Spawn and Message calls are refused (reasoned from the resolve ladder: an unmatched caller can't meet the `verified` + signature requirement).

The peer list and grants are declared in `nucleus/mesh.nix` and rendered to `generated/aoide/peers.json`. CI fails if any MGMT peer's rendered registry lists a RANGE, SVC or DMZ peer (reasoned check).

### 10.2 Grant table (entries in the receiver's `nodes.json`)

| Receiver ↓ / caller → | MGMT peer | RANGE peer (same range) | RANGE peer (other range) | WEB-DMZ | AI | Melete |
|---|---|---|---|---|---|---|
| **MGMT peer** | `verified`, `allows [read, spawn, message]`, `autogate: true` (planner ↔ reviewer ↔ keeper) | **not registered**; the firewall drops 8710 from RANGE/SVC | not registered | not registered | n/a (not a peer) | not registered; jump only (§10.5) |
| **RANGE peer** | `verified`, `allows [read, spawn, message]`, `autogate: true`. Commands flow down, and the range is disposable. | n/a (one peer per range) | not registered; the firewall isolates ranges | not registered | n/a | not registered |
| **WEB-DMZ** | **No Aoide peer in the DMZ.** Changes arrive as merged `main` via Flux, after an officer yes. | none | none | none | none | none |
| **AI** | not a peer. `llama-server --api-key`; the key is held by MGMT and each range executor. | inference port only (firewall) | same | none | none | none |
| **Phase 0 equivalents** | P0-MGMT (mgmt-0) registers only P0-MGMT peers | P0-RANGE (rng-0N) registers mgmt-0 with `[read, spawn, message]`, `autogate: true` | no rng-0N ever registers another rng | none | P0-AI (Hoot on Windows) is not a peer | none |

**Prerequisites before any cross-host entry exists (G0a):**

1. The CSRF fix (`fix/a2a-browser-origin`) is merged and deployed.
2. `tokenFile` is set on every peer, and each node's own token is registered with `node add --token-file`.
3. Pairing goes through the ceremony (signature rung), never `node add` alone.
4. Transport is either `node.via` ssh tunnels or a peer bound to its address inside WireGuard. A plain-HTTP A2A bind on a shared LAN is refused (reasoned: bearer tokens would cross in cleartext).

**Why `ssh -L` is acceptable only with tokens set (reasoned):**

- The far end of an `ssh -L` terminates on the receiver's loopback, so the receiver's `peer_addr()` is `127.0.0.1`.
- Without a token, that is exactly the hole the contract describes: the tunnelled caller is "misclassified as `ConnOrigin::Loopback`" (observed, CONTRACTS.md §6 "Security invariant").
- With a token set, the contract coerces tokenless loopback callers to `Unknown` (observed, 2026-08-19 amendment).

### 10.3 Fan-out

The keeper's apply step and the planner's exercise orders use Aoide to fan commands **down**: `spawn`/`message` to RANGE peers. There is never a call up and never a lateral call between ranges.

### 10.4 What reports up look like

Range executors don't message MGMT. They:

- write findings, case summaries and escalations into their range's **quarantine vault** (§11);
- expose their session graph to MGMT's **pull** (`graphSummary`, MGMT → RANGE, which is down).

Any MGMT session that reads either is **tainted** for its lifetime:

- it can't promote to Mneme;
- it can't answer asks;
- its PRs get the `tainted-origin` label (§9);
- its human-yes calls still ask.

### 10.5 Melete: the audited jump

- Melete reaches exactly one thing: `mgmt-jump-01`, over ssh. The key's `authorized_keys` line carries `command="…/range-request"` and `restrict`.
- `range-request` accepts one JSON line: `{job, args}`. `job` must be from a **fixed enum**: the automatic jobs in §12.3. Anything else is queued as an ask.
- Everything is logged. A compromised Melete can at worst trigger allow-listed jobs at the wrong time (reasoned).
- MGMT systemd timers run the same jobs if Melete or the jump is down.
- No Melete key exists in RANGE or SVC, and Melete's `secret_set` never holds lab secrets.

---

## 11. Mneme: the club knowledge hub, in tiers

**The personal Mneme (`magi`, `rook`, `necoconeco`) is never configured in any lab agent, host or flake.** A flake check greps for its vault names and URL, and fails on a hit.

| Tier | Vault | Written by | Read by | Enforced by |
|---|---|---|---|---|
| **T1 shared** | `club`: assets, bibles mirror, runbooks, promoted knowledge | officers; `mneme_promote` after an officer yes | every lab agent; members | separate instances + filesystem permissions |
| **T2 inbox** | `club-inbox`: MGMT agent output (health captures, inventory fix proposals, fleet-loop reports, reviewer diffs) | MGMT agents (trusted origin, agent-authored) | officers, the reviewer | same |
| **T3 quarantine** | `q-<range-id>`, one per range: range reports, findings, Ludus/Proxmox state captured *from inside* ranges | that range's executor only | the reviewer (tainted), officers | same |
| **T0 personal** | the user's vaults | the user | nobody in the lab | not installed anywhere in the lab |

**Why not rely on Mneme's write zones.** The vault conventions describe write zones as vault rules (observed, `get_conventions`: "write-zones … The non-negotiables"). This doc found no evidence that the server enforces them per client (**hypothesis**: check how the Mneme source handles zones). Agent compliance isn't a boundary, so the tiers are **separate Mneme processes with filesystem permissions**:

| Instance | Zone | Serves | Mode |
|---|---|---|---|
| `mneme-officer` | MGMT | `club` rw, `club-inbox` rw, `q-*` ro | the officers' group |
| `mneme-agent` | MGMT | `club` ro (read-only bind mount), `club-inbox` rw, `q-*` ro | the agent user |
| `mneme-range` | SVC (Phase 2+) | `club-ro` (a one-way rsync **pushed** from MGMT), `q-<id>` rw per range | its own VM with no route to MGMT. MGMT **pulls** `q-*` one-way. |
| Phase 0 | mgmt-0 | the two MGMT instances on one WSL. Range boxes write quarantine locally on rng-0N, and mgmt-0 pulls it. | reasoned; the SVC VM doesn't exist yet |

These are a club-owned `dendrites/mneme.nix`: several instances, no Funnel. Aoide's dendrite isn't reused, because its Funnel is unconditional and it takes one vault (observed).

**Promotion:**

1. The reviewer writes a proposal into `club-inbox`: source, target, diff, and why.
2. `mneme_promote` raises an ask.
3. An officer answers yes, and `mneme-officer` writes it.

Inventory is human-entered; agents only propose.

---

## 12. Agents run the fleet

Humans keep the rules, the merge button and the human-yes asks. Agents do the toil.

### 12.1 The loop

```text
      ┌──────────────────────────────── RECORD ◄────────────────────────────────┐
      │  decisions, runs, incidents → club-inbox / quarantine; bibles re-render  │
      ▼                                                                          │
   OBSERVE ─────────► PLAN ─────────► CHANGE ─────────► APPROVE ─────► APPLY ─────► VERIFY
   health capture     MGMT eidolon    agent edits the   CI gates:      colmena      post-checks
   (smartctl, racadm, sessions in     flake on agent/*  flake check,   apply,       (remediation
   lshw), monitoring, Minerva,        and opens a PR    render diff,   tofu apply,  pattern),
   Ludus/PVE state,   scheduled by    (range agents:    VM tests,      ludus        PBS restore
   drift (tofu plan,  Melete (fixed   never)            tofu plan,     deploy,      tests, CI
   colmena diff)      job names) or                     ludus validate ansible      re-run
   → inbox; range-    MGMT timers                       → a HUMAN      (fenced),
   sourced →                                            merges; a      aoide fan-out
   quarantine                                           human-yes      DOWN only
                                                        item → an
                                                        inline ask
```

### 12.2 Stages

| Stage | Who | Tools | Output | Human part |
|---|---|---|---|---|
| Observe | keeper (MGMT); range executors for in-range state | health-capture adapters, node exporter, `tofu plan -detailed-exitcode`, `colmena` dry-activate, `ludus range status`, `pvesh` reads | notes in `club-inbox` (host data) or `q-*` (range data) | none |
| Plan | planner (MGMT) | reads inbox, bibles and the flake | a plan in its session; a PR if a change is needed | none |
| Change | planner / keeper | git on `agent/*`, `pr_open` | a PR with CI results + `tofu plan` + render diff | none |
| Approve | CI, then an officer | the forge | a merge (and asks for human-yes items) | **merge; answer asks** |
| Apply | keeper | `colmena apply`, `tofu apply`, `ludus` deploy/reset, `ansible-playbook` on `generated/ansible/`, system-manager switch on hosts, Aoide spawn/message down | an applied state | asks only for human-yes items |
| Verify | keeper | the remediation pattern (precheck, change, postcheck, rollback note; observed, `triage-toolkit.md` §2), PBS restore test, a CI re-run on `main` | a pass/fail note | none unless it fails |
| Record | all | Mneme inbox, render → bibles | notes; regenerated bibles | promotion asks |

### 12.3 Recurring jobs

| Job | Trigger | Agent role | Tools | Automatic | Human ask | If the agent is down (runbook page) |
|---|---|---|---|---|---|---|
| Health capture / inventory refresh | weekly (Melete `inventory-refresh`) | keeper | read adapters (§16.3) | all | promotion of inventory fixes | §21.3 "Capture by hand" |
| Drift detection and correction | nightly (`drift-check`) | keeper | `tofu plan`, Colmena diff, system-manager diff, `ludus range status` | re-apply for NixOS guests and non-firewall Proxmox objects | drift in firewall/SDN/egress/DMZ; unexplained drift becomes an incident note | §21.4 "Drift by hand" |
| Patching | weekly window (`patch-window`) | keeper | `nix flake update` PR; Ansible check → apply on hosts | guest apply after merge; host security updates | the merge; kernel/PVE major versions | §21.8 "Patch by hand" |
| Windows eval-image rebuilds | template age > 150 days (180-day eval, reported) | keeper | `ludus templates build`, through the MGMT caching proxy with a fixed egress allowlist | the rebuild | changing the egress allowlist | §21.2 "Reset/rebuild ranges" |
| Reset after comps | the comp's end time (scenario runbook) | executor (MGMT) | `ludus` reset/snapshot revert via Aoide down | all (comp warrant) | none | §21.2 |
| Backup verification | nightly PBS verify; monthly test restore into an isolated test VLAN | keeper | PBS API, `qmrestore` | all | none unless it fails | §21.5 "Restore a VM" |
| Capacity check | daily | keeper | `pvesh`, Ludus range list | report into inbox | none | read the Proxmox UI |
| New member onboarding | an officer adds the person to `nucleus/people.nix` (or an agent drafts that PR from a signup form) | planner | PR → CI → merge → apply: NixOS account, hub/eidolon profile, Proxmox user/pool, Ludus user + range, (Phase 3) a WireGuard peer | apply after merge | the merge; a WireGuard peer counts as firewall, so it asks | §21.9 "Accounts by hand" |
| Adding a host | a new asset note (human) | planner | PR: `hosts/pve/<id>.nix` → rendered `answer.toml`, Tofu, system-manager set | Tofu for non-firewall objects after merge | the merge; **a human boots the installer USB**; the new mesh peer asks | §21.6 "Rebuild a node" |
| Retiring a host | an officer decision | planner | PR removes the host; drain its ranges | draining | the merge; the wipe (range node: one officer; core: two); a human pulls the hardware | §21.6 |
| Recovering a failed node | a monitoring alert or a failed capture | keeper | ssh-triage reads; a Ludus redeploy onto the other nodes | diagnosis; redeploying ranges elsewhere | a node reinstall asks; **a human presses power** (no BMC on the minis) | §21.6 |
| Cert/secret rotation | expiry calendar (sops metadata); Aoide tokens each term | keeper | generate + sops-encrypt to recipients in a PR | the PR | the merge; `secret_set` asks | §21.10 "Rotate by hand" |

### 12.4 What agents can't do

- **Physical work:** racking, cabling, drive swaps, the power button on a mini with no BMC, installer USBs, labels, photos.
- **The university:** IT sign-off, the campus network, the static IP, circuits.
- **Final merges and every human-yes answer.**
- **Judging promotion into shared knowledge.** They propose; an officer decides.
- **Anything that needs a credential on the human-only list**, such as buying things or registrar/Cloudflare accounts.

---

## 13. Reaching the lab without a static IP

### 13.1 Mesh and remote admin

| Option | Needs | Third party? What crosses it | Fits | Label / source |
|---|---|---|---|---|
| **LAN-only** | a lab switch | none | Phases 0–2 default; on-site only | reasoned |
| **Tailscale (SaaS)** | outbound internet from the lab | Tailscale Inc. The coordinator sees node keys, device names, OS, endpoint IPs, user identities and ACLs. DERP relays carry end-to-end-encrypted WireGuard. The free "Personal" plan: 6 users, unlimited user devices, 50 tagged resources, 3 ACL groups (Aug 2026). | officer remote admin into MGMT only | reported: tailscale.com/pricing; tailscale.com/docs/account/manage-plans/free-plans-discounts |
| **Headscale** (self-hosted Tailscale control) | a server with a public IP (a small VPS); HTTPS; embedded DERP needs 443/tcp + 3478/udp | the VPS provider sees metadata; you run the coordinator (upkeep, cost) | if SaaS is unacceptable to IT | reported: headscale.net/stable/setup/requirements/; headscale.net/stable/ref/derp/ |
| **Nebula** | a lighthouse on a persistent public IPv4, UDP 4242 | the VPS provider (metadata) | as Headscale | reported: nebula.defined.net/docs/config/lighthouse/; github.com/slackhq/nebula/discussions/809 |
| **Plain WireGuard** | one endpoint reachable by the others (a VPS, or the static IP later) | a VPS provider if one is used (metadata) | the Phase 3 end state on OPNsense; on the LAN, usable any time as encryption for peer links | reasoned |

**Default:**

- LAN-only through Phase 2.
- At G3, plain WireGuard on OPNsense, with peers rendered from people.nix.
- If officers need remote admin before a static IP: Tailscale SaaS on **MGMT hosts only, never RANGE or SVC**, and only with IT's written OK. Every overlay tunnels past the campus perimeter (reasoned).
- Aoide URLs use club DNS names, not MagicDNS, so the overlay can be swapped.

### 13.2 Public web hosting

| Option | How | Third party? What crosses it | Verdict | Source |
|---|---|---|---|---|
| **External static hosting** (e.g. GitHub Pages) for the club site | no lab exposure | the host serves public content only | **default until G3** | reasoned |
| Cloudflare Tunnel | `cloudflared` dials out; needs a domain on Cloudflare DNS; free | Cloudflare **terminates TLS** and sees plaintext, including logins | public static content only | reported: developers.cloudflare.com/cloudflare-one/connections/connect-networks/; pluggie.io/blog/cloudflare-tunnel-tls-privacy |
| Tailscale Funnel | ports 443/8443/10000 only; a `*.ts.net` name | TLS terminates **on the node**; Tailscale sees metadata. Bandwidth limits exist but aren't published. | small member demos after IT OK | observed: tailscale.com/kb/1223/funnel (fetched 2026-09-25) |
| Wait for the static IP | OPNsense → WEB-DMZ | ISP/campus only | **default for member containers** | reasoned |

### 13.3 Phase 4 sketch: internet-facing CTF hosting (gated, Q2)

- A dedicated node (not a range node, not the R730) on its own VLAN, egress-denied, with no route to any zone.
- A CTFd-style board plus per-challenge containers, rebuilt on a timer.
- Its own IT sign-off. No agent peer, no Mneme. Logs are shipped one-way to MGMT.

---

## 14. BotForge and mock comps, per phase

| Phase | Practice-image benchmark (the triage-toolkit loop) | Mock competitions |
|---|---|---|
| 0 | Exactly as `triage-toolkit.md` §3–§4 on rng-0N: QEMU/KVM in WSL, `restrict=on`, an overlay per run, the `minerva-scorer` user holding `answers.toml`, `results.jsonl`. The planner in mgmt-0 drives the rng peer via Aoide (down). The executor reaches the VM at `127.0.0.1:2222`. Bench mode: no web, browser or mneme tools. | **Hardening-image comps** (CyberPatriot-shaped): one practice image per member or team, one box each, hands-on, scored by the scorer user. No network topology, no AD. |
| 1 | Unchanged; the scorer moves to its own account on the R730 mgmt plane. After G0.5: a small multi-VM Linux topology on an isolated R730 VLAN as a rehearsal. | Same, plus a two-host rehearsal after G0.5. |
| 2 | `base-harness.qcow2` becomes a Proxmox template inside a Ludus range, and reset is a snapshot rollback. The executor peer sits in the range VLAN. The **scorer lives in MGMT** and reaches down over ssh, so the answers never enter RANGE. Fedora and canary images join (T8). | **Team topologies** (CCDC/CPTC/NCL-shaped, reported): one Nix-authored range per comp, rendered to Ludus YAML, one team per node (§15.4). A scoring-engine VM polls services. Injects run on a JEV schedule. Red: CALDERA + Atomic + Sliver. Blue: Security Onion/Elastic (reported). |
| 3 | Unchanged. | Remote teams over WireGuard; the optional Phase 4 public CTF. |

---

## 15. Hardware

### 15.1 Hardware roles (decision table, provisional while the servers are down)

| Asset ID | Hardware | Phase 0 role | End-state role (default, from research) | Why | Status |
|---|---|---|---|---|---|
| `cc-mini01` | EVO-X2 | mgmt-0 | range compute | the mgmt plane moves to the R730 in 1i | provisional |
| `cc-mini02` | EVO-X2 | rng-02 (BotForge loop) | range compute; **Ludus pilot (2b)** | the first wipe, fully imaged | provisional |
| `cc-mini03`..`06` | EVO-X2 | rng-0N (practice images) | range compute | none | provisional |
| `cc-mini07`, `08` | EVO-X2 | Hoot (Windows-native) | AI: Proxmox + inference LXC (hypothesis) or bare metal | the same boxes in both phases, so no role churn | provisional |
| one of the above | EVO-X2 | **on loan** as `yomi-strix` (user's flake) | returns to the pool | reported (user); the serial in 1c identifies it | on-loan |
| `cc-srv01` | Dell R730 | not up | core: Proxmox, OPNsense, NixOS mgmt VMs, NFS | ECC, iDRAC, bays | provisional |
| `cc-srv02` | Dell R330 | not up | PBS + corosync-qnetd | a separate failure domain for backups | provisional |
| `cc-sw01`… | switch(es) | lab switch | a managed VLAN switch, if 1b finds one | none | unknown |

### 15.2 Cluster topology (default: two failure domains)

| Option | Quorum | Pros | Cons | Verdict |
|---|---|---|---|---|
| **One cluster** (Dells + EVO-X2s, Ludus cluster mode) | 8 PVE votes (+1 QDevice = 9, needs 5) | one UI, live migration | Rolling wipes and reboots of expendable range nodes eat core quorum. Ludus "will completely take over the machine" (reported) on a pmxcfs that also holds OPNsense and the mgmt configs. Cluster mode needs a hand-made VXLAN SDN zone + shared storage (reported). | **rejected** (reasoned); no evidence found that mixing is supported or safe |
| **Two domains: core = R730 standalone Proxmox; range = independent single-node Ludus servers** | none needed; the R330's qnetd stays idle until a cluster exists | A dead range node loses only its ranges. No shared storage. Each node is rebuilt from the rendered answer file + Ludus install + `generated/ludus/`. | 6 Ludus APIs (the adapters take a `ludus_server` name); 200 GB of templates per node (observed Ludus minimum) | **default** |
| Two clusters: core + a 6-node Ludus range cluster | 6 votes + QDevice on the R330 = 7, needs 4, tolerates 3 down | cross-node ranges, shared templates | VXLAN + shared storage; NFS from the R730 couples range to core | revisit only if a comp topology outgrows a node (Q6) |

**AI nodes:** standalone Proxmox single nodes.

- **Hypothesis:** a privileged LXC with `/dev/kfd` + `/dev/dri` passed through runs llama.cpp (Vulkan or HIP) on gfx1151 under the Proxmox kernel. Measure with `llama-bench` in the LXC against bare metal on the same box.
- **Fallback:** bare-metal Linux on those one or two boxes only.

### 15.3 Rack and cable checklist (step 1e)

| Item | Check | Figure | Label |
|---|---|---|---|
| EVO-X2 power | budget **160 W each**: ~120 W sustained, 140 W short bursts, 147–160 W measured on 70B inference; 230 W adapter | 8 × 160 = 1,280 W | reported: notebookcheck.net and servethehome.com EVO-X2 reviews |
| R730 | PSUs are 495, 750 or 1100 W (Platinum), ×2 redundant. Read the actual label in 1a and the draw from iDRAC. | budget 500 W until measured | observed: Dell R730 spec sheet (search summary); hypothesis on the draw |
| R330 | 350 W PSUs, ×2 redundant | budget 150 W | reported: Dell R330 owner's manual (search summary) |
| Switch and edge gear | from 1b | budget 100 W | hypothesis |
| **Total** | ~2.0 kW worst case. A 15 A/120 V circuit gives 1,440 W continuous (80 % rule). | → **two circuits, or one 20 A** | reasoned (US assumption, Q10) |
| PDU | one per circuit; label each outlet with its asset ID; put the Dell PSU 1 and PSU 2 on **different** PDUs | none | reasoned |
| UPS | at minimum the R730, R330 and switch; range minis may drop | none | reasoned |
| Airflow | servers front-to-back, hot aisle at the rear. Minis on a shelf with exhaust to the rear and a gap between units; never stack them on vents. | none | hypothesis: check the EVO-X2 vent direction in 1c |
| Cable labels | both ends: `cc-srv01:nic1 ⇄ cc-sw01:p12`; colour by zone | none | reasoned |
| Expansion | leave 2U above the R730; one free outlet per PDU; switch ports in contiguous blocks per zone | none | reasoned |
| No BMC on the minis | a **switched PDU** (per-outlet power cycle), or a human | none | reasoned |

### 15.4 Capacity math

Formula: `ranges_per_node = floor((RAM_os_sees − host_reserve) / RAM_per_range)`.

| Phase | Node | RAM the OS sees | Reserve | Per unit | Concurrent |
|---|---|---|---|---|---|
| 0 | range EVO-X2 on Windows | 64 GB **if** the carve-out is minimal (yomi-strix shows a 32 GB view; hypothesis) | Windows ~12 GB; WSL cap 48 GB | BotForge 8 GB recommended, 4 GB minimum (observed, triage-toolkit §4.1) | **~5 per box**. Mini02–06 minus the loan = 4 boxes → **~20 practice VMs**. |
| 0 | mgmt-0 | 64 GB | Windows 12 GB | Mneme ~2, hub+doors ~4, 3–5 eidolon sessions ~8, CI runner ~4 (hypothesis) | fits |
| 0 | AI EVO-X2 | 64 GB, maximum carve-out | none | Qwen 27B Q4 is 18 GB (observed, Architecture.md) | one large model or several small ones per box |
| 2 | range EVO-X2 on Proxmox | ~63 GB | Proxmox ~3 GB | Ludus guideline **32 GB per range** (observed, ludus.cloud/docs/intro) | **1 per node by the guideline → 6 ranges.** At ~16 GB per light range, ~3 per node → ~18. **Hypothesis:** measure a deployed GOAD-Light with `pvesh get /nodes/<n>/status`. |
| 2 | CCDC-style team topology | none | none | ~6–10 VMs, ~30–40 GB (hypothesis) | **one team per node → 5–6 teams**, leaving one node for red, scoring and infra |
| 2 | disk per range node | none | none | 200 GB of templates + 50 GB per range (observed, Ludus docs) | 1 TB SSD → ~16 ranges' worth; RAM binds first |
| 1–3 | R730 | unknown until 1a (hypothesis: up to 768 GB) | none | OPNsense 4, ~6 mgmt VMs × 4–8, forge+runner 8, DMZ K3s 3 × 8, scorer 4 | inventory decides |

---

## 16. The bibles

**Where:** under `docs/range/` in the Minerva repo. The generated bibles are rendered by `nix build .#render` into `docs/range/generated/`, and CI fails on a diff. They are mirrored read-only into the club Mneme `club/bibles/` for agents.

| Bible | Sections | Authoritative for | Source of truth | Agents may | Humans |
|---|---|---|---|---|---|
| **Physical inventory** | per-asset table; missing/broken; loans; photo index | what exists, where, in what condition | **one Mneme note per asset** in `club/assets/` → `inventory.md` generated | propose fixes (T2), attach captures | enter and approve |
| **Topology diagram** | zone diagram (§4); per-host guest map; cabling map | how things connect | `nucleus/net.nix` + host defs + `cabling` data → generated | regenerate | edit the data via PR |
| **IP/VLAN plan** | supernet; per-zone VLAN ID, subnet, gateway, DHCP/static ranges; per-range allocation; WAN (empty until the static IP) | addressing | `nucleus/net.nix` → Tofu JSON and the markdown, from one evaluation | regenerate | edit via PR |
| **Build guide** | per host: bare metal → in service; per guest: blank → in service; **a non-Nix recovery path for every NixOS guest**; the "ejecting from Nix" page | how to rebuild anything | hand-written | propose edits | own |
| **Scenario runbook** | per scenario: goal, the range file, targets, scoring, injects, known issues, teardown | how to run a comp or exercise | hand-written, linking `generated/ludus/` | propose; append run notes | own |
| **Reset procedure** | per range, practice image, mgmt VM and host | how to get back to known-good | hand-written; tested at each phase gate | run the automatic resets only | own |

**The IP/VLAN plan works now and survives the static IP (reasoned):**

- Internal space: `10.66.0.0/16` (placeholder until IT confirms).
- MGMT v10 `10.66.10.0/24`: **used flat in Phase 0, with the same numbers**.
- AI v20 `.20.0/24`, SVC v30, WEB-DMZ v40, STORAGE v50, OOB v99.
- RANGE v100–v199, or Ludus's per-user `10.x` inside each node (reported), kept disjoint from the above.
- The static IP fills only the WAN section of OPNsense.

### 16.1 Asset note schema (`club/assets/<id>.md`)

```yaml
---
id: cc-srv01              # permanent; = bare-metal hostname = rack label
kind: server              # server | minipc | switch | router | pdu | ups | cable-kit | rails
model: Dell PowerEdge R730
serial: "..."
service_tag: "..."
cpu: "2x E5-26xx"         # as read, not as assumed
ram_gb: 0                 # as the OS/iDRAC sees it
nics: [{name: nic1, speed: 1G, type: RJ45}, {name: sfp1, speed: 10G, type: SFP+}]
bays: {count: 8, form: 3.5in}
drives: [{bay: 0, model: "...", size_gb: 0, smart: PASSED, hours: 0, realloc: 0}]
raid: "PERC H730, mode HBA|RAID"
psus: [{slot: 1, watts: 750, ok: true}, {slot: 2, watts: 750, ok: true}]
boots: yes                # yes | no | untested
uma_carveout_gb: null     # minis only
location: {rack: R1, u: "10-11"}
photos: [assets/_media/cc-srv01/2026-10-01-front.jpg, assets/_media/cc-srv01/2026-10-01-rear.jpg]
missing_broken: ["rail kit missing"]
status: in-service        # in-service | spare | broken | on-loan | retired
loan: null                # {to: user, hostname: yomi-strix, since: ...}
role: core                # from §15.1; a role change never renames the asset
aoide_nodes: []           # guest hostnames running Aoide on this asset
updated: 2026-10-01
---
```

**ID scheme (reasoned):**

- Asset IDs are role-free and permanent: `cc-<kind><nn>` (`cc-mini01`..`08`, `cc-srv01`/`02`, `cc-sw01`, `cc-pdu01`, `cc-ups01`).
- Bare-metal hostname = asset ID = rack-label text (`cc-srv01 · U10-11`). Roles live in the flake and in DNS aliases.
- Guests: `<zone>-<role>-<nn>` (`mgmt-hub-01`, `rng-agent-t03`).
- **Aoide node name = the guest hostname.** Aoide defaults `instance.name` to the OS hostname (observed, CONTRACTS.md `graphSummary`).
- Phase 0 WSL peers: `<asset>-wsl`.
- The flake's `hosts/pve/<asset-id>.nix` uses the asset ID, so asset note, host file and label all share one key.

### 16.2 Photos and captures

Mneme has no upload call (observed), so both go in by path convention:

- **Photos:** `club/assets/_media/<id>/<yyyy-mm-dd>-<front|rear|label|bios>.jpg`, copied in by an officer and linked from the note.
- **Captures:** `club-inbox/captures/<id>/<yyyy-mm-dd>/<tool>.txt|json`.

### 16.3 Health capture (read-only; how agents keep the inventory current)

A keeper JEV graph runs read adapters over ssh (the ssh-triage shape: argv only, host allow-list from config; observed, `ssh-triage.md` §1), under a `reads` warrant.

| Source | Commands |
|---|---|
| Linux/Proxmox | `lshw -json`, `smartctl -a -j /dev/<d>`, `dmidecode -t system,memory,baseboard`, `lsblk -J`, `ip -j link` |
| iDRAC (OOB VLAN, read-only account) | `racadm getsysinfo`, `racadm getsensorinfo`, `racadm getsel` |
| Phase 0 Windows (over Windows OpenSSH) | `Get-CimInstance Win32_ComputerSystem`, `Get-PhysicalDisk \| Get-StorageReliabilityCounter` |

On Proxmox hosts the capture is also a system-manager-installed systemd timer (§19.2), so it runs even when agents are down. Output goes to `club-inbox/captures/…`. The keeper proposes a frontmatter diff, and an officer applies it.

### 16.4 Phase 1 steps 1a–1c

| Step | Work | Done when |
|---|---|---|
| **1a** Inventory servers | model, CPU/RAM, NIC count and speeds, bays, drives present and SMART health, RAID controller and mode, PSUs, whether it boots (POST + iDRAC reachable) | the R730 and R330 notes have every field filled or `unknown: <reason>`, with a capture attached |
| **1b** Inventory network gear | switches (managed? VLANs? ports and speeds), router/firewall, SFP modules, uplink, patch and console cables, rails, PDUs, UPS units | one note per item, plus a `network-gear-checklist` note listing what to buy or borrow |
| **1c** Label and document | assign IDs, print labels, photograph front and rear, record serials and service tags, log missing or broken. **Includes all 8 EVO-X2s** (BIOS UMA, RAM the OS sees, SSD size, NIC) and the loaned unit. | every item carries a label matching a note, and the generated `inventory.md` builds with zero schema errors |

1a–1c need no network and may start during Phase 0.

---

## 17. Agent roles

| Role | Zone | Plans? | Executes? | Reviews? | Model | Reads range output? | Flake | Human-yes |
|---|---|---|---|---|---|---|---|---|
| **Planner** | MGMT | yes: exercises, graphs, scripts, flake changes | orders JEV / range peers | no | any eidolon model (Hoot or cloud) | yes, via reports → **tainted** | PRs (tainted-labelled if tainted) | raises asks; can't answer |
| **Executor (range)** | RANGE | no | JEV + adapters on range targets | no | Hoot via the inference port, or none | it is range-side | **none** | none (denied) |
| **Executor (mgmt)** | MGMT | no | resets, captures, fan-out | no | none / small | no | none | via the keeper's asks |
| **Reviewer** | MGMT | no | no | yes: quarantine, inbox, PR diffs, Tofu plans | a different model from the planner (reasoned: correlated injection) | yes → tainted | review comments, promotion PRs | raises `mneme_promote` asks |
| **Keeper** | MGMT | no | apply, patch, drift, backups, rotation | no | none / small | no | bump/drift PRs | asks for human-yes applies |
| **Scorer** | MGMT, own account | no | scores only | no | none | the practice VM, *after* the run | none | none |
| **Officer** (human) | via the hub | owns the rules | answers asks | merges; final say on promotion and wipes | none | none | merge | answers |

---

## 18. Risks

| # | Risk | Mitigation |
|---|---|---|
| 1 | **Prompt injection from range content** reaches MGMT | Commands flow down only (§10.2). Readers are tainted: no asks, no promotion, labelled PRs. Human-yes items ask. Range sessions deny rather than ask. |
| 2 | **A malicious or buggy flake PR** controls every host | Range agents have no forge credential. CI gates. A human merges. Protected paths need two officers (§19.4). Tainted-origin labels. |
| 3 | Campus policy: overlays, tunnels or vulnerable VMs violate the AUP | G0.5 before anything leaves one box; overlays and tunnels named in the sign-off; no campus bridge |
| 4 | Aoide cross-host without the CSRF fix or tokens: loopback trust via `ssh -L` | G0a is a hard gate; the render check refuses a peer without `tokenFile` |
| 5 | Personal Mneme contamination | never configured in the lab; the flake check greps for it |
| 6 | Consumer hardware 24/7: no ECC, no BMC, thermals | range nodes are expendable; switched PDU; core on the Dells |
| 7 | Ludus or PVE fights the fenced system-manager set | the fence (§19.2) never owns network, pmxcfs, corosync or Ludus files; drift check; eject to Ansible copies |
| 8 | Bus factor: Nix readers graduate | the eject rule (§1, §19.1); daily ops on plain files; PBS restore for NixOS guests |
| 9 | Hub is single-operator | per-person accounts now; roles later from the same people.nix (Q7) |
| 10 | Melete (internet-facing) compromise | forced-command jump with fixed job names; no lab secrets on Melete |
| 11 | Windows eval images expire at 180 days (reported) | the scheduled rebuild job (§12.3) |
| 12 | Strix Halo inference in an LXC fails | hypothesis tested at 2d; bare-metal fallback on the AI boxes only |
| 13 | WSL quirks block Phase 0 | G0c measured on one box first; Hyper-V/VMware fallbacks (§6) |
| 14 | CI needs lab access (`tofu plan`, Ludus validate, KVM VM tests) | the runner lives in MGMT and runs only jobs for repo-member branches, never fork PRs (Q8) |
| 15 | Circuits not available | the G1 gate; run fewer nodes until then |

---

## 19. Declarative layers: Nix authors, plain formats run

### 19.1 The layer table

| Layer | Authored in the flake as | Rendered to (committed) | Applied by | Eject path | Stays imperative |
|---|---|---|---|---|---|
| Physical install (every host) | `hosts/pve/<id>.nix` → the `render-install` dendrite (`pkgs.formats.toml`) | `generated/install/<id>-answer.toml` | the Proxmox automated installer (PVE 8.2+, observed: pve.proxmox.com/wiki/Automated_Installation); range nodes then get the Ludus install (Debian 12/13 or Proxmox 8/9, observed: ludus.cloud/docs/intro) | edit the TOML | BIOS, booting the USB, running the Ludus installer |
| Fenced host units (exporter, sshd drop-in + keys, health-capture timer) | the `sysmgr-fence` dendrite (system-manager) | `generated/host-files/<id>/…` (the same unit and config files as plain text) + a system-manager profile | `system-manager switch` on each host | apply `generated/host-files/` with the plain Ansible copy playbook in `generated/ansible/` | none |
| Proxmox objects: VMs, SDN/VLANs, pools, users, PVE firewall | terranix (`render-tofu`) from `nucleus/net.nix`, `nucleus/people.nix`, host defs | `generated/tofu/*.tf.json` | `tofu apply`, bpg provider (`terraform-providers.bpg_proxmox` is in nixpkgs, observed: nixpkgs PR #565842) | edit the `.tf.json` (or convert to HCL) and use plain `tofu` | `tofu apply` for human-yes categories (an ask) |
| Ansible inventory and vars (host baseline, Windows/Linux guest config via Ludus roles) | `render-ansible` | `generated/ansible/inventory.yml`, `group_vars/` | `ansible-playbook` | edit the YAML | none |
| Range content | `ranges/<name>.nix` → `render-ludus` | `generated/ludus/<name>.yml` | `ludus range config set` + `ludus range deploy` (validated in CI) | edit the YAML | deploy/reset (automatic under a warrant) |
| NixOS guests (mgmt, Mneme, hub, agents, jump, forge, SVC, DMZ K3s) | `hosts/<guest>/` + nucleus/dendrites | a system closure (not a plain file) | Colmena; first image from nixos-generators (proxmox format) or nixos-anywhere into a blank Tofu-made VM | **no plain-file eject.** Restore from PBS and freeze, or rebuild the guest's role on Debian by following the build guide. | secrets placement, first boot |
| Phase 0 WSL | `hosts/<asset>-wsl/` | a system closure | `nixos-rebuild` inside NixOS-WSL | `wsl --import` of the exported tarball | `.wslconfig`, `wsl --import` |
| Aoide peer list and grants | `nucleus/mesh.nix` | `generated/aoide/peers.json` | the NixOS guests read it; pairing is done by an officer | edit the JSON; `aoide node allow` | pairing (an ask) |
| People (officers, members) | `nucleus/people.nix` | feeds NixOS users, eidolon profiles, Proxmox users/pools, Ludus users, WireGuard peers | the applies above | edit each generated file | none |
| Bibles (IP/VLAN, topology, inventory) | `render-bibles` (+ an export of the Mneme asset notes) | `docs/range/generated/*.md` | none (docs) | edit the markdown by hand | the hand-written bibles |
| Tooling | the devShell pins OpenTofu, Ansible, Ludus CLI, Colmena, system-manager, aoide | none | none | install the tools by hand | none |

**The rule (decided): Nix authors, plain formats run.**

- `nix build .#render` produces everything in `generated/` and `docs/range/generated/`. CI fails if the committed files differ.
- Daily operations (reset a range, deploy a comp, restore a VM) use plain tools on committed files. Only changing the design needs Nix.
- **Ejecting:** delete the render step and the CI render check. From then on, `generated/` is the source, edited by hand (runbook §21.11).

**Hypothesis:** the Ludus CLI isn't in nixpkgs. Pin it in the devShell as a fixed-output fetch of a release binary (check with `nix search nixpkgs ludus`).

### 19.2 The system-manager fence on Proxmox hosts

system-manager officially supports NixOS, Ubuntu and Debian (observed, github.com/numtide/system-manager README). Its management of systemd units and `/etc` is **reported** (user). A Proxmox VE host is Debian-based.

**Hypothesis:** it runs on PVE without `allowAnyDistro`. Check on the pilot node.

| May own | Must never own |
|---|---|
| `/etc/ssh/sshd_config.d/50-club.conf`; the club's authorized-keys file named in it | `/etc/network/interfaces`, `/etc/hosts`, `/etc/resolv.conf` |
| `prometheus-node-exporter` (or equivalent) unit + config | `/etc/pve/*` (pmxcfs), `/etc/corosync/*`, pve-firewall |
| the `club-health-capture` service + timer | apt sources for PVE/Ceph; kernel, grub, initramfs |
| `/etc/club/*` (host metadata) | any Ludus-owned path or unit (hypothesis: `/opt/ludus`, `ludus*.service`; confirm on the pilot) |
| none | users/groups beyond one `club-capture` system user |

A CI check lists every path and unit the fence renders and fails on anything outside the "may own" column (reasoned).

### 19.3 One identity source

`nucleus/people.nix` declares each person once: `{name, role = officer|member, sshKeys, forgeUser, wg?}`. That single entry renders into:

| Rendered into | Role effect |
|---|---|
| NixOS accounts on MGMT guests (per-person Unix user, per-person hub) | none |
| eidolon policy profiles | officer: answers asks. Member: human-yes tools **denied**. |
| Proxmox users and pools (terranix) | none |
| Ludus users | none |
| WireGuard peers (Phase 3) | none |
| Mneme instance groups | officer ∈ `mneme-officer` |
| forge CODEOWNERS | none |

The harness's per-member identity gap (§8.2) and the flake's multi-user accounts are therefore **one list, not two**. Aoide's own `mkHost` takes a single `username` (observed), so the range flake's guest builder takes the people list instead (reasoned).

### 19.4 Changes land as PRs

| CI job | What it catches | Runs where |
|---|---|---|
| `nix flake check` | eval errors, module type errors, statix-style lint | anywhere |
| render diff: `nix build .#render` then `git diff --exit-code generated/ docs/range/generated/` | drift, and hand edits to generated files | anywhere |
| NixOS VM tests for `mneme-*`, `hub`, `aoided` (with `tokenFile` set), in the style of Aoide's `tests/vm-boot.nix` | a guest that doesn't boot or serve | a KVM runner in MGMT |
| `tofu plan` on `generated/tofu/`, posted to the PR | what will change in Proxmox | the MGMT runner (needs API credentials) |
| Ludus validation of `generated/ludus/*.yml` | a bad range config | the MGMT runner (needs a Ludus server) |
| fence check, mesh-direction check, personal-Mneme grep | trust-rule violations | anywhere |

**Protected paths need two officer reviews** (CODEOWNERS): `modules/nucleus/` (people, mesh grants, net, policy profiles), the CI config, CODEOWNERS itself, and anything rendering firewall/SDN/egress. Agents push only to `agent/*`, and branch protection blocks self-merge.

### 19.5 Repo layout

The flake follows a hosts/nucleus/dendrites layout (the pattern documented in `Aoide/modules/README.md`, copied in, not imported):

- one `default.nix` per module directory, listing that directory's files one per line;
- flags default off;
- `_`-prefixed files are shelved.

```text
Minerva/range/
  flake.nix  flake.lock          # inputs: nixpkgs, aoide (for its NixOS modules), nixos-wsl, colmena,
                                  #   nixos-generators, terranix, system-manager, sops-nix
  lib/
    mkGuest.nix                  # NixOS guest assembly (people-aware)
    mkPveHost.nix                # evalModules for a Proxmox host → its rendered outputs, not a system
    render.nix                   # collects every host's outputs → packages.render
  hosts/
    common/                      # shared guest defaults
    _guest/  _pve/               # shelved skeletons to copy
    mgmt-hub-01/  mgmt-mneme-01/  mgmt-agent-01/  mgmt-jump-01/  mgmt-forge-01/
    svc-mneme-01/  rng-agent/     # rng-agent = template for per-range peers
    dmz-k3s-01/ …                # Phase 3
    cc-mini01-wsl/ …             # Phase 0 NixOS-WSL
    pve/cc-srv01.nix  pve/cc-srv02.nix  pve/cc-mini02.nix …   # rendered-output hosts
  modules/
    default.nix                  # imports [ ./dendrites ./facets ./nucleus ]
    nucleus/                     # range core, unconditional
      default.nix options.nix people.nix net.nix mesh.nix aoided.nix policy.nix secrets.nix
    dendrites/                   # features, each gated on range.<name>.enable
      default.nix mneme.nix hub.nix eidolon.nix jev.nix scorer.nix jump.nix forge.nix
      monitoring.nix health-capture.nix sysmgr-fence.nix k3s.nix
      render-install.nix render-tofu.nix render-ansible.nix render-ludus.nix render-bibles.nix
    facets/                      # DMZ/web only
      default.nix web-site.nix
  ranges/                        # Nix-authored range definitions → generated/ludus
    botforge.nix goad-light.nix ccdc-team.nix
  tests/                         # NixOS VM tests
    mneme.nix hub.nix aoided.nix
  generated/                     # COMMITTED, rendered; plain tools run from here
    install/<id>-answer.toml  tofu/*.tf.json  ansible/inventory.yml  ansible/group_vars/
    ludus/*.yml  aoide/peers.json  host-files/<id>/…
  k8s/                           # Flux manifests for WEB-DMZ
Minerva/docs/range/              # hand-written bibles; generated/ holds the rendered ones
```

---

## 20. Open questions (defaults in the right column)

| # | Question | Default |
|---|---|---|
| Q1 | Does the lab LAN have outbound internet in Phase 0? | No. Windows hosts keep their own campus/Wi-Fi internet for updates and the forge. The lab switch has none, and VMs have no route. |
| Q2 | Web hosting scope | Club site + member containers only. The site stays on external static hosting until G3. Internet-facing CTF is Phase 4, gated separately. |
| Q3 | Agent autonomy | The §8.1 table as written. |
| Q4 | University IT authorization | G0.5, before any vulnerable workload leaves a single isolated box. |
| Q5 | AI plane machines and OS in Phase 0 | cc-mini07/08, Windows-native Hoot (llama.cpp Vulkan). In Phase 2, Proxmox + LXC (hypothesis), with a bare-metal fallback. |
| Q6 | Cluster topology | Two domains: the R730 standalone core, and independent single-node Ludus servers. Revisit only if a comp outgrows a node. |
| Q7 | Multi-user hub | Officers only through Phase 2, with per-person accounts and hubs. Member accounts with deny-profiles in Phase 3. Hub roles later, from people.nix. |
| Q8 | Where the flake and CI live | `Minerva/range/` on the repo's existing forge. Lab-needing CI jobs run on a MGMT runner for repo-member branches only. Move to a Forgejo guest in MGMT if the forge can't be reached from the lab. |
| Q9 | Remote officer admin before a static IP | None. If needed: Tailscale SaaS on MGMT hosts only, with IT's OK. |
| Q10 | Power: region and circuits | US 120 V: two 15 A circuits or one 20 A. |
| Q11 | Two-officer wipes | Yes, as two asks in two hubs until eidolon has a native rule. |
| Q12 | Which EVO-X2 is `yomi-strix` | Found by serial in 1c; stays `on-loan` until returned. |
| Q13 | Eject threshold for agent Nix PRs | First-pass CI rate below 60 % over a term, **and** no second Nix reader. |

---

## 21. Manual runbook outline (agents down; people own the rules)

1. **Is it down?** Check the hub page, `systemctl --user status` on the mgmt VMs, the PBS job list, and a ping sweep of MGMT/OOB. Log it in the club vault by hand.
2. **Reset or rebuild a range:** from the devShell or any machine with `ludus`, run `ludus range config set -f generated/ludus/<name>.yml` then `ludus range deploy`. For a practice VM, delete the overlay and recreate it (`triage-toolkit.md` §4.6).
3. **Capture by hand:** run the §16.3 commands and paste the output into the asset note.
4. **Drift by hand:** `tofu plan` in `generated/tofu/`; `colmena apply --dry-activate`; compare against the IP/VLAN bible.
5. **Restore a VM:** in the PBS web UI, restore the latest good snapshot, boot it, and check the hub and Mneme. No Nix needed.
6. **Rebuild a node:** boot the installer USB with `generated/install/<id>-answer.toml`, run the Ludus install, `system-manager switch` (or the Ansible copy playbook), then redeploy ranges.
7. **Mesh broken:** `systemctl --user stop aoide-a2a` on every peer. Ranges still work by hand. Re-pair through Minerva later.
8. **Patch by hand:** Ansible check → apply on hosts; `colmena apply` for guests, or restore from PBS if unsure.
9. **Accounts by hand:** add the person to Proxmox (UI), Ludus (`ludus user add`) and the mgmt guest (restore-safe `useradd`), then fix people.nix later.
10. **Rotate by hand:** new Aoide tokens (`tokenFile` on each peer + `node add --token-file`); PBS and Proxmox API tokens in their UIs; revoke the Melete jump key.
11. **Ejecting from Nix:** delete `packages.render` and the render-diff CI job; add "generated/ is the source" to the build guide; from then on edit `generated/tofu`, `generated/ludus`, `generated/ansible`, `generated/install` directly with plain tools. NixOS guests are frozen and PBS-restored, or rebuilt on Debian per the build guide.
12. **Suspected compromise:**
    - *A range agent:* stop the range (`ludus range rm`), set its quarantine to read-only for review, and rotate MGMT→range tokens.
    - *MGMT or Melete:* pull the WAN or the lab uplink, revoke the jump key and all forge tokens, restore from PBS to a known date, and rotate every secret.
13. **Comp day without agents:** follow the scenario runbook. Deploy the day before, start the scoring VM, fire injects from a printed schedule.
14. **Handover each term:** the rotated-secrets list, an inventory review, a test restore, a bible read-through, and the eject-trigger check (§1).

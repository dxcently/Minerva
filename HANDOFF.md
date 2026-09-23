> **Takeover update, 2026-09-19:** The product is now **Minerva**; `hoot`
> remains its launcher/CLI. The root is Git-initialized, but has no commits
> or remote at takeover. Both nested repositories have local changes.
> GitHub creation awaits authentication. Older measurements below are historical.
> Agents are run as `ollama:deepseek-v4.1-flash` with fallback disabled and
> externally supervised deadlines; inspect their output before accepting success.

# HANDOFF — Minerva / hoot launcher

For an agent picking this up cold, working **in parallel** with others.
Written 2026-09-19. Everything below was measured on the day, not remembered.

> **If you read only one section, read [§2 Coordination](#2-coordination--read-this-before-you-edit-anything).**
> This tree has lost work today. It is recoverable knowledge, not a warning label.

---

## 1. What this is

**Minerva** is a local AI workstation. `hoot` is its launcher and CLI control. Three pieces:

| piece | what it is | where |
|---|---|---|
| **Open WebUI** | the face — the chat UI the operator actually uses | `webui/`, runs on `:3000` |
| **eidolon** | a Rust coding-agent harness, and the **sole extension host** | `eidolon/`, serves `:8085` |
| **jev** | the tools sidecar — browser automation, graph-driven navigation | `jev/`, `extensions/` |

A local llama.cpp router serves models on `:8080`. Ollama is on `:11434`.

The design direction, in one line: **Open WebUI is the face, eidolon is the
sole extension host, jev is tools, and automation is a graph/state-machine**
rather than an agent loop.

### The current arc of the work

Two operator priorities, both live:

1. **Linux, as a Nix flake.** Nothing here has ever run on Linux. The ruling is
   `docs/design/linux.md`; the line-cited blocker list is
   `docs/design/posix-inventory.md` (68 findings, 31 blockers).
2. **Live systems triage** — "*what process is causing this behaviour*", answered
   by an agent that navigates, runs commands, and traces back to a config. The
   ruling is `docs/design/triage.md`.

---

## 2. Coordination — read this before you edit anything

### The git situation is three different situations

| path | git? | state |
|---|---|---|
| `bonsai2/` (root) | **NO** | **not a repository.** No index, no history, no worktrees, **nothing to recover from** |
| `bonsai2/eidolon/` | yes | **73 uncommitted entries**, including whole build-required modules (`crates/web/`, `crates/rune/src/ext.rs`, `crates/rune/src/warrant.rs`) |
| `bonsai2/harnox/` | yes | pinned `v0.3.6` @ `858f597`. Treat as read-only unless told otherwise |

**An agent lost an entire afternoon's work in this tree today.** It mirrored
into a git worktree, worked there, and removed the worktree without copying
back. It was caught only because a *different* agent measured a test count that
disagreed with a briefing by three.

So:

- **Claim a component before you edit it.** One agent per directory. `jev/`,
  `extensions/browser/`, `extensions/jev/`, `eidolon/crates/<name>/`, `bin/`,
  `docs/` are the natural units.
- **Do not use a git worktree for `bonsai2/` work.** There is no repository.
  Work in place, or back files up by hash first.
- If you *do* use a worktree for `eidolon/`, it must be a **sibling of
  `harnox/`** — nesting it breaks the `[patch]` path dependency on `../harnox` —
  and the copy-back is six steps, not one: mirror the dirty tree in by hand
  (`crates/web` is entirely untracked, so `git status` does not describe it),
  own your `CARGO_TARGET_DIR`, diff to confirm the change is purely additive,
  **copy back**, verify with `diff -q`, re-run the checks **in `eidolon/`
  itself**, and grep the live tree for a distinctive string from your change.
  *Then* remove the worktree.

### Protected — do not modify, confirm before deleting

| what | why |
|---|---|
| `%LOCALAPPDATA%\eidolon\extensions\jev\decisions.jsonl` | the operator's production decision log. md5 `c559328a1c24bfa60cdadf19fe80b93b`, 846 bytes. **There must be no `runs/` or `decisions/` directory beside it** |
| `%APPDATA%\eidolon\config.toml` | the live config |
| `%APPDATA%\eidolon\policy.rn` | the live policy table |
| anything under `models/` | 102 GB of weights. **Confirm with the operator before deleting a model file** |
| the running PIDs in §3 | live services the operator is using |

When you run jev tests, isolate with **all three** variables — they are read
once at import, so they must be set *before* the interpreter starts:

```
JEV_DECISIONS_LOG=<scratch>/decisions.jsonl \
JEV_RUNS_DIR=<scratch>/runs \
JEV_DECISIONS_DIR=<scratch>/decisions \
  <interpreter> -m unittest discover -s tests
```

An agent that set only two wrote two rows into the production log before
noticing.

### The swarm — how a subagent is summoned, held, and addressed

*Written here, once, and deliberately not in `docs/design/shim.md`: that
doc is the shim surface's own design, and its per-chat/"Yolo" section is
being rewritten right now (S70). What an agent needs **before its first
edit** — who holds a session, where its gate and log are, how to address
it — belongs beside §2's claim rule and the `peers`/`send` instructions
above. Link here instead of restating this anywhere else.*

**A summoned subagent is a chat in *this* process, not a new process.**
Nothing in this tree summons a session into another one. The only
production `SessionFactory` is `ServeHost`
(`eidolon/crates/cli/src/serve_host.rs:483`), and its only caller is the
shim: `Shim::summon` (`eidolon/crates/web/src/shim/mod.rs:162`) opens or
creates `<sessions_dir>\chats\<chat id>.eid`, journals the `owui-chat`
note, and then calls `factory.summon`, which runs `build()`
(`eidolon/crates/cli/src/main.rs:1642`) **inside the serve process** and
returns an `Agent` with its own `Dispatcher`, its own `EventBus`, its own
swarm `Presence` and the shim's `ChatUser` as its `UserIo`
(`docs/design/shim.md:120-133`). One chat therefore shares:

- **the process's gate** — one `Dispatcher` per chat, each compiling the
  same table (`config.policy_path()`, `%APPDATA%\eidolon\policy.rn`
  here) in `build()`; there is no second policy path (AGENTS.md, "One
  chokepoint"), and a chat cannot approve on its own;
- **the process's log directory** — `chats\<id>.eid`, the same directory
  the TUI's own sessions sit directly under, so `eidolon sessions` sees
  both (`crates/web/src/lib.rs:339-342`);
- **the process's posture** — inherited at summon, below.

An `eidolon tui` or `eidolon run` session is a **different process** with
its own gate and its own log. It is a peer of a chat, never a summon of
one.

**Posture is inherited at summon time, not passed by hand.** `Summoned`'s
`yolo` (`crates/web/src/lib.rs:325-329`) is read *fresh inside*
`SessionFactory::summon` (`serve_host.rs:565-579`: `let armed =
self.yolo.armed()`, handed to `build()` and to `ChatSession::new` at
`shim/mod.rs:214`), so a chat summoned after the operator flipped the
switch is armed and one summoned before keeps what it was born with —
"read back per chat", not once per process. The flip is **not journaled
and does not survive a resume** (`crates/core/src/yolo.rs`, "Arming"):
posture is a launch/summon-time fact, not a record. So a subagent inherits
the summoning session's posture with no per-agent flag threaded by anyone.
⚠ *S70 is in flight (bonsai2-08d4): that change is what turns `yolo` into
a per-chat `Option<Switch>`; until it lands, `lib.rs:325-329` and
`docs/design/shim.md:671` ("no per-chat switch exists") still describe the
tree.*

**Two names for one session, and they are not the same string.**

- **The roster id** — what `peers`/`send` use: `<cwd basename>-<4 hex of
  the log path>` (`crates/swarm/src/presence.rs:414` `name_for`), `-1`,
  `-2` suffixed when a directory is taken (`presence.rs:134-147`). It is
  published as `meta.json` in `$XDG_RUNTIME_DIR/eidolon/<id>/` beside
  `sock` and `inbox/` (`crates/swarm/src/lib.rs:20-35`; Windows has no
  `XDG_RUNTIME_DIR`, so the root is `std::env::temp_dir()`,
  `presence.rs:110-114`). A session registers on its way up, before the
  backend or the tools exist — that registration is what decides whether
  `peers` and `send` exist at all (`main.rs:1690-1733`).
- **The chat id** — what `X-Eidolon-Chat-Id`/`X-OpenWebUI-Chat-Id`
  carried, mapped to `chats\<sanitised>.eid` by `shim::path`
  (`crates/web/src/shim/path.rs:1-70`). This is the HTTP address; **no
  swarm message ever names it**.

The bridge is `meta.json`'s own `log` field (`presence.rs:154`): the log
path is what `name_for` hashed, so the roster id and the chat id are two
readings of one session. Anything that must surface a peer message *in a
chat* rather than in a roster crosses that bridge.

**`channel` is a fan-out; a direct message wakes.** A channel send goes to
every session whose `channel_key` is this repository
(`crates/swarm/src/api.rs:117-130`, `presence.rs:194`) and does not wake by
default; a direct message wakes by default (`api.rs:100-143`). Waking is
the same in every state: mid-turn it steers at the next safe point, idle it
starts a turn. `eidolon send` is that same door **from outside any
session**, and its envelope is marked `external` so the recipient's model
is told the sender is neither the operator nor a colleague
(`crates/core/src/peer.rs:50-60`).

**The roster is machine-wide; summoning is process-local.** One root holds
every registering session on this box, so `peers` lists sessions in other
repositories and other processes too — that is what the "elsewhere" rows
mean. Nothing on the summon path reads that roster: `Shim::summon` resolves
a chat from its own map and its own `chats\<id>.eid`, so a summoned
subagent is this process's chat by construction, and no other process can
answer as it. Talking to a peer with `send` never summons anything.

**Claim before you edit** — §2's own rule above, which the swarm only
carries: claim on `channel`, one agent per directory, announce the changed
paths on the way out.

---

## 3. Running services

| port | pid (2026-09-19) | what |
|---|---|---|
| `:3000` | 8736 | Open WebUI |
| `:8085` | 31900 | `eidolon serve` — the shim, `/ext`, `/auth`, `/jev` |
| `:8080` | 8168 | llama.cpp router |
| `:11434` | 29136 | ollama (local daemon) |
| `:54051` | 528 | jev extension service |
| `:54665` | 58728 | browser extension service |

`bin/hoot.ps1` is the launcher: `up` is idempotent and skips what is already
running, `stop` takes the shim down last, `status` reports. **There is no POSIX
counterpart yet** — that is task **L1**.

### The release binary is currently stale and locked

`eidolon/target/release/eidolon.exe` is from **05:52** and predates a lot of
landed work. Rebuilding needs **three** processes stopped, not one:

- `31900` (`eidolon serve`), **plus any `eidolon run` session** — an
  `eidolon run` *is* `eidolon.exe` and holds its own image open.
- The mechanism is an **image-section mapping**, not a stray file handle.
- `eidolon/target/release/eidolon.exe` and `…/deps/eidolon.exe` are the
  **same inode** (`links=2`), so mapping either locks both, and the file the
  linker actually fails on is the `deps/` one.

---

## 4. The record — where everything already decided lives

`docs/` is the project's memory. **Read before you design; most questions have
an answer here with its reasoning attached.**

⚠ **As of 2026-09-20 this sentence used to read "sits outside every git repo",
and that is no longer true** — the bonsai2 root was initialised on 2026-09-20.
It still has **zero commits**, though, so `docs/` is tracked by nothing and a
mistake here is still unrecoverable. Treat it as unversioned until somebody
commits; see §7.2's `R10 (3)`.

| file | what it is |
|---|---|
| `docs/Hoot.md` | what the project is, shortest entry point |
| `docs/Architecture.md` | how the pieces fit |
| `docs/Decisions.md` | **5,700 lines.** Every ruling *and why*, dated. The valuable one |
| `docs/Build-Log.md` | what landed, with measurements |
| `docs/Tasklist.md` | 116 tracked items, their state and dependencies |
| `docs/design/*.md` | per-surface rulings: `shim`, `automation`, `pages`, `fallback`, `observation`, `unattended`, `judgement`, `linux`, `triage`, `posix-inventory` |

Conventions that keep it navigable:

- Internal links are `(Doc.md#anchor)` using GitHub's slug algorithm. **163
  links currently resolve; keep it that way.**
- Source citations are `` `path/file.ext:NNN` ``. **226 currently checked, 0
  past end of file.** The 7 unresolvable ones are external trees (Open WebUI's
  installed package, the `cms-agent` sibling, a cargo dep) and are fine.
- **Anchor by name, not line number**, when referring to code. Line numbers move
  under your own edit — a report is a snapshot of a tree that its own change
  then modifies.

**Path shorthand used throughout this document and the record**: a path
beginning `crates/` is relative to `eidolon/`; a bare design-doc name such as
`triage.md` is in `docs/design/`; `%APPDATA%` and `%LOCALAPPDATA%` are the
operator's Windows profile, outside every repo.

---

## 5. Conventions that cost real work when violated

**POSIX is non-negotiable.** "It needs to run on Linux as well." Thread
`windows: bool` as a **parameter**, never `cfg!(windows)` — that way both
branches run as ordinary tests on every platform. Windows must keep working;
it is the development box.

**Never run `rustfmt` or `cargo fmt` in `eidolon/`.** It is hand-formatted to
~140 columns with prose comments set narrower. A formatter run is a destructive
change to every file it touches.

**Never put a key in argv, in a script, or in a log.** `eidolon secret set NAME
< key.txt` reads stdin. `EIDOLON_WIRE_LOG` and `EIDOLON_MCP_LOG` capture wire
traffic — keys must not reach them.

**Break it to prove it.** A test that has never failed is a hypothesis. For any
protection you add or rely on: delete or invert it, confirm the *right* test
fails for the *right* reason (read the failure text, not the exit code),
restore, and **verify the restore by content** — md5 or `diff`, never by the
absence of an error.

**Verify by content, never by absence of error.** "It didn't crash" is not
evidence. Read the value back.

**Measure counts; do not inherit them.** Five different jev test baselines were
reported today and all five reporters were honest. A test count is a
measurement of a tree **and** an environment, so name the interpreter *and* the
discovery root. An absolute count should enter the record only from a
measurement you ran; a delta may be inherited only if it is itemised.

**Say what you did not check.** A partial audit described as complete is worse
than a partial audit. The best reports today all ended with what they never
opened.

### The bug shape that dominated this project's day

**Ten blind assertions** were found — an assertion satisfied by *something other
than the behaviour it names*. The catalogue:

- a presence check with no binding to its source (`contains("data-p=\"2\"")`
  passes on a substring produced by a **different** option);
- assertions on `prompt.contains("browser")` satisfied vacuously by the
  envelope's own tool list;
- assertions that fell through to a **different guard** whose refusal message
  happened to contain the substring;
- a test that checked a command printed the right **message**, never that it
  did anything;
- a fixture that never contained the field the test existed to parse;
- a bulk find-and-replace that turned a **negative proof** into one that passed
  either way;
- a regex whose capture groups were **all optional**, so it matched the real
  input and captured nothing.

**The fix is always the same: bind the value to the thing that carries it.**
Assert the exact adjacent emission, not the bare presence of a substring.

### A filter's failure mode is silence

Four separate defects today were bounds that looked correct and silently
admitted or excluded the wrong set — a too-narrow landmark predicate, a cap
racing an ambiguity check, a ref regex that dropped every absolute URL, and a
route handler that never saw a redirect's second hop. **Count what your filter
excluded and ask whether that was meant.**

### A negative result from an instrument you just built is a claim about the instrument

Three times today. The tell is reusable: **clustered failures accuse the tool,
scattered failures accuse the work.**

---

## 6. Measured baselines

Re-measure before trusting these; they are true as of 2026-09-19.

| component | tests | notes |
|---|---|---|
| `jev/` | **308**, 7 skipped | interpreter `C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv/Scripts/python.exe`, Python 3.14.7, torch 2.14.0+cpu. Skips are `JEV_TEST_OPENJEV=1`-gated (~9 GB of weights) |
| `extensions/browser/` | **136** | real headless Chromium + real loopback fixtures; one known real-Chromium flake |
| **eidolon workspace** | **1,114**, 0 failures | `cargo test --workspace`, 25 targets |
| ↳ `eidolon-tui` | 396 | |
| ↳ `eidolon-web` | 318 | 261 lib + 20 `jev` + 18 `pages` + 17 `shim` + 2 `wire` |
| ↳ `eidolon-core` | 222 | |
| ↳ `eidolon-rune` | 106 | |
| ↳ `eidolon-cli` | 78 | |

The jev and browser suites are **not** in the workspace run — they are Python
and are measured separately, with their own interpreters.

`cargo check --workspace --all-targets` is clean apart from two pre-existing
items: an unused `Tool` import at `crates/rune/src/ext.rs:669`, and a
`conch-parser` future-incompatibility notice.

---

## 7. Tasklist — done, needed, pending

**116 tracked items. 54 closed.** Full detail and dependencies in
`docs/Tasklist.md`; this is the shape.

### 7.1 Done — the load-bearing ones

| id | what landed |
|---|---|
| **A6** | **The warrant model.** The operator's word given once per run as a written envelope — graph + content hash, tool set, browser origins, literal shell commands, action count, wall clock. Confidence/trust/judge/yolo never *create* a warrant. Gate accounting went **62 → 31 → 1 warranted interactive → 0 headless** |
| **U1/U2/U4** | The warrant implemented: `eidolon/crates/rune/src/warrant.rs` (1,389 lines, 27 tests), reaching cli/tui/web; the id hash **agrees across Rust and Python**, proven by an independent derivation from the spec rather than a port |
| **S47** | **Calibration, measured.** The shipped chooser is 4.48% top-1 against 3.62% chance, ECE 0.443. Trained 12.5 min on 41,291 Wikispeedia rows → **28.4% top-1, ECE 0.039** |
| **S53** | The `section` snapshot scope. Live "See also" on Cat: **1,871 chars, 31 links**, 0.71% of cap |
| **S54** | **The chooser blocker, solved** — see §8 |
| **U3** | Origin confinement, **and a real Playwright bug**: `context.route()` does not refire for a server-side redirect's continuation, so a listed origin 302-ing to an unlisted one sailed straight through |
| **S61** | Three blind assertions found by deliberate breaking, all bound |
| **A7** | The Linux/Nix ruling — `docs/design/linux.md`, 965 lines |
| **A8** | The triage ruling — `docs/design/triage.md`, 445 lines. **Answers the warrant-vs-generated-command question** |
| **P1** | `docs/design/posix-inventory.md` — 68 findings, 31 blockers, **88/88 citations mechanically verified** |

Also solved earlier: the **wiki-hop size blocker** — 773,372 chars (295% over
the 262,144 cap) down to **4,954** via `within: "main"` + `roles: ["link"]`, at
no measurable cost.

### 7.2 Needed from the operator — blocking

| id | what |
|---|---|
| **R10 (3)** | ~~**`git init` at the bonsai2 root.**~~ **Half done, and the remaining half is the half that matters.** The root *is* a repo as of 2026-09-20 — branch `master`, a `.gitignore` covering secrets/state/weights/binaries/nested repos — but it has **zero commits and every path untracked** (`git status`: `?? docs/`, `?? bin/`, `?? HANDOFF.md`, …). A flake resolves paths through git's *index*, not the filesystem, so an all-untracked repo does not give L1 a slow flake either — it gives a flake that sees an empty tree. **`git add -A && git commit` is what actually unblocks L1**, and it is also the point at which the root finally has a recovery story; until a commit exists, `git init` has added no safety net whatsoever |
| **rebuild** | Stop `31900` **and any live `eidolon run`**, then `cargo build --release --manifest-path eidolon\Cargo.toml`, then `.\bin\hoot.ps1 up` |
| **R10 (1,2,4)** | Only needed at flake stage two: commit `eidolon`'s 73 entries; commit and tag `harnox`; credentials for the two private repos (`jevlike` is public, MIT) |
| **S2** | Confirm before deleting ~12 GB of unreferenced model files |

### 7.3 Pending — ranked, with why

**Linux / flake** — the operator's stated priority.

- **L1** — stage-one flake: root `flake.nix`, `devShells` only (`default`,
  `serve`, `train`), tree runs in place. Hard constraints: **never
  `path:./eidolon`** (a `path` fetch copies **71 GiB / 75.8 GB across 118,384
  files** *before* any filter runs); `train` and `serve` share one Python env
  so a checkpoint is always scored by the torch that trained it; no CUDA; the
  flake never sees a gigabyte. **Blocked on R10 (3).**
- **S48** — `jev/server.py:62` defaults `JEVLIKE_ROOT` into a **different
  project**. Note the inventory found a **second** site: `extensions/jev/
  extension.rn:44` hard-codes that venv's python in its command string, so
  fixing the variable alone leaves the extension launching a foreign venv.
- **S66** — `_chromium_installed()` ignores `PLAYWRIGHT_BROWSERS_PATH`, exactly
  the variable a Nix browser set uses. nixpkgs' playwright is currently the
  pinned 1.63.0, so this is the only thing in the way.
- **S13, S14, S20, S15** — empty extensions dir is silent on Linux; harnox's
  tests have never compiled on Windows (unguarded `std::os::unix`); no Linux
  `llama-server` exists in this repo; eidolon has no CI.

**The warrant, unattended.**

- **W1** — `reads: bool`, the warrant's eighth key. A8's four steps, **no
  harnox change**: harnox already composes `read_only` per stage
  (`read_only &= v.read_only`), and `Ruling` already carries `structural` across
  as precedent. A nested `bash` call is covered when string-equal to a listed
  literal **or** `reads && ruling.read_only`. Pin the distinction with a test:
  *a literal covers any shape because the operator read the string; `reads`
  covers only parsed shapes because only then is the read a checked fact.*
  **Name the disclosure hole explicitly — `cat ~/.ssh/id_rsa` is `reads: true`,
  because `read_only` was only ever a claim about mutation.**
- **W2** — `capture` and typed context. One new action (a `matches` with named
  groups → `context.case.*`), `notes` demoted to a 400-char head, chooser
  context rendered from typed keys only. Today the pid in an observed `netstat`
  line sits **past byte 32 of the option window**, so the chooser cannot see the
  thing the row is about. This is also what makes the decision log *learnable*.
- **U5** — live re-run of wiki-hop under a warrant, on a rebuilt binary.

**The chooser.**

- **S56** — promote the trained checkpoint. **Coupled to S54: they are one
  decision.** Dedupe alone leaves the shipped checkpoint choosing `crepuscular`
  at 94.8%; the checkpoint alone parks on a tie with itself. Either without the
  other looks like a fix and changes nothing. Known transfer gap: trained on
  article *titles*, it clicks a taxobox `C` at 0.53 for goal `Dog`. Validation
  NLL was **still falling** at 4 epochs (3.38 → 3.02).
- **S64** — `prefer` bypasses the chooser; decide how far that should go. A rule
  hit writes `probs` one-hot, so **every rule hit is a training row that teaches
  nothing.**
- **S55** — a checkpoint with no calibration record cannot be trusted silently.

**Correctness debt.**

- **S63** — the blind-assertion audit's unreached majority: all of
  `eidolon/crates/cli` (49 `.contains(` sites grep-counted, never read), most of
  `crates/core/tests/loop.rs` (3,547 lines) and `crates/tui/src/app.rs`, all of
  `crates/rune/src/host.rs` — all relative to `eidolon/`. Three findings came
  from a small fraction of the volume, so the prior that the rest is clean is
  weak.
- **S24, S27, S32, S39, S41, S43, S44** — see the Tasklist rows; each is
  specific and cited.

**Docs truth** — **S50, S59, S19, S9.** Each is a doc claiming something the
code does not do. Cheap, checkable, and they mislead the next reader.

**Ops** — **S65** (Open WebUI writes `.webui_secret_key` into whatever cwd it
has; **two already exist**, at `bonsai2/` and `bonsai2/eidolon/`, so which key
is live depends on where the launcher was standing), **S37, S38, S21, S22**.

- **S57** is **flagged, not open**: its premise was voided. The `%TEMP%`
  directory two agents called an orphan is the **live profile of the protected
  browser service**. Re-argue from first principles or close it.

---

## 8. The two findings most worth understanding

### The right answer was winning three times and losing because of it

On the real captured Cat page with goal `Felidae`, scored by the trained
checkpoint:

| | n | top-1 | top-2 | margin | gate |
|---|---|---|---|---|---|
| before | 57 | **Felidae 0.2455** | **Felidae 0.2455** | **0.0000** | **PARK** |
| after | 54 | **Felidae 0.4859** | Felis 0.1478 | 0.3381 | **CLICK** |

`Felidae` was listed at indices **0, 23 and 32**. The chooser was right three
times, and the copies split its confidence — so **top-1 and top-2 were the same
word**, and the margin was exactly zero.

> A margin gate cannot distinguish "I am torn between two options" from "I am
> certain, and the thing I am certain about is listed three times." Both arrive
> as a tie. Neither number was wrong — the **menu** was wrong, and both
> protections faithfully reported a property of the menu as a property of the
> model.

Note the collapsed number is **not** the sum (0.2455 × 3 = 0.7365 ≠ 0.4859).
The chooser is **re-scored against a different menu**, because the menu is part
of its input. Dedupe changes the question, it is not arithmetic on the answer.

### The `ask` tier assumes a person, and headless runs do not have one

the operator's `%APPDATA%\eidolon\policy.rn` module docs record the split: **harnox owns the whole shell
decomposition algebra** — tokenization, pipe/`&&`/`||`/`;` decomposition,
command-substitution unwrapping, redirect scoping, and the rule that *a
composite is only ever as permissive as its most restrictive part*. The
operator's leaf table sees only already-decomposed simple commands.

Then:

> Everything the *algebra* refuses for want of understanding (a subshell, a
> `for` loop, an unparseable line, a path outside the working directory) becomes
> an `ask` instead, not a refusal — **there is a person here**, and "I cannot
> take this apart, do you want it?" is a fair question where "forbidden" is not.

**"There is a person here" is a premise, and headless operation falsifies it.**
Unattended, `ask` means *no* — and it means no to exactly the category the
design went out of its way not to forbid. Measured: pipes, `;`/`&&`/`||` chains
and one-level `$(…)` of a read into a read **all pass**; `for`/`while`/`if`/
`( )`/`{ }` are refused; `pid=$(…); ls /proc/$pid` loses the read verdict.

A8's ruling on this is the elegant part: **those refused shapes are control
flow, and control flow is the graph.** A `lines` menu *is* the `for`; an
`exists` guard *is* the `if`; a `capture` *is* the variable. The loop is not
lost — it moves somewhere the harness can inspect, log and escalate from.

---

## 9. Working here without this machine

Much of the open work does not need the box:

- **Doable from the files alone** — S50, S59, S19, S9 (doc truth: read the doc,
  read the code it cites, report the gap); S63 (read tests and *reason* about
  which assertions could pass for the wrong reason — but mark those as
  **hypotheses**, since only a break-test makes one a finding); design review of
  `docs/design/`’s `linux.md`, `triage.md`, `judgement.md`, `unattended.md`; L1's flake authored
  as a proposal.
- **Needs the box** — anything with a test count, any break-test, any live run,
  anything touching the services in §3.

**Label everything.** The convention in this project, arrived at independently
by several agents, is to mark each claim **observed / reasoned / hypothesis**,
and to say which specific sub-check was done differently from the rest. It costs
a sentence and it is the difference between a report and a claim.

---

## 10. If you change something, record it

Same session, not retrospectively (task **S5**).

- A **decision** and its reasoning → `docs/Decisions.md`, under a dated `##`
  heading.
- What **landed**, with measurements → `docs/Build-Log.md`.
- The task's **state** → `docs/Tasklist.md`.

Write the entry **after** the measurement returns, not in parallel with it — a
check whose result lands after the thing it was checking is not a check.

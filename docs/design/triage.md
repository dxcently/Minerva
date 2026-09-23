# Triage: what live-systems triage takes from jev's graph automation

**Task A8. Written 2026-09-19 on the Windows box (`C:\Users\dxcen\Projects\bonsai2`), against the tree as it stood at 14:10 EDT.** No implementation code was written. No live triage run was made against this machine (four agents live; `eidolon serve` on `:8085`, Open WebUI on `:3000`, jev on `:54051`, `browser` on `:54665`). Every measurement below is a read-only command, and each is named where it is used.

**How to read this.** Every claim carries one of three labels. **observed** — I ran a command or read the file and it says so; the command, or the file and symbol, is named. **reasoned** — follows from observed facts by an argument written out here. **hypothesis** — a design belief with no measurement behind it yet; it names what would measure it. One further label, **reported**, marks evidence the coordinator measured today and sent mid-task; I did not reproduce it. Each ruling ends with a confidence stated separately from the ruling, so a reader can disagree with the number without re-litigating the argument. Predecessors this builds on, not around: `docs/design/automation.md` (A4), `docs/design/observation.md` (A5, `expect`), `docs/design/unattended.md` (A6, the warrant), `docs/design/judgement.md` (S47, the chooser is not calibrated; `choose.prefer`). `docs/design/linux.md` (A7) is in flight in parallel and is handed the Linux-side measurements this document could not take.

---

## 0. The question, and the answer in one paragraph

The operator's words: *"navigate through dir to find errors in code or systems ... given the error and problem it can go and find the related configuration to it by using commands to trace back things ... 'whats the process thats causing this x issue' ... find which process is causing that behavior w various triage techniques."* A symptom goes in; the graph traces backwards through sockets, processes, command lines, directories and configs to a cause.

The answer: **triage is a chain of observations, not a search over a menu, and the graph as it stands can hold the menu but not the chain.** Four things are missing, one is a trap, and one wall stands in front of all of them. Missing: (1) a warrant clause for a command whose arguments come from evidence found at step 4 — ruled below as `reads`, an eighth envelope key that covers any `bash` command the deployed policy table classifies read-only, a verdict harnox already composes per stage and already tests; (2) a way to pull a typed fact out of an observation — ruled as one `capture` action (a `matches` with named groups), so the context carries `case.pid`, not a 512 KB note; (3) a termination rule that separates a **mechanical** question (what holds port 8085 — has an oracle, terminates when typed keys exist) from an **interpretive** one (which process *causes* the behaviour — has no oracle, terminates only by presenting the case and asking); (4) technique menus that exist per platform — this box has none of `ss`, `lsof`, `journalctl`, `systemctl`, and its `ps` refuses `-eo`. The trap: `cd` is not a graph action — every `bash` call is a fresh `bash -c` in the harness's cwd, and a `cd` chain also loses the read-only verdict, so "navigating a directory" is a path carried in the context and rendered into absolute-path arguments, never a change of state. The wall: the gate's structural tier turns every shape the shell algebra cannot take apart — loops, conditionals, subshells, brace groups — into an `ask`, and headless an `ask` is a no; the ruling (section 2.6) is that those shapes are control flow, control flow is the graph's job, and a graph that keeps its chain in its steps rather than in its command strings pays that wall nothing.

---

## 1. What was measured

All on this box, all read-only, all in the session that wrote this document. `$APPDATA` is `C:\Users\dxcen\AppData\Roaming`; the release binary is `C:\Users\dxcen\Projects\bonsai2\eidolon\target\release\eidolon.exe` (`eidolon 0.1.0`, mtime 2026-09-19 05:52 — the binary serving `:8085` as pid 31900, observed via `Get-CimInstance Win32_Process -Filter "Name='eidolon.exe'"`).

| # | Command | Why |
|---|---|---|
| M1 | `./eidolon/target/release/eidolon.exe policy '<cmd>'` for 38 commands (sections 2.4 and 6.1) | how the deployed classifier tiers each technique and whether it calls it read-only. `eidolon policy` is "the same path a real call takes" (`ScriptPolicy::explain`, `eidolon/crates/rune/src/policy.rs`) |
| M2 | the same with `--config <scratch>/config.toml` beside a copy of the shipped `policy.rn` | whether the running binary honours `--config` for the table. It does not: both runs print `table: C:\Users\dxcen\AppData\Roaming\eidolon\policy.rn`. Consistent with S51/S32 (Decisions, "Every gate reduction shipped this week is inert on this box"); the source now carries `policy_path_with_an_explicit_config_moves_beside_it` (`eidolon/crates/cli/src/config.rs`), which this binary predates |
| M3 | `diff -u "$APPDATA/eidolon/policy.rn" eidolon/crates/rune/policy.rn` | 69 changed lines in 3 hunks, all tool rows (`browser_open`/`browser_snapshot`/`browser_read`/`browser_back`, `jev_runs`, the trusted-extension read-only sentinel). **The shell leaf table is identical** in the deployed (md5 `3fcd0e7e…`, 2026-09-18 05:07) and shipped (md5 `39b6bc3b…`, 2026-09-19 11:32) copies, so M1's verdicts hold under both |
| M4 | `netstat -ano \| grep LISTENING \| grep ":8085 "`, and `":54051 "` | port to pid |
| M5 | `powershell -NoProfile -Command "Get-CimInstance Win32_Process -Filter 'ProcessId=31900' \| Select-Object ProcessId,ParentProcessId,ExecutablePath,CommandLine \| Format-List"`, the same for 528, then `(Get-CimInstance Win32_Process -Filter 'ProcessId=31900').CommandLine` | pid to command line; the second form because the first wraps |
| M6 | `ls "$APPDATA/eidolon"`; `find "$APPDATA" -maxdepth 2 -name config.toml -path "*eidolon*"`; `sed 's/token *=.*/token = <redacted>/' "$APPDATA/eidolon/config.toml"` | where the process's config is and what it says |
| M7 | `ps -eo comm,pid,user,pcpu,etime --sort=-pcpu \| head -30`; `ps \| head -3`; `command -v lsof ss tasklist wmic pwsh` | what the shipped triage graph's commands do here |
| M8 | `cat "$LOCALAPPDATA/eidolon/extensions/jev/decisions.jsonl"`; `ls` of `decisions/` and `runs/` beside it | the corpus on this box |
| M9 | `md5sum` and `ls -la --time-style=full-iso` on both `policy.rn` copies and both binaries; `grep` of `config.toml` stanzas | staleness; whether a standing warrant is configured (it is not: `extensions_dir` and two `[[extensions]]`, no `[policy]`, no `[[warrants]]`) |
| M10 | `Get-CimInstance Win32_Process \| Where-Object { $_.ParentProcessId -eq 31900 }` | whether `bash` children of the serve process persist (one child, `conhost.exe`; no `bash.exe`) |
| M11 | `eidolon policy` on 15 compound shapes (section 2.6) | which shapes the algebra takes apart and which it refuses — the coordinator's second wall |
| M12 | `C:\Users\dxcen\AppData\Local\Programs\Python\Python314\python.exe regex_check.py` (scratchpad; `re.MULTILINE`, 19 checks) | every `expect` and `capture` pattern this document proposes, run against the verbatim observations of M4–M7. **It caught one silent defect in my own first draft** — a fully-optional capture pattern that "matched" the command line while capturing nothing — which became the empty-match lint in 3.2 and spec step 5. The record's rule that a design regex must be tested earned its keep again. |

Reads of source, all by name: `warrant.rs`, `policy.rs` (rune and core), `harnox/src/policy/{mod,shell,tests}.rs`, `shell.rs`, `host.rs`, `dispatch.rs`, `config.rs`, `policy.rn` (all 588 lines, and the deployed copy's module doc), `bash.rn`, `run.rn`, `graph.py`, `run.py`, `options.py`, `guards.py`, `decisions.py`, `template.py`, `a11y.py`, `warrant.py`, `server.py`, `test_run.py`, `test_warrant.py`, both shipped graphs. What was **not** measured is in section 10.

---

## 2. Ruling 1 — the warrant for a command that did not exist at authoring time

### 2.1 What exists (observed)

- The warrant envelope is seven keys, `{graph, sha256, tools, origins, commands, actions, wall_s}` (`KEYS` in `eidolon/crates/rune/src/warrant.rs` and in `jev/automation/warrant.py`); an extra key is refused by name and a missing key is refused by name (tests `an_envelope_with_an_extra_key_is_refused_naming_it`, `a_missing_key_is_refused_naming_it`; `jev/tests/test_warrant.py::test_each_of_the_seven_fields_alone_causes_refusal`). The id is `w_` plus twelve hex of the SHA-256 of the canonical JSON, pinned for the shipped wiki-hop block on both sides (`the_shipped_wiki_hop_envelope_has_the_pinned_id`; `test_the_shipped_wiki_hop_id_is_the_pinned_literal`).
- A nested `bash` call under a live warrant is covered only by string equality: `Some(cmd) if env.commands.contains(cmd) => None` in `Warrant::pre_tool_nested`, tested by `a_bash_command_not_listed_in_the_warrant_asks`. `commands` is a `BTreeSet<String>`.
- The jev lint agrees from the other side: `_lint_warrant` (`jev/automation/graph.py`) collects only commands with no `{{` in them as `literal_commands` and refuses any listed command that is not one of those ("is not a command this graph can dispatch"). A templated `input.command` is simply not covered, and asks, as `unattended.md` says.
- The hook stack, outermost to innermost, is Judge → Yolo → Warrant → `ScriptPolicy` (`eidolon/crates/cli/src/main.rs`, the block that builds `Warrant::new(policy, …)`, then `Yolo::new`, then `Judge::new`). The warrant hook therefore holds the classifier's `Ruling` for every call before it decides anything, and it only ever rewrites an `Ask` (`pre_tool_nested` returns any non-`Ask` ruling untouched; `a_warrant_never_reaches_a_refusal`).
- `Ruling` (`eidolon/crates/core/src/policy.rs`) carries `verdict, reason, structural, judged, yolo, warrant`. It does **not** carry `read_only`. Its `structural` field is documented as "Carried across from `harnox::policy::Verdict::structural`" — a classification fact copied into the ruling for a downstream consumer. That is the precedent this ruling follows.
- harnox's `Verdict` (`harnox/src/policy/mod.rs`) is `{decision, reason, read_only, structural}`, and its `read_only` is **composed, not declared, for any composite**: `read_only &= v.read_only` per stage of a pipeline or chain (`harnox/src/policy/shell.rs`); a file redirect makes a command not read-only; a redirect outside the workspace is refused as `structural`. Tests: `read_only_survives_only_when_every_stage_is_a_read` (`ls | grep x` true, `ls && git commit` false), `a_redirect_out_of_the_workspace_is_refused`, `a_substituted_redirect_target_is_never_trusted`.
- The leaf is declared per program in `policy.rn` as the third element of `v(decision, reason, read_only)`: `ls cat head tail wc grep rg find stat readlink file ps pgrep pidof lsof ss netstat ip dig host nslookup uname hostname whoami id uptime df du free env printenv which …` are `true`; `systemctl` with `status/show/cat/is-active/…` is `true`, any other subcommand `false` and `FLAG`; `journalctl` without vacuum/rotate/flush is `true`; `curl`/`wget` without `-o/-O` are `true` ("fetches a URL"); `kill/pkill/killall` are `ALLOW` and `false` ("signals a process"); `sh/bash/python/node -c/-e` are `FLAG`; an unrecognized program takes the posture (`ALLOW` on this box; `FLAG` under `[policy] default = "flag"`) with `read_only: false`.
- `ScriptPolicy` exposes `explain(&self, command, cwd) -> Tier` and `eidolon policy '<cmd>'` prints it, including `reads: true|false`. That is what M1 and M11 ran.

### 2.2 The three readings, tested

**(i) Triage cannot run unattended.** True today (observed: the lint and the hook together leave a templated command uncovered; this box has no `[[warrants]]` either). As a *ruling* it would mean the product is "an operator watching a graph type commands", which J1 already measured: 7 steps, 2 escalations, 60.27 s for three menus. Rejected as the end state; accepted as the state of the tree.

**(ii) The warrant means something different for shell than for browsing.** Half right. For the browser the warrant already covers a *class* — any `browser_click` on any ref inside `origins` — not a list of refs. `commands` is the odd one out: the only clause that enumerates instances. So the sibling of `origins` for shell is not "a different meaning of warrant" but "the same meaning — a class — that shell has not been given yet". What the class is, is the question.

**(iii) A third constraint.** Three candidates were named in the brief.
- *A grammar.* Rejected: a grammar over command strings is a second parser beside harnox's, and the record already has a ruling regex that silently dropped every absolute link (Decisions, "The ruling's own regex silently dropped every absolute link"). Two parsers of the same string disagree eventually, and the disagreement is silent.
- *An allowlist of verbs with free arguments.* It is what a human operator would write ("you may run `ss`, `lsof`, `cat`, `grep`, with any arguments"). But the hook cannot check it cheaply: it sees the command string and the `Ruling`, and harnox's decomposition into programs happens inside `classify_command` and is not returned (`Verdict` carries no program list; observed). Checking verbs textually on the raw string is a second parser again. It is the *narrowing* to add on top of the third candidate once harnox returns the programs it saw; it is not the first step.
- *A read-only guarantee that is checkable rather than declared.* This exists, is composed per stage rather than trusted per command, and is tested (2.1). It is "declared at the leaf, checked at the shape": `cat` is a read because the table says so; `ls && git commit` is not a read because harnox saw the `git commit`; `ps aux | grep python > /tmp/x.txt` is refused because harnox saw the redirect escape the workspace (observed: `Deny (shape, not merits) — redirection escapes workspace`). Fully checkable would be an OS-level guarantee (a read-only mount namespace, no network namespace) — Linux can do that and Windows cannot; that is A7's to measure (hypothesis, section 10).

### 2.3 The ruling

**Add an eighth envelope key, `reads: bool`.** A `bash` command under a live warrant is covered when **either** it is string-equal to an entry of `commands` (as today) **or** `reads` is true and the classifier's verdict for that exact command, in the harness's cwd, was `Allow` with `read_only == true`. Nothing else changes: a merits `Deny` still stands, `actions` and `wall_s` still bound the run, `tools` must still name `bash`.

What the operator reads when they sign it: *"this graph may run any command the policy table calls a read, with arguments it finds as it goes, up to N actions in T seconds; and these literal commands besides."* That is a promise about a class, exactly as `origins` is, enforced by the same table that decides what asks in an attended session. The table is already the operator's word about their own box, so the warrant need not pin its hash (reasoned; the stale-table problem on this box, M2/M3, is a deployment defect owned by S51 and orthogonal to this ruling).

The mechanism, in the order the pieces already exist (spec steps 1–4, section 9):

1. `Ruling` gains `read_only: bool`, set by `ScriptPolicy::pre_tool` from `Tier.read_only`, the way `structural` already is. A hook with no classifier (`AllowAll`) sets `false`.
2. `Envelope` gains `reads`; `KEYS` becomes eight on both sides; `canonical_json` includes it; the two pinned ids are re-pinned once; both shipped graphs get the key (`false` for wiki-hop, `true` for triage) so their blocks stay complete.
3. `Warrant::pre_tool_nested`'s `bash` arm gains the second cover: `Some(_) if env.reads && ruling.read_only => None`.
4. `_lint_warrant` admits a templated `bash` command **iff** `reads` is true; otherwise the existing "not enumerable at authoring time" error stands, reworded to say what fixes it.

Why not a reserved entry inside `commands` (a magic `"read-only"` string): a list of literals that also contains one word of a different type is the kind of thing a later command named `read-only` matches silently. Why not `reads` as a verb list now: 2.2(iii) — the hook cannot check verbs without a second parser; a verb list is the narrowing to add when harnox returns programs.

### 2.4 What `reads` does not bound (observed, `eidolon policy` on this box)

A read-only clause bounds **mutation of this machine**. It does not bound three other things, and the warrant's summary text must say so:

| Command | Verdict | What it means for `reads` |
|---|---|---|
| `curl -s http://127.0.0.1:8085/health` | Allow, "fetches a URL", reads **true** | a GET to a local service is covered — legitimate ("is it answering?") |
| `curl -s -X POST -d "x=1" http://127.0.0.1:8085/x` | Allow, "fetches a URL", reads **true** | **a POST is also covered.** The table's `curl` row looks only for `-o/-O/--output/--remote-name`. `reads` does not bound network side effects. |
| `ssh box ls` | Flag, "reaches another host", reads false | not covered (asks) |
| `cat ~/.ssh/id_rsa`; `cat $APPDATA/eidolon/serve.token` | Allow, "read", reads **true** | **disclosure is not mutation.** A read-only run can put a secret into `context.notes`, the escalation payload, the report and the decision log. |
| `sleep 100000` | Allow, "read", reads true | time is bounded by `timeout_s` and `wall_s`, not by `reads` |
| `kill 8168`; `systemctl restart nginx` | Allow "signals a process" / Flag "changes a service", reads false | not covered — correct |
| `cd /etc/nginx && ls`; `cd /tmp` | Allow, "safe command chain" / "unrecognized command", reads **false** | `cd` is unrecognized and so not a read; a chain with `cd` in it is never covered. Section 5. |
| `nft list ruleset 2>/dev/null \|\| iptables -S` | Allow, "safe command chain", reads **false** | the shipped graph's "firewall rules" technique is not a read by the table; it is covered today only because it is listed literally |
| `powershell -NoProfile -Command "Stop-Process -Id 1"` | Allow, "unrecognized command", reads false | not covered; but it **runs silently** on this box's default-allow posture — `powershell -Command` is `sh -c` and is not in the table's `-c` FLAG list. Not this document's to fix; named for the record. |
| `tasklist /FI "PID eq 31900" /V /FO LIST` | Allow, "unrecognized command", reads false | not covered. Windows' native process listing has no table row. |
| `ps aux \| grep python > /tmp/x.txt` | Deny (shape), "redirection escapes workspace" | asks — correct |

Closures, cheapest first. **Disclosure** — the case (section 3) carries captured fields, not whole outputs, and the report renders the case, so a secret reaches the log only if an author captures it; the shipped `push notes "{{context.obs.text}}"` is the disclosure path and is demoted in section 3. **Network** — run warranted triage under the sandbox posture `automation.md` §6 already specifies (a box with nothing to reach, or A7's Linux namespace); a `reaches: bool` beside `read_only` in harnox's `Verdict` is the later sibling if an operator wants it in the envelope (hypothesis: one row per network program in `policy.rn`, one `&=` in `shell.rs`; not measured). **Time** — already bounded.

### 2.5 Confidence

That the third way is the right answer and that `reads` is its smallest honest form: **0.85**. That the four-step mechanism lands in one Sonnet day without touching harnox: **0.8** — the one risk is the number of `Ruling { … }` literal constructions the new field breaks (the compiler enumerates them; I did not count them). That the disclosure and network holes are as stated: **0.9** (observed verdicts). That the sandbox is a sufficient closure for network: **0.5** (hypothesis; depends on A7's box).

### 2.6 The second wall: shapes the algebra refuses

**Reported (coordinator, 2026-09-19).** A headless executor lane (`eidolon run -m ollama:deepseek-v4.1-flash`, gate armed, no `--yolo`) ran two multi-step investigations; each met one structural refusal — "compound command blocked", "unsafe command substitution blocked" — and headless, the resulting `ask` was auto-declined. One decline per investigation; both agents adapted and finished. The mechanism is the gate's own design: `policy.rn`'s module doc (deployed copy, lines 3–28, read) says everything the algebra refuses for want of understanding "becomes an `ask` instead, not a refusal — there is a person here". Headless falsifies the premise, and the tier built to be merciful becomes the harshest. The coordinator's point stands on its own: a warrant that solved literal matching would still meet this wall, because triage commands are compound by nature.

**Observed (M11), which shapes:**

| Shape | Verdict | reads |
|---|---|---|
| `cat /proc/$(pgrep -o nginx)/cmdline` | Allow, read | **true** |
| `readlink /proc/$(pgrep -o nginx)/cwd` | Allow, read | **true** |
| `grep -l 8080 $(find /etc -name "*.conf")` | Allow, read | **true** |
| `ps -o pid,cmd -p $(pgrep -d, python)` | Allow, read | **true** |
| `` echo `date` `` | Allow, read | true |
| `ls; ps` | Allow, safe command sequence | true |
| `ss -tulpn 2>/dev/null \|\| netstat -tulpn` | Allow, safe command chain | true |
| `pid=$(pgrep -o nginx); ls /proc/$pid` | Allow, safe command sequence | **false** (a bare assignment is not a read) |
| `for p in $(pgrep python); do cat /proc/$p/cmdline; echo; done` | Deny (shape), compound command blocked | false → asks |
| `ls /proc \| while read p; do cat /proc/$p/comm; done` | Deny (shape), loop condition cannot terminate | false → asks |
| `if [ -f /etc/nginx/nginx.conf ]; then cat /etc/nginx/nginx.conf; fi` | Deny (shape), compound command blocked | false → asks |
| `(cd /etc && ls)` | Deny (shape), compound command blocked | false → asks |
| `{ ls /etc; ps; }` | Deny (shape), compound command blocked | false → asks |

So the wall is narrower than "compound": **pipes, `;`/`&&`/`||` chains, and one level of substitution of a simple read into a read all pass as reads.** What the algebra refuses is **control flow** — loops, conditionals, subshells, brace groups — and a variable carried between commands loses the read verdict. "Unsafe command substitution" is a nested or non-simple substitution (`harnox/src/policy/shell.rs`, the substitution rule: "simple command with no redirects and no *further* nested substitution"); the coordinator's model wrote one the algebra could not reduce.

**The ruling, against the three options offered.** *The algebra learns more shapes* — harnox work; each shape taught also widens the attended gate, and the loop-liveness guard exists precisely because `while read` over a pipe has no termination the algebra can see; slow, and the wrong layer for triage's need. *The warrant covers shapes* — a grammar over shapes is 2.2(iii)'s second parser; rejected for the same reason. *Restrict unattended triage to simple commands and accept the technique cost* — **this, with one sharpening that makes the cost near zero for a graph:**

**The graph is the chain; the shell is the observation.** The shapes the algebra refuses are exactly the ones a chat model needs because it has one turn per command: a `for` over pids, an `if` on a file's existence, a variable carried from one command to the next. A graph has those already — a `lines` menu over `pgrep -a` output with `exclude: tried` and a visits budget *is* the `for`; an `exists` guard on a captured path *is* the `if`; `capture` and render *is* the variable (section 3). Each iteration becomes its own step: simple, read-only, `expect`-guarded, observed whole, and logged as its own row. The coordinator's one-decline-per-investigation is the cost of putting the chain in the shell; a graph pays it in steps, and steps are the corpus (section 7). What is genuinely lost: a loop over N pids is N steps against `budget.steps`/`actions` (40 in the shipped graph) and N `bash` spawns instead of one; and a technique an author truly needs in a refused shape cannot be covered by `reads` at all.

For that last case the escape is already in the envelope, and the asymmetry is correct: **a literal in `commands` covers any shape, because the operator read the exact string; `reads` covers only shapes the algebra took apart, because only then is "read-only" a checked fact.** Reasoned from the hook: `pre_tool_nested` rewrites an `Ask` whose command is string-equal to a listed literal without looking at `structural`; a structural refusal reaches it as an `Ask` (observed: `eidolon policy` prints `Deny (shape, not merits) … harness: asks you first`; `Ruling.structural`'s doc says the same). Spec step 3 adds the test that pins this, `a_listed_literal_compound_command_is_warranted_despite_its_shape`, because a reasoned claim about a hook is exactly the kind the record says to test.

One consequence for section 8's example, worth stating: on Linux `cat /proc/$(pgrep -o eidolon)/cmdline` would fold steps 1 and 2 into one covered command. The graph should **not** do that — the substitution's result never becomes a typed key, so the pid would be invisible to the case, the report and the corpus. A substitution hides evidence; `capture` makes it visible. The gate and the design point the same way.

The coordinator's other datum — a cheap model, asked only "what is holding this binary open", found three processes, distinguished an image-section mapping from a lock by matching sizes, traced both parent chains, separated the long-lived process by age, and named an MSYS2 path-mangling as a harness artifact rather than evidence — is **reported**, and it bears on section 4: the interpretive tail may be better served by a model in the loop (the Judge's seam) than by the 41,280-parameter chooser. Hypothesis; it does not change the ruling that the tail parks rather than concludes, because that model's confidence is no more calibrated on this task than the chooser's is.

Confidence that this wall is control flow only, as measured: **0.9** (observed on the deployed table, identical to shipped for shell). That the graph-is-the-chain sharpening removes most of the technique tax: **0.75** (reasoned; the measurement is the step count of `triage-port` against J1's).

---

## 3. Ruling 2 — what the context carries

### 3.1 What exists (observed)

- The shipped graph accumulates evidence as free text: `push notes "{{event.option.label}}: {{context.obs.text}}"` after every command (`extensions/jev/graphs/triage-linux.json`, both `PICK` handlers and `processes.entry`). A `bash` observation has no head block, so `build_obs` (`jev/automation/a11y.py`) gives `text` only — `url/title/h1/chars` are `None`, `is_truncated` is `False` ("not proven whole, but not provably not"). The producer cap is `MAX_OUTPUT = 512 * 1024` (`eidolon/crates/tools/src/shell.rs`), so one note can be half a megabyte.
- Templates have no logic and cannot slice (`jev/automation/template.py`: `render`, `render_value`; a list renders newline-joined, an object as JSON). `matches` tests but does not capture (`jev/automation/guards.py`). `automation.md` says so under "Extraction": openjev checks a claim, it does not pull a hostname out of a paragraph.
- The chooser sees `CONTEXT_TOKENS = 192` bytes of context and `OPTION_TOKENS = 32` bytes of each option (`jev/server.py`). S47 measured that no option label reaches the context window. A `lines` option's data is `{line, index}` (`options.py::_from_lines`) — no fields to reorder with `choose.label`.
- The observed netstat line is 78 characters: `  TCP    127.0.0.1:8085         0.0.0.0:0              LISTENING       31900`. Its first 32 bytes are `  TCP    127.0.0.1:8085         ` — the port is inside the option window, the pid is not (reasoned from the observed line and the observed constant).

### 3.2 The ruling

**Evidence is typed at the point it is extracted, and extraction is one new action, `capture`.** `{"type": "capture", "params": {"path": "context.obs.text", "pattern": "(?m)^\\s*TCP\\s+(?P<addr>\\S+):8085\\s+\\S+\\s+LISTENING\\s+(?P<pid>\\d+)", "into": "case"}}` runs `re.search` with `MULTILINE` — the engine and flags `matches` already uses — and assigns each participating named group to `context.<into>.<group>`. A group that did not participate assigns nothing (so an optional flag is `(?:--config (?P<config>\S+))?`, and its absence is a finding, not a failure). **The pattern as a whole not matching raises `ERROR` with `reason: "unexpected"` and `tool: "capture"`** — the belief `expect` guards ("this observation is of the kind I believe"), reached one step later; not a fourth reason. **A capture pattern that can match the empty string is a lint error**, because `re.search` then "succeeds" at offset 0 with every group `None` and the failure is silent — observed (M12): my first draft of section 8's step 2 capture, `(?:--cwd\s+(?P<cwd>\S+))?(?:.*--config\s+(?P<config>\S+))?`, matched the real command line and captured nothing; the anchored form `--cwd\s+(?P<cwd>\S+)(?:.*--config\s+(?P<config>\S+))?` captures `cwd` and leaves `config` unset. The lint is `re.search(pattern, "") is not None` → "a capture that can match nothing captures nothing silently; anchor it on a required group". `capture` may read any scope path, including `event.option.line` after a `lines` pick, so the division of labour is: **the chooser selects (which line), the author's pattern extracts (which field).**

The context then has two parts with different jobs:

- `context.case` — typed keys, small, the only thing guards read and the only thing later commands render: `case.port`, `case.pid`, `case.addr`, `case.exe`, `case.cmdline`, `case.cwd`, `case.config`, `case.dir`. This is what "later steps use earlier findings" means concretely: `{{context.case.pid}}` inside the next command — and, per 2.6, it is the graph's replacement for a shell variable or a substitution.
- `context.notes` — demoted to report material. The shipped `push notes "{{context.obs.text}}"` is replaced by a `capture` of a bounded head, `(?s)^(?P<head>.{0,400})` into `notes_last`, then a push of that. (A capture is how a template slices without a slicing template; sibling, not widening.)

**The chooser's context is rendered from the case, not from the notes.** `"port {{context.case.port}} held by pid {{context.case.pid}}; which technique next?"` is under 192 bytes with room for the state description; a notes blob is not, and S47's finding becomes a rule: *a step's context template may only name typed keys and the state description.* `graph.py` can lint this (a `choose.context` under a graph that declares `case` may not reference `context.notes` or `context.obs.text`) — spec step 6, optional.

**Format at the source.** Because an option is 32 bytes, the command's column order is the label's byte order: `ps -o pid,comm` puts the pid inside the window, `ps -o comm,pid` the name. The shipped graph already does this deliberately with `--sort=-pcpu`; the rule is now stated: *for a `lines` menu, the discriminating field is the first column.*

### 3.3 Confidence

That typed evidence is required and that free-text notes cannot carry a chain: **0.9** (observed constants and the observed 78-byte line settle it). That `capture` as a `matches`-with-groups is the smallest honest primitive and does not widen the grammar: **0.75** — the alternative, putting `capture` on the `expect` key of a `tool` action, is smaller still but cannot read `event.option.line`. That "no match is `unexpected`" is the right reason: **0.6**; an implementer who finds it confusing in the log should raise it rather than add a reason.

---

## 4. Ruling 3 — how it knows it is done

### 4.1 What exists (observed)

- There is no goal string and no equals-guard target for triage; the closed guard set is `equals contains matches exists count entails contradicts not and or`. Termination in the shipped graph is: a `count` on `tried` (technique exhaustion, three per menu), an `entails` on the notes ("the OS, version and hostname are all stated"), and the `judge` state, whose `PICK` goes straight to `done` with the pick pushed to `flagged` under `floor: 0.6`.
- The chooser is not calibrated (S47: 4.48% vs 3.62% chance; the floor passes 61.7% at 4.9% accuracy). The record has two wrong confident answers today: a `ps` usage error scored 0.995 as a rogue process (Decisions, "An observation that is not what the node believes it is"), and a day of naming the wrong process in the jev/browser service chains (Decisions, "I have been naming the wrong process all day").
- S47 ruled "found it" is an author-written rule. `choose.prefer` is landed and costs nothing.

### 4.2 The ruling

**Triage does not terminate by producing an answer; it terminates by producing a case, and there are two kinds of case.**

**Mechanical.** The question has an oracle: *what holds port 8085* is answered when `case.pid` exists and was read off a socket listing; *what is that pid* when `case.cmdline` exists and was read off the process table; *which config* when `case.config` names a file that `ls` showed. Each link is an observation, guarded by `expect` for its kind and by `capture` for its fields. The termination guard is `{"type": "and", "params": {"guards": [{"type": "exists", "params": {"path": "context.case.pid"}}, {"type": "exists", "params": {"path": "context.case.cmdline"}}, {"type": "exists", "params": {"path": "context.case.config"}}]}}` — checkable, because every key was set by a pattern matching a real line, never by a pick. A mechanical case may run unattended to `report`.

**Interpretive.** The question has no oracle: *which process causes the slowness*, *does anything look out of place*. The graph can assemble the case (the listing, the top consumers, the unusual parent), but **the last link is a judgement, and an uncalibrated chooser's judgement is the two wrong confident answers above.** The ruling: an interpretive graph never reaches `final` through a `PICK`. Its `judge` state's `PICK` targets a `confirm` state whose `meta.ask` presents the case and the candidate and parks; the operator's `answer` is the label (`source: human` on the row), and only that answer pushes `flagged`. With a standing warrant and `NoUser`, the run ends declined with the case in the report — the correct unattended outcome for an interpretive question: *here is what I found; I will not name a culprit.* The shipped `judge → done` edge changes accordingly (spec step 7). The `CLEAN` escape stays a `prefer`-able structural option.

**Exhaustion** is the third exit and is already built: `count tried gte N` per node, `visits`, `escalations`, `steps`, `wall_s`. A run that exhausts techniques reports the partial case, labelled partial. A `final` whose output is `{case, notes}` is the report shape; `flagged` appears only when a human or a checkable guard put it there.

### 4.3 Confidence

That the mechanical/interpretive split is the right line and that interpretive questions must not terminate on a pick: **0.85**. That `confirm` parks rather than `done` recording `verified: null`: **0.6** — the alternative (record the pick, mark it unverified, finish) is cheaper for the operator and worse for the corpus; I chose the corpus.

---

## 5. Ruling 4 — "navigating a directory"

### 5.1 What exists (observed)

- `eidolon_tools::shell::run(cwd, command, timeout, cancel)` spawns `bash -c <command>` with `.current_dir(cwd)` (`eidolon/crates/tools/src/shell.rs`, `pub async fn run`). The `cwd` is `Host::cwd()` — the dispatcher's live copy, or the host's own before a dispatcher is attached (`eidolon/crates/rune/src/host.rs`). The dispatcher has `cwd()` and `set_cwd()` (`eidolon/crates/core/src/dispatch.rs`), documented as moving with session adoption; no tool in `bash.rn` or `host.rs` sets it.
- The serve process that would run a triage graph is pid 31900, `eidolon.exe serve --cwd C:\Users\dxcen\Projects\bonsai2` (M5); at observation time its only child was `conhost.exe` (M10) — `bash` children do not persist between calls.
- `cd /etc/nginx && ls` classifies as `reads: false` ("safe command chain"; `cd` is "unrecognized"); `(cd /etc && ls)` is refused as a compound (M11); `ls /etc/nginx` is `reads: true` (M1). Reads outside the workspace are allowed (`cat /etc/nginx/nginx.conf`, `grep -rn listen /etc/nginx`, `tail -n 100 /var/log/syslog` all `Allow read true`); only *writes* outside it are refused — correcting a claim in my own notes that paths outside cwd ask.

### 5.2 The ruling

**cwd is a fact about the harness, not state in the graph.** Every command runs in the same directory, the one `eidolon serve --cwd` was given; a `cd` inside a command dies with that command's `bash`; and under `reads` a `cd` chain is not covered at all. So `cd` is a mistake, and "navigating a directory" is: a path found by evidence (`case.cwd` from `readlink /proc/<pid>/cwd`; `case.dir` from a `--config` flag, from `find`, or from a menu item that knows where a program keeps its files) rendered into **absolute-path arguments** of read commands — `ls {{context.case.dir}}`, `find {{context.case.dir}} -maxdepth 2 -name '*.toml'`, `grep -rn listen {{context.case.dir}}`. The graph descends a tree by capturing a child path from one listing and rendering it into the next.

Lints (spec step 4), all advisory prefix checks — the runtime algebra is the authority, and these exist so an author hears it at lint time rather than as a headless decline: under a `reads` warrant, a `bash` command whose first word is `cd`, or which contains `&& cd`, `; cd`, `| cd`, is an error ("cd is not a graph action; render the path into the next command"); one whose first word is `for`, `while`, `until`, `if`, `(` or `{` is an error ("the graph is the chain: a lines menu, an exists guard, or a capture"); and `background: true` is an error (a background job is not an observation).

The *target* process's cwd is evidence, distinct from the harness's: on Linux `readlink /proc/<pid>/cwd` (`Allow read true`, M1); on Windows there is no cheap equivalent — I did not obtain pid 31900's real cwd (section 10), only the `--cwd` flag on its command line, which is a declaration by the process, not an observation of it.

### 5.3 Confidence

**0.9.** The only way this is wrong is if a future tool exposes `set_cwd` to graphs, and that would be a deliberate widening this document argues against.

---

## 6. Ruling 5 — the techniques

### 6.1 What exists on this box (observed, M1 and M7)

Presence: `netstat`, `tasklist`, `powershell`, `python` (3.14.7), Git Bash `ps` (MSYS: `ps -eo …` → `ps: unknown option -- o`, exit 1; bare `ps` header `PID PPID PGID WINPID TTY UID STIME COMMAND`). **Missing:** `ss`, `lsof`, `wmic`, `journalctl`, `systemctl`, `pwsh`. Of the shipped graph's nine commands, on this box `ps -eo …` fails, `ss` and `nft`/`iptables` are absent, and `who`/`ip`/`hostname; uptime`/`uname -a`/`cat /etc/os-release` are MSYS approximations; J1 measured four of seven exiting nonzero.

Classification under the deployed table (identical to shipped for shell, M3):

| Question (node) | Technique (label, stable across platforms) | Linux command | verdict | Windows / Git Bash command | verdict |
|---|---|---|---|---|---|
| what holds the port | listening sockets | `ss -ltnp \| grep ":{{port}} "` | Allow, safe pipeline, reads true | `netstat -ano \| grep LISTENING \| grep ":{{port}} "` | Allow, safe pipeline, reads true |
| | open files on the port | `lsof -i :{{port}} -P -n` | Allow, read, reads true | none | — |
| | (fallback) netstat | `netstat -tulpn` | Allow, read, reads true | same | same |
| what is that pid | command line | `cat /proc/{{pid}}/cmdline \| tr '\0' ' '` | Allow, safe pipeline, reads true | `powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId={{pid}}').CommandLine"` | Allow, **unrecognized, reads false** |
| | process row | `ps -o pid,ppid,user,etime,cmd -p {{pid}}` | Allow, read, reads true | `tasklist /FI "PID eq {{pid}}" /V /FO LIST` | Allow, **unrecognized, reads false** |
| | working directory | `readlink /proc/{{pid}}/cwd` | Allow, read, reads true | none cheap | — |
| | open files | `lsof -nP -p {{pid}} \| grep -E "cwd\|txt\|REG"` | Allow, safe pipeline, reads true | none | — |
| where is its config | flag on the command line | (capture from `case.cmdline`) | — | same | — |
| | the program's default dir | `ls ~/.config/{{prog}}` | Allow, read, reads true | `ls "$APPDATA/{{prog}}"` | Allow, read, reads true |
| | search by name | `find /etc /opt ~ -maxdepth 3 -name '*.toml' -o -name '*.conf' 2>/dev/null \| head -20` | Allow, safe pipeline, reads true | `find "$APPDATA" -maxdepth 2 -name config.toml -path "*{{prog}}*"` | Allow, read, reads true |
| | search by content | `grep -rn "{{port}}" /etc /opt --include=*.conf --include=*.toml 2>/dev/null \| head -20` | Allow, safe pipeline, reads true | same shape | same |
| what did it log | unit journal | `journalctl -u {{unit}} -n 50 --no-pager` | Allow, journalctl read, reads true | `powershell … Get-WinEvent …` | unrecognized, reads false |
| | service state | `systemctl status {{unit}}` | Allow, systemctl read, reads true | `sc query {{svc}}` | not measured |
| | log file | `tail -n 100 /var/log/syslog` | Allow, read, reads true | `tail -n 100 {{file}}` | same |
| what is consuming | top consumers | `ps -eo pid,comm,pcpu,rss --sort=-pcpu \| head -15` | Allow, safe pipeline, reads true | `ps` (MSYS, no `-o`) | fails (exit 1) |

Windows conclusions (reasoned from the observed verdicts): **every Windows technique that names a process by pid goes through `powershell -Command` or `tasklist`, and both are unrecognized by the table, so neither is a read. Under `reads`, Windows triage asks at the process step every time.** Two ways out, neither in this document's remit: a `tasklist` row in `policy.rn` (`v(ALLOW, "read", true)` — one line; `tasklist` has no mutating form), and accepting that `powershell -Command <anything>` can never be a read by shape — it is `sh -c`. The practical ruling: **Windows triage is attended; Linux triage is what `reads` is for**, which is the direction A7 is ruling from the other side.

### 6.2 The ruling

- **Menus are per node, and a node is a question.** "What holds the port" has three techniques; "what is that pid" has four; a global menu of thirty commands is exactly what a 32-byte-per-option uncalibrated chooser cannot use. Each node keeps `exclude: {{context.tried}}` and a `count … gte N` exhaustion edge, as the shipped graph does.
- **One graph per platform, sharing labels.** The technique *label* is the same across platforms ("listening sockets", "command line"); the command, the `expect` pattern and the `capture` pattern differ, and so does the literal `commands` list. A single graph with platform branches would carry both warrants and both pattern sets in every node and lint against the union; two graphs, `triage-port` (Linux) and `triage-port-windows`, each lint against their own. Rows in the decision log are then comparable across platforms by label — the corpus transfers by technique name (section 7).
- **`prefer` before the chooser wherever the technique order is known.** "listening sockets" is always first for a port question; the chooser is consulted only when the author has no rule, which for mechanical cases is almost never (reasoned: an author who knows the question knows the first technique; the chooser's job in triage is the interpretive tail, and that tail parks).
- **`expect` on every technique**, with the pattern matched against the header or the first data row, never against the mere absence of an error: on this box `ps -eo` fails at exit 1 and E4 turns that into `ERROR failed` before `expect` runs, but the MSYS `ps` header `PID PPID PGID WINPID …` would pass an `expect` that only checked "some rows", so the shipped `^COMMAND\s+PID\s+USER` is the right kind of pattern and stays.
- **No control flow in a command string** (2.6). A loop is a `lines` menu; a conditional is a guard; a variable is a capture.

### 6.3 Confidence

That per-node menus and per-platform graphs are right: **0.75**. That the Linux commands classify as stated: **0.9** (observed under the deployed table; `--config` did not move it). That the Linux commands *return* what the `expect` patterns assume: **not measured** — section 10, handed to A7.

---

## 7. Ruling 6 — where a shell checkpoint's corpus comes from

### 7.1 The reading under test

Decisions, "Operator direction: per-domain checkpoints, a second box, and Nix", offers: the decision log *is* the corpus by construction — every row is `{context, options, label}`; S45's `log_run_end` promotes outcomes; an escalation is a labelling event (an ~82% escalation rate for a small checkpoint means most rows are human-labelled); directory navigation is cheapest to train because it has an oracle, shell selection has none.

### 7.2 Tested against what exists (observed)

- The row shape is real: `log_decision` writes `{id, context, options, label, chosen, probs, source, verified, floor, margin, run, graph, state, step, ts, ckpt, action, warrant}` and `correct_decision` relabels by `id` (`jev/automation/decisions.py`). `log_run_end` writes `{run, outcome, ts, warrant}` from `Run._settle` (S45, landed). The exporter and the relabel-on-outcome pass are **not built** (`automation.md` §5 describes them; S50's sweep lists what is unbuilt).
- The corpus on this box is **empty of graph rows**: `%LOCALAPPDATA%\eidolon\extensions\jev\decisions.jsonl` holds three rows of the interactive `jev_choose` toy (`"Which queue handles this? The customer wants a refund."` / `billing, sales, technical support`, no `run`/`graph`/`state` keys), and the `decisions/` and `runs/` directories do not exist. J1's seven triage rows were written under `JEV_DECISIONS_DIR` in a scratch directory and are gone.
- The `context` a row stores is what the chooser saw — the rendered template — and the chooser reads 192 bytes of it. A row whose 192 bytes are a notes blob does not contain the fact the label depended on (S47's measurement).
- `source ∈ {jevlike, model, human, forced, rule}` and `verified ∈ {null, outcome, model, human, rule}` are the provenance vocabulary; `rule` rows are `prefer` picks with one-hot probs and no chooser call.

### 7.3 The ruling on the reading

**True in structure, empty in fact, and conditional on Ruling 2.** Three corrections to the reading, then what survives:

1. *"By construction" holds only if the context is typed.* A training row is `{192 bytes of context, ≤32 bytes per option, label}`. If the 192 bytes are a notes blob, the label is unlearnable from the row — the model is asked to predict a pick from text that does not contain the evidence. With Ruling 2 the context is `port 8085 pid 31900 exe eidolon.exe; which technique next?` and the label is learnable. The corpus is a *consequence* of typed evidence, not of logging.
2. *`rule` rows are not labels; they are the rule.* A `prefer` pick is correct by definition and teaches the model to imitate a regex the graph already applies for free. They belong in the export only as distillation (a checkpoint that agrees with the rules where rules exist and generalises where they do not); they must be exported with `source: rule` intact so a trainer can weight them, and never counted toward the ~82% figure.
3. *Escalation answers are the corpus, and the escalation is the interpretive tail.* The human labels arrive exactly where Ruling 3 puts them — the `confirm` state and the floor/margin parks. That is where an uncalibrated chooser is most wrong and where a label is most valuable, so the ~82% rate is not a cost to drive down before there is a corpus; it *is* the corpus being written. A run that finishes `reached` on a mechanical case adds no human label and needs none; its rows promote to `verified: outcome` via the run-end row once the exporter exists.

What survives unchanged: **directory navigation is the cheapest domain to train**, for the reason given — its oracle is `exists` on a captured path that a later `ls` confirmed; a row's label is verifiable from the next observation without a human. Shell-technique selection among read-only techniques has no oracle beyond "did the case complete", a run-level rather than row-level signal (`verified: outcome` promotes the whole run's picks, the wasted ones included). Per-domain checkpoints are keyed by `_ckpt_id()` in the rows already; S47's calibration record per checkpoint is specified and not landed (`graph.py`'s `_JEV_KEYS` has no `calibration`; observed). And 2.6 adds a fourth reason the graph beats the shell here: every loop iteration the shell would have hidden inside a `for` is a row.

### 7.4 Confidence

That the reading needs corrections 1–3: **0.8**. That a triage checkpoint trained on typed-context rows would separate techniques better than chance: **hypothesis** — the measurement is S47's calibration record on a triage checkpoint against a held-out log, and there is no log yet. What produces one: run the mechanical `triage-port` graph on A7's Linux box under `JEV_DECISIONS_DIR` pointed at a kept directory, not scratch.

---

## 8. The worked example

**Symptom:** *"Something is already listening on 8085. What process is it, and which config is it running under?"* Mechanical case, traced on this box with the commands in section 1, exactly as `bash -c` would see them from the harness cwd `C:\Users\dxcen\Projects\bonsai2`. Every observation below is verbatim. Where a step cannot work in today's tree, it is shown failing and the spec step it needs is named.

**Input:** `{"port": "8085", "prog_hint": ""}`. **Warrant (proposed):** `{tools: ["bash"], origins: [], commands: [], reads: true, actions: 30, wall_s: 300}`. Under today's seven-key envelope every command below is templated and therefore uncovered — the run asks at step 1 and, headless, `NoUser` declines into `recover` (observed lint and hook behaviour, 2.1). **Needs spec steps 1–4.**

### Step 1 — node `socket`: what holds the port?

Option set (menu; labels are the technique names):

| label | command (this box) | table verdict | present |
|---|---|---|---|
| listening sockets (netstat) | `netstat -ano \| grep LISTENING \| grep ":8085 "` | Allow, safe pipeline, reads true | yes |
| listening sockets (ss) | `ss -ltnp \| grep ":8085 "` | Allow, safe pipeline, reads true | **no** (`ss: MISSING`) |
| open files on the port (lsof) | `lsof -i :8085 -P -n` | Allow, read, reads true | **no** (`lsof: MISSING`) |

Pick: `prefer: [{"type": "contains", "params": {"path": "option.label", "value": "netstat"}}]` → `source: rule`, no chooser call. (Had `ss` been picked: `bash` exits 127, `is_error` is true, E4 raises `ERROR reason: failed`, the root `on.ERROR` goes to `recover`, `CONTINUE` returns through the history state and `exclude: tried` removes it — J1's observed path and `test_a_failed_listing_command_reaches_recover`.)

Observation (exit 0):

```
  TCP    127.0.0.1:8085         0.0.0.0:0              LISTENING       31900
```

`expect`: `{"type": "matches", "params": {"path": "context.obs.text", "pattern": "^\\s*TCP\\s+\\S+:8085\\s"}}` — passes. It guards the *kind*: a grep that matched nothing returns exit 1 (caught earlier by `is_error`); a grep that matched a different port's substring (`:80850`) would pass `is_error` and fail here.

`capture` (**needs spec step 5**): `(?m)^\s*TCP\s+(?P<addr>\S+):8085\s+\S+\s+LISTENING\s+(?P<pid>\d+)` into `case` → `case.addr = "127.0.0.1"`, `case.pid = "31900"`.

Case after step 1: `{port: 8085, addr: 127.0.0.1, pid: 31900}`. **What fails today:** without `capture`, the only place `31900` can go is a `notes` string, and no template can get it back out into the next command; the chain stops here. (On Linux, `cat /proc/$(pgrep -o eidolon)/cmdline` would carry the pid through the shell instead — covered as a read, M11 — but then the pid is never a key in the case; 2.6.)

### Step 2 — node `process`: what is that pid?

Option set:

| label | command (this box) | table verdict |
|---|---|---|
| command line | `powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter 'ProcessId={{context.case.pid}}').CommandLine"` | Allow, **unrecognized, reads false** |
| process row | `tasklist /FI "PID eq {{context.case.pid}}" /V /FO LIST` | Allow, **unrecognized, reads false** |
| process row (ps) | `ps -o pid,ppid,user,etime,cmd -p {{context.case.pid}}` | Allow, read, reads true — but MSYS `ps` has no `-o`: exit 1 |

Pick: `prefer` "command line". **What fails today, and still under `reads`:** this command is not a read by the table, so under a `reads` warrant it **asks** — on Windows the process step is attended (6.1). On Linux the twin `cat /proc/31900/cmdline | tr '\0' ' '` is `reads: true` and runs.

Observation (exit 0), the bare-property form:

```
"C:\Users\dxcen\Projects\bonsai2\eidolon\target\release\eidolon.exe" serve --cwd C:\Users\dxcen\Projects\bonsai2 
```

Why the bare form: the `Select-Object … | Format-List` form (M5) wrapped the value at console width —

```
CommandLine     : "C:\Users\dxcen\Projects\bonsai2\eidolon\target\release\eidolon.exe" serve --cwd 
                  C:\Users\dxcen\Projects\bonsai2 
```

— and against that text (M12) `--cwd (?P<cwd>\S+)` and `--cwd[ \t]+(?P<cwd>\S+)` find nothing, while `--cwd\s+(?P<cwd>\S+)` finds the path only because the wrap happens to fall on the one whitespace run the pattern tolerates. A real failure, observed, that a pattern survives by luck; the fix is at the source (format so the field is on one line), the same rule as 3.2's "format at the source".

`expect`: `matches ^"?(?:[A-Za-z]:\\|/)` (the line begins with a path; M12: matches). `capture`: `^"?(?P<exe>[^"]+?)"?\s+(?P<args>.*)$` (M12: `exe` = the `.exe` path, `args` = `serve --cwd C:\Users\dxcen\Projects\bonsai2`), then on `case.args`: `--cwd\s+(?P<cwd>\S+)(?:.*--config\s+(?P<config>\S+))?` (M12: `cwd` captured, `config` not) — anchored on the required group, per 3.2; `config` does not participate, and *its absence is the finding* that the process runs on its default config location.

Case after step 2: `{port, addr, pid: 31900, exe: C:\…\eidolon.exe, cwd: C:\Users\dxcen\Projects\bonsai2, config: (absent)}`.

**The trap this step must carry, observed on the neighbour port.** For `:54051` the same two techniques disagree: `ExecutablePath : C:\Users\dxcen\AppData\Local\Programs\Python\Python314\python.exe` and `CommandLine : C:\Users\dxcen\Projects\cms-agent\models\jevlike\.venv\Scripts\python.exe -u server.py` — the venv launcher re-executes the base interpreter, so argv[0] is the launcher and the image is the base Python. Both are true. A case that stores only one of them names the wrong thing with full confidence (Decisions, "I have been naming the wrong process all day"). Ruling: `case.exe` (image) and `case.argv0` (launcher) are both captured when both are observable, and the report prints both.

### Step 3 — node `config`: which config does it run under?

`case.config` is absent, so the `always` edge `exists context.case.config` does not fire and the node offers its menu:

| label | command (this box) | table verdict | observation |
|---|---|---|---|
| the program's default dir | `ls "$APPDATA/eidolon"` | Allow, read, reads true | `config.toml policy.rn providers secrets serve.token system tools ui.rn` (exit 0) |
| search by name | `find "$APPDATA" -maxdepth 2 -name config.toml -path "*eidolon*"` | Allow, read, reads true | `C:\Users\dxcen\AppData\Roaming/eidolon/config.toml` (exit 0) |
| ask the program | `"{{context.case.exe}}" --help` | Allow, unrecognized, reads false | not run under `reads` — running the target binary is not a read. Its `--help` does say `default: ~/.config/eidolon/config.toml` (observed via `eidolon policy --help`), which is *program knowledge* an author writes into the menu, not something a read-only run may discover |

Pick: `prefer` "the program's default dir" — an author who wrote a graph for a program knows where it keeps its config; `find` is the fallback for a program the author did not anticipate. `expect`: `matches (?m)^config\.toml$`. `capture`: `(?m)^(?P<config_name>config\.toml)$`, then `assign` `case.config = "$APPDATA/eidolon/config.toml"`. (A template cannot join two strings with a separator, so the path is assembled by the command that reads it next, `cat "$APPDATA/eidolon/{{context.case.config_name}}"` — a limitation, named.)

Then `cat "$APPDATA/eidolon/config.toml"` (Allow, read, reads true), observed (secrets redacted; none present):

```
extensions_dir = "C:/Users/dxcen/Projects/bonsai2/extensions"

[[extensions]]
name = "jev"
enabled = true

[[extensions]]
name = "browser"
enabled = true
```

`capture`: `(?m)^extensions_dir = "(?P<extensions_dir>[^"]+)"` and `(?ms)\[\[extensions\]\]\s+name = "(?P<ext1>[^"]+)"`. The disclosure rule (2.4) applies here: a `cat` of this directory's `serve.token` would also be a read; the graph reads the file the case names, not the directory.

### Termination

`always`: `and(exists case.pid, exists case.cmdline, exists case.config)` → `report`. Output — the case, not an answer:

```
port 8085 (127.0.0.1) is held by pid 31900
  image     C:\Users\dxcen\Projects\bonsai2\eidolon\target\release\eidolon.exe
  argv      "…\eidolon.exe" serve --cwd C:\Users\dxcen\Projects\bonsai2
  parent    54644 (observed via M5; the graph would capture it from the process row)
  cwd       C:\Users\dxcen\Projects\bonsai2   (declared by --cwd; not observed from the process)
  config    C:\Users\dxcen\AppData\Roaming\eidolon\config.toml  (default location; no --config flag)
            extensions_dir C:/Users/dxcen/Projects/bonsai2/extensions; extensions jev, browser
  policy    C:\Users\dxcen\AppData\Roaming\eidolon\policy.rn   (sibling of the config; md5 3fcd0e7e…, 2026-09-18 — older than the shipped table)
```

Steps: 4 commands, 0 chooser calls (three `rule` picks), 0 escalations on Linux, 1 on Windows (the process step), 0 structural refusals (every command is a simple read or a pipe). Decision-log rows: three with `source: rule`, each with a typed context under 80 bytes. Evidence context at the end: eight keys, none longer than a path.

### The shipped graph's own failing step, for the record

`processes.entry` runs `ps -eo comm,pid,user,pcpu,etime --sort=-pcpu | head -30` under `expect matches ^COMMAND\s+PID\s+USER`. On this box (M7): stderr `ps: unknown option -- o`, exit 1. With E4 landed: `is_error` → `ERROR {reason: "failed", tool: "bash"}` → `recover`, and `expect` never runs. Before E4 (J1): the error text was stored as the listing and the chooser scored `ps: unknown option -- o` at 0.995 as the process that "does not belong on a server". Had the command been bare `ps`, exit 0 with the MSYS header `PID PPID PGID WINPID TTY UID STIME COMMAND` → `expect` fails → `ERROR {reason: "unexpected"}`. Both beliefs are needed; both are landed; the graph's command is simply not this platform's.

---

## 9. Staged implementation spec

Ordered. Each step names the file and symbol (by name, not line), the test that fails when the step is reverted, and the interpreter or crate that runs it. **Do not run `cargo build --release` while agents are live** (record rule); `cargo test -p <crate>` builds debug. jev tests: `C:\Users\dxcen\AppData\Local\Programs\Python\Python314\python.exe -m unittest discover -s jev/tests -t .` from `C:\Users\dxcen\Projects\bonsai2` — the system interpreter (3.14.7); `jev/` has no venv (observed). The last logged baseline is `jev/tests` 249 (S45, Build-Log); **re-measure on a clean tree before step 5**, because at the time of writing `jev/automation/options.py::_collapse_duplicates` carries another agent's live `BREAK-TEST` edit (observed) and the count is not mine to report. Isolation for any run: `JEV_DECISIONS_DIR`, `JEV_RUNS_DIR`, `JEV_DECISIONS_LOG` pointed at a scratch directory.

**Step 1 — `Ruling.read_only`.** `eidolon/crates/core/src/policy.rs`, `pub struct Ruling`: add `pub read_only: bool` with a doc comment mirroring `structural`'s ("Carried across from `harnox::policy::Verdict::read_only` for the warrant layer; `false` from a hook with no classifier"). `eidolon/crates/rune/src/policy.rs`, `impl PolicyHook for ScriptPolicy`, `pre_tool`: set it from the `Tier`. Every `Ruling { … }` literal the compiler then names gets `read_only: false` (Yolo, Judge, Warrant's `recompose`, tests). *Test (rune):* `a_read_only_shell_verdict_reaches_the_ruling` — `ScriptPolicy::builtin` classifying `bash {"command": "ls"}` yields `ruling.read_only == true`; `bash {"command": "kill 1"}` yields `false`; `bash {"command": "ls && git commit"}` yields `false`. Revert: does not compile. `cargo test -p eidolon-rune a_read_only_shell_verdict_reaches_the_ruling`.

**Step 2 — the eighth key.** `eidolon/crates/rune/src/warrant.rs`: `KEYS` gains `reads`; `Envelope` gains `reads: bool` (a non-bool is refused naming the key, as `ascii_string_set` refuses a non-ASCII origin); `canonical_json` emits it in key order; `summary` prints `reads: any read-only bash command` when true. `jev/automation/warrant.py`: `KEYS` gains `reads`; `canonical` reads it from the graph's block (default `false` when absent, so an attended graph need not say it); `_normalize` carries it. `jev/automation/graph.py`, `_WARRANT_KEYS` and `_validate_warrant`: admit `reads` as a bool. Both shipped graphs get the key (`wiki-hop.json`: `false`; `triage-linux.json`: `true`). *Tests:* re-pin `the_shipped_wiki_hop_envelope_has_the_pinned_id` (Rust) and `test_the_shipped_wiki_hop_id_is_the_pinned_literal` / `test_the_rust_placeholder_envelope_hashes_to_the_rust_pinned_id` (jev) — record the old and new ids in the Build-Log; extend `test_each_of_the_seven_fields_alone_causes_refusal` to eight with the `reads` case, renaming it; new `reads_changes_the_id` on both sides (two envelopes differing only in `reads` have different ids). Revert: the pinned-id tests fail. Note for the operator: any `[[warrants]]` stanza issued under the seven-key id must be re-issued (none exists on this box, M9).

**Step 3 — the cover.** `warrant.rs`, `Warrant::pre_tool_nested`, the `bash` match: add `Some(_) if env.reads && ruling.read_only => None,` immediately after the `commands.contains` arm; the uncovered-command prompt gains "(not a read)" when `env.reads` is true so the operator sees why. *Tests (rune, beside `a_bash_command_not_listed_in_the_warrant_asks`, using `triage_block()` with `reads: true`):* `a_read_only_bash_command_not_listed_under_a_reads_warrant_is_warranted` (`ss -ltnp | grep ":8080 "` → `Verdict::Allow`, `ruling.warrant.is_some()`); `a_mutating_bash_command_under_a_reads_warrant_asks` (`kill 1`); `a_write_redirect_under_a_reads_warrant_asks` (`ps aux > /tmp/x`); `a_cd_chain_under_a_reads_warrant_asks` (`cd /etc && ls`); `a_compound_command_under_a_reads_warrant_asks` (`for p in 1 2; do cat /proc/$p/comm; done`); `a_reads_false_warrant_still_asks_for_an_unlisted_read` (`reads: false`, `ls`); and, pinning 2.6's reasoned claim, `a_listed_literal_compound_command_is_warranted_despite_its_shape` (the `for` loop above listed verbatim in `commands` → `Allow`). Revert: the first fails. **Also** the existing `a_bash_command_not_listed_in_the_warrant_asks` must keep using `reads: false` or a mutating command, or it silently stops testing what it tests.

**Step 4 — the lint.** `graph.py`, `_lint_warrant`: when `reads` is true, a templated `bash` `input.command` is admitted; when false, the existing error is reworded: `"… is rendered from context and is not enumerable at authoring time; declare meta.jev.warrant.reads: true to cover read-only commands, or make it literal"`. Add the prefix rules from 5.2 (`cd`, control-flow keywords, `background`). *Tests (`jev/tests/test_graph.py`):* `a_templated_bash_command_needs_reads`, `a_cd_chain_under_reads_is_refused_by_lint`, `a_control_flow_command_under_reads_is_refused_by_lint`, `a_background_bash_under_reads_is_refused_by_lint`. Revert: each fails.

**Step 5 — `capture`.** `graph.py`: `_ACTION_KINDS` gains `capture`; `_validate_action` requires `path` (a `PATH_ROOTS` path), `pattern` (compiles under `re.MULTILINE`; a pattern with no named group is a lint error — a capture that captures nothing is a `matches`; a pattern for which `re.search(pattern, "")` is not `None` is a lint error — 3.2, the empty-match defect M12 caught), `into` (an identifier). `run.py`, `_run_actions`: `elif kind == "capture"` → resolve `path` via `template.resolve_path`, `re.search`, assign participating groups under `rt.context[into]`; no match → raise the same error type `_run_tool_action` raises, with `reason="unexpected"`, `tool="capture"`, text naming the path and pattern. `PATH_ROOTS` unchanged. *Tests (`jev/tests/test_run.py`, a new `CaptureActionTests(_IsolatedDecisionsDir)` beside `ExpectGuardTests`):* `a_capture_assigns_each_participating_named_group`; `an_optional_group_that_did_not_participate_is_not_assigned`; `a_capture_that_does_not_match_raises_error_unexpected_naming_capture`; `a_capture_reads_the_chosen_line_after_a_lines_pick` (`event.option.line`); and in `test_graph.py`, `a_capture_with_no_named_group_fails_lint`, `a_capture_pattern_that_matches_the_empty_string_fails_lint` (the section 8 first-draft pattern, verbatim). Revert: the first fails on an unknown action kind.

**Step 6 — the mechanical graph.** `extensions/jev/graphs/triage-port.json` (Linux): nodes `socket` → `process` → `config` → `report`, menus and patterns from 6.1 and section 8, `prefer` on each menu, `expect` on every technique, `capture` after each, `reads: true`, termination `and(exists …)`; `context.case` initial `{}`; `notes` capped by the 400-char head capture; no control flow in any command. `test_run.py`: `TriagePortEndToEndTests(_IsolatedDecisionsDir)` beside `TriageLinuxEndToEndTests`, with a `_bash` fake returning the section 8 observations keyed by command prefix (`ss -ltnp`, `cat /proc/`, `readlink`, `ls `, `cat `), asserting `outcome == "reached"`, the report's `case.pid == "31900"`, zero chooser calls, three `source: rule` rows, and — the negative — that a fake returning the MSYS `ps` header produces `ERROR unexpected` and a `recover` visit. Warrant test in rune: `a_templated_read_under_a_reads_warrant_dispatches` (the rendered `cat /proc/31900/cmdline | tr '\0' ' '` is `Warranted`). *Optional lint from 3.2:* a `choose.context` may not reference `context.notes` when the graph declares `case`. `triage-port-windows.json` is **not** shipped until `tasklist` has a table row; document the attended path instead.

**Step 7 — `judge` confirms.** `triage-linux.json`: `judge.on.PICK.target` becomes `confirm`; `confirm` has `meta.ask: "Flag {{event.option.label}}? The case so far: {{context.notes_last}}"` and transitions `FLAG` (pushes `flagged`, → `done`) / `CLEAR` (→ `done`); `CLEAN` unchanged. *Tests:* `test_a_flagged_process_is_confirmed_before_the_run_reaches_report` — with `constant_chooser` the run parks at `confirm` with the row `verified: null`; after `answer("FLAG")` the row is `verified: human` and `flagged` has one entry; `test_a_headless_flag_ends_declined_with_the_case_in_the_report`. Revert: the run reaches `report` with `flagged` set by a pick.

**Step 8 — out of scope here, named so nobody waits on it:** the exporter and outcome relabel (`automation.md` §5, S47's calibration record); the `tasklist` table row and a `powershell -Command` FLAG row (`policy.rn`, a policy task); `Verdict.reaches` (harnox, if an operator wants network in the envelope); the deployed-table staleness (S51); and, from 2.6, whether the algebra should learn any further shape at all — this document says no, the graph carries them.

---

## 10. Measurements not taken, and the fallback used

1. **No live triage run on this machine.** Forbidden by the brief. Fallback: section 8's observations were taken one command at a time as `bash -c` would run them; run-level behaviour (recover, exclude, history) is taken from J1's record and `TriageLinuxEndToEndTests`.
2. **No Linux outputs.** `ss`, `lsof`, `journalctl`, `systemctl`, `/proc` do not exist here; their verdicts are observed (`eidolon policy`), their *output shapes* are reasoned from their documented formats and unverified; every Linux `expect`/`capture` pattern in this document is a hypothesis until A7's box runs it. `observation.md`'s M5 has the same status for `lsof`.
3. **The target process's real cwd on Windows** (pid 31900). `readlink /proc/<pid>/cwd` has no cheap Windows twin (handle-level tooling). Fallback: the `--cwd` flag on its command line, which is a declaration, labelled so in the case.
4. **The jev test baseline on the current tree.** Not run: `options.py::_collapse_duplicates` is mid-break-test by a live agent (observed `BREAK-TEST` comment). Fallback: S45's 249; the implementer re-measures first.
5. **The chooser on triage menus.** No calibration record exists (`meta.jev.calibration` is specified, not landed); the only measurement is S47's on wiki-hop (4.48%). Fallback: `prefer` everywhere a rule exists, so the mechanical example makes zero chooser calls; the interpretive tail parks.
6. **`eidolon policy` under the shipped table.** The running binary ignores `--config` for the table (M2). Fallback: M3's diff shows the shell rows identical, so the verdicts hold for both; the changed rows are tool rows, not shell.
7. **The coordinator's two headless investigations.** Reported, not reproduced; I did not see the two refused command strings, so which substitution shape was "unsafe" is inferred from harnox's rule, not observed. M11 measured the shapes I could name.
8. **How many `Ruling { … }` literals step 1 breaks.** Not counted; the compiler will.
9. **`sc query` and `Get-WinEvent` verdicts.** Not run; assumed unrecognized like the other Windows-native commands (reasoned from the table having no rows for them).
10. **Timing.** No timing numbers are given because none were taken with same-session controls (record rule). J1's 60.27 s for the shipped graph is the only run-level number in the record.

---

## 11. What this does not solve

- **Interpretive triage stays attended.** By design (section 4). A graph that names a culprit without a human or a checkable guard is the wrong confident answer the record has twice today.
- **The second wall, for anything that is not a graph.** A chat model driving `bash` directly will keep meeting "compound command blocked" headless, and this document's answer — keep the chain in the steps — is available only to a graph. The coordinator's lane needs one of the other two options, or a person.
- **Windows unattended triage.** Blocked on two table rows that are not this document's (6.1). The mechanical example asks once per case on Windows.
- **Disclosure through reads.** Named and narrowed (2.4), not closed: an author who captures a secret has captured it. The lint cannot know which file is a secret.
- **Network through reads.** `curl` POST is a read to the table. Closure is the sandbox posture or a harnox bit; both named, neither measured.
- **Two facts that disagree** (image vs argv[0]). Carried, not resolved: the case shows both; which one "is the process" is the operator's question, and the report must not pick.
- **The chooser's place in triage.** With `prefer` on every mechanical menu the chooser is consulted only where no rule exists, which today means the interpretive tail, which parks. A triage checkpoint therefore has nothing to learn from until Rulings 2 and 3 land and A7's box produces a kept log — section 7 says why that is the right order and not a loss.

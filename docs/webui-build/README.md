# docs/webui-build — the web UI build's plan, specs, tracker, reviews and scripts

The 49-slice build of Minerva's web UI (perch routes, page tabs, terminals, runs, mesh, notes, installer keys) ran from 2026-10-07 to
2026-10-15 on one Windows 11 + WSL box. Its working files lived in a Claude session scratchpad. They were copied here on 2026-10-15, so that
development can move to another machine (TRACK.md:197). This folder is the build's record, and the place to resume it.

Citations: paths without a folder (`TRACK.md:N`) and paths under `specs/`, `plan/`, `reviews/`, `workflows/` and `drafts/` are relative to
this folder. Other paths are relative to the repository root. **Observed** means read off the old machine on 2026-10-15, between 01:20 and
02:00.

## What is here

| Path | What it is | Relation |
|---|---|---|
| `plan/pass2-final.json` | The approved plan. It holds the 49 slices (`slices`, :85) and the waves, MECHANICS, PORTS and CRITICAL PATH (`waves`, :1166). It also holds the limits (:1167), the decisions it needed (:1204), the demo (:1327), the $0 model-spend guarantee (:1328) and the operator-only steps (:1329-1342) | Source of every 49-plan slice |
| `specs/<ID>.json` | 60 slice specs: the 49 plan slices plus 11 later ones (R2c, I2, SA1, SA2, SA3, RT, GP-a, GP-b, GP-c, RA, PR). Fields: `id title what owns_files depends_on tests live_check size risk security_review` | One executor job each |
| `specs/BRIEF.md`, `specs/WAVE1-3.md` | The executor's rules. WAVE3 overrides BRIEF and WAVE2 (specs/WAVE3.md:1) | Every agent reads them first (workflows/slice-execute.js:64) |
| `specs/<X>-fixN.md`, `-findings.md`, `-contract.md`, `-notes.md`, `A1b-from-A1a.md`, `W1-session-autoopen.patch` | The orchestrator's round files: decisions, fixes and exact grants for one more round, or a hand-off between slices | They win over the spec where they differ (specs/T2-fix3.md:3-4) |
| `TRACK.md` | The orchestrator's running log, newest at the bottom: launches, verdicts, merges, decisions, user answers | **The authority for state.** Its line 3, "Master: 741b815", is stale. New orchestrator lines go at the bottom, committed with each merge |
| `reviews/<X>/inventory.md` | The executor's security claims, for a reviewer to check | Input to the security review (workflows/slice-security-review.js:19) |
| `reviews/<X>/result.md` | A review's outcome. Only A1b, M2, RT, T1b and T2 have one here | The merge verdict and the fix list |
| `workflows/` | The Claude Code workflow scripts that ran the build (see below) | |
| `drafts/eidolon-issue-tilde-workspace.md` | A public eidolon issue draft: `~/` paths are judged workspace-local in auto. The operator posts it (TRACK.md:108-109) | Open item U12 |

```text
plan/pass2-final.json --one slice--> specs/<ID>.json (+ BRIEF.md, WAVE3.md)
        |
        v
workflows/slice-execute.js:  execute --> verify --> fix (max 2) --> verify ...
        |  pass, and a review was asked for
        v
workflows/slice-security-review.js:  lenses --> skeptic per lens --> critic: merge | merge-after-fixes | block
        |  gaps left after the last round                        |
        v                                                        v
specs/<X>-fixN.md (the orchestrator) --> slice-execute again     orchestrator squash-merges to master, writes a TRACK.md line
```

## How a slice is built

1. **Spec.** `specs/<ID>.json`. Its `owns_files` are the files the executor may touch, plus whatever the prompt grants (specs/BRIEF.md:26).
2. **Execute.** An executor runs in a fresh git worktree (workflows/slice-execute.js:75). Step 0 is `git merge --ff-only master`, never
   `reset --hard` (workflows/slice-execute.js:54; specs/BRIEF.md:6). It builds, tests, and makes one commit with no attribution
   (specs/BRIEF.md:33).
3. **Verify.** An independent verifier is read-only for code. It checks seven things, citing file:line for each
   (workflows/slice-execute.js:86-112): spec items, the tests named and the whole suite, clippy both ways or the page tests, grants via
   `diff --stat`, no attribution, README rows, and no processes left running.
4. **Fix.** A fixer closes the verifier's gaps in the same worktree. The loop verifies at most 3 times, so there are at most 2 fix rounds
   (workflows/slice-execute.js:85,115).
5. **Security review**, only on a pass and only when the job passes `review` (workflows/slice-execute.js:141-147). One reviewer runs per
   lens, then a skeptic per lens tries to refute each finding, then a completeness critic gives the verdict
   (workflows/slice-security-review.js:57-86). The lenses are chosen per slice, e.g. RT's `input/state/leak` (TRACK.md:175). The
   orchestrator also starts reviews directly (TRACK.md:163-164).
6. **One more round.** If gaps survive round 2, the orchestrator writes `specs/<X>-fixN.md` and reruns slice-execute in the existing
   worktree (the `worktree`/`wsl`/`branch` args, workflows/slice-execute.js:52-53). The file has decisions, numbered fixes and `## Grants
   (exactly these)` (specs/T2-fix3.md:33-37). The spec still holds where the file is silent; where they differ, the file wins
   (specs/T2-fix3.md:3-4). `resume_commit` skips the executor when its report was lost (workflows/slice-execute.js:55-61).
7. **Merge.** The orchestrator squash-merges (specs/BRIEF.md:3; TRACK.md:190). At merge it applies the serve.rs pin flips the slice named
   (specs/WAVE3.md:15) and README rows. It then runs the whole suite and clippy on master in `$HOME/.cache/minerva-master-target`
   (TRACK.md:73,139). Browser-pane live checks are the orchestrator's, since executors have no browser (specs/BRIEF.md:32; TRACK.md:189).

**Grants.** A grant is a file outside `owns_files` that one job may edit. It is named in the prompt or the fix file (TRACK.md:180,185) and
checked by the verifier (workflows/slice-execute.js:101). The never-edit list (`tests/common/mod.rs`, `tests/http.rs`, `Cargo.toml`,
`Cargo.lock`, `src/bin/fake_eidolon.rs` (its submodules only where the spec lists them), `src/lib.rs`, `src/main.rs`, `src/serve.rs`)
holds unless granted (specs/WAVE3.md:14).

### The scripts

They run under the Claude Code workflow tool, using its globals `agent`, `phase`, `log`, `pipeline` and `workflow`. Their input is the
global `args` (workflows/slice-execute.js:13). TRACK.md names each run by its `wf_…` id. The script names at TRACK.md:58 and :115
(`slice-security-review-wf_6ce2c794-fdc.js`, `slice-execute-wf_e3830100-b6b.js`) are `workflows/slice-security-review.js` and
`workflows/slice-execute.js`.

| Script | Args (where read) |
|---|---|
| `slice-execute.js` | `slice`, `spec` (:64); `prompt`, the job text (:71); `base` (:54,88); `port` (:19); `model`, default `haiku` (:14); `kind: 'page'` switches check 3 to the page tests (:100); `check3`, `suite` (:99-100); `inventory`, a path the executor writes (:73); `review: {script, lenses: [{key, focus}]}` (:141-146); `worktree`, `wsl`, `branch` to reuse a worktree (:52-53); `resume_commit` (:56) |
| `slice-security-review.js` | `slice worktree wsl branch commit base spec plan brief inventory scratch port lenses model` (:13-23,58-60). It hard-codes no paths, but its COMMON text (:16-17, :22) assumes Git Bash and WSL: rewrite it for one shell |
| `agent-runs-design.js`, `graph-pane-design.js` | No args. These were one-off design jobs: they wrote RA/PR/GP-c and RT/GP-a/GP-b (TRACK.md:174,191). Kept as history. graph-pane-design.js:15 names a worktree that no longer exists (TRACK.md:198) |

**No launch call was saved.** The original job prompts and lens `focus` sentences are lost (TRACK.md:175,179 keep only the lens keys):
write them fresh. A composed example (workflows/slice-execute.js:13-75,141-147; specs/GP-a.json:18,28), with scriptPath
`<repo>/docs/webui-build/workflows/slice-execute.js`:

```json
{ "slice": "GP-a", "spec": "<repo>/docs/webui-build/specs/GP-a.json", "base": "<master head at launch, 4bcfa09 or later>",
  "port": 48143, "kind": "page", "model": "<the operator's pick>", "suite": "node --test \"webui/term/tests/*.test.mjs\"",
  "prompt": "Build slice GP-a exactly as its spec says. Grants: its owns_files only. Live check: the spec's curl part on 48143 with master's binaries copied per WAVE3.md:20; list the browser-pane steps for the orchestrator." }
```

To run a fix file in an existing worktree, e.g. T4a (TRACK.md:179,200), add `"worktree": "<repo>/.claude/worktrees/wf_52c52471-5b4-1"`,
`"wsl": "<same path>"`, `"branch": "worktree-wf_52c52471-5b4-1"`, `"port": 48132`, `"prompt": "<text of specs/T4a-fix2.md>"`,
`"inventory": "<repo>/docs/webui-build/reviews/T4a/inventory.md"` and `"review": {"script": "<repo>/docs/webui-build/workflows/slice-security-review.js",
"lenses": [{"key": "render", "focus": "<one sentence>"}, {"key": "input", "focus": "…"}, {"key": "stream", "focus": "…"}]}`. To skip the
executor and only verify and review a committed head, also pass `"resume_commit": "<branch head>"`. Pass inventory paths the agents may
write: they balk at .md report files (TRACK.md:180). Without the workflow tool, run the same agent steps by hand in the order
slice-execute.js gives.

**Change these on the new machine before any run:**

- workflows/slice-execute.js:15-16 hard-codes `SCR` (the old scratchpad) and `SL = SCR\slices`. Point `SL` at `docs/webui-build/specs`,
  and `SCR` at a scratch dir outside the repo. Reviewers experiment in `scratch` (workflows/slice-security-review.js:22).
- workflows/slice-execute.js:145-146 passes `SCR\pass2-final.json` (now `plan/pass2-final.json`), `SL\BRIEF.md` and `SCR\review-<X>`.
  Inventories went to `review-<X>/inventory.md`, which maps to `reviews/<X>/`.
- workflows/agent-runs-design.js:12-14 and workflows/graph-pane-design.js:12-15 hard-code the same paths, plus
  `C:\Users\dxcen\Projects\Minerva`.
- The `RULES` text assumes this Windows box: `df -h /mnt/c`, `wsl.exe -e bash -lc`, git from Git Bash
  (workflows/slice-execute.js:20-21). So do specs/BRIEF.md:3,6,9,12,29,30,33 and specs/WAVE3.md:9. On a Linux box, use `df -h $HOME`
  and plain bash. specs/BRIEF.md:6 calls origin/main stale; after the push it is not.
- Specs name machine paths too: `--plugins-dir /mnt/c/Users/dxcen/Projects/Minerva/eidolon-plugins` (specs/GP-a.json:24,
  specs/GP-c.json:26, specs/RA.json:33, specs/RT.json:16) is `<repo>/eidolon-plugins`. Fix files cite "scratchpad review-<X>"
  (specs/T2-fix3.md:28, RT-fix3.md:28), which is `reviews/<X>/`. `scratchpad/aoide-probe` (specs/M2-fix.md:8) and `rt-probe`
  (RT-fix3.md:19) were not carried.
- The executor default is `haiku` (workflows/slice-execute.js:14), set after a Sonnet weekly limit (TRACK.md:115). The same line records
  that the operator asked for "haiku 5.5" but notes the alias as Haiku 4.5. The review uses the executor's model
  (workflows/slice-execute.js:144), so reviews have run on haiku since TRACK.md:117, though specs/WAVE1.md:11 says Opus. Ask the operator
  for both models before relaunching.

## Where the build stands (observed 2026-10-15, 02:00)

At 02:00, master was `4bcfa09` (RT merged), 57 commits ahead of `origin/main` (f9efdb6), with 0 attribution lines in that range
(observed: `git rev-list --count`, `git log --format=%B | grep -ci co-authored`). 35 of those commits are this build's squash merges.
At the move, master is 4bcfa09 plus the one docs commit that added this folder and HANDOFF.md's top section. Whether it was pushed is
in the last lines of TRACK.md. No workflow was running when the old machine stopped. A workflow does not resume on another machine:
rerun slice-execute with `resume_commit` set to the branch head.

The critical path is S0P-a/b → T1a → T1b → T1c → T2/T4a → T4b → XS → X0 → X3 (plan:1166), and it stands at T2/T4a. The mail path M2 →
M3/N2b → XS is a second gate, held on U1.

### Status

| Slice | State | Where | Waits on / next | Cite |
|---|---|---|---|---|
| 35 merged | J1 2f11e1f · S0W-a 09a6267 · V0 3d84f34 · S0W-b f86f5c5 · W2c 21dd18a · W6 9cf17d9 · S0P-a 3c17994 · S0P-b 95c2b2b · F0 9b5b639 · R3 743ff49 · R2a 63f3385 · N1 741b815 · I1 02ac6ee · W5 9c1db84 · W1 ce7317f · R2b 7d467df · W2a 287cd67 · N2a cc0da12 · AG 9126946 · R1a cd2b89f · A1a c81db09 · G1 911af68 · W4 0e8c97c · R1b 3eaca65 · T1a 8bd0e87 · R4 d8c5353 · W7 71dc2b7 · SA2 3fc4dff · W2b-1 376839a · T3 c0d6568 · T1b b08a32e · W2b-2 b761462 · T1c 08c01fa · W3 5aa2a19 · RT 4bcfa09 | master | — | 34 observed with `merge-base --is-ancestor`, RT with `git log`; TRACK.md:37,57,88-190,204 |
| RT | merged 4bcfa09; master perch suite 919 pass, 0 fail, 1 ignored; clippy clean both ways | branch `worktree-wf_ce3f2350-5aa-1` @ 8435c9a kept until the re-bundle | review lows → follow-ups below; U15 | TRACK.md:203-204; reviews/RT/result.md |
| T2 | merged 4831852 (2026-10-15): round 4 with U21a-d as decided; 941 pass, clippy clean both ways; review merge, one info fixed (dd6ed97) | `worktree-wf_0cb668b4-3e3-1` @ dd6ed97 | none. Follow-ups: chunked 413 on an agent route (the stand-in door always sends Content-Length) | specs/T2-fix4.md; TRACK.md bottom |
| T4a | merged 251e55c (2026-10-15): fix rounds 2 and 3, page suite 492/492; review merge with 5 lows left (blank-looking Unicode titles; the yolo take-over covers only linked terminals; focus reports, CSI ? 1004 h; paste flow control and the ESC[201~ scrub; backoff reset). The browser live check was NOT run | `worktree-wf_52c52471-5b4-1` @ 37d4e9c | the live check (steps in the review inventory), then the lows | specs/T4a-fix2.md, specs/T4a-fix3.md |
| M2 | held U1 | `worktree-agent-aba970ccc3baacf9f` @ e71bcc6 | review: merge-after-fixes, 7 medium; redesign the receive half after the re-probe | TRACK.md:164,167 |
| R2c | held U1 | `worktree-wf_0543ff15-bf5-1` @ 0754f2f | review: merge-after-fixes, 0 high/medium; fold specs/R2c-fix3-notes.md in; merge R2c, then M2 | TRACK.md:156,163,165 |
| A1b | held U2 | `worktree-agent-ab08420a1c15687be` @ 062b9a1 | the three rule questions | TRACK.md:130; reviews/A1b/result.md |
| I2 | held U3; no review yet | `worktree-wf_1f64c6c7-a31-1` @ 33fae11 | round 5 on monotonic revocations | TRACK.md:161-162 |
| SA3 | held U4 | `worktree-wf_e55daf8f-292-1` @ 42dad9c | per-verb vouch vs subagent workflows | TRACK.md:154 |
| P1, SA1 | done upstream, not vendored | dxcently/eidolon-plugins `jev-wiki-hop-max` 2fa3b85, `subagent-parent-config` e9494fe; both pushed, 26 behind origin/main | U10, then P1b | TRACK.md:150,160,193 |
| P1b | queued | — | P1 on eidolon-plugins main; VENDORED.md:12 still pins 7a0ff8e | specs/P1b.json:9 |
| X1 | blocked | — | MemAvailable ≥ 22 GB on the old box. **Re-check on the new one** | TRACK.md:54 |
| RA, PR | held before launch | specs only | U5-U8; RA also waits on T2's merge, PR on RA | TRACK.md:179,186,191; specs/PR.json:19 |
| GW | proposed, no spec | — | U7, U8 | TRACK.md:191; specs/PR.json:30 |
| GP-a → GP-b → GP-c | GP-a is ready (RT, W2b-2, W3 merged); then GP-b, then GP-c, part A only | — | — | TRACK.md:174,190,204; specs/GP-a.json:18; specs/GP-c.json:3 |
| T4b | queued | — | T4a. Its plan port 48133 was also used by I2 | specs/T4b.json:12; TRACK.md:121 |
| M3, N2b | queued | — | M2 | specs/M3.json:11; specs/N2b.json:12 |
| A2 | queued | — | A1b | specs/A2.json:13 |
| XS, D1, X0, X2, X3 | queued (the finish) | — | XS: T2, T4a, A1b, A2, M2, M3, N2b. D1: T4b, N2b, M3, A2. X0: XS, D1. X2: X1, X0, D1. X3: X0 | specs/XS.json:6; specs/D1.json:12; specs/X0.json:6; specs/X2.json:6; specs/X3.json:6 |

Next, in order: GP-a (ready now), T2's round 4 once U21 is answered, T4a's fix round.

Proposed, with no spec yet:
- Y1: the perch answers yolo asks itself (TRACK.md:108).
- I2b: release-files.sh checked through the installer's own parser (TRACK.md:162).
- A T1 hardening slice from the T1b review's lows (TRACK.md:146).

### Open decisions (the operator's)

Settled rulings D1-D6, D9-D12 and "no decision-log gate" are in specs/BRIEF.md:14-23. Apply them; never re-ask.

| # | Question | Gates | Recorded recommendation | Cite |
|---|---|---|---|---|
| U1 | Go-ahead to re-probe **current** Aoide main. M2/R2c facts came from a build 308 commits behind | M2, R2c → M3, N2b, XS, D1, X0 | Re-probe; look for a non-consuming read; rebuild M2's receive half on aoide as the store | TRACK.md:158,164,167 |
| U2 | A1b: (a) rules for runners that execute workspace code; (b) rules answering in manual; (c) keep or drop a rule after `lost` | A1b, A2 | (a) do not ship them, or allow read-only commands only; (b) confirm; (c) return the settled answer in the body | reviews/A1b/result.md:7-9,47-49,106-107 |
| U3 | I2 round 5: should revocations be monotonic? | I2, I2b | Yes | TRACK.md:162 |
| U4 | Subagent per-verb vouch: it disables subagent's `door` and `fanout` workflows (exit 3) | SA3 | none recorded | TRACK.md:154 |
| U5 | Vouch `runs_start`? | PR | Vouch list, read and start | specs/PR.json:6,26 |
| U6 | `--agent-run-graphs` flag, or a hard D9 rule? | RA | The flag, default `wiki-hop` | specs/RA.json:35,39 |
| U7, U8 | GW: deny agent bash `eidolon workflow run …jev wiki-hop`/`resume`, and `eidolon plugins trust/untrust/grant/revoke`. The second is a hole on master today: an agent in auto or yolo can undo D6 | GW | One GW slice, launched now | TRACK.md:191 |
| U9 | Drop `message` from mail.* audit lines in the mesh stream | R2c | Applied as the default; not answered | TRACK.md:134,149 |
| U10 | How the two pushed eidolon-plugins branches reach main | P1b, SA1 | Rebase first | TRACK.md:193 |
| U11 | Push Minerva master | the move | Answered 2026-10-15, "push yes" (read as master only) | TRACK.md:192 |
| U12 | Post the eidolon `~` issue (fill in the sha) | upstream | The operator posts it | TRACK.md:109; drafts/ |
| U15 | Cascade a chat's tagged runs when the trash empties it, or keep them listed (current) | runs | none recorded | TRACK.md:203; reviews/RT/result.md |
| U16 | Push the 8 `worktree-*` branches and the 2 local tags, or carry a bundle | the move | Ask | see "Before the old machine goes" |
| U17 | Force-push dxcently/eidolon-plugins main to drop the trailers on ac402b5 and b678615 | — | Ask | HANDOFF.md rule 1 |
| U18 | Rust width: about 140 columns (specs/BRIEF.md:27) or about 100 (docs/Tasklist.md:300) | code | Match the file | HANDOFF.md rule 14 |
| U19 | A new chat defaults to yolo (perch/README.md:406), but the ship goal rules yolo out for shipping | shipping | Ask | HANDOFF.md rule 19 |
| U20 | Aoide node records yomi-strix and thinkchiyo-win (confirm or unpair; the stuck letter); whether native Windows aoided runs | mesh | Ask | plan/pass2-final.json:1337 |
| U21 | T2's four calls: (a) may an agent close a terminal the operator holds; (b) keys (up, ctrl-r, tab) the text guard never reads; (c) reads leave no trace on the operator's stream; (d) an operator share of the 16 terminals | T2 | (a) 423 while held; (b) keep, documented as unjudged; (c) document it; (d) 4 operator-only slots | specs/T2-fix4.md; reviews/T2/result.md |

U13 and U14 were working labels that never reached this table. Orchestrator calls flagged for the operator's review, none blocking:
TRACK.md:141,145,153,155,156,163,164,170,173,176,182,185,187,188,199,203,205; specs/T2-fix3.md:8; specs/T4a-fix2.md:9-12,33. TRACK.md:199
granted perch/Cargo.toml, which is on the never-edit list (specs/WAVE3.md:14). Operator-only: I1 live steps 2-5 on the real perch
(TRACK.md:68). Also the stale `~/.cargo/bin/aoide` 0.0.25 on the old box (TRACK.md:135).

### Flakes

| Test | What happens | Cite |
|---|---|---|
| perch `tests/runs.rs` | fixed ports 48140/48141 collide when two perch suites run at once. Fix: ephemeral ports via `--jev-health`/`--browser-health` | TRACK.md:100,196 |
| perch `tests/guard.rs` `nested_find_exec_and_repeated_flags_are_linear_and_bounded` | 60 s wall bound; 62-76 s under load or on a debug build; passes alone | TRACK.md:140,199 |
| perch `term::` lib test (unnamed), `watch::tests::a_sink_panic_is_logged_without_its_message` | 1 in 14 and 1 in 13. Keep the failing output next time | TRACK.md:173,178 |
| installer `perch_open` tests; `status/tests.rs` `fake_service` | fixed port 48110; ETXTBSY about 1 in 16 | TRACK.md:148,154 |
| page tests in a worktree | CRLF vendor files give 282/283. Restore byte-exact from the index; check with `git ls-files --eol` | TRACK.md:157 |
| `tests/common` `refused_start` | hangs if a refused start is served; bounded only on the R2c branch | TRACK.md:18,155 |
| perch `tests/install.rs:179-181` `calls()` | parses the fake's log while the child writes it ("EOF while parsing"). Fix: parse only newline-terminated lines | TRACK.md:206 |
| perch `tests/runs_log.rs:485` | reads the log, then the state, and misses records written in between. Fix: read the state first | TRACK.md:206 |

### Follow-ups (not slices yet)

- Perch:
  - AG attribution by SO_COOKIE (TRACK.md:66).
  - A1a runtime shutdown_timeout (TRACK.md:9).
  - R1a: the answer says 202 before eidolon's refusal lands (TRACK.md:170).
  - `--eidolon` bare PATH lookup (TRACK.md:74).
  - stream.rs:419 `(T2)` label (TRACK.md:196).
  - titles.rs:214 musl warning (TRACK.md:81).
  - M2 recv.rs:802 (TRACK.md:164).
  - Aoide audit.rs O_NOFOLLOW, upstream (TRACK.md:165).
  - Master's aoide `Shape::Version` is broken against real aoide 0.0.26, where `--version` exits 2; plus the mesh schema gate
    (TRACK.md:132,156).
  - RT review lows: an unknown chat's start walks `list()` (a `Registry::knows`, shared with the term twin); a stored session path is
    trusted without the run-folder check (TRACK.md:203).
- Guard: rows for reading perch-*.token and curl to /hub/install/keys/arm (TRACK.md:107); pin the AUTO verdicts of `~` file verbs
  (TRACK.md:110).
- Installer: clippy `-D warnings` fails on master, pre-existing; the fix is on the unmerged I2 branch (TRACK.md:76,152).
- Page:
  - one console 401 per login (/hub/sessions before /hub/redeem, TRACK.md:103);
  - the sidebar overlays the panes at narrow width (TRACK.md:14);
  - W3 residuals (TRACK.md:188-189).
- Docs (D1): stale doc lists (TRACK.md:75,80); remove whoami from the term and mail docs, for T2/M3 (TRACK.md:59,66).
- wiki-hop: no real end-to-end run yet. Fixtures and BotForge only (TRACK.md:195).

### Before the old machine went

Done on 2026-10-15:
- [x] `specs/RT-fix3.md`, `specs/T2-fix4.md` and the newer `reviews/T2/inventory.md` copied in; `reviews/RT/` and `reviews/T2/` hold
  their review results.
- [x] The old scratchpad path (it carried a Claude session id) is `<old scratchpad>` everywhere in this folder.
- [x] T2's review verdict recorded (TRACK.md:207): merge-after-fixes, not merged, U21.
- [x] The rook vault: `wiki/minerva-memory/` (31 notes) and the dated handoff note in `wiki/handoffs/`.
- [x] Build-Log, Decisions and Tasklist each got one dated entry pointing here.

Not carried:
- `aoide-main-since-0927.txt` (the copy was refused, TRACK.md:201). For U1, regenerate it from a clone of dxcently/Aoide:
  `git -C <Aoide clone> log --since=2026-09-27 --oneline origin/main`.

Left with the operator (see TRACK.md's last lines for what was answered):
- [ ] U16. The 2026-10-15 "push yes" was for master. The 8 `worktree-*` branches carry 0 attribution lines (observed, `master..branch`).
  To push them: `git push origin 'refs/heads/worktree-*:refs/heads/worktree-*'`. If not, re-bundle after T2 settles (TRACK.md:204 plans
  this): `git bundle create ../Minerva-archive/minerva-live-<date>.bundle master --branches='worktree-*' --tags` (composed). The
  2026-10-15 bundle holds RT 8065661, T2 df26ab2 and T4a c63f970, all older than the branches, plus the 36 deleted merged branches and
  no tags (observed `git bundle list-heads`: 45 heads). Tags `archive/extensions-2026-10-01` and `pre-sign-backup` exist only on the old
  machine; `pre-sign-backup` (82d9f2c) holds 5 commits on no branch (observed).
- [ ] Leftovers in the main checkout (observed, git status):
  - `bench/tasks/aeacus-loop-jev.task.tmpl`: a one-line content edit (jev reads 32 bytes per option). Commit it or revert it.
  - `devnull`, `err.log`, `out.log`: stray output from old runs.
  - `bin/PATH-backup-2026-09-25.txt`: a machine PATH backup. Do not commit it.

## Wrap-up before the wipe (2026-10-15)

- Merged on master: T2 4831852, FB 2560a7d (fresh-box docs: LF pins, prerequisites, Rust 1.88, release signing, WSL clone at ~/src/Minerva), T4a 251e55c, FL ca6b99c (the install.rs, runs_log.rs and runs.rs flakes).
- Master suite at ca6b99c: 939 pass, 2 fail, 1 ignored; clippy clean both ways. term_agent `another_chats_terminal_is_readable...` failed under load and passes alone (3/3). term.rs `the_answers_carry_no_environment_and_no_token` fails on this box every time: the shell screen shows "environ", likely from the WSL login MOTD (/etc/update-motd.d/50-landscape-sysinfo). The test is unchanged since 08c01fa, so this is environmental. Fix: start test shells without the login banner.
- Fresh-box audit (wf_68ce7483-735): a Linux dev box builds from a fresh clone today. A fresh Windows box cannot install from the installer: no GitHub release exists and installer/minisign.pub is a placeholder. Open, needing the operator: generate the minisign key (installer/README.md) and publish a release; record download sources and sha256 for the GGUFs in models.ini that setup-models.ps1 does not fetch; seed providers/hoot.rn from setup instead of a manual cp; re-tune models.ini on the new hardware; pin the llama.cpp tag.
- Sibling repos and local-only work were bundled into Minerva-archivecarry-2026-10-15 on the old PC (the Aoide nested repo, eidolon, eidolon-door, harnox, harnox-unix, eidolon-plugins and -sa1, WSL ~/src/aoide and ~/src/notes/eidolon, and patches of uncommitted work). That folder reaches the new machine only if the operator carries it.

## What to do first on the new machine

1. Confirm what arrived: `origin/main` at or past 4bcfa09, this folder tracked, and the slice branches. If they were not pushed, carry
   the whole old `Projects\Minerva-archive` folder (the slice bundle, eidolon-minerva-2026-09-21.bundle, extensions-2026-10-01/) and run
   `git fetch <dir>/<bundle> 'refs/heads/worktree-*:refs/heads/worktree-*' 'refs/tags/*:refs/tags/*'`. Not `refs/heads/*`: the bundle
   also holds master, and git refuses to fetch into the checked-out branch (reasoned).
2. Set up the machine per HANDOFF.md, "Set up a fresh machine". Clone outside `~/Minerva`: that path is the Minerva home
   (perch/README.md:62-65).
3. Rebuild the worktrees. `.claude/worktrees/` is ignored only by the old clone's `.git/info/exclude:9`, which does not travel
   (.gitignore has no entry). Run `printf '.claude/worktrees/\n' >> .git/info/exclude`, then
   `git worktree add .claude/worktrees/<name> worktree-<name>` for each live branch. On Linux, pass the same path as `worktree` and `wsl`.
4. Measure master yourself; do not inherit counts (HANDOFF.md §5, "Measure counts"):
   - the perch suite with `--features test-bins`, and clippy both ways;
   - the page suite.
   As recorded: perch 919 pass, 0 fail, 1 ignored at 4bcfa09 (TRACK.md:204), and page 396/396 at 5aa2a19 (TRACK.md:190). Linux node has
   never run the page suite.
5. Fix the paths in `workflows/` and `specs/` (the list above). Re-check X1's memory bound on this box (TRACK.md:54).
6. Start GP-a (TRACK.md:190,204): it waits on nothing. Then finish the two in flight:
   - T2: get U21a-d answered, rebase, run specs/T2-fix4.md, then merge **before RA** (TRACK.md:179).
   - T4a: rebase, run specs/T4a-fix2.md.
7. Take the open decisions to the operator, in the order they unblock work:
   - U1 (the mail path and the finish);
   - U5-U8 (RA, PR, GW; U8 closes a hole on master today);
   - U2, U3, U4;
   - U21 (T2), U10, U15-U20.
8. Work the queue in the status table. Merge order: R2c before M2 (TRACK.md:156), and T2 before RA (TRACK.md:179).

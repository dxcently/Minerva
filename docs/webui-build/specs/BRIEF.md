# Executor brief: Minerva web UI build (read this first, then your slice spec)

You implement ONE slice of a 49-slice plan, in your own git worktree of C:\Users\dxcen\Projects\Minerva (your cwd), then test it, commit it on your worktree's branch, and report. The orchestrator reviews, runs the browser-pane live checks, and squash-merges.

## Step 0, before reading or editing anything
Your worktree was cut from origin/main, which is STALE (an ancestor of master). Run from Git Bash in your worktree: `git merge --ff-only master` (non-destructive; do NOT use `git reset --hard`, the sandbox refuses it), then `git log --oneline -1` and confirm it matches `git rev-parse --short master`. If the fast-forward is refused, STOP and report; never work on the stale base. All work happens on master's code. Never touch the master branch itself.

## Where things are
- Your spec: `scratchpad\slices\<ID>.json`. The whole plan: `scratchpad\pass2-final.json` (read the slices that consume or feed yours). The scratchpad is <old scratchpad>
- Merged so far on master: J1 (jev /health: decisions_log, ckpt, loaded, openjev_dtype; JEV_DECISIONS_LOG=off; OPENJEV_DTYPE), S0W-a (core/bits.js Needs/Soon/Kv; panes/loaded.js LoadedTab; panes/diff.js DiffTab; graph/index.js GraphTab; mesh/index.js MeshTab; core/overlay.js overlay/openOverlay/closeOverlay/Overlay; core/api.js HUB_PATH/hubPathOk; core/lane.js hubPoll/hubPollText/LANE_TIMEOUT_MS; webui/term/tests with `node --test "webui/term/tests/*.test.mjs"`), V0 (vendor/dagre.js bundles its own graphlib as dagre.graphlib, vendor/graphlib.js also present; vendor/xterm.js exports Terminal; vendor/xterm.css; vendor/addon-fit.js exports FitAddon; update.sh --add/--check), S0W-b (core/registry.js: rightTabs/leftTabs, paneKinds, splitExtras, chatMenuItems, commands, keyRows, loadedSections, composerExtras, keyOwners/focusOwners/whereOwners, frameTaps + runTaps, extraStreams/chatCap, host {toast, openXPane, closePane, focusPane, focused, inspected, chatPanes, xpanes, cwd, showTab, chatStreams, openCmd}; stubs terms/index.js, notes/index.js, allow/list.js), W2c (graph/project.js project(); graph/draw.js layout()/drawGraph()).
- Line endings: the main checkout has core.autocrlf=true, so sources there are CRLF while your worktree may be LF. A test that reads a source file and searches for '\n' must normalise first (`.replace(/\r\n/g, '\n')`), or it passes for you and fails on master.
- The eidolon-plugins upstream checkout is C:\Users\dxcen\Projects\eidolon-plugins (branch jev-wiki-hop-max holds P1: wiki-hop snapshot max 100). The vendored copy in Minerva is eidolon-plugins/ (do not hand-edit it).

## Operator decisions (all settled; apply, never re-ask)
- D1 wiki-hop snapshot max 100. D2 jev-base-v0 + openjev float32 under a 20 GB cap, auto-fallback to bf16 (OPENJEV_DTYPE) if >16 GB. D3 runs detach; a restarted perch adopts them; terminals end with the perch. D12 the mesh may read the real ~/.aoide read-only (demo only, after X0 proves `mail outbox --json` changes nothing).
- NO jev decision-log GATE: nothing refuses runs over decisions_log; the graph tab only SHOWS the state (overrides the plan's R1a gate text).
- D4 key add/remove from the page is armed by `hoot keys allow-web <FP>` in a terminal (one add/remove of that FP within 120 s); fingerprint always typed back.
- D5 agents get term_open (max 4 per chat, in the chat's cwd) and term_close (own terminals only), besides term_list/read/send; reach for read/send is ANY web terminal.
- D6 manual: all term tools ask (with credential pre-grants); auto: term_send asks, list/read/open/close vouched; yolo: all run.
- D9 guard rows in all modes (minerva-guard.rn beside the member's policy): deny agent-run aoide, bash ssh (-> use term_open + term_send), minerva keys add/remove / hoot keys / --keys-arm.
- D10 allow rules are memory-only, bound to the door (end on any door stop, mode or model switch, perch restart). Narrowings: no name rule from a structural ask; a rule answers only its own origin.
- D11: terminals use system mono (Cascadia Mono, Consolas, monospace); 3 chat streams + 1 terminal stream (extraStreams); `:term` opens a web terminal and the native launcher moves to `:window term`; a 'Leave site?' guard only while focus is in a terminal; when the focused chat's agent opens a terminal the right rail switches to ⌨ once (never moves keyboard focus).
- xterm 6 has window.open for OSC 8 links when linkHandler is null: the terminal slices must set a linkHandler (no auto-open).

## Hard rules
- Touch only the files your spec owns (plus anything this brief or your prompt explicitly allows). If you need another file, stop and report.
- Rust: hand-formatted at about 140 columns; NEVER run rustfmt / cargo fmt. Match the surrounding code's naming, comment density and idiom. Every new folder gets a README.md like its siblings.
- Page: Preact 10 + @preact/signals + htm, vendored ESM, no build step, no npm at runtime; CSP script-src 'self' (no inline script, no eval, no innerHTML); TUI look, square edges, bracketed text actions `[ like this ]`, mouse-first, keys are shortcuts. Read app.js and the core/ files before editing.
- Perch builds/tests run in WSL through /mnt/c (your worktree path converted), with CARGO_TARGET_DIR=$HOME/.cache/minerva-slices-target (disk; WSL /tmp is tmpfs and eats RAM the jev demo needs); NEVER ~/perch-target, never a target dir under /tmp. Use PowerShell `wsl -e bash -lc '...'` if the Bash tool refuses wsl. In Git Bash, set MSYS_NO_PATHCONV=1 before passing /mnt/... paths to wsl.exe.
- Page tests: `node --test "webui/term/tests/*.test.mjs"` (quoted glob) with Windows node; pure modules only, no DOM. Each test must fail if the change it guards is reverted (check by hand once).
- Isolation: never touch the operator's perch on 127.0.0.1:47820, ~/Minerva, ~/.config/eidolon, ~/.aoide (writes). Any eidolon run: temp HOME and XDG_CONFIG/DATA/STATE/CACHE/RUNTIME_DIR, swarm off, `--provider mock` / `-m mock` only, no provider credentials; prove mock before any send. Your own test perch only on your slice's port (see the plan's PORTS list). No model calls, ever. Tokens and codes never go in argv, env dumps, logs or your report.
- You have no browser: skip browser-pane live checks and instead list the exact steps for the orchestrator.
- Git: run git from Git Bash on Windows (WSL git cannot read a Windows worktree's .git file). core.autocrlf=true; keep files as the repo stores them. When tests pass, commit everything on your current branch with ONE message in the repo's style (`area: what changed, plainly`, short body). NO Co-Authored-By line and no Claude/AI attribution of any kind. Do not push. Do not touch master.

## Report (your final message goes to the orchestrator, not the user)
Branch and sha (and its base = master's sha); files added/changed; exact test commands with pass/fail counts; clippy result for Rust; any deviation from the spec and why; what later slices must know (exports, signatures, state types, hook names); the browser-pane live-check steps for the orchestrator.

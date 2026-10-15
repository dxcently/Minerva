# Wave 2 rules (read after BRIEF.md; these override it where they differ)

- Step 0: `git merge --ff-only master` in your worktree (the sandbox refuses `git reset --hard`), then confirm `git log --oneline -1` equals `git rev-parse --short master`. If the fast-forward is refused, STOP and report.
- Master now holds, besides wave 0: S0P-a and S0P-b (perch seams, tests/common harness, a stub test file per slice, fake hooks), F0 (jev fixtures in perch/tests/fixtures/jev and webui/term/tests/fixtures/jev, with a README "Record shapes" section you must read if you touch runs or journals), R3 (/hub/meta), R2a (src/aoide runner), N1 (/hub/notes). AG, T1a, A1a, I1 and G1 are under security review and NOT on master yet; do not depend on them.
- Read S0P-a.json and S0P-b.json in the slices folder for the seams, and in pass2-final.json the slices that consume yours.

## Perch slices
- Use a PRIVATE target dir: CARGO_TARGET_DIR=$HOME/.cache/minerva-<your slice id>-target (on disk). The shared one is raced: another slice's build replaces target/debug/perch mid-test.
- Offline only. WSL PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin.
- NOT yours, never edit: tests/common/mod.rs, tests/http.rs, Cargo.toml, Cargo.lock, src/bin/fake_eidolon.rs (its submodules only where your spec lists them), src/lib.rs, src/main.rs, src/serve.rs. Replace only the stub pin in your own test file. If you need a new harness helper, a fake hook point, an option flag, a route arm or a dependency, STOP and report what and why.
- Run your own tests while iterating, the whole suite once at the end (`--features test-bins`), and `cargo clippy --features test-bins --offline --all-targets -- -D warnings` clean.
- Fake perch for live checks: `PORT=<your port> bash perch/tests/live/fake-perch.sh` with PERCH_BIN / FAKE_EIDOLON_BIN pointing at copies of YOUR binaries in a private dir; `--stop` before you report.

## Page slices
- No perch build needed. For a fake perch, copy master's verified binaries first: `mkdir -p $HOME/.cache/<slice>-bin && cp $HOME/.cache/minerva-master-target/debug/{perch,fake_eidolon} $HOME/.cache/<slice>-bin/`, then `PERCH_BIN=... FAKE_EIDOLON_BIN=... PORT=<your port> bash perch/tests/live/fake-perch.sh` from your worktree (its --ui-dir is your worktree's webui/term). You have no browser: use curl for the routes and list the browser-pane steps for the orchestrator.
- Tests: `node --test "webui/term/tests/*.test.mjs"` (quoted glob). Pure modules only. A test that reads a source file must normalise `\r\n` first (the main checkout is CRLF).
- app.js has no owner any more: never edit it. Plug in through core/registry.js (tabs, panes, commands, key rows, loaded sections, composer extras, frame taps, host). If the registry lacks a seam you need, STOP and report.

## Everyone
- Write big files in several smaller Write/Edit calls (a stream stall kills the turn).
- Your port only (see your prompt); never 47820, 8090 or 8091.
- One commit, repo style, no Co-Authored-By or any AI attribution. Do not push. Report per BRIEF.md.

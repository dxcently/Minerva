# Wave 3 rules (read after BRIEF.md; these override it and WAVE2.md where they differ)

- Step 0: `git merge --ff-only master` in your worktree (the sandbox refuses `git reset --hard`), then confirm `git log --oneline -1` equals `git rev-parse --short master`. If the fast-forward is refused, STOP and report.
- Master now holds wave 0, S0P-a/b, F0, R3, R2a, R2b (/hub/mesh), N1 (/hub/notes), N2a (notes in the page), I1 (installer --json + key arm), AG (agent gate: scoped term/mail tokens), R1a (/hub/runs: start, answer, cancel, abandon, health gate), W1, W2a, W5. T1a, A1a, G1 and R4 are NOT on master yet; do not depend on them.
- Read S0P-a.json and S0P-b.json in the slices folder for the seams, and pass2-final.json for the slices that consume yours. If you touch runs or journals, read perch/tests/fixtures/jev/README.md "Record shapes".
- The test harness (tests/common/mod.rs) now gives every perch it starts a temp HOME. Nothing a test runs may read or write the operator's ~/.aoide, ~/Minerva or ~/.config: name temp paths for every option that defaults under ~.

## Disk guard (the disk filled once today)
- Before every cargo build or test run: `df -h /mnt/c | tail -1`. If C: shows under 6G available, STOP and report. Never delete anything to make room.
- Never let a command's output grow without bound: pipe long test output through `tail`/`grep`, never into an unbounded file.

## Perch slices
- PRIVATE target dir: CARGO_TARGET_DIR=$HOME/.cache/minerva-<your slice id>-target. Offline only. WSL PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin (add $HOME/.local/bin only if your spec needs the real eidolon).
- NOT yours, never edit: tests/common/mod.rs, tests/http.rs, Cargo.toml, Cargo.lock, src/bin/fake_eidolon.rs (its submodules only where your spec lists them), src/lib.rs, src/main.rs, src/serve.rs. Replace only the stub pin in your own test file. If you need a new harness helper, fake hook point, option flag, route arm or dependency, STOP and report what and why.
- When your route stops being a 404, a pin in serve.rs's tests may go red (`every_seam_is_a_404_with_the_token_and_a_401_without_it`, `a_declared_length_over_the_cap_is_a_413_before_a_module_is_asked`). Do not edit serve.rs: name each red pin, its assertion and the exact fix in your report; the orchestrator applies it at merge.
- Run your own tests while iterating, the whole suite once at the end (`--features test-bins`), and `cargo clippy --features test-bins --offline --all-targets -- -D warnings` clean.
- Fake perch for live checks: `PORT=<your port> bash perch/tests/live/fake-perch.sh` with PERCH_BIN / FAKE_EIDOLON_BIN pointing at copies of YOUR binaries in a private dir; `--stop` before you report.

## Page slices
- No perch build needed. For a fake perch, copy master's verified binaries first: `mkdir -p $HOME/.cache/<slice>-bin && cp $HOME/.cache/minerva-master-target/debug/{perch,fake_eidolon,fake_aoide} $HOME/.cache/<slice>-bin/`, then run fake-perch.sh from your worktree (its --ui-dir is your worktree's webui/term). You have no browser: use curl for the routes and list the browser-pane steps for the orchestrator.
- Tests: `node --test "webui/term/tests/*.test.mjs"` (quoted glob). Pure modules only. A test that reads a source file must normalise `\r\n` first.
- app.js has no owner: never edit it. Plug in through core/registry.js. Periodic polls go through core/lane.js; person actions use hubGet. If the registry lacks a seam you need, STOP and report.

## Everyone
- Write big files in several smaller Write/Edit calls (a stream stall kills the turn).
- Your port only (see your prompt); never 47820, 8090 or 8091.
- One commit, repo style, no Co-Authored-By or any AI attribution. Do not push. Report per BRIEF.md.

# Wave 1 rules (read after BRIEF.md; these override it where they differ)

- Master is 95c2b2b: S0P-a (perch seams, every route arm and module stub) and S0P-b (tests/common harness with Perch::builder, a stub test file per slice, fake_eidolon hooks, portable-pty + alacritty_terminal in the lock, tests/live/fake-perch.sh) are merged. Step 0 is `git merge --ff-only master` and checking the sha (see BRIEF.md).
- Read S0P-a.json and S0P-b.json in the slices folder for the seams you sit on, and in pass2-final.json the slices that consume yours.
- Up to eight other slices build at the same time, all with CARGO_TARGET_DIR=$HOME/.cache/minerva-slices-target. Cargo will sometimes print "Blocking waiting for file lock"; that is expected. Give cargo calls a timeout of up to 600000 ms and filter their output (`2>&1 | grep -E '^test result|FAILED|panicked|^error' | head -40`). While iterating, run only your own tests (`--test <file>` or `--lib <module>::`), and run the whole suite once at the end (master: 317 pass with --features test-bins).
- Offline only (`--offline` on every cargo command). WSL PATH=$HOME/.cargo/bin:/usr/local/bin:/usr/bin:/bin.
- NOT yours, never edit: tests/common/mod.rs, tests/http.rs, Cargo.toml, Cargo.lock, src/bin/fake_eidolon.rs (its submodules are yours only where your spec lists them), src/lib.rs, src/main.rs, src/serve.rs. In your own test file you replace the stub pin. If you need a new harness helper, a fake hook point, an option flag, a route arm or a dependency, STOP and report exactly what and why; do not work around it.
- Avoid stream stalls: write a big file in several smaller Write/Edit calls, never one huge one.
- At the end, `cargo clippy --features test-bins --offline --all-targets -- -D warnings` is clean.
- Fake perch for live checks: `PORT=<your port> bash perch/tests/live/fake-perch.sh` (FAKE_PERCH_ARGS and FAKE_PERCH_ENV for extra flags and env; `--stop` to stop it). Only your slice's port. Stop it before you report.
- Security-review slices (AG, T1a, A1a, I1, G1): an Opus reviewer reads your diff before merge. In your report, list every input you accept (paths, headers, bodies, env, files) and how each is bounded or refused, and every secret you handle and where it can and cannot appear.
- One commit, repo style, no Co-Authored-By or any AI attribution. Do not push.

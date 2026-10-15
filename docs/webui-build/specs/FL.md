# FL: three perch test flakes (written 2026-10-15; test code only)

Base: master 1c8cbd9. Fresh worktree. Three perch tests fail now and then for reasons in the test, not the perch. Fix each in the
test, so the test still fails when the behaviour it pins breaks. No file under perch/src changes.

## Fixes

1. perch/tests/install.rs:179-181, `calls()`. It parses every line of the stand-in's log while the stand-in may still be writing
   one, so a half-written last line panics ("EOF while parsing"). Parse only newline-terminated lines: a trailing piece with no `\n`
   is a line still being written and is left out. A complete line that is not JSON still panics as now.
2. perch/tests/runs_log.rs, about :474: the loop polls the log, then asks the run's state. A record written between the two reads is
   missed by that batch, and then `state == "done" && batch.is_empty()` can end the loop before it is read. Ask the state BEFORE the
   log poll, so "done, and this batch is empty" means the journal was at rest when the batch was read. Keep `mid_run` honest: it
   counts batches that arrived while the state asked just before them was still "running". Keep the asserts (every record once, in
   order; mid_run >= 3; five polls at rest cost at most one ask).
3. perch/tests/runs.rs: the health tests bind fixed ports 48140 and 48141, so two perch suites at once (two worktrees) collide. Bind
   each stub on 127.0.0.1:0 and pass the port it got to the perch through `--jev-health` / `--browser-health`. The "down" test
   needs a port nothing listens on: bind :0, note the port, drop the listener, then use it. The host-header assert at :620 checks
   the stub's real port. The `ports()` mutex goes if nothing needs it any more; the module doc at :10-11 says what is true after.
   The non-loopback test (:677) keeps its literal addresses (it never connects).

## Checks

- Each fix: say how you showed the old code could fail and the new one does not (a short loop of the one test, e.g. 20 runs, is
  enough; a forced interleaving is better if it is cheap). Never leave a loop running.
- The whole perch suite once with `--features test-bins`, clippy clean both ways (`--all-targets -- -D warnings`, with and without
  `--features test-bins`).
- perch/tests/README.md: rows for install, runs_log and runs say what is now true, if they mention the ports or the polling.

## Grants

perch/tests/install.rs, perch/tests/runs_log.rs, perch/tests/runs.rs, perch/tests/README.md (those rows only). Never edit perch/src,
tests/common/mod.rs, Cargo.*, or anything outside perch/tests; never rustfmt. One commit, repo style (`perch: tests: ...`), no
attribution. CARGO_TARGET_DIR=$HOME/.cache/minerva-FL-target. Port for anything you start yourself: 48150.

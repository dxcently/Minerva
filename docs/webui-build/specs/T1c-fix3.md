# T1c fix round 3 (orchestrator's decisions on the round-2 verifier's last gaps)

T1c is at 1e5ba72 (7a0baea, 28e6f4f, 1e5ba72 on base b08a32e) in worktree wf_d29df11e-e42-1. T1c.json still holds where this file
is silent; where they differ, this file wins. The live check runs on 48138 (not the plan's 48114): accepted. The orchestrator squashes
at merge, so the commit count does not matter.

## Decisions

1. DECSTBM: option A. A join rebuilds the scroll region. GRANT perch/src/term/screen.rs, narrowly: on the feed path where the Guard
   already sees every byte (pty.rs:1185-1187 feeds the same bytes to the screen and the ring), record the last `CSI Pt;Pb r`; a bare
   `CSI r`, RIS (`ESC c`) and DECSTR (`CSI ! p`) clear it; expose it on Emu. snap::encode writes it after the modes and before the
   rows, and re-homes the cursor after it as DECSTBM moves the cursor. A snap.rs test feeds `ESC[2;4r` plus text and round-trips the
   region; a second test checks that RIS clears it. First CONFIRM the Guard passes `CSI r` through unchanged and that alacritty does
   not swallow it before you see it; if either is false, STOP and report with evidence (do not invent another path).
2. Underline style and underline colour: write them in the snapshot if alacritty 0.26 exposes them on the cell (the flags for
   double, curly, dotted, dashed underline and the cell's underline colour): `CSI 4:n m` and `CSI 58:...m`, with a snap.rs round-trip
   test. If it does not expose them, say so in the snap.rs module doc and perch/src/term/README.md residuals with the evidence.
3. The two pins, GRANTED to you this round (WAVE3 lets the orchestrator apply them; the orchestrator grants them here so the suite
   is green before review), exactly:
   - perch/src/serve.rs:1552: delete the row `("GET", "/hub/term/events"),` in every_seam_is_a_404_with_the_token_and_a_401_without_it,
     and after that loop add `assert_eq!(call(&perch, "GET", "/hub/term/events", "", "").await.0, 401, "the stream without it");`
     (match the test's own helper names; read the test first).
   - perch/tests/term.rs:406-407: the comment reads "// `a` is never an id: a bare `a` is nobody's." and the list drops
     "/hub/term/events" (ids_that_are_not_terminals_are_the_empty_404).
   No other line in either file. serve.rs:1672 and tests/agent.rs:524 pass unchanged: leave them.
4. Accepted as built, documented already: relist drops a closed terminal's unflushed output on a lagged channel; the snapshot seq
   ignores bytes the parser still holds.

## Fixes

5. perch/src/term/README.md:400 states the flood size the code uses (read stream.rs's constants and the test that floods).
6. The flake: `term::` lib tests failed once in 14 runs (name not captured). Run `cargo test --lib term:: --features test-bins` 20
   times, each run's output through `grep -E "^test .* FAILED|panicked"`, and name the test and its rate. If the cause is in T1c's
   files, fix it with a test that holds; if not, report the name, the rate and the file it lives in. Also report whether
   watch.rs:2668 (A1a's, seen once by the round-2 fixer) fails in your full runs.
7. Never write a login URL or one-time code to any file, even 0600 and even briefly (round 2 did; it was disclosed and cleaned).
   Redeem by piping the code from the command that prints it straight into the redeem call's stdin, or skip the cookie leg.

## Grants (exactly these, beyond T1c.json's owns_files)

perch/src/term/screen.rs (item 1 only), perch/src/serve.rs:1552 and the one added assert (item 3), perch/tests/term.rs:406-407
(item 3), perch/src/term/README.md, perch/tests/README.md rows. Never edit tests/common/mod.rs, tests/http.rs, Cargo.*, lib.rs or
main.rs; never rustfmt.

One new commit on top of 1e5ba72, repo style (`term: ...`), no attribution. Whole perch suite once with --features test-bins (no
--skip: it must not hang); clippy clean both ways; the live check on 48138 again for a join that rebuilds a scroll region (a
terminal that set `ESC[2;10r`, then enough output that the ring no longer holds byte 0 (use the same small-ring knob
a_small_ring_rebuilds_a_mode uses, if the perch takes it at start; else say the live leg is covered by the integration test only),
then a fresh stream: a term-reset with snap true whose data carries `ESC[2;10r`), then --stop.

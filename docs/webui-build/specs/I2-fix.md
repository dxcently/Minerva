# I2 fix round 3 (orchestrator's decisions on the verifier's last gaps)

I2 is at aff2f3b (a4f7b5b, eb5c921, aff2f3b) on base d8c5353 in worktree wf_1f64c6c7-a31-1. Installer tests 117/117, clippy
clean. The verifier's last round left two gaps outside I2's grants and the fixer raised D1-D5. Decisions:

- D1 accepted: revoke_fp is a sibling field of revoke ({id, fp} objects), so an installer built before it still reads revoke.
- D4 accepted: the stub binds a port the OS picks per process (no fixed 48110).
- D2/D3 decided (stricter, fail closed): a signed rotation whose `revoke` names an id with no fingerprint in `revoke_fp`
  is REFUSED whole, with the sentence "keys.json revokes <id> without its fingerprint; the maintainer lists every revoked
  key in revoke_fp", unless this machine holds that id's key (then its fingerprint is computed locally and recorded, as
  now). Nothing is committed from a refused rotation (serial unchanged). A revocation record with no fingerprint can then
  only come from a state.json written before I2 (development installs; nothing was released): keep the id-only match for
  those and say so in the README as the one residual. installer/README.md's operator step 2 says revoke_fp is required
  and what a client does without it. Tests: a rotation revoking a never-held id without fp is refused and commits
  nothing; with fp it is accepted and the same bytes under a fresh id are refused by add_manual; a held id without fp is
  recorded with the locally computed fingerprint.

New grants for this round (exactly these, beyond I2's own):
- bin/perch-open.sh, the arm's status case only (around :194-199): keep the reply body; a 404 whose body has
  "kind":"nix" prints "perch-open: managed by Nix: update through nix. A Nix install's update keys are not changed from a
  terminal or the page; nothing was armed" and exits 1; every other 404 keeps today's sentence. A test case in
  installer/tests/perch_open.rs (the_arm_says_what_the_perch_answered) for 404 kind nix.
- perch/tests/install.rs calls() (:179-182): parse only lines that end in a newline
  (split_inclusive('\n') filtered by ends_with('\n')), so a line the fake is still writing is never parsed.
- perch/src/bin/fake_minerva.rs:279: write each log line with one write_all of the whole line plus "\n".
  Run perch/tests/install.rs 5 times in a row (CARGO_TARGET_DIR=$HOME/.cache/minerva-I2-target, --features test-bins):
  31/31 each.

Then the security inventory (review-I2/inventory.md) is updated for the rotation refusal and the Nix arm sentence.

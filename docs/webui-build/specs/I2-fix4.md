# I2 fix round 4 (orchestrator's decisions on round 3's open points)

I2 is at e072d5e (a4f7b5b, eb5c921, aff2f3b, 4c4bc11, 08025ee, e072d5e) on base d8c5353 in worktree wf_1f64c6c7-a31-1.
Installer 120/120, perch install.rs 31/31 x5, clippy clean both. The round-3 verifier left one README gap and three decisions.
I2-fix.md still holds for everything not changed here; where they differ, this file wins.

## Decisions

A. Whole-file refusal STAYS on the client (keys.rs:780). The malformed file is stopped before any client sees it instead:
   `installer/release-files.sh` refuses (exit 1, one sentence naming the id, nothing written) when DIR/keys.json has an id in
   `revoke` with no entry in `revoke_fp`, and also when it cannot read the two lists at all (fail closed: a keys.json it cannot
   parse is refused, never waved through). No new dependency (no jq, no python): POSIX sh + sed/tr/grep, tolerant of whitespace and
   newlines in the JSON; ids compared case-insensitively. The check runs in both modes (build and `resign`).
   README operator step 2: add that a refused file changes nothing on that machine, so a leaked key it holds STAYS trusted there until
   a corrected file is out, and that release-files.sh refuses such a file first.
B. Record fingerprint fallback APPROVED. In plan_rotation, the fingerprint of a revoked id is: the file's revoke_fp; else this file's
   adds; else the keys dir or compiled-in key; else the fingerprint already in this machine's revocation record for that id (state.json,
   written only after a verified file). Only when all four are absent is the file refused. This covers a re-listed revocation and the
   crash window (state saved before the .pub removal and the serial). Test beside keys.rs:1098: serial 1 revokes a never-held id WITH
   revoke_fp; serial 2 re-lists it with no revoke_fp; accepted, the serial moves, no second revoked entry, the fingerprint is kept,
   and the same bytes under a fresh id are still refused. keys.rs:1098 still refuses (no record entry there).
C. The descendant case is ACCEPTED as the second residual (no code change). Reason: its bytes left this machine's keys dir, which only
   this user can write, and whoever can write there can trust any key directly. Verify every route by which a descendant's .pub can
   be gone (by hand; `minerva keys remove`, keys.rs:546; anything else) and name them. installer/README.md 'What add refuses'
   (:241-246) names BOTH residuals as accepted, each with what closes it (a signed file that names its fingerprint in revoke_fp). The
   inventory keeps R1(b) and Risk 2, reworded to match.

## Also

- Inventory: the M1 panic cite is keys.rs:1107 at HEAD (table says 1106); every line cite re-checked against the final HEAD.
- installer/README.md 'Releasing' and the release-files.sh header say what the new check refuses.

## Grants (exactly these, beyond I2's own and I2-fix.md's)

- installer/release-files.sh (the check and its header comment).
- A new installer/tests/release_files.rs (runs the script on temp dirs: revoke with fp passes; revoke without fp refused, nothing
  written; mixed-case ids; multi-line JSON; an unreadable list refused; no keys.json passes as today; `resign` mode also checks), plus
  its row in installer/tests/README.md.

One commit on top of e072d5e, repo style, no attribution. Never merge master; stay on base d8c5353.

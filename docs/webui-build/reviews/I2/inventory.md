# I2 security inventory: installer key hardening (round 4, with its verifier fixes)

Branch worktree-wf_1f64c6c7-a31-1, base d8c5353. Commits:
- eb5c921, aff2f3b (rounds 1-2); 4c4bc11 (round 3); 08025ee (round 3 fix); e072d5e (the Releasing paragraph).
- ef66078 (round 4): release-files.sh refuses a keys.json that revokes without its fingerprint; plan_rotation keeps the record's
  fingerprint for a revocation listed again; installer/tests/release_files.rs; the README and tests README for both.
- aba26bb (the round 4 fix after the verifier): Trust::remove keeps the key's `.json`; plan_rotation refuses a fingerprint that
  this machine's record contradicts (a given one, a held key's, a descendant's); release-files.sh says it is bash and what its check
  reads; release_files.rs: a file with neither list passes; the README and tests README for all of it.
- 33fae11 (HEAD, round 4 fix 2: the README gaps the verifier found): installer/README.md only. The paragraph after "Every id in
  `revoke` needs its fingerprint" says a recorded fingerprint is kept, wrong or not, and how to get out; step 2 says a client with a
  record of the fingerprint accepts a file without revoke_fp. No code changed in this commit.

Every cite below is checked against the final HEAD (33fae11). The code files (keys.rs, release-files.sh, the tests, perch) are the
aba26bb ones; `git diff --stat ef66078 HEAD` lists only installer/README.md,
installer/release-files.sh, installer/src/keys.rs, installer/tests/README.md and installer/tests/release_files.rs, so the cites in
every other file are the ef66078 ones, still valid. A mutation that was not re-run this round says "round N, not re-run".

Scope: the trust root of `minerva update` (installer/src/keys.rs and its callers, the keys verbs in installer/src/bin/minerva.rs,
nix/minerva.sh arm-5, bin/perch-open.sh's arm status, and installer/release-files.sh's keys.json check). perch/ product code is
unchanged in these rounds; the perch edits remain the test stand-in (perch/src/bin/fake_minerva.rs) and the test reader
(perch/tests/install.rs).

## Round 4 decisions and where each is applied (HEAD aba26bb)

Decision C's spec clause ("ACCEPTED (no code change)", I2-fix4.md:22) is superseded by aba26bb, which keeps the `.json` on removal and
adds the contradiction refusals (rows C and 20, and the fix row). The orchestrator records that at merge.

| Decision | Where (aba26bb) | Test that guards it | Mutation evidence |
|---|---|---|---|
| A. release-files.sh refuses DIR/keys.json, in BOTH modes and before anything is written, when (1) an id in `revoke` has no `revoke_fp` entry (ids compared without case); (2) a `revoke_fp` fingerprint is not 16 hex digits once spaces are removed; (3) the `revoke` or `revoke_fp` list cannot be read (a non-string or malformed entry, an unclosed array, a name given twice, a `revoke_fp` object that is not exactly an id and a fp); (4) the file is not one JSON object (its outer braces). Exit 1, one sentence, nothing written. It reads two lists and the outer braces; it is NOT a JSON validator (fix round 4: the header and the README say so). | installer/release-files.sh:9-14 (header: the bash note and the limit), :33-36 (refuse_keys), :37-69 (the check: list reads :38-49, revoke ids :50, revoke_fp objects :52-63, the missing-fingerprint refusal :67). It runs before the mode branch, so `resign` runs it too. | installer/tests/release_files.rs: a_revocation_with_its_fingerprint_passes_and_the_release_files_say_the_serial; a_revoked_id_without_its_fingerprint_is_refused_and_nothing_is_written; ids_are_compared_without_case_and_the_json_may_span_lines; a_list_the_script_cannot_read_is_refused_and_nothing_is_written (10 cases); no_keys_json_passes_as_it_always_did; a_keys_json_with_neither_list_passes (new, round 4 fix); resign_runs_the_same_check_and_writes_nothing_on_a_refusal | M-A (round 4): the :67 id check and the revoke-list regex replaced by `true`, one run: the id, unreadable-list, mixed-case and resign tests FAIL. Round 4, not re-run this round (the check is unchanged; only the header comment moved). |
| B. plan_rotation's fingerprint for a revoked id: the file's revoke_fp; else this file's adds; else the keys dir or compiled-in key; else the fingerprint the record already has for that id. Only when all four are absent is the file refused, whole. Round 4 fix: a fingerprint that the record contradicts refuses the whole file (see the next row). | keys.rs:750-755 (the rule, in the comment), :758 (`recorded` closure), :778-803 (the match: :779 a given and a held fingerprint that differ; :783 a given one; :792 a held one; :803 the `(None, None)` arm, which takes the record's fingerprint or refuses), :805 (push_revoke), :857-858 (commit keeps the record's fingerprint when it has one). | keys.rs:1149 a_revocation_listed_again_without_its_fingerprint_keeps_the_one_it_was_given (serial 2 accepted with the record's fingerprint, one record entry, the same bytes under a fresh id refused, the crash window); keys.rs:1126 a_rotation_that_revokes_a_key_this_machine_never_held_needs_its_fingerprint (a refusal where the record has no entry) | M-B (round 4 mutant: the `(None, None)` arm always refuses): test 1149 FAILS at keys.rs:1159. M1 (round 4 mutant: the record fallback removed; round 4 deleted a fall-through arm that no longer exists, so the same mutant is now `(None, None) => None`): test 1126 FAILS at keys.rs:1135. Both re-run this round, restored byte for byte (cmp). |
| C. The descendant residual. A key a revoked key added, whose `.pub` is no longer on this machine, is blocked by id when its `.json` still names the parent (which `keys remove` now keeps; round 4 fix), and its bytes under a fresh id are accepted. A key whose `.json` is also gone is not revoked at all (route 10). No code change for the bytes; the id is recorded. | keys.rs:809 (`fp_of(&d)`: None once the `.pub` is gone), :868-872 (commit saves the record, then removes the `.pub` and `.json` of every planned revoke, descendants included), :547-558 (Trust::remove: the `.pub` goes, the `.json` stays). The routes table is next. | keys.rs:1476 a_key_a_revoked_key_added_keeps_its_fingerprint_only_while_its_bytes_are_held (HELD recorded with its fingerprint; GONE by id alone); keys.rs:1496 a_descendant_removed_by_trust_remove_is_still_revoked_by_id (new, round 4 fix: the removal `keys remove` does; the id is recorded when the parent is revoked; the same id refused) | M-C (round 4, keys.rs:809 `let fp: Option<String> = None;`): tests 1476 (FAILS at keys.rs:1489) and 1212 (FAILS at keys.rs:1226). Re-run this round. M-D (round 4 fix; the `.json` removal put back in Trust::remove): test 1496 FAILS at keys.rs:1507. Re-run this round. |
| (fix) A fingerprint the record contradicts refuses the whole file: a given one (keys.rs:783-790), a held key's (keys.rs:792-799), a descendant's (keys.rs:810-815). The record's fingerprint and this machine's bytes must agree. | keys.rs:783-790, :792-799, :810-815 | keys.rs:1178 a_fingerprint_that_differs_from_the_records_refuses_the_whole_file (the round 4 probe case: record fp kept, serial unchanged); keys.rs:1195 a_held_key_whose_record_fingerprint_is_another_refuses_the_whole_file; keys.rs:1212 a_descendant_whose_record_fingerprint_is_another_refuses_the_whole_file | M-G1 (the given-fingerprint check off, `&& false`): 1178 FAILS at keys.rs:1188. M-G2 (held check off): 1195 FAILS at keys.rs:1206. M-G3 (descendant check off): 1212 FAILS at keys.rs:1226. Each run, restored byte for byte (cmp). |

## Routes by which a descendant's `.pub` can be gone (decision C)

A descendant is a key a revoked key added: its `.json` names the parent in `by` (keys.rs:561-589, `descendants`, the only reader of a
`.json` that has no `.pub`). Every route below was found by reading installer/src/, perch/src/ and nix/, and by grep for every remove_file,
remove_dir, remove_dir_all, rename and unlink call in perch/src/ (route 9 lists the results). "Record gets its fingerprint?" says whether the revocation of that descendant can carry its fingerprint. "Recorded by
id?" says whether the descendant's id is blocked after its parent is revoked.

| # | Route | Evidence | Record gets its fingerprint? | Recorded by id? |
|---|---|---|---|---|
| 1 | `minerva keys remove <id>`: the CLI; `hoot keys remove` through WSL; and the page's remove, armed by D4. The page's remove is perch/src/install/mod.rs:319 (`keys remove <id> --confirm <fp> --json`). | minerva.rs:521 calls keys_remove (minerva.rs:647), which takes the typed fingerprint (or `--confirm`) and calls Trust::remove (keys.rs:547-558): the `.pub` is removed at :556 (an error stops the call). The `.json` is kept (round 4 fix), so the cascade still finds a removed descendant. Under Nix, nix/minerva.sh:57 refuses every keys verb except `keys allow-web`. | No. The removed key itself gets no record of any kind: `keys remove` writes no revocation (keys.rs:547-558 has no record write; see risk 10). | A removed DESCENDANT: yes, once its parent is revoked (keys.rs:561-589 finds it through its kept `.json`; test keys.rs:1496). A removed key whose parent is not revoked: no. |
| 2 | A hand delete, move or rename of `<ID>.pub` in the keys dir, by any process running as this user. keys.rs:265 (the RECOVER text) tells the operator to move the directory aside. | The trusted set is read from the directory on every run (Trust::all, keys.rs:389-421). Nothing checks what is there. | No. | Yes, if its `.json` is still there and its parent is later revoked (the cascade reads the `.json`, :561-589). |
| 3 | The `.pub` cannot be read or parsed (truncated, disk damage, mode change, a directory of that name). Trust::all skips it. | keys.rs:399 and :400 (`let Ok(text) = ... else { continue }`; `let Ok(mut k) = parse_pub(...) else { continue }`). fp_of (keys.rs:757) then finds nothing, so the revocation is by id only (:809). The `.json` still names the parent, so the descendant is still walked (keys.rs:561-589). | No. | Yes (same condition as route 2). |
| 4 | A second `.pub` with the same id as a key already listed (hand-placed). Trust::all keeps the first. | keys.rs:407-409 (`if !out.iter().any(|o| o.id == k.id)`). | Hidden, not gone. Same result as 3 for the kept key. | Same as 2. |
| 5 | The whole keys dir removed or moved aside. state.json goes too, so EVERY revocation is forgotten, not only the descendants'. | keys.rs:265 (RECOVER); state() refuses a missing state.json only while the dir exists (keys.rs:352-359); ensure_dir starts fresh (keys.rs:458-484). | Not applicable: no record survives. | Not applicable: no record survives. |
| 6 | A rotation that revokes the descendant's parent (the cascade). | Plan: the descendants loop at keys.rs:808-816 reads fp_of(d) at plan time, while the `.pub` is still here, and the round 4 fix checks it against the record (:810-815). Commit: the record is saved at :868 and only then are the `.pub` and `.json` of every planned revoke removed (:869-872; descendants included). | Yes, when the bytes were on this machine at plan time. Not a residual; keys.rs:1476. | Yes. |
| 7 | A rotation that names the descendant in `revoke` or `revoke_fp`. | Same as 6: the id is in plan.revokes. With `revoke_fp`, the file's fingerprint is taken (keys.rs:783-790) and the record keeps it (keys.rs:857-858). keys.rs:1104 a_fingerprint_given_later_reaches_the_record_and_the_preview covers the mechanism for an id whose record has no fingerprint. | Yes, when the file gives `revoke_fp`. | Yes. |
| 8 | `minerva uninstall`. | install.rs:185-205 removes only the opt tree, the bin link and the installer state dir; minerva.rs:296 says the update-keys dir is kept. | Not a route. | Not a route. |
| 9 | The perch. | perch/src writes no key file and never builds a path into the keys dir. `grep -rn update-keys perch/src` finds it only in perch/src/guard/ (mod.rs, README.md, rows.rn), where it is the guard's refusal text and its rows, not a path. The test stand-in perch/src/bin/fake_minerva.rs keeps its own `keys` folder under MINERVA_FAKE_DIR (:222, :224, :247-248, :256), and perch/tests/install.rs:148 points MINERVA_FAKE_DIR at a fixture, so that folder is never the real keys dir. Its only removal of a key is route 1 (perch/src/install/mod.rs:319, the argv of `keys remove`). The grep for remove_file, remove_dir, remove_dir_all, rename and unlink in perch/src/ (round 4 fix 2) finds 25 product call sites outside #[cfg(test)] modules, and 5 in the test stand-ins (perch/src/bin/). Every product site is on a path the perch made, and none names the keys dir: agent/token.rs:208 (on_exit's compare-before-unlink; the rule is documented at agent/mod.rs:97-103), sessions.rs:1322 (drop_token_file), spawn.rs:347 (give_up), lib.rs:337 (remove_token, only when the content is the token it wrote), fork.rs:47 and :154 (a copy's own log, and a failed copy's), pending.rs:365 (the log of a fork that was not saved), pending.rs:520 (`<home>/first-<id>`, the pictures dir) and :539 (the Pics drop), dirs.rs:206 (remove_dir of the folder it just made: an empty folder only), config.rs:169 (the staged guard file, `minerva-guard-<8 hex>.rn`, named in guard/mod.rs:150-156), notes/mod.rs:410, :428, :446 (renames inside the notes folder and into `<home>/trash/notes`) and :457 (removes trashed notes from `<home>/trash/notes`), relocate.rs:212, :255, :287, :291 (a chat folder: promote requires its known cwd to be an inbox folder, relocate.rs:482; :287 removes a partial copy that call made), statefile.rs:41 and term/mod.rs:952 (a state or term file set aside under a suffix), term/mod.rs:974 (prune_aside: only copies whose names have that shape), and trash.rs:136, :196, :217 (a chat log into, out of and removed from `<home>/trash`). The stand-in sites are fake_minerva.rs:247-248 (`<MINERVA_FAKE_DIR>/keys/<id>.pub` and `.source`), fake_eidolon.rs:450 and :529, and fake_eidolon/agent.rs:222 (the stand-in's token and header files). Not product code: the removals inside #[cfg(test)] modules (for example rawfile.rs:280, :299, tree.rs:442, install/mod.rs:1072-1078, term/mod.rs:2117), and the comments that mention remove_file (serve.rs:1042, agent/mod.rs:98-102). | Not a route beyond 1. | Not a route beyond 1. |
| 10 | A hand delete of a key's `.json` (its `.pub` stays). NEW, round 4 fix; found while reading the cascade. | descendants() (keys.rs:561-589) finds children only through their `.json` `by`. A child with no `.json` is never walked, so after its parent is revoked it is NOT revoked and stays trusted: all() (keys.rs:389-421) lists it from its `.pub`, and trusted() (keys.rs:424-432) filters only revoked ids and fingerprints. By reading only; not probed. | No. | No. A key with its `.json` gone is not revoked at all. Needs write access to the keys dir: the same trust as route 2. |
| 11 | An agent's shell delete of a `.pub` through a guard gap. NEW, round 4 fix. NOT PROBED. | The guard refuses a shell text containing `minerva/update-keys` (guard/mod.rs:521) and a command word that names it (guard/mod.rs:581; guard/README.md row 4). A delete that reaches the directory without those words (`cd ~/.config/minerva` then `rm update-keys/<id>.pub`, a path built in a variable, a script on disk) is not covered by those rows. guard/README.md:117 says the missed shapes "can still put a file in the directory". Whether the file-verb rules refuse such a delete was not checked. | Not verified. | Not verified. |

What routes 1-5 leave, for the same bytes: a removed key that no revoked key added (route 1) leaves NOTHING recorded, so `keys add` (fingerprint typed back)
or a rotation accepts the bytes again, under any id; a removed or hidden descendant (routes 1-4) is blocked by id once its parent is revoked and
its bytes under a fresh id are accepted by `keys add` or a rotation (the closure is a signed file that names the key's fingerprint in
`revoke_fp`, with no `.pub` the file's value is taken, keys.rs:783-790, and the record gains it, keys.rs:857-858). Route 5 leaves nothing.
Routes 2-5 need write access to the keys dir, and whoever can write there can trust any key directly. That is why C is accepted for the
descendant residual. Route 1 for a non-descendant is the risk 10 question, not a residual of decision C.

## Trust-root changes from rounds 1-3 (still applied; cites re-checked against aba26bb)

| # | Change | Where (aba26bb) | Guard | Mutation evidence |
|---|---|---|---|---|
| 1 | A revocation record keeps the revoked key's fingerprint (`fp`, 16 lower-case hex) | keys.rs:214-222 (Revoked); :672-681 (push_revoke); :685-695 (want_revoke) | keys.rs:1024 a_revoked_key_has_no_way_back_under_a_fresh_id; keys.rs:1053 a_revocation_fingerprint_in_the_file_refuses_a_key_this_machine_never_held | Round 2 (aff2f3b), Mutation A: the test FAILS. Not re-run |
| 2 | `revocation_of` matches by id OR by fingerprint | keys.rs:226-229 | keys.rs:1024; keys.rs:987 a_revoked_key_cannot_come_back | Round 2: id-only and fp-only mutants fail. Not re-run |
| 3 | A fingerprint in the record that is not 16 hex digits refuses the record (`unsafe`) | keys.rs:232-251 (check_fingerprints, called at :368) | keys.rs:1277 a_fingerprint_that_is_not_sixteen_hex_digits_refuses_the_record | none |
| 4 | `trusted()` drops every key whose id or fingerprint is revoked | keys.rs:424-432 | keys.rs:1253 trusted_never_lists_a_key_whose_fingerprint_is_revoked | Round 2: id-only filter fails. Not re-run |
| 5 | add_manual refuses a key whose id or bytes (any id) were revoked, before anything is written | keys.rs:514-516 | keys.rs:1024; keys.rs:987 | Round 2: id-only fails. Not re-run |
| 6 | add_manual refuses the compiled-in key by id or bytes: `revoked` once revoked, else `compiled` | keys.rs:518-528 | keys.rs:1243 a_revoked_compiled_in_key_has_no_way_back_under_a_fresh_id; keys.rs:1561 every_refusal_carries_its_kind_through_added_context | none |
| 7 | add_manual refuses an id already listed, even with the same bytes (`collision`) | keys.rs:529-533 | keys.rs:1290 add_manual_refuses_a_key_already_in_the_list_and_never_rewrites_its_record | none |
| 8 | add_manual refuses the same key bytes under another id (`collision`) | keys.rs:534-536 | keys.rs:1290 | none |
| 9 | add_manual never rewrites an existing record; every existing id is refused before ensure_dir (:538) | keys.rs:510-538 | keys.rs:1290 (asserts the .json bytes are unchanged) | none |
| 10 | A signed rotation that adds a revoked key (id or bytes) refuses the whole file; the serial does not move | keys.rs:738-743 (plan_rotation) | keys.rs:1024 (rotation part); keys.rs:987 | Round 2: refusal off fails. Not re-run |
| 11 | ensure_dir builds the keys dir atomically: `.update-keys.new`, mode 0700, state.json written and fsynced, renamed into place (:477), parent fsynced (:478) | keys.rs:458-484 | keys.rs:1346 a_fault_after_the_record_and_before_the_rename_leaves_no_keys_dir; keys.rs:1316 a_crash_while_the_keys_dir_is_made_leaves_nothing_that_reads_as_a_keys_dir | Round 2 in-place mutant: fails. Not re-run |
| 12 | `keys list --json` and `keys inspect --json` fail as {"error":{"kind","sentence"}} on stdout, exit 1, nothing on stderr | minerva.rs:551 (keys_read), :621 (print_refusal) | installer/tests/cli.rs:484 keys_list_and_inspect_fail_in_the_json_shape_on_stdout_with_nothing_on_stderr | cli.rs: 22 passed (round 4 fix) |
| 13 | `minerva keys allow-web` under Nix is refused, prints "managed by Nix: update through nix", exits 1, arms nothing, runs no curl | nix/minerva.sh:57-58 | installer/tests/nix_allow_web.rs:43 keys_allow_web_under_nix_says_so_and_arms_nothing; :61 keys_verbs_other_than_the_arm_are_not_run_here | nix_allow_web.rs: 2 passed (round 4 fix) |
| 13b | The Linux arm's 404 branch separates the Nix 404 (`"kind":"nix"`) from every other 404 | bin/perch-open.sh:193 (rbody), :199-205 (the 404 case; the Nix test is the glob at :201) | installer/tests/perch_open.rs:231 the_arm_says_what_the_perch_answered; its Nix block: the stub at :253, the exact stderr at :257 | Round 3 (M2: the branch forced off): the test FAILS at perch_open.rs:257. Round 3, not re-run. perch_open.rs: 19 passed (round 4 fix) |
| 14 | Id-only `Trust::is_revoked` deleted (no caller) | none (grep for `is_revoked(` finds nothing; the name also appears in test names) | build and every keys test | none |
| 15 | Clippy-only fixes, no behaviour change | installer/build.rs:4,5,7; src/wslparse.rs:18; src/install.rs:191-193; src/keys.rs:733; src/notice.rs:42-43; src/plugins.rs:355; src/update.rs:79-80; src/bin/minerva.rs:320-321 and :807 | the full installer suite; clippy -D warnings clean (round 4 fix) | none |
| 16 | keys.json may carry `revoke_fp: [{id, fp}]`, a sibling of `revoke` so an installer built before it still reads `revoke`. A given fingerprint that is not the one this machine holds under that id refuses the whole file (keys.rs:779-780); a given one that the record contradicts refuses it too (keys.rs:783-790, round 4 fix); two different fingerprints for one id refuse it (want_revoke, :688); a fingerprint that is not 16 hex digits refuses it (:770-772) | Rotation struct keys.rs:601-614 (revoke_fp at :612-613); RevokeFp :616-620; plan_rotation :708-820 | keys.rs:1072 a_revocation_fingerprint_that_names_another_key_refuses_the_whole_file (mismatch, two fingerprints, malformed fingerprint); keys.rs:1178 (record contradiction) | Round 2: Mutation A FAILS. Not re-run |
| 17 | A preview (with_plan, update --check, the notice) keeps a fingerprint the on-disk record has for an id without one | keys.rs:369-379 (state(): the in-memory merge; the upsert at :373-375) | keys.rs:1104 a_fingerprint_given_later_reaches_the_record_and_the_preview (asserts the preview at :1119) | Round 3, Mutation B (the upsert off at :374): the test FAILS. Round 3, not re-run |
| 18 | A named revoke with no fingerprint in revoke_fp, no copy of the key here, and NO fingerprint in the record refuses the whole file, nothing committed. Round 4 narrows it: a record fingerprint now counts (decision B) | keys.rs:803 (the `(None, None)` arm); :758 (the record lookup) | keys.rs:1126 (refusal, no record); keys.rs:1149 (accepted with a record) | M1 and M-B, round 4 and re-run this round (see the decisions table) |
| 19 | A held id revoked without a fingerprint is recorded with the fingerprint computed on this machine (and a record that holds another one refuses the file: round 4 fix) | keys.rs:792-799 (`(None, Some(h))`) | keys.rs:1232 a_held_key_revoked_without_a_fingerprint_is_recorded_with_its_own; keys.rs:1195 | Round 3, M5 (the held fallback replaced by the file's value only): the test FAILS. Round 3, not re-run. M-G2 (round 4 fix): 1195 FAILS |
| 20 | A key a revoked key added is recorded with its fingerprint only while this machine holds its bytes, and by id alone while its `.pub` is gone; the id is recorded when its `.json` is still there | keys.rs:808-816 (the descendants loop: fp_of(&d) at :809, the record check at :810-815, push_revoke at :816) | keys.rs:1476 a_key_a_revoked_key_added_keeps_its_fingerprint_only_while_its_bytes_are_held; keys.rs:1496 a_descendant_removed_by_trust_remove_is_still_revoked_by_id (round 4 fix) | M-C (round 4) and M-D, M-G3, re-run this round (see the decisions table) |
| 21 | release-files.sh refuses a keys.json that revokes without its revoke_fp entry, in both modes, before writing; round 4 fix: the header says it is bash and that it checks the two lists and the outer braces, not the whole JSON | installer/release-files.sh:9-14, :33-69 | installer/tests/release_files.rs (seven tests, above) | M-A, round 4, not re-run |
| 22 | A revocation listed again with no revoke_fp keeps the record's fingerprint | keys.rs:758, :803 | keys.rs:1149 | M-B and M1, round 4 and re-run this round |

Clippy-only fixes (row 15), each an error in the `cargo clippy --offline --all-targets -- -D warnings` baseline: installer/build.rs
doc-list continuation lines re-indented; wslparse.rs `chunks_exact(2)` to `as_chunks::<2>().0`; install.rs nested `if let` to a let-chain;
keys.rs:733 expiry check to a let-chain; notice.rs:42-43 to a let-chain (it was cited as 43-44 before; corrected); plugins.rs:355
`.iter()` to `.keys()`; update.rs:79-80 to a let-chain; bin/minerva.rs:320-321 and :807 to let-chains (320 was cited as 321 before).

## Test-fixture and perch-test changes (not product code)

| Change | Where (aba26bb) | Why |
|---|---|---|
| The stub listens on a loopback port the OS picks for the process | installer/tests/common/mod.rs (stub_port, OPERATORS_PORT); perch_open.rs (every stub call site) | A fixed 48110 was shared by every run on the host |
| One host-wide lock file for every test that uses the stub port | common/mod.rs (port_lock) | Runs from two worktrees never took turns |
| Env::cmd pins MINERVA_HOME under the temp HOME | common/mod.rs (Env::cmd) | The spec names MINERVA_HOME. release_files.rs also uses Env, so each release test runs under a temp HOME |
| The interrupt test resets SIGINT in its child | perch_open.rs (reset_sigint) | A shell starts background jobs with SIGINT ignored |
| The stand-in writes each log line in one write | perch/src/bin/fake_minerva.rs:280 (`file.write_all(format!("{line}\n").as_bytes())`) | writeln! is several writes |
| The reader parses whole lines only | perch/tests/install.rs:179-185 (calls(); split_inclusive at :181, the ends_with('\n') filter at :182) | A line still being written is not parsed |

## Residuals (accepted, stated in installer/README.md)

- R1(a), round 3 (narrowed by round 4). A revocation with no fingerprint anywhere, from a record written before fingerprints were kept
  (a development install; nothing was released), refuses by id only. The same bytes under a fresh id are accepted by hand and by a
  rotation until a signed file names that key in `revoke_fp`. Round 4 changes nothing here: the record has no fingerprint to offer. The
  maintainer closes it with one rotation that names the fingerprint.
- R1(b) / Risk 2, reworded for round 4 as decision C: the descendant residual. A key a revoked key added, whose `.pub` is no longer on
  this machine, is blocked by id here while its `.json` still names the parent (`keys remove` keeps it, round 4 fix). Its bytes under a
  fresh id are accepted by `keys add` or a rotation. A key whose `.json` is also gone is not revoked at all (route 10). Accepted: whoever
  can change the keys dir can trust any key directly, and route 1 needs the fingerprint typed back. Closed by a signed file that names the
  key's fingerprint in `revoke_fp`. installer/README.md, "What `add` refuses", names these residuals with what closes each.
- R2. Concurrency on the keys dir: no cross-process lock. Two runs at once can race on `.update-keys.new`. The loser fails closed by
  inference. Not tested.
- R3. Durability: the fsyncs are not observable from a test. The ordering in ensure_dir and the record-before-key-files order in
  commit_rotation (keys.rs:868 before :869) carry the guarantee.
- R4. (test only) The stub's port is the OS's pick for each process. A foreign process could bind that exact port during a run; the
  result is a failure, not a false pass.
- R5. The defensive skip in commit_rotation (keys.rs:874, `if st.revocation_of(k).is_some() || trust.is_compiled(k)`) is a second guard
  after plan_rotation's refusal. Kept on purpose.

## Risks I see (for the reviewer to check)

1. (Round 3, unchanged.) A refused keys.json leaves a leaked key the machine holds trusted until a corrected file is out. plan_rotation
   bails before any write (keys.rs:776-816 for the revoke loop; the parse at :709), and update.rs:60 propagates it with `?`. The
   whole-file refusal is the operator's ruling and installer/README.md step 2 says so. release-files.sh refuses the file first.
2. (Round 3, Risk 2.) The descendant residual. Now decision C, accepted; see the routes table, route 10 included.
3. (Round 3, Risk 3.) A later file that re-names a revoked id with no revoke_fp, where the record has NO fingerprint, is refused whole
   (keys.rs:803). Round 4 fixed the case where the record has one (decision B). The pre-I2 case is R1(a): the maintainer names revoke_fp.
4. CLOSED in this round (was open in round 4). A signed file that names a fingerprint DIFFERENT from the one the record holds for that
   id was accepted and its value dropped (round 4 probe: record 29c55b0d8e35d18c, file 0123456789abcdef, result Ok(Applied([], [])),
   serial 2). Now refused whole: keys.rs:783-790 (a given one), :792-799 (a held key's), :810-815 (a descendant's); tests keys.rs:1178,
   :1195, :1212; M-G1, M-G2, M-G3 each FAIL. The probe itself was not re-run on a scratch copy; test keys.rs:1178 is the same case.
5. NARROWED in this round (was open in round 4): release-files.sh checks the two revoke lists and the outer braces, not the whole JSON.
   A keys.json with a malformed `add` array, a stray brace inside, or a `revoke_fp` nested in an `add` entry passes the script, and
   every client refuses that file whole (serde, in plan_rotation's parse at keys.rs:709; the nested case: the client reads only the
   top-level `revoke_fp`). The header (:9-14) and installer/README.md 'Releasing' say so and point the maintainer to a JSON parser
   before signing. The I2-fix4 spec's parenthetical ("a keys.json it cannot parse is refused, never waved through") is NOT met for those
   shapes; the narrowing is the orchestrator's to confirm, or a validator is the alternative. Also stricter than the client in one place: a
   `revoke_fp` object with an extra field is refused by the script, while the client ignores unknown fields (keys.rs:616-620 has no
   deny_unknown_fields).
6. (Round 4, unchanged.) The script cannot see a machine's record, so it refuses a revoke with no revoke_fp even where a client's record
   would supply the fingerprint (decision B). That is the stricter side, and it is what the README asks of the maintainer.
7. (Round 3, Risk 4.) The nix match in bin/perch-open.sh:201 is a substring test on the body. A non-Nix 404 whose body contains
   `"kind":"nix"` would print the Nix sentence. Nothing is armed on either path. Unchanged.
8. (Round 3, Risk 5.) The perch install.rs race fix is evidenced by repeated runs (3 runs in round 4, 31/31 each), not by a deterministic
   reproduction. Unchanged.
9. (Round 3, Risk 6.) The Nix sentence is written twice: nix/minerva.sh:58 and bin/perch-open.sh:202. The tails are identical today.
   installer/tests/perch_open.rs:23 pins the perch-open copy; nix_allow_web.rs checks only the two phrases for nix/minerva.sh.
10. NEW, OPEN, not changed (a decision for the orchestrator): `keys remove` is not a revocation. It records nothing for the key it removes,
    not even its id (keys.rs:547-558; minerva.rs:647-669 calls only Trust::remove). So `keys add` (fingerprint typed back) and a rotation
    accept the same bytes again, under any id. Recording the id at removal would block a re-add by hand, which is a behaviour change; it is
    not made here. README 'By hand' says so.
11. NEW, OPEN, not changed: route 10. A hand delete of a key's `.json` (its `.pub` stays) takes the key out of the cascade; after its
    parent is revoked it stays trusted. Not probed. The fix would need the parent kept somewhere the cascade reads (state.json, say), which
    is a format change; not made.
12. NEW, harmless: Trust::remove leaves an orphan `.json` for a removed key. all() (keys.rs:389-421) lists keys by `.pub` only, so the
    orphan changes no trust decision; descendants() (keys.rs:561-589) reads it, which is why it stays.
13. NEW, OPEN, not changed (a decision for the orchestrator). A wrong fingerprint in `revoke_fp` for an id a machine never held is
    recorded as given, and no signed file replaces it: keys.rs:783-790 refuses a different given one, and keys.rs:857-858 keeps the
    record's. On each machine that applied it, every later file that names that id with its real fingerprint is refused whole
    (keys.rs:787), and the same bytes under a fresh id are accepted there (keys.rs:514-516 and :738-743 match by id or by the recorded
    fingerprint only). The way out is a later file that leaves the id out of `revoke` and `revoke_fp`. Probe, round 4 fix 2 (a
    throwaway test in keys.rs, removed; keys.rs restored and cmp'd against HEAD): serial 1 with fingerprint 0123456789abcdef for a
    never-held id applied and the record kept it; serial 2 with the real fingerprint refused, serial unchanged; serial 3 leaving the id
    out applied; add_manual of the real bytes under a fresh id accepted. README "Every id in revoke" says so. Options: keep it (the
    maintainer checks with `minerva keys inspect` before signing; the file that leaves the id out is the way out), or let a signed file
    replace a recorded fingerprint, which changes the "a record is kept" rule (keys.rs:857-858) and the test at keys.rs:1178.

## What is never exposed

- No token, code or secret reaches argv, output or a log in any path of round 4 or this fix. The release-files.sh refusals print a fixed
  sentence and, for a missing fingerprint, the id; the id is validated as 16 hex digits before it is printed, and a public key id is not
  secret. Nothing else from keys.json is printed. The script never echoes file content.
- The record fingerprint checks (this round) print the key id and fingerprints. A fingerprint is of a public key, so it is not secret, and no
  path or file content is printed. The rotation refusal prints the id it refuses and no path.
- The probes and the mutation runs used keys made by the test helpers (keypair, sign), never a real key or any file under ~/.config.

## Verification (round 4 fix 2, HEAD 33fae11)

- Installer `cargo test --offline --no-fail-fast` (WSL, CARGO_TARGET_DIR=/home/testuser/.cache/minerva-I2-target,
  MINERVA_ALLOW_PLACEHOLDER_KEY=1, HOME a temp dir with MINERVA_HOME and XDG_CONFIG_HOME inside it; CARGO_HOME and RUSTUP_HOME name the
  toolchain): lib 82, cli 22, nix_allow_web 2, perch_open 19, release_files 7. 132 passed, 0 failed.
- `cargo clippy --offline --all-targets -- -D warnings` (installer), forced to re-check (keys.rs touched, content unchanged): exit 0,
  no warnings.
- perch `cargo test --offline --features test-bins --test install`: 31 passed on each of three runs in a row.
- `bash -n installer/release-files.sh`: exit 0 in Git Bash and in WSL. WSL `dash -n` exits 2 at line 39 (the here-string). The spec's
  "sh -n" check is `bash -n`: the script is bash (its header says so), and the POSIX premise was wrong before this round.
- The Risk 13 probe ran on keys.rs with one throwaway test; keys.rs was restored from a copy taken at HEAD and cmp'd; `git diff` is clean.
- Disk guard (WSL `df -h /mnt/c`) before each build or test: 182G free at every run.
- No code changed this round, so the round 4 mutations are not re-run. Every cite was re-checked against the final HEAD: the code
  cites by reading the cited line (a script printed all 116 file:line cites), the removal list by grep (route 9).

## Verification (round 4 fix, aba26bb)

- Installer `cargo test --offline --no-fail-fast`, WSL, CARGO_TARGET_DIR=$HOME/.cache/minerva-I2-target, MINERVA_ALLOW_PLACEHOLDER_KEY=1,
  every run under a temp HOME with MINERVA_HOME and XDG_CONFIG_HOME inside it: lib 82 (78 + 4 new), cli 22, nix_allow_web 2, perch_open 19,
  release_files 7 (6 + 1 new). 132 passed, 0 failed. Before this fix: 127.
- `cargo clippy --offline --all-targets -- -D warnings` (installer): exit 0 after the last edit (`touch` forced the crate to re-check).
- perch `cargo test --offline --features test-bins --test install`: 31 passed on each of three runs in a row. No perch file changed.
- `bash -n installer/release-files.sh`: exit 0. The named check `sh -n` is replaced by it: the script is bash (`#!/usr/bin/env bash`, here-strings
  and arrays). WSL `dash -n` fails at HEAD on line 39 (a here-string), and already failed at e072d5e and d8c5353 on line 37 (the arrays), so
  the spec's "POSIX sh" premise was false before this fix. Git Bash `sh` is bash and passes.
- Mutations, each restored byte for byte (cmp against a copy of the final keys.rs): M-D, M-G1, M-G2, M-G3, M-C, M1, M-B re-run this round,
  every one fails its targeted test (see the decisions table). M-A (round 4) not re-run: the check is unchanged.
- Disk guard before each build or test (WSL `df -h /mnt/c`): 189G free at every run.
- Not run this round: perch's full suite and perch clippy (no perch file changed); the browser checks (not in scope).

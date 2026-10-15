# M2 fix round 3 (orchestrator's decisions on the round-2 verifier's last gaps)

M2 is at bd1f05a (e08e42d, bce45f6, fdb2098, bd1f05a) on base cd2b89f in worktree agent-aba970ccc3baacf9f. Perch 648 passed,
2 ignored, clippy clean both ways. M2-fix.md still holds where this file is silent; where they differ, this file wins. STAY on base
cd2b89f; the orchestrator merges onto master (R2c lands first; the aoide/mod.rs and mesh conflicts are the orchestrator's).

## Ratified by the orchestrator (record each in M2.json notes as "ratified 2026-10-08"; no code change)

- Grant: perch/tests/mesh.rs as changed in bce45f6 (the wrapper and the `schema` argv), because V1 needs it. No further mesh.rs edit.
- Grant: perch/src/aoide/mod.rs pre_exec (S6, PR_SET_PDEATHSIG).
- V1 is narrowed to mail. The mesh half (mesh/mod.rs version_of / mesh_of calling schema_check, and the stale `--version`
  references in mesh/mod.rs and mesh/README.md) is a follow-up for the mesh owner AFTER R2c and M2 are both on master. Do NOT edit
  perch/src/mesh/*. Say so in aoide/README.md where it claims a refusal turns mesh off.
- S2: `disposition` in SentRow is the perch's own key, written and read only by the perch; it is not aoide's field. Kept. One
  recipient per send, so recipients[0].accepted is the whole send's answer. Kept. Say both in mail/README.md.
- D1 (subject is one printable line, silent clean) and D2 (one outbox call a minute while a row waits) are accepted as built.

## Fixes

1. A started child whose wait() fails gets an unknown row. GRANT aoide/mod.rs drive() (~:684) only: a wait() error after a
   successful spawn is a new Error variant for a started child (not Spawn); send.rs deliver() maps it to the unknown-row arm, as it
   does Timeout, TooBig and NotJson. A unit test for the mapping (a fake child whose wait fails, or the variant mapped directly in
   send.rs's tests).
2. The two re-arm rules get tests in tests/mail.rs, each failing when its re-arm line is removed (say which line): (a) a plain read
   whose store write fails for two ticks (received.jsonl made unwritable, then restored) re-arms a reread, and each letter is kept
   once; (b) a reread that fails for another reason re-arms after REREAD_GAP (set mail_reread_ms low). Add both to the mutation
   list in the tests/mail.rs header.
3. The GET error shows every sentence that applies: the lost sentence AND the full-store or receipts-stalled notice (joined, each
   line_safe'd, total capped). A test: after a too-big reread sets lost, a full store is still shown.
4. SentRow gets a hand-written Debug that prints sizes only, like Row; extend the S4 unit test to cover it.
5. Docs: mail/README.md 'Caps' says the 12x control-byte factor and the ~1 KiB per-entry overhead follow from JSON escaping and
   aoide's observed 2x duplication, and are not measured on a control-byte letter. S5: send-path unknown rows carry no msgid, are
   never settled automatically, and the page says to check the outbox; correct the inventory row S5 and mail/README.md to match.
   aoide/mod.rs:58-59 timeout wording agrees with mail/README.md (aoide's own timeout is 30 s); fake_aoide.rs's 'What it records'
   header names env_names.
6. The inventory (review-M2/inventory.md) matches the final HEAD: every line cite re-read; the report names every commit since
   cd2b89f with its sha and the counts at the final HEAD.

## Grants (exactly these, beyond M2-fix.md's)

aoide/mod.rs drive() for item 1 (and its doc lines 58-59 for item 5); perch/src/bin/fake_aoide.rs header comment; M2.json notes.
Never edit serve.rs, lib.rs, main.rs, Cargo.*, tests/common/mod.rs, tests/http.rs or perch/src/mesh/*.

One new commit on top of bd1f05a, repo style, no attribution. Whole perch suite once with --features test-bins; tests/mail.rs five
times in a row (the known flake a_linked_mail_folder_is_read_by_nothing: report its rate; fix it only if the cause is in mail/);
clippy clean both ways.

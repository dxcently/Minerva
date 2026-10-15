# R2c fix round 2 (orchestrator's decisions on the round-1 verifier's last gaps)

R2c is at 25b7113 (7447c61 + 25b7113, on eeb69ca/88b4220, base d8c5353) in worktree wf_0543ff15-bf5-1. Perch 856 passed, clippy
clean both ways, installer perch_open 19. R2c-fix.md still holds where this file is silent; where they differ, this file wins.

## Fixes

1. Dangling link at a feed path (mesh/mod.rs resolved(), ~:277). Follow the symlink chain before the parent join: at most 40 hops,
   symlink_metadata + read_link, a relative target joined to the link's own parent; then canonicalize the final path, or canonicalize
   its parent and join the name. Too many hops: refuse the start with a sentence. Unit assertion in
   a_start_with_the_two_feeds_as_one_file_is_refused_and_two_files_are_not: an audit path that is a dangling link to the (not yet
   existing) daemon feed path gives Err; a link chain of two hops too. The mesh README bullet then stays true.
2. Audit messages, fail closed (DECISION). An audit record's `message` is forwarded ONLY when `command` is `daemon` or
   `adapter.melete` AND `status` is `started` (the two start records aoide's docs name: docs/Aoide-Wiki .../Doors-and-Nodes.md ~:91 and
   ~:213; cite them in the README). Every other audit record forwards ts, door, class, command, status only. The mail rule becomes a
   special case of this; keep the case-insensitive command match. The secrets broker writes resolve/put/admin audit lines to the same
   log (Secrets-Broker.md ~:46-47, ~:122), which is why. Update audit_keep, the README rule, the page (view.js draws a message only
   where one is sent; no change if it already does), the assertions near mod.rs ~1520, and
   the_audit_allowlist_drops_what_it_cannot_pin_and_forwards_no_text in tests/mesh.rs. Add a test: a `secrets.resolve` line with a
   message carrying a name, and a `daemon` line with status `failed` and a message: neither message is in any answer byte.
3. Gate url credentials, fail closed. After without_userinfo, a url value that still contains `@` anywhere is dropped (the field is
   absent, not shown), and a value with no `://` that contains `@` is dropped too. Fix the scrubbed() comment and the README to say
   exactly that. Unit cases in the_feed_keeps_the_fields_the_docs_name_and_no_other: `user:pass@box-a:8710/`,
   `http://user:pa/ss@host/`, `http://u:p@host/x` (cut to `http://host/x`), and a plain `http://host:8710/` kept.
4. tests/common/mod.rs refused_start (GRANT, that function only): wait for the child with the same bounded poll tests/mesh.rs
   refusal_of uses, so a refusal that never comes fails within the bound instead of hanging. Every existing caller still passes.
5. perch/tests/README.md: the "400 and 502 cases" row says what the code does now (a link, FIFO or directory at one path is a 200 with
   error.<stream>; 502 only when the blocking join fails). The two README rows round 1 added (perch/tests/README.md mesh row,
   webui/term/tests/README.md mesh.test.mjs row) are granted retroactively; keep them accurate.
6. Long lines: hand re-wrap every line this slice added or changed that is over 150 columns in perch/src/mesh/mod.rs,
   perch/tests/mesh.rs, perch/src/main.rs, perch/src/aoide/mod.rs, perch/src/lib.rs. No behaviour change; never rustfmt.
7. Accepted as is (no work): the relative XDG_RUNTIME_DIR case is covered by unit and runner tests only (tests/common pins
   XDG_RUNTIME_DIR); say so in perch/src/mesh/README.md's test notes.
8. Inventory (review-R2c/inventory.md): R11 (dangling link) and R16 (scheme-less userinfo) closed with the tests named; section 4
   rewritten for the start-records-only rule; every line cite re-read at the final HEAD; the report names every commit since d8c5353.

Before the page tests, restore the checkout artifact: `git -C <worktree> checkout -- webui/term/vendor/hooks.js` (the index blob has
no CR; this is the only file outside the grants you may touch, and only with that command).

One new commit on top of 25b7113, repo style, no attribution. Whole perch suite once with --features test-bins; clippy clean both
ways; page tests 283/283; installer perch_open. Live check on 48131 again (fix round binaries in $HOME/.cache/R2c-bin): a secrets-style
audit line and a gate line with `user:pass@` in a scheme-less url, both absent from every answer byte. Bearer from a 0600 header
file, never argv; then --stop.

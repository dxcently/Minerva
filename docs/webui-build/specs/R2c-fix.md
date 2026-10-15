# R2c fix round (before merge)

R2c is at eeb69ca (88b4220 + eeb69ca) on base d8c5353 in worktree wf_0543ff15-bf5-1. The verifier passed it; the security
review (12 confirmed, none high or medium) says merge after these fixes. Where this file differs from R2c.json, it wins.

## New fact (observed by the orchestrator's aoide probe, installed 0.0.26, temp root)

aoide's audit log lines are exactly {ts, door, class:'audit', command, status, message}. A `mail.read` line's `message`
carries the letters' TEXT (cut at about 526 characters); `mail.send` lines carry no body ("filed to self/minerva (msgid ...)").
Usage-error exits write no line.

## Fixes

1. Letter text never reaches the page. For an audit record whose `command` starts with `mail.` (mail.read, mail.send,
   mail.outbox and any other mail.*), drop `message` (forward ts, door, class, command, status only; the page shows the
   command and status). Other commands keep `message`. A test with a mail.read line holding a body: the body is in no
   answer byte. README: say what is dropped and why (the observed fact above).
2. Equal paths: refuse at start when the audit log path and the daemon feed path are the same file (compare the resolved
   absolute paths; also when AOIDE_DAEMON_EVENTS equals the audit flag), with a sentence. A startup test.
3. A relative XDG_RUNTIME_DIR (or AOIDE_DAEMON_EVENTS) is treated like an unset one for the daemon default: no cwd-relative
   path is ever opened. The view and the runner agree (one resolver). A test.
4. Correct the claim 'the perch never writes either file' (mesh/mod.rs:12-13, the README and the inventory): the perch
   itself never opens either file for writing, but every aoide call it makes appends aoide's own audit records to the audit
   log (to the flag's file when --aoide-audit-log is given, else aoide's default), as any aoide call does. Say how often
   (the snapshot is cached 60 s; mail calls).
5. The daemon feed passes a fixed field set only. feed_keep forwards, for class audit and gate records, only the fields
   aoide's docs list for them (cite the doc section); anything else is dropped, so a free-text field a later aoided adds
   never reaches the page. payloadText draws only those fields. A record with no numeric ts is dropped (as audit records
   already are). Tests: a record with an extra free-text field and one with no ts.
6. One bad stream does not blank the other: a link, FIFO or unreadable file at one path gives that stream an error
   sentence in the answer (files.<stream> or a per-stream error field) and the other stream still answers; the page shows
   the sentence on that stream's label. Keep the answer shape documented.
7. Cursor reset when a feed was truncated and regrown past the cursor: if cheap, keep the file's identity (dev and inode)
   and size per path in memory and answer reset when the identity changed or the size went down since the last poll; if
   not cheap, document the limit in the README.
8. README rows: mesh/README.md:90 (what a reset covers), :136 (the test builder sets a temp HOME), the HOME-empty/unset
   audit default and the XDG-unset daemon default, and the daemon default being the XDG path rather than aoided's
   socket-relative rule (aoide's rule cited).
9. installer/tests/fixtures/perch-help-new.txt and installer/tests/perch_open.rs:709 still name the removed --aoide-events:
   regenerate the fixture from the new perch's --help exactly as installer/tests/fixtures/README.md says, and update the
   test line. GRANTED for this round. Run installer/tests/perch_open.rs (port 48110 must be free: check `ss -ltn`).
10. The security inventory (review-R2c/inventory.md) is updated for every claim above, and its section 2 (ts-less records)
    and section 6 (never writes) corrected.
11. Live check on PORT=48131 with copies of THIS worktree's binaries (perch, fake_eidolon, fake_aoide) in
    $HOME/.cache/R2c-bin: temp --aoide-home, both flags at temp files, append audit lines (one mail.read with a body, one
    daemon started) and daemon feed lines, curl /hub/mesh/events with cursors (both streams, a reset by truncation, a 400),
    the bearer from a header file never argv; the body never appears. Then --stop. Record every answer (status and size,
    never a token) in the report, and write the browser-pane steps for the orchestrator.

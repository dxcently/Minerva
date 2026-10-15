# M2 review (haiku) confirmed 34, rejected 2

## send-timeout-after-spool-double-send [medium] perch\src\mail\send.rs:333
A 504 on a remote send usually means the letter is already spooled, so the advised resend duplicates it
FIX: After a timeout, recover the msgid from `aoide mail outbox --json` (match to, subject and time) and record the row as queued, not unknown. Or raise the perch's send timeout above aoide's worst case (15 s plus tunnel dial time) and return 202 queued with a warning when the attempt is still pending. Add a client-visible idempotency key so a resend of an in-flight note and node is refused. Confirm the 0.0.26 delivery timing with X0 before relying on either.

## send-unknown-row-unresolvable [low] perch\src\mail\recv.rs:411
An unknown sent row can never be resolved, so the 504 advice to check the sent list is not actionable
FIX: Keep the unknown row, but have refresh_outbox also consider msgid-less rows, matching by to, subject and a time window against `mail outbox --json`. Until then the page should say 'unknown: check aoide mail outbox for <to> / <subject>' rather than point at a list that cannot answer.

## send-signal-or-error-after-spool-no-row [low] perch\src\mail\send.rs:328
A signal-ended or error-enveloped aoide after the spool is a clean refusal with no row
FIX: Treat Failed without an envelope, and signal-ended runs, like the timeout arm: write the unknown row and the maybe-sent sentence. Only a refusal that names a reason (unknown-node, unpaired-node, ambiguous-mesh) is certain that nothing was spooled.

## send-argv-exposure-procfs-and-orphan [low] perch\src\mail\README.md:91
The note's argv is readable by every local account, and an orphaned aoide keeps it past the 10 s bound
FIX: Correct the README claim to 'any local account can read it'. Make the child die with the perch: set PR_SET_PDEATHSIG (SIGKILL) in a pre_exec hook on Linux. Narrow the exposure further if aoide can take the text on stdin, or record in the README that it cannot.

## send-no-send-rate-or-concurrency-cap [low] perch\src\mail\send.rs:272
Nothing caps how many sends run at once or how many a caller can start
FIX: Put a semaphore (2 to 4 in flight) around the aoide call in deliver(), and a per-minute send budget that answers 429. Keep the operator's per-send click as the real control, since the page is what sends.

## send-letter-derives-debug-text [low] perch\src\mail\send.rs:162
Letter derives Debug over the full note text, outside the redaction the aoide module promises
FIX: Implement Debug for Letter by hand, printing the note name, node, mesh, hold and the sizes only, the same way Shape does at aoide/mod.rs:379.

## send-refusal-doc-and-newline-claim [info] perch\src\mail\send.rs:14
The module doc overstates the refusal order, and its U+2028 and newline 400 is unreachable over HTTP
FIX: Correct the doc: say which 409s spawn a read-only mesh call, and that a title is cleaned, not refused. Decide and record whether a title that needs cleaning should be a 400 or a silent clean.

## send-host-name-treated-as-self-by-aoide [info] perch\src\mail\send.rs:297
A node named after this machine's host name is filed locally by aoide, and the perch's guards do not see it
FIX: In resolve(), refuse a registry node whose name equals the local host name, or map it to SelfNode in to_of so the row and the guards match what aoide does.

## send-hold-mesh-flags-not-in-repo-aoide [info] perch\src\aoide\mod.rs:361
The --mesh and --hold argv is not in any aoide source in the repo, and only the fail-closed reading has been checked
FIX: Before the hold and mesh paths are trusted, probe the live aoide with `aoide mail send --help` (no send) and record the declared flags in perch/src/aoide/README.md. Treat the mesh pre-check as advisory in the docs.

## receive-reread-moves-cursor [medium] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:927
--reread moves aoide's cursor, so recovery can consume letters the perch never stores
FIX: Stop treating --reread as a cursor-neutral window. Keep the high-water seq already stored (Row.seq, recv.rs:129) and ask aoide for entries above it without moving the mark; that needs an aoide flag (for example --since <seq> that leaves the mark alone). Until then the perch must not use reread as recovery on a mailbox with unread letters, and must put any gap in the notice instead of advancing silently. Correct the docs at recv.rs 15-21 and 34-37 and aoide/mod.rs 298-300 once confirmed. Add an aoide-backed test (not the scripted fixture) for reread with unread letters.

## receive-entries-cap-drops-newest [medium] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:325
More than 2000 entries in one read silently drops the newest, with the cursor already past them
FIX: Treat a cut read as a failure: when skipped > 0, put the count in the notice and the GET error, and do not return Ok. Do not drop by position: store every entry of a read that fits the stdout cap, since the 2000 limit is a count the 1 MiB cap does not require. Once aoide offers a seq window (see receive-reread-moves-cursor), page by seq. Remove the nothing-is-lost sentence.

## receive-store-full-stuck [medium] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:586
A full card store halts receive and receipts, and the notice's fix does not work until a restart
FIX: Count only live (not dismissed) slots, or rotate old ones, and keep a tombstone set of dismissed msgids that survives a restart. When full, the notice must say a restart is needed after the files are moved, or the in-memory cards must reload when the files change. Surface a receipts-stalled state in the GET error.

## receive-reread-failure-not-retried [low] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:928
A failed reread is never retried, so letters a timed-out read may have consumed stay lost until restart
FIX: When a reread fails, set task.reread = true again with the same pause, and cap the retries. Show the attempt count in the notice.

## receive-card-window-hides [low] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:622
Cards older than the 2 MiB window vanish from GET /hub/mail and cannot be reached from the page
FIX: Build the listing from cards.json ordered by slot.at, and read only the rows the listed cards need. Compute more from the cards map rather than from the window.

## receive-base-size-unbounded [low] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:904
Nothing bounds base.jsonl before aoide is run on it, so a large base can turn every tick into a timeout
FIX: Check base.jsonl length against a limit before calling aoide and report it in the notice instead of reading. Back off geometrically on repeated timeouts rather than using the fixed 30 s pause.

## receive-sender-spoof [low] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:293
A remote node can make its letter show as sender 'minerva' (the operator's own mailbox)
FIX: Show node beside sender in every view, label sender as claimed, and gate nothing on sender. Later slices (M3 agent mail) must not treat sender as provenance.

## receive-cards-set-aside-resurrects [low] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:466
A corrupt cards.json is set aside and rebuilt empty, which resurrects dismissed letters
FIX: Refuse to start the receive task when cards.json exists but does not parse, with a notice and no reads and no silent set-aside. Keep dismissed msgids in an append-only tombstone file so a rebuild cannot resurrect them.

## receive-body-copy-before-clip [info] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:250
The 64 KiB body clip runs after the full text is decoded and copied
FIX: Low priority: clip while decoding, taking the body slice and cutting at a char boundary before to_string.

## receive-save-not-idempotent [info] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:826
POST save is not idempotent: a retry or double click writes a second note
FIX: Record the saved msgid in the slot, or return the existing note name, and answer a second save with 409.

## receive-reader-enrolment [info] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/aoide/mod.rs:228
AOIDE_SESSION_ID=minerva-perch is safe for reads, but the reader becomes an enrolled doorbell reader
FIX: No change needed for the read path. Keep the send-side petname-fallback risk on the M2 accepted-risk list, and do not let a human session take the petname minerva-perch.

## receive-slot-saved-check [info] C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f/perch/src/mail/recv.rs:1
No perch test can fail on the cursor, cap or reread findings as written
FIX: Before merge, add an offline probe against the real aoide on a temp AOIDE_ROOT (never ~/.aoide): deposit more than 2000 letters, run read and reread with unread letters present, and check what the cursor and the output show.

## files-1 [medium] perch\src\mail\recv.rs:927
A write failure lasting past one pause loses letters until the perch restarts
FIX: Re-arm task.reread in the reread-failure branch and clear it only after a reread has applied. Do not clear the notice on an applied-nothing read while a failure is outstanding. Add a test with a write that fails for two ticks.

## files-2 [medium] perch\src\mail\mod.rs:145
Appends never check for a missing final newline, so the next row glues onto a torn one
FIX: Before each append, read the last byte of the opened file and prefix a newline if it is not one. Call sync_all after the received.jsonl append and before persist. Add a test with a planted torn last line.

## files-3 [low] perch\src\mail\recv.rs:466
cards.json is lstat-checked, then read through a path-following open
FIX: Open with rawfile::open_regular, take the length from its fstat, and read through that descriptor with a take() cap, instead of statefile::load.

## files-4 [low] perch\src\mail\recv.rs:577
received.jsonl gets duplicate lines: dedupe lives in memory and the append runs before persist
FIX: Dedupe against the msgids already in received.jsonl under the cards lock (find_row already reads its tail), so a replayed letter is not appended twice.

## files-5 [low] perch\src\mail\recv.rs:466
An unparseable or unreadable cards.json is set aside and every card state resets
FIX: Keep the file and show a notice on a read error (do not rename it). On a parse error, keep the set-aside but refuse the reread until the operator confirms, or carry the dismissed msgids forward.

## files-7 [low] perch\src\mail\mod.rs:119
First creation of the mail folder races between a send and the receive task
FIX: Treat AlreadyExists as success and re-check the metadata, or take one lock around the create.

## files-9 [low] perch\src\mail\recv.rs:622
Reads go through a symlinked mail folder; the folder is checked only on writes
FIX: Check the folder with symlink_metadata in paths(ctx) and refuse the reads as the writes already do.

## files-10 [low] perch\src\mail\recv.rs:89
Two perches on one home share a mailbox cursor and overwrite each other's cards.json
FIX: Take an exclusive flock on <home>/mail/.lock when the receive side starts and refuse to start if it is held, or document one perch per home.

## files-11 [info] perch\src\mail\send.rs:421
sent.jsonl keeps the note title, and Letter and Row derive Debug over their text
FIX: Say in the doc that the title is kept, and give Letter and Row a manual Debug that prints lengths, as Shape and Reply do.

## files-12 [info] perch\src\mail\send.rs:440
sent.jsonl has no cap or rotation, and cards.json never drops dismissed cards
FIX: None needed now. Record it for M3, which adds agent sends to the same file.

## files-13 [info] perch\src\serve.rs:1558
The serve.rs 404 pins hold for the no-home reason, as claimed
FIX: None for security. A serve.rs pin with a home configured would make the route assertion strong.

## files-14 [info] perch\src\mail\send.rs:438
Send and receive share only sent.jsonl, with no lock inversion
FIX: None needed.

## files-15 [info] perch\src\mail\recv.rs:877
Shutdown stops the receive task promptly, but in-flight blocking work is not bounded
FIX: None for the normal case. Fixing files-3 bounds this.

## CRITIC verdict=merge-after-fixes
Completeness critic, slice M2 (commit e08e42d, base cd2b89f). Read-only: diff, vendored aoide-core 0.0.23, upstream Aoide 0.0.26, serve.rs gate, notes/mesh/statefile helpers, fixtures. No builds, tests, or aoide runs. No high-severity finding. The send path holds: the serve gate applies Sec-Fetch-Site and Host to every method, the cookie counts only same-origin, and body reads are capped; the runner strips every AOIDE_* variable, so the send child never inherits a session id or state dir; chat_known and msgid are validated before any write; notes::create writes 0600 atomically; mesh names are name_ok-filtered; no lock nesting across cards, sent and notes. The receive path has one new medium defect that every lens and the fixtures missed. aoide's --json mail read prints each letter twice: once raw inside the envelope's message (render_entry, which includes envelope.text) and again in data.entries. Both copies are JSON-escaped, so stdout is about 2x the letter bytes and about 12x for control characters. The perch's 1 MiB plain cap and 4 MiB reread cap therefore bind at roughly half the size the code assumes. A paired node can make reread fail for good with six 64 KiB control-character letters, and an ordinary backlog over about 2 MiB of history can lose its batch. The fixture (line 4) has "message": "9 entries", so no test sees it. The same render is in upstream 0.0.26, and the plan puts X0, the check that observes the real shapes, in wave W7 after M2. Required before merge: fix the fixture and cap accounting and make a TooBig plain read re-arm the reread (gap 1), and run the X0 shape probe or record an explicit operator decision to merge unverified (gap 2). The rest are low or contract items. Verified and not raised again: the executor's 405 claim (serve gate, serve.rs ~430-440); the no-network-from-GET claim for /hub/mail; ~/.aoide is never named by tests.
- [receive: aoide mail read prints each letter twice, so the caps bind at about half the size the perch assumes (perch/src/mail/recv.rs:67 REREAD_CAP, 956-977 read(); perch/src/aoide/mod.rs:50 STDOUT_CAP)] medium. Vendored aoide-core client/src/commands.rs:3862-3869 render_entry includes e.envelope.text, and handle_mail_read (4163-4190) puts those bodies into the envelope's message ('N entries' + bodies) AND into data.entries. protocol/src/output.rs:56 always serializes Outcome.message, and both copies are JSON-escaped, so each control byte costs 12 bytes of stdout and ordinary text costs about 2x. Against the 1 MiB plain cap, that is about 512 KiB of ordinary letter text, and two 64 KiB control-character letters (about 1.5 MiB) make a plain read TooBig on their own. storage/src/mail.rs:904 saves the cursor before read_for returns, so the TooBig batch is marked read and its letters are never stored. Reread (4 MiB) recovers them only while the whole minerva history stays under 4 MiB of stdout. About six such letters make every reread TooBig for good, and recv.rs:928-931 plus 959 do not re-arm a failed reread, so the loss is permanent, including after restart. Without an attacker: a page closed for a week, then about 600 KiB of ordinary mail, can lose that batch once history passes about 2 MiB. The fixture perch/tests/fixtures/aoide/mail-read-reread.json:4 has "message": "9 entries", and recv.rs:15-21 and the README claim reread recovers everything, which assumes one copy. Upstream 0.0.26 has the same render (Aoide/pkgs/aoide/crates/client/src/commands.rs:4710, 5071), so the installed binary is affected too. The 64 KiB letter cap is the perch's own send cap; aoide's deposit cap was not checked, and a larger deposit makes all of this worse. CHECK: Probe on a temp AOIDE_ROOT with the installed aoide: deposit one ordinary and one 64 KiB control-character letter, run `mail read --for=minerva --json`, and compare stdout bytes with the message length. Fix the fixture to the real shape. Size the plain and reread caps to the real output or ask aoide for a seq window. Make a TooBig plain read set the reread flag again. Add tests with two control-character letters and with a history over the reread cap.
- [process: the wire shapes are unobserved, and X0 (the check that observes them) is scheduled after M2 (perch/tests/fixtures/aoide/README.md:28-32; pass2-final.json wave plan)] medium. The fixture README says the shapes follow 0.0.23 source, were never run against a real aoide, and that X0 is where a disagreeing fixture gets fixed. version.txt says 0.0.26. The plan has M2 in W4 and X0 in W7, so this slice reaches master before any live shape check. Gap 1 is the kind of defect X0 is meant to catch, and the executor's inventory admits 'all fixture shapes are modelled, not observed'. CHECK: Run the X0 shape probe before the merge (temp root only, no provider keys, no ~/.aoide). Otherwise record an operator decision that the receive side merges with unverified shapes, and say so in the spec.
- [outbox: entries past ENTRIES_MAX are reported as accepted (perch/src/mail/recv.rs:393-395 outbox_of take(ENTRIES_MAX); 410-420 current())] low. outbox_of keeps the first 2000 entries. current() returns 'accepted' for any row absent from the map once the outbox was fetched after the row was sent. A queued or refused row that is entry 2001 or later therefore shows 'accepted' on the page. It needs more than 2000 spooled entries, and the 1 MiB stdout cap may trip first, which is the safer failure. CHECK: Unit test in recv.rs test module: 2001 outbox entries with a row past the cut; expect unknown, not accepted. Mark a cut outbox as incomplete so current() never returns accepted from it.
- [notice path: aoide's message reaches GET /hub/mail `error` unsanitised (recv.rs:956-960 read(); aoide/mod.rs:477-484 Reply::sentence, 525-535 Display)] low. The read path formats `mail could not be read: {e}`, and Error::Failed displays Reply::sentence(), which clips aoide's envelope message to 1000 characters with no line_safe, keeping newlines and control characters. tick_once sets it as the notice (recv.rs:930, 943), and list() returns it as `error`. The send path passes the same text through line_safe and 300 characters (send.rs refused_by_aoide). The read side has no equivalent, and the reviewers' log-only check did not cover the GET field. CHECK: Pass the read notice through line_safe and a 300-character cap. Test with an aoide failure envelope whose message holds ESC and newlines, and check the GET error field.
- [runner environment: only AOIDE_* is removed; the rest of the perch's environment reaches every aoide child (perch/src/aoide/mod.rs:174-180)] low. The loop at 175-178 env_removes AOIDE_* keys, command.envs(env) adds the call's own variables, and there is no env_clear. Whatever the perch was started with, including provider keys and HTTP_PROXY, HTTPS_PROXY or ALL_PROXY, reaches aoide and its curl. A proxy variable in the operator's shell would send letter deposits to paired peers through that proxy. The inventory's runner-environment claim covers only AOIDE_* and does not say this. CHECK: Use env_clear with an allowlist (PATH, HOME, LANG, and the call's own AOIDE_* set). Add a test that a proxy variable and a provider-key-named variable set on the perch do not reach the fake.
- [spec vs code: 'a subject with a newline gets 400 with no spawn' (M2 spec tests; send.rs:147-148; tests/mail.rs:673-685)] low. The title is the first line, so a newline cannot reach the subject over HTTP. The test at tests/mail.rs:676-680 expects 202 with subject 'PlanB x' for a U+2028 in the title. The only newline 400 is a unit test that builds a Letter in code (send.rs unit test the_checks_that_cost_nothing_are_the_runners). The inventory's 'newline -> 400, no spawn' is false over HTTP, and the spec's named HTTP test does not exist. CHECK: Operator decision: 400, or a silent clean as built. Record it in the spec, then make the test and the inventory match.
- [spec vs code: 'no aoide call while base.jsonl is unchanged' (recv.rs:900 refresh_outbox before the gate at 905)] low. refresh_outbox runs before the stat gate. While a sent row waits, `mail outbox --json` runs about once a minute with base.jsonl unchanged, and the spec's absolute wording is false. CHECK: Reword the spec ('one outbox call a minute while a row waits'), or move the outbox behind the stat gate.
- [test coverage: nothing exercises a cap, a cut, or the lost flag (tests/mail.rs; recv.rs test module from line 1013; header mutation list tests/mail.rs:16-36)] medium (coverage). tests/mail.rs has no TooBig, REREAD_CAP, ENTRIES_MAX, or outbox-cut case (grep finds none), and recv.rs's own tests cover decode, clamp, time, receipts and outbox parsing but not the caps. The mutation list has no cap, no lost re-arm, and no reread-failure case. The 'nothing is lost' sentences (recv.rs:34-38; perch/src/mail/README.md) are therefore untested, and gap 1 passes every test as written. CHECK: Add one test each for: plain TooBig (lost, reread after the pause), reread TooBig (no re-arm), 2001 entries (cut), and an outbox over the cut. Add the cap and the lost re-arm to the mutation list and check each by hand.
- [page contract: remote text has no page consumer yet (webui/term/notes/cards.js, send.js stubs; webui/term/core/markdown.js; perch/src/mail/mod.rs:190-197 line_safe)] info, contract for N2b and M3. cards.js and send.js are stubs that return null, so the page does not yet render remote text. Card subject, body, sender, mailbox and saved notes are all remote-controlled. markdown.js allows only http(s) links, which is correct, but nothing in M2 proves the page uses it or text nodes. line_safe drops controls, LRM/RLM/ALM, BOM, bidi embeddings and isolates, but keeps U+200B-U+200D, U+2060-U+2064, variation selectors and Unicode tag characters (U+E0000-U+E007F). A subject or body can therefore carry text the operator cannot see, and an M3 agent that reads card text would see it. CHECK: When N2b and M3 land: page tests with </script>, <img onerror>, ESC and tag characters. Consider dropping Unicode format (Cf) characters in line_safe, or document that bodies must not reach agents as instructions.
- [send: no idempotency on the 202 path (perch/src/mail/send.rs send() 272-292 and send_note 126-150)] low, contract for N2b. Two identical POSTs send two letters and write two rows. The reviewers' idempotency suggestion covers only the 504 retry path. CHECK: The page debounces the send control, or the server refuses an identical in-flight note and node pair for a few seconds.
- [inventory claims that were not checkable by reading (perch/src/mail/send.rs:332-337 and 328; executor's review-M2/inventory.md)] info. The inventory says the ~/.aoide state listing hash was identical before and after the suite. That is the executor's own run and cannot be checked by reading. Its '502/504 after the letter may have gone writes an unknown row' holds for Timeout, TooBig and NotJson only; the 502 for Failed without an envelope writes no row (send.rs:328), which the confirmed list already records as send-signal-or-error-after-spool-no-row. CHECK: Ask the executor for the hash listing and the test log before merge. The 502 point is already on the confirmed list, so only the hash evidence is new.

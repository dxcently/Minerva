# M2 fix round: the review's findings plus the observed aoide 0.0.26 shapes

M2 is at e08e42d (base cd2b89f) in worktree agent-aba970ccc3baacf9f. The review is review-M2/result.md
(34 confirmed, critic merge-after-fixes). This file is the fix list. Where it differs from M2.json, this file wins.

## What was observed (installed aoide 0.0.26, temp root, self-mail, env -i)

Raw outputs: scratchpad/aoide-probe/checker/ (r-*.out, v-*.out, audit-*.log, analysis.txt). They hold a throwaway
identity: build fixtures from their STRUCTURE with made-up values (names, msgids, sigs, node names); never copy the raw files.

1. `mail read --json` prints each letter twice: in the top-level `message` string and in `data.entries[i].envelope.text`.
   Both copies are JSON-escaped. An empty read is about 139 B with message "nothing new" and no entries.
2. `--reread` reprints the WHOLE history for the name and moves the cursor to the newest entry: it consumes unread
   letters exactly like a plain read. It never rewinds. Its output grows with the mailbox's whole history.
3. `mail send --json` has two shapes, and neither has a top-level msgid, `disposition` or `delivery`:
   - without --subject: `data = {envelope:{header:{from,to,mintedAt,originMesh,type,version}, msgid, sig, text}, receivedAt, ring, seq, type:'letter', via}`;
   - with --subject (THE PERCH ALWAYS SENDS ONE): `data = {accepted:<n>, complete:<bool>, recipients:[{accepted, address:{name,node}, delivery:<obj|null>, error:<obj|null>, msgid, ring, role:'to', status:'filed'|...}], replyTo, subject, threadId}`.
   - Top-level keys in both: status, command 'mail.send', message, gated, changed, data. The word `disposition` occurs in neither.
   - With --subject the stored letter text is `AOIDE-LETTER/1` followed by a JSON object {subject, to, cc, body, threadId}.
4. `aoide --version`, `version`, `-V` and `version --json` all exit 2 with usage on stderr. The ONLY version source is
   `aoide schema` (no flag needed): a JSON document of about 148 KB whose top-level `"aoide"` is the version
   ("0.0.26"), with "schemaVersion":"0" and `commands:[{path:[...], summary, args, flags:[{name,...}]}]`.
5. `mail` flags present in 0.0.26: send json,to,hold,from,subject,thread,reply-to,cc,mesh; read json,for,all-names,reread;
   outbox json plus an optional node. An empty self outbox is `data = {entries: [], summary: {}}`.
6. aoide's audit log: every line is exactly {ts, door, class:'audit', command, status, message}. `mail.read` lines carry
   letter text in `message` (cut at about 526 characters). (This matters to R2c, not to you.)
7. A stale aoide 0.0.25 is first on PATH in a login shell; perch-open passes `--aoide $HOME/.local/bin/aoide` (0.0.26), so
   the perch is unaffected. The perch must keep refusing a non-absolute --aoide.

Not observed (no paired node): a remote send, `--hold`, a non-empty outbox entry. Keep those fixtures as modelled and say so.

## The fix list

Receive path:
- R1 (medium). Fix the read fixtures to the observed shape (both copies; `message` holds the bodies). Size the caps to
  the real output and write the formula in perch/src/mail/README.md (about 2x the letter bytes, 12x for control bytes,
  plus per-entry overhead). Plain read 4 MiB, reread 16 MiB, unless you show a reason for other numbers.
- R2 (medium). A plain read that fails or is TooBig: its batch is already marked read, so set the reread flag again.
  A reread that is TooBig can never succeed later (its output only grows): do NOT re-arm it; raise one notice card,
  through line_safe, saying letters for minerva could not be read here and to run
  `aoide mail read --for=minerva --reread` in a terminal. A reread that fails for another reason (exit, timeout, not JSON)
  re-arms, at most once per minute. Correct the "nothing is lost" sentences (recv.rs header, README) to say exactly this.
- R3 (medium). Reread consumes unread letters (observed): every entry a reread returns is processed like a plain read's,
  deduplicated by msgid against what is already stored (also closes files-4, received.jsonl duplicates).
- R4 (medium). receive-entries-cap-drops-newest (2000): keep the newest, not the oldest, or store all within the cap
  and say which are dropped; never silently drop the newest.
- R5 (medium). receive-store-full-stuck: per the review's scenario and fix.
- R6 (medium). files-2: a torn last line in a .jsonl (no trailing newline) is never parsed as a record and the next
  append starts on a new line.
- R7 (low). The read notice goes through line_safe and a 300-character cap, like the send path.
- R8 (low). Outbox past ENTRIES_MAX: a cut outbox is marked incomplete, and current() never answers 'accepted' from it
  (unknown instead).
- R9 (low). receive-sender-spoof: the card shows the node beside the sender name.
- R10 (low). Dismissed cards stay dismissed when cards.json is set aside (tombstones by msgid, bounded).
- R11 (low). files-3 (open cards.json with the regular-file helper), files-7 (folder create race), files-9 (a symlinked
  folder refused on reads too), files-10 (two perches on one home: an flock on the mail state dir; the second refuses
  mail with a sentence).

Send path:
- S1 (medium). send-timeout-after-spool double send: aoide spools, then POSTs for up to about 15 s, and the perch kills it
  at 10 s. Give the send call a timeout longer than aoide's own (30 s), and on any timeout, signal or unparsable answer
  AFTER the spawn, write an 'unknown' row whose page sentence says to check the outbox before sending again. Never a
  plain 504 that invites a resend. (Also closes send-signal-or-error-after-spool-no-row.)
- S2 (medium, from the probe). Parse the --subject shape: per recipient, msgid = data.recipients[i].msgid,
  status = .status, delivery = .delivery, error = .error; data.complete and data.accepted for the whole send. Rows and
  receipts key on the recipient's msgid. Fix send.json to the observed --subject shape (made-up values) and keep a
  no-subject fixture only to prove the perch rejects or never asks for it. Remove every read of `disposition`.
- S3 (low). A send concurrency cap: at most 2 sends in flight, else 429 with a sentence.
- S4 (low). Letter and Row get a hand-written Debug that never prints the text or subject.
- S5 (low). unknown rows can be resolved: the outbox or a later receipt settles an unknown row by msgid; an unknown row
  with no msgid says so and offers nothing to retry.
- S6 (low). Orphan bound: the aoide child gets PR_SET_PDEATHSIG (SIGKILL) via pre_exec, so a perch that dies takes it
  along, ONLY if the perch already has the means (libc in perch/Cargo.toml, or an existing pre_exec helper to reuse):
  Cargo.* is never edited; without the means, skip and say so. Either way the README says the text is in argv
  (visible in /proc to the same user) because aoide reads no body on stdin.
- (files-10's lock: std's File::try_lock if the toolchain has it, else the same rule: skip and say so.)

Runner (perch/src/aoide/mod.rs):
- V1 (medium, master bug the fake hid). The version check runs `aoide schema` (stdout cap 512 KiB, the usual timeout),
  parses JSON and reads the top-level "aoide" string. Then it checks that the schema lists every command and flag the
  perch sends (mail send: json,to,from,subject,mesh,hold; mail read: json,for,reread; mail outbox: json; mesh: json).
  A missing one turns mail (or mesh) off with a sentence naming the command and flag. fake_aoide: `--version` exits 2
  with a usage line on stderr like the real one, and `schema` prints a small fixture with the same top-level fields
  and only the commands the perch uses (made-up summaries). Update the fixture README.
- V2 (low). env_clear, then an allowlist: PATH, HOME, LANG, LC_*, TZ, plus the call's own AOIDE_* set. No proxy
  variables (letter deposits must not go through an operator's proxy), no provider keys. A test sets HTTP_PROXY and
  OPENAI_API_KEY on the perch and checks the fake sees neither. NOTE for the orchestrator's merge: slice R2c (separately)
  makes the runner pass AOIDE_AUDIT_LOG when the perch was given --aoide-audit-log; keep the allowlist a single list so
  that merge is one line.

Spec drift (record in M2.json's notes and the inventory, no behaviour change):
- D1. A newline in the subject cannot arrive over HTTP (the title is the first line); the behaviour is a silent clean,
  as built. The inventory's 'newline -> 400' is corrected.
- D2. The outbox refresh runs before the base.jsonl stat gate; the spec's sentence becomes 'one outbox call a minute
  while a row waits'.

Tests (each would fail if its rule broke; add the cap and re-arm rules to the mutation list in tests/mail.rs's header):
plain TooBig re-arms the reread; reread TooBig raises the notice and does not re-arm; reread of a history re-delivers
nothing twice; 2001 entries; an outbox past the cut answers unknown; the --subject send shape gives the recipient's
msgid to the row; a timeout after spawn writes an unknown row and no 504; the third concurrent send is 429; schema
version and a missing flag; --version exit 2 is not an error any more; the env allowlist; a torn last line.

## Fixer status (second round, commit bd1f05a)

Done: R1-R11, S1-S6, V1 (mail half), V2 and D1/D2 as recorded. Each item's file:line and test is in review-M2/inventory.md. Suite 648 passed, 0 failed, 2 ignored; clippy clean both ways; the folder tests 25/25.

Deferred (not in the grant, or the mesh owner's):
- V1 mesh half: the mesh gate is not wired (mesh/mod.rs:216-217, :220 call only aoide.version()).
- tests/mesh.rs changed in bce45f6, out of grant.
- aoide/mod.rs:682 maps a child.wait() error to Spawn, so a started child gets no row.
- Relative --aoide-home: refused by the mail gate only (main.rs is out of grant).

Deviations recorded:
- The notice card is shown as the page's error string (same words, 300-character cap), not a separate card.

Open (product notes, not fixed):
- Card window: GET /hub/mail shows only the last 2 MiB of received.jsonl. Letters beyond it are kept on disk but not shown (mail/README.md).
- Listening before the lock: the receive task starts before the mail lock is taken (mail/README.md).
- SentRow still derives Debug (S4 residual).
- receive-base-size-unbounded, receive-save-not-idempotent, receive-body-copy-before-clip, and the reread-failure attempt count in the notice.
- D1 and D2 need user ratification before merge.

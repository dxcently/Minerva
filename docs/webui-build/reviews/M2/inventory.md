# M2 inventory: final HEAD e71bcc6 (fifth fix round)

## Identity

- HEAD: e71bcc6, "perch: M2 fifth fix round: the post-run mapping is a function with a test". Not pushed. Master not touched
  (master is b08a32e; this round did not use it). Every cite below is a line at e71bcc6. Sources that this round did not change
  (aoide/mod.rs, mail/mod.rs, recv.rs, tests/mail.rs, the mail README's cited lines) have the same numbers as at 74ae824.
- Branch: worktree-agent-aba970ccc3baacf9f. Worktree (Windows): C:/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f.
  Worktree (WSL): /mnt/c/Users/dxcen/Projects/Minerva/.claude/worktrees/agent-aba970ccc3baacf9f.
- Base: cd2b89f. `git log --oneline cd2b89f..HEAD` lists seven commits, oldest first:
  - e08e42d, the M2 build (the "base e08e42d" in older notes is this commit, the M2 review's base)
  - bce45f6, the first fix round
  - fdb2098, the second fix round (mail fix round, store bounds, signalled sends, linked folders)
  - bd1f05a, the second fix round, caps pinned, lock and send branches
  - fbff34b, the third fix round (M2-fix3, recorded in its own section below)
  - 74ae824, the fourth fix round: the verifier's gaps on fbff34b
  - e71bcc6, the fifth fix round (this round): the verifier's gaps on 74ae824 (the list is in the round 5 section)
- This round builds on 74ae824, which is the HEAD the gap cites were checked against. The fixer brief named bd1f05a as the base and as the
  place the commit goes; the worktree was at 74ae824 when the round began. bd1f05a and fbff34b are ancestors of it, and nothing was rebased.
- Spec and review: scratchpad\slices\M2-fix.md (R1-R11, S1-S6, V1-V2, D1-D2), scratchpad\slices\M2-fix3.md (round 3, and its grants) and
  scratchpad\review-M2\result.md. Slice record: scratchpad\slices\M2.json, notes.
- Files changed since bd1f05a: perch/src/aoide/README.md, perch/src/aoide/mod.rs, perch/src/bin/fake_aoide.rs, perch/src/mail/README.md,
  perch/src/mail/recv.rs, perch/src/mail/send.rs, perch/tests/mail.rs. Nothing under perch/src/mesh/ has changed. Every file is inside a grant
  except perch/src/aoide/README.md: fbff34b added the Started entry to its Error list (docs for item 1's variant). No grant names that file, so
  it is for the orchestrator to ratify; it was not reverted, because the README would then misdescribe the variant.

## Verification at e71bcc6

Run in WSL, CARGO_TARGET_DIR under ~/.cache/minerva-M2-target. Nothing was run against the operator's perch, ~/Minerva, ~/.config or ~/.aoide.
No real aoide ran, and no model was called. Only the fake_aoide and the test doubles were run.

- `cargo test --features test-bins --offline --no-fail-fast` in perch/: exit 0. Total 655 passed, 0 failed, 2 ignored; zero compiler warnings.
- Per binary: lib 296; fake_aoide 0; fake_eidolon 4; fake_minerva 0; perch main 4; agent 32; allow 1; aoide 24 (2 ignored); deps_smoke 1;
  gpl_boundary 2; guard 1; http 132; install 2; mail 52; mail_agent 1; manual_policy 4; mesh 20; meta 18; notes 19; runs 38; term 1;
  term_agent 1; term_stream 1; watch 1; doc-tests 0. The per-binary numbers sum to 655.
- tests/mail.rs, five runs in a row (`--test mail`): 52 passed, 0 failed in each (12.61 to 12.95 s each).
- a_linked_mail_folder_is_read_by_nothing: 25 single repeats, 25 pass, 0 fail (flake rate 0 of 25 at e71bcc6, and 0 of 5 full mail runs).
- Clippy: `cargo clippy --all-targets --offline -- -D warnings` exit 0, zero warnings. `cargo clippy --all-targets --features test-bins --offline -- -D warnings`
  exit 0, zero warnings.
- Clippy at e71bcc6: `cargo clippy --all-targets --offline -- -D warnings` exit 0; `cargo clippy --all-targets --features test-bins --offline -- -D warnings`
  exit 0. Zero warnings both ways.
- Count history: fdb2098 638; bd1f05a 648 (lib 293, mail 48); fbff34b 652 (lib 294, mail 51); 74ae824 654 (lib 295, mail 52); e71bcc6 655
  (lib 296, mail 52). Round 5 adds one lib unit test, a_started_run_that_could_not_be_followed_writes_one_row_and_a_spawn_failure_writes_none
  (src/mail/send.rs:755). Round 4 added two (see the round 4 section).
- Width: the lines this round adds are within 140 columns. The unknown closure's long format line (send.rs:377) is the 74ae824 line, moved.
- Line endings: the worktree sources and the index are LF (git ls-files --eol reports i/lf and w/lf). git status is clean at e71bcc6.

## Findings and where each is handled (HEAD e71bcc6)

Line numbers are for e71bcc6. Test line numbers are in perch/tests/mail.rs unless marked.

| ID | Where | Test |
|---|---|---|
| R1 | Fixtures to the observed aoide 0.0.26 shape (tests/fixtures/aoide/). Caps: src/mail/recv.rs:86 PLAIN_CAP (4 MiB), :87 REREAD_CAP (16 MiB). Formula and provenance: src/mail/README.md "Caps" | letters_become_cards_decoded_dated_and_attributed (1121) |
| R2 | recv.rs tick_once, 1125-1196. Constants PAUSE :102 (30 s), REREAD_GAP :105 (60 s). A failed plain read re-arms: task.reread = true at :1196, inside `if !task.reread_dead`. A reread too big to print sets task.reread_dead (:1175) and a permanent notice. Another reread failure sets task.reread_after = now + REREAD_GAP (:1181). task.reread_after is cleared on success at :1169 | a_plain_read_too_big_re_arms_the_reread_and_the_reread_keeps_the_letters (1481); a_plain_read_whose_store_write_fails_re_arms_the_reread_and_keeps_each_letter_once (1521); a_reread_too_big_to_print_is_a_loss_said_on_the_page_and_never_armed_again (1504); a_reread_that_fails_for_another_reason_is_tried_again_after_the_gap (1549) |
| R3 | A reread's entries take the plain read's path: deduped by msgid against received.jsonl (recv.rs:645 ids_on_file) and against cards (recv.rs:672, :941, :964) | a_plain_read_too_big_re_arms_the_reread_and_the_reread_keeps_the_letters (1481); a_reread_of_a_history_re_delivers_nothing_twice (1815) |
| R4 | Every entry is kept: recv.rs:406 parse_entries does not cut by position (2001 letters = 2001 cards, 25 shown) | two_thousand_and_one_letters_are_all_kept_and_the_page_counts_the_rest (1611) |
| R5 | Store bounds: recv.rs:82 MAX_CARDS (5000 live cards; dismissed do not count), :83 RECEIVED_CAP (32 MiB). Check at recv.rs:712-717. Stop sentence at :1008-1009. Reads resume once under the bound: dismiss, or move received.jsonl aside. Moving cards.json aside needs a restart | a_full_store_stops_the_reading_and_says_so (1425); a_store_full_of_letters_goes_on_once_the_file_is_moved_aside (1451) |
| R6 | src/mail/mod.rs:174 append_lines: a last line with no newline is never a record; the next append starts a new line; sync_all at :203 | unit: a_torn_last_line_is_never_a_record_and_the_next_append_starts_on_its_own_line (src/mail/mod.rs:390) |
| R7 | Read notice through line_safe at NOTICE_MAX (recv.rs:110, 300 characters), set_notice at :1105-1109 and set_lost at :1111-1114. Round 3: list() at recv.rs:798 joins the loss and the notice with sentences() (:1118-1120), capped at NOTICES_MAX (:112, 600). Round 4: the snapshot-error arm at :799 joins the same two and the snapshot's own sentence | a_notice_is_one_printable_line_of_at_most_300_characters (1728); a_full_store_is_shown_beside_the_loss_of_a_reread_too_big_to_print (1573); a_loss_is_still_shown_when_the_store_cannot_be_read (1593) |
| R8 | Outbox cut at recv.rs:89 OUTBOX_MAX (2000): take at :480, complete = listed.len() <= OUTBOX_MAX at :489. A row the cut outbox does not list is unknown, never accepted | unit: a_cut_outbox_cannot_say_that_a_row_left_it (src/mail/recv.rs:1500) |
| R9 | Card shows the mailbox on the node and the sender as claimed | letters_become_cards_decoded_dated_and_attributed (1121); what_a_sender_chose_is_clamped_and_never_trusted (1185) |
| R10 | Tombstones: dismissed.jsonl, one msgid per dismissed card. recv.rs:93 TOMBSTONES_KEEP (10,000), :94 TOMBSTONES_WINDOW (1 MiB), read at :575-577 | unit: a_tombstone_keeps_a_letter_dismissed_when_cards_json_is_gone (src/mail/recv.rs:1522) |
| R11 | Mail folder: mail/mod.rs:145 and :155 (0700), :161 readable_dir, :174 and :180 (O_NOFOLLOW \| O_NONBLOCK \| O_CLOEXEC), the lock field at :79 (`lock: OnceLock<std::fs::File>`; :78 is its doc comment). recv.rs:790 list() answers a link or non-directory with a sentence and reads nothing; recv.rs:1129 tick_once checks the folder before any read | the_mail_folder_is_private_and_a_planted_link_or_a_loose_file_is_not_followed (1005); a_linked_mail_folder_is_read_by_nothing (1761); a_fifo_where_a_file_goes_is_an_error_sentence_and_not_a_hang (1404); a_cards_file_that_does_not_parse_refuses_mail_until_it_is_moved_aside (1379); unit: a_second_lock_on_the_same_folder_is_held_out_and_a_lock_given_back_is_taken (src/mail/recv.rs:1512) |
| S1 | send.rs deliver 356-362 and settle 368-422 (round 5 moved the post-run match into settle; the guard arm and the row writes are in it). SEND_TIMEOUT aoide/mod.rs:61 = 45 s; its doc at :58-60 says aoide itself POSTs under a 30 s timeout. After spawn, a timeout, a too-big answer, an unusable answer, a signal, or a started child that could not be followed writes an unknown row and answers 502 "may or may not have been sent". The guard arm is send.rs:413-415, decided by unknown_row_after_spawn (:439-444; Started at :441). A spawn failure writes no row (send.rs:420). No 504. A child whose wait fails is Started (aoide/mod.rs:614 variant, :628 Display, :658 no pipes, :689 wait), not Spawn | an_answer_that_never_came_may_have_gone_and_leaves_a_row_that_says_so (968); a_send_that_times_out_after_aoide_started_is_an_unknown_row_and_no_resend (1636); a_send_ended_by_a_signal_after_aoide_started_is_an_unknown_row (1746); unit: a_child_that_started_and_cannot_be_followed_is_started_and_not_spawn (src/aoide/mod.rs:1043); unit: a_run_that_may_have_spooled_is_an_unknown_row_and_one_that_never_started_is_not (src/mail/send.rs:737); unit: a_started_run_that_could_not_be_followed_writes_one_row_and_a_spawn_failure_writes_none (src/mail/send.rs:755) |
| S2 | parse_took, send.rs:462-490, reads data.recipients[0] (msgid, status, delivery, error, accepted) and the send's data.complete. send.rs:382 treats recipients[0].accepted false as a refusal; :386 an answer with no msgid is an unknown row; :392 an incomplete send keeps the row with a warning. row.disposition is the perch's own copy of the recipient's status word; recv.rs reads it at :497 (current) and :519 (waiting). aoide's `disposition` key is read nowhere. send.rs:7, and mail/README "Files" (the sent.jsonl paragraph, lines 76-81) say so; the one-recipient sentence is at README :80-81 | unit: a_letter_aoide_took_is_read_from_the_recipient_of_its_subject_shape (src/mail/send.rs); a_note_sent_to_this_machine_is_one_exact_call_and_one_row (751) |
| S3 | mail/mod.rs:57 SENDS_AT_ONCE = 2; take_send_slot at :97-99; the third send is 429 at once (send.rs:323). Nothing run or written | many_sends_at_once_write_whole_lines (989) |
| S4 | Hand-written Debug, no words: Letter send.rs:186; Row recv.rs:202; SentRow recv.rs:241 (struct at :219, derive is Clone, Serialize, Deserialize). Round 3 closed the residual: SentRow was the last derived Debug holding subject and note | unit: a_letter_prints_its_sizes_and_never_its_words (src/mail/send.rs:711); unit: a_letter_row_prints_its_sizes_and_never_its_words (src/mail/recv.rs:1365, extended with SentRow: no subject or note words; subject_chars 13, note_chars Some(10)) |
| S5 | recv.rs current() at :495-509. An unknown row with a msgid is settled by a receipt or the outbox (complete and absent = accepted). Without a msgid it says so and offers no retry. Rows of 502 carry no msgid, so no receipt or outbox answer settles them. The no-msgid rows are written at send.rs:387 (an answer with no msgid), :409 (a failed run that is not certain) and :414 (a run that started and gave no answer); the closure that builds their 502 is send.rs:376. A 202 whose recipient gave neither a delivery.status nor filed, held or spooled is `unknown` but keeps its msgid, so the outbox can still settle it (send.rs:482-488 status mapping, :390 row). Rule text: mail/README "Every refusal", the 502 paragraph (lines 55-65) | a_letter_that_is_still_in_the_outbox_shows_aoides_status_and_reason_and_a_forged_receipt_changes_nothing (1274); a_receipt_from_the_node_a_letter_went_to_marks_it_delivered_and_the_outbox_says_the_rest (1246); an_answer_with_no_msgid_is_an_unknown_row_and_no_resend (1893) |
| S6 | src/aoide/mod.rs pre_exec, 224-238: PR_SET_PDEATHSIG(SIGKILL) at :230, then getppid() against the perch's pid at :233 (ESRCH if the perch is already gone). The closure allocates nothing (comment :224-227). Argv doc at aoide/mod.rs:26; the signal is tied to the thread that started the child (aoide/README.md) | the_child_dies_with_the_perch_that_started_it (tests/aoide.rs; runs inner_perch_that_starts_a_sleeper, ignored, in a copy of the test binary) |
| V1 | aoide's version is the top-level "aoide" of `aoide schema` (aoide 0.0.26 has no --version; version() at aoide/mod.rs:166). schema_check at :174 checks USED (:311, four entries incl. `mesh --json`); missing() at :319 names the sentence. Mail gate: mail/mod.rs:266 schema_refusal, asked at most once a minute. NARROWED: mail only. The mesh half is open (blocker a) | a_schema_without_a_flag_the_perch_sends_turns_mail_off_with_the_sentence (1673); a_schema_without_the_mesh_command_refuses_mail_too (tests/aoide.rs); unit: a_missing_command_or_flag_is_named_in_the_sentence (src/aoide/mod.rs) |
| V2 | Env: cleared, then ENV_KEEP (aoide/mod.rs:68, `["PATH", "HOME", "LANG", "TZ"]`), ENV_KEEP_PREFIX (:70, "LC_") through kept_env (:305), then env_for's AOIDE_* set (:271). A relative path is refused with "{name} must be an absolute path" (:281) | the_proxy_and_the_provider_key_of_the_perch_do_not_reach_aoide (1694); unit: kept_env (src/aoide/mod.rs) |
| D1 | No code change: the subject is one printable line, whatever the note starts with (a newline cannot arrive, the title is the first line). Ratified 2026-10-08 (M2.json) | a_subject_is_one_printable_line_whatever_the_note_starts_with (841) |
| D2 | No code change: the outbox refresh runs before the base.jsonl stat gate, so the rule is one outbox call a minute while a row waits (mail_ttl_ms). Ratified 2026-10-08 (M2.json) | no test: the rate is mail_ttl_ms |

Relative --aoide-home (round 2 item, unchanged): every mail route answers 503 with the sentence "AOIDE_ROOT must be an absolute path" before aoide runs
(send.rs:328 via usable(), mail/mod.rs:245; the sentence from aoide/mod.rs:281). Test: a_relative_aoide_home_is_refused_before_aoide_runs (1715).
The startup check in main.rs covers --aoide and --minerva only (main.rs:129-130, definition :186); a relative --aoide-home is left to the mail gate
(blocker d).

Linked or non-plain mail folder: GET /hub/mail answers "not a plain directory" (recv.rs:790); nothing is read or written behind the link (R11).

## Round 3 changes (fbff34b; where each is now at e71bcc6)

| # | Change | Where now (e71bcc6) | Test |
|---|---|---|---|
| 1 | A started child whose wait fails, or that has no output pipes, is Error::Started, not Spawn. send.rs writes an unknown row for it | aoide/mod.rs:614, :628, :658, :689; the send.rs guard arm at :413-415 (:397-399 at 74ae824, :400 at fbff34b); the predicate at :439-444 | unit: a_child_that_started_and_cannot_be_followed_is_started_and_not_spawn (src/aoide/mod.rs:1043); unit: a_run_that_may_have_spooled_is_an_unknown_row_and_one_that_never_started_is_not (src/mail/send.rs:737); unit: a_started_run_that_could_not_be_followed_writes_one_row_and_a_spawn_failure_writes_none (src/mail/send.rs:755) |
| 2 | Two re-arm tests name the lines they need | recv.rs:1196 (task.reread = true) and :1181 (task.reread_after) | a_plain_read_whose_store_write_fails_re_arms_the_reread_and_keeps_each_letter_once (1521); a_reread_that_fails_for_another_reason_is_tried_again_after_the_gap (1549) |
| 3 | The GET error shows every applicable sentence, joined and capped | recv.rs:798 list() (the success arm); :799 (the snapshot-error arm, round 4); sentences() :1118-1120; NOTICES_MAX :112 (600) | a_full_store_is_shown_beside_the_loss_of_a_reread_too_big_to_print (1573); a_loss_is_still_shown_when_the_store_cannot_be_read (1593) |
| 4 | SentRow has a hand-written Debug that prints sizes only; the S4 test covers it | recv.rs:219 (struct), :241 (impl), the subject_chars field at :252 | unit: a_letter_row_prints_its_sizes_and_never_its_words (src/mail/recv.rs:1365) |
| 5 | Docs: Caps provenance (mail/README "Caps", "Where those numbers come from" at :136); the S5 rule (mail/README 502 paragraph, :62-64); aoide's own timeout 30 s (aoide/mod.rs:58-60); aoide/README "What comes back" lists Started (:81); fake_aoide's header names env_names (src/bin/fake_aoide.rs:29-30, the field at :167) | as listed | n/a (docs) |
| 6 | This inventory, rewritten for HEAD | scratchpad\review-M2\inventory.md, rewritten in round 4 for 74ae824 and in round 5 for e71bcc6 | every cite above re-read at e71bcc6 |

Deviations, recorded: round 3 item 1 also maps the no-pipe check in drive() to Started (the same class, inside the granted function). The GET error
joins sentences with "; " and caps the total at NOTICES_MAX (600), a design choice not in the spec.

## Round 4 changes (74ae824: the verifier's gaps on fbff34b)

| # | Change | Where | Test |
|---|---|---|---|
| A | Gap 1: the unknown-row rule is one predicate. unknown_row_after_spawn is an exhaustive match over Error, and the arm that writes the unknown row is guarded by it. Spawn and NoBinary keep their no-row answers (502 and 503) | send.rs:397-399 (guard arm), :402 (NoBinary), :404 (Spawn catch-all), predicate :423-428 at 74ae824 (e71bcc6: :413-415, :418, :420, :439-444) | unit: a_run_that_may_have_spooled_is_an_unknown_row_and_one_that_never_started_is_not (src/mail/send.rs:721 at 74ae824, :737 at e71bcc6): true for Timeout, TooBig, NotJson, Started; false for NoBinary, Refused, Spawn, Failed |
| B | Gap 3 (item 3, judged a real gap): when the snapshot fails, the GET error still shows the lost sentence and the notice, joined with the snapshot's own sentence through sentences() (each line at most 300, the total at most 600) | recv.rs:799 | a_loss_is_still_shown_when_the_store_cannot_be_read (tests/mail.rs:1593). Applied, not left as the narrower reading: "every sentence that applies" covers a failed snapshot too, and without this the lost letters go unsaid while the page shows only the snapshot error |
| C | Gap 2 (ratified S2): mail/README.md states that data.recipients[0] is the whole send's answer: its accepted and data.complete | mail/README.md:80-81 (the sent.jsonl paragraph, after the disposition sentence at :79) | n/a (docs); S2 cites the sentence |
| D | Gap 4 (item 6): the inventory's S1 and S5 cites and the route table moved to the 74ae824 numbers. The verifier's :399-400 was the fbff34b numbering, and the guard arm moved. M2.json note 4 cites recv.rs:497 and :519 (the disposition reads), not :476 and :498 | this file; M2.json notes | n/a |

Deviations, recorded: gap 1 uses a named exhaustive predicate rather than the matches!-with-catch-all form the verifier wrote. With the predicate
written as a match over every Error variant, a new variant is a compile error until it is decided, where a wildcard would let it fall through. The
deliver match keeps one catch-all, Spawn (send.rs:404 at 74ae824, :420 at e71bcc6), after the guard arm and NoBinary. The row write inside the guard
arm (send.rs:398 at 74ae824, :414 at e71bcc6) was not reached by any test at 74ae824. Round 5 added a unit test that reaches it, and the two
integration timeout tests reach it too (M6 below); see risk 7.


## Routes and answers

Full refusal table: src/mail/README.md "Every refusal" (lines 39-70). Codes and their sources:

| Route | Answers |
|---|---|
| POST /hub/notes/<n>/send | 202 with the recipient copy's msgid (send.rs:159); 400 bad body or subject (:125, :217); 404 no home or no note (:137, :303); 409 unknown or unpaired node, ambiguous mesh with the meshes list, or aoide's certain refusal before filing (:221, :527, :530); 413 note over 64 KiB (:305, :308); 429 two sends already running (:323); 502 no row when spawn failed (:420), or unknown row after spawn: timeout, signal, too much output, unusable answer, no recipient copy, or a started child that could not be followed (:376, :387, :409, :414, :533); 503 no aoide, schema missing a command or flag, relative --aoide-home, or another perch holds the lock (:318, :328, :418). All send.rs cites are at e71bcc6 |
| GET /hub/mail | page of cards and sent rows (recv.rs list() :783-807); an error sentence (300 characters at most a line, joined at most 600) for a full store, a reread too big to print, a cards file that does not parse, a mail folder that is a link or not a plain directory, a lock held elsewhere, or a snapshot that cannot be read (the loss and the notice are shown beside it, :798-799) |
| POST /hub/mail/<msgid>/place, /dismiss, /save | unchanged from M2 |
| GET /hub/mail/nodes | unchanged from M2 |
| all mail routes | 503 with a sentence when the mail folder is refused, the lock is held, or --aoide-home is relative |

## Never exposed

- The text of a sent note: sent.jsonl holds its sha256 only (mail/README "Files").
- argv is not logged. The runner logs the verb and the exit code of each aoide call (aoide/mod.rs:198-201), never an argument, the text, or what aoide printed. Shape and Reply print sizes only.
- Letter, Row and SentRow print no text or subject under Debug (send.rs:186, recv.rs:202, recv.rs:241).
- The perch environment (proxies, provider keys, a conducted session's AOIDE_SESSION_ID) does not reach aoide. Only the reader identity is set, and only on a mail read (aoide/mod.rs:68, :70, :271, :305).
- A card's text is drawn as text. A card's sender is what the letter claims, shown beside the node.
- Tombstones keep message ids only (mail/README "Files").

## Risks accepted

1. Argv visibility. A letter's text is in argv while aoide runs, so any local account that can read /proc/<pid>/cmdline can read it for that moment. The cap is 64 KiB (send.rs:305). aoide 0.0.26's schema gives no stdin body shape, so there is no other shape to send it in.
2. Reread loss is permanent past 16 MiB. Once the history no longer fits a reread (REREAD_CAP, recv.rs:87), letters a plain read consumed and the perch did not keep cannot be recovered by the perch. The page says so (recv.rs:1008-1009) and names `aoide mail read --for=minerva --reread` for a terminal (mail/README "Caps"). The perch keeps none of them.
3. cards.json refusal. A cards file that does not parse is not set aside automatically: mail reads refuse until the operator moves it aside by hand, then the perch restarts (it is read at start). This prefers an outage to a silent reset that would bring dismissed letters back (tombstones cover that case, but the refusal is the safer default). Test: 1379.
4. Lock. One perch per home, an flock on <home>/mail/.lock (mail/README "Files"). A second perch answers its mail routes with a sentence (503; test a_second_perch_on_one_home_is_refused_mail_and_reads_none, 1860). The lock goes with the process, so a crash does not leave it held.
5. Env allowlist. A variable a call newly needs must be added to ENV_KEEP (aoide/mod.rs:68). Anything not in it is dropped. AOIDE_* names are set only by env_for (:271).
6. PR_SET_PDEATHSIG (S6, aoide/mod.rs:230) has one test, the_child_dies_with_the_perch_that_started_it (tests/aoide.rs, about 5 s). The getppid() check (:233) is covered by reading only.
7. A run that started and gave no answer gets an unknown row (round 3 made Started a row; round 4 made the row a named rule; round 5 moved the post-run match into settle). The rule is unknown_row_after_spawn (send.rs:439-444), an exhaustive match with its own unit test. The arm that writes the row (send.rs:414) is reached in two ways at e71bcc6: the unit test a_started_run_that_could_not_be_followed_writes_one_row_and_a_spawn_failure_writes_none (src/mail/send.rs:755) calls settle with Error::Started, and two integration tests run it through the route: tests/mail.rs:968 (an answer that is not JSON, or more output than the cap: NotJson and TooBig) and :1636 (a timeout after aoide started). Round 5 M6 showed each one fails when the write is removed, so the earlier "reached by no test" (74ae824 wording) was wrong for these paths. What is still untested is the Started wiring in deliver(): the wait error that becomes Error::Started is in aoide::drive (aoide/mod.rs:689), reached only by the unit test a_child_that_started_and_cannot_be_followed_is_started_and_not_spawn (src/aoide/mod.rs:1043), and no integration test forces a wait failure through the send route.
8. An unknown 502 row has no msgid, so the outbox cannot settle it (S5). The page tells the person to check `aoide mail outbox` before resending.

## Blockers and out-of-grant items (status at e71bcc6)

| | Item | Status |
|---|---|---|
| a | Mesh gate not wired. mesh/mod.rs:216-217 version_of calls aoide.version() (aoide/mod.rs:166, which runs `aoide schema`) with no schema_check; mesh_of (:220) runs `mesh --json` with no check. A schema without the mesh command does not refuse the mesh view | OPEN (mesh owner) |
| b | tests/mesh.rs changed in bce45f6 (the wrapper and the `schema` argv) | RESOLVED by ratification 2026-10-08 (M2.json). No further edit |
| c | aoide/mod.rs drive() mapped a child.wait() error to Spawn (no row) | RESOLVED in round 3: :689 maps it to Started; the send.rs guard arm at :413-415 (:397-399 at 74ae824) writes the row |
| d | Relative --aoide-home: refused by the mail gate only. main.rs:129-130 and :186 cover --aoide and --minerva only; mesh/mod.rs:153 drops a relative --aoide-home to None silently, so mesh and mail disagree | OPEN (main.rs and mesh owners) |
| e | Stale references to the old --version design. mesh/mod.rs:13 doc link names Shape::Version, which no longer exists (aoide/mod.rs:387 is the Shape enum; no Version variant); mesh/mod.rs:31-32 says --version is cached; :217 the user-visible error "aoide --version: {e}" when the schema call fails (no test asserts it); :536 "its --version". mesh/README.md:26, :73, :83, :112 | OPEN (mesh owner) |
| f | D1 and D2 need user ratification | RESOLVED by ratification 2026-10-08 (M2.json notes) |

Note on e: the schema call is the one behind `version()`, so the user-visible text is wrong now, not the behaviour.

## Mutation checks (the record, by round)

Each mutation was applied to one source file, one at a time, with a targeted test run, then restored from a backup and checked with cmp. Restore result per round is in the table headers.

Round 1 (fdb2098), restored byte-equal:

| # | Mutation | Test that failed |
|---|---|---|
| M1 | PR_SET_PDEATHSIG off (aoide/mod.rs pre_exec) | the_child_dies_with_the_perch_that_started_it |
| M2 | receive task folder check off (recv.rs tick_once) | a_linked_mail_folder_is_read_by_nothing |
| M3 | a failed plain read does not re-arm the reread (recv.rs) | a_plain_read_too_big_re_arms_the_reread_and_the_reread_keeps_the_letters |
| M4 | notice cap raised from 300 to 100 000 (recv.rs) | a_notice_is_one_printable_line_of_at_most_300_characters |
| M5 | HTTP_PROXY added to ENV_KEEP (aoide/mod.rs) | the_proxy_and_the_provider_key_of_the_perch_do_not_reach_aoide |
| M6 | a send ended by a signal made a refusal (send.rs deliver) | a_send_ended_by_a_signal_after_aoide_started_is_an_unknown_row |

Round 2 (bd1f05a), restored byte-equal:

| # | Mutation | Test that failed |
|---|---|---|
| A | file-level dedupe removed (recv.rs) | mail::recv::tests::a_letter_on_file_is_not_written_again_when_the_cards_are_gone |
| B1 | PLAIN_CAP 4 MiB to 1 MiB | a_plain_read_of_two_mebibytes_is_kept_whole |
| B2 | REREAD_CAP 16 MiB to 4 MiB | a_reread_of_four_and_a_half_mebibytes_is_kept_whole |
| B3 | too-big reread re-armed | a_reread_too_big_to_print_is_a_loss_said_on_the_page_and_never_armed_again |
| C | SEND_TIMEOUT 45 s to 10 s (aoide/mod.rs) | a_send_that_stalls_for_twelve_seconds_is_still_accepted |
| E | usable() holds the schema lock across the call | a_slow_schema_check_does_not_hold_up_the_page |
| F | outbox complete forced true (recv.rs) | mail::recv::tests::a_cut_outbox_cannot_say_that_a_row_left_it |
| G | 2001 cut restored (take 2000) | two_thousand_and_one_letters_are_all_kept_and_the_page_counts_the_rest |
| H | lock refusal not set (recv.rs) | a_second_perch_on_one_home_is_refused_mail_and_reads_none |
| H2 | `if !took.accepted` off (send.rs) | a_recipient_aoide_will_not_take_is_a_409_and_no_row |
| I | `if !took.complete` branch off (send.rs) | a_send_aoide_did_not_complete_is_kept_with_a_warning |
| J | `if took.msgid.is_none()` off (send.rs) | an_answer_with_no_msgid_is_an_unknown_row_and_no_resend |

Process finding (round 2): the restored copies kept an older mtime, so cargo did not rebuild and the binaries kept the last mutation. The first full run then failed two tests. Fix: touch each restored file, or `cargo clean -p perch`, before the suite. Any later mutation run must do the same. Rounds 3 and 4 touched each restored file.

Round 3 (fbff34b): the four mutations listed in the tests/mail.rs header at that commit (lines 68-74 then; the header now lists five pairs at lines 67-77, the fifth added in round 4), each run with its named test; the file was restored after each (cmp) and touched:

| Mutation | Test that fails |
|---|---|
| recv.rs:1196 `task.reread = true` removed after a failed plain read | a_plain_read_whose_store_write_fails_re_arms_the_reread_and_keeps_each_letter_once |
| recv.rs:1181 `task.reread_after = ...` removed after a failed reread | a_reread_that_fails_for_another_reason_is_tried_again_after_the_gap |
| recv.rs:798 `lost.or(notice)` in list() (the one-sentence form that round 3 replaced) | a_full_store_is_shown_beside_the_loss_of_a_reread_too_big_to_print |
| aoide/mod.rs drive(): the wait error back to Spawn | aoide::tests::a_child_that_started_and_cannot_be_followed_is_started_and_not_spawn |

Round 3 has five saved backups, not four: A, B, C and E (recv.rs) and D (one mod.rs) in WSL ~/.cache/M2-fix3-mut (the Windows-side home has none). The
tests/mail.rs header lists five pairs for it now: the four above plus the snapshot-error arm in list() (round 4's M-item3, below). The table above lists
four of the five. The backups' names do not say which of A-E is which pair, so this file does not assign them.

Round 4 (74ae824): each mutation applied with a python anchor (refused unless it occurs exactly once), the named test run, the file restored byte-equal (cmp) and touched. The final state check compared both sources with their pre-mutation copies: equal.

| # | Mutation | Test that failed |
|---|---|---|
| M-e | send.rs:425 at 74ae824 (441 at e71bcc6): `Error::Started(_)` moved from the true arm of unknown_row_after_spawn to the false arm (the no-row side) | mail::send::tests::a_run_that_may_have_spooled_is_an_unknown_row_and_one_that_never_started_is_not (assert at send.rs:724 at 74ae824, :740 at e71bcc6) |
| M-item3 | recv.rs:799: `sentences(&[lost, notice, Some(why)])` back to `Some(why)` | a_loss_is_still_shown_when_the_store_cannot_be_read (the until helper panics at tests/mail.rs:419 after 10 s) |
| M-r1196 | recv.rs:1196 `task.reread = true;` replaced by a comment | a_plain_read_whose_store_write_fails_re_arms_the_reread_and_keeps_each_letter_once |
| M-r1181 | recv.rs:1181 `task.reread_after = Some(...)` replaced by a comment | a_reread_that_fails_for_another_reason_is_tried_again_after_the_gap (tests/mail.rs:1561) |
| M-S4 | recv.rs:252 SentRow's subject_chars field replaced by the subject itself | mail::recv::tests::a_letter_row_prints_its_sizes_and_never_its_words (recv.rs:1376) |

Note on M-e: it is the predicate that is mutated. At 74ae824 a mutation that only dropped the row write inside the guard arm (send.rs:398) was not
caught. Round 5 closed that (M6, below).

## Round 5 changes (e71bcc6: the verifier's gaps on 74ae824)

| # | Gap | Change | Where (e71bcc6) | Test |
|---|---|---|---|---|
| 1 | Started's arm in the post-run mapping had no test | The match after the run moved into settle(mail_state, dir, letter, sha, address, run). The row writer takes the shared mail State (Arc), so a test can build one (Ctx is not built in tests). deliver() is the checks and the run | send.rs:356-362 deliver, :368-422 settle, :414 the guard arm's row write, :439-444 unknown_row_after_spawn (its Started case at :441) | unit: a_started_run_that_could_not_be_followed_writes_one_row_and_a_spawn_failure_writes_none (src/mail/send.rs:755): Started gives one unknown row (msgid null, to and sha written) and a 502 that says "may or may not have been sent"; Spawn gives a 502 and no row. M8 fails it |
| 2 | Item 6: risk 7 and the round 4 deviation said the guard arm's row write is reached by no test | Rewritten. M6 shows the write is reached by the unit test and by tests/mail.rs:968 and :1636 | risk 7; round 4 deviations; the M-e note | M6 |
| 3 | Item 6: R11 cites :78 | :79, the field line (`lock: OnceLock<std::fs::File>`); :78 is its doc comment | mail/mod.rs:79 | n/a |
| 4 | Item 6 and the report: the round-3 report named fbff34b as HEAD, omitted 74ae824, and had the wrong counts | The commits, the counts (655, lib 296, mail 52) and the five round-3 backups are recorded at e71bcc6 (Identity, Verification, the round 3 backup note) | this file; M2.json notes | n/a |
| 5 | Item 5 (minor): the ~1 KiB per entry was not tied to escaping and the 2x copies | mail/README.md:137-138: the ~1 KiB per entry is the envelope aoide prints around each of the two escaped copies; it is estimated, not measured. The 12x paragraph is unchanged | mail/README.md:136-139 | n/a (docs) |
| 6 | Spec drift to ratify: S5 read literally (a send-path unknown row is never settled, and never carries a msgid) | Not a code change. The code's rule: a 502 row carries no msgid, so no receipt or outbox answer settles it; a 202 whose recipient gave neither a delivery.status nor filed, held or spooled is `unknown` and keeps its msgid, so the outbox can settle it. Ratified in M2.json | mail/README.md:55-65 (the 502 paragraph); send.rs:482-488 (status mapping) | n/a (ratification) |
| 7 | Grant note: Error::Started's variant and its Display arm are outside the literal "drive() only" grant | Not a code change. They are part of item 1 of M2-fix3, which asks for "a new Error variant". Ratified in M2.json | aoide/mod.rs:614 (variant), :628 (Display) | n/a |

Mutations (round 5, at e71bcc6). Each was one edit to perch/src/mail/send.rs, the named test run, and the file restored byte-equal (cmp) and touched; the committed send.rs is the restored one:

| # | Mutation | Test that failed |
|---|---|---|
| M8 | send.rs:413, the guard becomes `Err(e) if unknown_row_after_spawn(&e) && !matches!(e, Error::Started(_))`, so a started run answers 502 with no row | a_started_run_that_could_not_be_followed_writes_one_row_and_a_spawn_failure_writes_none (src/mail/send.rs:765, the "may or may not have been sent" assert). 7 other mail::send tests pass |
| M6 | send.rs:414, the guard arm's row write removed | tests/mail.rs:978 (an_answer_that_never_came_may_have_gone_and_leaves_a_row_that_says_so); tests/mail.rs:1643 (a_send_that_times_out_after_aoide_started_is_an_unknown_row_and_no_resend); and the unit test a_started_run_... (the sent.jsonl read, send.rs:766 at e71bcc6, reported at :765 in the mutated copy, which is one line shorter) |

## Open items found in earlier rounds (recorded, not fixed)

- receive-card-window-hides: GET /hub/mail shows the cards of the last 2 MiB of received.jsonl (RECEIVED_WINDOW, recv.rs:77, read at :755). Letters beyond the window are kept on disk but not shown. Round 2: a 6 MiB reread (32 letters) showed 31 of 32; the test was resized to 24 letters.
- receive-base-size-unbounded, receive-save-not-idempotent, receive-body-copy-before-clip: noted by the review, not changed.
- reread-failure attempt count is not in the notice.

## Merge note for R2c (not applied here)

R2c makes the runner pass AOIDE_AUDIT_LOG when the perch was given --aoide-audit-log. env_for already sets AOIDE_AUDIT_LOG from Aoide.audit_log (aoide/mod.rs:275, inside env_for at :271). R2c feeds that same field; the change belongs in env_for, not in ENV_KEEP. ENV_KEEP (aoide/mod.rs:68) stays the single passthrough list: PATH, HOME, LANG, TZ, plus every LC_* by prefix (kept_env, :305). Do not add AOIDE_* names to ENV_KEEP. Expect a textual conflict in env_for and in the kept_env doc comment. Rerun the full suite after the merge, and touch restored files after any mutation run.

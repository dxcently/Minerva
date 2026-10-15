# R2c security inventory: the mesh tab's two aoide event streams (round 2 close)

Claims for a reviewer. Each claim names where it is enforced. Branch: worktree-wf_0543ff15-bf5-1. Base: d8c5353. Final HEAD: 857bffb.

Commits since the base, oldest first:
- 88b4220 mesh: the events of both aoide streams, the audit log and the daemon feed
- eeb69ca mesh: aoided's start record in the audit allowlist, relative feed paths refused at the start
- 7447c61 mesh: the R2c fix round: mail letters out, feed fields by name, one bad stream answers alone
- 25b7113 mesh: R2c review fixes: mail case, gate url credentials, relative XDG, refusal test
- db564e8 mesh: R2c fix round 2: start records keep a message, links followed, url credentials fail closed
- db6cd45 mesh: R2c fix round 2 close: the audit path's order cited, code lines at 150 columns
- 857bffb mesh: R2c fix round 2 close, verifier gaps: relative links tested at the start, secrets audit cite corrected

Cites: every line cite below was re-read at 857bffb (the blobs are LF; the CRLF working copy has the same line numbers). Aoide doc cites are read from `/c/Users/dxcen/Projects/Aoide/` (the Aoide checkout the earlier pass verified; the lines were re-checked for this pass). The copy under `Minerva/Aoide/` is another revision with other line numbers, and is not cited.

Changes in this pass (857bffb, on top of db6cd45; no logic change):
- Section 3: the relative-link arm (mesh/mod.rs 296) is now tested. A relative dangling link beside the feed, a `../` link from a sub-folder, and a chain of two relative links (unit 1451-1465; integration tests/mesh.rs 922-928). Mutation M4 fails both (section 8).
- Section 3: the unit test for the runtime dir is in `src/aoide/mod.rs` 796, not in mesh/mod.rs as the old text said.
- Section 4: the secrets broker cite is Secrets-Broker.md lines 46-48 and 122 (the old text said 46-47). The `audit_keep` doc comment (mesh/mod.rs 930-933) no longer claims resolve, put or admin lines.
- Section 6: the allowlist integration test is renamed `the_audit_allowlist_drops_unpinnable_fields_and_forwards_only_start_messages` (tests/mesh.rs 646). The old name said no text is forwarded, which is no longer true.
- Section 9: the live check is the round 3 run at 857bffb (answers in scratchpad review-R2c/out/).

Corrections carried from round 2 (the old text is gone, not just softened):
- Section 3: the start refusal follows links. A chain of more than 40 links, or a loop, refuses the start with a sentence (`resolved`). A dangling link is followed to the path it names.
- Section 4: the audit `message` is forwarded only for a start record: command `daemon` or `adapter.melete` (any letter case) with status `started`. Every other record forwards `ts`, `class`, `door`, `command` and `status` only. The mail rule is gone from the perch: it was a special case of this rule.
- Section 5: a gate `url` with an `@` left after the cut is dropped (absent), not returned as it is.
- Section 6: the list of never-exposed values names the round-2 sentinels.
- Section 7: R11 and R16 are closed, each with named tests. R10 is narrowed and stays open. R17 and R18 are new.
- db6cd45: the audit path's precedence is named in the code (the doc above `audit_path`, 210) and in the README. CONTRACTS.md gives no precedence for that path (its line 985 is a migration note), so the order is the wiki's.

## 1. Files that carry R2c

| file | role |
|---|---|
| perch/src/mesh/mod.rs | `GET /hub/mesh/events`: the path resolvers (`audit_path` 210, `audit_file` 215, `daemon_path` 226, `daemon_file` 231, `daemon_default` 241); the start check (`feeds_apart` 256, `same_file` 270, `LINK_HOPS` 281, `resolved` 286); the audit allowlist (`audit_keep` 934, `start_record` 963, `command_ok` 969); the daemon filter (`FEED_TOP` 882, `GATE_PAYLOAD` 883, `AUDIT_PAYLOAD` 884, `feed_keep` 888, `scrubbed` 914, `without_userinfo` 429); the registry's url (`project_registry` 479, url 493, `via` 497); the bounded tail (`EVENTS_WINDOW` 85, `tail` 979, its `open_regular` call 980); `settle` 1033; the merge in `answer` 1051; the state's `feeds` map 131; the route (`route` 140, `cursors_asked` 806, `events` 1071). |
| perch/src/aoide/mod.rs | `audit_flag` 104 (the one reader of `--aoide-audit-log`); `runtime_dir` 111 and `absolute_dir` 116 (the one reader of `XDG_RUNTIME_DIR`); `from_options` 127; `launch` 188 removes every `AOIDE_*` variable (198-202) and the perch's own `XDG_RUNTIME_DIR` (205) from each child. |
| perch/src/main.rs | `--aoide-audit-log` and `--aoide-daemon-events` are absolute only (`absolute_file` 211, called at 136-137); both at one file refuses the start (`feeds_apart`, called at 139). |
| perch/src/rawfile.rs | `open_regular` (78): `O_NOFOLLOW \| O_NONBLOCK` (80), regular files only. Called by the tail at mesh/mod.rs 980. Not in the R2c range. |
| webui/term/mesh/view.js | Page side: `str` 29, `PAYLOAD_KEYS` 254, `isMail` 258 (a second guard), `payloadText` 261 (at most four scalar fields, each cut, 264-270), `eventLine` 277 (message drawn at 282), `emptyFeed` 290, `feedView` 329. No DOM, no markup. Not changed in this pass. |
| webui/term/tests/mesh.test.mjs | Page tests: 189 (the events fixture; its start record keeps its message, asserted at 199), 332 (a mail command's message is never drawn, 332-340; a start record's is drawn, 341-342). |
| perch/tests/mesh.rs, perch/tests/common/mod.rs | Integration tests (section 8). `refused_start` at common/mod.rs 128, its 10 s bounded poll at 145-155. `refusal_of` at tests/mesh.rs 504-516 uses the same bound. |
| perch/src/mesh/README.md, perch/tests/README.md | The contract and the test index. mesh/README.md: message rule 82-90, sources 118-125, start refusal 144-147, tests 207-222 (the relative `XDG_RUNTIME_DIR` note at 220). tests/README.md line 60 is the mesh row (400 and 502 cases). |
| webui/term/tests/README.md | The mesh row (line 25) names no mail or message wording, so it is accurate as it stands. |

Outside mesh/, changed in the R2c range by commits 88b4220 to db564e8 (not by db6cd45 or 857bffb): perch/src/aoide/mod.rs, perch/src/main.rs, perch/src/lib.rs, perch/src/serve.rs, perch/tests/aoide.rs. The old text said these were not changed in round 2; that was wrong.

## 2. The route: GET /hub/mesh/events[?audit=N][&daemon=M]

- Behind the perch's token like every `/hub` route. Only GET has an answer; any other method is the empty 404 (`route`, 140).
- Query keys: `audit` and `daemon`, each at most once, each digits only, at most 20 digits, in either order. No query is the first poll (`cursors_asked`, 806).
- 400 for: `after=` or any other key, a repeated key, an empty value or pair, a non-digit value (including `-1`, `1.5`, `%31`, `+1`), more than 20 digits, `audit=1=2`. Checked live (section 9): `?after=0`, `?audit=x`, `?audit=1&audit=2`, `?foo=1`, `?daemon=-1`, `?audit=1&after=2`, each answered with the route's 400 sentence, 82 bytes.
- 200 body: `{events: [{stream: "audit"|"daemon", event: {...}}], next: {audit, daemon}, reset: {audit, daemon}, error: {audit, daemon}, files: {audit, daemon}}`. `event` is the record as the stream keeps it (section 4 for audit, section 5 for daemon).
- `error.<stream>` is null, or the perch's own sentence for that stream alone ("<what> could not be read as a plain file", `settle` 1039), when the path is a link, a FIFO or a directory (`open_regular`, read at mesh/mod.rs 980), or cannot be read as a plain file. The sentence names the stream and nothing from the file. That stream's events are empty and its cursor is the one it was asked from. The other stream answers as usual. Status is 200; 502 is only for a failed blocking join.
- A stream whose file is missing is empty, with cursor 0 and no error. A cursor past the end of the file, or a file that is another file (a new inode or device), sets that stream's `reset` true and reads it from 0. The other stream is untouched.
- Reset memory is per path, `{dev, ino, len}` (`Seen`, 830), kept in the state's `feeds` map (131; read at 1077, written at 1097-1100). `reset` is true when the inode or device changed since the last poll that saw the path, or the length is shorter than the last seen length, or the asked cursor is past the end. The memory is lost on a restart (R13).
- At most 64 KiB read per stream per call (`EVENTS_WINDOW`, 85; the read is `take(EVENTS_WINDOW)` at 1002). The first poll reads the newest window from its first whole line. Only whole lines are used: a last line without its newline waits, and a window with no newline at all is passed over.
- Merge, in `answer` (1051): by `ts` (`sort_by_key`, 1058), then audit before daemon at a tie, then file order (a stable sort, noted at 1050).
- `files`: each path as a `~` path (`tilde`, 315), or null when the stream has no usable path.

## 3. Defaults, and the start refusal

| path | precedence (first that applies wins) | source |
|---|---|---|
| aoide home | `--aoide-home` (absolute) > `$HOME/.aoide` (`home_of`, 171) | the docs' runtime root `$AOIDE_ROOT`, default `~/.aoide`: `Projects/Aoide/docs/Aoide-Wiki/concepts/cli/Doors-and-Nodes.md` line 29 |
| audit log | `--aoide-audit-log` (absolute) > `<aoide-home>/log`; with `HOME` empty and no `--aoide-home`, no file (`audit_file`, 215) | aoide's own rule: `Doors-and-Nodes.md` lines 37-39 (`default_audit_log`: `$AOIDE_AUDIT_LOG`, else `$AOIDE_ROOT/log` when absolute, else `<home>/.aoide/log`). The perch reads no environment variable for this path. |
| daemon feed | `--aoide-daemon-events` (absolute) > `$AOIDE_DAEMON_EVENTS` (the perch's env, absolute) > `$XDG_RUNTIME_DIR/aoide/events.jsonl` (`XDG_RUNTIME_DIR` absolute); otherwise no file (`daemon_default`, 241) | aoide's own rule for the same file: `Doors-and-Nodes.md` "aoide events tail" (heading line 238; lines 245-246), `docs/architecture/AOIDED.md` lines 88-89, and `CONTRACTS.md` lines 6777-6783 (the file is named at 6783). The operator decision in R2c.json (2026-10-07) fixes the perch's order. |

- One resolver for `XDG_RUNTIME_DIR`: `aoide::runtime_dir()` (aoide/mod.rs 111, through `absolute_dir` 116), read by `Aoide::from_options` (127) for every call and by `mesh::daemon_default` (241). A relative or empty value is none for both. The runner removes the perch's own `XDG_RUNTIME_DIR` from each child's environment (aoide/mod.rs 205), so a relative one reaches no aoide call. The view names no default path from it.
- Tests for that: unit `a_relative_or_empty_runtime_dir_is_none_as_an_unset_one_is` (src/aoide/mod.rs 796); unit `the_default_feed_is_never_a_relative_path` (mesh/mod.rs 1490); tests/aoide.rs `no_path_lookup_and_no_ambient_identity` (291), where the test process's environment has `XDG_RUNTIME_DIR=run/user` (313) and every child reports none (comment at 334).
- Accepted without a live test (item 7): a relative `XDG_RUNTIME_DIR` cannot be set on a running perch in a test, because tests/common pins `XDG_RUNTIME_DIR` to a temp dir. The view's rule is a pure function test, and the runner's is tested in the child's environment, not on a live perch. Stated in `perch/src/mesh/README.md` line 220.

Where aoide's rule differs from the perch's:
- Audit: the one difference is `$AOIDE_AUDIT_LOG` from the environment. The runner removes every ambient `AOIDE_*` and passes only the flag, so under the perch the step is the flag alone.
- Daemon: aoide's fallback is socket-relative, the perch's is XDG. The two are the same file while the daemon socket is the default one. A `--aoide-daemon-socket` elsewhere moves aoide's path and not the perch's (R5).

Start refusal (main.rs 136-139; mesh/mod.rs 256-307):
- A relative `--aoide-audit-log` or `--aoide-daemon-events` refuses the start (`absolute_file`, main.rs 211; its unit at main.rs 352-364).
- Both feeds at one file refuse the start (`feeds_apart`, 256-266: the sentence at 259-263, the `same_file` call at 258). "The same file" is equal resolved paths (`resolved`, called from `same_file` 270-279) or equal device and inode when both exist (hard links).
- `resolved` (286-307) follows links one at a time. The hop loop is 289-299: while the path is a link (289), at `hops == LINK_HOPS` (290) it refuses with "is a chain of more than 40 links, or a loop: the perch cannot tell which file it names" (291). `LINK_HOPS` is 40 (281), so 40 links are followed and the 41st is refused. A link that cannot be read is refused (294). A relative target is joined to the link's own folder (296). A dangling link is followed to the path it names, so a dangling link at the audit path names the daemon feed when both name one path.
- Tests:
  - Unit `a_start_with_the_two_feeds_as_one_file_is_refused_and_two_files_are_not` (mesh/mod.rs 1420): two files are fine (1425); the same path is refused (1426); a dangling link to the feed (1441-1444); a chain of two links (1445-1450); relative links, a dangling one beside the feed, a `../` link from a sub-folder, and a chain of two relative links (1451-1465; the mutation note is at 1452-1453); `LINK_HOPS` links followed (1475-1476); `LINK_HOPS + 1` refused with the sentence (1477-1479); a loop refused, with nothing written (1480-1484).
  - Integration `a_start_with_both_feeds_at_one_file_is_refused_with_a_sentence` (tests/mesh.rs 888): the env case (918-919) and a relative dangling link at the audit flag with the daemon flag at the later path (922-928). It runs the perch through `refusal_of` (504-516: a 10 s deadline that kills and fails).

## 4. The audit allowlist (fails closed)

Kept, in this order: `ts` (non-negative integer, checked at 936, kept at 939), `class` (937, kept at 940), then only if present and passing their checks: `command` (941-943, shape check `command_ok` 969-973), `door` (944-946, `AUDIT_DOORS` 92), `status` (947-949, `AUDIT_STATUS` 93), and `message` under the rule below (950-957). Everything else is dropped with the record.

Rules a record must pass (any failure drops the whole record):
- It is a JSON object; `ts` is a non-negative integer; `class` is `audit` or `gate`.
- `door`, if present: one of `AUDIT_DOORS`.
- `command`, if present: first character an ASCII letter; then ASCII letters, digits, `.`, `/`, `-`; at most 64 bytes (`COMMAND_MAX`, 94). A space or any other character fails (`node add osaka`).
- `status`, if present: one of `AUDIT_STATUS`. Case is exact. `failed` is not on the list, so a daemon line with status `failed` is dropped whole at this check.

The `message` rule (round 2), `audit_keep` 950-957:
- `started` is the (command, status) pair (950). The message is kept only when `start_record(command, status)` holds (the filter is at 951-954): status is `started` and command is `daemon` or `adapter.melete`, compared with `eq_ignore_ascii_case` (963-965). Otherwise the record has no `message` key (956 is the only insert).
- Basis: `Doors-and-Nodes.md` line 91 (the `daemon started` record, door `daemon`, class `audit`) and line 213 (the `adapter.melete started` record). `Secrets-Broker.md` lines 46-48 say the broker writes its audit lines broker-side, before the value is released, and line 122 says audits name only. So a `secrets.resolve` line's message can name a secret, and no message is forwarded for it.
- The failure this closes: the round-1 rule forwarded the message of every non-mail command, so a secrets line's message reached an answer.
- The `error` status has no message either (the rule is by command and status, not by status alone). A `failed` daemon line is dropped whole at the status check, before the message rule runs; the unit case at 1655 asserts that its message does not reach an answer.

What the allowlist drops, by name in the tests:
- `untrusted_data` (another's text), `admitted`, `argv`, and any field a later aoide adds (unit 1618-1620; tests/mesh.rs 650-651 and 660).
- A `message` on any record that is not a start record: a mail letter, a `secrets.resolve` line, a `node.add` line, an `error` daemon line (unit 1675-1696; tests/mesh.rs 810-835 and 844-877).
- A record with no `ts`, or a `ts` that is not a non-negative integer.
- A `class` of `secret`, `notification`, `rice`, `content`, or none (tests/mesh.rs 673 lists `secret-class`).
- A `door` or `status` the list does not name. The docs also use other audit labels (`unmailed`, `attribution-drift`, `auto-deny-on-code-mismatch`); none is on the list, so such a record is hidden until a reviewer adds it (R3).
- A `command` with a space, or any character outside the shape above (`node add`, tests/mesh.rs 673).
- A line that is not a JSON object, and a line that does not parse.

## 5. The daemon feed filter

- Kept: a record whose `class` is `audit` or `gate`, with an integer `ts` (`feed_keep` 888-909). Dropped: other classes, a missing or non-integer `ts`, non-object, and unparsable lines.
- Top level, kept: `ts`, `class`, and of `v`, `kind`, `source` only the ones present (`FEED_TOP`, 882; the loop at 895-899). Any other top-level field (for example a free-text `body` or `note`) is dropped.
- `payload`, kept by name only: `GATE_PAYLOAD` (883) for `gate`: `id`, `name`, `originAddr`, `url`, `direction`; `AUDIT_PAYLOAD` (884) for `audit`: `file`. The payload is built at 900-906. A `note`, `node`, or any other name is dropped, and so is the whole payload when no named field is in it.
- A gate's `url` (`scrubbed`, 914-919; comment 911-913): a string with a `://` scheme is cut to its host and path (`without_userinfo`, 429-442: the authority's last `@` and what is before it are removed). A url with an `@` left after the cut is absent (441, `(!cut.contains('@')).then_some(cut)`). That covers a scheme-less `user:pass@box-a:8710/`, a `/` in the password (`http://user:pa/ss@host/`), and a query `@`. A non-string url is absent.
- Unit cases: `a_url_loses_what_comes_before_the_at_sign_in_its_authority_and_an_at_left_over_drops_it` (1206-1219: `http://u:p@host/x` gives `http://host/x`; `http://host:8710/` kept; the absent forms as `None`, at 1214 and 1217); the feed's url table in `the_feed_keeps_the_fields_the_docs_name_and_no_other` (1347-1362: the four named forms, a non-string url, and `name` kept beside it).
- The registry's url is cut the same way (`project_registry`, 493, `and_then(without_userinfo)`), so the snapshot and the feed agree (registry unit 1225-1231).
- The page draws the same names (`PAYLOAD_KEYS`, view.js 254). `payloadText` (261) draws at most the first four of them whose value is text, a number or a boolean, each cut to 40 characters (264-270). An object value is not drawn.
- Basis: `CONTRACTS.md` lines 6783 and 6798 (the P-P5 pairing events feed: the path, and `payload` carries fields BY NAME ONLY); the audit's `file` is AOIDED.md's hand-edit watcher.
- Tested: unit `the_feed_keeps_the_fields_the_docs_name_and_no_other` (1330); tests/mesh.rs `the_feed_keeps_two_classes_and_survives_a_truncate_in_place` (590), where `NOTE-SENTINEL` and the `node` field are absent and top-level `source` is kept; page tests at mesh.test.mjs 189 and 332.

## 6. What is never exposed, and what is written

Never exposed (each with the test that holds it):
- The audit log's `untrusted_data`, `admitted`, `argv`, and any field outside the allowlist: `FORWARDED-SENTINEL` and `--token=x` (unit `the_audit_log_forwards_its_allowlist_and_drops_what_it_cannot_pin`, 1618-1620); `FORWARD-SENTINEL` and `ARG-SENTINEL` (tests/mesh.rs `the_audit_allowlist_drops_unpinnable_fields_and_forwards_only_start_messages`, 646; the assertion loop at 673).
- A message on any record that is not a start record: `LETTER-BODY-SENTINEL` (a mail letter, tests/mesh.rs `a_mail_letter_is_in_no_answer_byte`, 810, asserted at 835); `SECRET-NAME-MESSAGE` (a `secrets.resolve` line, with `api-key`, 850), `FAILED-MESSAGE` (a `failed` daemon line, 853) and `ERROR-MESSAGE` (an `error` daemon line, 854), all asserted absent in tests/mesh.rs `a_message_is_forwarded_for_a_start_record_and_for_no_other_record` (844; the assertion loop at 876-877). Unit: `FAILED-TEXT` (1655) and `name=api-key` (1664). Live (section 9): `LIVE-AUDIT-SENTINEL` (a `mail.send` message), `LETTER-SENTINEL` (an upper-case `MAIL.read`), `SECRET-NAME-SENTINEL`, `FAILED-MESSAGE-SENTINEL`: 0 occurrences in every answer.
- A start record's message is kept, and the test asserts it (`STARTED-MESSAGE` at 859 and 881; `MELETE-MESSAGE` at 866 and 882). The page test at mesh.test.mjs 189 (assertion at 199) and 341-342 keeps it too.
- A gate `url`'s `user:pass@`: the unit cases at 1347-1362 (the password `pass`, `pa/ss` and `u:p`) and 1214-1217; the registry's `hunter2` (tests/mesh.rs SENTINELS, 45). Live: `CRED-SENTINEL` (scheme url) and `PASS-SENTINEL` (scheme-less url): 0 occurrences in every answer.
- Any daemon field not named in section 5: `NOTE-SENTINEL` (tests/mesh.rs 596-597 and 615), `NOTIFICATION-SENTINEL` (528, 536, 599), `CONSUMER-SENTINEL` (594, 615). Live only: `PAYLOAD-EXTRA-SENTINEL`, `EXTRA-FREE-TEXT-SENTINEL`.
- A `secret` or `notification` record of the daemon feed, and a record with no `ts`: `SECRET-NAME-SENTINEL` (526, 594), `NOTIFICATION-SENTINEL`, and the record with no `ts` in tests/mesh.rs 673 (`no-ts-here`); `feed_keep` 888-891 drops all three.
- The feed's path to aoide: `AOIDE_DAEMON_EVENTS` is in no aoide call's environment (tests/mesh.rs `the_runner_gives_aoide_the_audit_flag_only_and_never_the_feed_path`, 725, asserted at 733). The live script prints whether it is set on each fake call (section 9 notes where that output went).
- The contents of a link or FIFO planted at either path: `PLANTED-SENTINEL` (772, 780) and `PLANTED-FEED-SENTINEL` (800, 804) in tests/mesh.rs `a_link_or_a_fifo_at_either_path_is_not_read_and_the_other_stream_still_answers` (768).
- Errors name a stream and a reason, never the file's content (`settle` 1039).
- The page draws every string as a text node: `str` (view.js 29) runs `clean` and a cut; the label `audit` or `daemon` comes from the stream key, never from record text (`eventLine`, 277).

Writes:
- The perch never opens either event file for writing. `open_regular` is `O_NOFOLLOW | O_NONBLOCK`, read-only, regular files only (rawfile.rs 78-80; the events tail reads at mesh/mod.rs 980).
- aoide appends its audit records for each audited call it runs. Calls the perch makes that are audited: `mail.read` and `mail.send` (observed records). `mail outbox` is expected from the docs and was not observed. A usage-error exit writes none. The daemon feed is written by aoided, never by the perch.
- How often, for the perch's calls: the snapshot's three calls (`--version`, `mesh --json`, `mail outbox --json`) are each cached for 60 s (`TTL`, 65), so any number of open tabs cost at most one run of each a minute; mail calls run once per request; the events poll runs no aoide call and reads the files at their cursors only.

## 7. Risks, for the reviewer

- R1 (resolved). The daemon feed had no field allowlist; section 5 adds one. The remaining trust is in the names: `name`, `url`, `originAddr` are the docs' fields and are text another node chose. They are drawn as text, cut to 40 characters, and are not interpreted.
- R2. The `command` check is a shape check, not a list of command names. A bare 64-character alphanumeric word passes. aoide writes command names from its dispatch table (the docs say so); this slice does not verify that in aoide's code, which it does not read.
- R3. A closed list drops silently. A new aoide `status` or `door` is hidden, and the page cannot tell a dropped record from no record. Fail closed, but it is invisible drift.
- R4. The `ts` unit is not documented for audit records (the real log's `ts` is an integer, observed). The merge orders the two streams by `ts`. If the feed's `ts` is in another unit, the merge order is wrong. No security effect.
- R5. The daemon default is XDG, not socket-relative. A perch with a non-default `--aoide-daemon-socket` and no `--aoide-daemon-events` shows the wrong file, or none. Not a leak.
- R6. A relative `--aoide-home` or `--aoide-daemon-socket` is not refused at start, unlike the two feed flags. The runner refuses it on each call, and the mesh names no path for it. Same class of silent failure, not a leak.
- R7. `files` shows absolute paths of the two feeds to the page, behind the token. The snapshot already shows `~` paths for the state folder.
- R8. A line longer than 64 KiB is passed over and not shown. Availability only.
- R9. The live checks and the tests never read the operator's `~/.aoide`, `~/Minerva`, `~/.config`, `$XDG_RUNTIME_DIR/aoide`, or the perch on 127.0.0.1:47820. Every run used a temp `HOME`, a temp `XDG_RUNTIME_DIR`, a temp `--aoide-home`, temp feed copies, and `fake_aoide` by explicit path. No real aoide or aoided ran.
- R10 (narrowed, open). A start record's `message` is kept and drawn, and nothing strips a `user:pass@` or other text from it. A length cap and a scrub were considered and not added: the start record is aoided's own listen text (the round-2 decision), and a cap or a scrub would change the start-record contract. The start record's listen URL is the only one in the fixtures. Other commands' messages are no longer forwarded at all (section 4), so the a2a door's messages are out of the answer. For the reviewer to decide.
- R11 (closed). A dangling link at the audit path, pointing at the daemon feed, was not caught at start. `resolved` now follows links to the file they name, so `feeds_apart` refuses it when both name one file. Tests: unit `a_start_with_the_two_feeds_as_one_file_is_refused_and_two_files_are_not` (1441-1450 dangling and two-link; 1451-1465 relative, from the link's own folder); integration `a_start_with_both_feeds_at_one_file_is_refused_with_a_sentence` (922-928, a relative dangling link). Live (section 9, the `linked` poll): a link at the audit path that points at the daemon feed gives `error.audit` ("could not be read as a plain file", since `open_regular` refuses a link), and the daemon stream still answers. The start-time refusal of a dangling link is covered by the tests only, not live.
- R12. The snapshot answer (`/hub/mesh`) carries node roster text, `cache[].text`, which is remote-authored and may hold markup (the fixture does). The page never draws that field (`nodeRows` uses name, age and the cut flag only). It is pre-existing (R2b), behind the token, and not part of the events.
- R13. The identity memory is in process only. After a restart, the first poll starts a stream from its newest window, and a replacement made before the restart is not seen as a reset. A same-inode regrowth between two polls is not seen either: size is only seen at a poll.
- R14. The page shows at most four payload fields per record (`payloadText`, view.js 264-270), so a pair-parked line draws `id`, `name`, `originAddr`, `url` and not `direction`. Not a security effect; a readability cost, left as is for the reviewer.
- R15. The `url` scrub removes `user:pass@` only. A credential in a query string (`?token=`) of a gate `url`, or in another gate field, is not removed. The docs give the gate `url` as an address, so this is not expected; a reviewer may ask for a query-string rule.
- R16 (closed). `without_userinfo` cut `user:pass@` only after a `://` scheme, so a scheme-less value and a `/` in a password passed through. Now a url with an `@` left after the cut is absent (`without_userinfo` 441). Tests: unit 1206-1219 (the named cases, `user:pass@box-a:8710/` and `http://user:pa/ss@host/` as `None`), 1225-1231 (the registry url absent, the rest of the node kept), and the feed table at 1347-1362. Live: section 9.
- R17 (new). The registry's `via` field is forwarded to the snapshot (`via: clip_str(entry.get("via"), 200)`, mesh/mod.rs 497). The fixture's value is `ssh://khoa@osaka`: a user name, not a password, and the field is the registry's, not the events'. Not changed here (out of scope for R2c); a reviewer may want the same cut as the url.
- R18 (new). `scrubbed` forwards every named payload field other than `url` as the record has it (917). An object-valued `id` or `name` would reach the answer with its nested text. The page does not draw an object value (`payloadText` skips non-scalars). The docs name these fields as strings, and no record of that shape was observed. Not changed; a reviewer's call.

## 8. Test coverage (what fails if a guard is reverted)

Rust (perch), the whole suite at 857bffb: `cargo test --features test-bins --offline --no-fail-fast`: 858 passed, 0 failed, 1 ignored (the aoide test that was already ignored). Round 1 was 856 passed. 857bffb adds no `#[test]` functions (it extends two existing tests and renames one), so the per-binary counts are the same as at db6cd45: lib 460; fake_eidolon 4; main 5; agent 32; allow 1; aoide 19 (1 ignored); deps_smoke 1; gpl_boundary 2; guard 31; http 132; install 31; mail 1; mail_agent 1; manual_policy 4; mesh 31; meta 18; notes 19; runs 38; runs_log 14; term 1; term_agent 1; term_stream 1; watch 11.

Clippy at 857bffb: `cargo clippy --offline --all-targets -- -D warnings` and `cargo clippy --features test-bins --offline --all-targets -- -D warnings`: both clean.

Long lines (the R2c range): no added line in a Rust file is over 150 columns (mesh/mod.rs, tests/mesh.rs, main.rs, aoide/mod.rs, lib.rs). The 16 added lines over 150 columns are all in non-Rust files and are not wrapped, because they are table rows, JSON or JSONL fixture data, or the generated `--help` text: perch/tests/README.md (1 table row), webui/term/tests/README.md (1 table row), webui/term/tests/fixtures/aoide/README.md (3 table rows), webui/term/tests/fixtures/aoide/audit.log (3 lines), daemon-events.jsonl (2), events.json (4), and installer/tests/fixtures/perch-help-new.txt (2, generated from `--help`).

Named tests this round (cites at 857bffb):
- Unit (mesh/mod.rs): `a_message_reaches_an_answer_only_for_a_start_record` (1675); `the_audit_log_forwards_its_allowlist_and_drops_what_it_cannot_pin` (1618); `a_url_loses_what_comes_before_the_at_sign_in_its_authority_and_an_at_left_over_drops_it` (1206); `a_registry_url_with_an_at_left_in_it_is_absent_and_the_rest_of_the_node_is_not` (1225); `the_feed_keeps_the_fields_the_docs_name_and_no_other` (1330); `a_start_with_the_two_feeds_as_one_file_is_refused_and_two_files_are_not` (1420).
- Integration (tests/mesh.rs): `a_message_is_forwarded_for_a_start_record_and_for_no_other_record` (844); `a_mail_letter_is_in_no_answer_byte` (810); `the_audit_allowlist_drops_unpinnable_fields_and_forwards_only_start_messages` (646); `the_feed_keeps_two_classes_and_survives_a_truncate_in_place` (590); `a_link_or_a_fifo_at_either_path_is_not_read_and_the_other_stream_still_answers` (768); `a_start_with_both_feeds_at_one_file_is_refused_with_a_sentence` (888); `a_relative_feed_path_is_refused_at_the_start_and_the_refusal_names_its_flag` (965); `a_feed_replaced_by_a_new_file_starts_again_though_the_cursor_is_inside_it` (934); `the_runner_gives_aoide_the_audit_flag_only_and_never_the_feed_path` (725); `the_defaults_are_aoides_own_log_and_the_feed_beside_the_socket_or_the_env_path` (688); `both_streams_merge_by_ts_and_each_record_says_which_stream_it_came_from` (521).
- Harness: `refused_start` (tests/common/mod.rs 128; the 10 s deadline at 145 kills a perch that starts and fails, as `refusal_of` does at tests/mesh.rs 504-516). Item 4 is this bound, and every caller passes unchanged.
- Other: tests/aoide.rs `no_path_lookup_and_no_ambient_identity` (291); main.rs `a_feed_file_the_perch_is_told_of_is_an_absolute_path_or_it_is_refused` (352); src/aoide/mod.rs `a_relative_or_empty_runtime_dir_is_none_as_an_unset_one_is` (796).

Hand reverts, round 2 (each a perl change to one file, the named tests run, the file restored from a saved copy and compared with `cmp`; the script is `scratchpad/review-R2c/mutate-r2.sh`). Run at db6cd45; the logic is unchanged at 857bffb (only comments, tests and README changed):
- M1, the message rule off (`if start_record(command, status)` becomes `if true || start_record(...)`): lib, `a_message_reaches_an_answer_only_for_a_start_record` and `the_audit_log_forwards_its_allowlist_and_drops_what_it_cannot_pin` fail (458 passed, 2 failed). tests/mesh.rs, `a_mail_letter_is_in_no_answer_byte`, `a_message_is_forwarded_for_a_start_record_and_for_no_other_record`, `both_streams_merge_by_ts_and_each_record_says_which_stream_it_came_from` and `the_audit_allowlist_drops_unpinnable_fields_and_forwards_only_start_messages` fail (27 passed, 4 failed).
- M2, the link bound at 1 (`LINK_HOPS` 40 becomes 1): lib, `a_start_with_the_two_feeds_as_one_file_is_refused_and_two_files_are_not` fails (459 passed, 1 failed).
- M3, the `@` drop off (`(!cut.contains('@')).then_some(cut)` becomes `Some(cut)`): lib, `a_url_loses_what_comes_before_the_at_sign_in_its_authority_and_an_at_left_over_drops_it`, `a_registry_url_with_an_at_left_in_it_is_absent_and_the_rest_of_the_node_is_not` and `the_feed_keeps_the_fields_the_docs_name_and_no_other` fail (457 passed, 3 failed).
- M4, the relative arm off, at 857bffb (line 296: `Some(dir) if target.is_relative() => dir.join(&target),` becomes `Some(dir) if false => dir.join(&target),`): lib, `a_start_with_the_two_feeds_as_one_file_is_refused_and_two_files_are_not` fails at mod.rs:1456 (the first relative assertion); tests/mesh.rs, `a_start_with_both_feeds_at_one_file_is_refused_with_a_sentence` fails at tests/mesh.rs:511 (the perch started and was killed after 10 s). Restored from a saved copy (`r2c-fix3/mod.rs.good`); `cmp` identical; the arm is at line 296 again.
- Restored: `mesh/mod.rs` identical to the saved copy after each run (cmp).
- Not rerun in round 2: the previous pass's reverts (the gate url scrub off, the runner's `env_remove("XDG_RUNTIME_DIR")` removed, `absolute_dir` without the absolute filter, `feeds_apart`'s same-file refusal off, the page's `i` flag off). Their targets still exist; their results are from the previous pass.

Page (`node --test "webui/term/tests/*.test.mjs"`, 283 tests, Windows node v26.7.0 from Git Bash): 283 pass, 0 fail at 857bffb. The round-1 failure, `no vendored file has a CR` (`webui/term/tests/vendor.test.mjs:62`), does not reproduce: `git status` is clean, and `git -C <worktree> checkout -- webui/term/vendor/hooks.js` changed nothing (git compares the checkout with autocrlf in mind). No vendored file was edited in this round.

Installer: `cargo test --offline --test perch_open` in installer/ with `MINERVA_ALLOW_PLACEHOLDER_KEY=1` and a private target dir: 19 passed, 0 failed. Port 48110 had no listener before the run.

Observed one-off flake, not fixed (outside this slice's grants): lib `watch::tests::a_sink_panic_is_logged_without_its_message` failed once under load (`eventually()` timed out; watch.rs is unchanged since the base). It passed 5 of 5 alone and in the next full suite run.

## 9. Live check (round 3 close; port 48131, at 857bffb)

Run at 857bffb with copies of this worktree's binaries in `~/.cache/R2c-bin` (cmp-identical to the test build of the final tree), `fake_aoide` by explicit path, temp feed copies, a temp aoide home, and the bearer in a 0600 header file read by `curl -H @file` (never argv, never printed); then `--stop`. Script: `scratchpad/r2c-fix3/live-check-fix3.sh`. Answers (bodies only): `scratchpad/review-R2c/out/`, 13 files. The script prints each status (`%{http_code}`, line 38) and the fake's environment checks (line 116) to its own stdout at run time; that run log was not saved, so the status codes are not in `out/`. The bad-query bodies are the route's 400 sentence. Port 48131 was closed at the probe after the run.

- First poll (`first.json`): 1422 bytes, the same as the db6cd45 run.
- Audit grows by four lines: `mail.send` with a message, an upper-case `MAIL.read` with a letter, a `secrets.resolve` with a name in its message, and a `daemon` line with status `failed` and a message (`second.json`: 616 bytes, the same as db6cd45). The `failed` line is absent entirely, and the `secrets.resolve` record has no `message`.
- Daemon grows by two gate lines, one with `http://user:CRED-SENTINEL@box-a:8710/` and one with `user:PASS-SENTINEL@box-a:8710/` (`third.json`: 611 bytes, the same as db6cd45). The first payload's url is `http://box-a:8710/`; the second payload has no url.
- Daemon cut in place and written shorter (`fourth.json`: 370 bytes): `reset.daemon` true, `reset.audit` false.
- The six malformed queries (`bad1.json` to `bad6.json`): each 82 bytes, the route's 400 sentence.
- The audit path replaced by a link to the feed file (`linked.json`: 325 bytes): 0 events; `error.audit` is "the audit log could not be read as a plain file"; `error.daemon` null; `next` audit 1364 (the cursor it was asked from), daemon 64.
- The daemon feed replaced by a new file, new inode and longer than its cursor (`replaced.json`: 482 bytes; the same as db6cd45): `reset.daemon` true, `reset.audit` false, `error` both null.
- Snapshot `GET /hub/mesh` (`snapshot.json`): 3031 bytes, the same as db6cd45; `available` true; `errors` empty.
- Sentinel scan over `out/*.json`, run for this report: 0 occurrences of each of `LIVE-AUDIT-SENTINEL`, `LETTER-SENTINEL`, `SECRET-NAME-SENTINEL`, `FAILED-MESSAGE-SENTINEL`, `CRED-SENTINEL`, `PASS-SENTINEL`, `hunter2`, `FORWARDED-SENTINEL`, `NOTIFICATION-SENTINEL`, `PAYLOAD-EXTRA-SENTINEL`, `EXTRA-FREE-TEXT-SENTINEL`, `NO-TS-SENTINEL`.
- Disk guard for this run: 183G free on C: (`df -h /mnt/c` in WSL, with `MSYS_NO_PATHCONV=1`), above the 6G stop line.

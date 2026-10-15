# T2 security inventory (checked against the code at eade8cb, the final HEAD: fix round 3)

Outcome: T2 is on worktree-wf_0cb668b4-3e3-1 as five commits on base 08c01fa: 97a0bcd (the routes), 5549f63 (round 1: one pin line in perch/tests/agent.rs), 9e2bb9b (round 2: tests and doc lines, no route change), df26ab2 (round 2 tail: the term README's note_input paragraph), and eade8cb (round 3, final: tests and a README section, no route, guard or gate code changed). The full perch suite with --features test-bins is green in its run at eade8cb: 920 passed, 0 failed, 1 ignored (tests/aoide.rs), across 26 test binaries; doc-tests run none. Two other full runs at this tree were not clean (see section 7). Clippy is clean both ways (cargo clippy --all-targets -- -D warnings, with and without --features test-bins). Nothing was pushed and master is untouched. No live check was run in round 3, since no route changed; the round-1 live check on 48115 was not re-run.

Corrections made in this round are marked [corrected] or [new].

## 0) Round 3 (eade8cb): what changed and how each new assertion was shown to fail

Changes, all in the four granted files:
1. perch/src/term/README.md: a section "What an agent's terminal tools expose", after "The hold", with three bullets: a read of any web terminal, the operator's included, sends its screen to the model provider, secrets included (ruling 2); that text is untrusted and nothing in the perch filters it; in yolo the door's policy answers yes to term_send and the perch never asks, and the text guard (D9) only judges one send's text. Checked against perch/src/mode.rs: yolo is "every question is a yes; a guard row is a refusal, not a question".
2. perch/tests/term_agent.rs, open_takes_the_chats_folder_and_refuses_a_fifth_terminal: one open with title w, cols 100, rows 30, then four plain opens (201, 201, 201, then 409 as the fifth overall). Pins: the operator's row reads cols 100 and rows 30; the agent's list row for it has title w; a plain open is 120 by 32.
3. perch/src/term/agent.rs, lines 471-508: an inline #[cfg(test)] module with every_named_key_presses_its_bytes_and_an_unknown_name_presses_none. It pins the bytes of all 14 names (enter 13, tab 9, esc 27, backspace 127, up ESC[A, down ESC[B, right ESC[C, left ESC[D, home ESC[H, end ESC[F, pgup ESC[5~, pgdn ESC[6~, ctrl-a 1, ctrl-z 26) and None for ctrl-A, ctrl-, ctrl-1 and nope.
4. perch/tests/term_agent.rs, open_then_send_then_read_for_the_text_answers_with_it: the read body has cursor (an object or null, the key present), alt_screen (a bool) and seq (a u64).
5. perch/tests/README.md: the term_agent row says what round 3 pins.

Each new assertion was shown to fail under a scratch break. Each break was applied to the worktree file, the test was run, and the file was restored from a saved copy and checked with cmp (equal each time). No break was committed. Logs are in scratchpad/T2-fix3-work (mut-*.log).

| assertion | scratch break | what went red |
|---|---|---|
| the 14 named keys' bytes | every named key's bytes shifted by one; ctrl-a and ctrl-z shifted by one | all 14 names, each with the bytes it pressed |
| the 4 non-key names | the fallback returns Some(empty) | ctrl-A, ctrl-, ctrl-1 and nope, all four |
| open's title | title: None | tests/term_agent.rs:327, left "" right "w" |
| open's cols | cols: None | tests/term_agent.rs:322, (120, 30) against (100, 30) |
| open's rows | rows: None | tests/term_agent.rs:322, (100, 32) against (100, 30) |
| a plain open is 120 by 32 | cols default moved to Some(80) | tests/term_agent.rs:324, (80, 32) against (120, 32) |
| the read's cursor | cursor key renamed to curs0r | tests/term_agent.rs:348 |
| the read's alt_screen | alt_screen key renamed to alt | tests/term_agent.rs:349 |
| the read's seq | seq made a String | tests/term_agent.rs:350 |

After the breaks, the full suite ran on the restored tree (= the commit). The README section was checked by reading the code it describes (mode.rs, guard rows, agent.rs); it is documentation, so it has no mutation check.

## 1) Files

- perch/src/term/agent.rs: the five agent routes (97a0bcd). 468 lines at 97a0bcd (git show 97a0bcd:perch/src/term/agent.rs | wc -l; the round-2 figure of 469 was wrong) [corrected]; 469 at df26ab2 (round 2 adds one module doc line at line 23); 508 at eade8cb (round 3 appends the test module at lines 471-508, after all route code, so no route line above 469 moved).
- perch/tests/term_agent.rs: 20 tests. Round 3 adds no test function; it adds assertions to two of them (see section 0).
- perch/tests/README.md: a 2-line row (97a0bcd), its term_agent row updated in rounds 2 and 3.
- perch/src/term/README.md: round 2 changed the note_input paragraph (df26ab2); round 3 adds the new section.
- perch/tests/agent.rs: one assertion line changed to term/list 200 [] (5549f63); its doc comment was rewritten in round 2 (section 8).
- Not touched by T2 in any round: serve.rs, perch/src/guard/mod.rs, perch/src/agent/token.rs, the terms core, stream.rs, mod.rs, screen.rs, tests/common/mod.rs, Cargo.*, lib.rs, main.rs.

## 2) Routes, bounds and refusals (perch/src/term/agent.rs)

Line numbers are as of 5549f63 (round 1). Fix round 2 (9e2bb9b) added one line at agent.rs:23, so from line 23 on, every number below is one higher at eade8cb than the one written. Round 3 adds no line above 470.

Dispatch:
- No Caller in the request's extensions: 403 (line 147). The chat comes only from the gate's Caller, never from a body, query or header.
- Non-POST: 405 with Allow: POST (150-152).
- Unknown verb: 404 (161).
- Body: one JSON object of named fields (deny_unknown_fields), at most 16 KiB (JSON_BODY, line 44). A declared Content-Length over the cap is a 413 before a byte is read (declared_over, 241-244). An empty body counts as {} (236). A named field that is not known is a 400 (237), so a cwd or chat in a body is refused, never honoured.

list (166-185):
- No fields. 200 with an array of {id, title, fg, by, chat, mine, alive, hold} (184). Any field: 400.
- Every web terminal, including the operator's and other chats' terminals (ruling 2). mine is true only for this chat's own agent terminals.

open (190-227):
- {title?, cols?, rows?}. 201 {id} (209). The title is cleaned by clean_title (screen.rs:378) and cut at MAX_TITLE, 80 characters (screen.rs:60). The size is clamped by clamp_size, 20-400 columns and 5-200 rows.
- 409 when the chat already has AGENT_CHAT_CAP = 4 open (216; constant at term/mod.rs:77; D5).
- 409 when the perch holds MAX_TERMS = 16 (217; term/mod.rs:59).
- 422 when the chat's folder is no longer a directory (218).
- 503 while the perch stops (219).
- 400 when the terminal names no chat (221; unreachable, per its own comment).
- 500 when the shell cannot start (224).
- The shell starts in the chat's known folder, or $HOME when none is known (195-201).
- A cwd or chat in the body: 400.

read (248-286; wait_of 303-316; settle 321-356; screen_of 359-364):
- {id, lines?, max_chars?, wait?}. lines 1..=400, default 100 (MAX_TEXT_LINES = 400, screen.rs:57; READ_LINES = 100). max_chars 1..=7000, default 7000 (MAX_TEXT_CHARS = 7000, screen.rs:58). Both out of range: 400.
- wait.for 1..=200 characters (304-305). wait.idle_ms 0..=60000, default 500 (308-309). wait.timeout_s 0..=60, default 30 (312-313). idle_ms 0 answers at once, text or not, so a for is waited for only above 0. Stated in the module doc at agent.rs:23 (round 2).
- 200 {text, cursor, seq, alive, exit?, alt_screen}. Round 3 pins the cursor, alt_screen and seq keys through the door (section 0).
- 404 for a bad or unknown id (254). 404 also when the terminal goes away between the check and the answer (273-276).
- 503 when the perch stops during the wait (271). Pinned by a_stop_during_a_long_wait_exits_within_three_seconds: the door logs the held read as 503 "the perch is shutting down".
- Works on any web terminal, held or not.
- An ended terminal answers from the screen it kept, with alive false and exit named. Pinned through the door in round 2: an_ended_terminal_keeps_its_screen_and_refuses_an_agents_input (the shell ended in this run) and an_ended_terminal_read_back_from_terms_json_answers_an_agent_from_its_saved_screen (after a restart, from terms.json). Both refuse a send with 409.
- The answer has no flag that says whether the wait matched, went quiet, timed out or ended (ReadAnswer, 132-141). [kept]

send (378-411; key_bytes 415-437; send_refused 439-447):
- {id, text?, keys?}. text at most 4 KiB, counted in bytes (391-392). keys at most 32 names (394-395). A key outside the named set is a 400 (401). No text and no keys: 400 (388-389).
- 200 {seq} (408).
- 404 for a bad or unknown id (384, 441).
- 403 from the guard, before any byte is typed, with the guard's sentence (404-405).
- 413 when the bytes typed (text plus keys, after key names are expanded) exceed MAX_INPUT (442; InputError::TooBig, term/mod.rs:694, 708).
- 409 when the shell has ended (443).
- 503 when the shell is not reading its input (444).
- 423 while the operator holds the terminal (445).
- Order: the guard runs before the hold check, so a guarded text sent to a held terminal answers 403, not 423. [new]
- Key names: the bytes are pinned by the inline module (round 3). Cursor keys are the normal-mode forms only.

close (451-468):
- {id}. 202 {id} only for a terminal this chat opened (465; is_agents_of, term/mod.rs:106).
- 403 for another chat's terminal or the operator's (461).
- 404 for an unknown id (457, 459, 466).

Gate (AG, unchanged; perch/src/agent/token.rs):
- Wrong or missing scoped token with the master bearer presented: 404 (261).
- Wrong or missing scoped token with no master bearer: 401 (261).
- Socket that no live door owns: 403 (267-270).
- The gate's own whoami answers the attribution and runs the Host, sec-fetch and method gates first (281-299).

## 3) What is exposed, and to whom

- Never exposed: the master token (404 on agent paths); the scoped term token in argv, environment, stdout, stderr or the door's log (asserted by the_term_token_is_in_no_argv_environment_stdout_stderr_or_the_doors_log); any chat id from a body, query or header (the Caller comes from the peer socket only).
- Exposed to every chat (ruling 2): list rows for every web terminal, with id, title, fg (the foreground program), by, the opener's chat id, mine, alive and hold. [corrected: the earlier inventory named only the chat id]
- Exposed to every chat (ruling 2): read answers the screen of any web terminal, including the operator's, up to 400 lines and 7000 characters, with cursor, seq, exit and alt_screen. Anything typed on that screen, secrets included, goes to the model provider. Stated for the reader in the term README section added in round 3.

## 4) Decisions applied

- D5: open and close are allowed; 4 open per chat; close only the chat's own terminals.
- D9: refuses_term_text (guard/mod.rs:680) refuses three things and names a sentence for each:
  - the aoide CLI (REASON_AOIDE), read as shell words, so echo aoide passes and bash -c "aoide pair" is refused;
  - a change to the update keys, such as hoot keys allow-web or the minerva keys add/remove forms (REASON_KEYS);
  - a word that names the update-keys directory (REASON_KEYFILE). [corrected: the earlier inventory named only the aoide sentence]
- D9, ssh: not applied to term text. REASON_SSH is for bash only. The guard's doc says ssh through a terminal is the intended path. [new]
- D6: the approval mode is eidolon's policy on the door's tool call: manual and auto ask, yolo answers yes. The perch route does not ask. [corrected: the earlier inventory said "send types without asking (yolo)" as if the perch decided]
- Ruling 2: list, read and send work on any web terminal. The operator's stop is the hold: it refuses send with 423 and leaves reads open.
- Round 3, from the orchestrator's decisions (T2-fix3.md):
  - idle_ms 0 with a for answers at once, with no text wait: accepted as documented at agent.rs:23 (no new 400).
  - The "(T2)" label in the note_input doc comment at perch/src/term/stream.rs:417 (the brief said 419) is outside round 3's grant. Not edited here. Follow-up for the orchestrator.
  - The runs.rs fixed-port flake (48140/48141) is R1a's, not T2's. It did not occur in the final run (runs.rs: 38 passed).
  - $HOME/.cache/T2-live was removed by the orchestrator. Round 3 ran no live check, so no header file was created.

## 5) Risks

1. read hands raw terminal output (command output, page text) to the model provider. Terminal text can carry prompt injection, and nothing filters it. The tool note warns about this; the route does not change it. Now also stated in perch/src/term/README.md (round 3).
2. Ruling 2: any agent can read any web terminal, including one where the operator typed a secret. That text goes to the provider.
3. The perch never asks before send. Under yolo the door's policy answers yes to every term_send (perch/src/mode.rs: "every question is a yes"). The perch's only stops on a send are the hold (423) and the text guard (403). [corrected]
4. The text guard is a hint, not a gate. It judges one send's text only. It misses a command split across sends (ao, then ide), DEL or tab completion, a variable set in one send and used in another, and a file the shell reads. [corrected: quoting is handled, and the 8 KiB guard limit never applies within one send, because send caps text at 4 KiB]
5. Keys bypass the text guard by design: ctrl-c and ctrl-d are allowed (ctrl-a to ctrl-z). An agent can interrupt the operator's running command, or end the operator's shell with ctrl-d at an empty prompt. [new]
6. ssh through a terminal is allowed. The bash ssh row does not cover term_send. This is intended, and it depends on the operator watching the terminals tab; under yolo nothing asks. [new]
7. Arrow keys send normal-mode sequences only. A full-screen program in application cursor mode may ignore them.
8. The wait is OR: text, quiet, timeout, or shell end. A silent long command returns early on quiet (default idle 500 ms), so an agent can read an empty screen as its result. A timed-out wait looks the same as a matched one, since the answer has no flag. With idle_ms 0 the quiet is met at once, so a for is not waited for (documented in the module doc). [new]
9. The wait has no per-chat cap on concurrent reads. Each call can hold a connection for up to 60 s. The bound is per call, not per chat. [new, minor]
10. The title an agent gives open is shown on the operator's list and stream. clean_title (screen.rs:378) removes control and invisible characters, and the title is cut at 80. Whether the page shows it as text and never as HTML is in the page code, which is outside T2's grant and was not checked here. The reviewer should check the page's title rendering. [new in round 3, not verified]

## 6) Branches and trade-offs

- OR wait over AND wait: OR matches the tool's model-facing promise. Loss: a silent command returns early.
- Hold blocks send only; reads stay open (ruling 2). Gain: the operator's stop works for input, and the screen stays readable for the agent. Loss: the agent can still read whatever the operator is looking at.
- Guard in the route, not only in the policy: it holds in every mode, whether or not eidolon's policy rows are on. Gain: a refusal that does not depend on the policy. Loss: a second, weaker judge that can disagree with the policy rows.
- ssh allowed through a terminal, but denied for bash: gain, the operator watches the session live; loss, a policy row is bypassed by design.
- list shows other chats' ids: needed for mine and for "who opened this". Loss: one chat can learn another chat's id.

## 7) Tests (suite at eade8cb)

Full suite, --features test-bins, no skips, run once at the final tree (eade8cb): 920 passed, 0 failed, 1 ignored (tests/aoide.rs), across 26 test binaries. Per binary: lib 479 (the round-3 inline test is one of them); fake_eidolon 4; perch (main) 4; agent 32; allow 1; aoide 19 (1 ignored); deps_smoke 1; eidolon_config 2; gpl_boundary 2; guard 31; http 132; install 31; mail 1; mail_agent 1; manual_policy 4; mesh 20; meta 18; notes 19; runs 38; runs_log 14; term 18; term_agent 20; term_stream 18; watch 11. fake_aoide and fake_minerva have no tests. Doc-tests run none. Clippy: clean both ways. Flake watch, not in that count: the verifier saw two other full runs at eade8cb go red. In one, tests/install.rs fake::a_key_write_outlives_its_client_and_holds_the_writer_lock_until_it_exits panicked at install.rs:181: calls() (179-181) parses a half-written last log line while the poll loop (799-805) still reads the log the fake is writing. In the other, watch::tests::a_sink_panic_is_logged_without_its_message failed. Both files are outside T2's grant and not fixed in T2; tests/install.rs is not changed by T2. Neither reproduced in the fix round 3 full run at eade8cb, which was green (920 passed, 0 failed, 1 ignored).

term_agent: 20 tests, none added in round 3. Round 3 tightens open_takes_the_chats_folder_and_refuses_a_fifth_terminal (title, size, plain 120 by 32) and open_then_send_then_read_for_the_text_answers_with_it (cursor, alt_screen, seq). Round 2 tightened these: list_marks_the_chats_own_terminals_and_no_others (alive, hold, title and fg, and a third chat's terminal); the_operators_hold_refuses_an_agents_send_with_423_and_its_reads_still_answer (the list shows hold on, then off); a_stop_during_a_long_wait_exits_within_three_seconds (the door logs the held read as 503); open_then_send_then_read_for_the_text_answers_with_it (a minute of quiet and a 20 s timeout, and it must answer in under 15 s). The two round-2 tests for an ended terminal are listed in section 2.

The mutation results for round 3 are in section 0. Round 2's were: with the for branch disabled, open_then_send_then_read failed at 20 s; with the terms.json branch broken, the restart test failed with 404.

Also run this round: a_call_that_passes_the_gate_reaches_the_modules_route (tests/agent.rs, 32 passed in that binary).

## 8) Stale wording, not changed

- Fixed in round 2 (9e2bb9b): the doc comment above a_call_that_passes_the_gate_reaches_the_modules_route (perch/tests/agent.rs) said "Until T2 and M3 land the routes are the empty 404 of their stubs". It now says the term module's list answers 200 with an empty list (T2), and that mail/send is still the empty 404 stub until M3 lands. That 404 is neither the gate's 401 nor its 403, so the door got through.
- The assertion message for the term list was changed from "the term module's stub, past the gate" to "the term module's list, past the gate", because the line changed.
- The mail/send assertion still says "the mail module's stub", which is still true: M3 is a stub.

# T2 security review (wf_759b31f7-674, 2026-10-15)

Commit: eade8cb (base 08c01fa). Lenses: scope, input, leak.

Critic verdict: **merge-after-fixes**. Verdict: merge-after-fixes. No high-severity item. I re-read agent.rs (all of it at eade8cb), the term/mod.rs reserve, agent_input and close paths, pty.rs spawn and hold, serve.rs gate ordering and body_of, and term_agent.rs and tests/agent.rs. The critic's findings hold, and rounds 2-3 changed only docs and tests in agent.rs (diff 97a0bcd..eade8cb), so the reviewed logic is the round-1 logic. Checked and clean: every spec test bullet has a test; agent_last_ms is stamped by note_agent (pty.rs:1008-1012); the scoped-token compare is constant time (agent/token.rs:228); the shell's descriptors are closed on exec (pty.rs:652-654); close has only two callers (agent.rs:464, routes.rs:251) and shutdown is the only other hang-up; the agent's bytes pass through char-based typed_text and clean_title. Blockers before merge are small: decide close-while-held, pin the term token's refusal on the operator's hold and close routes, and fix the T2 risk text. The rest are operator decisions or low-severity notes. ( ˘ω˘ )

## Critic gaps

1. **close ignores the operator's hold (critic scope-2, medium; fix before merge)**
   - Why: The spec makes the hold the operator's per-terminal stop, and it is enforced for send only. close (agent.rs:452-469, owner check at 461, no hold check) ends a held terminal, and auto vouches term_close (BRIEF D6), so it runs silently. close.rn:5 tells the model a held terminal is 'theirs for the moment: leave it', and close.rn:125 describes a 423 that close never returns. The perch and its own tool text disagree.
   - Check: Operator decision. Either refuse close with 423 while info.hold (same check as send at agent.rs:445) and add a test beside the_operators_hold_refuses_an_agents_send_with_423 (term_agent.rs:401-427), or write the exception into the term/README.md hold table and fix close.rn:5 and :125. While there, move the owner rule into Terms::close (critic scope-3) so it is not only in the route.

2. **no test that the agent token is refused on the operator's hold and close routes (test gap, cheap; fix before merge)**
   - Why: The claim that an agent cannot lift the hold rests on reading routes.rs:230-243. tests/agent.rs:519-537 sends the term and mail tokens only to /hub/term/t1/input, /hub/term/events, /hub/term/a/x/y and similar. Nothing sends them to POST /hub/term/<t>/hold, the one route that lifts a hold, or to /hub/term/<t>/close or /resize (routes.rs:249-251).
   - Check: Add POST /hub/term/t1/hold, /hub/term/t1/close and /hub/term/t1/resize to the list at tests/agent.rs:519-537 and expect 401 with an empty body for both tokens. No code change needed.

3. **T2 risk text in term/README.md (critic leak-1, leak-2, leak-3, scope-1, leak-4; fix before merge)**
   - Why: The section at term/README.md:196-200 names reads, untrusted text and yolo, and leaves out four things an agent can reach: the master bearer file under XDG_RUNTIME_DIR (lib.rs:355-358, 388-395; the shell keeps XDG_RUNTIME_DIR per pty.rs:512-531); exported credentials in the shell env (pty.rs:512-531, the T1b test checks only MINERVA_TEST_TOKEN); list titles and fg for every chat (agent.rs:166-186); and keys bypassing the text guard (agent.rs:398-405). Line 119 says tokens are in 0600 files with no same-uid caveat, and line 335 says ^X where typed_text emits U+2400 glyphs (term/mod.rs:981-991).
   - Check: Name each item in the T2 section, with the same-uid caveat that :114-116 gives terms.json. State that keys are not judged (decide the up, ctrl-r and tab question with the operator). Correct :335 to match typed_text. Keep the guard row changes as the critic proposed, and label them hints.

4. **reads leave no operator-visible trace (new, low; operator decision)**
   - Why: Change (term/mod.rs:243-252) has no read variant, agent.rs has one tracing call (224, open failure only), and the stream shows term-input only for sends. Under auto, read is vouched (D6), so any chat can copy any web terminal's screen, secrets included (ruling 2), to the model provider with nothing on the operator's ⌨ tab. The README says reads are exposed but not that they are invisible.
   - Check: Decide whether a read should emit a stream event (chat, terminal id, lines; no text) so the operator sees who reads what. If not, state in the T2 section that reads leave no record. No code is needed to decide this.

5. **the 16-terminal table has no operator share (new, low; operator decision)**
   - Why: AGENT_CHAT_CAP is 4 per chat (term/mod.rs:77), but MAX_TERMS is 16 for the whole perch (term/mod.rs:59). reserve (mod.rs:620-677) evicts only ended terminals of the same chat (635-649), so a running terminal is never let go. Four chats with four running terminals each (16 total) make the operator's own create refuse with 409 'the perch holds 16 terminals' (routes.rs:160-161), and every other chat's open refuses too. Hold does not free a slot.
   - Check: Decide on a reserve for the operator or a lower total for agents. Add a test that the operator can open while the agents hold their caps (for example four chats, four each, then an operator open).

6. **the text guard judges only this send's text, not the shell's line (new, low; hint class)**
   - Why: refuses_term_text runs on text only (agent.rs:405, guard/mod.rs:680-687). If the operator has typed 'aoi' in t1 without Enter, an agent's {text: 'de pair', keys: ['enter']} makes the shell run aoide pair, and 'de pair' passes. This is a split across the operator's input, which the critic's split-send note does not cover (reasoned from the segmenter, not run).
   - Check: State in the guard hint (guard/mod.rs:672-679) and the send tool text that the guard sees only this call's text, and add the operator-pending-line case to the pinned misses next to tests/guard.rs:557.

7. **chunked over-cap body untested (new, low)**
   - Why: declared_over (agent.rs:241-245) refuses a declared length over 16 KiB, and the only test, term_agent.rs:581-589, sends a Content-Length. A chunked body goes through body_of's Limited (serve.rs:516-536), which is the cap that actually holds, and no test drives it through a term route.
   - Check: Add a chunked over-cap send to a term route and expect 413, with the route not reading past the cap.

8. **chat identity is the eidolon session stem; resume inherits terminals (new, low; assumption not verified)**
   - Why: The chat is the session log's file stem (sessions.rs:84-95, used at serve.rs:875). T2's mine, close and is_agents_of rest on that stem being unique and kept across a resume, so a resumed chat inherits its earlier terminals (mine true, close allowed). The eidolon side of id generation was not checked, and no test or README line says resume inherits.
   - Check: Confirm in eidolon that a new session gets a fresh id and --resume keeps it. Then say in term/README.md that a resumed chat keeps its terminals. If that is not wanted, the fix is on the perch side.

9. **agent-supplied strings reach the operator's page (new, low; contract for T4a)**
   - Why: The open title (cleaned, 80 characters), the term-input text (a picture, 80 characters) and fg reach the operator's stream and list. webui/term/terms/index.js is a stub at this base, and the stream section of term/README.md (about lines 332-335) does not tell the page to render them as text. The critic left page rendering unverified.
   - Check: Add a line to the stream section: agent-supplied strings are text and never HTML. T4a's review must check that the page follows it, and no innerHTML (brief hard rule).

10. **live check at 48115 not run on the final tree (info)**
   - Why: The spec's live check (fake perch on 48115 with curl -N showing term-input and term-out) was run on round 1 and not repeated. The diff shows the route logic has not changed since 97a0bcd, so the round-1 result still applies to the logic.
   - Check: The orchestrator runs the live check once on eade8cb, or accepts the round-1 result on the diff evidence. Use only the loopback port 48115 and never the operator's perch.

## Confirmed findings

### scope-1 (low): Keys bypass the D9 text guard: an agent can re-run a refused command from shell history or completion

`perch/src/term/agent.rs:405`

guard::refuses_term_text is called on `text` only (agent.rs:405). Named `keys` are appended to the bytes unjudged (agent.rs:398-404; key_bytes at 416-438 accepts up, tab, enter and ctrl-r). The operator has typed `hoot keys allow-web <FP>` in t1, which D9 denies to agents. Later an agent in yolo (no ask) sends {"id":"t1","keys":["up","enter"]}. The text is empty, the guard returns None, and ESC[A then CR are queued, so a readline shell recalls and runs the arm command. Second path: {"id":"t1","text":"ao","keys":["tab","enter"]} completes to aoide when it is the only ao* program on PATH. guard/mod.rs:678-679 discloses tab completion as a hint, but it does not mention history recall or ctrl-r. Shell behaviour is assumed from readline and was not run.

- Fix: Judge what the shell will receive rather than the request text, or refuse the recall and completion keys (up, down, ctrl-r, and tab when text is empty) on terminals the agent did not type into itself. If tab stays allowed for legitimate completion, state in the send tool text (plugins/term/tools/send.rn:5) and the D9 row that keys are not judged, and get the operator's decision before merge.
- Verify: The code path is real. agent.rs:398-404 appends named-key bytes without judging them, and agent.rs:405 passes only `text` to refuses_term_text, which judges that one string. An empty text gives no words to judge, so a keys-only send such as up+enter is never judged, and up, ctrl-r and tab are unjudged. Severity is downgraded from medium to low for three reasons. (1) The guard is documented as a hint: guard/mod.rs:672-679 says a split send and tab completion pass, and install/mod.rs:40-49 says an agent's term_send can type `hoot keys allow-web` and that the term-text rows are a hint, not a gate. (2) The same outcome is already reachable by splitting text across two sends, which the agent can build from the screen it may read (read is ruling 2). Recall adds no new reach. (3) The arm is bound to one fingerprint (install/mod.rs:40-41), so a recalled arm can only re-arm the operator's own key. The trigger also needs yolo, since auto asks on each term_send (BRIEF D6). Shell recall behaviour was assumed from readline and not run. The fix (judge the shell's received bytes) is not achievable generically, so documenting keys as unjudged in send.rn and the README is the right scale.

### scope-2 (medium): close ends a terminal the operator has put on hold; the hold is enforced only for send

`perch/src/term/agent.rs:464`

close checks only ownership (agent.rs:461) and then calls Terms::close (term/mod.rs:756-770), which has no hold check. The hold table (term/README.md:169-183) lists input only, and no test closes a held terminal (tests/term_agent.rs:401-420 covers send only). The agent opened an ssh terminal t3, the operator took it over (hold on), and the agent's tidy-up POST /hub/term/a/close {"id":"t3"} returns 202. The shell is hung up and whatever ran in it ends. plugins/term/tools/close.rn:5 tells the model 'A terminal the operator has taken over (held) is theirs for the moment: leave it', so the perch and its own tool text disagree.

- Fix: Answer 423 to close while info.hold, as send does, and add a test beside the_operators_hold_refuses_an_agents_send_with_423. If the operator wants close to stay open while held, write that into the hold table and change close.rn:5. This is an operator decision.
- Verify: Confirmed. The close route checks only ownership (agent.rs:461-463). Terms::close (term/mod.rs:756-770) has no hold check, and the hold is read only in agent_input (mod.rs:704-715). The hold table in term/README.md:169-182 lists input rows only. The shipped tool text close.rn:5 tells the model a held terminal is 'theirs for the moment: leave it', and close.rn:125 describes a 423 that close never returns. No test closes a held terminal: term_agent.rs:401-427 covers send and read under hold only. The inventory (section 6) says 'hold blocks send only' as a trade-off, but the model-facing text disagrees. Auto mode vouches term_close (BRIEF D6), so the call is silent there. Trigger: the operator takes over an agent-opened terminal, and the agent's routine tidy-up closes it, which hangs up the operator's running job. The spec never required 423 on close, so the code matches the literal spec, but the perch and its own tool text contradict each other. Medium holds. This is an operator decision.

### scope-3 (low): Owner rule for close and the chat argument of agent_input live in route handlers, not in Terms

`perch/src/term/mod.rs:756`

Terms::close(id) (mod.rs:756) takes no owner, and Terms::agent_input(id, chat, bytes) (mod.rs:704) accepts any chat string, used only as the event label. The owner rule sits only in agent.rs:461. A later slice that calls Terms::close from a new verb, or passes a body-supplied chat into agent_input, removes another chat's terminal or attributes operator-visible input to the wrong chat, and the table does nothing to stop it. The comment at mod.rs:700-703 ('the route's decision (T2)') is the only contract.

- Fix: Give close an Owner argument that returns a NotYours error from Terms, and keep an unchecked close under a separate name for the operator route. Pass the chat into agent_input as an Owner taken from the Caller, so the rule is enforced where the data is.
- Verify: Real as a latent contract, with no present reach. Terms::close (mod.rs:756) takes no owner, and agent_input (mod.rs:704-715) takes the chat only as the Change::Input label. The current callers are correct: the agent route checks ownership before close (agent.rs:461-464), and the operator's close route (routes.rs:249-251) is operator-only by design. The failure needs a future slice to call close or agent_input with an unchecked chat, so low fits the brief's 'contract later slices could break'.

### scope-4 (info): read reaches any web terminal with no perch-side consent

`perch/src/term/agent.rs:249`

Any chat's agent can read the operator's own terminal (agent.rs:249-287, ruling 2), including a secret the operator typed, and the text goes to the model provider. The only gate is the plugin approval tier, approval read_only (plugins/term/tools/read.rn:6), and under auto the D6 vouch makes it silent. That vouch is in eidolon, not in this diff. term/README.md ('What an agent's terminal tools expose') already states this, so no code change is needed in T2.

- Fix: None in T2. The operator should know that the protection for operator terminal text is the plugin's approval setting and not the perch.
- Verify: Accurate. read (agent.rs:249-287) checks neither owner nor hold, so any web terminal is readable (ruling 2). read.rn:5 and term/README.md:196-201 state the exposure. The only gate is the plugin's read_only approval (read.rn:6), which lives in eidolon and is outside this diff. No change in T2.

### scope-5 (info): list shows every other chat's id, title and foreground command

`perch/src/term/agent.rs:180`

A chat learns another chat's id from list (Row.chat, agent.rs:180). Chat ids are not credentials: the gate takes the chat from the peer socket (token.rs:266-277), so knowing an id gives no reach.

- Fix: None needed. Recorded because ruling 2 makes it visible to every chat.
- Verify: Accurate. The chat is taken from the Peer extension via attrib::chat_of_hinted (token.rs:266), and a socket that no live door owns is refused with 403 (token.rs:267-270). list returns every row's chat id (agent.rs:175-180), but an id reaches nothing without the socket. No fix needed.

### scope-6 (info): open starts in $HOME when the chat has no known folder, not in the chat's known_cwd

`perch/src/term/agent.rs:199`

With no remembered folder, known_cwd returns None (agent.rs:199), so OpenSpec.cwd is None and the shell starts in $HOME (term/mod.rs open: cwd unwrap_or_else host.home()). The brief says cwd = the chat's known_cwd. The operator create does the same for a listed chat with no folder (term/routes.rs:145-156), so this is consistent but wider than the wording. The start folder is not a boundary anyway, since any shell can cd. Changes only if the spec is read literally.

- Fix: None unless the spec is read literally. If it is, refuse open with 422 when the chat has no known folder.
- Verify: Behaviour confirmed. agent.rs:197-199 passes known_cwd (None when unknown), and term/mod.rs:593 falls back to host.home(). The operator create path does the same for a listed chat with no folder (routes.rs:109 maps NoFolder to None). This is consistent with the code base but differs from a literal reading of the brief's 'cwd = the chat's known_cwd'. Info only.

### scope-7 (info): A chat's live terminals outlive its door and stay drivable by every live chat

`perch/src/term/mod.rs:756`

When a door stops, nothing closes the terminals its agent opened: the only callers of Terms::close are the close routes and tests, and shutdown (mod.rs:795, 1008) is the only other hang-up. Those shells stay listed under the dead chat's id, alive true. Any live chat can read and send to them (ruling 2, agent.rs:166-287), and only the operator can close them, because the dead chat's token now gets 403 at the socket step (token.rs:266-270). A token whose chat is gone cannot act, which is what the scope asked. The terminals keep acting, by spec (D3 ends terminals only with the perch).

- Fix: None by spec. Decide whether a door stop should end its agent-opened terminals or leave them to the operator.
- Verify: The conclusion holds: no door-stop path ends an agent's terminals. Terms::close has two callers, agent.rs:464 and routes.rs:251. The other hang sites are shutdown (mod.rs:478 commit refusal while stopping, mod.rs:795 shutdown, Drop at 1008), eviction of ended terminals only (mod.rs:640-687, which takes only ended entries), and prune of exited shells only (mod.rs:838-851). The finding's line list is incomplete, but none of those sites is a door stop. A dead chat's token gets 403 at the socket step (token.rs:266-270). D3 says terminals end with the perch, so this is by spec.

### input-guard-history-recall (low): Keys up and ctrl-r recall shell history the guard never reads

`perch/src/term/agent.rs:405`

State: the operator ran `aoide mail send ...` earlier in a WSL terminal. bash (or zsh) loads its history file when the shell starts, and pty.rs child_env (lines 512-525) sets only TERM, COLORTERM, MINERVA_TERM and LANG, so HISTFILE is not changed. Inputs: send {id, keys:["up"]} has no text, so refuses_term_text is never called (the guard is applied only to `text` at agent.rs:405; keys are turned into bytes at 398-404 with no check). The answer is 200. A read then shows the recalled line on screen, and send {id, keys:["enter"]} runs aoide. Variant: keys ["ctrl-r"], then text "aoi" (not "aoide", so segments at guard/mod.rs:464-498 passes it), then keys ["enter"]. Every step is 200 and no 403 is returned. Under yolo nothing asks. Under auto the approval shows only the keys. Each up press also walks older history, including any secret the operator once typed on a command line, into the model provider. The risk text in T2 and the agent.rs module doc cover the screen only, not history.

- Fix: Either drop up, down and ctrl-r from the named keys offered to agents (the guard cannot see the buffer they recall), or judge the line the shell is about to run when a send presses enter. The first is the one that cannot be wrong. Add a term_agent test: after an aoide line sits in history, keys [up] then [enter] must not run it, and a keys-only send must be covered by the guard rather than skipped.
- Verify: The mechanism is real: a keys-only send skips the guard, and the shell is an interactive login shell on a PTY, so up and ctrl-r recall history. Downgraded from medium. The text-only contract is what the spec and route doc state, and the executor's inventory (section 5, item 5) says keys bypass the text guard by design. Tab completion (text 'aoi' then key tab) reaches the same program with no history, and it is a pinned miss (tests/guard.rs:557 'aoi\tde x', guard/mod.rs:678-679), so recall adds no new reach. The proposed fix does not close the gap: text is typed as raw bytes, so '\x1b[A' in text presses up, and ctrl-r is a ctrl-letter the named-key list already allows. Under yolo an agent can already run arbitrary shell through term_send; only aoide, keys and the update-keys directory are refused. The real gap is documentary: the risk text does not say keys can surface history, although unguarded text such as 'cat ~/.bash_history' already reads it.

### input-guard-shell-view (low): Guard judges raw text; the shell applies quoting and control bytes first

`perch/src/guard/mod.rs:476`

segments() (guard/mod.rs:464-498) drops only backslash, double quote and single quote (lines 467-469) and splits only on ; & | ) ` newline CR and, outside code, ( { } (line 476). Inputs, each one send and each judged 200 with no 403: (a) text "$'\\x61oide' pair\r": the quotes are dropped but the $ is kept, so the word is $x61oide. base() (305-307) and start() (347-360) see that program, not aoide, and bash expands $'\x61' to a and runs aoide. (b) text "aox\u0008ide pair\r": \x08 is not a boundary, so the word is aox\x08ide. readline's ^H binding erases the x and the shell runs aoide pair. (c) text "\u001b[200~aoide pair\u001b[201~\r" (bracketed paste markers): the word is \x1b[200~aoide, which does not match, and readline pastes and runs it. Pinned misses in tests/guard.rs:557 cover DEL and a same-send variable (a=aoi; b=de; $a$b) only, so these forms are unpinned. Control input is also not bounded by the closed key list: text is typed as raw bytes (agent.rs:387 and 398), so \u0003, \u0004 and ESC sequences such as \u001b[3~ are pressed although KEY_NAMES (agent.rs:67) names the closed set. Under yolo the policy answers yes and the perch never asks, so this guard is the only perch-side check on aoide and key changes.

- Fix: Judge a normalised form, not the raw text. Refuse any text holding a C0 control byte other than tab, LF and CR (ESC included), and any $' or ${ form, since none of them can be judged as the shell reads it. Pin the new forms in tests/guard.rs next to line 557 and add one send case to term_agent.rs. In the route doc, say plainly that text is typed raw, so the named-key list is not a limit on control input.
- Verify: Each bypass reproduces by reading the segmenter, and bash's expansion was checked offline: $'\x61oide' prints aoide. Downgraded from medium. The module doc calls the guard a speed bump and says disguises get past it (guard/mod.rs:40-41). It states the hint misses DEL, tab completion, split sends and variables (guard/mod.rs:678-679), and tests/guard.rs:557 pins DEL, tab, a split and $a$b. The $'...' form and ^H are the same class. The control-byte point (outside KEY_NAMES) is the documented 'typed as it is' contract (agent.rs:376), so it is a doc-clarity matter, not a bypass. Yolo already lets an agent run arbitrary shell, so the aoide row is narrow. The inventory's 'quoting is handled' (section 5, item 4) is wrong for the $' form and should be corrected. Cheap hardening (reject C0 bytes except tab, LF and CR, and $' or ${ forms) is reasonable defence in depth. Medium would need the operator to decide that D9 must hold against deliberate disguise, which is a design change to raise with them.

### input-read-disconnect (low): A read keeps waiting after its client has gone, with no cap on concurrent reads

`perch/src/term/agent.rs:346`

hyper-1.11.1 src/proto/h1/dispatch.rs poll_write (lines 355-365) keeps polling the in-flight service future while should_poll() is true, and is_done (lines 469-483) requires the write side to finish too, so a client that disconnects does not cancel the route future. serve.rs (lines 145-178) has no concurrency cap on connections. So a read whose caller has gone runs to its own end: the 60 s timeout at most, a match, quiet, or ctx.stop. Failure: an agent retries a timed-out read 200 times, leaving 200 settle() waits. Each wakes on every terminal tick (term/mod.rs:547-549, agent.rs:348-353), and a for-wait copies its terminal's screen (up to 400 lines and 7000 characters, agent.rs:337-344 and 360-364) on each change of that terminal's seq. The per-call bound holds, but nothing bounds the number of waits per chat, and no test covers a disconnect.

- Fix: Cap concurrent reads per chat (for example 8, refused in the route's own refusal style) so one chat cannot stack waits. Keep the per-call timeout as the backstop. Optionally let the for-wait compare against the seq it last copied, which it already does, but cap the copy size while waiting.
- Verify: Confirmed by reading hyper 1.11.1: when a client disconnects, the h1 dispatcher does not cancel an in-flight service future. Read EOF closes only when the write side is already closed, and is_done needs the write side finished too, so settle() runs until its own end, at most 60 s. Neither the route nor serve.rs caps concurrent reads or connections, so a chat can stack waits. Real but low: the per-call bound holds, and the cost is waiter pile-up from retries, which needs many reads. A per-chat cap is reasonable defence in depth.

### input-wait-wall-clock (info): Quiet timer uses the wall clock while its sleep is monotonic

`perch/src/term/mod.rs:372`

now_ms() is SystemTime (mod.rs:372-374). settle() stamps quiet_at from it (agent.rs:331-332) and sleeps on Instant (345). A backward wall-clock step (for example an NTP correction) puts quiet_at far in the future, so an idle-only read sleeps until its timeout (30 s by default) instead of idle_ms after the last output. A forward step makes it return early. The timeout bound itself still holds, because the nap is capped by time left (agent.rs:333 and 345).

- Fix: Stamp the terminal's last write with a monotonic clock for the quiet test, or accept the skew and note it in the module doc. No security impact.
- Verify: Real. now_ms() is wall-clock SystemTime, and last_ms is stamped with it, while the nap is a monotonic sleep. A backward wall-clock step makes an idle-only wait run to its deadline unless output wakes it. A forward step makes it return early. The subtraction at agent.rs:345 cannot underflow, because the check at line 334 returns first. The timeout bound still holds. No security impact, so info.

### leak-1 (medium): Master bearer file reachable from an agent's shell, output returned to provider

`perch/src/guard/mod.rs:523`

Input: any chat with term_open (agent.rs:191) and term_send. The perch's bearer is the 256-bit token in $XDG_RUNTIME_DIR/minerva/perch-<port>.token (lib.rs:237, 388-395): 0600, same uid as the shell. XDG_RUNTIME_DIR is required at perch start (lib.rs:355-358) and survives pty::child_env (pty.rs:512-530), since only MINERVA_* names are dropped. A send of cat "$XDG_RUNTIME_DIR/minerva/"perch-*.token is not refused: refuses_term_text (guard/mod.rs:523-531, 680-686) matches only aoide, hoot keys, minerva keys add/remove, --keys-arm with perch-open.sh or minerva, and the words minerva/update-keys. Outcome A: term_read (agent.rs:249-287) returns the printed bearer to the model provider. Outcome B: with $(cat ...) nothing is printed, but the bearer clears the master gate from any socket (serve.rs:268-270, 279-291), so curl to /hub/install/keys/arm (install/mod.rs:889-905, bearer only, no browser headers) arms a fingerprint without the person-in-a-terminal step that install/mod.rs:35-47 describes, and /hub/install/keys/add (install/mod.rs:809-814) then runs with that armed fingerprint. POST /hub/code (serve.rs:664) mints a login code from the same bearer. Nothing asks in yolo (BRIEF D6). Context: bash in yolo already reaches this file (install/mod.rs:45-48 accepts that same-uid reads the bearer), so T2 adds a second channel that the guard calls a hint rather than creating the only one. The gap is the documentation: term/README.md:196-200 (the T2 risk section) and :119 (tokens 'in 0600 files') never say the bearer file is readable from a shell, while :114-116 does say that for terms.json.

- Fix: Name the bearer file in the T2 risk section and at term/README.md:119, with the same same-uid caveat that :114-116 gives terms.json. Add the token path, /hub/install/keys/arm and /hub/code to refuses_term_text as hint rows, and say in the code comment that they are hints. Refer the arm-then-keys/add consequence to the auth review, since arm was meant to need a person in a terminal.
- Verify: The chain holds step by step: the perch's bearer file is $XDG_RUNTIME_DIR/minerva/perch-<port>.token, mode 0600 and same uid (lib.rs:355-358, 388-395); child_env keeps XDG_RUNTIME_DIR (pty.rs:512-531, only MINERVA_* private names are dropped); refuses_term_text (guard/mod.rs:680-687) judges only aoide, the minerva/update-keys word (528-530), and keys/--keys-arm/keyfile rows (585-613), so a send of cat on the token path passes (cat is flavor 0, no rule). term_read (agent.rs:248-286) returns the screen to the model. The arm route takes the bearer only with no browser headers (install/mod.rs:889-905), add runs under that arm (809-814), and POST /hub/code mints a code from the bearer alone (serve.rs:664-666). The finding overstates severity. Default mode is yolo (mode.rs:65-66), where an agent's own bash is a same-uid child that already reaches the same file, and the install module's 'What the arm does not defend' paragraph already states that anything running as the uid can read the bearer and that terminals would let an agent arm. So T2 adds no reach in the default mode. Its real increments are: auto vouches term_read (BRIEF D6), manual and auto ask only on term_send, the T2 risk section (term/README.md:196-200) and the residual list (:114-119) do not name the bearer file, and refuses_term_text has no rows for the token path, /hub/install/keys/arm or /hub/code. The auth-bypass framing belongs to the auth review, as the finding's own fix says. Citation drift: refuses_term_text is at guard/mod.rs:680-687, not 523-531.

### leak-2 (low): Shells inherit every non-MINERVA credential, and agents can now read them

`perch/src/term/pty.rs:512`

Input: an agent with term_open runs env or printenv, then term_read. Shells start from the perch's full environment (term/mod.rs:347 std::env::vars_os()) minus only names that start with MINERVA_ and contain TOKEN, SECRET, CODE or PASS (pty.rs:527-530). Any other credential the operator exported to the perch (*_API_KEY for a provider, GITHUB or cloud tokens, SSH_AUTH_SOCK) reaches the screen and then the model provider. The installer's child env is an allow-list (install/mod.rs:127 PASSED, :349), so the terminal path is inconsistent with it. The T1b test named the_shell_has_its_terminal_id_and_none_of_the_perchs_credentials (term/mod.rs:1586) pins only MINERVA_TEST_TOKEN, so its name claims more than it checks. term/README.md:117-119 documents the inheritance as the operator's own login, but the T2 risk section (:196-200) omits it, and T2 is the first route that lets an agent open the shell and read it. Whether this operator's perch carries such variables was not checked.

- Fix: Switch child_env to an allow-list like install's PASSED, or state in the T2 section that every exported credential reaches agents. Narrow the test's name to what it checks, or extend it to the other credential names.
- Verify: The inheritance is real and was a deliberate residual: pty.rs:11-12 says the shell has the operator's own login, and term/README.md:117-119 documents it. The finding overstates the T2 delta. The door is forked with the perch's full environment (spawn.rs door_command, no env_clear), so an agent's own bash, a same-uid child, already sees the same variables unless eidolon clears env for bash (upstream, not checked). So T2 is not the first route to these names, and the installer allow-list comparison is weaker than stated because the door itself is not allow-listed. The test-name overclaim is real: the_shell_has_its_terminal_id_and_none_of_the_perchs_credentials plants and checks only MINERVA_TEST_TOKEN (term/mod.rs:1300-1308 and 1583-1595). That test is T1b's and outside T2's owned files, so it is a follow-up, not a T2 fix.

### leak-3 (low): list hands every terminal's OSC title to every chat, outside the stated risks

`perch/src/term/agent.rs:166`

Input: any program writing into a web terminal (a remote host, a catted log) sends OSC 0 or 2 with text. screen.rs:149 keeps it after clean_title (screen.rs:378-380: 80 characters, no control or invisible characters), and term/mod.rs:198 keeps it on restore. list (agent.rs:166-186) returns it to every chat, along with fg (pty.rs:1021-1022). Outcome: up to 80 characters of attacker text enter every agent's list call, a route the T2 risk section (term/README.md:196-200) does not name; that section names only read as the untrusted route. A normal shell title (user@host:path) also tells the provider the operator's user, host and folder for every terminal, on every list call.

- Fix: Name list's title and fg as untrusted in the README's T2 section, or omit titles from agent list rows. Either way the section should say list is a second route for untrusted text.
- Verify: Confirmed. The list rows carry title and fg for every web terminal, and send_refused-style gating does not apply to list. The title is the OSC-driven title, cleaned by clean_title (80 characters, control and invisible characters dropped), and fg is the foreground process's comm from /proc, cleaned the same way. Both reach every chat's list call. The T2 risk section names only read as the untrusted route, so list is an unnamed second route for untrusted text. Impact is up to 80 characters of attacker-written text per row, plus a shell title that may disclose user, host and folder. Low, documentation and contract.

### leak-4 (low): term-input frame documented as ^X pictures but emits U+2400 glyphs

`perch/src/term/README.md:335`

term/README.md:335 says other control characters appear as ^X in the term-input frame. typed_text (term/mod.rs:984-991) renders each control character as a U+2400-block glyph (ESC becomes U+241B, not ^[), and the README's section on pictures (around :193) uses a third form again (^M as the return glyph). A page slice (T4a) that parses or displays the frame by the README will misread what the agent typed, so the operator's view of agent input can disagree with the frame's text.

- Fix: Make term/README.md:335 match typed_text (glyphs for C0 and DEL, shown characters as they are, invisible ones dropped) and use one picture form throughout the README.
- Verify: Confirmed as a docs/code mismatch. term/README.md:335 says other control characters appear as ^X, but typed_text emits U+2400-block glyphs for C0 controls (ESC becomes U+241B), U+2421 for DEL, and drops characters the screen would not show. The README is also inconsistent with itself: :193 uses the glyph form (^M as the return sign), so there are two forms, not three as the finding says. :335 also says CR and LF both show as the return sign, while the code gives CR U+240D and LF U+240A. Low, but a contract a later page slice (T4a) could parse wrongly.

## Rejected

- **leak-5**: list exposes every chat's id; safe only while routes take the chat from the socket. The fact holds: list rows expose each terminal's opener chat id, and every agent route takes the chat from the Caller in the request extensions, never from a body, query or header. But no T2 failure can be shown. The stated hazard needs a future route that takes a chat id from a body, and none exists in this slice. Drop it as a finding and carry it as a contract note in the slice notes: agent routes keep taking the chat from Caller only.

## Checked

- [scope] Gate 1 for /hub/term/a/*: serve.rs:378-391 runs agent::authenticate before the master check, and the Caller is inserted only on success. The scoped term token is compared only to the term scope's own token (agent/token.rs:230-232, 258-263), never to the mail token or the master token.
- [scope] Master bearer and same-origin login cookie on agent paths get 404, a missing or wrong token gets 401, and a live-socket miss gets 403 (token.rs:237-263; agent/mod.rs:9-17). A cross-site cookie is not accepted (master_presented requires same-origin).
- [scope] Non-agent /hub/term routes (events, operator create/input/hold/close) accept only the master bearer or a same-origin cookie (serve.rs authenticated, around 273-290). The scoped term token gets 401 there, so an agent cannot reach the operator's routes. set_hold is called only from the operator route (term/routes.rs:243).
- [scope] Path agreement: agent_scope (agent/mod.rs:65-73) and pick/below (term/mod.rs:1024-1031; serve.rs:585-607) read the same raw path with one slash stripped. Percent-encoded, double-slash and trailing-slash spellings reach a 401 or a 404, never an agent route. Tests at term/mod.rs:1066-1090 and agent/mod.rs:115-152 pin this.
- [scope] Gates 2-4 (stop, Host, Sec-Fetch-Site, OPTIONS, method) run after gate 1 for agent paths (serve.rs:387-420), so a valid token from a DNS-rebinding or cross-site origin is still refused.
- [scope] The chat id is never read from a request: the agent routes take only the Caller extension (agent.rs:146-149), and every body struct is deny_unknown_fields (agent.rs:75-117). A cwd or chat in a body is a 400.
- [scope] Attribution: socket inode, then live door pid, then Registry::id_of_pid, which requires the door to be is_up (sessions.rs:1110-1127). A child process (curl, bash) has its own socket and gets 403 (attrib.rs:12-17). The hint cache is re-validated with holds_at before use (attrib.rs:95-110), so a dead chat's token gets 403.
- [scope] Hold: WriteQueue::agent_input refuses under the queue lock (term/pty.rs:215-222, Terminal::agent_input 1065-1075). Turning the hold on drops queued agent bytes (pty.rs:243-250). Terms::agent_input maps Held to 423 (agent.rs:446). The guard runs before the hold check and a missing terminal gets 404 before 423 (agent.rs:384-407). No agent route calls set_hold, so an agent cannot lift a hold.
- [scope] Cap and eviction: reserve counts and evicts only entries with is_agents_of(chat) (term/mod.rs:106-108), so an operator terminal linked to a chat (By::Operator, routes.rs create) is never the agent's. The 5th open gets 409 under one lock.
- [scope] close: the owner check (agent.rs:461) refuses operator and other-chat terminals with 403, and an unknown id gets 404 (agent.rs:457-467).
- [scope] read and list cover the operator's, other chats' and terms.json-restored terminals, as ruling 2 says (term/mod.rs:567-575, 731-737). list's mine uses the same predicate as close (agent.rs:175).
- [scope] Shutdown is the only path that ends all terminals (term/mod.rs:795, 819, 1008). No door-stop hook closes a chat's terminals.
- [scope] Tests: term_agent.rs covers the close 403 and the cross-chat read and write (tests 373-396), the held send 423 with reads still answering (401-420), and the fifth open 409. No test closes a held terminal, so that behaviour is unspecified rather than pinned.
- [scope] Not verified, outside this diff: chat id minting and reuse (sessions and serve, not in the diff); page rendering of agent-supplied titles (webui/term/terms/index.js is a stub at this base); D6 approval behaviour in eidolon; readline behaviour of up, tab and ctrl-r (assumed, not run).
- [scope] No high-severity scope finding: the gate, attribution, hold and cap all hold as specified. Findings are the two mediums (keys bypass the D9 guard; close ignores the hold), one low contract note, and informational items.
- [input] Body cap: fields() reads through body_of's Limited (serve.rs:516-536) under JSON_BODY 16 KiB (agent.rs:44, 232-239). A declared Content-Length over the cap is 413 before any read (agent.rs:242-245; test term_agent.rs:581-590).
- [input] Unknown fields: every body struct is deny_unknown_fields (agent.rs:75-117), so cwd or chat in a body is 400 (test term_agent.rs:548). An empty body counts as {} (agent.rs:237).
- [input] send bounds: empty send 400 (agent.rs:389), text 4 KiB by bytes (392), keys at most 32 (395), unknown key 400 (400-403). All run before any byte is built into the send or any guard call, so nothing reaches the queue on these.
- [input] Guard placement: refuses_term_text is called on the one text path (agent.rs:405) before agent_input (408). Keys are not judged (see input-guard-history-recall).
- [input] Guard truncation: TERM_TEXT_LIMIT (guard/mod.rs:302) is 8 KiB, but send caps text at 4 KiB, so the cap is unreachable from the route. The inventory's claim on this is correct.
- [input] Inventory correction: the inventory describes a variable set in one send and used in another as a miss. Same-send assignment and expansion also pass (is_assign at guard/mod.rs:315-320 skips the assignment, and the $-word is not a program). The pin at tests/guard.rs:557 covers that same-send form.
- [input] Refusals before the queue: agent_input is all or none, and the hold is checked before the size cap under the queue lock (pty.rs:215-229 and pty.rs:1065-1070; term/mod.rs:704-716). Tests check that a refused send types nothing and stamps nothing (term_agent.rs:401-419 and 506-522).
- [input] read bounds: lines 1..=400 and max_chars 1..=7000 (agent.rs:257-264 and bounded 368-374). wait.for 1..=200 chars (305-306), idle_ms at most 60000 (308-310), timeout_s at most 60 (312-315). Timeout arithmetic: quiet_at minus now is computed only after the now >= quiet_at early return (334), so it cannot underflow, and the nap is capped by time left (333, 345).
- [input] read stop: the select arm on ctx.stop returns Settled::Stopping (agent.rs:354), which is a 503 (272). Pinned by term_agent.rs:459 (a stop during a long wait exits within three seconds).
- [input] open: cols and rows are u32 and go through clamp_size before the u16 cast (screen.rs:68-72), so no overflow. Title is cleaned by shown() and then cut at 80 characters (screen.rs:378-380). Control and invisible characters are removed, and the same filter is used for the typed-text echo (mod.rs:981-1000).
- [input] open refusal order: reserve() returns ChatCap or Cap before any eviction (mod.rs:651-669), and it evicts only the chat's own ended terminals, so a refused open changes nothing.
- [input] Error responses do not echo input: they are static strings (agent.rs:61-72). close echoes only an id that passed valid_id (agent.rs:457 and 466).
- [input] Method and verb order: a non-POST is 405 with Allow before the body is read, and an unknown verb is 404 (agent.rs:150-162).
- [input] Key names: the ctrl- form matches exactly one a to z byte (agent.rs:431-434), and the inline test pins the 14 named keys and the four non-keys (agent.rs:471-507).
- [input] Refused-send tests: guard refusal (term_agent.rs:506-522), 400 shapes including 33 keys and a 4097-byte text (524-552), and read bounds (554-578).
- [input] hyper behaviour for input-read-disconnect was read from source (hyper-1.11.1 dispatch.rs), not executed. No probe, build or perch run was done; every finding is settled by reading. The shell behaviours used (readline ^H, bash ANSI-C quoting, history loading) are stock bash behaviour, not run.
- [leak] term_read text: built from grid cells through screen::shown (screen.rs:1079-1083, 1087-1108, 298-300), so no raw escape bytes reach it; bidi (U+202A-202E, U+2066-2069), zero-width/invisible and tag characters are dropped (screen.rs:262-289). Ended terminals use the same collect (screen.rs:1110-1114); terms.json is written 0600 and atomically (term/mod.rs:858-885).
- [leak] Titles are cleaned on OSC (screen.rs:149), on open (term/mod.rs:608) and on restore (term/mod.rs:198); fg is clean_title of /proc comm (pty.rs:1021-1022).
- [leak] term-input frame: typed_text caps at 80 characters with an ellipsis, pictures control characters and drops non-displayable ones (term/mod.rs:981-1000). The frame carries the picture, not raw bytes.
- [leak] Logs: no tracing call on the term or agent path records typed input, request bodies or read text. watch.rs:617-626 logs ids and kind only, with a comment saying why. Token logging is file paths only (lib.rs:338-342, agent/token.rs:209-214).
- [leak] Error answers are fixed strings; JSON parse failures answer the shape string, not the body (agent.rs:232-239, 306-314, 372, 390-402). The guard refusal is a fixed sentence (guard/mod.rs:52-55, 680-686).
- [leak] Scoped term and mail tokens are file-based and never in argv or env (lib.rs:58-60; agent/token.rs:72-110). A shell that reads one gets 403 (term) or 401 (mail) from the gate (agent/mod.rs:12-17), so the exposure is the agent's own scoped token reaching the provider, not usable access. Not reported as a finding.
- [leak] Login codes: only POST /hub/code mints one, behind the bearer (serve.rs:664). No terminal route returns one. Covered under leak-1.
- [leak] The README's three stated risks are present (term/README.md:196-200): reads of any web terminal with secrets included, untrusted text, yolo with no ask. Not named there: leak-1, leak-2, leak-3.
- [leak] The inventory's 'Never exposed: the master token (404 on agent paths)' is accurate for HTTP answers only; the file route is leak-1.
- [leak] README wording 'nothing in the perch filters it' (term/README.md:200) understates screen::shown stripping control and invisible characters. Wording only, no finding.
- [leak] Method: every finding was settled by reading code at eade8cb. No probe, test or live check was run; nothing was edited, staged or committed in the worktree or main repo. Excluded per the brief: page rendering (T4a), the stream.rs '(T2)' label, the install, runs_log and runs test flakes.

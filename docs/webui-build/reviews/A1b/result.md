# A1b review (haiku) confirmed 23, rejected 3

## match-replay-window [low] perch/src/watch.rs:1133
Asks opened during a reconnect replay are stamped live and can match a rule
FIX: Set reconnecting = !self.caught_up so every pre-caught-up listing gets seq 0 on every stream, including the first. The post-caught-up broadcast gap cannot be closed from the frames alone: have eidolon's caught-up carry the door's next ask id as of subscribe time, and mark ids below it seq 0 in Held::open. Add a test that opens an ask during the replay (fake_eidolon's replay model cannot yet). Until then the matcher.rs:150 comment overstates the guarantee.

## match-exact-command-not-exact-effect [medium] perch/src/allow/matcher.rs:169
A command rule answers the same bytes even when what they run has changed
FIX: Do not ship command rules for runners that execute workspace-defined code (cargo, make, npm/yarn/pnpm, node, python, sh/bash, ./ or any path-prefixed program) unless the rule is downgraded, or restrict command rules to a closed list of read-only commands. At minimum put the warning in perch/src/allow/README.md and in the page text before merge, and take the XS disposition back to the operator.

## match-claim-not-released-on-decide-failure [low] perch/src/allow/mod.rs:161
A failed rule answer leaves its ask claimed, so POST /allow for it returns 409 for good
FIX: In decide(), on any outcome other than Answered::Done, lock the store and call door.release(ask_id), matching make().

## match-origin-call-id-collision [low] perch/src/watch.rs:474
Origin is joined by call id alone, so a colliding id can borrow another call's origin
FIX: Namespace caller-named ids in dispatch_as (host.rs:529-533) so no model id can collide. In joined(), trust a call's origin only when the tool name recorded for that call id equals the ask's tool.

## match-cwd-snapshot-contract [info] perch/src/allow/matcher.rs:165
The cwd clause compares a hello snapshot, while bash runs in a live dispatcher cwd
FIX: Contract for later slices: any code that calls set_cwd on a web door must also rebind the store entry, or the matcher must read the live cwd from the door.

## match-spec-allow-json-dropped [info] perch/src/allow/store.rs:1
Spec asks for allow.json persistence that the operator's memory-only ruling dropped
FIX: Reconcile A1b.json (drop D10 b) with the operator before A2 or the page relies on it.

## match-test-gaps [info] perch/tests/allow.rs:846
Tests miss the replay window and call-id collisions; NUL is not in the byte list
FIX: Add a replay-window test and a call-id collision test. Mutate watch.rs:1133 to confirm the replay-window test fails.

## match-timeout-s-not-in-approval [info] eidolon/crates/rune/src/host.rs:819
A rule approves the command but not its kill time (timeout_s)
FIX: Document in perch/src/allow/README.md; no code change.

## routes-claim-stuck-after-failed-answer [low] perch\src\allow\mod.rs:162
A failed auto-answer leaves the ask claimed, so later POST /allow returns a false 409
FIX: In decide()'s spawned task, release the claim when the outcome is Failed or Stopped, mirroring make()'s cleanup (mod.rs:204-215): locked(&store).door_mut(key).map(|d| d.release(ask_id)). Keep the claim on Done and Lost. Add a test where the door refuses the decide() answer, then POST /allow for the same ask must succeed.

## routes-502-after-door-accepted [low] perch\src\allow\mod.rs:344
A door that acted on the answer but replied late gets a rule revoked and a 502 saying no rule was made
FIX: Split the outcome: a request that was written but got no clean reply should be Unknown, not Failed. For Unknown, check whether the ask is still in the view (view.ask(ask_id)) before revoking, and either keep the rule or return a status that says the answer may have gone through. The 502 text should not claim 'no rule was made' when the answer may have been delivered.

## routes-revoke-204-does-not-stop-matched-answer [low] perch\src\allow\mod.rs:147
A revoke that returns 204 can still be followed by one answer already matched
FIX: Inside the spawned task, re-check under the store lock that the rule still exists immediately before post_answer, which narrows the window to the write itself. Make the doc say exactly what 204 means: no new matches after it returns, and a POST already written cannot be recalled. The page should not describe a 204 as 'stopped' without that caveat.

## routes-lost-keeps-rule-and_hides-the-answer [low] perch\src\allow\mod.rs:238
answered:'lost' hides whether the person said yes or no, and the rule is kept either way
FIX: Decision for the operator: keep the rule on 'lost' (current, and what the brief's 409 rule says) or drop it when the person's answer was not yes. Either way, return the settled answer ('yes' or 'no', from the ask-settled frame's answer field) in the body, so A2 can render it. Today the page cannot tell 'you allowed it' from 'the person refused it, and a standing allow now exists'.

## routes-stale-closed-removes-newer-door [low] perch\src\allow\store.rs:168
A queued closed for an old door can delete a newer door's entry and rules when pid and port are both reused
FIX: Tag each watcher with a generation from open() (or a counter in DoorKey) and make end() remove only the entry whose generation matches the closed event. Correct the doc at store.rs:165-167.

## routes-uses-excludes-first-answer [info] perch\src\allow\mod.rs:203
uses stays 0 after the rule answers the ask it was made from
FIX: Either document in store.rs:47-49 that uses counts answers after the one the rule was made from, or count the first answer when it is Done. The page should label the field to match.

## routes-command-shown-raw [info] perch\src\allow\store.rs:65
Rule command and cwd go out verbatim, so the A2 approval view must show them escaped
FIX: A2 renders command and cwd as escaped text, with non-printing and bidi characters shown as \u escapes and the byte length shown beside the command. Do not render them as markup.

## routes-list-unbounded-size [info] perch\src\allow\mod.rs:394
GET /allow has no size bound: up to about 2 MiB in one response
FIX: A2 truncates long commands in the row and expands on demand. Optionally cap the command field in list() with a footer, as the mneme reply does.

## routes-rule-without-click [info] perch\src\serve.rs:279
Any script in the bundle's origin can make a standing rule without a human click
FIX: A2 calls POST /allow only from an explicit click on that ask's own row, never from a timer, a stream handler, or message text. Keep the bundle CSP as it is.

## routes-user-bash-tool-reachable [info] perch\src\allow\matcher.rs:35
POST /allow on a user tool named bash makes a command rule that answers builtin bash calls
FIX: None in this slice. A2 shows the tool name on every rule (the list already carries tool), and the risk stays on the operator's list.

## lifecycle-decide-claim-kept [low] perch/src/allow/mod.rs:161
Failed rule answer keeps the ask claimed, so a later allow on it gets 409
FIX: On Failed in decide's spawned task, release the claim under the store lock, as make() does at mod.rs:211-213. Alternatively, narrow the store.rs:114-117 doc and make the 409 text say that an earlier rule answer failed.

## lifecycle-uses-skip-own-answer [low] perch/src/allow/mod.rs:203
A rule made by POST never counts the ask it answered
FIX: In make()'s spawned task, call locked(&store).used(key, rid, now_ms()) on Answered::Done before returning. Change tests/allow.rs:363 to expect uses 1 after the first ask and 2 after the second.

## lifecycle-answer-after-door-stop [low] perch/src/allow/mod.rs:161
An answer that passed the liveness check can reach a door being stopped
FIX: Re-check the door just before post_answer: re-run door_of_key (state Live, same port) inside the spawned task, or have Registry::stop cancel a per-door token that answer() selects on before watcher.close(). Keep the claim, so a stopped door's ask goes back to the person.

## lifecycle-doorkey-late-closed [info] perch/src/allow/store.rs:179
A late closed or release for a reused DoorKey can touch the next door's entry
FIX: Stamp each watch::open with a generation (a u64 from a shared counter) and pass it to bind, end, release and revoke, so a late closed or release touches only the entry it belongs to. Or have end() ignore a call whose generation is not the entry's.

## lifecycle-claim-cap-evicts [info] perch/src/allow/store.rs:107
Claim cap at 1024 silently forgets the smallest ids; no double answer found
FIX: No change required. Add a comment at store.rs:105-112 that eviction is safe because the pending check at mod.rs:144 and 185 is the real guard.

## CRITIC verdict=merge-after-fixes
Verdict: merge-after-fixes. No high findings, and the lenses' checked clauses hold where I re-read them: route gates and body shaping, base wiring at perch/src/serve.rs:639-641, the answer request against eidolon's post_answer (Host 127.0.0.1:port, Bearer token, {ask_id, answer:"yes", note}), approval answers ["yes","no"], the note passing Words::new, and the note reaching the model via with_operator_note (dispatch.rs:519-526). Ran here, offline and locked: cargo check --tests --features test-bins (exit 0, 0 warnings), cargo clippy --lib --tests (0 warnings), cargo test allow::matcher (17 passed), cargo test allow::store (9 passed). Worktree still at 062b9a1, git status clean. Not run: tests/allow.rs and the mod.rs unit tests (they spawn processes and bind loopback ports; brief limits me to 48122), and the 48122 live check. Real exceptions to the lenses' "checked" list: the replay-window hole (matches the inventory's claim it does not exist), the uses undercount, the failed-decide claim leak, the SIGTERM exit bound the spec states but the test does not assert, and A2 reading A1b's status codes too coarsely. The XS disposition for command rules is code execution for runners like cargo test and should be re-confirmed by the operator. (A cargo test for worktree wf_2902c6f9-c37-1 was already running in the same WSL; I did not touch it.) (o^_^o)
- [perch/tests/allow.rs (25 integration tests, run needs --features test-bins) and the A1b live_check on PORT=48122] [medium, verification] Nothing in the review inputs shows the integration suite or the live check ever ran. This review ran only the compile, clippy and pure matcher/store checks listed in the summary. tests/allow.rs and the mod.rs unit tests spawn perch and stand-in doors on loopback ports, which the brief does not allow me to run, so every route, lifecycle, restart, lagged-stream and stop claim there rests on reading. The executor's pass counts are absent. CHECK: Orchestrator: run cargo test --features test-bins --locked --offline --test allow with CARGO_TARGET_DIR in the WSL home (not /tmp), and the 48122 live check (fake perch, FAKE_ASKS, curl; the answer log must show RULE_NOTE). Require the executor's pass counts before merge.
- [perch/tests/allow.rs:799-840 (a_stop_signal_does_not_wait_for_a_door_that_is_not_answering)] [low, spec test not met] Spec: perch SIGTERM with an answer in flight exits in under 3 s. The test asserts only that the in-flight POST returns 503 within 3 s (line 831). The perch's own exit is bounded at 20 s after the frozen door is thawed (lines 833-835; the message says it did not stop until the door could take SIGTERM). So with a wedged door the perch does not exit under 3 s, and the test name ('does not wait') contradicts its last assertion. The door wait is probably the existing door stop in sessions.rs (not A1b code), but the spec bound is unmet and unasserted. CHECK: Either assert perch process exit under 3 s with a door that is alive but holding the answer, or amend the spec and rename the test to the bound actually met, and decide whether door-wait is in scope for A1b.
- [perch/src/allow/mod.rs:126-166 (decide) and perch/src/allow/store.rs:114-117 (release contract)] [low] decide() claims the ask (line 151) and spawns the answer (line 162). Only the Done arm calls used() (line 164). A Failed answer releases nothing, although store.rs:114-117 says release is for an answer that did not reach the door. Scenario: the door answers 422, or stalls past 5 s, on a rule-matched ask. The ask stays with the person, and a later POST /allow for it returns 409 'already being answered' (store.rs:205-206, mod.rs:197) while nothing is answering it. Fail-closed, but the message is false. No test drives decide() to a failed answer. CHECK: On Failed, lock the store and release(ask_id) in the spawned task, mirroring make()'s cleanup. Add a test: the door refuses a rule answer, then POST /allow for that ask returns the real outcome (502), not 409 TAKEN.
- [perch/src/allow/mod.rs:161-165 (decide's uses) and make()'s answer task; perch/tests/allow.rs:343 and :363] [low] The rule's own first answer is never counted. Only decide() calls Store::used. A POST-made rule shows uses 0 after answering its ask: tests/allow.rs:343 pins 0, and :363 expects 1 only after a second answer. The store doc (store.rs:47-49, 'confirmed by the door') and the README ('count what the rule has answered since') both say it should count. A2 would display 'uses 0' on a rule that did answer. Display only; no allow decision changes. CHECK: Count Done in make()'s answer task too, and change tests 343 and 363 to expect 1 then 2; or change the docs and the A2 label to say the creating answer is not counted.
- [perch/src/watch.rs:1133 with eidolon crates/web/src/stream.rs:76-101 (reconnect replay window); perch/src/allow/mod.rs make() has no caught-up check] [low, spec intent broken in a narrow race] The door subscribes to asks (stream.rs:76) before the pending snapshot (:79) and yields asks buffered during the branch replay only after CaughtUp (:101). An ask K opened during the replay is read with reconnecting=false (watch.rs:1133) and gets a live seq. If the operator allows a listed ask H during that replay (no caught-up check exists anywhere in src/allow), the rule is stamped at the view's seq, and a same-command K matches it (matcher.rs:151-156), although K was pending on the door before the rule. The inventory claim 'an ask first seen in a reconnect replay is never answered' and the README Limits line are wrong for K. Harm is bounded: the bytes are the operator's own approved command. No test opens an ask mid-replay (tests/allow.rs:846-872 reconnects over asks already open). CHECK: Refuse make() while the view is not caught up; this closes the wide window. Full closure needs the door's CaughtUp to carry the next ask id, which is an eidolon change. Correct the inventory and README wording. Add a test where the stand-in opens an ask during replay.
- [perch/src/allow/mod.rs:234 (reply_of) against A1b.json routes] [info, spec drift] A1b.json promises 201 {rule, answered} for POST. The code returns 200 with the same body when an identical rule already exists (mod.rs:234; a unit test around mod.rs:493). README documents it; A1b.json does not, and A2's spec does not mention 200. CHECK: Reconcile A1b.json, and tell A2 that 200 means 'rule already there, ask answered' and is a success.
- [A2 spec (webui/term/panes/asks.js and core/state.js send) against perch/src/allow/mod.rs:175-243 and the README Routes table] [medium, cross-slice contract] A2 says 'A 409 keeps answered elsewhere'. A1b's 409 covers five cases: yolo chat, ask not pending, question, 32-rule cap, ask already being answered (mod.rs:175-197). The cap and yolo refusals would be labelled 'answered elsewhere', which is false. A2 also has no handling for 200 (above), 404 (no live door), 502 (door refused: rule revoked, ask still the person's; mod.rs:238-243) or 503 (perch stopping). CHECK: Copy the full status table from perch/src/allow/README.md into A2's spec and allow.test.mjs before A2 is built. Drive the 409 label from the one-sentence body, not the status alone.
- [A1b.json 'what', 'risk' and the (D10 b) test, versus BRIEF D10 and perch/src/allow/store.rs:1, mod.rs:35-37, tests/allow.rs:746] [info, stale spec] A1b.json still asks for a 0600 <home>/allow.json and a corrupt-file test. The operator ruled memory only, and the code follows it (no file is written; tests/allow.rs:746 checks this). A later slice reading the spec would expect a file that does not exist. CHECK: Amend A1b.json to D10 (a) before A2 or the page relies on it.
- [perch/src/watch.rs:473-478 (joined origin) and eidolon crates/rune/src/host.rs:529-541 (dispatch_as accepts any non-empty caller id), as cited by the lens] [low] A workflow passes a caller-chosen call id equal to an in-flight model call's id. If the nested tool-call-started frame is lost or read after the ask, the nested ask joins the model's origin, and a model-origin rule for the same command answers it (matcher.rs:155). Existing tests (watch.rs:1627-1670) cover only the order where both frames are read first. CHECK: Namespace caller-supplied ids in dispatch_as, or make joined() require the recorded call's tool to equal the ask's tool. Add reordered-frame and lost-frame tests.
- [perch/src/allow/matcher.rs:163-169 (command bytes only), A1b.json 'risk', and A2's 'allow "<command>" for this chat' row] [medium, operator decision not yet re-examined] The spec files 'an exact command is not an exact effect' as XS. For cargo test, make test, npm test, ./build.sh or python script.py, a model that can edit build.rs, a Makefile, package.json scripts or the script itself gets its next identical call answered yes with no prompt. That is code execution the operator never saw in the ask. The README warns (Limits), but A2 offers the button for any command with no warning in the row. CHECK: Re-confirm the XS disposition with the operator before merge, or limit command rules for runners that execute workspace code. Put the warning in A2's row text, not only in the README.
- [A2 mode rule (mode != yolo) against BRIEF D6 (manual: all term tools ask)] [info, unstated] Rules answer in manual mode (A1b.json: 'auto or manual'). A manual operator who clicks 'allow every term_send for this chat' turns D6's ask-everything manual mode into an auto-answer for that chat. The click is explicit, so this may be intended, but nothing states it. CHECK: Confirm with the operator that rules in manual mode are wanted. If yes, say so in A2's row text.
- [perch/src/allow/store.rs:179-181 (end by DoorKey) and perch/src/watch.rs:612-638 (closed delivered after the queue), as cited by the lens] [info] If a door's closed is delivered late and the OS reuses both its pid and port for a new door, the late closed removes the new door's entry and rules, and POST /allow then answers 409 'not being watched'. Fail-closed. The store.rs:165-167 doc says this cannot happen, and no test covers it. CHECK: Add a generation to the watch and to end()/bind(), or document the limit at store.rs:165-167.

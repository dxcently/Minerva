# A1b executor's security inventory (model output: claims to check, not facts)

Commit 062b9a1 on worktree-agent-ab08420a1c15687be, base c81db09 (A1a's watcher is on that base). Files: perch/src/allow/{mod.rs,
matcher.rs, store.rs, README.md}, perch/tests/allow.rs. No hook needed: serve::door_up already passes ctx.allow as every door's sink.

## Operator decisions
D10 (a) MEMORY ONLY: rules live in memory under the DoorKey alive when each was made; stop, model switch, mode switch, fork, resume
and perch restart all give a door with none. No allow.json. Narrowings: no name rule from a structural ask; a rule answers only its
own origin. D6/D9 for context (modes, guard rows).

## Routes and refusals
- GET /hub/sessions/<id>/allow -> 200 {mode, rules:[{rid, tool, origin, added_ms, uses, command?, cwd?, last_ms?}]}
- POST /hub/sessions/<id>/allow {ask_id} -> 201 {rule, answered:"yes"|"lost"}; 200 same body if the same rule already exists.
  The rule is built from the watcher's own copy of that ask (tool, input, origin); nothing but ask_id is read from the body. The
  body must be a JSON OBJECT (a `number_in` helper; serde had accepted `[1]`).
- POST /hub/sessions/<id>/revoke {rid} -> 204; unknown rid 404.
- 404 empty: no live door (kept, stopping, unknown, bad id), decided before the body is read. 400: not an object with an unsigned
  number. 413: body over 4 KiB. 409: yolo chat; ask not pending (settled, never read, over the 64 KiB line bound, watcher ended); a
  question; 32 rules already; ask already being answered. 422: structural ask by name; background call; unknown origin; unreadable
  tool or command; a bash call with a field beyond command, timeout_s, background. 502: the door did not take the answer (a rule
  newly made for that ask is revoked). 503: the perch is stopping. No refusal makes a rule or logs an answer.

## Matcher (pure)
Matches only when all hold: kind approval; chat mode auto or manual and EQUAL to the mode at creation (hello.yolo overrides the
registry mode); ask.seen_seq > rule stamp (0 never matches); live origin re-read via AskView::ask(id) and equal to the rule's;
origin None never matches; tool name byte-exact; bash: input.command a String byte-equal after JSON decode, background absent or
false (string "true" fails), timeout_s ignored, only those three fields allowed, door's hello cwd byte-equal to rule.cwd (raw bytes,
no normalisation); other tools: ask.structural == false. Checks is_closed() and the DoorKey before answering.

## Answerer
Spawned POST to the door's /api/answer {ask_id, answer:"yes", note: RULE_NOTE}; RULE_NOTE = "allowed by the operator's rule for
this chat (Minerva)". Selects on ctx.stop first (biased). 204 done; 409 = the person won (`lost`, rule kept). Claim set capped at 1024
ids per door. Registry and stop token captured lazily by the first POST /allow (OnceLock). decide runs on the watcher's blocking
threads. Door token read from the registry per answer, never in a log, error or Debug; the perch log has ids, rid and tool only,
never the command.

## Risks it names (for XS)
An exact command is not an exact effect (./build.sh, cargo test run files the model can edit); a user tool named `bash` with only
bash's fields inherits bash rules; the model sees the note; revoke does not recall an answer already on its way; an ask first seen in
a reconnect replay (seen_seq 0) is never answered by a rule.

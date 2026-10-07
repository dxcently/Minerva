# src/allow/ — allow for the rest of this chat

Rules the operator makes from a pending ask ("allow this, here, from now on"), and the answerer that applies them without a page. A rule is
kept in the perch's memory, for one chat's current door, and ends with that door (D10 a). Nothing is written to disk.

```text
mod.rs      State (the rules; also the watch::Sink), the answerer (POST /api/answer to the door), and the three routes: list, create, revoke
matcher.rs  the pure matcher: what a rule is made from (pattern_of), and rules + ask + door -> the rule that answers it, or none (find)
store.rs    the rules and the doors' entries, in memory: Rule, Door, Store; the 32-rule cap and the claim set (one answer per ask)
```

The rest of the perch is touched at two places only: `serve.rs` hands the three routes over before the body is read (S0P-a), and
`serve::door_up` passes `ctx.allow.clone()` as every door's `watch::Sink`. `State` is `Default` because `lib.rs` creates it before any hook
runs; it learns the registry and the perch's stop token from the first `POST /allow` (no rule can exist before one).

## Routes

```text
GET  /hub/sessions/<id>/allow                 200 {mode, rules: [rule]}      mode is "auto" | "manual" | "yolo"
POST /hub/sessions/<id>/allow {ask_id}        201 {rule, answered}           a new rule, and the ask it was made from, answered "yes"
                                              200 {rule, answered}           the same rule was there already; this ask is answered
POST /hub/sessions/<id>/revoke {rid}          204                            the rule answers nothing from now on
```

`answered` is `"yes"` when the door took the answer and `"lost"` when the door said the ask was settled already (the person answered first,
and the rule is kept: it was asked for). A **rule** is `{rid, tool, origin, added_ms, uses, command?, cwd?, last_ms?}`; `rid` is a number,
unique in the perch's life; `command` and `cwd` are there for a bash rule; `uses` and `last_ms` count what the rule has answered since.

Every refusal below is a status and a one-sentence `{error}` body (`application/json`), except the ones marked empty. Each makes no rule and
answers nothing.

```text
GET|POST allow, POST revoke
  404 empty   the chat has no live door (kept, stopping, unknown, a bad id), decided before any body is read; a rid that is not there
  405 empty   any other method (serve.rs)             401 / 403 empty   the gate: no token, another site, another Host (serve.rs)
POST allow
  400         the body is not a JSON object holding an unsigned number under "ask_id" (a list, a string, -1, 1.5, null)
  413 empty   a body over 4 KiB, refused unread
  409         a yolo chat (the door answers every ask itself)    |  the ask is not pending: settled, never read, over the line bound
              (a line over 64 KiB goes by unread), or its door is gone  |  the ask is a question, not an approval
              this chat has 32 rules already  |  the ask is already being answered (by a rule, or by this route a moment ago)
  422         the ask has no rule that could be made from it:
                a structural ask (eidolon asks about the call's shape, not its name)   a background call
                an unknown origin (the call's start was never seen, or the frames disagreed)
                a tool call with no readable tool or command   a bash call with a field beyond command, timeout_s, background
  502         the door did not take the answer (it refused, it was unreachable, it said nothing for 5 s): the ask is the person's again,
              and a rule made for it is not kept
  503 empty   the perch is stopping; the in-flight answer is dropped, not waited for
POST revoke
  400         the body is not a JSON object holding an unsigned number under "rid"
```

## What a rule is, and when it answers

A rule is made from the **watcher's copy of the ask** (`watch::AskView`), never from the request: the body is read for `ask_id` and for
nothing else, so a body that names another tool, command, origin or folder shapes nothing. The rule holds the tool's name, the origin
(`model`, `script` or `user`), the door's mode when it was made, and for bash the exact command string and the door's cwd.

```text
a rule answers an ask when ALL of these hold, read again at the moment of deciding (matcher::find, one clause each, each a unit test)
  the door is the rule's door (DoorKey pid+port), not closed; its mode is auto or manual and is the mode the rule was made in
  the ask is an approval, was first seen by the watcher after the rule was made (seen_seq != 0 and > the rule's stamp), and its id is
    above the highest id the door had shown (a replayed older ask is never the rule's)
  its origin, joined live from the call's start frame, is known and is the rule's origin
  the tool's name is the rule's, byte for byte
  bash:  the same command string (byte for byte), the same cwd, foreground, and no field but command, timeout_s, background
  other: the ask is not structural (its shape is not what eidolon asked about)
```

The answer is one `POST /api/answer {ask_id, answer: "yes", note: RULE_NOTE}` to the door, under the door's own token (read from the
registry; never logged, never in an error), inside 5 s, and it selects on the perch's stop. An ask is **claimed** before it is answered, so
two paths (the sink and the route, or a replay) never answer it twice. `uses` counts after the door took the answer.

`RULE_NOTE` is "allowed by the operator's rule for this chat (Minerva)". The door puts it in the `ask-settled` frame and the model reads
it as the reason for the answer; the page looks for it to say "allowed by this chat's rule".

## Lifetime (D10 a)

```text
made   POST /allow, on a pending ask                          kept   in memory, in Store.doors[DoorKey]
ends   the door's watcher ends (watch::Sink::closed): a stop, a model switch, a mode switch (both restart the door), the door dying,
       the perch stopping or restarting; also a revoke, and an answer the door did not take
never  a file: no <home>/allow.json, no rule in any other file (the source is scanned for a write in a unit test, and the perch's dirs
       are scanned after a restart in tests/allow.rs); a fork or a resume is a new door and starts with none
```

## Limits a reviewer should know

- An exact command is not an exact effect: the same bytes in another working tree state, or a command that reads what it did not before,
  do something else. A rule answers a command, not a consequence. The cap is 32 rules per chat.
- A user-defined tool named `bash` is a bash rule's tool. The field allow-list (command, timeout_s, background) is what keeps a tool with
  other fields from inheriting it.
- An ask the watcher first learns of in a reconnect's replay is stamped "pending before the watcher saw it" and is left to the person, so a
  rule does not answer asks that arrive while the stream is down. This is the safe side of a lag.
- A revoke stops the rule answering anything new; an answer already on its way is not recalled.
- The rule's cwd is the `cwd` in the door's `hello`; the web door never changes it.
- The note is visible to the model, so it must say nothing the operator would not tell it.
- The perch's log has ids, a rule number and a tool's name for an answer a rule gave. The command and the door's token are in no log.

## Tests

`tests/allow.rs` runs the routes over a real perch and the stand-in door (`FAKE_ASKS` scripts the asks, the stand-in's log shows each
answer and its note): allow then the next identical ask, revoke, near misses, body shaping, every refusal, the 32-rule cap, a second chat,
stop/resume and model and mode switches, a fork, a perch restart and the disk scan, a stop signal with an answer in flight, a lagged
stream, a line over the bound, and the person answering first. The clauses of the matcher, the store and the answerer are unit tests in
`matcher.rs`, `store.rs` and `mod.rs`.

"""jev/automation -- the graph runner: every section of
docs/design/automation.md. The schema (1); a node, its four option
sources, and the service-driver contract (2); guards evaluated for real
through an injected openjev `entail` (3); escalation-with-parking, the hop
budget, and -- for an escalate-suspended run -- surviving a service
restart (4); the decision log (5).

Guard evaluation through openjev and escalation-with-parking, once D2b's
open seams, both landed: `guards.py`'s NLI kinds call the injected
`entail`, and a low-confidence choice or an unhandled `EMPTY` suspends the
run (`run.py`'s `_park`) rather than stopping it, exactly as section 4
specifies. So does most of section 4's persistence: a run parked on an
`escalate` writes a snapshot before yielding and reconstructs for real
after a restart; only an `act`-suspended run stays unrecoverable, on
concrete evidence rather than by omission -- see `run.py`'s own module
docstring for the split and why it is not a partial build of the design's
fuller promise. `ask: "user"`, the escalation tier that answers through a
person via `choices_user` rather than back as the driving tool call's own
result, remains the one piece of section 4 not built here.

Nothing in this package executes an action. Every module here only ever
*decides* and *requests*; the request crosses into `jev/server.py`'s
`automation.start`/`automation.step` methods, and from there into
`extensions/jev/tools/run.rn`, which is the only thing that ever calls
`eidolon::dispatch`. See docs/Decisions.md, "The automation design is
accepted; `eidolon::dispatch` is its price". Persistence adds exactly one
new write path, `run.py`'s own `_persist_park`/`_persist_resolved`, and it
is not an exception to "this package never executes an action": both go
through `_paths.append_locked`, the same helper `automation/decisions.py`
already uses for its own log, and write only a JSON record of a run's own
state -- never a command, never anything a policy gate would need to see.
"""

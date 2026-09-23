"""run.py -- docs/design/automation.md section 2 (the interpreter loop, the
service-driver contract), section 3 (guard evaluation, batched, through the
injected `entail`) and section 4 (escalation-with-parking and the hop
budget).

**The one architectural rule.** Nothing below ever performs an action. The
interpreter is a Python generator that *yields* one of three request kinds
-- `{"kind": "act", ...}`, `{"kind": "escalate", ...}` or the exhaustion
that produces a `{"kind": "final", ...}` report -- and, for `act`, is *sent*
`{"text": ...}` / `{"error": ...}` results back; for `escalate`, it is sent
(`.send`) a validated pick or interrupted (`.throw`) with a stop. The only
thing that crosses into `extensions/jev/tools/run.rn` (and its `resume.rn`/
`stop.rn` siblings), which is the only thing that calls
`eidolon::dispatch`. See docs/Decisions.md, "The automation design is
accepted; `eidolon::dispatch` is its price".

**Escalation parks.** The full design (section 4): a choice under the
floor or margin, or a menu that came back empty with nothing to handle
`EMPTY`, suspends the run instead of ending it -- `_park` (below) yields
the escalation payload, exactly as `_run_tool_action` already yields an
`act` request, and the generator sits there until `Run.answer` sends a
validated pick or `Run.answer`/`Run.stop` interrupts it with a stop. This
is why `"escalated"` is no longer a terminal `outcome` anywhere in this
module: a run that used to stop and report now waits, and only the answer
it eventually gets decides what it becomes next -- a further step, a
further escalation, or `stopped`. What is *not* built here, on purpose,
because the design scopes it to the `ask: "user"` tier and this task's
brief does not ask for it: a driver-side `choices_user` path. `ask` is
still not read anywhere in this module -- section 4's `return` tier (the
one that needs no new plumbing, because the question travels back as the
driving tool call's own result) is what is implemented; `"user"` remains
future work, same as it was before this file parked anything.

**Escalate-parked runs survive a service restart; `act`-suspended ones do
not, and that split is deliberate, not a partial build of the design's
full promise.** Docs/Decisions.md, "Amendment: the persistence refusal
was right about half a program", is the record of why: `_park` has
exactly two call sites -- the floor/margin miss in `_choose_phase`, the
unhandled `EMPTY` in `_fire_event` -- and both are reached only through
`_choose_phase`, itself called from exactly one place, `_interpret`'s
main loop. A single, fixed, shallow position every time: nothing of
`_enter`/`_execute_transition`/`_run_actions` is ever mid-cascade there.
`_park` snapshots exactly what that position needs -- `context`,
`active`, `history`, `visits`, `counts`, `warnings`, `path_log`, `input`,
`ckpt_id`, and the escalation payload itself, all of it JSON already --
to `%LOCALAPPDATA%\eidolon\extensions\jev\runs\<run-id>.jsonl`, appended
through `_paths.append_locked`, the same shared, already-proven write
path `automation/decisions.py` uses for its own log and the only other
write this whole package makes (see that module's docstring for the
Windows measurement behind why the lock is load-bearing). A second,
smaller line marks the park *resolved* -- run id, graph ref, active,
counts, path_log, ckpt_id, no `context`/`history`/`escalation` -- the
instant it is answered or stopped, whichever happens first, so a *later*
crash further down the walk this resumed into is never mistaken, on the
next restart, for still being parked at a question that was already
answered: reading a run's file is always "take the last line".

A fresh process finding a run's last line still `"escalate"`
reconstructs it for real, not a promise that sometimes can't keep itself:
`_restore_run_state` rebuilds a `_RunState` from the snapshot;
`options.build_options` -- pure, given the restored context, the
reviewer's own finding -- rebuilds the one thing the trimmed
`{index, label, p}` escalation shape does not already carry
(`Option.data`/`.event`); `_interpret_from_park` resumes the walk from
exactly there, sharing `_run_loop` with `_interpret` itself so the live
and the reconstructed path can never quietly drift apart. `Run.answer`
needs no changes at all to cover this -- a pick is still validated
against the escalation's own `options` before the generator is ever
touched, because a reconstructed `Run` is the exact same class, `_lock`
included, just built from a snapshot instead of a running generator.

`act`-suspension stays refused, on the same evidence as before, not
merely un-revisited: `_run_tool_action`'s `act` yield can occur at
arbitrary depth through the recursive `_enter`/`_execute_transition`/
`_handle_final_entered` cascade, and concretely, `into =
params.get("into", "obs")` stays local to the frame and never appears in
the yielded request -- a restored process would not know where to put
the result even if the rest of the position were somehow recoverable. No
snapshot is written for it: `_park` is the only hook this module adds;
`_run_tool_action` gains none. Once a run *has* parked at least once,
though, its file exists, and a crash further downstream of an answered
park is distinguishable from an id that was never known at all: the
file's last line reads `"resolved"` rather than `"escalate"`, which is
enough to say what is still known (the step and state as of that last
answer) without pretending to know what happened after -- it may have
finished cleanly, or died again mid-`act`; this process cannot tell
which, and says so rather than guessing either way. `_load_orphan`
reports that as outcome `"orphaned"` with a `lost` field naming it, and
hands back an already-`finished` `Run` -- the exact same idempotent shape
`answer`/`resume`/`stop` already give any other finished run, so no
caller needs a special case for this one either.

An id with no file at all -- genuinely never known, or an `act`-suspended
run that never got the chance to park even once -- still gets
`_no_such_run_message`, unchanged and still true: that message was never
the lie to fix; only treating a *known*, now-orphaned id the same way
would have been. `step`/`answer`/`status`/`stop_run` all take the same
optional `graphs_dir`/`chooser`/`entail`/`option_tokens`/`context_tokens`
`start` already did, threaded through to `_load_orphan` only when a
lookup misses `_RUNS` -- omit them (every call in this module's own
pre-persistence tests does) and the behaviour is exactly what it always
was, a pure `_RUNS` lookup with no disk fallback at all. `automation.runs`
still only ever lists what this process has *itself* tracked since it
last started -- reconstruction is lazy, on the first
`step`/`answer`/`status`/`stop` that actually names a given id, not a
startup scan, so a run nobody has asked about yet still will not appear
there even though its file already exists; asking once is what adds it.

**Per-run locking.** Before parking, a run's entire lifecycle was one
synchronous call sequence inside a single `jev_run` tool call, so two
callers racing to advance the same run's generator was not a reachable
state. Now `jev_resume`/`jev_stop` are later, separate tool calls --
possibly from a different session -- so `Run` gets its own lock, held
across every generator touch (`begin`/`resume`/`answer`/`stop`), the same
belt-and-braces `jev/server.py` already applies to its own model loads.

**The guard simplification is gone.** `guards.py`'s `entails`/`contradicts`
kinds are now evaluated for real, batched per list via
`guards.first_passing` -- this module just wires the service's `entail`
callable through, the same injection shape `chooser` already used for
jevlike.

**Request idempotency.** `Run.resume` checks the request id it is given
against the one pending `act` request; an id that does not match (a
retried `step` after a dropped connection, a stale answer to a request
already consumed, or simply a run parked on an `escalate` request instead)
is ignored and the current pending request is handed back unchanged, per
docs/design/automation.md section 2's service-driver contract table.
`Run.answer` is symmetric: it only ever answers a currently-pending
`escalate` request, by design carrying no id of its own (the contract
table gives `automation.answer` none) -- called when nothing is parked, it
is the same no-op.
"""
from __future__ import annotations

import copy
import json
import os
import re
import threading
import time
import uuid
from pathlib import Path
from typing import Callable, Generator

import _paths

from . import a11y, decisions, guards, options, template
from . import graph as graph_mod
from . import warrant as warrant_mod

Chooser = Callable[[str, list[str]], list[float]]
# `(premise, hypotheses) -> [{"contradiction": p, "entailment": p, "neutral":
# p}, ...]` -- `guards.Entailer`'s own shape, restated here so this module's
# signatures do not have to import guards.py just to spell it out.
Entailer = Callable[[str, list], list]

def _resume_hint(rt: "_RunState", request: str | None = None) -> str:
    """docs/design/unattended.md section 5: the escalation payload's own
    `answer` hint, unchanged for an attended run; under a warrant, the
    `pick` form grows a `warrant` field so the text itself tells whoever
    (or whatever) is about to call `jev_resume` that this run expects the
    block echoed back, the same block `jev_warrant` already handed them.

    `request` -- the escalation's own stable request id (`next_request_id`,
    carried on the payload as `request`) -- is named in the hint too when
    the caller has one, so the text read off a parked run already contains
    the one field `jev_resume` needs to prove it is answering *this*
    question and not an older one (see `Run.answer`). Omitted when there is
    none, which keeps the legacy `{run, pick: ...}` shape for any caller
    that never learned about request ids."""
    fields = ["run"]
    if request is not None:
        fields.append(f"request: {request}")
    fields.append("pick: <index or label>")
    if rt.warrant is not None:
        fields.append("warrant: <this payload's warrant>")
    return "jev_resume {" + ", ".join(fields) + "} or {run, stop: <reason>}"


def _warrant_field(rt: "_RunState") -> dict | None:
    """The `warrant` key every escalation payload, final report and status
    record carries: `None` for an attended run, else the graph's canonical
    block with the id folded in (`{"id": rt.warrant_id, **rt.warrant}`) --
    one object a caller can both display and pass straight back as
    `jev_resume`'s own `warrant` argument, rather than two fields it would
    have to recombine itself."""
    if rt.warrant is None:
        return None
    return {"id": rt.warrant_id, **rt.warrant}


# --- control-flow signals -----------------------------------------------------
# Exceptions used purely for flow control inside the generator below; none
# of them ever escapes `_interpret` itself -- the outer try/except in
# `_interpret` turns each into an outcome on `_RunState`.


class _ToolError(Exception):
    """A `tool` action's result failed one of the three beliefs an
    observation must earn before it is stored, guarded over or scored --
    section 4: whole (the producer's declared length matches what arrived),
    succeeded (the tool's own `is_error` did not fire), of-kind (the
    action's `expect` guard, when it has one, passed) -- or a `PICK` named
    a ref outside the current snapshot. `reason` is one of `failed`
    (`tool`'s own error), `truncated` (whole belief failed), `unexpected`
    (of-kind belief failed) or `stale` (a ref no longer current); `tool`
    names what was being asked of (`name` for a `tool` action, `"pick"`
    for a stale ref or an unrecoverable orphan reconstruction). Caught once
    per loop iteration (and once around the bootstrap entry) and turned
    into an `ERROR` event dispatch carrying all three, per section 1's
    "Reserved events"."""

    def __init__(self, text: str, *, reason: str, tool: str):
        self.text = text
        self.reason = reason
        self.tool = tool
        super().__init__(text)


class _BudgetExhausted(Exception):
    def __init__(self, name: str):
        self.name = name
        super().__init__(name)


class _RunReached(Exception):
    def __init__(self, state_name: str):
        self.state_name = state_name
        super().__init__(state_name)


class _RunStopped(Exception):
    """Raised *into* the generator (`.throw`) at whatever point it is
    currently suspended -- an `act` yield inside `_run_tool_action`, or an
    `escalate` yield inside `_park` -- by `Run.stop`, and by `Run.answer`
    when the answer is a `stop` rather than a `pick`. Caught once, in
    `_interpret`'s outer try, same tier as `_RunReached`/`_BudgetExhausted`."""

    def __init__(self, reason: str):
        self.reason = reason
        super().__init__(reason)


# --- the run's mutable state ---------------------------------------------------


class _RunState:
    """The interpreter's working memory -- context, active configuration,
    history, counters. `Run` (below) is the public wrapper `server.py`
    talks to; this class never crosses that boundary."""

    def __init__(
        self,
        run_id: str,
        graph: graph_mod.Graph,
        run_input: dict,
        chooser: Chooser,
        entail: Entailer | None,
        ckpt_id: str | None,
        graph_ref=None,
        warrant: dict | None = None,
        warrant_id: str | None = None,
    ):
        self.id = run_id
        self.graph = graph
        # Whatever `automation.start` was originally given -- a graph id
        # (string) or an inline graph object (dict) -- kept verbatim so a
        # park snapshot can reload the *same* graph after a restart via
        # `load_graph_ref`, the same call `start()` itself made. `graph`
        # above is already-loaded/validated and cannot be reserialised
        # back into something `load_graph_ref` accepts; this can. `None`
        # for a run this process itself started before persistence cared
        # (never happens today -- `start()` always supplies it -- but
        # `_RunState.__init__` has exactly one caller either way, so this
        # default costs nothing and closes off a silent `AttributeError`
        # if that ever stops being true).
        self.graph_ref = graph_ref
        # The canonical block this run was started under, and its id --
        # both `None` for an attended run. Set once, here, from whatever
        # `start()` validated through `warrant_mod.check` before this
        # `_RunState` was ever constructed (or restored verbatim by
        # `_restore_run_state`, which recomputes `warrant_id` rather than
        # trusting a persisted copy of it); nothing past this point ever
        # assigns either field again -- a run's warrant, like its graph, is
        # fixed for its whole life.
        self.warrant: dict | None = warrant
        self.warrant_id: str | None = warrant_id
        self.context: dict = copy.deepcopy(graph.context)
        self.input: dict = run_input
        self.event: dict = {}
        self.active: tuple[str, ...] = ()
        self.history: dict[tuple[str, ...], str] = {}
        self.visits: dict[tuple[str, ...], int] = {}
        self.counts = {
            "steps": 0, "actions": 0, "escalations": 0, "chooser_calls": 0,
            "duplicates_collapsed": 0, "rule_picks": 0,
        }
        self.warnings: list[str] = []
        self.path_log: list[str] = []
        self.decisions: list[dict] = []
        self.transitions: list[dict] = []
        self.trace: list[dict] = []
        self.trace_sequence = 0
        self.orders: list[dict] = []  # automation.order's bounded history (see _ORDERS_HISTORY_LIMIT)
        self.outcome: str | None = None
        self.final_state: str | None = None
        self.output: dict | None = None
        self.error: str | None = None
        # Set only in the `outcome == "error"` paths, beside `self.error`:
        # one of `failed | truncated | unexpected | stale` (`_ToolError`'s
        # own `reason`), or `None` for the one `error` outcome that is not a
        # `_ToolError` at all (`_run_loop`'s "no always guard passed and
        # there is no choose block"). `_fire_event`'s `ERROR` arm sets it
        # from `event_data`; `_final_report` surfaces it as `report["reason"]`,
        # the same key the `stopped` outcome already uses -- safe, since the
        # two outcomes are exclusive.
        self.error_reason: str | None = None
        # Set only by `_run_from_lost_record` -- section 4's `orphaned`,
        # the `act`-suspended half: what an answered-but-then-silent run's
        # last snapshot still knows, once its walk could not be
        # reconstructed past that point. `None` on every run that is
        # live, finished normally, or reconstructed as a genuine
        # escalate-resume -- only ever read by `_final_report`/
        # `_status_dict`'s own `outcome == "orphaned"` branches.
        self.lost: str | None = None
        self.exhausted: str | None = None
        self.stop_reason: str | None = None
        self.escalation: dict | None = None
        self.chooser = chooser
        self.entail = entail
        self.ckpt_id = ckpt_id
        self.start_mono = time.monotonic()
        self.start_wall = time.time()
        self._req_seq = 0

    def next_request_id(self) -> str:
        self._req_seq += 1
        return f"{self.id}-req-{self._req_seq}"

    def scope(self) -> dict:
        """The template/guard scope: the four roots `schema.json`'s `path`
        pattern allows (`context|input|event|run`). `state.*` and
        `option.*` are added by callers that need them (`options.py`),
        never here, since they are valid only in specific templates."""
        return {
            "context": self.context,
            "input": self.input,
            "event": self.event,
            "run": {"id": self.id, "step": self.counts["steps"]},
        }

    def default_threshold(self) -> float:
        return self.graph.default("threshold", 0.6)


def _graph_ref(graph: graph_mod.Graph) -> str:
    return f"{graph.id}@{graph.version}" if graph.version else graph.id


def _common_prefix_len(a: tuple, b: tuple) -> int:
    n = 0
    while n < len(a) and n < len(b) and a[n] == b[n]:
        n += 1
    return n


def _render_deep(value, scope: dict, warnings: list[str]):
    """Used for every JSON-shaped template site run.py owns -- a `tool`
    action's `input`, `assign`'s and `push`'s values, a final state's
    `output`. Each string leaf goes through `render_value`, not `render`:
    a leaf that is exactly one `{{path}}` keeps its real type (a list stays
    a list -- `arrived`'s `output: {"path": "{{context.path}}"}` must reach
    the caller as JSON array, not a newline-joined blob), and a leaf with
    any literal text around the placeholder(s) still stringifies, same as
    `render` -- `render_value` already falls back to that for anything but
    the whole-string-is-one-placeholder case, so this loses nothing tool
    inputs like `browser_open`'s `.../wiki/{{context.start}}` rely on."""
    if isinstance(value, str):
        return template.render_value(value, scope, warnings)
    if isinstance(value, dict):
        return {k: _render_deep(v, scope, warnings) for k, v in value.items()}
    if isinstance(value, list):
        return [_render_deep(v, scope, warnings) for v in value]
    return value


# --- dotted-path context mutation (push/inc/assign) ----------------------------


def _get_at(context: dict, dotted_path: str):
    node = context
    for segment in dotted_path.split("."):
        if not isinstance(node, dict) or segment not in node:
            return None
        node = node[segment]
    return node


def _set_at(context: dict, dotted_path: str, value) -> None:
    segments = dotted_path.split(".")
    node = context
    for segment in segments[:-1]:
        node = node.setdefault(segment, {})
    node[segments[-1]] = value


def _ensure_list_at(context: dict, dotted_path: str) -> list:
    segments = dotted_path.split(".")
    node = context
    for segment in segments[:-1]:
        node = node.setdefault(segment, {})
    last = segments[-1]
    if not isinstance(node.get(last), list):
        node[last] = list(node.get(last) or [])
    return node[last]


# --- running actions (entry/exit/transition.actions) ----------------------------


def _run_actions(rt: _RunState, actions: list[dict]) -> Generator[dict, dict, None]:
    for action in actions:
        kind = action["type"]
        params = action.get("params") or {}
        if kind == "tool":
            yield from _run_tool_action(rt, params)
        elif kind == "capture":
            _run_capture_action(rt, params)
        elif kind == "assign":
            for key, value in params.items():
                rt.context[key] = _render_deep(value, rt.scope(), rt.warnings)
        elif kind == "push":
            value = _render_deep(params["value"], rt.scope(), rt.warnings)
            _ensure_list_at(rt.context, params["path"]).append(value)
        elif kind == "inc":
            current = _get_at(rt.context, params["path"]) or 0
            _set_at(rt.context, params["path"], current + params.get("by", 1))


def _run_capture_action(rt: _RunState, params: dict) -> None:
    """docs/design/triage.md section 3.2, "Evidence is typed at the point it
    is extracted": `re.search` with `MULTILINE` -- the engine and flags
    `guards.matches` already uses, so this is `matches` with groups rather
    than a second expression language -- writing each *participating* named
    group to `context.<into>.<group>`.

    Three deliberate details, each a rule from that section:

    - a group that did not participate assigns **nothing**. An optional
      field's absence is a finding, not a failure, and must never overwrite
      what an earlier capture or the graph's own initial `context` already
      put at that key (so `context.case` accumulates rather than resets);
    - the pattern as a whole not matching raises the *same* `_ToolError` a
      failed `expect` does -- `reason="unexpected"`, `tool="capture"` --
      because "none of my patterns describe this observation" is the same
      belief one step later, not a fourth reason;
    - `path` may be any scope path, including `event.option.line` after a
      `lines` pick: the chooser selects which line, the author's pattern
      extracts which field.

    `path` is read with `template.resolve_path` (a literal path, not a
    template -- lint restricts it to `PATH_ROOTS`) and stringified the way
    `matches` stringifies its own left-hand side, so the two actions agree
    about what they are matching against. A path that resolves to nothing
    yields `""`, which no lint-admissible pattern can match, so it surfaces
    as the not-matching error below, naming the path."""
    path = params["path"]
    pattern = params["pattern"]
    into = params["into"]
    value = template.resolve_path(path, rt.scope())
    text = value if isinstance(value, str) else template.stringify(value)
    match = re.search(pattern, text, re.MULTILINE)
    if match is None:
        # The pattern is named *plainly*, not through `!r`: a regex is
        # mostly backslashes, and `repr` doubles every one of them, so the
        # one diagnostic an author has to read would be the least legible
        # form of the very thing they need to fix.
        raise _ToolError(
            f"capture at {path!r} matched nothing with pattern '{pattern}': not the kind of "
            "result this action asked for",
            reason="unexpected", tool="capture",
        )
    target = rt.context.get(into)
    if not isinstance(target, dict):
        target = {}
        rt.context[into] = target
    for group, captured in match.groupdict().items():
        if captured is not None:
            target[group] = captured


def _trace(rt, kind, **data):
    rt.trace_sequence += 1
    rt.trace.append({"sequence": rt.trace_sequence, "kind": kind, "state": ".".join(rt.active),
                     "elapsed_s": round(time.monotonic() - rt.start_mono, 3), **copy.deepcopy(data)})
    del rt.trace[:-500]


def _run_tool_action(rt: _RunState, params: dict) -> Generator[dict, dict, None]:
    if rt.counts["actions"] + 1 > rt.graph.budget("actions", 100):
        raise _BudgetExhausted("actions")
    name = params["name"]
    rendered_input = _render_deep(params.get("input") or {}, rt.scope(), rt.warnings)
    into = params.get("into", "obs")
    request = {
        "kind": "act",
        "id": rt.next_request_id(),
        "tool": name,
        "input": rendered_input,
        # Rides along for the driver's information; see this module's
        # docstring on why it is not independently enforced -- Rune has no
        # timer primitive to wrap a dispatch in one, so a tool that wants a
        # deadline (`bash`) takes it inside its own `input`, the way both
        # worked example graphs already do.
        "timeout_s": params.get("timeout_s"),
    }
    _trace(rt, "tool_call", request=request["id"], tool=name, input=rendered_input)
    result = yield request
    _trace(rt, "tool_result", request=request["id"], tool=name, result=result)
    rt.counts["actions"] += 1
    # The three beliefs an observation must earn before it is stored,
    # guarded over or scored (docs/design/observation.md section 4): an
    # invalid one is never any of those. Succeeded first -- a tool's own
    # `is_error` is the producer speaking directly, and nothing past this
    # point is worth checking against a result that already says it
    # failed. Whole second -- the producer's own declared `chars:` against
    # what actually arrived, replacing the old single-layer marker match
    # that only ever saw `crates/tools`'s own clip and missed every other
    # place a result could be shortened in transport. Of-kind last -- an
    # observation can be whole and successful and still not be the kind of
    # thing this action asked for (a `ps` usage error is a complete,
    # successful bash result; it is not a process listing).
    error = (result or {}).get("error")
    if error:
        raise _ToolError(str(error), reason="failed", tool=name)
    text = (result or {}).get("text") or ""
    declared = a11y.declared_length(text)
    if declared is not None:
        _head, body = a11y.parse_head(text)
        if len(body) != declared:
            raise _ToolError(
                f"observation from {name!r} was truncated in transport: {len(body)} of {declared} "
                "declared characters arrived",
                reason="truncated", tool=name,
            )
    candidate = a11y.build_obs(text)
    expect = params.get("expect")
    if expect is not None:
        probe = rt.scope()
        probe["context"] = {**rt.context, into: candidate}
        if not guards.evaluate_guard(
            expect, probe, rt.warnings, entail=rt.entail, default_threshold=rt.default_threshold()
        ):
            raise _ToolError(
                f"observation from {name!r} did not satisfy expect ({expect['type']}): not the kind "
                "of result this action asked for",
                reason="unexpected", tool=name,
            )
    rt.context[into] = candidate


# --- entering and exiting states ------------------------------------------------


def _resolve_history_chain(rt: _RunState, path: tuple[str, ...]) -> tuple[str, ...]:
    """If `path` names a `history` state, resolve it to its parent's last
    active child (`rt.history`), or the history state's own `target`
    fallback if the parent was never entered -- section 1, "History"."""
    state = rt.graph.state_at(path)
    if state.get("type") != "history":
        return path
    parent = path[:-1]
    recorded = rt.history.get(parent)
    if recorded is not None:
        resolved = parent + (recorded,)
    else:
        resolved = rt.graph.resolve_target(path, state["target"])
        if resolved is None:
            raise RuntimeError(
                f"history state {'.'.join(path)!r} has an unresolvable fallback target "
                f"{state['target']!r} (should have failed lint)"
            )
    return _resolve_history_chain(rt, resolved)


def _exit_to(rt: _RunState, keep_len: int) -> Generator[dict, dict, None]:
    """Pop `rt.active` down to length `keep_len`, deepest state first,
    running each popped state's `exit` actions and recording shallow
    history for any compound ancestor being left."""
    while len(rt.active) > keep_len:
        leaving = rt.active
        state = rt.graph.state_at(leaving)
        yield from _run_actions(rt, state.get("exit") or [])
        parent = leaving[:-1]
        if len(parent) >= keep_len:
            rt.history[parent] = leaving[-1]
        rt.active = parent


def _handle_final_entered(rt: _RunState, seg: tuple[str, ...]) -> Generator[dict, dict, bool]:
    """`seg` is a `final` state that was just entered. A top-level one ends
    the run (`_RunReached`); a nested one fires its parent's `onDone`, if
    any -- "phase complete" without an event name, section 1. Returns
    whether an `onDone` transition navigated the run elsewhere, so the
    caller (`_enter`) knows its own cascade-into-`.initial` epilogue no
    longer applies."""
    state = rt.graph.state_at(seg)
    if state.get("output") is not None:
        rt.output = _render_deep(state["output"], rt.scope(), rt.warnings)
    if len(seg) == 1:
        raise _RunReached(seg[0])
    parent = seg[:-1]
    on_done = rt.graph.state_at(parent).get("onDone")
    if not on_done:
        return False
    transitions = graph_mod.as_transition_list(on_done)
    idx = guards.first_passing(
        transitions, rt.scope(), rt.warnings, entail=rt.entail, default_threshold=rt.default_threshold()
    )
    if idx is None:
        return False
    yield from _execute_transition(rt, parent, transitions[idx])
    return True


def _enter(rt: _RunState, from_path: tuple[str, ...], target_path: tuple[str, ...]) -> Generator[dict, dict, None]:
    """Enter every state from `len(from_path)` down to `target_path`'s
    leaf, running each newly-entered state's `entry` actions in order
    (outer to inner), then cascade further via `.initial` if the leaf is
    itself compound. `target_path`'s leaf is resolved through
    `_resolve_history_chain` first, since a `history` target can redirect
    it entirely."""
    path = _resolve_history_chain(rt, target_path)
    for depth in range(len(from_path) + 1, len(path) + 1):
        seg = path[:depth]
        rt.active = seg
        rt.path_log.append(seg[-1])
        _trace(rt, "node_enter")
        rt.visits[seg] = rt.visits.get(seg, 0) + 1
        state = rt.graph.state_at(seg)
        max_visits = (state.get("meta") or {}).get("visits", rt.graph.default("visits", 20))
        if rt.visits[seg] > max_visits:
            raise _BudgetExhausted("visits")
        yield from _run_actions(rt, state.get("entry") or [])
        if state.get("type") == "final":
            navigated = yield from _handle_final_entered(rt, seg)
            if navigated:
                return
    leaf_state = rt.graph.state_at(rt.active)
    if rt.active == path and isinstance(leaf_state.get("states"), dict) and leaf_state.get("initial"):
        yield from _enter(rt, rt.active, rt.active + (leaf_state["initial"],))


def _execute_transition(rt: _RunState, owning_path: tuple[str, ...], transition: dict) -> Generator[dict, dict, None]:
    """Run one transition: exit, the transition's own actions, then enter
    -- section 1, "Order" and "Re-entry". `target` absent is internal: only
    the actions run, `rt.active` does not move. `owning_path` is the state
    the transition was *declared on* (used to resolve a bare, relative
    `target`); the exit/enter cascade itself is always relative to the
    *actual* active configuration, `rt.active`, which can differ from
    `owning_path` when the transition was found on an ancestor."""
    target = transition.get("target")
    actions = transition.get("actions") or []
    if target is None:
        yield from _run_actions(rt, actions)
        return
    new_path = rt.graph.resolve_target(owning_path, target)
    if new_path is None:
        raise RuntimeError(
            f"unresolved target {target!r} from {'.'.join(owning_path) or '<root>'!r} "
            "(should have failed lint)"
        )
    _trace(rt, "transition", source=".".join(owning_path), target=".".join(new_path), transition=transition)
    rt.transitions.append({"source": ".".join(owning_path), "target": ".".join(new_path), "step": rt.counts["steps"]})
    del rt.transitions[:-500]
    reenter = bool(transition.get("reenter", False))
    natural_lca = _common_prefix_len(rt.active, new_path)
    lca = min(natural_lca, len(new_path) - 1) if reenter else natural_lca
    yield from _exit_to(rt, lca)
    yield from _run_actions(rt, actions)
    yield from _enter(rt, rt.active, new_path)


# --- events: PICK / EMPTY / ERROR / a menu item's own / an onDone-free target ----


def _find_transition(
    graph: graph_mod.Graph,
    from_path: tuple[str, ...],
    event: str,
    scope: dict,
    warnings: list[str] | None,
    entail: Entailer | None,
    default_threshold: float,
) -> tuple[tuple[str, ...], dict] | None:
    """Section 1, "Order": "An event's transitions are looked up in the
    active state first, then each ancestor, then the root `on`." Within
    one state's list for `event`, `guards.first_passing` gives document
    order, first-pass-wins, batched section-3 evaluation; the walk up the
    hierarchy between levels stays sequential -- each level is tried in
    full before falling back to the next, never merged into one batch with
    it. Returns `(owning_path, transition)` for the winner."""
    path = from_path
    while True:
        on = graph.root_on if not path else (graph.state_at(path).get("on") or {})
        entry = on.get(event)
        if entry is not None:
            transitions = graph_mod.as_transition_list(entry)
            idx = guards.first_passing(transitions, scope, warnings, entail=entail, default_threshold=default_threshold)
            if idx is not None:
                return path, transitions[idx]
        if not path:
            return None
        path = path[:-1]


def _fire_event(rt: _RunState, from_path: tuple[str, ...], event: str, event_data: dict) -> Generator[dict, dict, str]:
    """Returns `"continue"` (the transition ran, keep looping) or
    `"ended_error"` (an unhandled `ERROR`, or any other event nothing
    handles). An unhandled `EMPTY` no longer returns a status at all: it
    parks (`_park`), and what happens next is whatever answering that
    escalation leads to -- a normal return, once resumed, or `_RunStopped`,
    unwound by `_interpret`'s own outer try, exactly as `_choose_phase`'s
    floor/margin escalation already does."""
    rt.event = event_data or {}
    found = _find_transition(rt.graph, from_path, event, rt.scope(), rt.warnings, rt.entail, rt.default_threshold())
    if found is None:
        if event == "EMPTY":
            escalation = _empty_escalation(rt, from_path)
            yield from _park(rt, escalation)
            # `_empty_escalation`'s `options` is always `[]` (there is
            # nothing to click, run, or route to), so `Run.answer` can
            # never validate a `pick` against it -- `stop` is the only
            # answer that can complete this park, and that unwinds via
            # `_RunStopped`, caught well above this point, before control
            # ever returns here. Kept as a loud, explicit failure rather
            # than a silent fallthrough in case that invariant is ever
            # loosened without this comment being noticed.
            raise AssertionError("unreachable: an EMPTY escalation has no options a pick could validate against")
        if event == "ERROR":
            rt.error = event_data.get("error", "unhandled tool error")
            rt.error_reason = event_data.get("reason")
        else:
            rt.error = f"event {event!r} raised at {'.'.join(from_path) or '<root>'!r} but nothing handles it"
        return "ended_error"
    owning_path, transition = found
    yield from _execute_transition(rt, owning_path, transition)
    return "continue"


def _current_known_refs(rt: _RunState) -> set[str]:
    """Mirrors `extensions/browser/service.py`'s own `STATE.known_refs`:
    a ref is only valid against the snapshot that produced it. Re-derived
    from `context.obs` on demand rather than cached, since it is cheap and
    it is the one thing guaranteed to reflect whatever `context.obs`
    currently holds."""
    obs = rt.context.get("obs")
    if not isinstance(obs, dict) or obs.get("url") is None and obs.get("title") is None:
        return set()
    text = obs.get("text")
    if not isinstance(text, str):
        return set()
    records, _skipped = a11y.parse_refs(text)
    return {r["ref"] for r in records if r.get("ref")}


# --- the always-check and choose phases ------------------------------------------


def _check_always(rt: _RunState, path: tuple[str, ...], state: dict) -> Generator[dict, dict, bool]:
    transitions = graph_mod.as_transition_list(state.get("always"))
    idx = guards.first_passing(
        transitions, rt.scope(), rt.warnings, entail=rt.entail, default_threshold=rt.default_threshold()
    )
    if idx is None:
        return False
    yield from _execute_transition(rt, path, transitions[idx])
    return True


def _budget_status(rt: _RunState) -> dict:
    return {
        "steps": f"{rt.counts['steps']}/{rt.graph.budget('steps', 50)}",
        "actions": f"{rt.counts['actions']}/{rt.graph.budget('actions', 100)}",
        "wall_s": f"{int(time.monotonic() - rt.start_mono)}/{rt.graph.budget('wall_s', 600)}",
        "escalations": f"{rt.counts['escalations']}/{rt.graph.budget('escalations', 10)}",
    }


def _evidence(rt: _RunState) -> dict:
    """Section 4's payload, "evidence": {goal, h1, url, excerpt} -- the
    same evidence a model and a person are looking at when deciding what
    to answer, sourced from the run's current context and its most recent
    observation (never re-fetched; `context.obs` is whatever the last
    `tool` action wrote there). Any piece this graph or this moment does
    not have -- no `goal` on a menu-driven graph like triage-linux, no
    snapshot yet, a tool whose output never carried a url/h1 -- is `None`,
    same as the rest of this payload's optional pieces (`question`)."""
    obs = rt.context.get("obs")
    if not isinstance(obs, dict):
        obs = {}
    text = obs.get("text")
    excerpt = text[:1500] if isinstance(text, str) else None
    return {
        "goal": rt.context.get("goal"),
        "h1": obs.get("h1"),
        "url": obs.get("url"),
        "excerpt": excerpt,
    }


def _empty_escalation(rt: _RunState, from_path: tuple[str, ...]) -> dict:
    state = rt.graph.state_at(from_path) if from_path else {}
    ask = (state.get("meta") or {}).get("ask")
    question = template.render(ask, rt.scope(), rt.warnings) if ask else None
    request = rt.next_request_id()
    return {
        "kind": "escalate",
        "request": request,
        "run": rt.id,
        "graph": _graph_ref(rt.graph),
        "state": ".".join(from_path) or rt.graph.id,
        "step": rt.counts["steps"],
        "why": "the menu was empty and nothing handled EMPTY",
        "question": question,
        "evidence": _evidence(rt),
        "options": [],
        "budget": _budget_status(rt),
        "warrant": _warrant_field(rt),
        "answer": _resume_hint(rt, request),
    }


def _floor_escalation(
    rt: _RunState, path: tuple[str, ...], state: dict, opts: list, probs: list[float], floor: float, margin: float
) -> dict:
    top1 = probs[max(range(len(probs)), key=lambda i: probs[i])]
    ordered = sorted(probs, reverse=True)
    top2 = ordered[1] if len(ordered) > 1 else 0.0
    if top1 < floor:
        why = f"top {top1:.2f} under floor {floor:.2f}"
    else:
        why = f"top {top1:.2f}, margin {top1 - top2:.2f} under {margin:.2f}"
    ask = (state.get("meta") or {}).get("ask")
    question = template.render(ask, rt.scope(), rt.warnings) if ask else None
    request = rt.next_request_id()
    return {
        "kind": "escalate",
        "request": request,
        "run": rt.id,
        "graph": _graph_ref(rt.graph),
        "state": ".".join(path),
        "step": rt.counts["steps"],
        "why": why,
        "question": question,
        "evidence": _evidence(rt),
        "options": [
            {"index": i, "label": opt.label, "p": round(p, 4)} for i, (opt, p) in enumerate(zip(opts, probs))
        ],
        "budget": _budget_status(rt),
        "warrant": _warrant_field(rt),
        "answer": _resume_hint(rt, request),
    }


# --- persistence: parked runs surviving a service restart (section 4) -----------
# The module's one new write hook, called only from `_park` (below) and
# `_resume_parked_choice` (above) -- never from `_run_tool_action`, on
# purpose; see the module docstring for the full split. Both go through
# `_paths.append_locked`, the same helper `automation/decisions.py` already
# uses for its own log, rather than a second locking implementation.


def _encode_path(path: tuple[str, ...]) -> str:
    """A dotted string a JSON object key (or a plain string field) can
    carry -- reversible because a state name can never itself contain
    `.` (`schema.json`'s own name pattern), so this can never collide two
    different paths onto the same string."""
    return ".".join(path)


def _decode_path(encoded: str) -> tuple[str, ...]:
    return tuple(encoded.split(".")) if encoded else ()


def _request_seq(escalation: dict | None) -> int | None:
    """The counter value behind a persisted escalation's own `request`
    (`next_request_id`'s format, `"<run-id>-req-<n>"`), or `None` if it has
    no usable one. The only place a snapshot written before `req_seq`
    existed can recover the sequence from, so an old park's file still
    resumes its ids monotonically instead of restarting them at 1."""
    request = (escalation or {}).get("request")
    if not isinstance(request, str) or "-req-" not in request:
        return None
    tail = request.rsplit("-req-", 1)[1]
    return int(tail) if tail.isdigit() else None


def runs_dir() -> Path:
    """Mirrors `automation/decisions.py`'s own `decisions_dir()` exactly:
    `JEV_RUNS_DIR` overrides the directory (what every test in this
    package's suite sets, the same idea as that module's own
    `JEV_DECISIONS_DIR` -- see its docstring); otherwise a sibling of
    `decisions/` under the same cache root, matching section 2's
    `.../extensions/jev/runs/<run-id>...` path. A *directory* of one file
    per run, not the flat single file `server.py`'s own ad-hoc `choose`
    log uses -- kept as its own environment variable, not reused from
    `decisions.py`, for the same reason that module's own docstring gives
    for not reusing `server.py`'s: these are a different write path for a
    different purpose, and a test that means to isolate one must not
    accidentally isolate the other instead."""
    override = os.environ.get("JEV_RUNS_DIR")
    if override:
        return Path(override)
    return _paths.cache_dir() / "eidolon" / "extensions" / "jev" / "runs"


def _run_file(run_id: str) -> Path:
    return runs_dir() / f"{run_id}.jsonl"


def _iso_now() -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime())


_runs_log_lock = threading.Lock()  # in-process half; _paths.append_locked is the cross-process half


def _write_run_record(run_id: str, record: dict) -> None:
    line = (json.dumps(record, ensure_ascii=False) + "\n").encode("utf-8")
    with _runs_log_lock:
        _paths.append_locked(_run_file(run_id), line)


# --- standing orders: their own audit log, not the run-recovery log ----------
# One file per run, written through `_paths.append_locked` like the log above.
# Deliberately not a new `"kind"` line in the run-recovery log: `_load_orphan`
# dispatches on the last record's own `kind`. `_RunState.orders` is the bounded
# history an operator reads back; the log is append-only and never pruned, so an
# id evicted from that history is still recognised (`_replay_order`).

_ORDERS_HISTORY_LIMIT = 100
# The two input bounds `automation.order` enforces before anything is
# journaled or mutated (see `Run.order`).
_ORDER_TEXT_MAX = 2000
_ORDER_ID_MAX = 128
_ORDER_PREFIX = "Operator standing order: "


def orders_dir() -> Path:
    """`JEV_ORDERS_DIR` overrides it (what this package's tests set);
    otherwise a sibling of `runs/` under the same cache root."""
    override = os.environ.get("JEV_ORDERS_DIR")
    if override:
        return Path(override)
    return _paths.cache_dir() / "eidolon" / "extensions" / "jev" / "orders"


def _order_file(run_id: str) -> Path:
    return orders_dir() / f"{run_id}.jsonl"


_orders_log_lock = threading.Lock()  # in-process half; _paths.append_locked is the cross-process half


def _write_order_record(run_id: str, record: dict) -> None:
    """Journals one order event *before* the state it describes is mutated,
    and lets a write failure propagate (unlike `_persist_park`): an order this
    process cannot audit is an order it must not acknowledge."""
    line = (json.dumps(record, ensure_ascii=False) + "\n").encode("utf-8")
    with _orders_log_lock:
        _paths.append_locked(_order_file(run_id), line)


def _normalize_order(entry: dict) -> dict:
    """Fills in every field a snapshot written before it existed would lack;
    unknown keys are kept."""
    out = dict(entry)
    out["id"] = out.get("id") if isinstance(out.get("id"), str) else None
    out["text"] = out.get("text") if isinstance(out.get("text"), str) else ""
    out["status"] = out.get("status") or "pending"
    out.setdefault("at", None)
    out.setdefault("applied_at", None)
    out.setdefault("applied_step", None)
    return out


def _find_order(rt: _RunState, order_id: str) -> dict | None:
    for entry in rt.orders:
        if entry.get("id") == order_id:
            return entry
    return None


def _read_order_records(run_id: str) -> list[dict]:
    path = _order_file(run_id)
    if not path.is_file():
        return []
    rows = []
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows


def _replay_order(rt: _RunState, order_id: str) -> dict | None:
    """The entry an id already used by this run maps to, rebuilt from the
    unpruned audit log once the bounded history has evicted it -- so a repeated
    id is still recognised (and a differing text still refused) rather than
    queued as a second order."""
    entry = None
    text = None
    for row in _read_order_records(rt.id):
        if row.get("id") == order_id:
            if text is None:
                text = row.get("text") or ""
            entry = {"id": order_id, "text": text, "status": "pending", "at": row.get("at")}
            if row.get("action") == "applied":
                entry["status"] = "applied"
                entry["applied_at"] = row.get("at")
                entry["applied_step"] = row.get("step")
        elif entry is not None and order_id in (row.get("superseded") or []):
            entry["status"] = "cleared" if row.get("action") == "cleared" else "superseded"
            entry["superseded_by"] = row.get("id")
            entry["superseded_at"] = row.get("at")
    return _normalize_order(entry) if entry is not None else None


def _pending_order(rt: _RunState) -> dict | None:
    """The most recently queued order not yet applied, or `None`."""
    for entry in reversed(rt.orders):
        if entry.get("status") == "pending":
            return entry
    return None


def _active_order(rt: _RunState) -> dict | None:
    """The order currently in force, or `None` (`Run.order` keeps at most one
    entry `applied` at a time, so this is simply the last one)."""
    for entry in reversed(rt.orders):
        if entry.get("status") == "applied":
            return entry
    return None


def _append_order(rt: _RunState, entry: dict) -> dict:
    rt.orders.append(entry)
    if len(rt.orders) > _ORDERS_HISTORY_LIMIT:
        del rt.orders[: len(rt.orders) - _ORDERS_HISTORY_LIMIT]
    return entry


def _apply_pending_order(rt: _RunState) -> str | None:
    """The one place a standing order takes effect. Journals `"applied"` once
    for the pending entry, makes it the active order and marks whatever it
    replaced `"superseded"` -- or `"cleared"` when its own text is empty, which
    is how a clear takes effect. Returns the text now in force, so an applied
    order stays in every later decision context until replaced or cleared, or
    `None` when none is."""
    entry = _pending_order(rt)
    if entry is not None:
        clearing = not (entry.get("text") or "")
        replaced = [e for e in rt.orders if e is not entry and e.get("status") in ("pending", "applied")]
        record = {
            "kind": "order",
            "action": "applied",
            "at": _iso_now(),
            "run": rt.id,
            "id": entry.get("id"),
            "text": entry.get("text") or "",
            "step": rt.counts["steps"],
            "superseded": [e.get("id") for e in replaced],
        }
        _write_order_record(rt.id, record)
        for prior in replaced:
            prior["status"] = "cleared" if clearing else "superseded"
            prior["superseded_by"] = entry.get("id")
            prior["superseded_at"] = record["at"]
        entry["status"] = "applied"
        entry["applied_at"] = record["at"]
        entry["applied_step"] = rt.counts["steps"]
    active = _active_order(rt)
    text = (active.get("text") or "") if active is not None else ""
    return text or None


def _persist_park(rt: _RunState, escalation: dict) -> None:
    """Everything `_restore_run_state` needs to rebuild an equivalent
    `_RunState`: `context`/`active`/`history`/`visits`/`counts`/
    `warnings`/`path_log`/`input`/`ckpt_id`/`graph_ref`, the escalation
    payload itself verbatim, and `elapsed_before_park` -- the run's own
    *duration* so far (`time.monotonic() - rt.start_mono`), not a raw
    `time.monotonic()` reading, because that clock's reference point is
    undefined across process instances and a value from a process that no
    longer exists cannot be compared against anything (Python's own
    `time` docs); a duration, unlike a timestamp, survives the crossing
    intact. `_restore_run_state` anchors a fresh `start_mono` so that
    `time.monotonic() - rt.start_mono` reads exactly `elapsed_before_park`
    again the instant reconstruction happens -- the wall budget picks up
    where the walk left off rather than either forgetting the time already
    spent or, worse, charging the run for however long it sat parked
    waiting on an operator plus however long the service was down, which
    would make a slow-to-answer human indistinguishable from a runaway
    loop. Best-effort, like `_log_pick`'s own decision-log write just
    above: a full disk here must not stop the escalation this run already
    computed from reaching its caller, so a failure is a warning on the
    run, not a raised exception -- the run simply will not survive a
    restart, which is the pre-persistence behaviour every run already had.
    """
    record = {
        "kind": "escalate",
        "at": _iso_now(),
        "run": rt.id,
        "graph_ref": rt.graph_ref,
        "input": rt.input,
        "context": rt.context,
        "active": _encode_path(rt.active),
        "history": {_encode_path(k): v for k, v in rt.history.items()},
        "visits": {_encode_path(k): v for k, v in rt.visits.items()},
        "counts": dict(rt.counts),
        "decisions": copy.deepcopy(rt.decisions),
        "transitions": copy.deepcopy(rt.transitions),
        "trace": copy.deepcopy(rt.trace),
        "trace_sequence": rt.trace_sequence,
        "warnings": list(rt.warnings),
        "path_log": list(rt.path_log),
        "elapsed_before_park": time.monotonic() - rt.start_mono,
        "start_wall": rt.start_wall,
        "ckpt_id": rt.ckpt_id,
        "orders": copy.deepcopy(rt.orders),
        "escalation": escalation,
        # The request-id counter as of this park, so a reconstructed run
        # cannot rewind `next_request_id` and hand out an id a previous
        # process already used for an `act` or an earlier park of this same
        # run (the escalation's own `request` is on `escalation` above, but
        # only that one id -- this is where the counter resumes *after* it).
        "req_seq": rt._req_seq,
        "warrant": rt.warrant,
    }
    try:
        _write_run_record(rt.id, record)
    except OSError as e:
        rt.warnings.append(f"park snapshot write failed: {e} -- this run will not survive a service restart")


def _persist_resolved(rt: _RunState) -> None:
    """Marks the park just answered (or stopped) as no longer this run's
    live suspension point. Deliberately smaller than `_persist_park`'s own
    record -- no `context`/`history`/`visits`/`escalation` -- because
    nothing downstream of this point should ever be reconstructed *from*
    a `"resolved"` line; `_load_orphan` only ever reads one to report that
    a run went dark sometime after it, never to resume it. Just enough
    (`active`, `counts`, `path_log`, `graph_ref`, `ckpt_id`) for that
    report to say something concrete rather than only "unknown". Same
    best-effort reasoning as `_persist_park`: a write failure here means a
    *future* crash could misread a stale `"escalate"` line as still live,
    which is a real gap, but raising here would mean a full disk could
    turn a driver's ordinary, successful answer into an exception -- worse
    than the gap it would be closing.
    """
    record = {
        "kind": "resolved",
        "at": _iso_now(),
        "run": rt.id,
        "graph_ref": rt.graph_ref,
        "active": _encode_path(rt.active),
        "counts": dict(rt.counts),
        "decisions": copy.deepcopy(rt.decisions),
        "transitions": copy.deepcopy(rt.transitions),
        "trace": copy.deepcopy(rt.trace),
        "trace_sequence": rt.trace_sequence,
        "path_log": list(rt.path_log),
        "ckpt_id": rt.ckpt_id,
        "warrant": rt.warrant_id,
    }
    try:
        _write_run_record(rt.id, record)
    except OSError as e:
        rt.warnings.append(
            f"park-resolved marker write failed: {e} -- a later restart could misread this run's last "
            "park as still pending an answer"
        )


def _read_last_record(run_id: str) -> dict | None:
    """The read side of `_persist_park`/`_persist_resolved`: the last
    complete JSON line in this run's own file, or `None` if nothing was
    ever written for it at all -- an id that was never real, or one that
    was, but suspended on an `act` and crashed before ever parking (this
    module never writes a snapshot for that case). `_load_orphan` is the
    only caller, and only once `run_id` has already missed `_RUNS` --
    never on the normal, still-in-memory path."""
    path = _run_file(run_id)
    if not path.is_file():
        return None
    last = None
    with path.open("r", encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                last = json.loads(line)
    return last


def _park(rt: _RunState, escalation: dict) -> Generator[dict, dict, dict]:
    """Yield `escalation` (kind `"escalate"`) and suspend until answered.
    Exhausting the graph's `escalations` budget here, before ever parking,
    is deliberate: once escalation can happen more than once in a run's
    life (it always could park/answer/park again from here on), the budget
    section 4 names alongside steps/actions/wall_s is the only thing that
    bounds that -- ending the run with `exhausted` rather than parking
    again, same as any other budget.

    `Run.answer` validates a `pick` against `escalation["options"]` before
    ever touching the generator, so what comes back here on a normal
    `.send` is always `{"index": int, "by": str}` -- a checked pick, never
    a raw caller payload. A `stop` answer, and `Run.stop` directly, never
    resume this `yield` at all: both interrupt it with `_RunStopped`
    (`.throw`), which propagates out of this generator to `_interpret`'s
    outer try -- this function has no stop-handling of its own to write.

    **Persists before yielding.** This is the module's one new write hook
    (see the module docstring): `_persist_park` snapshots enough of `rt`
    to rebuild an equivalent `_RunState` after a restart, written *before*
    the escalation ever reaches a caller, so there is no window where a
    driver could see the question but the snapshot not yet be durable.
    `_persist_resolved` marks it answered-or-stopped afterward, on the two
    paths that mean this park is genuinely settled -- an ordinary resume
    (falls through below the `try`) or a `_RunStopped` interrupt (caught,
    persisted, re-raised) -- so a *later* crash is never misread as still
    being parked here.

    Deliberately **not** a bare `finally`: this generator can also exit via
    `GeneratorExit` when nothing ever answers or stops it and the last
    reference to it simply goes away (an abandoned `Run` falling out of
    `_RUNS`, or a test dropping it to simulate a restart) -- a real crash
    never runs this at all, but an in-process drop does, and a `finally`
    cannot tell that apart from a real resolution. A `GeneratorExit` here
    is left uncaught, so it propagates with nothing written: the persisted
    `"escalate"` snapshot stays the live, resumable truth for whoever
    reconstructs this run next, instead of being overwritten with a
    `"resolved"` marker for a park nothing ever actually answered."""
    if rt.counts["escalations"] + 1 > rt.graph.budget("escalations", 10):
        raise _BudgetExhausted("escalations")
    rt.escalation = escalation
    rt.counts["escalations"] += 1
    _persist_park(rt, escalation)
    try:
        answer = yield escalation
    except _RunStopped:
        rt.escalation = None
        _persist_resolved(rt)
        raise
    rt.escalation = None
    _persist_resolved(rt)
    return answer


def _resolve_pick(pick, options_list: list[dict]) -> int | None:
    """`pick` (an index, or a label) matched against an escalation's own
    `options` list (`[{"index":, "label":, "p":}, ...]`) -- section 4,
    "An answer outside the menu is not accepted." Returns the matched
    option's recorded `index` (read off the record, not assumed to equal
    its position, in case a future caller ever hands back a filtered
    subset), or `None` for no match -- including every pick against an
    empty `options` list, which is exactly what an `EMPTY` escalation
    always has."""
    if isinstance(pick, bool):
        return None  # bool is an int subclass in Python; never a real pick
    if isinstance(pick, int):
        return pick if any(o["index"] == pick for o in options_list) else None
    if isinstance(pick, str):
        for o in options_list:
            if o["label"] == pick:
                return o["index"]
        if pick.lstrip("-").isdigit():
            return _resolve_pick(int(pick), options_list)
        return None
    return None


def _preview_action(rt: _RunState, path: tuple[str, ...], event: str, event_scope: dict) -> dict | None:
    """The decision row's `action` field: the first `tool` action the
    transition this pick is about to run would dispatch, rendered against
    the same scope `_execute_transition` will use. A preview, not a
    dispatch -- re-rendering is safe because templates are pure. `entail`
    is deliberately not threaded in here even though `_find_transition`
    now accepts one: every shipped `on: {PICK: ...}` handler is unguarded,
    so this never needs it in practice, and the real evaluation already
    happened once for the event this call is about to fire (`_fire_event`,
    right after this returns) -- wiring a second copy of it into a logging
    preview would mean paying for the model twice per pick."""
    found = _find_transition(rt.graph, path, event, event_scope, None, None, rt.default_threshold())
    if found is None:
        return None
    _owning, transition = found
    for action in transition.get("actions") or []:
        if action["type"] == "tool":
            params = action["params"]
            return {"tool": params["name"], "input": _render_deep(params.get("input") or {}, event_scope, [])}
    return None


def _record_choice(rt, path, opts, index, probs, source, floor, margin, waiting=False):
    scored = source not in {"rule", "forced"}
    ordered = sorted(probs, reverse=True) if scored else []
    row = {
        "state": ".".join(path), "step": rt.counts["steps"], "source": source,
        "chosen": opts[index].label if index is not None else None,
        "options": [{"label": opt.label, "score": probs[i] if scored else None} for i, opt in enumerate(opts)],
        "top_score": ordered[0] if ordered else None,
        "gap": ordered[0] - ordered[1] if len(ordered) > 1 else None,
        "floor": floor, "required_margin": margin, "waiting": waiting,
    }
    if rt.decisions and (rt.decisions[-1]["state"], rt.decisions[-1]["step"]) == (row["state"], row["step"]):
        rt.decisions[-1] = row
    else:
        rt.decisions.append(row)


def _log_pick(
    rt: _RunState,
    path: tuple[str, ...],
    context_str: str,
    opts: list,
    index: int,
    probs: list[float],
    source: str,
    verified: str | None,
    floor: float,
    margin: float,
) -> None:
    _record_choice(rt, path, opts, index, probs, source, floor, margin)
    _trace(rt, "decision", decision=rt.decisions[-1], context=context_str)
    chosen = opts[index]
    event_scope = {**rt.scope(), "event": {"option": {**chosen.data, "label": chosen.label, "event": chosen.event}}}
    action = _preview_action(rt, path, chosen.event, event_scope)
    row_id = f"{rt.id}-{rt.counts['steps']:04d}"
    try:
        decisions.log_decision(
            id=row_id,
            context=context_str,
            options=[o.label for o in opts],
            label=index,
            chosen=chosen.label,
            probs=list(probs),
            source=source,
            verified=verified,
            floor=floor,
            margin=margin,
            run=rt.id,
            graph=_graph_ref(rt.graph),
            state=".".join(path),
            step=rt.counts["steps"],
            ckpt=rt.ckpt_id,
            action=action,
            warrant=rt.warrant_id,
        )
    except OSError as e:
        rt.warnings.append(f"decision log write failed: {e}")


def _choice_scoring_context(rt: _RunState, state: dict, choose_cfg: dict, scope: dict, *, apply_order=True) -> tuple[float, float, str]:
    """`floor`, `margin`, `context_str` -- the three things `_choose_phase`
    needs once it knows `opts` is non-empty, split out only so
    `_resume_parked_choice` (below) can re-derive the exact same three
    after a restart, deterministically, from the restored context --
    never trusted from a stale copy, since `context_str` is a *rendering*
    of a template, not data a snapshot could carry as-is.

    Since a standing order (`automation.order`) is precisely a guidance
    string for whoever reads this context next, this is also where a pending
    one is spent: `_apply_pending_order` journals it and marks it applied,
    and its text is prepended as `"Operator standing order: ..."`. Nothing
    else about the choice changes -- the same `opts`, floor, margin, graph,
    tool permissions, warrant, pending request and menu as with no order at
    all. An order guides scoring; it is not a restriction and not a stop
    (`automation.stop` is the separate, explicit way to end a run).
    """
    floor = (state.get("meta") or {}).get("floor", rt.graph.default("floor", 0.5))
    margin = (state.get("meta") or {}).get("margin", rt.graph.default("margin", 0.1))
    context_str = options.chooser_context(choose_cfg, scope, state.get("description"), rt.warnings)
    # `automation.order`'s pending standing order takes effect here and only
    # here: this is the one place a chooser is about to read a context. Guarded
    # on `rt.escalation is None` because the other invocation of this function
    # (`_resume_parked_choice`, on the way out of a park) re-derives the same
    # three values for `_log_pick`'s row only -- a human's parked menu has
    # already been decided by the person looking at it, so that path must not
    # spend the order (see that function's own docstring). `_apply_pending_order`
    # journals before it marks the entry applied, and a journal failure
    # propagates out of the run rather than being swallowed.
    if apply_order:
        standing = _apply_pending_order(rt)
        if standing:
            context_str = f"{_ORDER_PREFIX}{standing}\n{context_str}"
    return floor, margin, context_str


def _finish_choice(
    rt: _RunState,
    path: tuple[str, ...],
    opts: list,
    index: int,
    probs: list[float],
    source: str,
    verified: str | None,
    floor: float,
    margin: float,
    context_str: str,
) -> Generator[dict, dict, str]:
    """The tail every choice reaches once an `index` has been decided,
    whichever of the three ways got it there (forced, cleared the floor,
    or a park just answered) -- and, after a restart, the exact same tail
    `_resume_parked_choice` reaches too, sharing this function rather than
    a second copy of it: the stale-ref check, the decision-log row, and
    firing the picked option's event."""
    chosen = opts[index]
    ref = chosen.data.get("ref")
    if ref and ref not in _current_known_refs(rt):
        raise _ToolError(
            f"stale ref {ref!r}: the snapshot that produced it is no longer current",
            reason="stale", tool="pick",
        )
    _log_pick(rt, path, context_str, opts, index, probs, source, verified, floor, margin)
    event_data = {"option": {**chosen.data, "label": chosen.label, "event": chosen.event}}
    status = yield from _fire_event(rt, path, chosen.event, event_data)
    return status


def _choose_phase(rt: _RunState, path: tuple[str, ...], state: dict, choose_cfg: dict) -> Generator[dict, dict, str]:
    scope = rt.scope()
    opts, warnings, duplicates_collapsed = options.build_options(state, choose_cfg, scope, rt.graph)
    rt.warnings.extend(warnings)
    rt.counts["duplicates_collapsed"] += duplicates_collapsed
    if not opts:
        status = yield from _fire_event(rt, path, "EMPTY", {})
        return status
    floor, margin, context_str = _choice_scoring_context(rt, state, choose_cfg, scope)
    # docs/design/judgement.md section 2.2: `choose.prefer` is tried before
    # anything else reaches for the chooser or the floor -- "found it" is a
    # rule the author wrote, not a number to weigh against a threshold.
    # `first_preferred` never mutates `opts`/`scope` and never calls the
    # chooser itself, so a graph with no `prefer` (or one whose rules all
    # miss) falls through to exactly the two branches below, unchanged --
    # same options, same scores, same escalation as before this existed.
    preferred = options.first_preferred(
        choose_cfg.get("prefer") or [], opts, scope, rt.warnings,
        entail=rt.entail, default_threshold=rt.default_threshold(),
    )
    if preferred is not None:
        # docs/design/judgement.md section 3's cost table is literal: "zero
        # cost (no model call)" -- the chooser is never reached on a rule
        # hit, full stop, the same way a forced single-option pick below
        # never reaches it either. `probs` gets the same convention a
        # forced pick already uses for "no one was asked, so there is no
        # score" -- one-hot on the winning index, not a chooser opinion --
        # and it is `source`/`verified` == "rule" that a reader must check
        # before trusting `probs` as anything but that: a rule is a
        # decision, not a score, never a confidence number dressed up as
        # one (judgement.md section 2.2, point (d)).
        index = preferred
        probs = [1.0 if i == index else 0.0 for i in range(len(opts))]
        source = verified = "rule"
        rt.counts["rule_picks"] += 1
    elif len(opts) == 1:
        index, probs, source, verified = 0, [1.0], "forced", None
    else:
        probs = list(rt.chooser(context_str, [o.label for o in opts]))
        rt.counts["chooser_calls"] += 1
        index = max(range(len(probs)), key=lambda i: probs[i])
        top1 = probs[index]
        top2 = sorted(probs, reverse=True)[1]
        if not (top1 >= floor and (top1 - top2) >= margin):
            _record_choice(rt, path, opts, None, probs, "jevlike", floor, margin, waiting=True)
            escalation = _floor_escalation(rt, path, state, opts, probs, floor, margin)
            answer = yield from _park(rt, escalation)
            index = answer["index"]
            source = verified = answer["by"]
        else:
            source, verified = "jevlike", None
    status = yield from _finish_choice(rt, path, opts, index, probs, source, verified, floor, margin, context_str)
    return status


def _resume_parked_choice(
    rt: _RunState, path: tuple[str, ...], state: dict, choose_cfg: dict, escalation: dict
) -> Generator[dict, dict, str]:
    """The reconstructed twin of the tail `_choose_phase` runs once its own
    `_park` returns -- entered only by `_interpret_from_park`, for a run
    whose persisted snapshot shows it was suspended on an `escalate` when
    its process died. Never redoes the chooser call or the floor/margin
    verdict that led to parking in the first place: that already happened,
    in the process that died, and `rt.counts`/the persisted `escalation`
    already carry its result. Never re-evaluates `choose.prefer` either,
    for the same reason: a park is only ever reached *after* `prefer` has
    already had its chance and missed (a matching rule skips the floor,
    the park, and this whole function -- `_choose_phase`, above), so
    reaching `_resume_parked_choice` at all is itself proof no rule
    matched the first time and none needs asking again now. What this
    *does* redo: `opts`, since no `Option` is JSON-shaped and so none
    survived the restart -- rebuilt by calling the same pure
    `options.build_options` again against the restored `rt.context`,
    exactly the reviewer's finding this module's own docstring now
    carries. `floor`/`margin`/`context_str` are re-derived the same
    deterministic way, needed only for `_log_pick`'s row once
    `_finish_choice` runs. `duplicates_collapsed` genuinely is counted
    again here, unlike `chooser_calls`: it is a fact about this call to
    `build_options`, which really did just run a second time (once in the
    process that died, once more here, in this one), not a fact about the
    chooser or the floor, neither of which re-runs.

    The one thing this re-derivation deliberately does *not* do: spend a
    pending `automation.order` standing order. `_choice_scoring_context`
    folds one in only while no park is live, and right here one still is
    (`rt.escalation` is the restored escalation until the `yield` below
    resumes) -- correct, because this `context_str` feeds `_log_pick`'s row
    for a choice the parked menu already decided, not a chooser call. An
    order queued while this run sat parked therefore still applies to the
    next choice the graph actually makes, and the operator's answer to the
    parked question is never silently rescored or replaced by it.
    """
    scope = rt.scope()
    opts, warnings, duplicates_collapsed = options.build_options(state, choose_cfg, scope, rt.graph)
    rt.warnings.extend(warnings)
    rt.counts["duplicates_collapsed"] += duplicates_collapsed
    floor, margin, context_str = _choice_scoring_context(rt, state, choose_cfg, scope, apply_order=False)
    rt.escalation = escalation
    try:
        answer = yield escalation
    except _RunStopped:
        # A stop while parked here interrupts the `yield` with
        # `_RunStopped` (`.throw`, from `Run.stop`) instead of resuming it
        # -- this park is settled either way, so persist the same as the
        # ordinary-answer path below before letting it propagate on out to
        # `_with_outcome_handling`.
        rt.escalation = None
        _persist_resolved(rt)
        raise
    # Deliberately not a bare `finally` covering both this and the `except`
    # above: this generator can also exit via `GeneratorExit` when nothing
    # ever answers or stops it and the last reference to it simply goes
    # away (an abandoned `Run` falling out of `_RUNS`, or a test dropping
    # it to simulate a restart) -- not a real resolution, and a `finally`
    # cannot tell it apart from one. Left uncaught here, so it propagates
    # with nothing written and the persisted `"escalate"` snapshot stays
    # the live, resumable truth. See `_park`, which persists before
    # yielding rather than after and so carries the fuller version of this
    # reasoning.
    rt.escalation = None
    _persist_resolved(rt)
    menu = escalation.get("options") or []
    if len(opts) != len(menu):
        # `options.build_options` is pure in `rt.context`, and nothing
        # between the original park and this resume ever changes it (no
        # `tool` action runs in between -- a park is the one place the
        # interpreter suspends with no action in flight) -- so this should
        # be unreachable in practice. Refusing loudly beats indexing into
        # `opts` with a position that may no longer name the same option
        # the operator was actually looking at.
        #
        # reason="stale", tool="pick": docs/design/observation.md section 4
        # names `stale` for a `PICK` whose ref is no longer current and does
        # not separately name this site -- but the belief being refused is
        # the same one: a pick this runner cannot trust belongs to the
        # snapshot (here, the restored context) it claims to. Reusing the
        # closest existing category rather than inventing a fifth reason
        # value nothing else produces or tests for.
        raise _ToolError(
            f"orphan recovery at {'.'.join(path) or '<root>'!r}: rebuilt {len(opts)} option(s) from the "
            f"restored context but the persisted escalation had {len(menu)} -- refusing to guess which "
            "option is which rather than risk firing the wrong one",
            reason="stale", tool="pick",
        )
    index = answer["index"]
    source = verified = answer["by"]
    probs = [float(o.get("p", 0.0)) for o in menu]
    status = yield from _finish_choice(rt, path, opts, index, probs, source, verified, floor, margin, context_str)
    return status


def _check_wall_budget(rt: _RunState) -> None:
    if time.monotonic() - rt.start_mono > rt.graph.budget("wall_s", 600):
        raise _BudgetExhausted("wall_s")


# --- the interpreter loop --------------------------------------------------------


def _run_loop(rt: _RunState) -> Generator[dict, dict, None]:
    """The steady state both `_interpret` (a fresh run, after its initial
    `_enter`) and `_interpret_from_park` (a reconstructed one, after its
    first pending pick has been answered) fall into -- extracted verbatim
    from what used to be `_interpret`'s own `while True:` body, so the
    live and the reconstructed path share the exact same loop rather than
    two copies that could quietly drift apart. Behaviourally identical to
    before this split: every statement below is unchanged, only its
    address moved."""
    while True:
        _check_wall_budget(rt)
        path = rt.active
        state = rt.graph.state_at(path)
        try:
            fired = yield from _check_always(rt, path, state)
            if fired:
                continue
            choose_cfg = (state.get("meta") or {}).get("choose")
            if choose_cfg is None:
                rt.outcome = "error"
                rt.error = (
                    f"state {'.'.join(path)!r} has no way to progress: no always guard "
                    "passed and there is no choose block"
                )
                return
            if rt.counts["steps"] + 1 > rt.graph.budget("steps", 50):
                raise _BudgetExhausted("steps")
            rt.counts["steps"] += 1
            status = yield from _choose_phase(rt, path, state, choose_cfg)
        except _ToolError as e:
            status = yield from _fire_event(
                rt, rt.active, "ERROR", {"error": e.text, "reason": e.reason, "tool": e.tool}
            )
        if status == "ended_error":
            rt.outcome = "error"
            return


def _with_outcome_handling(rt: _RunState, body: Generator[dict, dict, None]) -> Generator[dict, dict, None]:
    """The outer try/except both `_interpret` and `_interpret_from_park`
    need -- extracted so the four outcome-ending exceptions
    (`_RunReached`/`_BudgetExhausted`/`_RunStopped`/a `_ToolError` nothing
    caught) are handled in exactly one place rather than two copies that
    name the same four exceptions and could silently stop agreeing about
    what each one means. `yield from` makes this fully transparent to
    `.send`/`.throw` either way (PEP 380) -- a caller driving the
    generator this returns cannot tell it is not `body` itself, except
    that an exception `body` raises is now handled here instead of
    propagating further."""
    try:
        yield from body
    except _RunReached as e:
        rt.outcome = "reached"
        rt.final_state = e.state_name
    except _BudgetExhausted as e:
        rt.outcome = "exhausted"
        rt.exhausted = e.name
    except _RunStopped as e:
        rt.outcome = "stopped"
        rt.stop_reason = e.reason
    except _ToolError as e:
        rt.outcome = "error"
        rt.error = f"unhandled tool error while already recovering from another: {e.text}"
        rt.error_reason = e.reason


def _bootstrap_and_loop(rt: _RunState) -> Generator[dict, dict, None]:
    try:
        yield from _enter(rt, (), (rt.graph.initial,))
    except _ToolError as e:
        status = yield from _fire_event(
            rt, rt.active, "ERROR", {"error": e.text, "reason": e.reason, "tool": e.tool}
        )
        if status == "ended_error":
            rt.outcome = "error"
            return
    yield from _run_loop(rt)


def _interpret(rt: _RunState) -> Generator[dict, dict, None]:
    """Observe / check / choose / act, forever, until `rt.outcome` is set.
    Yields `{"kind": "act", ...}` or `{"kind": "escalate", ...}` and
    nothing else; the caller (`Run`) turns generator exhaustion into the
    final report."""
    yield from _with_outcome_handling(rt, _bootstrap_and_loop(rt))


def _interpret_from_park(rt: _RunState) -> Generator[dict, dict, None]:
    """`_interpret`'s reconstructed twin: entered only for a run whose
    persisted snapshot shows it was suspended on an `escalate` when its
    process died. Never the original generator -- that died with the
    process, by construction (a call stack is not serialisable) -- a new
    one built to start exactly where the old one was: `rt.active`/`state`/
    `choose_cfg` are all already restored (`_restore_run_state`), and
    `rt.escalation` holds the exact payload the old process yielded, so
    this re-yields it (through `_resume_parked_choice`) rather than
    re-deciding anything, and only then falls into the same `_run_loop`
    `_interpret` itself runs -- the two can never quietly drift apart,
    because past this first step they are the same code."""
    path = rt.active
    state = rt.graph.state_at(path)
    choose_cfg = (state.get("meta") or {}).get("choose")
    escalation = rt.escalation

    def body():
        status = yield from _resume_parked_choice(rt, path, state, choose_cfg, escalation)
        if status == "ended_error":
            rt.outcome = "error"
            return
        yield from _run_loop(rt)

    yield from _with_outcome_handling(rt, body())


def _final_report(rt: _RunState) -> dict:
    report = {
        "kind": "final",
        "run": rt.id,
        "graph": _graph_ref(rt.graph),
        "outcome": rt.outcome,
        "final_state": rt.final_state,
        "output": rt.output,
        "steps": rt.counts["steps"],
        "actions": rt.counts["actions"],
        "escalations": rt.counts["escalations"],
        "chooser_calls": rt.counts["chooser_calls"],
        # docs/design/judgement.md section 2.1/2.2: on the record at the
        # run level, not only per-row, so an operator (or a test) can see
        # the mechanism fired without reading the decision log at all --
        # a `prefer` hit is a rule, and this is where "how many rules
        # fired, how many labels this run ever had to collapse" is legible
        # without reconstructing it from `chooser_calls`/`escalations`.
        "duplicates_collapsed": rt.counts["duplicates_collapsed"],
        "rule_picks": rt.counts["rule_picks"],
        "wall_s": round(time.monotonic() - rt.start_mono, 3),
        "exhausted": rt.exhausted,
        "path": list(rt.path_log),
        "log": str(decisions.graph_log_path(rt.graph.id)),
        "warrant": _warrant_field(rt),
    }
    if rt.outcome == "stopped":
        report["reason"] = rt.stop_reason
    if rt.outcome == "error":
        report["error"] = rt.error
        report["reason"] = rt.error_reason
    if rt.outcome == "orphaned":
        # `_run_from_lost_record`'s only caller of this branch: an
        # `act`-suspended run this process rediscovered as a `"resolved"`
        # (or otherwise non-`"escalate"`) last line -- section 4's other
        # `orphaned` kind, which reports rather than resumes. Never set on
        # a run that lived out this process's whole life or that
        # reconstructed as a genuine escalate-resume; both of those reach
        # every other branch above exactly as they always did.
        report["lost"] = rt.lost
    if rt.warnings:
        report["warnings"] = list(dict.fromkeys(rt.warnings))
    return report


def _log_run_end(rt: _RunState) -> None:
    """automation.md section 5: "A run's end appends `{run, outcome, ts}`,
    which is what lets the exporter promote that run's unverified rows to
    `outcome`." S45 (docs/Tasklist.md): `decisions.log_run_end` existed,
    tested in isolation (`test_decisions.py`), and had no caller anywhere
    in this module -- this is that caller.

    The only call site is `Run._settle`'s `yielded is None` branch: every
    outcome the interpreter itself reaches (`reached`/`exhausted`/
    `stopped`/`error`) funnels through there, whichever of `begin`/
    `resume`/`answer`/`stop` drove the generator to exhaustion, and
    `rt.outcome` is always one of those four literal strings by the time
    a caller gets this far (`_with_outcome_handling` sets it on every one
    of the four ways `_interpret`/`_interpret_from_park` can end, and nothing
    reaches `_settle` with `yielded is None` before that has happened).

    Deliberately **not** called for `outcome="orphaned"`
    (`_run_from_lost_record`, this module's fourth outcome): that function
    builds an already-`finished` `Run` directly and never touches
    `_settle` at all, and that is not an oversight this function should
    route around -- `orphaned` is not an outcome the interpreter reached,
    it is this process admitting it does not know one. Writing a row that
    named it anyway would tell a future exporter something this process
    does not actually know; the correct record of an orphaned run's
    fate is whatever real `log_run_end` row (if any) a *previous*
    process already wrote for it before it was lost, not a new one
    manufactured here.

    Called *before* `_final_report`, not after: a failed write appends to
    `rt.warnings` exactly like `_log_pick`'s own decision-log write does,
    and `_final_report` is what copies `rt.warnings` into the report the
    caller actually sees (`_status_dict` never re-reads `rt.warnings` on a
    later poll) -- calling this after `_final_report` would append a
    warning nothing downstream ever shows. Best-effort for the same reason
    `_log_pick` is: the summary line written once, at the very end, is not
    worth losing a run's own report over.
    """
    try:
        decisions.log_run_end(graph=_graph_ref(rt.graph), run=rt.id, outcome=rt.outcome, warrant=rt.warrant_id)
    except OSError as e:
        rt.warnings.append(f"run-end log write failed: {e}")


# --- the public Run wrapper -----------------------------------------------------


class Run:
    """Wraps `_interpret`'s generator with the request/response shape the
    service-driver contract needs: `begin()` primes it; `resume(request_id,
    result)` answers a pending `act` request; `answer(pick=..., stop=...)`
    answers a pending `escalate` request; `stop(reason)` ends the run
    outright, from wherever it currently is. Idempotent against a stale or
    repeated `act` id, or against `answer`/`resume` called when nothing of
    that kind is pending -- see this module's docstring.

    `_lock` serialises every one of those four against each other: a run
    now genuinely outlives the tool call that created it, so a later
    `jev_resume`/`jev_stop` can race the driver that is still mid-`act` for
    the same run, from a different call, possibly a different session --
    something that was never reachable before parking existed.
    """

    def __init__(
        self,
        run_id: str,
        graph: graph_mod.Graph,
        run_input: dict,
        chooser: Chooser,
        entail: Entailer | None,
        ckpt_id: str | None,
        graph_ref=None,
        warrant: dict | None = None,
        warrant_id: str | None = None,
    ):
        self.id = run_id
        self.state = _RunState(
            run_id, graph, run_input, chooser, entail, ckpt_id, graph_ref, warrant, warrant_id,
        )
        self._gen = _interpret(self.state)
        self._pending: dict | None = None
        self._last_public: dict | None = None
        self.finished = False
        self._lock = threading.Lock()

    def begin(self) -> dict:
        with self._lock:
            try:
                yielded = next(self._gen)
            except StopIteration:
                yielded = None
            return self._settle(yielded)

    def resume(self, request_id: str, result: dict) -> dict:
        with self._lock:
            if self.finished:
                return self._last_public
            if (
                self._pending is None
                or self._pending.get("kind") != "act"
                or request_id != self._pending["id"]
            ):
                # Stale or duplicate id, or nothing (or something other
                # than an `act`) is actually pending: a driver that
                # retried after a dropped connection must not double-act.
                # Hand back the current request unchanged rather than
                # touching the generator again.
                return self._last_public
            try:
                yielded = self._gen.send(result or {})
            except StopIteration:
                yielded = None
            return self._settle(yielded)

    def answer(
        self, *, pick=None, stop=None, by: str | None = None, note: str | None = None,
        warrant: dict | None = None, request: str | None = None,
    ) -> dict:
        """`automation.answer`: validate `pick`/`stop` against whatever is
        *currently* parked and resume the generator. Answers the run's one
        live `escalate` request; if there is not one right now (already
        answered, mid-`act`, or finished), this is a no-op that hands back
        the current state, the same idempotency shape `resume` already
        gives a stale `act` id.

        `request`, when given, must equal the pending escalation payload's
        own `request` (`_floor_escalation`/`_empty_escalation`, both of
        which mint one via `next_request_id`) -- the stable id that names
        *which* question this answer is for. A mismatch is refused with
        `ValueError` before the generator is touched at all, so a caller
        still holding an older question's id (a run that parked, was
        answered, and parked again) cannot silently answer the new one;
        the live park is left exactly as it was. Omitting `request`
        (every caller written before request ids existed) skips the check
        entirely and answers whatever is pending, unchanged.

        A `pick` that does not match the pending escalation's `options`
        raises `ValueError` *without* touching the generator at all -- the
        run stays parked, exactly as it was, ready for a real answer;
        section 4, "An answer outside the menu is not accepted."

        `warrant`, when given, must equal (after the same list-sort/dedup
        `warrant_mod._normalize` applies at `start`) the block this run was
        actually started under -- checked, like the pick-vs-menu check
        above, *before* the generator is touched at all, so a bad warrant
        never disturbs a live park either. Never checked when omitted
        (`warrant is None`): this is the Python service's own
        graph-identity-and-bookkeeping check, not the gate's per-call
        enforcement (docs/design/unattended.md section 3's table gives that
        job to `crates/rune`, not to this module), so a caller that already
        established the envelope at `jev_run` and simply does not repeat it
        on every `jev_resume` is not refused here for that omission alone.
        """
        with self._lock:
            if self.finished:
                return self._last_public
            if self._pending is None or self._pending.get("kind") != "escalate":
                return self._last_public
            if request is not None:
                # Explicit stale rejection, checked here -- before the
                # warrant check and before anything touches `self._gen`
                # -- so a caller whose view of the run is out of date
                # learns that, in full, without the live decision being
                # altered one bit: the run stays parked on whatever
                # question it is actually on. Deliberately applied to a
                # `stop` as well: an id that no longer names the pending
                # handoff means the caller is not looking at the run as it
                # is now, and `automation.status` (not a blind stop) is
                # what closes that gap.
                current = self._pending.get("request")
                if current != request:
                    what = (
                        f"request {current!r}" if current is not None
                        else "an escalation carrying no request id at all (a legacy snapshot)"
                    )
                    raise ValueError(
                        f"stale request {request!r}: this run is currently parked on {what} -- the answer was "
                        "refused and the live park is unchanged; re-read automation.status for the question "
                        "actually pending"
                    )
            if warrant is not None:
                rt = self.state
                if rt.warrant is None:
                    raise ValueError("this run was started attended and cannot be resumed under a warrant")
                normalized = warrant_mod._normalize(warrant) if isinstance(warrant, dict) else warrant
                if warrant_mod.canonical_json(normalized) != warrant_mod.canonical_json(rt.warrant):
                    raise ValueError(
                        f"warrant mismatch: this run was started under {warrant_mod.canonical_json(rt.warrant)}"
                    )
            if stop is not None:
                return self._stop_locked(str(stop))
            menu = self._pending.get("options") or []
            index = _resolve_pick(pick, menu)
            if index is None:
                raise ValueError(f"pick {pick!r} does not match any option on the current escalation")
            try:
                yielded = self._gen.send({"index": index, "by": (by or "model")})
            except StopIteration:
                yielded = None
            return self._settle(yielded)

    def order(self, text: str, order_id: str) -> dict:
        """`automation.order`: queue (or clear) a standing order on this run
        -- `{run, order}` where `order` is the entry as stored, `status`
        either `"pending"` (queued, not yet folded into a chooser context)
        or `"applied"` (already folded in by `_choice_scoring_context`,
        which sets `applied_at`/`applied_step`), `"cleared"` for the entry
        the empty text itself leaves behind, or `"superseded"` for a prior
        pending order a newer one replaced.

        Runs under `self._lock`, the same lock `begin`/`resume`/`answer`/
        `stop` already serialise on, so an order queued while the run is
        mid-`act`, parked, or scoring cannot interleave with any of them --
        which is also why this touches nothing but `rt.orders`: never
        `_pending`, never `rt.escalation`, never the graph, the context, the
        menu, the warrant or a live park. A `pending` order's only effect
        anywhere is the `_choice_scoring_context` call it is consumed by
        (that function documents the guard that keeps a park's own
        re-derivation from spending it).

        Idempotent in `order_id`: a retry repeating an id already in the
        bounded history gets that entry back verbatim, whatever status it has
        since reached, and mutates nothing -- no second history entry and no
        second audit row. Text is journaled *before* the mutation (see
        `_write_order_record`, which deliberately lets a write failure
        propagate), so a journaled order is never one the run did not queue,
        and an order the run did not queue is never journaled as accepted.

        Refuses a finished run with `ValueError` -- a standing order guides a
        *live* run's next choice, so there is nothing to attach one to once a
        run has ended (the caller's own `_lookup` has already refused one
        this process never knew at all; no order ever starts a run
        implicitly)."""
        with self._lock:
            rt = self.state
            if self.finished:
                outcome = rt.outcome or "unknown"
                raise ValueError(
                    f"run {self.id!r} is finished (outcome {outcome!r}) -- a standing order can only be queued "
                    "on a run that is still live, and automation.order never starts one implicitly"
                )
            if not isinstance(order_id, str) or not order_id.strip():
                raise ValueError(
                    "automation.order needs 'id' (a nonempty string of at most 128 characters -- the idempotency "
                    "key a retry repeats to get the same order back rather than queueing a second one)"
                )
            if len(order_id) > _ORDER_ID_MAX:
                raise ValueError(f"automation.order 'id' is {len(order_id)} characters; the limit is {_ORDER_ID_MAX}")
            if not isinstance(text, str):
                raise ValueError(
                    "automation.order needs 'text' (a string of at most 2000 characters; the empty string clears "
                    "the standing order rather than queueing one)"
                )
            if len(text) > _ORDER_TEXT_MAX:
                raise ValueError(f"automation.order 'text' is {len(text)} characters; the limit is {_ORDER_TEXT_MAX}")
            existing = _find_order(rt, order_id)
            if existing is not None:
                if existing["text"] != text:
                    raise ValueError("order id already used with different text")
                return {"run": self.id, "order": copy.deepcopy(existing)}
            if len(rt.orders) >= _ORDERS_HISTORY_LIMIT:
                raise ValueError("standing order limit reached for this run")
            at = _iso_now()
            clearing = not text.strip()
            record = {
                "kind": "order",
                "action": "cleared" if clearing else "accepted",
                "at": at,
                "run": self.id,
                "id": order_id,
                "text": text,
                "step": rt.counts["steps"],
                "superseded": [e.get("id") for e in rt.orders if e.get("status") == "pending"],
            }
            # Journal before the mutation (and before this call is
            # acknowledged): a failure here leaves `rt.orders` exactly as it
            # was and propagates to the caller, rather than acknowledging an
            # order that nothing durable records.
            _write_order_record(self.id, record)
            for entry in rt.orders:
                if entry.get("status") == "pending":
                    # Visibly superseded, not rewritten out of history: the
                    # operator can see which pending order a newer one (or a
                    # clear) replaced, and by what id.
                    entry["status"] = "cleared" if clearing else "superseded"
                    entry["superseded_by"] = order_id
                    entry["superseded_at"] = at
            queued = _append_order(
                rt,
                _normalize_order({
                    "id": order_id,
                    "text": text,
                    "status": "pending",
                    "at": at,
                }),
            )
            if rt.escalation is not None:
                _persist_park(rt, rt.escalation)
            return {"run": self.id, "order": copy.deepcopy(queued)}

    def stop(self, reason: str) -> dict:
        """`automation.stop`: end the run now, wherever it currently is --
        mid-`act`, parked on an `escalate`, or (a no-op) already finished."""
        with self._lock:
            return self._stop_locked(reason)

    def _stop_locked(self, reason: str) -> dict:
        if self.finished:
            return self._last_public
        try:
            yielded = self._gen.throw(_RunStopped(str(reason)))
        except StopIteration:
            yielded = None
        return self._settle(yielded)

    def _settle(self, yielded: dict | None) -> dict:
        if yielded is None:
            self.finished = True
            self._pending = None
            _log_run_end(self.state)
            self._last_public = _final_report(self.state)
            return self._last_public
        kind = yielded.get("kind")
        if kind == "act":
            self._pending = yielded
            self._last_public = {
                "kind": "act",
                "id": yielded["id"],
                "tool": yielded["tool"],
                "input": yielded["input"],
                "timeout_s": yielded.get("timeout_s"),
            }
            return self._last_public
        if kind == "escalate":
            self._pending = yielded
            self._last_public = yielded  # already the public shape
            return self._last_public
        raise AssertionError(f"unexpected yielded request kind {kind!r}")


# --- the module-level registry: what server.py's automation.* methods call ------

_RUNS: dict[str, Run] = {}
_RUNS_LOCK = threading.Lock()


def _fresh_run_id() -> str:
    return "r_" + uuid.uuid4().hex[:20]


def _no_such_run_message(run_id: str) -> str:
    """The message for the two cases persistence does not change: an id
    that was never real, and an id `_lookup` never even attempts to
    reconstruct because the caller supplied none of `graphs_dir`/
    `chooser` (every call in this module's own pre-persistence tests, by
    choice -- see `_lookup`'s own docstring). Deliberately *not* reached
    for a known, now-orphaned id once reconstruction is attempted:
    `_load_orphan` answers that case for real -- a genuinely resumable
    `Run` for one still `"escalate"`-parked, an already-`finished`
    `outcome="orphaned"` one otherwise -- so this string never has to
    speak for a run whose progress is actually recoverable, or whose loss
    this process can actually name instead of only gesturing at."""
    return (
        f"no such run {run_id!r}: either that id is wrong, or the jev service has restarted "
        "since this run was created or last parked -- runs live only in this process's memory "
        "(see automation/run.py's module docstring), so its question and its progress are gone "
        "either way. Start the graph again with automation.start."
    )


def _check_input(graph: graph_mod.Graph, run_input: dict) -> None:
    schema = graph.jev.get("input")
    if not isinstance(schema, dict):
        return
    missing = [k for k in schema.get("required") or [] if k not in run_input]
    if missing:
        raise ValueError(f"run input is missing required field(s): {missing}")


def load_graph_ref(graph_ref, graphs_dir: Path, option_tokens: int, context_tokens: int) -> graph_mod.Graph:
    if isinstance(graph_ref, dict):
        return graph_mod.load_graph(graph_ref, option_tokens=option_tokens, context_tokens=context_tokens)
    if not isinstance(graph_ref, str) or not graph_ref:
        raise ValueError("graph must be an id (string) naming a file under graphs/, or an inline graph object")
    path = Path(graphs_dir) / f"{graph_ref}.json"
    if not path.is_file():
        raise ValueError(f"no such graph {graph_ref!r} under {graphs_dir}")
    return graph_mod.load_graph(path, option_tokens=option_tokens, context_tokens=context_tokens)


def start(
    graph_ref,
    run_input: dict | None,
    *,
    graphs_dir: Path,
    chooser: Chooser,
    entail: Entailer | None = None,
    option_tokens: int = 32,
    context_tokens: int = 192,
    ckpt_id: str | None = None,
    warrant: dict | None = None,
) -> dict:
    """`automation.start`: `{graph, input}` -> `{run, request}`. `entail`
    is optional and defaults to `None` -- a graph with no `entails`/
    `contradicts` guard never needs it, and one that does simply never
    passes that guard without it (guards.py's own safety default, the same
    one a real `neutral` verdict gives). `ask` (the design's third
    argument, choosing between the `return` and `user` escalation tiers) is
    still not read here at all -- see this module's own docstring for why
    only `return` is implemented.

    `warrant`, when given, must equal the graph's own declared block
    (`warrant_mod.check`, docs/design/unattended.md section 2) -- checked
    *before* `Run` is ever constructed, so a mismatch refuses the whole
    call with the correct canonical block in the message and dispatches
    nothing, not even the graph's own entry actions. `None` starts an
    **attended** run, exactly as every run before this step did: every
    flagged action still asks, one at a time."""
    graph = load_graph_ref(graph_ref, graphs_dir, option_tokens, context_tokens)
    run_input = run_input or {}
    _check_input(graph, run_input)
    block = block_id = None
    if warrant is not None:
        block, block_id = warrant_mod.check(warrant, graph)
    run_id = _fresh_run_id()
    run = Run(run_id, graph, run_input, chooser, entail, ckpt_id, graph_ref, block, block_id)
    # docs/design/judgement.md section 2.2: an NLI guard inside
    # `choose.prefer` is admitted, never refused -- but it costs one
    # `entail` round trip per option per visit to that state, never
    # batched the way a state's `on`/`always` transitions are
    # (`guards.first_passing`), so a fresh run is told up front, by state,
    # rather than discovering it hop by hop. Before `run.begin()`: the
    # first choose phase can escalate or reach a final state before this
    # call returns, and the warning must already be on `rt.warnings` for
    # that first report to carry it.
    for dotted in graph.prefer_nli_states:
        run.state.warnings.append(f"{dotted}.meta.choose.prefer: one entail pair per option per visit")
    with _RUNS_LOCK:
        _RUNS[run_id] = run
    return {"run": run_id, "request": run.begin()}


# --- orphan reconstruction: the read side of the persistence above --------------


def _restore_run_state(
    record: dict,
    *,
    graphs_dir: Path,
    chooser: Chooser,
    entail: Entailer | None,
    option_tokens: int,
    context_tokens: int,
) -> _RunState:
    """Rebuild a `_RunState` from one of `_persist_park`'s own records --
    `graph` is reloaded fresh through `load_graph_ref`, the same call
    `start()` itself makes, from the `graph_ref` the record carried
    verbatim; `chooser`/`entail` are this reconstruction's own (a snapshot
    cannot serialise a function), everything else restored as written.
    """
    graph_ref = record.get("graph_ref")
    graph = load_graph_ref(graph_ref, graphs_dir, option_tokens, context_tokens)
    rt = _RunState(
        record["run"], graph, record.get("input") or {}, chooser, entail, record.get("ckpt_id"), graph_ref,
    )
    rt.context = record.get("context") or {}
    rt.active = _decode_path(record.get("active") or "")
    rt.history = {_decode_path(k): v for k, v in (record.get("history") or {}).items()}
    rt.visits = {_decode_path(k): v for k, v in (record.get("visits") or {}).items()}
    # Merged onto the freshly-initialised defaults, not replaced by the
    # record outright: a park (or lost-record) persisted before a new
    # counter key existed (`duplicates_collapsed`/`rule_picks`, this step)
    # must not come back missing it -- `rt.counts[...] += 1` downstream, or
    # `_final_report`/`_status_dict` simply reading the key, would KeyError
    # on a dict a restore silently narrowed. A record already carrying
    # every current key overlays them all, a no-op change from the old
    # `dict(record.get("counts") or rt.counts)`.
    rt.counts = {**rt.counts, **(record.get("counts") or {})}
    rt.decisions = copy.deepcopy(record.get("decisions", []))
    rt.transitions = copy.deepcopy(record.get("transitions", []))
    rt.trace = copy.deepcopy(record.get("trace", []))
    rt.trace_sequence = record.get("trace_sequence", len(rt.trace))
    rt.warnings = list(record.get("warnings") or [])
    rt.path_log = list(record.get("path_log") or [])
    rt.orders = [_normalize_order(o) for o in (record.get("orders") or []) if isinstance(o, dict)]
    rt.escalation = record.get("escalation")
    # The request-id sequence resumes where the snapshot left it -- see
    # `_persist_park`'s own `req_seq`. A snapshot older than that key
    # falls back to the id the persisted escalation itself carries, which
    # is exactly the value the counter held when it was issued; neither
    # present (an escalation from before request ids existed at all)
    # leaves the freshly-initialised 0.
    seq = record.get("req_seq")
    if seq is None:
        seq = _request_seq(rt.escalation)
    if isinstance(seq, int):
        rt._req_seq = seq
    # `_persist_park` writes the block itself (not just its id) so a
    # restart can restore both without re-deriving one from the other by
    # guesswork; `warrant_id` is still recomputed here, not read off a
    # persisted copy, the same "never trust a stale derived value" rule
    # `context_str` gets in `_choice_scoring_context`'s own docstring.
    rt.warrant = record.get("warrant")
    rt.warrant_id = warrant_mod.warrant_id(rt.warrant) if rt.warrant is not None else None
    # A duration survives the restart intact; a raw `time.monotonic()`
    # reading from a process that no longer exists does not -- see
    # `_persist_park`'s own docstring. Anchoring `start_mono` like this
    # makes `time.monotonic() - rt.start_mono` read `elapsed_before_park`
    # again right now, so the wall budget resumes exactly where the walk
    # left off rather than charging the run for however long it sat
    # parked plus however long the service was down.
    rt.start_mono = time.monotonic() - float(record.get("elapsed_before_park") or 0.0)
    rt.start_wall = record.get("start_wall") or time.time()
    return rt


def _run_from_snapshot(
    record: dict,
    *,
    graphs_dir: Path,
    chooser: Chooser,
    entail: Entailer | None,
    option_tokens: int,
    context_tokens: int,
) -> Run:
    """The escalate-orphan half of `_load_orphan`: `record`'s own `kind`
    is still `"escalate"`, so nothing has answered this park since it was
    written and this run genuinely reconstructs. Builds the `_RunState`
    and the `_interpret_from_park` generator, wires them into a `Run` the
    same shape `start()` itself builds (bypassing `Run.__init__`, which
    always builds a *fresh* `_RunState`/`_interpret` for a graph's
    `initial` state -- there is no variant of it this call could use
    instead), and primes it with the same `begin()` every run primes with
    -- which re-yields the persisted escalation and settles `_pending`,
    so `Run.answer` needs no reconstruction-specific case of its own at
    all."""
    rt = _restore_run_state(
        record, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
        option_tokens=option_tokens, context_tokens=context_tokens,
    )
    run = Run.__new__(Run)
    run.id = rt.id
    run.state = rt
    run._gen = _interpret_from_park(rt)
    run._pending = None
    run._last_public = None
    run.finished = False
    run._lock = threading.Lock()
    run.begin()
    return run


def _run_from_lost_record(
    record: dict, *, graphs_dir: Path, option_tokens: int, context_tokens: int
) -> Run:
    """The act-orphan half of `_load_orphan`: `record`'s own `kind` is
    `"resolved"` (or, in principle, anything else `_persist_park`/
    `_persist_resolved` did not write) -- this run answered a park at
    some point and this process has no record of what happened next. It
    may have reached a final state cleanly; it may have died again
    mid-`act`. This cannot tell which, and does not guess: it returns an
    already-`finished` `Run`, outcome `"orphaned"`, reporting the last
    position this process actually knows (`active`, `counts`, `path_log`,
    as of that last resolved park) rather than anything past it. No
    `_interpret_from_park` here -- there is nothing this deep a
    reconstruction could resume *to*."""
    graph_ref = record.get("graph_ref")
    graph = load_graph_ref(graph_ref, graphs_dir, option_tokens, context_tokens)
    rt = _RunState(record["run"], graph, {}, None, None, record.get("ckpt_id"), graph_ref)
    rt.active = _decode_path(record.get("active") or "")
    # Merged onto the freshly-initialised defaults, not replaced by the
    # record outright: a park (or lost-record) persisted before a new
    # counter key existed (`duplicates_collapsed`/`rule_picks`, this step)
    # must not come back missing it -- `rt.counts[...] += 1` downstream, or
    # `_final_report`/`_status_dict` simply reading the key, would KeyError
    # on a dict a restore silently narrowed. A record already carrying
    # every current key overlays them all, a no-op change from the old
    # `dict(record.get("counts") or rt.counts)`.
    rt.counts = {**rt.counts, **(record.get("counts") or {})}
    rt.decisions = copy.deepcopy(record.get("decisions", []))
    rt.transitions = copy.deepcopy(record.get("transitions", []))
    rt.trace = copy.deepcopy(record.get("trace", []))
    rt.trace_sequence = record.get("trace_sequence", len(rt.trace))
    rt.path_log = list(record.get("path_log") or [])
    rt.outcome = "orphaned"
    rt.lost = (
        f"run {record['run']!r} answered an escalation and this process has no further record of it -- "
        f"it may have reached a final state normally, or it may have been suspended on an action (or "
        f"parked again) when the service restarted. Its position past step {rt.counts.get('steps', 0)} "
        f"at {'.'.join(rt.active) or '<root>'!r}, as of that last answer, cannot be reconstructed further: "
        "an `act` suspension's frame depth is not serialisable (this module's own docstring). "
        "automation.stop closes this run out; automation.start begins a new one."
    )
    run = Run.__new__(Run)
    run.id = rt.id
    run.state = rt
    run._gen = None  # never read: `finished=True` short-circuits every Run method first
    run._pending = None
    run.finished = True
    run._lock = threading.Lock()
    run._last_public = _final_report(rt)
    return run


def _load_orphan(
    run_id: str,
    *,
    graphs_dir: Path,
    chooser: Chooser,
    entail: Entailer | None,
    option_tokens: int,
    context_tokens: int,
) -> Run | None:
    """The read side of `_persist_park`/`_persist_resolved`: the one place
    `_no_such_run_message` must not be the last word for an id this
    process has forgotten but an earlier one wrote to disk. `None` when
    nothing was ever written for `run_id` at all -- an id that really was
    never known, or one that was, but suspended on an `act` and crashed
    before ever parking once -- in which case the caller's existing
    `_no_such_run_message` is not a lie, and this function has nothing
    further to say. Otherwise dispatches on the *last* record in the
    run's own file: `"escalate"` reconstructs a genuinely resumable `Run`;
    anything else means the run moved past its last park and this process
    lost track of it -- see `_run_from_snapshot`/`_run_from_lost_record`.
    """
    record = _read_last_record(run_id)
    if record is None:
        return None
    if record.get("kind") == "escalate":
        return _run_from_snapshot(
            record, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
            option_tokens=option_tokens, context_tokens=context_tokens,
        )
    return _run_from_lost_record(record, graphs_dir=graphs_dir, option_tokens=option_tokens, context_tokens=context_tokens)


def _lookup(
    run_id: str,
    *,
    graphs_dir: Path | None = None,
    chooser: Chooser | None = None,
    entail: Entailer | None = None,
    option_tokens: int = 32,
    context_tokens: int = 192,
) -> Run:
    """Every entry point below that needs a `Run` by id goes through here,
    not `_RUNS.get` directly, so `step`/`answer`/`status`/`stop_run` all
    get the same fallback: this process's own memory first (unchanged,
    the only thing consulted before this task), then -- only when the
    caller supplies enough to reload a graph and score with, which
    `server.py` always does and this module's own pre-persistence tests
    never did and still do not need to -- whatever `_load_orphan` finds on
    disk. `_RUNS.setdefault` on a successful reconstruction, not a plain
    assignment: two callers racing to look up the same just-restarted id
    each build their own throwaway `Run` from the same file, and only one
    may win a permanent place in `_RUNS` -- otherwise a second `jev_resume`
    for the same run could reach a *different* reconstructed generator
    than the first and double-fire the same pick. Raises
    `_no_such_run_message` only once neither has anything."""
    with _RUNS_LOCK:
        run = _RUNS.get(run_id)
    if run is not None:
        return run
    if graphs_dir is not None and chooser is not None:
        loaded = _load_orphan(
            run_id, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
            option_tokens=option_tokens, context_tokens=context_tokens,
        )
        if loaded is not None:
            with _RUNS_LOCK:
                run = _RUNS.setdefault(run_id, loaded)
            return run
    raise ValueError(_no_such_run_message(run_id))


def step(
    run_id: str,
    request_id: str,
    result: dict | None,
    *,
    graphs_dir: Path | None = None,
    chooser: Chooser | None = None,
    entail: Entailer | None = None,
    option_tokens: int = 32,
    context_tokens: int = 192,
) -> dict:
    """`automation.step`: `{run, request, result}` -> `{request}`. The
    `graphs_dir`/`chooser`/`entail`/`option_tokens`/`context_tokens`
    keywords enable orphan reconstruction via `_lookup` (see its
    docstring) and change nothing when omitted -- every call in this
    module's own pre-persistence tests omits them, and behaves exactly as
    it always did: a pure `_RUNS` lookup, `_no_such_run_message` on a
    miss. In practice a `step` against a reconstructed run is always a
    no-op (`Run.resume` only ever answers a pending `act`, and a
    reconstructed run's pending request, if any, is an `escalate`), the
    same idempotent shape `IdempotencyTests` already covers for a live
    parked run -- reconstruction is wired in here anyway, for the same
    reason `answer`/`status`/`stop_run` all are: so a caller need not
    already know which of the four it is about to ask before it asks."""
    run = _lookup(
        run_id, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
        option_tokens=option_tokens, context_tokens=context_tokens,
    )
    return {"request": run.resume(request_id, result or {})}


def answer(
    run_id: str,
    *,
    pick=None,
    stop=None,
    by: str | None = None,
    note: str | None = None,
    warrant: dict | None = None,
    request: str | None = None,
    graphs_dir: Path | None = None,
    chooser: Chooser | None = None,
    entail: Entailer | None = None,
    option_tokens: int = 32,
    context_tokens: int = 192,
) -> dict:
    """`automation.answer`: `{run, pick|stop, by, note, warrant, request}`
    -> `{request}`. `request` is optional and legacy-compatible: omitted
    (as every caller before it existed does), the answer applies to
    whatever is parked, exactly as before; given, it must be the pending
    escalation's own `request` id -- see `Run.answer`, which refuses a
    stale one without disturbing the park.
    `note` is accepted, per the design's own table, but this build has
    nowhere specified to keep it -- section 5's decision-log row shape is
    fixed and has no slot for a free-text note on an answer, unlike `by`,
    which becomes the row's `source`/`verified` (`Run.answer`). It is
    simply not dropped on the floor loudly; it is also not silently made
    up a home it was never given.

    `graphs_dir`/`chooser`/`entail`/`option_tokens`/`context_tokens`: see
    `step`'s own docstring -- the same opt-in orphan reconstruction, and
    this is the one call where it matters most: an escalate-suspended run
    genuinely resumes here, `Run.answer`'s own pick-vs-menu validation
    completely unchanged, because a reconstructed `Run` is the exact same
    class as a live one."""
    run = _lookup(
        run_id, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
        option_tokens=option_tokens, context_tokens=context_tokens,
    )
    return {"request": run.answer(pick=pick, stop=stop, by=by, note=note, warrant=warrant, request=request)}


def stop_run(
    run_id: str,
    reason: str,
    *,
    graphs_dir: Path | None = None,
    chooser: Chooser | None = None,
    entail: Entailer | None = None,
    option_tokens: int = 32,
    context_tokens: int = 192,
) -> dict:
    """`automation.stop`: `{run, reason}` -> the run record -- per the
    design's contract table, `.stop` joins `.status`/`.runs` in answering
    with the run record, not `{request}`: stopping never leaves a request
    pending, so there is nothing for that shape to carry. `Run.stop` is
    called here for effect only (it ends the run, wherever it currently
    is); its own return value -- the same settled act/escalate/final shape
    every other `Run` method returns -- is discarded in favour of
    `_status_dict`, which is what actually answers this call.

    `graphs_dir`/`chooser`/`entail`/`option_tokens`/`context_tokens`: see
    `step`'s own docstring. Stopping an act-orphan (already `finished`,
    outcome `"orphaned"`) is the idempotent no-op every other already-
    finished run already gives `stop_run` -- there is nothing left to
    interrupt, which is exactly what "report what was lost, and stop"
    means for that kind."""
    run = _lookup(
        run_id, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
        option_tokens=option_tokens, context_tokens=context_tokens,
    )
    run.stop(reason)
    return _status_dict(run_id, run)


def order(
    run_id: str,
    text: str,
    order_id: str,
    *,
    graphs_dir: Path | None = None,
    chooser: Chooser | None = None,
    entail: Entailer | None = None,
    option_tokens: int = 32,
    context_tokens: int = 192,
) -> dict:
    """`automation.order`: `{run, text, id}` -> `{run, order}`. Queues a
    standing order on a run that already exists -- see `Run.order` for the
    validation, the idempotent `id`, the empty-text clear and the
    supersede-and-journal rules, all of which live there.

    `id` is required and nonempty: it is the idempotency key, not a label a
    caller may omit, because the retry it exists for (a dropped connection
    around an order that did land) is exactly the case where a caller cannot
    re-read the run to find out.

    `graphs_dir`/`chooser`/`entail`/`option_tokens`/`context_tokens`: see
    `step`'s own docstring -- the same opt-in orphan reconstruction, so an
    order may be queued on a parked run this process no longer holds in
    memory but a previous one snapshotted. A run nothing ever knew raises
    `_no_such_run_message` from `_lookup` (never an implicit new run), and
    one whose last snapshot shows it moved past its park reconstructs as
    already-`finished`, which `Run.order` refuses with `ValueError` -- the
    same "reject a finished or stale run" answer either way."""
    run = _lookup(
        run_id, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
        option_tokens=option_tokens, context_tokens=context_tokens,
    )
    return run.order(text, order_id)


def _status_dict(run_id: str, run: Run) -> dict:
    """The run record `.status`, `.stop` and `.runs` all answer with.
    `counts`/`active`/`outcome`/`finished` describe a live run completely;
    a finished one gets whichever further field its own outcome makes
    meaningful -- the same fields `_final_report` itself would have
    carried at the moment it finished, so a `.status` call after the fact
    is not missing anything a driver watching the original `{request}`
    would have seen.

    S33 (docs/Tasklist.md): a *parked* run is neither "live and silent" nor
    "finished" -- `rt.outcome` is still `None` (none of the `elif` branches
    below fire) and `run.finished` is `False`, so before this the only
    thing `.status`/`.stop`/`.runs` said about a parked run was that it
    exists, at some state, some step: not the one thing an operator
    actually needs from that view, the question it is parked on. `rt.escalation`
    already holds it -- the exact payload `_park`/`_choose_phase` yielded
    (`_floor_escalation`/`_empty_escalation`), `None` once answered or
    stopped (`_park`/`_resume_parked_choice` both clear it before
    resuming) -- so this serialises it verbatim, the same "verbatim, not
    reconstructed" rule `_persist_park` already applies to the identical
    payload for the exact same reason. A sibling gap in this same
    function, closed earlier the same day: the `error` branch below gained
    `record["reason"]` so a finished run polled after the fact does not
    drop it; this is that class of omission, read before adding to it,
    not past it."""
    rt = run.state
    record = {
        "run": run_id,
        # `automation.order`'s bounded history, verbatim -- the same
        # "verbatim, not reconstructed" rule `escalation` above follows, so
        # whatever status each order reached (pending, applied, superseded,
        # cleared) is what an operator reads back. `_restore_run_state`
        # normalises a snapshot written before this key existed to `[]`, and
        # `_normalize_order` fills in any per-entry field such a snapshot
        # would be missing (see both).
        "orders": copy.deepcopy(rt.orders),
        "graph": _graph_ref(rt.graph),
        "active": ".".join(rt.active),
        "outcome": rt.outcome,
        "finished": run.finished,
        "counts": dict(rt.counts),
        "decisions": copy.deepcopy(rt.decisions),
        "transitions": copy.deepcopy(rt.transitions),
        "trace": copy.deepcopy(rt.trace),
        "trace_sequence": rt.trace_sequence,
        "warrant": _warrant_field(rt),
    }
    if rt.escalation is not None:
        record["escalation"] = rt.escalation
    if rt.outcome == "reached":
        record["final_state"] = rt.final_state
        record["output"] = rt.output
    elif rt.outcome == "exhausted":
        record["exhausted"] = rt.exhausted
    elif rt.outcome == "stopped":
        record["reason"] = rt.stop_reason
    elif rt.outcome == "error":
        record["error"] = rt.error
        record["reason"] = rt.error_reason
    elif rt.outcome == "orphaned":
        record["lost"] = rt.lost
    return record


def status(
    run_id: str,
    *,
    graphs_dir: Path | None = None,
    chooser: Chooser | None = None,
    entail: Entailer | None = None,
    option_tokens: int = 32,
    context_tokens: int = 192,
) -> dict:
    """`automation.status`: a snapshot of a run that does not consume
    anything -- pending `act` or `escalate` request untouched either way.

    `graphs_dir`/`chooser`/`entail`/`option_tokens`/`context_tokens`: see
    `step`'s own docstring. For an escalate-orphan this is the cheapest
    way to confirm a reconstruction landed correctly without touching the
    one pending answer at all; for an act-orphan it is the whole of
    "report what was lost"."""
    run = _lookup(
        run_id, graphs_dir=graphs_dir, chooser=chooser, entail=entail,
        option_tokens=option_tokens, context_tokens=context_tokens,
    )
    return _status_dict(run_id, run)


def list_runs() -> dict:
    """`automation.runs`: every run this process has tracked since it last
    started -- see this module's docstring, "Escalate-parked runs survive
    a service restart; `act`-suspended ones do not", for why "tracked"
    still means only what this process itself has in `_RUNS`, not
    everything a file on disk could answer for: reconstruction is lazy, on
    the first `step`/`answer`/`status`/`stop_run` that actually names a
    given id (`_lookup`), never a startup scan, so a run nobody has asked
    about yet is genuinely absent here even though its file already
    exists. No filtering by outcome or age: there is no eviction here
    either (pre-existing, not new to this build), so a listing that hid
    finished runs would not actually bound anything, only hide it."""
    with _RUNS_LOCK:
        snapshot = list(_RUNS.items())
    return {"runs": [_status_dict(run_id, run) for run_id, run in snapshot]}

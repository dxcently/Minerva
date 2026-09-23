"""graph.py -- docs/design/automation.md section 1: load, validate, lint.

No `jsonschema` package is installed in the jevlike venv this service runs
under (checked directly against
`C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv`), so this is a
hand-rolled validator shaped specifically to `schema.json` -- not a general
draft-2020-12 engine. It covers every constraint that schema actually uses
(required keys, the enum sets, `additionalProperties: false`, the
name/event/path patterns, and each guard/action kind's own required
parameters) and always collects every error it finds in one pass rather
than stopping at the first, because a graph author fixing errors one at a
time against a dozen is the failure mode a lint step exists to avoid.

Three lints run after shape validation, because a schema cannot see across
keys (section 1, the paragraph just above "Worked example: wiki-hop"):

  - every `target` (on `always`, `on`, `onDone`, and the root's own `on`)
    resolves to a real state;
  - a state whose `meta.choose.from` is not the literal `"transitions"`
    handles `PICK` in its `on`; one whose source *is* `"transitions"` has
    at least two non-reserved events in `on`;
  - every `menu`/`also` item's `label`, in UTF-8 bytes, fits the loaded
    checkpoint's `option_tokens` (a label the chooser cannot see the end of
    is one it cannot learn from) -- checked against the literal text in the
    graph, since `choose.label` can override it with a runtime template no
    load-time check can evaluate.

Two more, needed for the same "a schema cannot see across keys" reason:
`initial` (the graph's own, and every compound state's) must name a real
child; a `history` state must live inside some other state's `states` map,
never the graph's own top-level one -- nothing ever "re-enters" the root,
so a root-level history state names nothing meaningful to remember history
*for*.

One more, needed because a schema cannot see *behaviour*: an `always`
entry that is unconditional (no `guard`), does no observable work (no
`tool` action), and cannot actually leave the state (no `target`, or a
`target` back to itself with `reenter` unset) is rejected -- section 1's
re-entry rule ("a transition with no target is internal... nothing else
happens"; a same-state transition without `reenter` re-enters nothing
either) means such an entry re-runs and counts nothing, so it can only
spin, bounded solely by `wall_s`, while every budget counter that would
otherwise reveal a hang (`steps`, `actions`, `visits`) sits still. A
guarded self-transition is fine (the guard is what stops it); a
`reenter: true` self-transition is fine (bounded by `visits`); one with a
`tool` action is fine (real work, a real budget-counted action, a fresh
observation next time around).
"""
from __future__ import annotations

import hashlib
import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from urllib.parse import urlsplit

from . import guards
from . import warrant as warrant_mod

ID_RE_SRC = r"^[a-z][a-z0-9-]{0,63}$"
NAME_RE_SRC = r"^[a-z][a-zA-Z0-9_-]{0,63}$"
EVENT_RE_SRC = r"^[A-Z][A-Z0-9_]{0,31}$"
PATH_RE_SRC = r"^(context|input|event|run)(\.[A-Za-z0-9_-]+)*$"

ID_RE = re.compile(ID_RE_SRC)
NAME_RE = re.compile(NAME_RE_SRC)
EVENT_RE = re.compile(EVENT_RE_SRC)
PATH_RE = re.compile(PATH_RE_SRC)

# `option` is a fifth path root, valid only inside `choose.prefer`
# (docs/design/judgement.md section 2.2) -- everywhere else a guard's
# `path` is checked against the plain four-root `PATH_RE` above.
# `_validate_guard` below picks between the two by its `roots` keyword
# rather than building a pattern per call, and uses the option-admitting
# one a second time, defensively, to tell "not a path at all" apart from
# "an option.* path outside the one place it is allowed" so that second
# case gets its own, more useful message.
PATH_ROOTS = frozenset({"context", "input", "event", "run"})
PATH_ROOTS_WITH_OPTION = PATH_ROOTS | {"option"}
PATH_RE_WITH_OPTION_SRC = r"^(context|input|event|run|option)(\.[A-Za-z0-9_-]+)*$"
PATH_RE_WITH_OPTION = re.compile(PATH_RE_WITH_OPTION_SRC)

RESERVED_EVENTS = frozenset({"PICK", "EMPTY", "ERROR"})

STATE_TYPES = frozenset({"atomic", "compound", "final", "history"})
LANDMARK_ROLES = frozenset(
    {"banner", "navigation", "main", "complementary", "contentinfo", "search", "region"}
)


class GraphError(Exception):
    """Raised by `load_graph` with every error found, not just the first.
    `str(e)` joins them with `"; "`; `e.errors` is the list, each entry
    already naming the JSON path it is about (`states.reading.on.PICK`)."""

    def __init__(self, errors: list[str]):
        self.errors = list(errors)
        super().__init__("; ".join(self.errors))


@dataclass
class Graph:
    id: str
    version: str | None
    description: str | None
    initial: str
    context: dict
    states: dict
    root_on: dict
    jev: dict
    paths: dict[tuple[str, ...], dict]
    option_tokens: int
    context_tokens: int
    # docs/design/unattended.md section 2: the graph document's own content
    # hash, canonical JSON, never file bytes (so CRLF and LF checkouts hash
    # alike) -- computed once in `load_graph` over the same `json.dumps(...,
    # sort_keys=True, separators=(",", ":"), ensure_ascii=False)` shape
    # `warrant.canonical_json` uses for a warrant block, since both are just
    # "the canonical serialisation of a JSON-shaped Python object" and
    # `json.dumps`'s recursion does not care that a graph document is a
    # deeper, differently-shaped tree than a seven-key block. `warrant.check`
    # verifies a caller's `sha256` against this field before a warranted run
    # is ever allowed to start.
    sha256: str
    # docs/design/judgement.md section 2.2: every state's dotted path
    # (`"states.reading"`) whose `meta.choose.prefer` contains an NLI leaf
    # (`entails`/`contradicts`, direct or nested under `not`/`and`/`or`) --
    # admitted by the grammar, not refused, but costed: `run.py`'s `start()`
    # reads this to append one warning per state to a fresh run's
    # `rt.warnings`, naming the state and "one entail pair per option per
    # visit" (`first_preferred` pays that cost once per option per visit,
    # never batched the way `guards.first_passing` batches a state's `on`).
    # Populated by `_lint_choose`, after `_build_graph` has already
    # returned this `Graph` -- a plain mutable field, not a lint error,
    # because an NLI rule in `prefer` is a cost to know about, not a shape
    # mistake to refuse.
    prefer_nli_states: list[str]

    def resolve_target(self, from_path: tuple[str, ...], target: str) -> tuple[str, ...] | None:
        """`target` is either `#id.a.b` (absolute) or a bare name, resolved
        as a sibling of the state at `from_path` -- i.e. relative to that
        state's *parent*. This is deliberately narrower than every target
        form XState itself allows (a dotted relative path with no `#` is
        not supported); anything this cannot resolve is a lint error naming
        both ends rather than a silent mis-navigation at run time."""
        if target.startswith("#"):
            parts = tuple(p for p in target[1:].split(".") if p != "")
            if not parts or parts[0] != self.id:
                return None
            path = parts[1:]
        else:
            path = from_path[:-1] + (target,)
        return path if path in self.paths else None

    def state_at(self, path: tuple[str, ...]) -> dict:
        return self.paths[path]

    def is_compound(self, path: tuple[str, ...]) -> bool:
        state = self.paths[path]
        return isinstance(state.get("states"), dict) and bool(state["states"])

    def default(self, key: str, fallback):
        return (self.jev.get("defaults") or {}).get(key, fallback)

    def budget(self, key: str, fallback):
        return (self.jev.get("budget") or {}).get(key, fallback)


def load_graph(
    source: dict | str | Path,
    *,
    option_tokens: int = 32,
    context_tokens: int = 192,
) -> Graph:
    """`source` is a parsed graph object, a JSON string, or a path to a
    `.json` file. Raises `GraphError` with every shape and lint problem
    found; returns a `Graph` only when both passes are clean."""
    doc = _load_doc(source)
    errors: list[str] = []
    _validate_shape(doc, errors)
    if errors:
        raise GraphError(errors)
    graph = _build_graph(doc, option_tokens, context_tokens)
    _lint(graph, errors)
    if errors:
        raise GraphError(errors)
    return graph


def _load_doc(source: dict | str | Path) -> dict:
    if isinstance(source, dict):
        return source
    if isinstance(source, Path):
        text = source.read_text(encoding="utf-8")
    elif isinstance(source, str) and source.lstrip()[:1] in "{[":
        text = source
    else:
        text = Path(source).read_text(encoding="utf-8")
    try:
        return json.loads(text)
    except json.JSONDecodeError as e:
        raise GraphError([f"invalid JSON: {e}"]) from e


# --- shape validation --------------------------------------------------------

_TOP_KEYS = {"id", "version", "description", "initial", "context", "states", "on", "meta"}
_STATE_KEYS = {
    "type", "description", "initial", "states", "history", "target",
    "entry", "exit", "always", "on", "onDone", "output", "meta",
}
_TRANSITION_KEYS = {"target", "reenter", "guard", "actions", "description", "meta"}
_STATE_META_KEYS = {"choose", "floor", "margin", "visits", "ask", "editor"}
_CHOOSE_KEYS = {"from", "also", "exclude", "max", "context", "label", "prefer"}
_REFS_KEYS = {"of", "roles", "within", "named", "url"}
_MENU_ITEM_REQUIRED = {"label"}
_LINES_KEYS = {"of", "skip"}
_JEV_KEYS = {"schema", "requires", "input", "defaults", "budget", "warrant"}
_DEFAULTS_KEYS = {"floor", "margin", "visits", "threshold", "max"}
_BUDGET_KEYS = {"steps", "actions", "wall_s", "escalations"}
_WARRANT_KEYS = {"tools", "origins", "commands"}
_TOOL_NAME_RE = re.compile(r"^[a-z][a-z0-9_]*$")
_ORIGIN_RE = re.compile(r"^https?://[a-z0-9.\-]+(:\d+)?$")


def _unknown_keys(obj: dict, allowed: set[str], where: str, errors: list[str]) -> None:
    for key in obj:
        if key not in allowed:
            errors.append(f"{where}: unknown key {key!r}")


def _require(obj: dict, keys: set[str] | list[str], where: str, errors: list[str]) -> None:
    for key in keys:
        if key not in obj:
            errors.append(f"{where}: missing required key {key!r}")


def _check_number(value, lo, hi, where: str, errors: list[str]) -> None:
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        errors.append(f"{where}: must be a number")
        return
    if not (lo <= value <= hi):
        errors.append(f"{where}: must be between {lo} and {hi}")


def _check_int(value, lo, where: str, errors: list[str]) -> None:
    if not isinstance(value, int) or isinstance(value, bool):
        errors.append(f"{where}: must be an integer")
        return
    if value < lo:
        errors.append(f"{where}: must be >= {lo}")


def _validate_shape(doc: Any, errors: list[str]) -> None:
    if not isinstance(doc, dict):
        errors.append("graph: must be a JSON object")
        return
    _unknown_keys(doc, _TOP_KEYS, "graph", errors)
    _require(doc, {"id", "initial", "states", "meta"}, "graph", errors)
    if "id" in doc and not (isinstance(doc["id"], str) and ID_RE.match(doc["id"])):
        errors.append(f"graph.id: {doc.get('id')!r} does not match {ID_RE_SRC}")
    for key in ("version", "description"):
        if key in doc and not isinstance(doc[key], str):
            errors.append(f"graph.{key}: must be a string")
    if "initial" in doc and not (isinstance(doc["initial"], str) and NAME_RE.match(doc["initial"])):
        errors.append(f"graph.initial: {doc.get('initial')!r} does not match {NAME_RE_SRC}")
    if "context" in doc and not isinstance(doc["context"], dict):
        errors.append("graph.context: must be an object")
    if "states" in doc:
        if not isinstance(doc["states"], dict) or not doc["states"]:
            errors.append("graph.states: must be a non-empty object")
        else:
            for name, state in doc["states"].items():
                if not NAME_RE.match(name):
                    errors.append(f"states.{name}: state name does not match {NAME_RE_SRC}")
                _validate_state(state, f"states.{name}", errors)
    if "on" in doc:
        _validate_transitions(doc["on"], "graph.on", errors)
    if "meta" in doc:
        _validate_meta(doc["meta"], "graph.meta", errors)


def _validate_meta(meta: Any, where: str, errors: list[str]) -> None:
    if not isinstance(meta, dict):
        errors.append(f"{where}: must be an object")
        return
    _require(meta, {"jev"}, where, errors)
    if "editor" in meta and not isinstance(meta["editor"], dict):
        errors.append(f"{where}.editor: must be an object")
    if "jev" in meta:
        _validate_jev(meta["jev"], f"{where}.jev", errors)


def _validate_jev(jev: Any, where: str, errors: list[str]) -> None:
    if not isinstance(jev, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(jev, _JEV_KEYS, where, errors)
    _require(jev, {"schema"}, where, errors)
    if "schema" in jev and jev["schema"] != 1:
        errors.append(f"{where}.schema: must be 1")
    if "requires" in jev:
        if not isinstance(jev["requires"], list) or not all(isinstance(x, str) for x in jev["requires"]):
            errors.append(f"{where}.requires: must be an array of strings")
    if "input" in jev and not isinstance(jev["input"], dict):
        errors.append(f"{where}.input: must be an object")
    if "defaults" in jev:
        defaults = jev["defaults"]
        if not isinstance(defaults, dict):
            errors.append(f"{where}.defaults: must be an object")
        else:
            _unknown_keys(defaults, _DEFAULTS_KEYS, f"{where}.defaults", errors)
            for key in ("floor", "margin"):
                if key in defaults:
                    _check_number(defaults[key], 0, 1, f"{where}.defaults.{key}", errors)
            if "visits" in defaults:
                _check_int(defaults["visits"], 1, f"{where}.defaults.visits", errors)
            if "threshold" in defaults:
                _check_number(defaults["threshold"], 0.34, 1, f"{where}.defaults.threshold", errors)
            if "max" in defaults:
                _check_int(defaults["max"], 2, f"{where}.defaults.max", errors)
    if "budget" in jev:
        budget = jev["budget"]
        if not isinstance(budget, dict):
            errors.append(f"{where}.budget: must be an object")
        else:
            _unknown_keys(budget, _BUDGET_KEYS, f"{where}.budget", errors)
            for key in ("steps", "actions", "wall_s"):
                if key in budget:
                    _check_int(budget[key], 1, f"{where}.budget.{key}", errors)
            if "escalations" in budget:
                _check_int(budget["escalations"], 0, f"{where}.budget.escalations", errors)
    if "warrant" in jev:
        _validate_warrant(jev["warrant"], f"{where}.warrant", errors)


def _validate_warrant(value: Any, where: str, errors: list[str]) -> None:
    """docs/design/unattended.md section 2: the seven-key envelope's
    *declared* half -- `tools` (required) plus the optional `origins`/
    `commands` a graph author writes at `meta.jev.warrant`; the other four
    keys (`graph`, `sha256`, `actions`, `wall_s`) are the service's own to
    fill in (`warrant.canonical`), never authored here. `origins`/
    `commands` are restricted to printable ASCII by construction -- the
    same "so the two serialisers [Python, Rust] cannot disagree on any
    block a graph can produce" reasoning the design doc states for why the
    lint admits only printable ASCII there at all."""
    if not isinstance(value, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(value, _WARRANT_KEYS, where, errors)
    _require(value, {"tools"}, where, errors)
    if "tools" in value:
        tools = value["tools"]
        if not isinstance(tools, list) or not tools or not all(isinstance(t, str) for t in tools):
            errors.append(f"{where}.tools: must be a non-empty array of strings")
        else:
            bad = [t for t in tools if not _TOOL_NAME_RE.match(t)]
            if bad:
                errors.append(f"{where}.tools: {sorted(bad)} do not match ^[a-z][a-z0-9_]*$")
    if "origins" in value:
        origins = value["origins"]
        if not isinstance(origins, list) or not all(isinstance(o, str) for o in origins):
            errors.append(f"{where}.origins: must be an array of strings")
        else:
            bad = [o for o in origins if not _ORIGIN_RE.match(o)]
            if bad:
                errors.append(f"{where}.origins: {sorted(bad)} do not match ^https?://[a-z0-9.-]+(:\\d+)?$")
    if "commands" in value:
        commands = value["commands"]
        if not isinstance(commands, list) or not all(isinstance(c, str) for c in commands):
            errors.append(f"{where}.commands: must be an array of strings")
        else:
            bad = [c for c in commands if not (c and all(0x20 <= ord(ch) < 0x7F for ch in c))]
            if bad:
                errors.append(f"{where}.commands: {sorted(bad)} must be printable ASCII with no control characters")


def _validate_state(state: Any, where: str, errors: list[str]) -> None:
    if not isinstance(state, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(state, _STATE_KEYS, where, errors)
    state_type = state.get("type")
    if "type" in state and state_type not in STATE_TYPES:
        errors.append(f"{where}.type: {state_type!r} is not one of {sorted(STATE_TYPES)}")

    if state_type == "final":
        for key in ("states", "initial", "on", "always", "onDone"):
            if key in state:
                errors.append(f"{where}.{key}: not allowed on a final state")
    if state_type == "history":
        if state.get("history") != "shallow":
            errors.append(f"{where}.history: a history state requires \"history\": \"shallow\"")
        for key in ("states", "on", "always", "entry", "exit", "meta"):
            if key in state:
                errors.append(f"{where}.{key}: not allowed on a history state")
        # A history state past this point has nothing else to validate.
        return
    if "history" in state and state_type != "history":
        errors.append(f"{where}.history: only allowed when type is \"history\"")

    if "description" in state and not isinstance(state["description"], str):
        errors.append(f"{where}.description: must be a string")
    if "target" in state and not isinstance(state["target"], str):
        errors.append(f"{where}.target: must be a string")
    if "output" in state:
        if not isinstance(state["output"], dict):
            errors.append(f"{where}.output: must be an object")
        if state_type != "final":
            errors.append(f"{where}.output: only allowed on a final state (type must be \"final\")")

    if "states" in state:
        if not isinstance(state["states"], dict) or not state["states"]:
            errors.append(f"{where}.states: must be a non-empty object")
        else:
            if "initial" not in state:
                errors.append(f"{where}: has \"states\" but no \"initial\"")
            elif not (isinstance(state["initial"], str) and NAME_RE.match(state["initial"])):
                errors.append(f"{where}.initial: {state.get('initial')!r} does not match {NAME_RE_SRC}")
            for name, child in state["states"].items():
                if not NAME_RE.match(name):
                    errors.append(f"{where}.states.{name}: state name does not match {NAME_RE_SRC}")
                _validate_state(child, f"{where}.states.{name}", errors)
    elif "initial" in state:
        errors.append(f"{where}.initial: only allowed alongside \"states\"")

    for key in ("entry", "exit"):
        if key in state:
            _validate_actions(state[key], f"{where}.{key}", errors)
    if "always" in state:
        _validate_transition_or_list(state["always"], f"{where}.always", errors)
    if "on" in state:
        _validate_transitions(state["on"], f"{where}.on", errors)
    if "onDone" in state:
        _validate_transition_or_list(state["onDone"], f"{where}.onDone", errors)
    if "meta" in state:
        _validate_state_meta(state["meta"], f"{where}.meta", errors)


def _validate_state_meta(meta: Any, where: str, errors: list[str]) -> None:
    if not isinstance(meta, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(meta, _STATE_META_KEYS, where, errors)
    if "choose" in meta:
        _validate_choose(meta["choose"], f"{where}.choose", errors)
    for key in ("floor", "margin"):
        if key in meta:
            _check_number(meta[key], 0, 1, f"{where}.{key}", errors)
    if "visits" in meta:
        _check_int(meta["visits"], 1, f"{where}.visits", errors)
    if "ask" in meta and not isinstance(meta["ask"], str):
        errors.append(f"{where}.ask: must be a string")
    if "editor" in meta and not isinstance(meta["editor"], dict):
        errors.append(f"{where}.editor: must be an object")


def _validate_choose(choose: Any, where: str, errors: list[str]) -> None:
    if not isinstance(choose, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(choose, _CHOOSE_KEYS, where, errors)
    _require(choose, {"from"}, where, errors)
    if "from" in choose:
        _validate_choose_from(choose["from"], f"{where}.from", errors)
    if "also" in choose:
        _validate_menu(choose["also"], f"{where}.also", errors)
    if "exclude" in choose and not isinstance(choose["exclude"], str):
        errors.append(f"{where}.exclude: must be a string")
    if "max" in choose:
        _check_int(choose["max"], 2, f"{where}.max", errors)
    for key in ("context", "label"):
        if key in choose and not isinstance(choose[key], str):
            errors.append(f"{where}.{key}: must be a string")
    if "prefer" in choose:
        _validate_prefer(choose["prefer"], f"{where}.prefer", errors)


def _validate_prefer(value: Any, where: str, errors: list[str]) -> None:
    """docs/design/judgement.md section 2.2: `choose.prefer` is a non-empty
    ordered list of guards from the same closed grammar every other guard
    site uses, evaluated with `option.*` admitted into the path roots --
    the one place in the whole grammar that scope is valid at all
    (`_validate_guard`'s `roots` keyword; an `option.*` path anywhere else
    is refused by the same function's default four-root call)."""
    if not isinstance(value, list) or not value:
        errors.append(f"{where}: must be a non-empty array of guards")
        return
    for i, g in enumerate(value):
        _validate_guard(g, f"{where}[{i}]", errors, roots=PATH_ROOTS_WITH_OPTION)


def _validate_choose_from(value: Any, where: str, errors: list[str]) -> None:
    if value == "transitions":
        return
    if not isinstance(value, dict):
        errors.append(f'{where}: must be "transitions" or an object with one of refs/menu/lines')
        return
    sources = [k for k in ("refs", "menu", "lines") if k in value]
    extra = set(value) - {"refs", "menu", "lines"}
    if extra:
        errors.append(f"{where}: unknown key(s) {sorted(extra)}")
    if len(sources) != 1:
        errors.append(f"{where}: must have exactly one of refs, menu, lines (found {sources or 'none'})")
        return
    if "refs" in value:
        refs = value["refs"]
        if refs is not None:
            if not isinstance(refs, dict):
                errors.append(f"{where}.refs: must be an object")
            else:
                _unknown_keys(refs, _REFS_KEYS, f"{where}.refs", errors)
                if "of" in refs and not (isinstance(refs["of"], str) and PATH_RE.match(refs["of"])):
                    errors.append(f"{where}.refs.of: {refs.get('of')!r} does not match {PATH_RE_SRC}")
                if "roles" in refs and not (
                    isinstance(refs["roles"], list) and all(isinstance(r, str) for r in refs["roles"])
                ):
                    errors.append(f"{where}.refs.roles: must be an array of strings")
                if "within" in refs and not isinstance(refs["within"], str):
                    errors.append(f"{where}.refs.within: must be a string")
                if "named" in refs and not isinstance(refs["named"], bool):
                    errors.append(f"{where}.refs.named: must be a boolean")
                if "url" in refs:
                    if not isinstance(refs["url"], str):
                        errors.append(f"{where}.refs.url: must be a string")
                    else:
                        try:
                            re.compile(refs["url"])
                        except re.error as e:
                            errors.append(f"{where}.refs.url: not a valid regular expression: {e}")
    if "menu" in value:
        _validate_menu(value["menu"], f"{where}.menu", errors)
    if "lines" in value:
        lines = value["lines"]
        if lines is not None:
            if not isinstance(lines, dict):
                errors.append(f"{where}.lines: must be an object")
            else:
                _unknown_keys(lines, _LINES_KEYS, f"{where}.lines", errors)
                if "of" in lines and not (isinstance(lines["of"], str) and PATH_RE.match(lines["of"])):
                    errors.append(f"{where}.lines.of: {lines.get('of')!r} does not match {PATH_RE_SRC}")
                if "skip" in lines:
                    _check_int(lines["skip"], 0, f"{where}.lines.skip", errors)


def _validate_menu(value: Any, where: str, errors: list[str]) -> None:
    if not isinstance(value, list) or not value:
        errors.append(f"{where}: must be a non-empty array")
        return
    for i, item in enumerate(value):
        item_where = f"{where}[{i}]"
        if not isinstance(item, dict):
            errors.append(f"{item_where}: must be an object")
            continue
        _require(item, _MENU_ITEM_REQUIRED, item_where, errors)
        if "label" in item and not (isinstance(item["label"], str) and item["label"]):
            errors.append(f"{item_where}.label: must be a non-empty string")
        if "event" in item and not (isinstance(item["event"], str) and EVENT_RE.match(item["event"])):
            errors.append(f"{item_where}.event: {item.get('event')!r} does not match {EVENT_RE_SRC}")


def _validate_transitions(value: Any, where: str, errors: list[str]) -> None:
    if not isinstance(value, dict):
        errors.append(f"{where}: must be an object")
        return
    for event, entry in value.items():
        if not EVENT_RE.match(event):
            errors.append(f"{where}: event {event!r} does not match {EVENT_RE_SRC}")
        _validate_transition_or_list(entry, f"{where}.{event}", errors)


def _validate_transition_or_list(value: Any, where: str, errors: list[str]) -> None:
    if isinstance(value, str):
        return
    if isinstance(value, dict):
        _validate_transition(value, where, errors)
        return
    if isinstance(value, list):
        if not value:
            errors.append(f"{where}: must be a non-empty array")
            return
        for i, item in enumerate(value):
            if isinstance(item, str):
                continue
            if isinstance(item, dict):
                _validate_transition(item, f"{where}[{i}]", errors)
            else:
                errors.append(f"{where}[{i}]: must be a string or a transition object")
        return
    errors.append(f"{where}: must be a string, a transition object, or an array of either")


def _validate_transition(value: dict, where: str, errors: list[str]) -> None:
    _unknown_keys(value, _TRANSITION_KEYS, where, errors)
    if "target" in value and not isinstance(value["target"], str):
        errors.append(f"{where}.target: must be a string")
    if "reenter" in value and not isinstance(value["reenter"], bool):
        errors.append(f"{where}.reenter: must be a boolean")
    if "description" in value and not isinstance(value["description"], str):
        errors.append(f"{where}.description: must be a string")
    if "meta" in value and not isinstance(value["meta"], dict):
        errors.append(f"{where}.meta: must be an object")
    if "guard" in value:
        _validate_guard(value["guard"], f"{where}.guard", errors)
    if "actions" in value:
        _validate_actions(value["actions"], f"{where}.actions", errors)


_GUARD_PATH_KINDS = {"equals", "contains", "matches", "exists", "count"}


def _validate_guard(
    value: Any, where: str, errors: list[str], *, roots: frozenset[str] = PATH_ROOTS
) -> None:
    """`roots` is the set of path roots a `path` param may name here --
    the plain four (`context`/`input`/`event`/`run`) everywhere a guard is
    ever validated from, except inside `choose.prefer`
    (`_validate_prefer`, above), the one call site that passes
    `PATH_ROOTS_WITH_OPTION` -- and is threaded unchanged through every
    recursive call below (`not`/`and`/`or`), so an `option.*` path stays
    valid arbitrarily deep inside a `prefer` rule's own combinators and
    stays refused everywhere else, including inside a combinator nested
    under a transition's `guard` or a `tool` action's `expect`."""
    if not isinstance(value, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(value, {"type", "params"}, where, errors)
    _require(value, {"type", "params"}, where, errors)
    kind = value.get("type")
    if "type" in value and kind not in guards.GUARD_KINDS:
        errors.append(f"{where}.type: unknown guard type {kind!r}")
        return
    params = value.get("params")
    if "params" in value and not isinstance(params, dict):
        errors.append(f"{where}.params: must be an object")
        return
    if params is None:
        return
    pwhere = f"{where}.params"
    if kind in _GUARD_PATH_KINDS:
        _require(params, {"path"}, pwhere, errors)
        if "path" in params:
            path = params["path"]
            pattern = PATH_RE_WITH_OPTION if "option" in roots else PATH_RE
            if not (isinstance(path, str) and pattern.match(path)):
                if "option" not in roots and isinstance(path, str) and PATH_RE_WITH_OPTION.match(path):
                    # A path that would be valid under the five-root
                    # pattern but was checked against the plain four is,
                    # unambiguously, an `option.*` path outside `prefer` --
                    # worth its own message naming exactly where `option`
                    # *is* in scope, rather than the generic "does not
                    # match" every other bad path gets.
                    errors.append(f"{pwhere}.path: 'option' is only in scope inside choose.prefer")
                else:
                    errors.append(f"{pwhere}.path: {path!r} does not match {PATH_RE_SRC}")
    if kind == "equals":
        _unknown_keys(params, {"path", "value", "fold"}, pwhere, errors)
        _require(params, {"value"}, pwhere, errors)
        if "fold" in params and not isinstance(params["fold"], bool):
            errors.append(f"{pwhere}.fold: must be a boolean")
    elif kind == "contains":
        _unknown_keys(params, {"path", "value"}, pwhere, errors)
        _require(params, {"value"}, pwhere, errors)
        if "value" in params and not isinstance(params["value"], str):
            errors.append(f"{pwhere}.value: must be a string")
    elif kind == "matches":
        _unknown_keys(params, {"path", "pattern"}, pwhere, errors)
        _require(params, {"pattern"}, pwhere, errors)
        if "pattern" in params and not isinstance(params["pattern"], str):
            errors.append(f"{pwhere}.pattern: must be a string")
        elif "pattern" in params:
            try:
                re.compile(params["pattern"])
            except re.error as e:
                errors.append(f"{pwhere}.pattern: invalid regular expression: {e}")
    elif kind == "exists":
        _unknown_keys(params, {"path"}, pwhere, errors)
    elif kind == "count":
        _unknown_keys(params, {"path", "gte", "lte"}, pwhere, errors)
        for b in ("gte", "lte"):
            if b in params:
                _check_int(params[b], 0, f"{pwhere}.{b}", errors)
    elif kind in ("entails", "contradicts"):
        _validate_nli(params, pwhere, errors)
    elif kind == "not":
        _unknown_keys(params, {"guard"}, pwhere, errors)
        _require(params, {"guard"}, pwhere, errors)
        if "guard" in params:
            _validate_guard(params["guard"], f"{pwhere}.guard", errors, roots=roots)
    elif kind in ("and", "or"):
        _unknown_keys(params, {"guards"}, pwhere, errors)
        _require(params, {"guards"}, pwhere, errors)
        if "guards" in params:
            if not isinstance(params["guards"], list) or len(params["guards"]) < 2:
                errors.append(f"{pwhere}.guards: must be an array of at least 2 guards")
            else:
                for i, g in enumerate(params["guards"]):
                    _validate_guard(g, f"{pwhere}.guards[{i}]", errors, roots=roots)


def _validate_nli(params: Any, where: str, errors: list[str]) -> None:
    if not isinstance(params, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(params, {"premise", "hypothesis", "threshold", "window", "max_chars"}, where, errors)
    _require(params, {"hypothesis"}, where, errors)
    for key in ("premise", "hypothesis"):
        if key in params and not isinstance(params[key], str):
            errors.append(f"{where}.{key}: must be a string")
    if "threshold" in params:
        _check_number(params["threshold"], 0.34, 1, f"{where}.threshold", errors)
    if "window" in params and params["window"] not in ("head", "tail"):
        errors.append(f"{where}.window: must be \"head\" or \"tail\"")
    if "max_chars" in params:
        _check_int(params["max_chars"], 1, f"{where}.max_chars", errors)


def _validate_actions(value: Any, where: str, errors: list[str]) -> None:
    if not isinstance(value, list) or not value:
        errors.append(f"{where}: must be a non-empty array")
        return
    for i, action in enumerate(value):
        _validate_action(action, f"{where}[{i}]", errors)


_ACTION_KINDS = {"tool", "assign", "push", "inc", "capture"}


def _validate_action(value: Any, where: str, errors: list[str]) -> None:
    if not isinstance(value, dict):
        errors.append(f"{where}: must be an object")
        return
    _unknown_keys(value, {"type", "params"}, where, errors)
    _require(value, {"type", "params"}, where, errors)
    kind = value.get("type")
    if "type" in value and kind not in _ACTION_KINDS:
        errors.append(f"{where}.type: unknown action type {kind!r}")
        return
    params = value.get("params")
    if "params" in value and not isinstance(params, dict):
        errors.append(f"{where}.params: must be an object")
        return
    if params is None:
        return
    pwhere = f"{where}.params"
    if kind == "tool":
        _unknown_keys(params, {"name", "input", "into", "timeout_s", "expect"}, pwhere, errors)
        _require(params, {"name"}, pwhere, errors)
        if "name" in params and not (
            isinstance(params["name"], str) and re.match(r"^[a-z][a-z0-9_]*$", params["name"])
        ):
            errors.append(f"{pwhere}.name: {params.get('name')!r} does not match ^[a-z][a-z0-9_]*$")
        if "input" in params and not isinstance(params["input"], dict):
            errors.append(f"{pwhere}.input: must be an object")
        if "into" in params and not (
            isinstance(params["into"], str) and re.match(r"^[a-z][A-Za-z0-9_]*$", params["into"])
        ):
            errors.append(f"{pwhere}.into: {params.get('into')!r} does not match ^[a-z][A-Za-z0-9_]*$")
        if "timeout_s" in params:
            _check_int(params["timeout_s"], 1, f"{pwhere}.timeout_s", errors)
        if "expect" in params:
            # A guard from the same closed grammar every other guard site
            # uses (docs/design/observation.md section 4: "not a second
            # expression language") -- evaluated once, after a result is
            # believed whole and successful, before it is ever stored
            # (`run.py`'s `_run_tool_action`).
            _validate_guard(params["expect"], f"{pwhere}.expect", errors)
    elif kind == "assign":
        if not params:
            errors.append(f"{pwhere}: must have at least one key")
    elif kind == "push":
        _unknown_keys(params, {"path", "value"}, pwhere, errors)
        _require(params, {"path", "value"}, pwhere, errors)
        if "path" in params and not isinstance(params["path"], str):
            errors.append(f"{pwhere}.path: must be a string")
    elif kind == "inc":
        _unknown_keys(params, {"path", "by"}, pwhere, errors)
        _require(params, {"path"}, pwhere, errors)
        if "path" in params and not isinstance(params["path"], str):
            errors.append(f"{pwhere}.path: must be a string")
        if "by" in params and not isinstance(params["by"], int):
            errors.append(f"{pwhere}.by: must be an integer")
    elif kind == "capture":
        # docs/design/triage.md section 3.2: evidence is typed at the point
        # it is extracted, and `capture` is `matches` with groups -- the
        # same engine and flags, no second expression language. `path` is an
        # ordinary four-root `PATH_RE` path; a `lines` pick's chosen line is
        # reached as `event.option.line`, not by widening the roots (`option`
        # is a fifth root only inside `choose.prefer`).
        _unknown_keys(params, {"path", "pattern", "into"}, pwhere, errors)
        _require(params, {"path", "pattern", "into"}, pwhere, errors)
        if "path" in params and not (
            isinstance(params["path"], str) and PATH_RE.match(params["path"])
        ):
            errors.append(f"{pwhere}.path: {params.get('path')!r} does not match {PATH_RE_SRC}")
        if "pattern" in params:
            pattern = params["pattern"]
            if not isinstance(pattern, str):
                errors.append(f"{pwhere}.pattern: must be a string")
            else:
                try:
                    compiled = re.compile(pattern, re.MULTILINE)
                except re.error as e:
                    errors.append(f"{pwhere}.pattern: invalid regular expression: {e}")
                else:
                    # Two independent defects, each reported on its own: a
                    # capture with no named group is a `matches` (it would
                    # "succeed" and assign nothing), and one that can match
                    # the empty string "succeeds" at offset 0 with every
                    # group unparticipating, silently -- the empty-match
                    # defect M12 caught (triage.md section 3.2).
                    if not compiled.groupindex:
                        errors.append(
                            f"{pwhere}.pattern: has no named group -- a capture that captures "
                            "nothing is a `matches`"
                        )
                    if re.search(pattern, "") is not None:
                        errors.append(
                            f"{pwhere}.pattern: can match the empty string -- a capture that "
                            "can match nothing captures nothing silently; anchor it on a "
                            "required group"
                        )
        if "into" in params and not (
            isinstance(params["into"], str) and re.match(r"^[a-z][A-Za-z0-9_]*$", params["into"])
        ):
            errors.append(
                f"{pwhere}.into: {params.get('into')!r} does not match ^[a-z][A-Za-z0-9_]*$"
            )


# --- building the Graph -------------------------------------------------------


def _walk_states(states: dict, prefix: tuple[str, ...], out: dict[tuple[str, ...], dict]) -> None:
    for name, state in states.items():
        path = prefix + (name,)
        out[path] = state
        children = state.get("states") if isinstance(state, dict) else None
        if isinstance(children, dict):
            _walk_states(children, path, out)


def _document_sha256(doc: dict) -> str:
    """The graph document's content hash: SHA-256 over the *parsed*
    document's canonical JSON, never over the file's raw bytes, so a CRLF
    checkout and an LF checkout of the identical graph hash alike -- the
    same `warrant.canonical_json` a warrant block itself hashes with,
    since both are just "the canonical serialisation of a JSON-shaped
    Python object" (`json.dumps`'s recursion does not care that a graph
    document nests states/transitions/actions rather than seven flat
    keys). Kept as its own function, rather than inlined into
    `_build_graph`, only so `load_graph`'s docstring-promised "over the
    parsed document" claim has one obvious definition to point at."""
    return hashlib.sha256(warrant_mod.canonical_json(doc).encode("utf-8")).hexdigest()


def _build_graph(doc: dict, option_tokens: int, context_tokens: int) -> Graph:
    paths: dict[tuple[str, ...], dict] = {}
    _walk_states(doc["states"], (), paths)
    return Graph(
        id=doc["id"],
        version=doc.get("version"),
        description=doc.get("description"),
        initial=doc["initial"],
        context=doc.get("context") or {},
        states=doc["states"],
        root_on=doc.get("on") or {},
        jev=doc["meta"]["jev"],
        paths=paths,
        option_tokens=option_tokens,
        context_tokens=context_tokens,
        sha256=_document_sha256(doc),
        prefer_nli_states=[],  # filled in by _lint_choose, which runs after this returns
    )


# --- lint ----------------------------------------------------------------------


def _lint(graph: Graph, errors: list[str]) -> None:
    _lint_initial_children(graph, errors)
    _lint_history_placement(graph, errors)
    _lint_targets(graph, errors)
    _lint_choose(graph, errors)
    _lint_always_spin(graph, errors)
    _lint_warrant(graph, errors)


def _lint_initial_children(graph: Graph, errors: list[str]) -> None:
    if graph.initial not in graph.states:
        errors.append(f"graph.initial: {graph.initial!r} is not a top-level state")
    for path, state in graph.paths.items():
        if isinstance(state.get("states"), dict) and "initial" in state:
            if state["initial"] not in state["states"]:
                dotted = ".".join(path)
                errors.append(f"states.{dotted}.initial: {state['initial']!r} is not a child of this state")


def _lint_history_placement(graph: Graph, errors: list[str]) -> None:
    for name, state in graph.states.items():
        if isinstance(state, dict) and state.get("type") == "history":
            errors.append(
                f"states.{name}: a history state cannot live at the graph's top level "
                "(nothing ever re-enters the root); nest it inside the compound state "
                "it belongs to"
            )


def _iter_transitions(state_or_root: dict, kind: str) -> list[tuple[str | None, dict]]:
    """Normalise `always`/`on`/`onDone` into `(event_or_None, transition)`
    pairs, `event` only meaningful for `on`."""
    out: list[tuple[str | None, dict]] = []
    if kind == "on":
        for event, entry in (state_or_root.get("on") or {}).items():
            out.extend((event, t) for t in as_transition_list(entry))
    else:
        entry = state_or_root.get(kind)
        if entry is not None:
            out.extend((None, t) for t in as_transition_list(entry))
    return out


def as_transition_list(value) -> list[dict]:
    if isinstance(value, str):
        return [{"target": value}]
    if isinstance(value, dict):
        return [value]
    if isinstance(value, list):
        return [({"target": v} if isinstance(v, str) else v) for v in value]
    return []


def _lint_targets(graph: Graph, errors: list[str]) -> None:
    def check(path: tuple[str, ...], state_or_root: dict, label: str) -> None:
        for kind in ("always", "on", "onDone"):
            for event, transition in _iter_transitions(state_or_root, kind):
                target = transition.get("target")
                if target is None:
                    continue
                if graph.resolve_target(path, target) is None:
                    where = f"{label}.{kind}" + (f".{event}" if event else "")
                    errors.append(f"{where}: target {target!r} does not resolve from {label!r}")

    check((), {"on": graph.root_on}, "graph")
    for path, state in graph.paths.items():
        dotted = "states." + ".".join(path)
        check(path, state, dotted)


def _lint_choose(graph: Graph, errors: list[str]) -> None:
    for path, state in graph.paths.items():
        meta = state.get("meta") or {}
        choose = meta.get("choose")
        if choose is None:
            continue
        dotted = "states." + ".".join(path)
        on_events = set((state.get("on") or {}).keys())
        from_val = choose.get("from")
        if from_val == "transitions":
            non_reserved = on_events - RESERVED_EVENTS
            if len(non_reserved) < 2:
                errors.append(
                    f"{dotted}.meta.choose: source is \"transitions\" but {dotted}.on has fewer "
                    f"than two non-reserved events ({sorted(non_reserved)})"
                )
        else:
            if "PICK" not in on_events:
                errors.append(
                    f"{dotted}.meta.choose: source is not \"transitions\" but {dotted}.on has no PICK handler"
                )

        items = []
        if isinstance(from_val, dict) and isinstance(from_val.get("menu"), list):
            items.extend(from_val["menu"])
        if isinstance(choose.get("also"), list):
            items.extend(choose["also"])
        for item in items:
            if not isinstance(item, dict):
                continue
            label = item.get("label")
            if isinstance(label, str):
                nbytes = len(label.encode("utf-8"))
                if nbytes > graph.option_tokens:
                    errors.append(
                        f"{dotted}.meta.choose: menu label {label!r} is {nbytes} bytes, "
                        f"over the checkpoint's option_tokens ({graph.option_tokens})"
                    )

        # docs/design/judgement.md section 2.2: not an error -- the closed
        # grammar admits an NLI leaf inside `prefer` -- but a cost worth
        # recording where the graph is loaded, once, rather than
        # re-walking every rule on every run. `run.py`'s `start()` reads
        # this list to warn a fresh run's operator by name.
        prefer = choose.get("prefer")
        if isinstance(prefer, list) and any(
            guards.collect_nli_leaves(rule) for rule in prefer if isinstance(rule, dict)
        ):
            graph.prefer_nli_states.append(dotted)


def _lint_always_spin(graph: Graph, errors: list[str]) -> None:
    """Reject an `always` entry that can only spin -- see this module's
    docstring for why. Each entry of every state's `always` (a bare
    string, a single transition object, or a list of either, all
    normalised by `as_transition_list`) is checked on its own; this is a
    local, per-entry rule, not a reachability analysis across a whole
    `always` list, the same granularity every other lint here works at."""
    for path, state in graph.paths.items():
        raw = state.get("always")
        if raw is None:
            continue
        dotted = "states." + ".".join(path)
        entries = as_transition_list(raw)
        multiple = isinstance(raw, list) and len(entries) > 1
        for i, transition in enumerate(entries):
            if not isinstance(transition, dict):
                continue
            if transition.get("guard") is not None:
                continue  # a guard is what stops it -- legitimate
            actions = transition.get("actions") or []
            if any(isinstance(a, dict) and a.get("type") == "tool" for a in actions):
                continue  # real work -- a budget-counted action, a fresh observation
            target = transition.get("target")
            if target is None:
                spins = True  # internal transition: "nothing else happens"
            else:
                resolved = graph.resolve_target(path, target)
                reenter = bool(transition.get("reenter", False))
                # An unresolvable target is already a `_lint_targets` error;
                # only a target that resolves back to this same state, with
                # no `reenter` to force the exit/entry cascade, is a spin.
                spins = resolved == path and not reenter
            if spins:
                where = f"{dotted}.always[{i}]" if multiple else f"{dotted}.always"
                errors.append(
                    f"{where}: unconditional (no guard), does no tool action, and cannot "
                    "leave this state (no target, or a self-target with reenter unset) -- "
                    "this can only spin, advancing no step, action or visit, until wall_s"
                )


def _third_slash_prefix(url: str) -> str:
    """The text of `url` up to (not including) its third `/` -- for
    `https://host/path`, that is `https://host`, the origin. A schema/host
    with fewer than two slashes returns the whole string, which is exactly
    right: there is no third slash to be *before*, so the whole thing is
    "before" it, and if it contains a template marker the check below still
    catches it."""
    return "/".join(url.split("/", 3)[:3])


def _url_origin(url: str) -> str:
    """Scheme + lowercase host [+ port], no path -- docs/design/unattended.md
    section 2's own definition of an origin, "scheme, host and port,
    lowercase host, no path.\" Safe to call on a templated URL: the `{{...}}`
    a graph author writes always sits in the path (the lint that requires a
    literal origin runs first, in `_lint_warrant` below, via
    `_third_slash_prefix`), and `urlsplit` parses the scheme/host of a
    string like that the same as any other -- it does not validate the path
    it never looks at."""
    parsed = urlsplit(url)
    origin = f"{parsed.scheme}://{(parsed.hostname or '').lower()}"
    if parsed.port is not None:
        origin += f":{parsed.port}"
    return origin


def _lint_warrant(graph: Graph, errors: list[str]) -> None:
    """docs/design/unattended.md section 2: a graph that declares
    `meta.jev.warrant` must be internally consistent with the rest of the
    document, because the warrant is what an operator reads *instead of*
    the table asking about each of the graph's actions one at a time --
    every bound below is enforced for real, at run time, by a different
    layer (section 3's table is the map); this lint is the one place that
    catches an author writing a warrant the graph itself could never
    actually honour, before it ever reaches an operator as a question.
    Skipped entirely when the graph declares no warrant -- an attended
    graph's `tools` need never be enumerated anywhere."""
    raw_warrant = graph.jev.get("warrant")
    if raw_warrant is None or not isinstance(raw_warrant, dict):
        return  # not a dict is already a _validate_warrant shape error
    tools = set(raw_warrant.get("tools") or [])
    origins = set(raw_warrant.get("origins") or [])
    commands = set(raw_warrant.get("commands") or [])

    dispatched_tools: set[str] = set()
    literal_commands: set[str] = set()
    browser_opens: list[tuple[str, dict]] = []

    def scan_actions(actions: Any, where: str) -> None:
        for i, action in enumerate(actions or []):
            if not isinstance(action, dict) or action.get("type") != "tool":
                continue
            params = action.get("params")
            if not isinstance(params, dict):
                continue
            name = params.get("name")
            if not isinstance(name, str):
                continue
            dispatched_tools.add(name)
            action_where = f"{where}[{i}]"
            if name == "browser_open":
                browser_opens.append((action_where, params))
            if name == "bash":
                tool_input = params.get("input")
                command = tool_input.get("command") if isinstance(tool_input, dict) else None
                if isinstance(command, str) and "{{" not in command:
                    literal_commands.add(command)

    def scan_menu_items(items: Any) -> None:
        if not isinstance(items, list):
            return
        for item in items:
            if isinstance(item, dict) and isinstance(item.get("command"), str):
                literal_commands.add(item["command"])

    def scan_state(dotted: str, state_or_root: dict) -> None:
        for key in ("entry", "exit"):
            scan_actions(state_or_root.get(key), f"{dotted}.{key}")
        for kind in ("always", "on", "onDone"):
            for event, transition in _iter_transitions(state_or_root, kind):
                twhere = f"{dotted}.{kind}" + (f".{event}" if event else "")
                if isinstance(transition, dict):
                    scan_actions(transition.get("actions"), f"{twhere}.actions")
        meta = state_or_root.get("meta") or {}
        choose = meta.get("choose") if isinstance(meta, dict) else None
        if isinstance(choose, dict):
            from_val = choose.get("from")
            if isinstance(from_val, dict):
                scan_menu_items(from_val.get("menu"))
            scan_menu_items(choose.get("also"))

    scan_state("graph", {"on": graph.root_on})
    for path, state in graph.paths.items():
        scan_state("states." + ".".join(path), state)

    missing = dispatched_tools - tools
    extra = tools - dispatched_tools
    if missing:
        errors.append(
            f"meta.jev.warrant.tools: the graph dispatches {sorted(missing)} which the warrant does not name"
        )
    if extra:
        errors.append(f"meta.jev.warrant.tools: names {sorted(extra)} which no action dispatches")

    for where, params in browser_opens:
        tool_input = params.get("input")
        tool_input = tool_input if isinstance(tool_input, dict) else {}
        iwhere = f"{where}.input"
        confine = tool_input.get("confine")
        if not isinstance(confine, list) or set(confine) != origins:
            errors.append(f"{iwhere}.confine: must equal meta.jev.warrant.origins")
        url = tool_input.get("url")
        if isinstance(url, str):
            if "{{" in _third_slash_prefix(url):
                errors.append(f"{iwhere}.url: the origin must be literal under a warrant")
            else:
                origin = _url_origin(url)
                if origin not in origins:
                    errors.append(f"{iwhere}.url: origin {origin} is not in meta.jev.warrant.origins")

    if ("browser_click" in tools or "browser_type" in tools) and not origins:
        errors.append("meta.jev.warrant: a warrant covering browser_click or browser_type must name origins")

    dispatchable_commands = literal_commands
    for cmd in sorted(commands):
        if cmd not in dispatchable_commands:
            errors.append(f"meta.jev.warrant.commands: {cmd!r} is not a command this graph can dispatch")

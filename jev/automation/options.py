"""options.py -- docs/design/automation.md section 2, "The four sources" /
"The chooser's context".

One node's `meta.choose` block produces an ordered list of `Option`
records, whichever of the four sources it names -- `transitions` (a
state's own outgoing events), `refs` (accessibility-snapshot elements),
`menu` (a fixed list written in the graph) or `lines` (a text's non-empty
lines) -- so the interpreter never has to know which source built the list
it is about to score. `also` appends fixed items, `exclude` drops options
already tried, `max` caps the source's own contribution before `also` is
appended (so a structural escape added via `also` is never at risk of being
capped away), and `choose.label`, when given, re-renders every option's
label from its own data.
"""
from __future__ import annotations

import re
from dataclasses import dataclass, field

from . import a11y, guards, template
from .graph import RESERVED_EVENTS, as_transition_list


@dataclass
class Option:
    label: str
    event: str
    data: dict = field(default_factory=dict)


def _collapse_duplicates(opts: list[Option]) -> tuple[list[Option], int]:
    """docs/design/judgement.md section 2.1, "The menu the record was
    measured on": collapse options whose `label` is identical -- exact
    string, no folding (`Cat` and `cat` are two links) -- to the first
    occurrence; the survivor carries the first occurrence's `ref`/`url`/
    other data, never a later one's. Identical text pointing at two
    destinations is the page's ambiguity, not this function's to resolve
    -- the `url` bound (`choose.from.refs.url`) is where a graph author
    resolves it, same as the design already says for the menu bound in
    general. Returns `(collapsed, n_dropped)`, `n_dropped` counting every
    later copy removed, so a caller can accumulate it across more than one
    call (`build_options` calls this twice -- see below) rather than
    re-deriving it from a length difference.

    Same definition of "duplicate" as `jev/server.py`'s `_choose` already
    enforces for the interactive `jev_choose` tool -- label text, exact,
    nothing else (`len(set(options)) != len(options)` there) -- just a
    different answer to what to do about one. `_choose`'s options are bare
    strings with no `ref` to prefer between, so it refuses the call
    outright rather than guess (`"choose needs distinct options; two
    options were identical"` -- a caller handed two identical strings
    almost certainly meant one, and picking a survivor for it would hide
    that). `build_options` cannot refuse the same way: its options carry a
    `ref` a live page actually produced, a run has no operator standing by
    to go fix the page mid-hop, and a real page routinely repeats a link
    without that being anyone's authoring mistake -- `Felidae` three times
    in the Cat article's lead and infobox is not a bug in the Cat article.
    Collapsing to the first occurrence is the graceful version of the same
    contract `_choose` enforces the hard way, and it is why a softmax over
    the collapsed menu no longer splits one decision's mass across copies
    of itself (judgement.md section 1: the trained checkpoint's `Felidae`
    at 0.243 against its own copy, a manufactured tie the margin check
    reads as "no answer" -- collapsed, 0.468 against `Felis` at 0.131, a
    real margin)."""
    seen: set[str] = set()
    collapsed: list[Option] = []
    dropped = 0
    for option in opts:
        if option.label in seen:
            dropped += 1
            continue
        seen.add(option.label)
        collapsed.append(option)
    return collapsed, dropped


def build_options(state: dict, choose: dict, scope: dict, graph) -> tuple[list[Option], list[str], int]:
    """`scope` already carries `context`/`input`/`event`/`run`; this
    function adds nothing to it except, per option, a `state`-scoped
    `option` key while rendering `choose.label`. Returns
    `(options, warnings, duplicates_collapsed)` -- never raises on an
    empty result; an empty list is `EMPTY`'s job to raise, in `run.py`, not
    this module's.

    `duplicates_collapsed` is `_collapse_duplicates`'s own count, taken
    **before** `max` is applied (so a source with duplicates ahead of
    distinct options still admits as many distinct options as `max` allows,
    rather than losing slots to copies of a label already seen) and
    **before** `exclude` (whose own per-label filter commutes with the
    collapse either way, so the earlier ordering costs nothing and keeps
    the very first thing done to a freshly-built raw list one thing:
    remove exact repeats of itself). `also`'s own items go through the
    identical pass, folded into the running total -- a `menu`-sourced
    `from` and an `also` list both build through `_from_menu`, and both
    get the same treatment, so an author who lists a label twice in either
    gets one option and a count of one, never two options the chooser
    would have to split a decision across."""
    warnings: list[str] = []
    from_val = choose["from"]
    if from_val == "transitions":
        raw = _from_transitions(state)
    elif "refs" in from_val:
        raw = _from_refs(from_val.get("refs") or {}, scope, warnings)
    elif "menu" in from_val:
        raw = _from_menu(from_val["menu"])
    elif "lines" in from_val:
        raw = _from_lines(from_val.get("lines") or {}, scope)
    else:  # unreachable once graph.py's lint has run
        raise ValueError(f"choose.from: unrecognised source {from_val!r}")

    raw, duplicates_collapsed = _collapse_duplicates(raw)

    exclude_tpl = choose.get("exclude")
    if exclude_tpl:
        excluded = _as_excluded_set(template.render_value(exclude_tpl, scope, warnings))
        raw = [o for o in raw if o.label not in excluded]

    max_n = choose.get("max", graph.default("max", 64))
    if max_n is not None:
        raw = raw[:max_n]

    also = choose.get("also")
    if also:
        raw, also_dropped = _collapse_duplicates(raw + _from_menu(also))
        duplicates_collapsed += also_dropped

    label_tpl = choose.get("label")
    if label_tpl:
        for option in raw:
            option_scope = dict(scope)
            option_scope["option"] = {**option.data, "label": option.label, "event": option.event}
            option.label = template.render(label_tpl, option_scope, warnings)

    return raw, warnings, duplicates_collapsed


def chooser_context(choose: dict, scope: dict, state_description: str | None, warnings: list[str]) -> str:
    """`choose.context`, rendered; or the design's own default -- the goal
    first when `context.goal` exists (so the fixed-position 192-byte read
    sees it before the observation), the state's description, then
    `context.obs.text`."""
    tpl = choose.get("context")
    if tpl:
        return template.render(tpl, scope, warnings)
    goal = template.resolve_path("context.goal", scope)
    if goal is not template.MISSING and goal not in (None, ""):
        default_tpl = "{{context.goal}}\n{{state.description}}\n{{context.obs.text}}"
    else:
        default_tpl = "{{state.description}}\n{{context.obs.text}}"
    return template.render(default_tpl, {**scope, "state": {"description": state_description or ""}}, warnings)


def first_preferred(
    prefer: list[dict],
    opts: list[Option],
    scope: dict,
    warnings: list[str],
    *,
    entail: guards.Entailer | None = None,
    default_threshold: float = 0.6,
) -> int | None:
    """docs/design/judgement.md section 2.2, `choose.prefer`: a list of
    guards from the closed grammar (`graph.py`'s `_validate_choose` admits
    an `option.*` path only inside this list), each evaluated per option
    with the option added to scope as `option.*` -- `label`, `event`, and
    the source's own data (`ref`, `name`, `url` for `refs`; whatever a
    `menu` item carried beyond `label`/`event`). Rules are tried in order
    and, within a rule, options in menu order; the first `(rule, option)`
    pair that passes wins. That is the whole answer to "what if more than
    one thing matches": an earlier rule always outranks a later one
    regardless of where either's matching option sits in the menu, and a
    single rule that matches more than one option takes whichever comes
    first -- deterministic, no score anywhere in the decision. Returns the
    winning option's index, or `None` when nothing passed; the caller
    (`run.py`'s `_choose_phase`) is what falls through to the chooser and
    the floor when this returns `None` -- this function never calls
    either, and never scores anything itself.

    A rule that does not match must leave every one of those `None` calls
    looking identical to a graph with no `prefer` at all -- same `opts`,
    same `scope`, no state mutated here -- so the ordinary floor/margin
    path downstream is untouched bit-for-bit; this function has nothing to
    do with that path succeeding or parking once it returns `None`.

    `entail`/`default_threshold` are only reached if a graph author places
    an NLI guard (`entails`/`contradicts`) inside `prefer` -- admitted by
    the grammar (the closed set is closed, not narrowed, for this scope),
    not recommended, and priced by `graph.py`'s lint the moment such a
    graph loads (`prefer_nli_states`, "one entail pair per option per
    visit"). Each option is its own scope, so this is not `first_passing`'s
    batch -- every NLI leaf here is its own `evaluate_guard` call, one
    `entail` round trip per option per rule reached. wiki-hop's own rule
    needs none of that: `equals` against `{{context.goal}}` is a
    deterministic string comparison against operator-supplied data, zero
    model calls -- judgement.md section 3's cost table: "microseconds; the
    chooser call it does not save is 2.5 ms." A rule is safest when the
    non-option side of its comparison comes from `context`/`input` (data
    the run itself controls) rather than from another `option.*` path (data
    the page controls): the grammar does not forbid comparing two
    page-supplied fields against each other, but a rule shaped that way is
    not reading anything the operator asked for -- it is asking whether a
    page agrees with itself, which is not a decision, just a lint-invisible
    tautology waiting to fire on every menu. That is a graph-authoring
    discipline this function cannot enforce structurally (a template can
    mix roots), the same place NLI-in-`prefer`'s cost is a discipline
    rather than a limit; both are why a `prefer` hit is logged as
    `source: "rule"` rather than folded into a score -- so a row like that
    is always legible as "a rule fired" and reviewable as one, never
    mistaken for the chooser having been especially confident.
    """
    for rule in prefer:
        for index, option in enumerate(opts):
            option_scope = {**scope, "option": {**option.data, "label": option.label, "event": option.event}}
            if guards.evaluate_guard(
                rule, option_scope, warnings, entail=entail, default_threshold=default_threshold
            ):
                return index
    return None


# --- the four sources ----------------------------------------------------------


def _from_transitions(state: dict) -> list[Option]:
    options = []
    for event, entry in (state.get("on") or {}).items():
        if event in RESERVED_EVENTS:
            continue
        transitions = as_transition_list(entry)
        description = None
        if transitions and isinstance(transitions[0], dict):
            description = transitions[0].get("description")
        options.append(Option(label=description or event, event=event, data={}))
    return options


def _from_refs(refs_cfg: dict, scope: dict, warnings: list[str]) -> list[Option]:
    of = refs_cfg.get("of", "context.obs")
    roles = refs_cfg.get("roles") or ["link", "button"]
    within = refs_cfg.get("within")
    named = refs_cfg.get("named", True)
    url_re = refs_cfg.get("url")
    if url_re is not None:
        url_re = re.compile(url_re)
    resolved = template.resolve_path(of, scope)
    records = _refs_of(resolved, warnings)
    options = []
    for record in records:
        if record["role"] not in roles:
            continue
        if within is not None and record.get("landmark") != within:
            continue
        if named and not record.get("name"):
            continue
        if not record.get("ref"):
            continue  # nothing to click
        # docs/design/unattended.md section 3, "the menu bound": an option
        # whose snapshot url is missing, or present but not matching, never
        # reaches the chooser at all -- the bound the adversary meets first,
        # whatever text a page writes to steer the choice.
        if url_re is not None and not (record.get("url") and re.search(url_re, record["url"])):
            continue
        label = record.get("name") or ""
        options.append(
            Option(
                label=label,
                event="PICK",
                data={
                    "ref": record["ref"],
                    "role": record["role"],
                    "name": record.get("name"),
                    "url": record.get("url"),
                },
            )
        )
    return options


def _refs_of(resolved, warnings: list[str]) -> list[dict]:
    """`of` (default `context.obs`) may resolve to the whole `obs` object,
    to an explicit `refs` list already sitting on it, or directly to a bare
    list of records. The tree is parsed here, on demand, rather than
    eagerly by `a11y.build_obs` -- see that module's docstring for why:
    most observations (a `bash` listing, for instance) are not an
    accessibility tree and must not be scanned as though they were.

    `a11y.parse_refs` never raises on a line it cannot read -- one bad line
    must not fail a whole snapshot -- but this is the one place that count
    means something concrete: a line skipped here is a link, button or
    heading that never became an option, silently. `parse_refs` itself
    only counts; this is where the count stops being swallowed and becomes
    a warning on the choice this menu is about to be scored for."""
    if isinstance(resolved, dict):
        refs = resolved.get("refs")
        if isinstance(refs, list):
            return refs
        text = resolved.get("text")
        if isinstance(text, str):
            records, skipped = a11y.parse_refs(text)
            if skipped:
                warnings.append(
                    f"{skipped} accessibility-tree line(s) did not match the expected "
                    "`role \"name\" [attrs]` shape and were skipped -- this menu may be missing options"
                )
            return records
        return []
    if isinstance(resolved, list):
        return resolved
    return []


def _from_menu(items: list[dict]) -> list[Option]:
    options = []
    for item in items:
        label = item["label"]
        event = item.get("event") or "PICK"
        data = {k: v for k, v in item.items() if k not in ("label", "event")}
        options.append(Option(label=label, event=event, data=data))
    return options


def _from_lines(lines_cfg: dict, scope: dict) -> list[Option]:
    of = lines_cfg.get("of", "context.obs.text")
    skip = lines_cfg.get("skip", 0)
    resolved = template.resolve_path(of, scope)
    text = resolved if isinstance(resolved, str) else ""
    body = text.splitlines()[skip:]
    options = []
    for index, line in enumerate(body):
        stripped = line.strip()
        if not stripped:
            continue
        options.append(Option(label=stripped, event="PICK", data={"line": stripped, "index": index}))
    return options


def _as_excluded_set(value) -> set[str]:
    if value is None:
        return set()
    if isinstance(value, (list, tuple)):
        return {v if isinstance(v, str) else template.stringify(v) for v in value}
    if isinstance(value, str):
        return {value}
    return set()

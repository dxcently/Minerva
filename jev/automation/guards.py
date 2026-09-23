"""Guards -- docs/design/automation.md section 1 ("the closed set of
kinds") and section 3 ("Guards through openjev").

The deterministic kinds -- `equals`, `contains`, `matches`, `exists`,
`count`, and the `not`/`and`/`or` combinators -- run in microseconds over
the context and have nothing to do with openjev. The two NLI kinds,
`entails` and `contradicts`, are section 3's actual subject ("Guards
*through openjev*"): a premise and a hypothesis, both rendered from
templates, go to an injected `entail` callable (`jev/server.py`'s
`_openjev()`, wired in through `automation/run.py` -- this module never
imports `server` itself, the same dependency-injection shape `run.py`
already uses for `jevlike`'s `chooser`) and the guard passes iff its named
label's probability meets its threshold. `neutral` is not a label either
kind checks, so it can never be the reason a guard passes -- and neither
can "no `entail` was available to ask": `_evaluate_nli_guard` treats a
missing `entail`, or an unrenderable premise/hypothesis, exactly like the
service answering neutral, with a warning saying which. That is the
safety property section 3 states directly: "neutral never passes
anything."

**Batching.** `first_passing` is what `run.py` calls instead of hand-rolling
a loop over `evaluate_guard`: give it the transitions a state, an `on`
handler or an `onDone` is about to try, in document order, and it returns
the index of the first one whose guard passes. A purely deterministic
prefix is evaluated with no `entail` call at all; the moment a transition's
guard needs openjev, every NLI leaf from there to the end of the list --
`entails`/`contradicts` guards nested inside `not`/`and`/`or` included -- is
rendered and sent in exactly one round trip (one per distinct rendered
premise, which collapses to one call outright whenever every guard on the
list shares the default `{{context.obs.text}}` premise, the common case),
and evaluation then resumes in document order against those answers.
`evaluate_guard` itself stays guard-at-a-time and recursive for
callers -- direct tests, mainly -- that have no batch to hand it; it takes
the same `entail`/`default_threshold` a batch would have used, and simply
asks for one hypothesis at a time.
"""
from __future__ import annotations

import re
from typing import Any, Callable

from . import template

DETERMINISTIC_KINDS = frozenset({"equals", "contains", "matches", "exists", "count"})
COMBINATOR_KINDS = frozenset({"not", "and", "or"})
NLI_KINDS = frozenset({"entails", "contradicts"})
GUARD_KINDS = DETERMINISTIC_KINDS | COMBINATOR_KINDS | NLI_KINDS

# schema.json's own `$defs/nli` defaults -- not baked in by the loader
# (graph.py's `_validate_nli` checks shape and range but inserts nothing),
# so a guard whose `params` omits one of these falls back to it here, at
# the one place both the batched and unbatched paths go through.
DEFAULT_PREMISE_TEMPLATE = "{{context.obs.text}}"
DEFAULT_WINDOW = "head"
DEFAULT_MAX_CHARS = 1500

# `Entailer(premise, hypotheses) -> [{"contradiction": p, "entailment": p,
# "neutral": p}, ...]`, one scored dict per hypothesis, same order -- the
# exact shape `jev/server.py`'s `_openjev()` closure already returns.
Entailer = Callable[[str, list], list]


def evaluate_guard(
    guard: dict,
    scope: dict,
    warnings: list[str] | None = None,
    *,
    entail: Entailer | None = None,
    nli_cache: dict | None = None,
    default_threshold: float = 0.6,
) -> bool:
    """`guard` is `{"type": ..., "params": {...}}`, already schema-valid (an
    unknown `type` is refused at lint time -- see graph.py -- so this never
    has to handle one). `scope` is a template scope: at minimum
    `{"context": ..., "input": ..., "event": ..., "run": ...}`.

    `entail`/`nli_cache`/`default_threshold` only matter to an `entails`/
    `contradicts` leaf, direct or nested under `not`/`and`/`or`, and are
    threaded unchanged through every recursive call so a combinator never
    has to know which of its children are NLI. `nli_cache`, when given, is
    `first_passing`'s pre-fetched batch (`id(leaf guard dict) -> bool`);
    supplying it is what lets a caller batch, but nothing about this
    function's own behaviour requires it -- called without one (every
    direct call in this file's own tests), an NLI leaf is evaluated on the
    spot, one `entail` call per leaf.
    """
    kind = guard["type"]
    params = guard.get("params", {})
    if kind == "equals":
        left = template.resolve_path(params["path"], scope)
        if left is template.MISSING:
            left = None
        right = params["value"]
        if isinstance(right, str):
            right = template.render_value(right, scope, warnings)
        if params.get("fold", False) and isinstance(left, str) and isinstance(right, str):
            # docs/design/judgement.md section 2.2: a link's text is "cat"
            # and a goal is "Cat" -- casefold (broader than `.lower()`, the
            # correct choice for caseless Unicode comparison) plus a strip
            # so surrounding whitespace a page's markup introduced does not
            # defeat an otherwise-matching rule either. Only ever applies
            # between two strings -- `fold` on a numeric or `None` `left`
            # falls straight through to the plain `==` below, the same as
            # it would if `fold` were never set.
            return left.casefold().strip() == right.casefold().strip()
        return left == right
    if kind == "contains":
        left = template.resolve_path(params["path"], scope)
        needle = template.render(params["value"], scope, warnings)
        if isinstance(left, str):
            return needle in left
        if isinstance(left, (list, tuple)):
            return any(item == needle or template.stringify(item) == needle for item in left)
        return False
    if kind == "matches":
        left = template.resolve_path(params["path"], scope)
        text = left if isinstance(left, str) else template.stringify(left)
        return re.search(params["pattern"], text, re.MULTILINE) is not None
    if kind == "exists":
        value = template.resolve_path(params["path"], scope)
        return value is not template.MISSING and value is not None
    if kind == "count":
        value = template.resolve_path(params["path"], scope)
        length = len(value) if isinstance(value, (list, tuple, str, dict)) else 0
        gte, lte = params.get("gte"), params.get("lte")
        if gte is not None and length < gte:
            return False
        if lte is not None and length > lte:
            return False
        return True
    if kind == "not":
        return not evaluate_guard(
            params["guard"], scope, warnings, entail=entail, nli_cache=nli_cache, default_threshold=default_threshold
        )
    if kind == "and":
        return all(
            evaluate_guard(g, scope, warnings, entail=entail, nli_cache=nli_cache, default_threshold=default_threshold)
            for g in params["guards"]
        )
    if kind == "or":
        return any(
            evaluate_guard(g, scope, warnings, entail=entail, nli_cache=nli_cache, default_threshold=default_threshold)
            for g in params["guards"]
        )
    if kind in NLI_KINDS:
        if nli_cache is not None and id(guard) in nli_cache:
            return nli_cache[id(guard)]
        return _evaluate_nli_guard(guard, scope, warnings, entail=entail, default_threshold=default_threshold)
    raise ValueError(f"unknown guard type {kind!r}")  # unreachable past lint


def collect_nli_leaves(guard: dict) -> list[dict]:
    """Every `entails`/`contradicts` guard dict reachable from `guard`,
    walking through `not`/`and`/`or` -- `guard` itself, if it already is
    one. Document order, duplicates included (a graph author repeating the
    identical guard object twice is their call, not this function's to
    collapse)."""
    kind = guard["type"]
    if kind in NLI_KINDS:
        return [guard]
    if kind == "not":
        return collect_nli_leaves(guard["params"]["guard"])
    if kind in ("and", "or"):
        leaves: list[dict] = []
        for g in guard["params"]["guards"]:
            leaves.extend(collect_nli_leaves(g))
        return leaves
    return []


def first_passing(
    transitions: list[dict],
    scope: dict,
    warnings: list[str] | None = None,
    *,
    entail: Entailer | None = None,
    default_threshold: float = 0.6,
) -> int | None:
    """The batched form of "try each of `transitions`' guards in document
    order, first pass wins" -- what `run.py` calls for a state's `always`,
    one `on[event]` list, or an `onDone` list, instead of looping over
    `evaluate_guard` itself. `transitions` is whatever
    `graph.as_transition_list` already produced; each entry's `"guard"`
    key may be absent (an unconditional transition, always the answer the
    moment nothing before it needed asking) or a guard tree.

    Section 3: "the deterministic guards before them in the list are
    evaluated first and can short-circuit the batch entirely." A prefix
    with no NLI leaf anywhere in it is walked with zero `entail` calls; the
    first transition whose guard needs openjev stops that walk, and every
    NLI leaf from *that* transition to the end of the list -- there is
    nothing NLI-bearing before it, by definition of "first" -- is rendered
    and answered in one batch (one `entail` call per distinct rendered
    premise), then the same document-order walk resumes using those
    answers. Returns the winning index, or `None` if nothing passed.
    """
    n = len(transitions)
    i = 0
    while i < n:
        guard = transitions[i].get("guard")
        if guard is not None and collect_nli_leaves(guard):
            break
        if guard is None or evaluate_guard(guard, scope, warnings, default_threshold=default_threshold):
            return i
        i += 1
    else:
        return None
    leaves: list[dict] = []
    for t in transitions[i:]:
        g = t.get("guard")
        if g is not None:
            leaves.extend(collect_nli_leaves(g))
    nli_cache = _resolve_batch(leaves, scope, warnings, entail, default_threshold)
    for j in range(i, n):
        guard = transitions[j].get("guard")
        if guard is None or evaluate_guard(
            guard, scope, warnings, entail=entail, nli_cache=nli_cache, default_threshold=default_threshold
        ):
            return j
    return None


def _render_nli_strings(guard: dict, scope: dict, warnings: list[str] | None) -> tuple[str, str]:
    """The premise (windowed to `max_chars`) and the hypothesis, both
    rendered and handed to `entail` exactly as they come out -- section 3:
    "Both strings go to the service as they are; the service, and only the
    service," formats them into one `Premise: ...\\nHypothesis: ...` string,
    through the vendor class. Nothing in this module ever builds that
    string itself."""
    params = guard.get("params", {})
    premise = template.render(params.get("premise", DEFAULT_PREMISE_TEMPLATE), scope, warnings)
    hypothesis = template.render(params["hypothesis"], scope, warnings)
    max_chars = params.get("max_chars", DEFAULT_MAX_CHARS)
    if len(premise) > max_chars:
        window = params.get("window", DEFAULT_WINDOW)
        premise = premise[-max_chars:] if window == "tail" else premise[:max_chars]
    return premise, hypothesis


def _label_for(kind: str) -> str:
    return "entailment" if kind == "entails" else "contradiction"


def _passes(kind: str, params: dict, scores: dict, default_threshold: float) -> bool:
    threshold = params.get("threshold", default_threshold)
    return scores.get(_label_for(kind), 0.0) >= threshold


def _resolve_batch(
    leaves: list[dict],
    scope: dict,
    warnings: list[str] | None,
    entail: Entailer | None,
    default_threshold: float,
) -> dict[int, bool]:
    """`id(leaf) -> bool` for every leaf in `leaves`. Groups by rendered
    premise so guards that share one -- the default `{{context.obs.text}}`,
    the overwhelmingly common case -- go in a single `entail` call; a leaf
    with a genuinely different premise costs its own call rather than
    silently mixing pairs the service was never asked to batch across, and
    an empty premise or hypothesis (nothing rendered, or `entail` itself
    unavailable) resolves to `False` with a warning, never a call."""
    cache: dict[int, bool] = {}
    rendered: dict[int, tuple[str, str]] = {}
    groups: dict[str, list[dict]] = {}
    order: list[str] = []
    for leaf in leaves:
        premise, hypothesis = _render_nli_strings(leaf, scope, warnings)
        rendered[id(leaf)] = (premise, hypothesis)
        if not premise or not hypothesis:
            if warnings is not None:
                warnings.append(
                    f"guard type {leaf['type']!r}: empty premise or hypothesis, cannot evaluate; "
                    "treated as not passing"
                )
            cache[id(leaf)] = False
            continue
        groups.setdefault(premise, [])
        if premise not in order:
            order.append(premise)
        groups[premise].append(leaf)
    if entail is None:
        for leaf in leaves:
            if id(leaf) in cache:
                continue
            if warnings is not None:
                warnings.append(
                    f"guard type {leaf['type']!r} has no openjev connection available in this "
                    "build; treated as not passing, the same safe default a neutral verdict gives"
                )
            cache[id(leaf)] = False
        return cache
    for premise in order:
        group = groups[premise]
        hypotheses = [rendered[id(leaf)][1] for leaf in group]
        scored = entail(premise, hypotheses)
        for leaf, scores in zip(group, scored):
            cache[id(leaf)] = _passes(leaf["type"], leaf.get("params", {}), scores, default_threshold)
    return cache


def _evaluate_nli_guard(
    guard: dict,
    scope: dict,
    warnings: list[str] | None,
    *,
    entail: Entailer | None = None,
    default_threshold: float = 0.6,
) -> bool:
    """One `entails`/`contradicts` guard, unbatched: render its premise and
    hypothesis, ask `entail` for exactly that one pair, and pass iff its
    named label's probability meets its threshold (`params.threshold`, or
    `default_threshold` -- `run.py` passes the graph's own
    `meta.jev.defaults.threshold`, 0.6 unless the graph overrides it). No
    `entail` available, or nothing to render, is the same "cannot tell, so
    it does not pass" outcome the design gives an actual neutral verdict --
    see this module's own docstring."""
    kind = guard["type"]
    params = guard.get("params", {})
    premise, hypothesis = _render_nli_strings(guard, scope, warnings)
    if not premise or not hypothesis:
        if warnings is not None:
            warnings.append(
                f"guard type {kind!r}: empty premise or hypothesis, cannot evaluate; treated as not passing"
            )
        return False
    if entail is None:
        if warnings is not None:
            warnings.append(
                f"guard type {kind!r} has no openjev connection available in this build; treated as "
                "not passing, the same safe default a neutral verdict gives"
            )
        return False
    scores = entail(premise, [hypothesis])[0]
    return _passes(kind, params, scores, default_threshold)

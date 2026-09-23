"""warrant.py -- docs/design/unattended.md section 2, "The envelope."

The operator's word for one run, in writing: which tools it may dispatch,
which browser origins it may reach, which literal shell commands it may
run, and a count and a duration. A graph author declares it at
`meta.jev.warrant` (`tools` required; `origins`/`commands` optional,
default empty -- shape-validated by `graph.py`'s `_validate_jev` and
cross-checked against the graph's own actions by `graph.py`'s
`_lint_warrant`). This module turns that declaration into the exact
seven-key block a caller must restate to `jev_run`/`jev_resume`
(`canonical`), the canonical serialisation that block hashes over
(`canonical_json`), the id that hash produces (`warrant_id`), and the
equality check `automation.start`/`automation.answer` run before anything
else happens (`check`).

**The id.** `"w_"` plus the first twelve hex characters of the SHA-256 of
`canonical_json`'s output -- sorted object keys, no whitespace, UTF-8, not
ASCII-escaped, exactly `json.dumps(obj, sort_keys=True, separators=(",",
":"), ensure_ascii=False)`. Computed identically in
`eidolon/crates/rune/src/warrant.rs`'s `Envelope::id` (a separate repo;
read, not ported -- see that file's own `canonical_json` doc comment,
which already states the byte-for-byte contract this module fulfils on
the Python side) and pinned on both sides by a test that hard-codes the
same literal id. `canonical_json` itself only sorts *object* keys --
`json.dumps(sort_keys=True, ...)` never reorders a list's own elements --
which is why the three list-valued keys (`tools`/`origins`/`commands`)
must already be sorted and deduplicated *before* they ever reach
`canonical_json`: `canonical` does this when it builds the block from a
graph; `check`'s own `_normalize` does it for whatever a caller passed,
so a warrant block that lists them in a different order (or repeats one)
still hashes to the same id as the graph's own declared block. The
Rust side reaches the same list-order independence a different way --
`Envelope`'s `tools`/`origins`/`commands` fields are `BTreeSet`s, sorted
by construction the moment the block is parsed -- so there is nothing to
port; both sides just have to agree on the *output*, which is what the
pinned literal proves.

**Why this module, not `graph.py`, owns `canonical_json`.** `graph.py`'s
own `Graph.sha256` needs the identical canonical serialisation (over the
whole parsed document, not just a warrant block -- `json.dumps(...,
sort_keys=True, ...)` recurses through nested objects regardless of their
shape, so the same function serves both), and importing it the other way
(`warrant.py` from `graph.py`) would be circular: `canonical`/`check`
below take a `Graph` as their argument. The `TYPE_CHECKING` guard on that
import is why the cycle never actually forms at runtime -- this module
never calls anything on `Graph` beyond plain attribute reads
(`.jev`, `.sha256`, `.budget(...)`), so nothing here needs the real class
object, only its shape, which `from __future__ import annotations` (this
module's own first import) already defers to a string.
"""
from __future__ import annotations

import hashlib
import json
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from .graph import Graph

# The seven keys, in the order the design doc's own worked example uses --
# not load-bearing for `canonical_json` (which sorts unconditionally), only
# for readability of a literal built in this order elsewhere.
KEYS = ("graph", "sha256", "tools", "origins", "commands", "actions", "wall_s")


def declared(graph: "Graph") -> dict | None:
    """The graph author's own `meta.jev.warrant`, unmodified -- `None` when
    the graph declares no warrant at all, the ordinary case for a graph
    that only ever runs attended."""
    return graph.jev.get("warrant")


def _graph_ref(graph: "Graph") -> str:
    return f"{graph.id}@{graph.version}" if graph.version else graph.id


def canonical(graph: "Graph") -> dict | None:
    """The full seven-key block this graph's warrant *means*, right now,
    against the loaded document -- `None` when `declared` is `None`.
    `graph`/`sha256` are the service's own reading (never taken from the
    declaration, which cannot know its own version string or content
    hash); `tools`/`origins`/`commands` are the declared lists, sorted and
    deduplicated (an origin's host is already lowercase by construction --
    `graph.py`'s own lint pattern for `origins` admits no uppercase);
    `actions`/`wall_s` are the graph's own budget, the same fallback
    defaults `Graph.budget` already uses elsewhere."""
    warrant = declared(graph)
    if warrant is None:
        return None
    return {
        "graph": _graph_ref(graph),
        "sha256": graph.sha256,
        "tools": sorted(set(warrant.get("tools") or [])),
        "origins": sorted(set(warrant.get("origins") or [])),
        "commands": sorted(set(warrant.get("commands") or [])),
        "actions": graph.budget("actions", 100),
        "wall_s": graph.budget("wall_s", 600),
    }


def canonical_json(block: dict) -> str:
    """Sorted keys, no whitespace, UTF-8, not ASCII-escaped -- byte for
    byte what `eidolon/crates/rune/src/warrant.rs`'s `Envelope::canonical_json`
    produces (confirmed by reading that function, not by importing or
    porting it: this crate is a separate repo this task does not touch).
    Sorts object keys only, at every nesting level `json` recurses into --
    it does **not** reorder a list's own elements; see this module's own
    docstring for why every caller must hand it already-sorted lists."""
    return json.dumps(block, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def warrant_id(block: dict) -> str:
    """`"w_"` plus the first twelve hex characters of the SHA-256 of
    `canonical_json(block)`, UTF-8-encoded."""
    digest = hashlib.sha256(canonical_json(block).encode("utf-8")).hexdigest()
    return "w_" + digest[:12]


def _normalize(block: dict) -> dict:
    """A shallow copy of `block` with `tools`/`origins`/`commands` sorted
    and deduplicated when present and list-shaped -- the same
    normalisation `canonical` applies when it builds a block from a graph,
    applied here to whatever a caller (a driving model, an operator's
    `[[warrants]]` entry restated by the CLI, a resume) actually passed,
    so a block that is equal *as a warrant* but differently ordered on the
    wire still canonicalises, hashes and compares equal to the graph's own
    declared block. Any other key is passed through unchanged; a key of
    the wrong shape is left for `canonical_json`/the caller's own
    validation to fail on, not silently coerced here."""
    out = dict(block)
    for key in ("tools", "origins", "commands"):
        value = out.get(key)
        if isinstance(value, list):
            out[key] = sorted(set(value))
    return out


def check(passed, graph: "Graph") -> tuple[dict, str]:
    """`passed` is whatever a caller (`jev_run`'s `warrant` argument, an
    escalation's `answer`) supplied; `graph` is the service's own loaded
    reading of the document it is about to run. Raises `ValueError` naming
    exactly what is wrong -- an operator or a driving model reads this
    string, so "the warrant is invalid" is not an acceptable message (the
    same rule `eidolon/crates/rune/src/warrant.rs`'s `Envelope::parse`
    documents for its own errors):

    - the graph declares no warrant at all (`meta.jev.warrant` absent) --
      naming the graph and pointing at running it attended instead;
    - `passed`, normalised the same way `canonical` itself normalises,
      does not canonicalise to the same JSON as the graph's own declared
      block -- naming the graph and echoing the *correct* block verbatim,
      so a caller can simply copy it rather than diff two JSON blobs by
      eye (`jev_warrant` exists so the caller need never construct one by
      hand in the first place).

    Returns `(block, id)` -- the graph's own canonical block (not
    whatever shape `passed` happened to be typed in) and its id, since
    once this returns, the two are already known to be the same warrant;
    everything downstream (the run record, the journal, the gate) should
    carry the service's own reading, the same "the gate sees the copy the
    operator approved, verified against the owner's reading before
    anything runs" rule section 2 states for the call boundary one layer
    up from here.
    """
    ref = _graph_ref(graph)
    declared_block = canonical(graph)
    if declared_block is None:
        raise ValueError(f"graph {ref} declares no warrant; run it attended, without a warrant argument")
    if not isinstance(passed, dict):
        raise ValueError(f"warrant mismatch for {ref}: pass exactly {canonical_json(declared_block)}")
    if canonical_json(_normalize(passed)) != canonical_json(declared_block):
        raise ValueError(f"warrant mismatch for {ref}: pass exactly {canonical_json(declared_block)}")
    return declared_block, warrant_id(declared_block)

"""decisions.py -- docs/design/automation.md section 5: the decision log,
append-only, and the correction path's row shape.

Out of this task's scope (see `automation/__init__.py`'s module docstring):
the `/jev` correction page, `extensions/jev/tools/relabel.rn`,
`crates/web/src/jev.rs`, and the export/train pipeline
(`decisions.export`, the 80/10/10-by-run-id split). What *is* in scope, and
built here, is the row shape section 5 specifies and the append-only
writer -- `correct_decision` exists and is tested so a caller (a future
`relabel` tool, or a person editing the file with the fix in hand) is not
painted into a corner: correcting a row is already "append a new row with
the same id and a new label", never "rewrite the old one".

One JSONL file per graph, under
`<cache_dir>/eidolon/extensions/jev/decisions/<graph-id>.jsonl` by default
(`JEV_DECISIONS_DIR` overrides the directory, the same idea as
`jev/server.py`'s `JEV_DECISIONS_LOG` for its own ad-hoc log -- kept
separate on purpose: this is a *directory* of per-graph files, that is one
flat file). `label` is the option's index, never its text -- matching
`jev/server.py`'s `_log_decision`, which already gets this right (see its
own docstring for the incident that made it matter), and reusing its
cross-process locking via `_paths.append_locked` rather than a second
implementation of the same lesson.

Unlike `_log_decision`, appends here are **not** swallowed on `OSError` --
these rows are the thing this whole document exists to produce ("The log
is the product"), not a side channel, so a write failure is the caller's
(`run.py`'s) to decide what to do with, not silently lost here.
"""
from __future__ import annotations

import json
import os
import threading
import time
from pathlib import Path

import _paths

_log_lock = threading.Lock()  # in-process half; _paths.append_locked is the cross-process half


def decisions_dir() -> Path:
    override = os.environ.get("JEV_DECISIONS_DIR")
    if override:
        return Path(override)
    return _paths.cache_dir() / "eidolon" / "extensions" / "jev" / "decisions"


def graph_log_path(graph_id: str) -> Path:
    return decisions_dir() / f"{graph_id}.jsonl"


def _iso(ts: float | None) -> str:
    return time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(ts if ts is not None else time.time()))


def _bare_graph_id(graph_ref: str) -> str:
    """`graph_ref` is `id` or `id@version` (docs/design/automation.md's own
    example: the row's `"graph"` field is `"wiki-hop@1"`, but its `"log"`
    field -- and the design's own worked sandbox example, "copy
    decisions/<graph>.jsonl home" -- names the file after the bare id with
    no version. One file pools every version of a graph's decisions on
    purpose: the row itself still carries the exact version each one came
    from, so pooling loses nothing an exporter needs. A `graph_ref` with no
    `@` is returned unchanged, so a caller that already has the bare id
    (`graph_log_path` itself, callers outside this module) does not need to
    know this split exists."""
    return graph_ref.split("@", 1)[0]


def _append(graph_ref: str, row: dict) -> None:
    line = (json.dumps(row, ensure_ascii=False) + "\n").encode("utf-8")
    with _log_lock:
        _paths.append_locked(graph_log_path(_bare_graph_id(graph_ref)), line)


def log_decision(
    *,
    id: str,
    context: str,
    options: list[str],
    label: int,
    chosen: str,
    probs: list[float],
    source: str,
    verified: str | None,
    floor: float,
    margin: float,
    run: str,
    graph: str,
    state: str,
    step: int,
    ckpt: str | None,
    action: dict | None,
    ts: float | None = None,
    warrant: str | None = None,
) -> None:
    """The row from section 5, verbatim, plus this step's `warrant`:
    `{id, context, options, label, chosen, probs, source, verified, floor,
    margin, run, graph, state, step, ts, ckpt, action, warrant}`.

    `source` is who made the pick that was taken: `"jevlike"`, `"model"`,
    `"human"`, `"forced"` (a menu with one option -- written for the
    record, per the design, even though this build has no exporter yet to
    skip it on export), `"rule"` (a `choose.prefer` guard matched before
    the chooser was ever consulted for a verdict -- docs/design/
    judgement.md section 2.2 and section 3's cost table, "zero cost, no
    model call": the chooser is never reached on a rule hit, so `probs`
    for a `"rule"` row is one-hot on the winning index, not a chooser
    opinion -- a reader must check `source`/`verified` before trusting
    `probs` as a score at all, the same caution a `"forced"` row's own
    `probs` (`[1.0]`) already required).
    `verified` is who vouches for the label: `None`
    (the chooser's own, unverified), `"outcome"`, `"model"`, `"human"`,
    `"rule"` (deterministic by construction, and the strongest tier after
    `"human"` -- a future exporter's ruling, not this module's to enforce).
    `warrant` is the run's own `warrant_id` -- `None` for an attended run,
    written unconditionally either way so every row in a graph's log has
    the same key set regardless of which kind of run produced it.
    """
    if not 0 <= label < len(options):
        raise ValueError(f"label {label!r} is not a valid index into {len(options)} options")
    row = {
        "id": id,
        "context": context,
        "options": options,
        "label": label,
        "chosen": chosen,
        "probs": probs,
        "source": source,
        "verified": verified,
        "floor": floor,
        "margin": margin,
        "run": run,
        "graph": graph,
        "state": state,
        "step": step,
        "ts": _iso(ts),
        "ckpt": ckpt,
        "action": action,
        "warrant": warrant,
    }
    _append(graph, row)


def correct_decision(
    *,
    graph: str,
    id: str,
    label: int,
    by: str,
    verified: str = "human",
    note: str | None = None,
    ts: float | None = None,
) -> None:
    """Append a correction row -- never an edit to the original. Same `id`,
    a new `label`; export (not built here) merges by id and the last row
    wins. Does not check that `id` was actually seen in this file first --
    the design gives that job to the exporter ("the exporter refuses a
    correction whose id it has not seen in full"), which is out of this
    task's scope; this function's job is only to write the row shape
    correctly.
    """
    row = {"id": id, "label": label, "verified": verified, "by": by, "ts": _iso(ts)}
    if note is not None:
        row["note"] = note
    _append(graph, row)


def log_run_end(
    *, graph: str, run: str, outcome: str, ts: float | None = None, warrant: str | None = None,
) -> None:
    """`{run, outcome, ts, warrant}` -- what lets a (not-yet-built) exporter
    promote that run's unverified rows to `verified: "outcome"` once it
    reached its goal without a `stop`. `warrant` is the run's own
    `warrant_id`, `None` for an attended run -- the same value every
    decision row from this run already carries, so a reader can join this
    line back to them without re-deriving anything."""
    _append(graph, {"run": run, "outcome": outcome, "ts": _iso(ts), "warrant": warrant})

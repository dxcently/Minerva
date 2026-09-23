"""Stable escalation request ids: one per parked question, carried on the
escalation payload (`request`), persisted inside the park snapshot, echoed
by the payload's own `answer` hint, and optionally demanded back by
`automation.answer`/`jev_resume` so a caller holding an older question's id
cannot answer a newer one.

Kept in its own file rather than added to `test_run.py` on purpose: this is
one narrow contract (docs/design/automation.md section 4's escalation
payload, plus the stale-answer refusal section 4's "an answer outside the
menu is not accepted" already established the shape of) and it belongs to
whichever session is adding request ids, not to the file every other
escalation test already lives in.

Isolation is set up *before* the imports below, not in a `setUp`: the two
env-var readers this file depends on (`automation.decisions.decisions_dir`,
`automation.run.runs_dir`) are read at call time, but `jev/server.py`'s own
ad-hoc `JEV_DECISIONS_LOG` is cheaper to pin here than to reason about, and
pinning all three at module import makes "no test can see another test's
rows, run files or server log" true for the whole file at once. The temp
root is removed in `tearDownModule`.
"""
from __future__ import annotations

import gc
import json
import os
import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

_TMP = tempfile.mkdtemp(prefix="jev-escalation-identity-")
os.environ["JEV_DECISIONS_LOG"] = str(Path(_TMP) / "server-decisions.jsonl")
os.environ["JEV_RUNS_DIR"] = str(Path(_TMP) / "runs")
os.environ["JEV_DECISIONS_DIR"] = str(Path(_TMP) / "decisions")
Path(os.environ["JEV_RUNS_DIR"]).mkdir(parents=True, exist_ok=True)
Path(os.environ["JEV_DECISIONS_DIR"]).mkdir(parents=True, exist_ok=True)

import automation.decisions as decisions  # noqa: E402
import automation.run as run_mod  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
GRAPHS_DIR = REPO_ROOT / "extensions" / "jev" / "graphs"


def tearDownModule():
    shutil.rmtree(_TMP, ignore_errors=True)


def constant_chooser(prefer_index: int = 0, top: float = 0.9, rest: float = 0.05):
    """A chooser that clearly prefers whichever option sorts to
    `prefer_index` -- the same small stand-in `test_run.py` uses, copied
    rather than imported because the only thing two test files can share
    here is a test-utils module neither of them has."""

    def chooser(context: str, labels: list[str]) -> list[float]:
        idx = min(prefer_index, len(labels) - 1)
        return [top if i == idx else rest for i in range(len(labels))]

    return chooser


def refusing_chooser(context: str, labels: list[str]) -> list[float]:
    raise AssertionError(f"chooser should not have been called -- labels were {labels!r}")


def _minimal_graph(**overrides) -> dict:
    graph = {
        "id": "synthetic",
        "initial": "a",
        "meta": {"jev": {"schema": 1}},
        "context": {},
        "states": {"a": {"on": {"GO": "b"}}, "b": {"type": "final"}},
    }
    graph.update(overrides)
    return graph


def _read_jsonl(path: Path) -> list[dict]:
    with path.open("r", encoding="utf-8") as f:
        return [json.loads(line) for line in f if line.strip()]


class EscalationRequestIdTests(unittest.TestCase):
    """The one contract: a park's `request` names that park, and only that
    park, for as long as the question is pending -- in memory, in the
    snapshot on disk, and in the text a caller reads off it."""

    def setUp(self):
        # A still-parked run left dangling in the module-level `_RUNS` is
        # only collected at some later point, long after this file's own
        # env-var window closed -- so every test drops whatever it created,
        # the same discipline `test_run.py`'s own `PersistenceTests` uses.
        self._runs_before_test = set(run_mod._RUNS.keys())
        self.addCleanup(self._drop_runs_created_by_this_test)

    def _drop_runs_created_by_this_test(self):
        with run_mod._RUNS_LOCK:
            for run_id in set(run_mod._RUNS.keys()) - self._runs_before_test:
                run_mod._RUNS.pop(run_id, None)

    # -- fixtures ---------------------------------------------------------

    def _single_park_graph(self):
        return _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )

    def _double_park_graph(self):
        # Two parks in one run's life: the floor miss at `a`, then -- once
        # answered -- the floor miss at `b`, which is exactly the shape a
        # stale request id needs to be wrong *about*.
        return _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
                "b": {"meta": {"choose": {"from": {"menu": [{"label": "p"}, {"label": "q"}]}}}, "on": {"PICK": "c"}},
                "c": {"type": "final"},
            },
        )

    def _empty_park_graph(self):
        return _minimal_graph(
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}]}, "exclude": "{{context.tried}}"}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            },
            context={"tried": ["x"]},  # excludes the only menu item -> zero options
        )

    def _start_parked(self, graph, chooser=None):
        return run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=chooser or constant_chooser(0, top=0.6, rest=0.4),
            ckpt_id=None,
        )

    def _simulate_restart(self, run_id: str) -> None:
        """The genuine drop `test_run.py`'s `PersistenceTests` established:
        remove the run from `_RUNS` (the only place a live process keeps it)
        and force collection, so the still-suspended generator's cleanup
        runs now -- the only way back to the id afterwards is `_lookup`
        reading that run's own file from disk."""
        with run_mod._RUNS_LOCK:
            run_mod._RUNS.pop(run_id, None)
        gc.collect()
        self.assertNotIn(run_id, run_mod._RUNS, "run still referenced elsewhere -- restart not actually simulated")

    # -- the isolation itself ---------------------------------------------

    def test_the_isolation_variables_are_in_effect_before_any_import_reads_them(self):
        for name in ("JEV_DECISIONS_LOG", "JEV_RUNS_DIR", "JEV_DECISIONS_DIR"):
            self.assertTrue(os.environ[name].startswith(_TMP), f"{name} not isolated: {os.environ[name]!r}")
        self.assertEqual(str(decisions.decisions_dir()), os.environ["JEV_DECISIONS_DIR"])
        self.assertEqual(str(run_mod.runs_dir()), os.environ["JEV_RUNS_DIR"])

    # -- one id per question, on both park paths --------------------------

    def test_a_floor_park_mints_a_request_id_and_names_it_in_the_hint(self):
        started = self._start_parked(self._single_park_graph())
        esc = started["request"]
        self.assertEqual(esc["kind"], "escalate")
        self.assertIn("request", esc)
        self.assertTrue(esc["request"].startswith(started["run"] + "-req-"), esc["request"])
        self.assertEqual(
            esc["answer"],
            f"jev_resume {{run, request: {esc['request']}, pick: <index or label>}} or "
            "{run, stop: <reason>}",
        )
        # the run's own live view of the park agrees with what it yielded
        self.assertEqual(run_mod.status(started["run"])["escalation"]["request"], esc["request"])

    def test_an_empty_park_mints_its_own_request_id_too(self):
        started = self._start_parked(self._empty_park_graph(), chooser=refusing_chooser)
        esc = started["request"]
        self.assertEqual(esc["kind"], "escalate")
        self.assertIn("EMPTY", esc["why"])
        self.assertEqual(esc["options"], [])  # nothing a pick could ever match
        self.assertTrue(esc["request"].startswith(started["run"] + "-req-"), esc["request"])
        self.assertIn(f"request: {esc['request']}", esc["answer"])

    def test_each_park_of_the_same_run_mints_a_different_id(self):
        started = self._start_parked(self._double_park_graph())
        first = started["request"]
        second = run_mod.answer(started["run"], request=first["request"], pick=0)["request"]
        self.assertEqual(second["kind"], "escalate")
        self.assertEqual(second["state"], "b")
        self.assertNotEqual(first["request"], second["request"])

    # -- persisted with the snapshot --------------------------------------

    def test_the_request_id_is_what_the_snapshot_persists(self):
        started = self._start_parked(self._single_park_graph())
        run_id, esc = started["run"], started["request"]
        record = _read_jsonl(run_mod._run_file(run_id))[0]
        self.assertEqual(record["kind"], "escalate")
        self.assertEqual(record["escalation"]["request"], esc["request"])
        # the sequence behind it is persisted too, so a reconstruction
        # cannot rewind `next_request_id` onto an id already used
        self.assertEqual(record["req_seq"], int(esc["request"].rsplit("-req-", 1)[1]))

    # -- a stale id cannot answer a new question --------------------------

    def test_an_old_request_id_cannot_answer_the_new_question(self):
        started = self._start_parked(self._double_park_graph())
        run_id = started["run"]
        first = started["request"]
        second = run_mod.answer(run_id, request=first["request"], pick=0)["request"]
        self.assertNotEqual(first["request"], second["request"])

        with self.assertRaises(ValueError) as caught:
            run_mod.answer(run_id, request=first["request"], pick=1)
        self.assertIn("stale request", str(caught.exception))
        self.assertIn(second["request"], str(caught.exception))

        live = run_mod.status(run_id)  # the live decision is untouched
        self.assertFalse(live["finished"])
        self.assertIsNone(live["outcome"])
        self.assertEqual(live["active"], "b")
        self.assertEqual(live["counts"]["escalations"], 2)
        self.assertEqual(live["escalation"]["request"], second["request"])

        # and the id that *does* name the pending question still works
        final = run_mod.answer(run_id, request=second["request"], pick=1)["request"]
        self.assertEqual(final["outcome"], "reached")
        self.assertEqual(final["final_state"], "c")

    def test_a_stale_request_id_cannot_stop_the_new_question_either(self):
        # A stale id means the caller is not looking at the run as it is
        # now, so neither an answer nor a stop is applied -- the same
        # refusal, for the same reason, rather than a blind stop.
        started = self._start_parked(self._double_park_graph())
        run_id = started["run"]
        first = started["request"]
        second = run_mod.answer(run_id, request=first["request"], pick=0)["request"]

        with self.assertRaises(ValueError):
            run_mod.answer(run_id, request=first["request"], stop="give up")
        live = run_mod.status(run_id)
        self.assertFalse(live["finished"])
        self.assertEqual(live["escalation"]["request"], second["request"])

        final = run_mod.answer(run_id, request=second["request"], stop="give up")["request"]
        self.assertEqual(final["outcome"], "stopped")
        self.assertEqual(final["reason"], "give up")

    # -- the same id survives a restart -----------------------------------

    def test_the_persisted_request_survives_restoration_and_answers_the_same_question(self):
        started = self._start_parked(self._single_park_graph())
        run_id, esc = started["run"], started["request"]
        persisted = _read_jsonl(run_mod._run_file(run_id))[0]["escalation"]["request"]
        self.assertEqual(persisted, esc["request"])

        self._simulate_restart(run_id)
        restored = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser())
        self.assertEqual(restored["escalation"]["request"], esc["request"])

        # an id that names nothing is refused, and leaves the restored
        # park exactly where it was
        with self.assertRaises(ValueError):
            run_mod.answer(
                run_id, request="r_deadbeefdeadbeef-req-99", pick=0,
                graphs_dir=GRAPHS_DIR, chooser=constant_chooser(),
            )
        self.assertEqual(run_mod.status(run_id)["escalation"]["request"], esc["request"])

        final = run_mod.answer(
            run_id, request=persisted, pick=0, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(),
        )["request"]
        self.assertEqual(final["outcome"], "reached")
        self.assertEqual(final["final_state"], "b")

    # -- legacy callers ---------------------------------------------------

    def test_a_legacy_answer_without_a_request_id_still_answers_and_stops(self):
        started = self._start_parked(self._single_park_graph())
        final = run_mod.answer(started["run"], pick=0)["request"]
        self.assertEqual(final["outcome"], "reached")
        self.assertEqual(final["final_state"], "b")

        other = self._start_parked(self._single_park_graph())
        stopped = run_mod.answer(other["run"], stop="not worth answering")["request"]
        self.assertEqual(stopped["outcome"], "stopped")
        self.assertEqual(stopped["reason"], "not worth answering")


if __name__ == "__main__":
    unittest.main()

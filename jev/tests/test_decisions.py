"""Tests for automation/decisions.py -- docs/design/automation.md section 5's
decision log: the row shape, append-only correction, and the bare-graph-id
file naming (`wiki-hop.jsonl`, not `wiki-hop@1.jsonl`, even though the row's
own `graph` field carries the version -- confirmed against the design doc's
own worked example, automation.md line ~1106's `"log"` field vs line ~1032's
`"graph"` field. Caught as a real bug during this task's own smoke testing:
`_append` was originally keying the filename off whatever string
`log_decision`'s caller put in the row's `graph` field, so every row landed
in `<id>@<version>.jsonl` instead of the one pooled `<id>.jsonl` file the
design's own log path names -- `test_the_log_file_is_named_by_the_bare_graph_id_even_when_the_row_carries_a_version`
below is the regression test for exactly that.
"""
from __future__ import annotations

import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import automation.decisions as decisions  # noqa: E402


class _TempDecisionsDir(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self._patch_env("JEV_DECISIONS_DIR", str(Path(self._tmp.name)))

    def _patch_env(self, name, value):
        import os

        old = os.environ.get(name)
        os.environ[name] = value

        def _restore():
            if old is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = old

        self.addCleanup(_restore)

    def _rows(self, graph_id: str) -> list[dict]:
        path = decisions.graph_log_path(graph_id)
        if not path.is_file():
            return []
        return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


class RowShapeTests(_TempDecisionsDir):
    def test_logs_the_exact_section_5_row_shape(self):
        decisions.log_decision(
            id="r_1-0001",
            context="ctx",
            options=["a", "b"],
            label=1,
            chosen="b",
            probs=[0.2, 0.8],
            source="jevlike",
            verified=None,
            floor=0.5,
            margin=0.1,
            run="r_1",
            graph="wiki-hop@1",
            state="reading",
            step=1,
            ckpt="deadbeef",
            action={"tool": "browser_click", "input": {"ref": "e1"}},
        )
        rows = self._rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        row = rows[0]
        self.assertEqual(
            set(row.keys()),
            {
                "id", "context", "options", "label", "chosen", "probs", "source", "verified",
                "floor", "margin", "run", "graph", "state", "step", "ts", "ckpt", "action", "warrant",
                "chooser",
            },
        )
        self.assertEqual(row["label"], 1)
        self.assertIsInstance(row["label"], int)
        self.assertEqual(row["graph"], "wiki-hop@1")

    def test_a_row_carries_warrant_null_when_attended(self):
        # This step's own field: written unconditionally, `None` for a run
        # that never passed one -- the row's key set is the section-5 set
        # (proven above) plus `warrant`, and an attended run's value is
        # explicitly `None`, not an absent key a reader would have to
        # special-case.
        decisions.log_decision(
            id="r_1-0002", context="c", options=["a", "b"], label=0, chosen="a", probs=[0.9, 0.1],
            source="jevlike", verified=None, floor=0.5, margin=0.1, run="r_1", graph="wiki-hop@1",
            state="reading", step=1, ckpt=None, action=None,
        )
        row = self._rows("wiki-hop")[0]
        self.assertIn("warrant", row)
        self.assertIsNone(row["warrant"])

    def test_label_must_be_a_valid_index_not_text_and_not_out_of_range(self):
        # jevlike/data.py's validate() raises on anything but an index --
        # see decisions.py's module docstring. This must be caught here,
        # before a row is ever written, not left for the exporter.
        with self.assertRaises(ValueError):
            decisions.log_decision(
                id="r_1-0001", context="c", options=["a", "b"], label=2, chosen="b", probs=[0.5, 0.5],
                source="jevlike", verified=None, floor=0.5, margin=0.1, run="r_1", graph="g@1",
                state="s", step=1, ckpt=None, action=None,
            )
        with self.assertRaises(ValueError):
            decisions.log_decision(
                id="r_1-0001", context="c", options=["a", "b"], label=-1, chosen="a", probs=[0.5, 0.5],
                source="jevlike", verified=None, floor=0.5, margin=0.1, run="r_1", graph="g@1",
                state="s", step=1, ckpt=None, action=None,
            )
        self.assertEqual(self._rows("g"), [])

    def test_the_log_file_is_named_by_the_bare_graph_id_even_when_the_row_carries_a_version(self):
        decisions.log_decision(
            id="r_1-0001", context="c", options=["a", "b"], label=0, chosen="a", probs=[0.9, 0.1],
            source="jevlike", verified=None, floor=0.5, margin=0.1, run="r_1", graph="wiki-hop@3",
            state="reading", step=1, ckpt=None, action=None,
        )
        self.assertTrue(decisions.graph_log_path("wiki-hop").is_file())
        self.assertFalse(decisions.graph_log_path("wiki-hop@3").is_file())
        # the row itself still remembers exactly which version made the pick
        self.assertEqual(self._rows("wiki-hop")[0]["graph"], "wiki-hop@3")

    def test_a_bare_graph_id_with_no_version_still_works(self):
        decisions.log_decision(
            id="r_1-0001", context="c", options=["a", "b"], label=0, chosen="a", probs=[0.9, 0.1],
            source="forced", verified=None, floor=0.5, margin=0.1, run="r_1", graph="wiki-hop",
            state="reading", step=1, ckpt=None, action=None,
        )
        self.assertTrue(decisions.graph_log_path("wiki-hop").is_file())


class CorrectionTests(_TempDecisionsDir):
    def test_a_correction_is_appended_never_rewrites_the_original(self):
        decisions.log_decision(
            id="r_1-0001", context="c", options=["a", "b"], label=0, chosen="a", probs=[0.6, 0.4],
            source="jevlike", verified=None, floor=0.5, margin=0.1, run="r_1", graph="wiki-hop@1",
            state="reading", step=1, ckpt=None, action=None,
        )
        decisions.correct_decision(graph="wiki-hop@1", id="r_1-0001", label=1, by="khoaho404@gmail.com")
        rows = self._rows("wiki-hop")
        self.assertEqual(len(rows), 2, rows)
        self.assertEqual(rows[0]["label"], 0)  # original untouched
        self.assertEqual(rows[1], {
            "id": "r_1-0001", "label": 1, "verified": "human", "by": "khoaho404@gmail.com", "ts": rows[1]["ts"],
        })

    def test_a_correction_note_is_optional_but_included_when_given(self):
        decisions.correct_decision(graph="g", id="x", label=0, by="tester", note="typo'd the ref")
        row = self._rows("g")[0]
        self.assertEqual(row["note"], "typo'd the ref")


class RunEndTests(_TempDecisionsDir):
    def test_log_run_end_writes_the_run_outcome_row(self):
        decisions.log_run_end(graph="wiki-hop", run="r_1", outcome="reached")
        rows = self._rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["run"], "r_1")
        self.assertEqual(rows[0]["outcome"], "reached")
        self.assertIn("ts", rows[0])
        self.assertIsNone(rows[0]["warrant"])

    def test_the_run_end_row_carries_the_warrant(self):
        decisions.log_run_end(graph="wiki-hop", run="r_1", outcome="reached", warrant="w_828cc0ac48f5")
        rows = self._rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["warrant"], "w_828cc0ac48f5")


class CrossProcessLockReuseTests(_TempDecisionsDir):
    """Not a from-scratch lock implementation -- decisions.py's `_append`
    must actually call through to `_paths.append_locked`, the same helper
    `jev/server.py`'s own `_log_decision` reuses (see this module's own
    docstring: "reusing its cross-process locking ... rather than a second
    implementation"). Proven with teeth: break `_paths.append_locked` and
    watch `_append` break the same way, not silently keep working through
    some other path."""

    def test_append_goes_through_paths_append_locked(self):
        import unittest.mock as mock

        import _paths

        with mock.patch.object(_paths, "append_locked", side_effect=OSError("disk is on fire")) as spy:
            with self.assertRaises(OSError):
                decisions.log_decision(
                    id="r_1-0001", context="c", options=["a", "b"], label=0, chosen="a", probs=[0.9, 0.1],
                    source="jevlike", verified=None, floor=0.5, margin=0.1, run="r_1", graph="wiki-hop@1",
                    state="reading", step=1, ckpt=None, action=None,
                )
            spy.assert_called_once()

    def test_unlike_server_pys_ad_hoc_logger_a_write_failure_is_not_swallowed(self):
        # decisions.py's own docstring: "these rows are the thing this
        # whole document exists to produce ... not a side channel" -- so,
        # unlike jev/server.py's _log_decision (best-effort, catches
        # OSError), a failed append here must propagate.
        import unittest.mock as mock

        import _paths

        with mock.patch.object(_paths, "append_locked", side_effect=OSError("no space left on device")):
            with self.assertRaises(OSError):
                decisions.log_run_end(graph="wiki-hop", run="r_1", outcome="reached")


if __name__ == "__main__":
    unittest.main()

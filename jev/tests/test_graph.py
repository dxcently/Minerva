"""Tests for automation/graph.py's own lints -- `load_graph` validating a
graph *document*, not `run.py` interpreting one. Run the same way every
other module under jev/tests runs:

    python -m unittest discover -s jev/tests -t .      (from the repo root)

graph.py's schema-shaped validation and its earlier cross-key lints already
have one representative negative case each in `test_schema.py`; this module
is the home `docs/design/triage.md` section 9 gives a rule of its own --
"*Tests (`jev/tests/test_graph.py`)*" -- so a rule that document adds has an
address rather than growing `test_schema.py`'s per-rule list sideways. It
holds section 9 step 5's `capture` lint now; step 4's `reads` lint is a
separate increment and lands here when it does.

Test method names carry the design's own names with the `test_` prefix
unittest discovery requires (`a_capture_with_no_named_group_fails_lint` ->
`test_a_capture_with_no_named_group_fails_lint`).
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from automation.graph import Graph, GraphError, load_graph  # noqa: E402


def _minimal(**overrides) -> dict:
    """The smallest graph that passes every lint rule, so each negative test
    below can break exactly one thing and know that is the only reason it
    fails (same shape as `test_schema.py`'s own helper)."""
    graph = {
        "id": "t",
        "initial": "a",
        "meta": {"jev": {"schema": 1}},
        "states": {
            "a": {"on": {"GO": "b"}},
            "b": {"type": "final"},
        },
    }
    graph.update(overrides)
    return graph


def _with_capture(params: dict) -> dict:
    """`params` in an `entry` action on the one non-final state."""
    return _minimal(
        states={
            "a": {"entry": [{"type": "capture", "params": params}], "on": {"GO": "b"}},
            "b": {"type": "final"},
        }
    )


# docs/design/triage.md section 3.2's worked pattern: `re.search` with
# MULTILINE, two participating named groups, cannot match the empty string.
NETSTAT_PORT_PATTERN = r"(?m)^\s*TCP\s+(?P<addr>\S+):8085\s+\S+\s+LISTENING\s+(?P<pid>\d+)"

# section 3.2's *first draft*, verbatim, of what became section 8's step 2
# capture -- the pattern that "matched the real command line and captured
# nothing" (M12). Every part optional, so it matches the empty string with
# every group unparticipating, and only the empty-match lint catches it.
FIRST_DRAFT_CMDLINE_PATTERN = r"(?:--cwd\s+(?P<cwd>\S+))?(?:.*--config\s+(?P<config>\S+))?"


class CaptureLintTests(unittest.TestCase):
    """docs/design/triage.md section 3.2 and section 9 step 5: `path` must be
    a `PATH_ROOTS` path, `pattern` must compile under `re.MULTILINE`, name at
    least one group, and not match the empty string, and `into` must be an
    identifier. Each rule gets its own message naming the defect, and a
    well-formed capture passes."""

    def test_a_well_formed_capture_lints_clean(self):
        g = load_graph(_with_capture({
            "path": "context.obs.text",
            "pattern": NETSTAT_PORT_PATTERN,
            "into": "case",
        }))
        self.assertIsInstance(g, Graph)
        self.assertIn(("a",), g.paths)

    def test_a_capture_with_no_named_group_fails_lint(self):
        # "a pattern with no named group is a lint error -- a capture that
        # captures nothing is a `matches`". This one *does* match the line
        # it would be pointed at, which is exactly the silent success the
        # lint exists to refuse.
        with self.assertRaises(GraphError) as ctx:
            load_graph(_with_capture({
                "path": "context.obs.text",
                "pattern": r"(?m)^\s*TCP\s+(\S+):8085\s+\S+\s+LISTENING\s+(\d+)",
                "into": "case",
            }))
        self.assertTrue(any("no named group" in e for e in ctx.exception.errors), ctx.exception.errors)

    def test_a_capture_pattern_that_matches_the_empty_string_fails_lint(self):
        # section 8's step 2 first draft, verbatim (section 3.2, M12)
        with self.assertRaises(GraphError) as ctx:
            load_graph(_with_capture({
                "path": "context.case.args",
                "pattern": FIRST_DRAFT_CMDLINE_PATTERN,
                "into": "case",
            }))
        self.assertTrue(
            any("can match the empty string" in e for e in ctx.exception.errors), ctx.exception.errors
        )
        # it *does* match this document's first draft pattern, so the error
        # is about the defect itself and not about a group count
        self.assertNotIn("no named group", " ".join(ctx.exception.errors))

    def test_an_invalid_capture_pattern_fails_lint(self):
        with self.assertRaises(GraphError) as ctx:
            load_graph(_with_capture({
                "path": "context.obs.text",
                "pattern": r"(?P<pid>\d+",
                "into": "case",
            }))
        self.assertTrue(
            any("invalid regular expression" in e for e in ctx.exception.errors), ctx.exception.errors
        )

    def test_a_capture_path_outside_path_roots_fails_lint(self):
        # `obs.text` names no root at all; `option.line` names the fifth
        # root, which stays valid only inside `choose.prefer` (`PATH_ROOTS`
        # unchanged -- a `lines` pick's line arrives as `event.option.line`).
        for path in ("obs.text", "option.line", "option"):
            with self.subTest(path=path):
                with self.assertRaises(GraphError) as ctx:
                    load_graph(_with_capture({
                        "path": path,
                        "pattern": NETSTAT_PORT_PATTERN,
                        "into": "case",
                    }))
                self.assertTrue(any(".path:" in e for e in ctx.exception.errors), ctx.exception.errors)

    def test_a_capture_into_that_is_not_an_identifier_fails_lint(self):
        for into in ("Case", "case.port", "1case", ""):
            with self.subTest(into=into):
                with self.assertRaises(GraphError) as ctx:
                    load_graph(_with_capture({
                        "path": "context.obs.text",
                        "pattern": NETSTAT_PORT_PATTERN,
                        "into": into,
                    }))
                self.assertTrue(any(".into:" in e for e in ctx.exception.errors), ctx.exception.errors)

    def test_a_capture_missing_a_required_param_fails_lint(self):
        for missing in ("path", "pattern", "into"):
            with self.subTest(missing=missing):
                params = {"path": "context.obs.text", "pattern": NETSTAT_PORT_PATTERN, "into": "case"}
                del params[missing]
                with self.assertRaises(GraphError) as ctx:
                    load_graph(_with_capture(params))
                self.assertTrue(
                    any(f"missing required key {missing!r}" in e for e in ctx.exception.errors),
                    ctx.exception.errors,
                )

    def test_an_unknown_capture_param_fails_lint(self):
        # `flags` is the tempting one -- the design fixes the flags
        # (MULTILINE, `matches`'s own), so there is nothing to configure
        with self.assertRaises(GraphError) as ctx:
            load_graph(_with_capture({
                "path": "context.obs.text",
                "pattern": NETSTAT_PORT_PATTERN,
                "into": "case",
                "flags": "IGNORECASE",
            }))
        self.assertTrue(any("unknown key 'flags'" in e for e in ctx.exception.errors), ctx.exception.errors)


if __name__ == "__main__":
    unittest.main()

"""Real-openjev tests for automation/guards.py's NLI evaluation --
docs/design/automation.md section 3. A separate file from test_guards.py
(which stays fast and fake-only) so importing `server` here -- fastapi,
uvicorn, and on every test below, the real ~9 GB openjev weights -- never
taxes the default run; the same split jev/tests/test_server_automation.py
already draws against jev/test_server.py for jevlike.

Gated exactly the way jev/test_server.py's own
`EntailTests.test_the_vendor_class_probe_from_the_incident` is:
JEV_TEST_OPENJEV=1 loads the real weights and runs for real, skipped
otherwise. This is where "neutral never passes anything" -- section 3's
actual safety claim -- is checked against the real model rather than a
fake standing in for it.

Run with jev's venv, from jev/:

    JEV_TEST_OPENJEV=1 ./.venv/bin/python -m unittest tests.test_guards_openjev -v
"""
from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

os.environ.setdefault("EIDOLON_SERVICE_PORT", "8092")
os.environ["EIDOLON_SERVICE_TOKEN"] = "test-token"
# Never touched by anything in this file (server._openjev/_entail do not
# log decisions -- only _choose does), but set anyway for the same reason
# every sibling test file sets it: so nothing here can ever be the thing
# that first imports server.py against a developer's real cache.
os.environ.setdefault("JEV_DECISIONS_LOG", str(Path(tempfile.mkdtemp()) / "decisions.jsonl"))

import server  # noqa: E402
from automation.guards import evaluate_guard, first_passing  # noqa: E402


def g(type_: str, **params) -> dict:
    return {"type": type_, "params": params}


_SKIP = unittest.skipUnless(
    os.environ.get("JEV_TEST_OPENJEV"),
    "loads the real ~9 GB openjev weights; set JEV_TEST_OPENJEV=1 to run it",
)


class RealOpenjevGuardTests(unittest.TestCase):
    @_SKIP
    def test_the_vendor_class_probe_passes_a_contradicts_guard(self):
        # docs/Decisions.md's pinned probe: 0.9109 on this box, matching
        # 0.898 from 2026-09-17 -- through guards.py's real evaluation
        # path this time (server._openjev() injected as `entail`), not
        # server._entail() called directly the way test_server.py does.
        entail = server._openjev()
        guard = g(
            "contradicts",
            premise="the server returned 500 on every request after the deploy",
            hypothesis="the service is healthy",
            threshold=0.85,
        )
        self.assertTrue(evaluate_guard(guard, {"context": {}}, entail=entail))

    @_SKIP
    def test_the_same_pair_does_not_pass_an_entails_guard_at_any_allowed_threshold(self):
        entail = server._openjev()
        guard = g(
            "entails",
            premise="the server returned 500 on every request after the deploy",
            hypothesis="the service is healthy",
            threshold=0.34,  # schema.json's own floor -- the most permissive value allowed
        )
        self.assertFalse(evaluate_guard(guard, {"context": {}}, entail=entail))

    @_SKIP
    def test_neutral_never_passes_either_kind_even_at_the_schema_floor(self):
        # A premise that never mentions colour, against a hypothesis about
        # colour: measured on this box at neutral 0.9933, entailment
        # 0.0018, contradiction 0.0049 -- both labels a guard can name
        # land well under 0.34, schema.json's own floor ("the point below
        # which a three-way softmax's argmax means nothing"). This is
        # section 3's actual safety claim -- "neutral never passes
        # anything" -- proven against the real model, not a fake standing
        # in for it.
        entail = server._openjev()
        premise = "the cat sat on the mat in the warm afternoon sun"
        entails_guard = g("entails", premise=premise, hypothesis="the mat is blue", threshold=0.34)
        contradicts_guard = g("contradicts", premise=premise, hypothesis="the mat is blue", threshold=0.34)
        self.assertFalse(evaluate_guard(entails_guard, {"context": {}}, entail=entail))
        self.assertFalse(evaluate_guard(contradicts_guard, {"context": {}}, entail=entail))

    @_SKIP
    def test_entails_passes_for_real_when_entailment_clears_threshold(self):
        entail = server._openjev()
        guard = g(
            "entails",
            premise=(
                'url: https://en.wikipedia.org/wiki/Bicycle\ntitle: Bicycle - Wikipedia\n\n'
                'heading "Bicycle" [level=1]\nA bicycle is a human-powered vehicle with two wheels.'
            ),
            hypothesis="This is the encyclopedia article about Bicycle.",
            threshold=0.6,
        )
        self.assertTrue(evaluate_guard(guard, {"context": {}}, entail=entail))

    @_SKIP
    def test_first_passing_still_makes_one_real_round_trip_for_two_guards_on_one_state(self):
        # Section 3: "All NLI guards on one state's always list are sent
        # in one entail call." Wraps the real `_openjev()` closure to
        # count calls without changing what it answers.
        real = server._openjev()
        calls = []

        def counting_entail(premise, hypotheses):
            calls.append((premise, list(hypotheses)))
            return real(premise, hypotheses)

        premise = "the server returned 500 on every request after the deploy"
        transitions = [
            {"guard": g("entails", premise=premise, hypothesis="the service is healthy", threshold=0.6)},
            {"guard": g("contradicts", premise=premise, hypothesis="the service is healthy", threshold=0.6)},
        ]
        idx = first_passing(transitions, {"context": {}}, entail=counting_entail)
        self.assertEqual(idx, 1)  # entails fails, contradicts passes
        self.assertEqual(len(calls), 1)


if __name__ == "__main__":
    unittest.main()

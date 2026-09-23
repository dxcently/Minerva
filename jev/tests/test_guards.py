"""Tests for automation/guards.py -- docs/design/automation.md section 1's
closed set of guard kinds and section 3's NLI evaluation and batching. The
deterministic kinds and combinators need nothing but fixtures; the two NLI
kinds (`entails`/`contradicts`) are evaluated here against an injected
fake `entail` (fast, deterministic, the 94%-of-the-suite kind) -- the real
~9 GB openjev weights are exercised separately, gated behind
JEV_TEST_OPENJEV=1, in tests/test_guards_openjev.py, the same split
jev/tests/test_server_automation.py already draws against jev/test_server.py
for jevlike. See guards.py's own module docstring and
docs/Decisions.md, "The automation design is accepted...".
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from automation.guards import evaluate_guard, first_passing  # noqa: E402


def g(type_: str, **params) -> dict:
    return {"type": type_, "params": params}


class _FakeEntail:
    """Stands in for `jev/server.py`'s `_openjev()` closure: same shape,
    `(premise, hypotheses) -> [{"contradiction":, "entailment":,
    "neutral":}, ...]`, one scored dict per hypothesis in the same order.
    `calls` records exactly what was asked, so a test can assert not just
    the *answer* but that batching actually happened (or did not) --
    everything this class is for."""

    def __init__(self, scores_by_hypothesis: dict | None = None, default: dict | None = None):
        self.calls: list[tuple[str, list[str]]] = []
        self._by_hyp = scores_by_hypothesis or {}
        self._default = default or {"contradiction": 0.0, "entailment": 0.0, "neutral": 1.0}

    def __call__(self, premise: str, hypotheses: list[str]) -> list[dict]:
        self.calls.append((premise, list(hypotheses)))
        return [self._by_hyp.get(h, self._default) for h in hypotheses]


def _boom(premise, hypotheses):
    raise AssertionError(f"entail should not have been called -- premise={premise!r} hypotheses={hypotheses!r}")


class EqualsTests(unittest.TestCase):
    def test_matching_strings(self):
        scope = {"context": {"h1": "Cat"}}
        self.assertTrue(evaluate_guard(g("equals", path="context.h1", value="Cat"), scope))

    def test_non_matching_strings(self):
        scope = {"context": {"h1": "Dog"}}
        self.assertFalse(evaluate_guard(g("equals", path="context.h1", value="Cat"), scope))

    def test_value_can_itself_be_a_template(self):
        scope = {"context": {"h1": "Cat", "goal": "Cat"}}
        self.assertTrue(evaluate_guard(g("equals", path="context.h1", value="{{context.goal}}"), scope))

    def test_a_missing_left_side_is_none_not_a_crash(self):
        scope = {"context": {}}
        self.assertFalse(evaluate_guard(g("equals", path="context.h1", value="Cat"), scope))

    def test_fold_matches_case_and_surrounding_whitespace_insensitively(self):
        # docs/design/judgement.md section 2.2: a link's text is "  cat  "
        # (a page's markup adding whitespace a plain `==` would never
        # forgive) and a goal is "Cat" -- `fold` clears both casefold and
        # strip at once.
        scope = {"context": {"h1": "  cat  ", "goal": "Cat"}}
        self.assertTrue(
            evaluate_guard(g("equals", path="context.h1", value="{{context.goal}}", fold=True), scope)
        )

    def test_fold_does_not_apply_to_non_strings(self):
        # `fold` on a numeric or `None` left side falls straight through to
        # the plain `==` below it, the same as if `fold` were never set --
        # never a crash from calling `.casefold()` on a non-string.
        scope = {"context": {"count": 3}}
        self.assertTrue(evaluate_guard(g("equals", path="context.count", value=3, fold=True), scope))
        self.assertFalse(evaluate_guard(g("equals", path="context.count", value="3", fold=True), scope))
        missing_scope = {"context": {}}
        self.assertFalse(
            evaluate_guard(g("equals", path="context.h1", value="Cat", fold=True), missing_scope)
        )


class ContainsTests(unittest.TestCase):
    def test_substring_in_a_string(self):
        scope = {"context": {"text": "hello world"}}
        self.assertTrue(evaluate_guard(g("contains", path="context.text", value="world"), scope))
        self.assertFalse(evaluate_guard(g("contains", path="context.text", value="xyz"), scope))

    def test_membership_in_a_list_of_strings(self):
        scope = {"context": {"visited": ["Cat", "Felis"]}}
        self.assertTrue(evaluate_guard(g("contains", path="context.visited", value="Felis"), scope))
        self.assertFalse(evaluate_guard(g("contains", path="context.visited", value="Dog"), scope))

    def test_neither_string_nor_list_is_false(self):
        scope = {"context": {"n": 5}}
        self.assertFalse(evaluate_guard(g("contains", path="context.n", value="5"), scope))


class MatchesTests(unittest.TestCase):
    def test_a_regex_search_not_full_match(self):
        scope = {"context": {"text": "error: connection refused"}}
        self.assertTrue(evaluate_guard(g("matches", path="context.text", pattern=r"^error:"), scope))
        self.assertFalse(evaluate_guard(g("matches", path="context.text", pattern=r"^refused"), scope))

    def test_multiline_flag_is_on(self):
        scope = {"context": {"text": "a\nb\nerror: c"}}
        self.assertTrue(evaluate_guard(g("matches", path="context.text", pattern=r"^error:"), scope))


class ExistsTests(unittest.TestCase):
    def test_present_and_non_null(self):
        self.assertTrue(evaluate_guard(g("exists", path="context.x"), {"context": {"x": 0}}))

    def test_absent_is_false(self):
        self.assertFalse(evaluate_guard(g("exists", path="context.x"), {"context": {}}))

    def test_present_but_null_is_false(self):
        self.assertFalse(evaluate_guard(g("exists", path="context.x"), {"context": {"x": None}}))


class CountTests(unittest.TestCase):
    def test_gte_only(self):
        scope = {"context": {"tried": ["a", "b", "c"]}}
        self.assertTrue(evaluate_guard(g("count", path="context.tried", gte=3), scope))
        self.assertFalse(evaluate_guard(g("count", path="context.tried", gte=4), scope))

    def test_lte_only(self):
        scope = {"context": {"tried": ["a", "b"]}}
        self.assertTrue(evaluate_guard(g("count", path="context.tried", lte=2), scope))
        self.assertFalse(evaluate_guard(g("count", path="context.tried", lte=1), scope))

    def test_a_non_sized_value_counts_as_zero(self):
        self.assertFalse(evaluate_guard(g("count", path="context.n", gte=1), {"context": {"n": 42}}))
        self.assertTrue(evaluate_guard(g("count", path="context.n", gte=0), {"context": {"n": 42}}))


class CombinatorTests(unittest.TestCase):
    def test_not(self):
        scope = {"context": {"x": 1}}
        self.assertFalse(evaluate_guard({"type": "not", "params": {"guard": g("exists", path="context.x")}}, scope))
        self.assertTrue(evaluate_guard({"type": "not", "params": {"guard": g("exists", path="context.y")}}, scope))

    def test_and_short_circuits_correctly(self):
        scope = {"context": {"x": 1}}
        guard = {"type": "and", "params": {"guards": [g("exists", path="context.x"), g("exists", path="context.y")]}}
        self.assertFalse(evaluate_guard(guard, scope))
        guard_both = {"type": "and", "params": {"guards": [g("exists", path="context.x"), g("exists", path="context.x")]}}
        self.assertTrue(evaluate_guard(guard_both, scope))

    def test_or(self):
        scope = {"context": {"x": 1}}
        guard = {"type": "or", "params": {"guards": [g("exists", path="context.y"), g("exists", path="context.x")]}}
        self.assertTrue(evaluate_guard(guard, scope))

    def test_combinators_nest(self):
        scope = {"context": {"x": 1}}
        guard = {
            "type": "and",
            "params": {
                "guards": [
                    g("exists", path="context.x"),
                    {"type": "not", "params": {"guard": g("exists", path="context.missing")}},
                ]
            },
        }
        self.assertTrue(evaluate_guard(guard, scope))


class NliThresholdTests(unittest.TestCase):
    """`entails` passes iff p[entailment] >= threshold; `contradicts`
    passes iff p[contradiction] >= threshold -- section 3, checked against
    a fake standing in for the real cross-encoder. `neutral` is not a
    label either kind checks, so a high `neutral` score can never itself
    make a guard pass -- see `test_a_high_neutral_score_does_not_pass_either_kind`."""

    def test_entails_passes_when_entailment_clears_threshold(self):
        entail = _FakeEntail({"h": {"contradiction": 0.05, "entailment": 0.9, "neutral": 0.05}})
        guard = g("entails", premise="p", hypothesis="h", threshold=0.6)
        self.assertTrue(evaluate_guard(guard, {"context": {}}, entail=entail))

    def test_entails_fails_when_entailment_is_under_threshold(self):
        entail = _FakeEntail({"h": {"contradiction": 0.1, "entailment": 0.55, "neutral": 0.35}})
        guard = g("entails", premise="p", hypothesis="h", threshold=0.6)
        self.assertFalse(evaluate_guard(guard, {"context": {}}, entail=entail))

    def test_entails_fails_on_a_confident_contradiction(self):
        entail = _FakeEntail({"h": {"contradiction": 0.9, "entailment": 0.05, "neutral": 0.05}})
        guard = g("entails", premise="p", hypothesis="h", threshold=0.6)
        self.assertFalse(evaluate_guard(guard, {"context": {}}, entail=entail))

    def test_contradicts_passes_when_contradiction_clears_threshold(self):
        entail = _FakeEntail({"h": {"contradiction": 0.9, "entailment": 0.05, "neutral": 0.05}})
        guard = g("contradicts", premise="p", hypothesis="h", threshold=0.6)
        self.assertTrue(evaluate_guard(guard, {"context": {}}, entail=entail))

    def test_contradicts_fails_on_a_confident_entailment(self):
        entail = _FakeEntail({"h": {"contradiction": 0.05, "entailment": 0.9, "neutral": 0.05}})
        guard = g("contradicts", premise="p", hypothesis="h", threshold=0.6)
        self.assertFalse(evaluate_guard(guard, {"context": {}}, entail=entail))

    def test_a_high_neutral_score_does_not_pass_either_kind(self):
        # The design's own safety property, stated directly: "neutral
        # never passes anything." Checked at the schema's lowest allowed
        # threshold (0.34) so there is no floor left to hide behind.
        entail = _FakeEntail({"h": {"contradiction": 0.02, "entailment": 0.03, "neutral": 0.95}})
        entails_guard = g("entails", premise="p", hypothesis="h", threshold=0.34)
        contradicts_guard = g("contradicts", premise="p", hypothesis="h", threshold=0.34)
        self.assertFalse(evaluate_guard(entails_guard, {"context": {}}, entail=entail))
        self.assertFalse(evaluate_guard(contradicts_guard, {"context": {}}, entail=entail))

    def test_threshold_falls_back_to_the_injected_default_when_params_omits_it(self):
        entail = _FakeEntail({"h": {"contradiction": 0.05, "entailment": 0.7, "neutral": 0.25}})
        guard = g("entails", premise="p", hypothesis="h")  # no threshold in params
        self.assertFalse(evaluate_guard(guard, {"context": {}}, entail=entail, default_threshold=0.8))
        self.assertTrue(evaluate_guard(guard, {"context": {}}, entail=entail, default_threshold=0.6))

    def test_premise_defaults_to_context_obs_text(self):
        entail = _FakeEntail({"h": {"contradiction": 0.0, "entailment": 0.9, "neutral": 0.1}})
        guard = g("entails", hypothesis="h", threshold=0.6)  # no "premise" in params
        self.assertTrue(evaluate_guard(guard, {"context": {"obs": {"text": "the real premise"}}}, entail=entail))
        self.assertEqual(entail.calls, [("the real premise", ["h"])])

    def test_window_tail_keeps_the_end_of_a_long_premise(self):
        entail = _FakeEntail()
        long_premise = "x" * 20 + "THE END"
        guard = g("entails", premise=long_premise, hypothesis="h", max_chars=7, window="tail")
        evaluate_guard(guard, {"context": {}}, entail=entail)
        self.assertEqual(entail.calls, [("THE END", ["h"])])

    def test_window_head_is_the_default(self):
        entail = _FakeEntail()
        long_premise = "THE START" + "x" * 20
        guard = g("entails", premise=long_premise, hypothesis="h", max_chars=9)
        evaluate_guard(guard, {"context": {}}, entail=entail)
        self.assertEqual(entail.calls, [("THE START", ["h"])])

    def test_max_chars_is_applied_after_rendering_not_before(self):
        entail = _FakeEntail()
        # the template placeholder is short; only the *rendered* text is
        # long enough to need truncating -- proves the cut happens after
        # substitution, not on the template string itself.
        guard = g("entails", premise="{{context.obs}}", hypothesis="h", max_chars=4, window="head")
        evaluate_guard(guard, {"context": {"obs": "abcdefgh"}}, entail=entail)
        self.assertEqual(entail.calls, [("abcd", ["h"])])

    def test_hypothesis_is_also_a_template(self):
        entail = _FakeEntail({"the goal is Cat": {"contradiction": 0, "entailment": 1, "neutral": 0}})
        guard = g("entails", premise="p", hypothesis="the goal is {{context.goal}}", threshold=0.6)
        self.assertTrue(evaluate_guard(guard, {"context": {"goal": "Cat"}}, entail=entail))

    def test_empty_rendered_premise_is_a_safe_false_and_warns(self):
        warnings: list[str] = []
        guard = g("entails", premise="{{context.missing}}", hypothesis="h", threshold=0.0)
        self.assertFalse(evaluate_guard(guard, {"context": {}}, warnings, entail=_boom))
        self.assertTrue(any("empty premise" in w for w in warnings))

    def test_no_entailer_available_is_a_safe_false_and_warns(self):
        # The seam's own safety net: called with no `entail` at all (the
        # default), an NLI guard degrades to exactly the same "does not
        # pass" a neutral verdict gives, never a crash and never a pass.
        warnings: list[str] = []
        guard = g("entails", premise="p", hypothesis="h", threshold=0.0)
        self.assertFalse(evaluate_guard(guard, {"context": {}}, warnings))
        self.assertEqual(len(warnings), 1)
        self.assertIn("entails", warnings[0])
        self.assertIn("no openjev connection", warnings[0])

    def test_a_graph_relying_solely_on_nli_still_evaluates_without_raising(self):
        guard = {"type": "or", "params": {"guards": [g("entails", premise="p", hypothesis="h", threshold=0.9)]}}
        self.assertFalse(evaluate_guard(guard, {"context": {}}))

    def test_nli_nested_inside_and_or_not_is_still_evaluated_through_the_injected_entail(self):
        entail = _FakeEntail({"h": {"contradiction": 0.9, "entailment": 0.0, "neutral": 0.1}})
        guard = {
            "type": "and",
            "params": {
                "guards": [
                    {"type": "not", "params": {"guard": g("entails", premise="p", hypothesis="h", threshold=0.6)}},
                    g("contradicts", premise="p", hypothesis="h", threshold=0.6),
                ]
            },
        }
        self.assertTrue(evaluate_guard(guard, {"context": {}}, entail=entail))


class FirstPassingTests(unittest.TestCase):
    """`first_passing` -- the batched form `run.py` actually calls: a
    deterministic prefix short-circuits with zero `entail` calls, and the
    moment a transition needs openjev, every NLI leaf from there to the
    end of the list goes in one round trip per distinct rendered premise."""

    def test_a_deterministic_prefix_short_circuits_before_any_entail_call(self):
        transitions = [
            {"guard": g("exists", path="context.missing")},
            {"guard": g("exists", path="context.x")},
            {"guard": g("entails", premise="p", hypothesis="h")},  # never reached
        ]
        idx = first_passing(transitions, {"context": {"x": 1}}, entail=_boom)
        self.assertEqual(idx, 1)

    def test_an_unconditional_transition_after_a_failing_deterministic_one_wins_with_no_entail_call(self):
        transitions = [{"guard": g("exists", path="context.missing")}, {}]
        idx = first_passing(transitions, {"context": {}}, entail=_boom)
        self.assertEqual(idx, 1)

    def test_every_nli_guard_on_the_list_goes_in_one_call_when_the_first_transition_already_needs_one(self):
        entail = _FakeEntail({
            "h1": {"contradiction": 0.0, "entailment": 0.2, "neutral": 0.8},
            "h2": {"contradiction": 0.9, "entailment": 0.0, "neutral": 0.1},
        })
        transitions = [
            {"guard": g("entails", premise="{{context.obs}}", hypothesis="h1", threshold=0.6)},
            {"guard": g("contradicts", premise="{{context.obs}}", hypothesis="h2", threshold=0.6)},
        ]
        idx = first_passing(transitions, {"context": {"obs": "text"}}, entail=entail)
        self.assertEqual(idx, 1)
        self.assertEqual(entail.calls, [("text", ["h1", "h2"])])

    def test_document_order_wins_after_the_batch_resolves(self):
        entail = _FakeEntail({"h1": {"contradiction": 0.0, "entailment": 0.9, "neutral": 0.1}})
        transitions = [
            {"guard": g("entails", premise="p", hypothesis="h1", threshold=0.6)},
            {"guard": g("exists", path="context.x")},
        ]
        idx = first_passing(transitions, {"context": {"x": 1}}, entail=entail)
        self.assertEqual(idx, 0)  # the NLI guard passed and comes first
        self.assertEqual(len(entail.calls), 1)

    def test_a_deterministic_transition_after_a_failing_nli_one_is_still_reached(self):
        entail = _FakeEntail({"h1": {"contradiction": 0.0, "entailment": 0.0, "neutral": 1.0}})
        transitions = [
            {"guard": g("entails", premise="p", hypothesis="h1", threshold=0.6)},
            {"guard": g("exists", path="context.x")},
        ]
        idx = first_passing(transitions, {"context": {"x": 1}}, entail=entail)
        self.assertEqual(idx, 1)
        self.assertEqual(len(entail.calls), 1)  # no second round trip for the deterministic tail

    def test_distinct_premises_are_not_silently_merged_into_one_call(self):
        entail = _FakeEntail({
            "h1": {"contradiction": 0.0, "entailment": 0.0, "neutral": 1.0},
            "h2": {"contradiction": 0.0, "entailment": 0.9, "neutral": 0.1},
        })
        transitions = [
            {"guard": g("entails", premise="one", hypothesis="h1", threshold=0.6)},
            {"guard": g("entails", premise="two", hypothesis="h2", threshold=0.6)},
        ]
        idx = first_passing(transitions, {"context": {}}, entail=entail)
        self.assertEqual(idx, 1)
        self.assertEqual(len(entail.calls), 2)
        self.assertEqual({c[0] for c in entail.calls}, {"one", "two"})

    def test_nli_nested_in_a_combinator_is_collected_into_the_same_batch(self):
        entail = _FakeEntail({"h": {"contradiction": 0.0, "entailment": 0.9, "neutral": 0.1}})
        transitions = [
            {
                "guard": {
                    "type": "and",
                    "params": {"guards": [g("exists", path="context.x"), g("entails", premise="p", hypothesis="h", threshold=0.6)]},
                }
            },
        ]
        idx = first_passing(transitions, {"context": {"x": 1}}, entail=entail)
        self.assertEqual(idx, 0)
        self.assertEqual(len(entail.calls), 1)

    def test_nothing_passing_returns_none(self):
        entail = _FakeEntail()
        transitions = [
            {"guard": g("exists", path="context.missing")},
            {"guard": g("entails", premise="p", hypothesis="h", threshold=0.6)},
        ]
        self.assertIsNone(first_passing(transitions, {"context": {}}, entail=entail))

    def test_an_empty_transition_list_returns_none(self):
        self.assertIsNone(first_passing([], {"context": {}}, entail=_boom))


if __name__ == "__main__":
    unittest.main()

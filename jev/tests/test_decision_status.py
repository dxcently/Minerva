import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from automation.run import _record_choice

class DecisionStatusTests(unittest.TestCase):
    def test_parked_choice_retains_scores_when_a_person_answers(self):
        run = SimpleNamespace(counts={"steps": 3}, decisions=[])
        options = [SimpleNamespace(label="a"), SimpleNamespace(label="b")]
        _record_choice(run, ("identify",), options, None, [0.55, 0.45], "jevlike", 0.7, 0.2, waiting=True)
        self.assertTrue(run.decisions[0]["waiting"])
        self.assertIsNone(run.decisions[0]["chosen"])
        _record_choice(run, ("identify",), options, 1, [0.55, 0.45], "human", 0.7, 0.2)
        self.assertEqual(len(run.decisions), 1)
        self.assertEqual(run.decisions[0]["chosen"], "b")
        self.assertAlmostEqual(run.decisions[0]["gap"], 0.1)
        self.assertEqual(run.decisions[0]["options"][0]["score"], 0.55)
    def test_rule_and_forced_choices_do_not_claim_model_confidence(self):
        for source in ("rule", "forced"):
            run = SimpleNamespace(counts={"steps": 1}, decisions=[])
            _record_choice(run, ("state",), [SimpleNamespace(label="a")], 0, [1.0], source, 0.7, 0.2)
            self.assertIsNone(run.decisions[0]["top_score"])
            self.assertIsNone(run.decisions[0]["options"][0]["score"])

"""HTTP-level tests for the `automation.*` methods wired into jev/server.py's
`METHODS` dict -- docs/design/automation.md section 2's service-driver
contract, over the same `/call` envelope jev/test_server.py's ChooseTests
already exercise for `choose`. A separate file, not an addition to
jev/test_server.py itself: that file's 33 tests must stay exactly as they
are (see its own module docstring and this task's brief).

Run with jev's venv:

    jev/.venv/bin/python jev/tests/test_server_automation.py

Scores through the real jevlike checkpoint, same as ChooseTests -- "fast
enough on CPU that faking it would not save anything" (test_server.py's own
words). Uses a tiny inline synthetic graph, not the two worked examples --
this file is about proving the *wiring* (server.py's `_automation_start`/
`_automation_step`/`_automation_status`, `METHODS`, `GRAPHS_DIR`, `_ckpt_id`),
which the worked examples already prove end to end at the `automation.run`
layer in jev/tests/test_run.py; a browser/bash round trip over HTTP here
would only add latency, not coverage.
"""
from __future__ import annotations

import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

os.environ.setdefault("EIDOLON_SERVICE_PORT", "8091")
# Forced, not setdefault -- see jev/test_server.py's own comment on why an
# already-set EIDOLON_SERVICE_TOKEN in the ambient environment must not be
# trusted for a test run.
os.environ["EIDOLON_SERVICE_TOKEN"] = "test-token"
os.environ.setdefault("JEV_DECISIONS_LOG", str(Path(__file__).resolve().parent / "_scratch_decisions.jsonl"))

# automation/decisions.py's per-graph log directory -- separate from the
# flat JEV_DECISIONS_LOG above (server.py's own ad-hoc `choose` log; see
# decisions.py's module docstring for why the two are intentionally
# distinct paths). Isolated to a throwaway directory for this whole file,
# the same reasoning as jev/test_server.py isolating JEV_DECISIONS_LOG:
# without it, every automation.start below would append real rows to a
# developer's actual cache.
_DECISIONS_TMP = tempfile.TemporaryDirectory()
os.environ["JEV_DECISIONS_DIR"] = _DECISIONS_TMP.name

# automation/run.py's own park-persistence directory (`runs_dir()`) --
# GRAPH_3 below parks for real over the actual HTTP envelope, so without
# this isolation every test that reaches it would write a real
# `<run-id>.jsonl` snapshot under a developer's real cache, and could even
# create the `runs/` directory itself for the first time. Same reasoning
# as JEV_DECISIONS_DIR just above; a separate temp directory, not the same
# one, since the two are deliberately distinct write paths (run.py's own
# module docstring) and a test that means to isolate one must not
# accidentally isolate the other instead.
_RUNS_TMP = tempfile.TemporaryDirectory()
os.environ["JEV_RUNS_DIR"] = _RUNS_TMP.name

import server  # noqa: E402
from fastapi.testclient import TestClient  # noqa: E402

GRAPH_2 = {
    "id": "http-smoke",
    "initial": "a",
    "meta": {"jev": {"schema": 1}},
    "context": {},
    "states": {
        "a": {
            "meta": {"choose": {"from": {"menu": [{"label": "left"}, {"label": "right"}]}}},
            "on": {"PICK": "b"},
        },
        "b": {"type": "final", "output": {"picked": "{{event.option.label}}"}},
    },
}

GRAPH_1 = {
    "id": "http-smoke-forced",
    "initial": "a",
    "meta": {"jev": {"schema": 1}},
    "context": {},
    "states": {
        "a": {"meta": {"choose": {"from": {"menu": [{"label": "only"}]}}}, "on": {"PICK": "b"}},
        "b": {"type": "final"},
    },
}

# A floor no real jevlike score over two short, undistinguished labels is
# ever going to clear (six nines) -- deterministic enough to force a park
# through the *real* checkpoint, the same "do not fake what is fast enough
# to run for real" choice this file already made for GRAPH_1/GRAPH_2, now
# extended to automation.answer/.stop/.runs (section 4's parking, not just
# section 2's plain start/step/status).
GRAPH_3 = {
    "id": "http-smoke-park",
    "initial": "a",
    "meta": {"jev": {"schema": 1, "defaults": {"floor": 0.999999, "margin": 0.000001}}},
    "context": {},
    "states": {
        "a": {
            "meta": {"choose": {"from": {"menu": [{"label": "left"}, {"label": "right"}]}}},
            "on": {"PICK": "b"},
        },
        "b": {"type": "final", "output": {"picked": "{{event.option.label}}"}},
    },
}


class AutomationHttpTests(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(server.app)
        # A fresh in-memory run registry per test -- automation.run's
        # module-level _RUNS dict otherwise accumulates across the whole
        # process, which would make these tests order-dependent.
        import automation.run as automation_run

        automation_run._RUNS.clear()

    def _call(self, method: str, args: dict) -> dict:
        resp = self.client.post(
            "/call", json={"method": method, "args": args}, headers={"Authorization": "Bearer test-token"}
        )
        self.assertEqual(resp.status_code, 200)
        return resp.json()

    def test_start_with_an_inline_graph_and_a_forced_pick_reaches_final_over_http(self):
        started = self._call("automation.start", {"graph": GRAPH_1, "input": {}})
        self.assertTrue(started["ok"], started)
        result = started["result"]
        self.assertIn("run", result)
        self.assertEqual(result["request"]["kind"], "final")
        self.assertEqual(result["request"]["outcome"], "reached")

    def test_start_then_step_through_a_real_choose_over_http(self):
        started = self._call("automation.start", {"graph": GRAPH_2, "input": {}})
        self.assertTrue(started["ok"], started)
        # a menu source with no tool actions needs no `act` round trip at
        # all -- GRAPH_2's PICK has none, so the very first response is
        # already the final report, with a real jevlike-scored choice
        # baked into which branch it took.
        result = started["result"]
        self.assertEqual(result["request"]["kind"], "final")
        self.assertEqual(result["request"]["outcome"], "reached")
        self.assertIn(result["request"]["output"]["picked"], ("left", "right"))

    def test_step_on_an_unknown_run_is_a_clean_ok_false_not_a_500(self):
        stepped = self._call("automation.step", {"run": "r_does_not_exist", "request": "x", "result": {}})
        self.assertFalse(stepped["ok"])
        self.assertIn("r_does_not_exist", stepped["error"])

    def test_status_reports_a_finished_run(self):
        started = self._call("automation.start", {"graph": GRAPH_1, "input": {}})
        run_id = started["result"]["run"]
        status = self._call("automation.status", {"run": run_id})
        self.assertTrue(status["ok"], status)
        self.assertTrue(status["result"]["finished"])
        self.assertEqual(status["result"]["outcome"], "reached")

    def test_status_on_an_unknown_run_is_a_clean_ok_false_not_a_500(self):
        status = self._call("automation.status", {"run": "r_nope"})
        self.assertFalse(status["ok"])

    def test_start_without_a_graph_is_a_clean_ok_false_not_a_500(self):
        started = self._call("automation.start", {"input": {}})
        self.assertFalse(started["ok"])
        self.assertIn("graph", started["error"].lower())

    def test_ckpt_id_is_attached_to_the_logged_decision(self):
        import json

        import automation.decisions as decisions

        started = self._call("automation.start", {"graph": GRAPH_2, "input": {}})
        self.assertTrue(started["ok"], started)
        path = decisions.graph_log_path("http-smoke")
        self.assertTrue(path.is_file())
        lines = [json.loads(l) for l in path.read_text(encoding="utf-8").splitlines() if l.strip()]
        # S45: GRAPH_2 finishes inside this same `automation.start` call
        # (see `test_start_then_step_through_a_real_choose_over_http`
        # above -- no `act` round trip needed), so `decisions.log_run_end`
        # (wired from `Run._settle`) now appends its own run-end row
        # (`{run, outcome, ts}`, no `id`) right after the decision row in
        # this same file. `[-1]` would grab that marker instead; filter to
        # the row this test actually means.
        decision_rows = [r for r in lines if "id" in r]
        self.assertEqual(len(decision_rows), 1, lines)
        row = decision_rows[-1]
        self.assertEqual(row["ckpt"], server._ckpt_id())
        self.assertIsNotNone(row["ckpt"])  # the real checkpoint is on disk in this dev environment

    # --- section 4: escalation parks, over the real /call envelope ---------

    def test_a_park_over_http_can_be_answered_via_automation_answer(self):
        started = self._call("automation.start", {"graph": GRAPH_3, "input": {}})
        self.assertTrue(started["ok"], started)
        run_id = started["result"]["run"]
        esc = started["result"]["request"]
        self.assertEqual(esc["kind"], "escalate")
        self.assertEqual([o["label"] for o in esc["options"]], ["left", "right"])
        self.assertIn("jev_resume", esc["answer"])

        answered = self._call("automation.answer", {"run": run_id, "pick": 0, "by": "test"})
        self.assertTrue(answered["ok"], answered)
        final = answered["result"]["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "reached")
        self.assertEqual(final["output"]["picked"], "left")

    def test_an_out_of_menu_pick_over_http_is_a_clean_ok_false_not_a_500(self):
        started = self._call("automation.start", {"graph": GRAPH_3, "input": {}})
        run_id = started["result"]["run"]
        answered = self._call("automation.answer", {"run": run_id, "pick": "not-a-real-option"})
        self.assertFalse(answered["ok"])
        # the rejected pick did not disturb the park -- a real one still works
        retried = self._call("automation.answer", {"run": run_id, "pick": 1})
        self.assertTrue(retried["ok"], retried)
        self.assertEqual(retried["result"]["request"]["output"]["picked"], "right")

    def test_automation_answer_without_pick_or_stop_is_a_clean_ok_false_not_a_500(self):
        started = self._call("automation.start", {"graph": GRAPH_3, "input": {}})
        run_id = started["result"]["run"]
        answered = self._call("automation.answer", {"run": run_id})
        self.assertFalse(answered["ok"])
        self.assertIn("pick", answered["error"].lower())

    def test_answer_on_an_unknown_run_is_a_clean_ok_false_not_a_500(self):
        answered = self._call("automation.answer", {"run": "r_does_not_exist", "pick": 0})
        self.assertFalse(answered["ok"])
        self.assertIn("r_does_not_exist", answered["error"])

    def test_stop_over_http_ends_a_parked_run_and_answers_with_the_run_record_not_request(self):
        started = self._call("automation.start", {"graph": GRAPH_3, "input": {}})
        run_id = started["result"]["run"]
        stopped = self._call("automation.stop", {"run": run_id, "reason": "http test stop"})
        self.assertTrue(stopped["ok"], stopped)
        record = stopped["result"]
        self.assertEqual(record["outcome"], "stopped")
        self.assertEqual(record["reason"], "http test stop")
        self.assertTrue(record["finished"])
        self.assertNotIn("request", record)

    def test_stop_without_a_reason_is_a_clean_ok_false_not_a_500(self):
        started = self._call("automation.start", {"graph": GRAPH_1, "input": {}})
        run_id = started["result"]["run"]
        stopped = self._call("automation.stop", {"run": run_id})
        self.assertFalse(stopped["ok"])

    def test_runs_over_http_lists_started_runs(self):
        started1 = self._call("automation.start", {"graph": GRAPH_1, "input": {}})
        started2 = self._call("automation.start", {"graph": GRAPH_3, "input": {}})
        listing = self._call("automation.runs", {})
        self.assertTrue(listing["ok"], listing)
        ids = {r["run"] for r in listing["result"]["runs"]}
        self.assertIn(started1["result"]["run"], ids)
        self.assertIn(started2["result"]["run"], ids)

    # --- docs/design/unattended.md section 5: automation.warrant, and warrant --
    # threaded through automation.start/.status. wiki-hop's own `open` entry
    # is a `tool` action (`browser_open`), so a real `automation.start` under
    # a *correct* warrant yields `{"kind": "act", ...}` immediately -- no
    # `choose` phase, no real jevlike scoring, the same "do not need the
    # checkpoint for this" shape GRAPH_1's forced pick already gave the tests
    # above; a *mismatched* warrant is refused by `warrant_mod.check` before
    # `Run` is ever constructed (`run.py`'s `start`), so it never reaches the
    # graph's entry actions either. Neither of the four tests below touches
    # jevlike at all.

    def test_automation_warrant_answers_the_shipped_wiki_hop_block_over_http(self):
        result = self._call("automation.warrant", {"graph": "wiki-hop"})
        self.assertTrue(result["ok"], result)
        self.assertEqual(result["result"]["id"], "w_378be6b7176d")
        self.assertEqual(
            result["result"]["warrant"],
            {
                "graph": "wiki-hop@1",
                "sha256": "384f87f86068d3841429c5f66f88f1cc4169d2b9b46f50f52023e165f7660f91",
                "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
                "origins": ["https://en.wikipedia.org"],
                "commands": [],
                "actions": 70,
                "wall_s": 900,
            },
        )

    def test_automation_warrant_on_an_undeclared_graph_is_a_clean_ok_false(self):
        result = self._call("automation.warrant", {"graph": GRAPH_1})
        self.assertFalse(result["ok"])
        self.assertIn("http-smoke-forced", result["error"])
        self.assertIn("warrant", result["error"].lower())

    def test_start_with_a_mismatched_warrant_is_a_clean_ok_false_with_the_block_in_the_message(self):
        bad = {
            "graph": "wiki-hop@1",
            "sha256": "384f87f86068d3841429c5f66f88f1cc4169d2b9b46f50f52023e165f7660f91",
            "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
            "origins": ["https://en.wikipedia.org"],
            "commands": [],
            "actions": 71,  # the one field that differs from the graph's own block
            "wall_s": 900,
        }
        started = self._call(
            "automation.start",
            {"graph": "wiki-hop", "input": {"start": "Cat", "goal": "Felis"}, "warrant": bad},
        )
        self.assertFalse(started["ok"])
        self.assertIn("wiki-hop@1", started["error"])
        # the correct canonical block, echoed whole -- not the caller's bad
        # one reflected back, and not merely "a mismatch" with no way for
        # the caller to see what would have worked.
        self.assertIn('"actions":70', started["error"])
        self.assertIn('"wall_s":900', started["error"])

    def test_a_warranted_start_reports_the_warrant_in_status(self):
        good = {
            "graph": "wiki-hop@1",
            "sha256": "384f87f86068d3841429c5f66f88f1cc4169d2b9b46f50f52023e165f7660f91",
            "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
            "origins": ["https://en.wikipedia.org"],
            "commands": [],
            "actions": 70,
            "wall_s": 900,
        }
        started = self._call(
            "automation.start",
            {"graph": "wiki-hop", "input": {"start": "Cat", "goal": "Felis"}, "warrant": good},
        )
        self.assertTrue(started["ok"], started)
        # browser_open, dispatched before any choose phase -- confirms this
        # test never needed jevlike to reach the assertions below.
        self.assertEqual(started["result"]["request"]["kind"], "act")
        run_id = started["result"]["run"]
        status = self._call("automation.status", {"run": run_id})
        self.assertTrue(status["ok"], status)
        self.assertEqual(status["result"]["warrant"]["id"], "w_378be6b7176d")
        self.assertEqual(status["result"]["warrant"]["actions"], 70)
        self.assertEqual(status["result"]["warrant"]["origins"], ["https://en.wikipedia.org"])


# --- section 3, end to end through a real run, not just guards.py direct -----
# Every test above exercises real jevlike (small, fast, always on). These
# two are the one place in the whole suite that drive an actual
# automation.start through server.py's `entail=lambda ...: _openjev()(...)`
# wiring (jev/server.py's `_automation_start`) rather than calling
# automation.guards or server._openjev directly -- gated the same as every
# other real-openjev test in this repo (JEV_TEST_OPENJEV=1), since it loads
# the real ~9 GB weights.


class RealOpenjevAutomationHttpTests(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(server.app)
        import automation.run as automation_run

        automation_run._RUNS.clear()

    def _call(self, method: str, args: dict) -> dict:
        resp = self.client.post(
            "/call", json={"method": method, "args": args}, headers={"Authorization": "Bearer test-token"}
        )
        self.assertEqual(resp.status_code, 200)
        return resp.json()

    @unittest.skipUnless(
        os.environ.get("JEV_TEST_OPENJEV"),
        "loads the real ~9 GB openjev weights; set JEV_TEST_OPENJEV=1 to run it",
    )
    def test_a_contradicts_always_guard_routes_through_the_real_model_over_http(self):
        # docs/Decisions.md's pinned probe again -- this time reached
        # through the full stack an actual driver would use: automation.
        # start -> automation/run.py's `_check_always` -> guards.
        # first_passing -> the injected `entail` -> server._openjev().
        graph = {
            "id": "http-smoke-contradicts",
            "initial": "check",
            "meta": {"jev": {"schema": 1}},
            "context": {"obs": {"text": "the server returned 500 on every request after the deploy"}},
            "states": {
                "check": {
                    "always": [
                        {
                            "guard": {
                                "type": "contradicts",
                                "params": {"hypothesis": "the service is healthy", "threshold": 0.85},
                            },
                            "target": "unhealthy",
                        },
                        {"target": "unknown"},
                    ],
                },
                "unhealthy": {"type": "final", "output": {"verdict": "unhealthy"}},
                "unknown": {"type": "final", "output": {"verdict": "unknown"}},
            },
        }
        started = self._call("automation.start", {"graph": graph, "input": {}})
        self.assertTrue(started["ok"], started)
        final = started["result"]["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "reached")
        self.assertEqual(final["output"]["verdict"], "unhealthy")

    @unittest.skipUnless(
        os.environ.get("JEV_TEST_OPENJEV"),
        "loads the real ~9 GB openjev weights; set JEV_TEST_OPENJEV=1 to run it",
    )
    def test_neutral_passes_neither_always_guard_over_http(self):
        # Section 3's actual safety claim, once more at the top of the
        # stack: a premise/hypothesis pair the real model scores as
        # neutral must fall through both an entails and a contradicts
        # `always` guard and reach the graph's own fallback branch, not
        # get stuck or silently take either labelled one.
        graph = {
            "id": "http-smoke-neutral",
            "initial": "check",
            "meta": {"jev": {"schema": 1}},
            "context": {"obs": {"text": "the cat sat on the mat in the warm afternoon sun"}},
            "states": {
                "check": {
                    "always": [
                        {
                            "guard": {
                                "type": "entails",
                                "params": {"hypothesis": "the mat is blue", "threshold": 0.34},
                            },
                            "target": "entailed",
                        },
                        {
                            "guard": {
                                "type": "contradicts",
                                "params": {"hypothesis": "the mat is blue", "threshold": 0.34},
                            },
                            "target": "contradicted",
                        },
                        {"target": "neither"},
                    ],
                },
                "entailed": {"type": "final"},
                "contradicted": {"type": "final"},
                "neither": {"type": "final"},
            },
        }
        started = self._call("automation.start", {"graph": graph, "input": {}})
        self.assertTrue(started["ok"], started)
        final = started["result"]["request"]
        self.assertEqual(final["outcome"], "reached")
        self.assertEqual(final["final_state"], "neither")


# --- S28: automation.lint over the wire ----------------------------------------
# jev/automation/graph.py has always had a real linter -- jev/server.py's
# METHODS dict just had no "lint" key, so the graph editor's LINT button
# could only ever see `{"ok":false,"error":"unknown method
# 'automation.lint'"}`. Every test below goes through TestClient's real
# ASGI request/response cycle -- routing, header auth, JSON body parsing,
# the METHODS lookup itself -- the same "HTTP-level" standard this file's
# own module docstring sets for AutomationHttpTests above. Calling
# server._automation_lint directly would not catch the one failure this
# task exists to fix: a method that exists as a function but is never
# reachable through METHODS.


def _rust_parse_lint_answer(text: str):
    """A deliberately literal Python port of one function,
    `crates/cli/src/serve_host.rs::parse_lint_answer` -- not a second
    implementation of the *linter* (graph.py owns that, alone; see S28's
    own task description), but a line-for-line mirror of how the one real
    consumer reads this method's answer, so this file can check itself
    against that consumer without building crates/cli (scope: "read them
    to learn the consumer's expected shape, edit neither"). Same branches,
    same order: not JSON -> unavailable; missing option_tokens/
    context_tokens -> unavailable; errors present but not a non-empty
    array -> Clean when empty, unavailable when the key is absent at all;
    otherwise Errors. Returns a `(tag, errors)` pair mirroring
    `lint_kind()` in that same Rust file's own tests."""
    try:
        v = json.loads(text)
    except json.JSONDecodeError:
        return ("Unavailable", None)
    if not isinstance(v, dict):
        return ("Unavailable", None)
    option_tokens = v.get("option_tokens")
    context_tokens = v.get("context_tokens")
    if not isinstance(option_tokens, int) or isinstance(option_tokens, bool):
        return ("Unavailable", None)
    if not isinstance(context_tokens, int) or isinstance(context_tokens, bool):
        return ("Unavailable", None)
    errors = v.get("errors")
    if not isinstance(errors, list):
        return ("Unavailable", None)
    if errors:
        return ("Errors", errors)
    return ("Clean", None)


_SPINNY_GRAPH = {
    "id": "http-lint-spin",
    "initial": "a",
    "meta": {"jev": {"schema": 1}},
    # The exact fixture jev/tests/test_schema.py's own AlwaysSpinLintTests
    # uses for `_lint_always_spin` -- reused, not reinvented, so a "dirty"
    # answer here proves the real spin rule rather than a stand-in for it:
    # an `always` transition with no guard, no `tool` action and no
    # `target` can only spin, advancing no step/action/visit, until
    # `wall_s`.
    "states": {"a": {"always": {}, "on": {"GO": "b"}}, "b": {"type": "final"}},
}


class LintHttpTests(unittest.TestCase):
    def setUp(self):
        self.client = TestClient(server.app)

    def _call(self, method: str, args: dict) -> dict:
        resp = self.client.post(
            "/call", json={"method": method, "args": args}, headers={"Authorization": "Bearer test-token"}
        )
        self.assertEqual(resp.status_code, 200)
        return resp.json()

    def test_automation_lint_is_reachable_through_methods(self):
        # The literal bug: a "lint" key absent from server.METHODS is the
        # entire reason the editor's LINT button had nothing to call.
        self.assertIn("automation.lint", server.METHODS)

    def test_a_clean_graph_sent_as_raw_json_text_lints_clean_over_http(self):
        # The exact shape crates/web/src/pages/jev.rs's JevHost::lint
        # sends: `args = json!({"graph": json})` with `json: &str` the
        # <textarea>'s raw bytes (checked directly against that file and
        # crates/cli/src/serve_host.rs -- see server.py's own
        # _automation_lint docstring) -- so "graph" arrives as a JSON
        # *string* holding the document's own text, never a nested
        # object. json.dumps(GRAPH_2) reproduces exactly that.
        answer = self._call("automation.lint", {"graph": json.dumps(GRAPH_2)})
        self.assertTrue(answer["ok"], answer)
        result = answer["result"]
        self.assertEqual(result["errors"], [])
        self.assertEqual(result["option_tokens"], server.OPTION_TOKENS)
        self.assertEqual(result["context_tokens"], server.CONTEXT_TOKENS)
        # And the real consumer's own parser, run against exactly the text
        # it would receive once `service::call` unwraps the envelope
        # (crates/tools/src/service.rs: a non-string `result` comes back
        # "as JSON" -- i.e. this object, re-serialised).
        tag, _ = _rust_parse_lint_answer(json.dumps(result))
        self.assertEqual(tag, "Clean")

    def test_a_graph_with_a_real_spin_transition_lints_dirty_over_http(self):
        answer = self._call("automation.lint", {"graph": json.dumps(_SPINNY_GRAPH)})
        self.assertTrue(answer["ok"], answer)
        result = answer["result"]
        self.assertTrue(any("spin" in e.lower() for e in result["errors"]), result["errors"])
        self.assertEqual(result["option_tokens"], server.OPTION_TOKENS)
        self.assertEqual(result["context_tokens"], server.CONTEXT_TOKENS)
        tag, errors = _rust_parse_lint_answer(json.dumps(result))
        self.assertEqual(tag, "Errors")
        self.assertTrue(any("spin" in e.lower() for e in errors))

    def test_clean_and_dirty_produce_visibly_different_answers(self):
        # Non-vacuous, per this task's own method: a lint that always says
        # "no errors" (or always says "errors") regardless of input would
        # pass every test above in isolation. This is the one that could
        # not pass against a lint like that.
        clean = self._call("automation.lint", {"graph": json.dumps(GRAPH_2)})["result"]
        dirty = self._call("automation.lint", {"graph": json.dumps(_SPINNY_GRAPH)})["result"]
        self.assertEqual(clean["errors"], [])
        self.assertNotEqual(dirty["errors"], [])
        self.assertNotEqual(clean["errors"], dirty["errors"])
        self.assertEqual(_rust_parse_lint_answer(json.dumps(clean))[0], "Clean")
        self.assertEqual(_rust_parse_lint_answer(json.dumps(dirty))[0], "Errors")

    def test_an_inline_object_lints_the_same_as_its_own_json_text(self):
        # graph.py::load_graph's dict branch (automation.start's "inline
        # object" case) is not the shape the real consumer sends today,
        # but it is the same `_load_doc` this method already goes
        # through for the string case -- one implementation either way,
        # so both inputs must agree rather than one being a second,
        # untested path.
        as_text = self._call("automation.lint", {"graph": json.dumps(GRAPH_1)})["result"]
        as_object = self._call("automation.lint", {"graph": GRAPH_1})["result"]
        self.assertEqual(as_text, as_object)

    def test_lint_without_a_graph_is_a_clean_ok_false_not_a_500(self):
        answer = self._call("automation.lint", {})
        self.assertFalse(answer["ok"])
        self.assertIn("graph", answer["error"].lower())

    def test_unparseable_graph_text_is_ok_true_with_a_lint_error_not_a_500(self):
        # Malformed JSON is a load failure `graph.py::_load_doc` folds
        # into the same `GraphError.errors` list as any other lint
        # finding (its own `try: return json.loads(text) except
        # json.JSONDecodeError as e: raise GraphError([f"invalid JSON:
        # {e}"])`), not a server-level exception -- so this is ok: true
        # with a populated `errors`, the same shape as the spin case
        # above, not ok: false. A 500 here (or an unhandled exception
        # anywhere in the middle) is what this test actually guards.
        answer = self._call("automation.lint", {"graph": "{not json"})
        self.assertTrue(answer["ok"], answer)
        self.assertTrue(any("invalid json" in e.lower() for e in answer["result"]["errors"]), answer)
        self.assertEqual(_rust_parse_lint_answer(json.dumps(answer["result"]))[0], "Errors")

    def test_lint_creates_no_run_and_writes_no_decision_row(self):
        # Purely diagnostic: no chooser call, no run, no decision-log
        # write -- so the editor can lint on every keystroke with no
        # side effects piling up. Its own graph id, never started
        # elsewhere in this file (GRAPH_2's own log already exists by
        # this point -- AutomationHttpTests starts it), so "the file does
        # not exist yet" is a real assertion here rather than an artifact
        # of test order.
        import automation.decisions as decisions
        import automation.run as automation_run

        never_run_graph = {**GRAPH_2, "id": "http-lint-no-side-effects"}
        before = set(automation_run._RUNS.keys())
        log_path = decisions.graph_log_path(never_run_graph["id"])
        self.assertFalse(log_path.exists())
        self._call("automation.lint", {"graph": json.dumps(never_run_graph)})
        self.assertEqual(set(automation_run._RUNS.keys()), before, "automation.lint must not create a run")
        self.assertFalse(log_path.exists(), "automation.lint must not write a decision row")


if __name__ == "__main__":
    unittest.main()

"""Tests for automation/run.py -- the interpreter loop and the
service-driver contract (docs/design/automation.md section 2) plus the hop
budget (section 4's part of it, in scope for this task; see
automation/__init__.py and run.py's own module docstring for what is not).

Both worked-example graphs run end to end here, against a scripted fake
driver standing in for extensions/jev/tools/run.rn + eidolon::dispatch --
the one thing this whole package may never do itself (see run.py's module
docstring, "The one architectural rule"). Also covers what "done" requires
beyond the happy path: a budget stopping a run outright; a confidence
floor or an unhandled EMPTY *parking* the run instead (section 4) and
`automation.answer` resuming it -- a pick by index, a pick by label, an
out-of-menu pick rejected without disturbing the park, `stop` ending the
run as `"stopped"`; `automation.stop` reaching a run mid-`act` as well as
mid-park; `automation.runs` listing what this process has tracked;
the escalations budget itself (a second park past it exhausts the run,
same as any other budget); EMPTY/ERROR event handling; forced
(single-option) picks; request idempotency (`step` against a stale or
already-consumed id, `step` against a parked run, `answer` against a run
that has nothing parked); and the stale-ref check ("a PICK whose ref is
not in the current obs is an ERROR").
"""
from __future__ import annotations

import re
import sys
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import automation.a11y as a11y  # noqa: E402
import automation.decisions as decisions  # noqa: E402
import automation.graph as graph_mod  # noqa: E402
import automation.options as options  # noqa: E402
import automation.run as run_mod  # noqa: E402
import automation.warrant as warrant_mod  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
GRAPHS_DIR = REPO_ROOT / "extensions" / "jev" / "graphs"
FIXTURES_DIR = Path(__file__).resolve().parent / "fixtures" / "wiki"


# --- shared test scaffolding -----------------------------------------------------


def constant_chooser(prefer_index: int = 0, top: float = 0.9, rest: float = 0.05):
    """A chooser that clearly prefers whichever option sorts to
    `prefer_index` (clamped to the actual list length), for tests that do
    not care about scoring itself, only about what happens after a choice
    is made."""

    def chooser(context: str, labels: list[str]) -> list[float]:
        idx = min(prefer_index, len(labels) - 1)
        return [top if i == idx else rest for i in range(len(labels))]

    return chooser


def prefer_label(label: str, top: float = 0.9, rest: float = 0.05):
    """A chooser that prefers whichever option's label equals `label`,
    wherever it lands in the list -- for tests that want a specific option
    taken regardless of how many others surround it (unlike
    `constant_chooser`, which is pinned to a position and so breaks the
    moment a fixture grows an extra option)."""

    def chooser(context: str, labels: list[str]) -> list[float]:
        idx = labels.index(label)
        return [top if i == idx else rest for i in range(len(labels))]

    return chooser


def refusing_chooser(context: str, labels: list[str]) -> list[float]:
    raise AssertionError(f"chooser should not have been called -- labels were {labels!r}")


class FakeDriver:
    """Stands in for run.rn + eidolon::dispatch: `answer(request)` returns
    the `{"text": ...}` / `{"error": ...}` a real dispatch would. `script`
    maps a tool name to either a fixed dict (every call to that tool gets
    it), a list (consumed one at a time, in order), or a callable
    `(request) -> dict` for tools whose answer depends on the input."""

    def __init__(self, script: dict):
        self.script = script
        self.calls: list[dict] = []

    def answer(self, request: dict) -> dict:
        self.calls.append(request)
        tool = request["tool"]
        if tool not in self.script:
            raise AssertionError(f"no scripted response for tool {tool!r}; request={request}")
        entry = self.script[tool]
        if callable(entry):
            return entry(request)
        if isinstance(entry, list):
            if not entry:
                raise AssertionError(f"scripted responses for {tool!r} ran out; request={request}")
            return entry.pop(0)
        return entry


def run_to_completion(run_id: str, request: dict, driver: FakeDriver, max_hops: int = 200) -> dict:
    hops = 0
    while request.get("kind") == "act":
        hops += 1
        if hops > max_hops:
            raise AssertionError(f"more than {max_hops} act requests -- probably not terminating")
        result = driver.answer(request)
        request = run_mod.step(run_id, request["id"], result)["request"]
    return request


class _RecordingLock:
    """Wraps a real `threading.Lock` so a test can prove the *code under
    test* actually acquires and holds it, not just that a bare lock would
    exclude if it were still there -- true of any lock by construction, and
    blind to a `with self._lock:` quietly dropped from `Run.answer`/
    `Run.stop`. Mirrors jev/test_server.py's own `_RecordingLock`, which
    proves `_jevlike_lock`/`_openjev_lock` the same way; there is no shared
    test-utils module between the two files, so this is a deliberate,
    small, single-purpose copy rather than a cross-file import that would
    drag in fastapi/uvicorn just to borrow twenty lines."""

    def __init__(self, real_lock):
        self._real = real_lock
        self.acquire_count = 0
        self.max_concurrent = 0
        self._current = 0
        self._meta = threading.Lock()  # guards the two counters, not `_real`

    def acquire(self, *args, **kwargs):
        ok = self._real.acquire(*args, **kwargs)
        if ok:
            with self._meta:
                self._current += 1
                self.max_concurrent = max(self.max_concurrent, self._current)
            self.acquire_count += 1
            # Widen the held window so a second, genuinely concurrent caller
            # has time to reach its own `acquire` and be seen blocking on
            # `_real` -- without this a fast critical section could finish
            # before a broken exclusion ever had a chance to show itself.
            time.sleep(0.1)
        return ok

    def release(self, *args, **kwargs):
        with self._meta:
            self._current -= 1
        self._real.release(*args, **kwargs)

    def __enter__(self):
        self.acquire()
        return self

    def __exit__(self, *exc):
        self.release()
        return False


class _IsolatedDecisionsDir(unittest.TestCase):
    """Every test below writes real decision-log rows (run.py does not
    have a way to disable that, on purpose -- the log is not optional
    machinery), and every test that parks a run now also writes a real
    run-persistence snapshot (`_park`'s own `_persist_park`/
    `_persist_resolved` -- same reasoning, no way to disable it and
    should not be one). Point *both* at a throwaway directory per test so
    nothing lands in a developer's real cache, no `runs/` directory is
    ever created beside the production decisions log, and tests cannot
    see each other's rows or run files."""

    def setUp(self):
        import tempfile

        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self._set_env("JEV_DECISIONS_DIR", self._tmp.name)
        self._runs_tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._runs_tmp.cleanup)
        self._set_env("JEV_RUNS_DIR", self._runs_tmp.name)

    def _set_env(self, name, value):
        import os

        old = os.environ.get(name)
        os.environ[name] = value
        self.addCleanup(lambda: (os.environ.pop(name, None) if old is None else os.environ.__setitem__(name, old)))


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


# --- wiki-hop, end to end ---------------------------------------------------------

CAT_PAGE = (
    "url: https://en.wikipedia.org/wiki/Cat\n"
    "title: Cat - Wikipedia\n"
    "\n"
    # A realistic sidebar/nav region ahead of the article body, same as a
    # real Wikipedia page's chrome -- exactly what `within: "main"` exists
    # to keep out of the option list (section 2's worked example: "a bias
    # stated rather than hidden"). If the landmark filter is ever dropped,
    # these links sort before "Felis"/"Mammal" and a `prefer_index=0`
    # chooser picks "Main page" instead, changing which ref gets clicked
    # and what the decision row records -- not just lengthening the list.
    '- navigation:\n'
    '  - link "Main page" [ref=e10]\n'
    '  - link "Contents" [ref=e11]\n'
    '  - link "Random article" [ref=e12]\n'
    '- main:\n'
    '  - heading "Cat" [level=1]\n'
    '  - link "Felis" [ref=e1]\n'
    '    - /url: https://en.wikipedia.org/wiki/Felis\n'
    '  - link "Mammal" [ref=e2]\n'
    '    - /url: https://en.wikipedia.org/wiki/Mammal\n'
)

FELIS_PAGE = (
    "url: https://en.wikipedia.org/wiki/Felis\n"
    "title: Felis - Wikipedia\n"
    "\n"
    '- navigation:\n'
    '  - link "Main page" [ref=e10]\n'
    '  - link "Contents" [ref=e11]\n'
    '- main:\n'
    '  - heading "Felis" [level=1]\n'
    '  - link "Cat" [ref=e1]\n'
    '    - /url: https://en.wikipedia.org/wiki/Cat\n'
)


class WikiHopEndToEndTests(_IsolatedDecisionsDir):
    """The refs source, exclude/reenter/visits, an equals-guard reaching a
    top-level final, and output rendering that must keep `context.path`'s
    real list type -- all against the actual extensions/jev/graphs/
    wiki-hop.json, not a synthetic stand-in."""

    def test_two_hop_walk_reaches_the_goal(self):
        driver = FakeDriver({
            "browser_open": {"text": "opened"},
            "browser_snapshot": [{"text": CAT_PAGE}, {"text": FELIS_PAGE}],
            "browser_click": {"text": "clicked"},
        })
        started = run_mod.start(
            "wiki-hop", {"start": "Cat", "goal": "Felis"}, graphs_dir=GRAPHS_DIR,
            chooser=constant_chooser(prefer_index=0), ckpt_id="test-ckpt",
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["kind"], "final")
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "arrived")
        self.assertEqual(report["output"], {"path": ["Cat", "Felis"]})
        # one real choose on Cat; the second "reading" visit (Felis) resolves
        # via the equals `always` guard and never reaches choose at all --
        # steps counts every choose-phase entry, forced/rule picks included
        # (the design's own definition: "steps: chooser calls, forced
        # picks, rule picks and escalations"), so this is not evidence that
        # forced or rule picks are free.
        self.assertEqual(report["steps"], 1)
        self.assertEqual(report["actions"], 4)
        # docs/design/judgement.md section 2.2, section 3's cost table:
        # wiki-hop's own `choose.prefer` rule (`option.label ==
        # {{context.goal}}`, folded) matches "Felis" directly on the Cat
        # page's own menu -- caught at zero cost, no chooser call at all,
        # not merely a confident one.
        self.assertEqual(report["chooser_calls"], 0)
        self.assertEqual(report["rule_picks"], 1)
        self.assertEqual(report["duplicates_collapsed"], 0)
        self.assertEqual(report["escalations"], 0)
        self.assertEqual(report["path"], ["open", "reading", "reading", "arrived"])
        self.assertEqual(
            [c["tool"] for c in driver.calls],
            ["browser_open", "browser_snapshot", "browser_click", "browser_snapshot"],
        )

        rows = _decision_rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["options"], ["Felis", "Mammal"])
        self.assertEqual(rows[0]["label"], 0)
        self.assertEqual(rows[0]["chosen"], "Felis")
        # the goal was IN the menu -- caught by the rule, not the chooser
        # (constant_chooser(prefer_index=0) would have picked "Felis" too,
        # by construction; this assertion is what proves it was never
        # asked, not merely that the two would have agreed).
        self.assertEqual(rows[0]["source"], "rule")
        self.assertEqual(rows[0]["verified"], "rule")
        self.assertEqual(rows[0]["probs"], [1.0, 0.0])
        self.assertEqual(rows[0]["action"], {"tool": "browser_click", "input": {"ref": "e1"}})
        self.assertEqual(rows[0]["ckpt"], "test-ckpt")

    def test_an_unhandled_browser_error_reaches_the_root_error_handler(self):
        def failing_open(request):
            return {"error": "net::ERR_CONNECTION_REFUSED"}

        driver = FakeDriver({"browser_open": failing_open})
        started = run_mod.start(
            "wiki-hop", {"start": "Cat", "goal": "Felis"}, graphs_dir=GRAPHS_DIR,
            chooser=refusing_chooser, ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "failed")
        # "reason" is new: failed.output now carries it beside "error" (this
        # step's edit to wiki-hop.json) so the root ERROR handler's report
        # names which of the three beliefs an invalid observation broke --
        # here, "failed" (a browser_open network error is the tool's own
        # is_error, not a wholeness or of-kind failure).
        self.assertEqual(
            report["output"], {"error": "net::ERR_CONNECTION_REFUSED", "reason": "failed", "path": []}
        )

    def test_the_snapshot_is_requested_within_main(self):
        # docs/design/observation.md section 6 (M1): a `within: "main"`
        # -scoped snapshot of the live Cat article is 762,764 body
        # characters -- roughly 94% of an unscoped one (814,323), not "a
        # fraction" of it, and no `depth` value trims it under `MAX_BODY`
        # (262,144) without also cutting the real, `main`-landmarked
        # option list below the graph's own `max: 64` (see this step's
        # Build-Log entry for the full sweep) -- so the graph asks for
        # `within` alone, every time, and nothing narrower.
        driver = FakeDriver({
            "browser_open": {"text": "opened"},
            "browser_snapshot": [{"text": CAT_PAGE}, {"text": FELIS_PAGE}],
            "browser_click": {"text": "clicked"},
        })
        started = run_mod.start(
            "wiki-hop", {"start": "Cat", "goal": "Felis"}, graphs_dir=GRAPHS_DIR,
            chooser=constant_chooser(prefer_index=0), ckpt_id=None,
        )
        run_to_completion(started["run"], started["request"], driver)
        snapshot_calls = [c for c in driver.calls if c["tool"] == "browser_snapshot"]
        self.assertTrue(snapshot_calls)
        for call in snapshot_calls:
            self.assertEqual(call["input"], {"within": "main", "roles": ["link"]})


class WikiHopRealCaptureTests(_IsolatedDecisionsDir):
    """docs/Build-Log.md, 2026-09-19, "The flagship worked example could
    never have worked": CAT_PAGE above is 10 real-shaped lines standing in
    for an article; the actual Cat article is ~11,000 (jev/tests/fixtures/
    wiki/cat_full_capture.txt) and, before this task's three defects were
    fixed, 58 of its lines -- including the real "Felis" infobox link's
    `/url:` child, on some captures -- silently failed to parse. This runs
    the real, unmodified wiki-hop.json against a real trimmed capture
    (cat_snapshot.txt, jev/tests/fixtures/wiki/README case) instead of the
    hand-written CAT_PAGE, so the menu the chooser scores is the genuine
    63-option main-landmark link list a real Wikipedia page produces --
    property lines, cursor=pointer attributes and all -- not an 2-option
    stand-in of it. The second hop reuses the small synthetic FELIS_PAGE:
    the real fixture's job is to prove the *first* hop's parse and option
    build survive contact with a real page; a second real capture would
    not exercise anything this test does not already cover."""

    def test_the_real_graph_picks_the_real_felis_link_from_the_real_page(self):
        # docs/design/judgement.md section 2.2: the goal ("Felis") is
        # itself one of the real page's own link labels, so this run is
        # now decided by `choose.prefer`'s rule, not the chooser --
        # `refusing_chooser` (rather than the old `prefer_label("Felis")`
        # fake) makes that the test itself: an AssertionError from inside
        # the run is what a chooser call would look like here, and none
        # happens.
        cat_real = (FIXTURES_DIR / "cat_snapshot.txt").read_text(encoding="utf-8")
        driver = FakeDriver({
            "browser_open": {"text": "opened"},
            "browser_snapshot": [{"text": cat_real}, {"text": FELIS_PAGE}],
            "browser_click": {"text": "clicked"},
        })
        started = run_mod.start(
            "wiki-hop", {"start": "Cat", "goal": "Felis"}, graphs_dir=GRAPHS_DIR,
            chooser=refusing_chooser, ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "arrived")
        self.assertEqual(report["output"], {"path": ["Cat", "Felis"]})
        self.assertEqual(
            [c["tool"] for c in driver.calls],
            ["browser_open", "browser_snapshot", "browser_click", "browser_snapshot"],
        )
        # the ref actually clicked is the real page's own -- not a small
        # fixture's e1, proof the real tree (not a summary of it) drove
        # the choice.
        self.assertEqual(driver.calls[2]["input"], {"ref": "e374"})
        self.assertEqual(report["chooser_calls"], 0)
        self.assertEqual(report["rule_picks"], 1)

        rows = _decision_rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        # 63 real main-landmark links live in the trimmed capture; this
        # step's "url" bound (docs/design/unattended.md section 3, "the
        # menu bound") drops 6 of them before the menu is built at all --
        # 4 same-page citation anchors ("[1]" x2, "[2]", "[3]", each an
        # `#cite_note-...` fragment, no article to hop to) and 2 cross-wiki
        # links ("hissing"/"grunting" -> en.wiktionary.org, an origin the
        # graph's warrant never declares) -- leaving 57 raw options; this
        # step's dedup then collapses 3 more (two extra copies of
        # "Felidae", one extra copy of "Carnivora" -- the real page's own
        # repeats, judgement.md section 1's own subject) to their first
        # occurrence, leaving the 54 distinct options the chooser (or, as
        # here, the rule) actually sees.
        self.assertEqual(len(rows[0]["options"]), 54)
        self.assertIn("Felis", rows[0]["options"])
        self.assertEqual(rows[0]["chosen"], "Felis")
        self.assertEqual(rows[0]["source"], "rule")
        self.assertEqual(rows[0]["verified"], "rule")
        self.assertEqual(rows[0]["action"], {"tool": "browser_click", "input": {"ref": "e374"}})

    def test_the_real_cat_page_with_goal_felidae_is_taken_by_rule(self):
        # docs/design/judgement.md section 1's own worked failure and
        # section 2.2's own fix, both against the identical real fixture:
        # the trimmed capture's menu has "Felidae" three times (the lead
        # sentence and two infobox links) -- section 1's own measured
        # numbers, on a real trained checkpoint, before either fix existed
        # in this build: top1 == top2 == 0.2455 (a three-way split of one
        # decision's mass against copies of itself), margin 0.0000, gate
        # PARK, even though the checkpoint "knew" the answer. Dedup
        # collapses the three copies to the first (`ref=e256`, the lead
        # sentence's own link -- see the probe this step ran directly
        # against `options.build_options`/`options.first_preferred`), and
        # with the goal literally the survivor's label, `choose.prefer`'s
        # rule takes it before the chooser is ever asked: zero chooser
        # calls, not merely a correct one -- `refusing_chooser` is the
        # proof, the same technique the sibling Felis test above now uses.
        cat_real = (FIXTURES_DIR / "cat_snapshot.txt").read_text(encoding="utf-8")
        driver = FakeDriver({
            "browser_open": {"text": "opened"},
            "browser_snapshot": [{"text": cat_real}],
            "browser_click": {"text": "clicked"},
        })
        started = run_mod.start(
            "wiki-hop", {"start": "Cat", "goal": "Felidae"}, graphs_dir=GRAPHS_DIR,
            chooser=refusing_chooser, ckpt_id=None,
        )
        run_id = started["run"]
        # Not driven to completion -- there is no landing-page fixture for
        # "Felidae" (FELIS_PAGE stands in only for the "Felis" hop other
        # tests in this class take), and nothing past the first PICK is
        # this test's subject. Left parked mid-`act` rather than run to a
        # final state, so it must be dropped at teardown the same way
        # test_a_parked_runs_escalation_is_readable_from_status_with_its_
        # content_intact already documents doing (process-lifetime `_RUNS`
        # entry, outside this test's own JEV_RUNS_DIR isolation window).
        self.addCleanup(lambda: run_mod._RUNS.pop(run_id, None))

        request = started["request"]
        self.assertEqual(request["tool"], "browser_open")
        request = run_mod.step(run_id, request["id"], driver.answer(request))["request"]
        self.assertEqual(request["tool"], "browser_snapshot")
        # the PICK already happened inside this very step -- what comes
        # back is the browser_click for whichever ref the rule chose,
        # proof the choice was made with no floor/margin/park in between.
        request = run_mod.step(run_id, request["id"], driver.answer(request))["request"]
        self.assertEqual(request["tool"], "browser_click")
        self.assertEqual(request["input"], {"ref": "e256"})

        record = run_mod.status(run_id)
        self.assertEqual(record["counts"]["chooser_calls"], 0)
        self.assertEqual(record["counts"]["rule_picks"], 1)
        self.assertEqual(record["counts"]["duplicates_collapsed"], 3)

        rows = _decision_rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        self.assertEqual(len(rows[0]["options"]), 54)
        self.assertEqual(rows[0]["options"].count("Felidae"), 1)  # deduped, not three copies
        self.assertEqual(rows[0]["chosen"], "Felidae")
        self.assertEqual(rows[0]["source"], "rule")
        self.assertEqual(rows[0]["verified"], "rule")
        self.assertEqual(rows[0]["action"], {"tool": "browser_click", "input": {"ref": "e256"}})

    def test_the_main_scoped_capture_is_accepted_whole_with_a_real_main_only_option_list(self):
        # cat_main_scoped.txt: a real `within: "main"` capture of the live
        # Cat article (jev/tests/fixtures/wiki/cat_main_scoped.txt),
        # carrying the `scope:`/`chars:` head lines a scoped
        # `extensions/browser/service.py` `_snapshot` now stamps. Whole by
        # its own declaration (`is_truncated` False), `h1` is "Cat", and
        # the real, unmodified wiki-hop.json's own "reading" choose config
        # (`roles: ["link"], within: "main"`, capped at its own `max: 64`)
        # builds a non-empty option list every one of whose refs is
        # landmarked "main" in the underlying tree -- cross-checked
        # against `a11y.parse_refs` directly, since `Option.data` itself
        # does not carry `landmark`.
        raw = (FIXTURES_DIR / "cat_main_scoped.txt").read_text(encoding="utf-8")
        obs = a11y.build_obs(raw)
        self.assertFalse(a11y.is_truncated(raw))
        self.assertEqual(obs["h1"], "Cat")
        self.assertEqual(obs["scope"], "main")

        graph = graph_mod.load_graph(GRAPHS_DIR / "wiki-hop.json", option_tokens=32, context_tokens=192)
        state = graph.state_at(("reading",))
        choose_cfg = state["meta"]["choose"]
        scope = {
            "context": {"obs": obs, "goal": "Felis", "visited": []},
            "input": {}, "event": {}, "run": {"id": "x", "step": 0},
        }
        opts, warnings, duplicates_collapsed = options.build_options(state, choose_cfg, scope, graph)
        self.assertEqual(warnings, [])
        self.assertTrue(opts)
        self.assertEqual(len(opts), 64)  # capped by the graph's own "max": 64 -- still exactly
        # saturated after dedup: this real capture has enough distinct
        # links that removing its own duplicates (3, the same repeats
        # cat_snapshot.txt carries) does not even bring the raw count
        # under the cap -- dedup runs before max, but max still binds.
        self.assertEqual(duplicates_collapsed, 3)
        self.assertEqual(len(opts), len(set(o.label for o in opts)))  # the 64 survivors are all distinct

        records, _skipped = a11y.parse_refs(obs["text"])
        landmark_by_ref = {r["ref"]: r["landmark"] for r in records if r.get("ref")}
        for opt in opts:
            self.assertEqual(landmark_by_ref.get(opt.data["ref"]), "main")

    def test_the_scoped_capture_yields_only_article_links_under_the_url_bound(self):
        # W2 (this step's own Build-Log entry): the real, unmodified
        # trimmed capture (cat_snapshot.txt) carries 63 real main-landmark
        # links with a name and a ref. This step's own "url" bound
        # (docs/design/unattended.md section 3, "the menu bound") drops 6
        # of them: 4 same-page citation anchors (`#cite_note-...`, no
        # article to hop to) and 2 links to a different wiki entirely
        # (en.wiktionary.org, an origin wiki-hop.json's own warrant never
        # declares) -- leaving 57 raw options either way. A later step
        # (docs/design/judgement.md section 2.1) adds dedup on top of that
        # same bound: the page repeats "Felidae" (x3, lead + two infobox
        # links) and "Carnivora" (x2), so of the 63 raw links, 4 collapse
        # to their first occurrence (59 distinct) before the url bound,
        # and of the 57 the url bound leaves, 3 collapse (54 distinct) --
        # one of the 4 duplicate-label repeats was itself one of the 6
        # the url bound alone already dropped (63-6=57, 59-5=54: the
        # overlap is real, not an arithmetic mismatch). Both bounds proven
        # here directly against `options.py` rather than only observed
        # indirectly through which option a chooser happened to pick.
        raw = (FIXTURES_DIR / "cat_snapshot.txt").read_text(encoding="utf-8")
        obs = a11y.build_obs(raw)
        graph = graph_mod.load_graph(GRAPHS_DIR / "wiki-hop.json")
        state = graph.state_at(("reading",))
        choose_cfg = state["meta"]["choose"]
        scope = {
            "context": {"obs": obs, "goal": "Felis", "visited": []},
            "input": {}, "event": {}, "run": {"id": "x", "step": 0},
        }

        # before: this same source and config, with the "url" key this
        # step added taken back out -- the option list wiki-hop.json would
        # have built before that step, still through this step's dedup.
        unbound_cfg = dict(choose_cfg, **{"from": {"refs": {"roles": ["link"], "within": "main"}}})
        before, before_warnings, before_dup = options.build_options(state, unbound_cfg, scope, graph)
        self.assertEqual(before_warnings, [])
        self.assertEqual(len(before), 59)
        self.assertEqual(before_dup, 4)

        after, warnings, after_dup = options.build_options(state, choose_cfg, scope, graph)
        self.assertEqual(warnings, [])
        self.assertEqual(len(after), 54)
        self.assertEqual(after_dup, 3)

        url_re = re.compile(r"^(?:https://en\.wikipedia\.org)?/wiki/[^:#?]+$")
        for opt in after:
            self.assertRegex(opt.data["url"], url_re)


class WarrantedRunTests(_IsolatedDecisionsDir):
    """docs/design/unattended.md section 5: `run.py`'s own warrant
    plumbing -- `start`'s pre-dispatch check (`warrant_mod.check`, before
    `Run` is ever constructed, so a mismatch dispatches nothing, not even
    a graph's own entry actions), the block/id threaded through every
    escalation, the final report, `.status`, and every decision + run-end
    row, and `Run.answer`'s own resume-time check. Loads the real, shipped
    wiki-hop.json's own canonical block via `warrant_mod.canonical` --
    never a hand-copied literal -- so a future edit to that graph cannot
    silently desync this class from what `test_warrant.py`'s own pins
    already prove is shipped. `PersistenceTests.
    test_a_park_snapshot_carries_the_warrant_and_a_restart_keeps_it` is
    this same coverage's seventh case, kept in that class instead because
    it needs `_simulate_restart`."""

    def _wiki_hop_block(self) -> dict:
        graph = graph_mod.load_graph(GRAPHS_DIR / "wiki-hop.json")
        return warrant_mod.canonical(graph)

    @staticmethod
    def _warranted_park_graph() -> dict:
        # Declares a warrant naming exactly the one tool it ever dispatches
        # (`_lint_warrant`'s tools-set-equality) on the PICK transition a
        # park never reaches unless answered -- so every test in this class
        # that only starts, escalates, or resumes-and-immediately-stops
        # never needs a real driver call at all, the park itself is enough.
        return _minimal_graph(
            meta={
                "jev": {
                    "schema": 1,
                    "defaults": {"floor": 0.9, "margin": 0.1},
                    "warrant": {"tools": ["bash"]},
                }
            },
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": {"target": "b", "actions": [
                        {"type": "tool", "params": {"name": "bash", "input": {"command": "true"}}}
                    ]}},
                },
                "b": {"type": "final"},
            },
        )

    def test_a_run_started_with_the_graphs_own_block_is_warranted_end_to_end(self):
        block = self._wiki_hop_block()
        driver = FakeDriver({
            "browser_open": {"text": "opened"},
            "browser_snapshot": [{"text": CAT_PAGE}, {"text": FELIS_PAGE}],
            "browser_click": {"text": "clicked"},
        })
        started = run_mod.start(
            "wiki-hop", {"start": "Cat", "goal": "Felis"}, graphs_dir=GRAPHS_DIR,
            chooser=constant_chooser(prefer_index=0), ckpt_id="test-ckpt", warrant=block,
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["warrant"]["id"], warrant_mod.warrant_id(block))
        self.assertEqual(report["warrant"]["actions"], block["actions"])
        self.assertEqual(report["warrant"]["origins"], block["origins"])

        rows = _decision_rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["warrant"], warrant_mod.warrant_id(block))

        # the run-end row, appended after the one decision row -- it too
        # carries the warrant id (run.py's `_log_run_end`).
        raw_rows = _read_jsonl(decisions.graph_log_path("wiki-hop"))
        self.assertEqual(len(raw_rows), 2)
        self.assertEqual(raw_rows[1]["warrant"], warrant_mod.warrant_id(block))

        # the open itself carried the confine this step's wiki-hop.json
        # edit added -- proof the warrant's origins reached the actual
        # dispatched call, not just the bookkeeping around it.
        self.assertEqual(driver.calls[0]["tool"], "browser_open")
        self.assertEqual(driver.calls[0]["input"]["confine"], ["https://en.wikipedia.org"])

    def test_a_run_started_without_a_block_is_attended(self):
        driver = FakeDriver({
            "browser_open": {"text": "opened"},
            "browser_snapshot": [{"text": CAT_PAGE}, {"text": FELIS_PAGE}],
            "browser_click": {"text": "clicked"},
        })
        started = run_mod.start(
            "wiki-hop", {"start": "Cat", "goal": "Felis"}, graphs_dir=GRAPHS_DIR,
            chooser=constant_chooser(prefer_index=0), ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "reached")
        self.assertIsNone(report["warrant"])
        rows = _decision_rows("wiki-hop")
        self.assertEqual(len(rows), 1)
        self.assertIsNone(rows[0]["warrant"])

    def test_a_mismatched_block_is_refused_before_the_first_action(self):
        correct = self._wiki_hop_block()
        bad = dict(correct, actions=correct["actions"] + 1)  # the one field that differs
        driver = FakeDriver({})  # scripted for nothing -- any call here is itself a failure
        with self.assertRaises(ValueError) as ctx:
            run_mod.start(
                "wiki-hop", {"start": "Cat", "goal": "Felis"}, graphs_dir=GRAPHS_DIR,
                chooser=constant_chooser(prefer_index=0), ckpt_id=None, warrant=bad,
            )
        message = str(ctx.exception)
        self.assertIn("wiki-hop@1", message)
        self.assertIn(warrant_mod.canonical_json(correct), message)
        self.assertEqual(driver.calls, [])  # not even the graph's own entry actions ran

    def test_an_escalation_under_a_warrant_carries_the_block_and_the_hint_names_it(self):
        graph_dict = self._warranted_park_graph()
        graph = graph_mod.load_graph(graph_dict)
        block = warrant_mod.canonical(graph)
        started = run_mod.start(
            graph_dict, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4),
            ckpt_id=None, warrant=block,
        )
        esc = started["request"]
        self.assertEqual(esc["kind"], "escalate")
        self.assertEqual(esc["warrant"], {"id": warrant_mod.warrant_id(block), **block})
        self.assertIn("jev_resume", esc["answer"])
        self.assertIn("warrant", esc["answer"])

    def test_a_resume_with_the_wrong_block_is_refused_without_disturbing_the_park(self):
        graph_dict = self._warranted_park_graph()
        graph = graph_mod.load_graph(graph_dict)
        block = warrant_mod.canonical(graph)
        started = run_mod.start(
            graph_dict, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4),
            ckpt_id=None, warrant=block,
        )
        run_id = started["run"]
        wrong = dict(block, actions=block["actions"] + 1)
        with self.assertRaises(ValueError) as ctx:
            run_mod.answer(run_id, pick=0, warrant=wrong)
        self.assertIn("warrant mismatch", str(ctx.exception))
        # the park survived the rejected answer -- the real block still
        # works, and drives the PICK transition's own `bash` action
        # (this fixture's one dispatch) all the way to a genuine final.
        request = run_mod.answer(run_id, pick=0, warrant=block)["request"]
        driver = FakeDriver({"bash": {"text": "ok"}})
        final = run_to_completion(run_id, request, driver)
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "reached")

    def test_a_resume_of_an_attended_run_with_a_block_is_refused(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        self.assertEqual(started["request"]["kind"], "escalate")
        fake_block = {
            "graph": "synthetic", "sha256": "0" * 64, "tools": [], "origins": [], "commands": [],
            "actions": 100, "wall_s": 600,
        }
        with self.assertRaises(ValueError) as ctx:
            run_mod.answer(started["run"], pick=0, warrant=fake_block)
        self.assertIn("attended", str(ctx.exception))
        # the park survived -- an ordinary, unwarranted answer still works
        final = run_mod.answer(started["run"], pick=0)["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "reached")


class TriageLinuxEndToEndTests(_IsolatedDecisionsDir):
    """The hierarchical machinery: compound states, the count-guard
    fallback standing in for the stubbed `entails` NLI guard, a nested
    `final` firing its parent's `onDone`, and the `lines` source plus
    `also`. Shallow history is exercised for real (not just parsed): a
    `bash` failure mid investigation raises the root's `ERROR`, lands in
    `recover`, and `CONTINUE` must resume at whichever child of
    `investigate` was actually active when it failed -- never at `hist`'s
    own static fallback `target: "identify"` -- so the test below arranges
    for those two to be different states, the only way the assertion can
    tell a real history resume from a coincidental one."""

    @staticmethod
    def _triage_chooser(context: str, labels: list[str]) -> list[float]:
        if "nothing looks wrong" in labels:
            scores = [0.05] * len(labels)
            for i, lbl in enumerate(labels):
                if lbl != "nothing looks wrong":
                    scores[i] = 0.9
                    break
            return scores
        scores = [0.05] * len(labels)
        scores[0] = 0.9
        return scores

    @staticmethod
    def _bash(request):
        cmd = request["input"]["command"]
        if cmd.startswith("ps "):
            return {"text": "COMMAND PID USER %CPU ELAPSED\nsshd 100 root 0.0 01:00:00\ncron 101 root 0.0 01:00:00\n"}
        return {"text": f"(output of: {cmd})"}

    def test_full_investigation_reaches_report_with_a_flagged_process(self):
        driver = FakeDriver({"bash": self._bash})
        started = run_mod.start(
            "triage-linux", {}, graphs_dir=GRAPHS_DIR, chooser=self._triage_chooser, ckpt_id="test-ckpt",
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["final_state"], "report")
        self.assertEqual(report["output"]["flagged"], ["sshd 100 root 0.0 01:00:00"])
        self.assertEqual(len(report["output"]["notes"]), 7)  # 3 identify + 3 network + 1 processes
        self.assertEqual(report["steps"], 7)  # 3 + 3 + 1 real chooses; processes' always needs none
        self.assertEqual(report["chooser_calls"], 7)
        self.assertEqual(
            report["path"], ["investigate", "identify", "network", "processes", "judge", "done", "report"]
        )
        commands = [c["input"]["command"] for c in driver.calls]
        self.assertEqual(commands[0], "cat /etc/os-release")
        self.assertEqual(commands[1], "uname -a")
        self.assertEqual(commands[2], "hostname; uptime")
        self.assertEqual(commands[3], "ss -tulpn 2>/dev/null || netstat -tulpn")
        self.assertTrue(commands[6].startswith("ps "))

        rows = _decision_rows("triage-linux")
        self.assertEqual(len(rows), 7)
        self.assertTrue(all(row["graph"] == "triage-linux@1" for row in rows))

    def test_a_mid_investigation_error_recovers_through_the_remembered_child_not_the_static_fallback(self):
        """docs/design/automation.md section 1, "History": "`history`
        resolves to its parent's last active child... or to its `target`
        if the parent was never entered." Those two can agree by accident
        (the fallback happens to be `identify`, the phase most failures
        would still be in), so this test fails the *first* three `identify`
        commands, moves the run into `network`, and only then fails a
        `network` command -- forcing the remembered child to be `network`,
        never `identify`, so a resume at the hardcoded fallback and a
        resume at the real history are distinguishable."""
        network_commands = {
            "ss -tulpn 2>/dev/null || netstat -tulpn",
            "ss -tnp state established",
            "ip -br addr",
            "nft list ruleset 2>/dev/null || iptables -S",
        }
        failed_once = [False]

        def bash(request):
            cmd = request["input"]["command"]
            if cmd in network_commands and not failed_once[0]:
                failed_once[0] = True
                return {"error": "connection refused"}
            return self._bash(request)

        driver = FakeDriver({"bash": bash})
        started = run_mod.start(
            "triage-linux", {}, graphs_dir=GRAPHS_DIR, chooser=self._triage_chooser, ckpt_id="test-ckpt",
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["final_state"], "report")
        self.assertEqual(report["escalations"], 0)
        # the point of the test: "network" follows "recover" -- the
        # remembered child -- never a second "identify", the static
        # fallback `hist` would use if history were not actually recorded.
        self.assertEqual(
            report["path"],
            ["investigate", "identify", "network", "recover", "investigate", "network",
             "processes", "judge", "done", "report"],
        )
        self.assertEqual(report["path"].count("identify"), 1)
        self.assertIn("error: connection refused", report["output"]["notes"])

        rows = _decision_rows("triage-linux")
        recover_rows = [r for r in rows if r["state"] == "recover"]
        self.assertEqual(len(recover_rows), 1)
        self.assertEqual(recover_rows[0]["chosen"], "continue where it left off")

    def test_a_failed_listing_command_reaches_recover(self):
        """The J1 case (docs/Decisions.md, "The fourth time..."): a `ps`
        invocation that fails outright -- `{"error": "[exit code 1]\\nps:
        unknown option -- o"}`, the shape a failing command takes now that
        `crates/rune`'s `bash.rn` stops treating a nonzero exit as ordinary
        text (step 5, a different tree; landed separately -- confirmed by
        reading `bash.rn`'s `shell_exec` call and `host.rs`'s `dispatch`,
        which maps `is_error` to `Err`, so this is no longer a projection
        of a pending change). This test still drives that shape through a
        `FakeDriver` rather than a real process, on purpose: it is a proof
        of the jev-side wiring (history-resume, `notes`, `reason: "failed"`),
        independent of and faster than whatever the Rust layer does on any
        given day. It must reach `recover` exactly like any other tool
        error, with `notes` ending in the error text. The first test in
        this suite to exercise `recover` from a command's own failure
        rather than a scripted dispatch failure with no such shape
        (closing D7's finding). `failed_once` mirrors the sibling test
        above: the retried `ps`, after `CONTINUE` resumes at the
        remembered `processes` child (never `identify`, `hist`'s own
        static fallback), succeeds and satisfies the new `expect` guard,
        so the investigation completes normally."""
        failed_once = [False]

        def bash(request):
            cmd = request["input"]["command"]
            if cmd.startswith("ps ") and not failed_once[0]:
                failed_once[0] = True
                return {"error": "[exit code 1]\nps: unknown option -- o"}
            return self._bash(request)

        driver = FakeDriver({"bash": bash})
        started = run_mod.start(
            "triage-linux", {}, graphs_dir=GRAPHS_DIR, chooser=self._triage_chooser, ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["final_state"], "report")
        self.assertIn("recover", report["path"])
        # history resumes exactly at "processes" -- the state active when
        # the `ps` command failed -- not "identify", the same proof shape
        # as the sibling test above (it appears twice: once entered and
        # failed, once entered again on resume and succeeded).
        self.assertEqual(report["path"].count("processes"), 2)
        self.assertEqual(report["path"].count("identify"), 1)
        notes = report["output"]["notes"]
        error_notes = [n for n in notes if n.startswith("error:")]
        self.assertEqual(len(error_notes), 1)
        self.assertTrue(error_notes[0].startswith("error: [exit code 1]"))

        rows = _decision_rows("triage-linux")
        recover_rows = [r for r in rows if r["state"] == "recover"]
        self.assertEqual(len(recover_rows), 1)
        self.assertEqual(recover_rows[0]["chosen"], "continue where it left off")

    def test_a_triage_run_under_its_warrant_dispatches_only_listed_commands(self):
        # The real, shipped triage-linux.json's own canonical block --
        # loaded fresh, never a hand-copied literal (test_warrant.py
        # already pins the literal; this test's own job is only to prove
        # the *run* stays inside it end to end, not to re-prove the block
        # itself). Every `bash` request this full investigation dispatches
        # must be one of the block's nine literal commands -- the graph
        # never asks anything outside what the warrant lists.
        graph = graph_mod.load_graph(GRAPHS_DIR / "triage-linux.json")
        block = warrant_mod.canonical(graph)
        driver = FakeDriver({"bash": self._bash})
        started = run_mod.start(
            "triage-linux", {}, graphs_dir=GRAPHS_DIR, chooser=self._triage_chooser,
            ckpt_id="test-ckpt", warrant=block,
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["warrant"]["id"], warrant_mod.warrant_id(block))
        commands = [c["input"]["command"] for c in driver.calls]
        self.assertTrue(commands)  # the investigation genuinely dispatched something
        for cmd in commands:
            self.assertIn(cmd, block["commands"])

        rows = _decision_rows("triage-linux")
        self.assertTrue(rows)
        self.assertTrue(all(row["warrant"] == warrant_mod.warrant_id(block) for row in rows))


# --- input validation ---------------------------------------------------------------


class InputValidationTests(_IsolatedDecisionsDir):
    """`run.py`'s `_check_input` -- `meta.jev.input`'s `required` list,
    checked against `automation.start`'s `input` before a `Run` is even
    created. Every other call in this suite supplies satisfying input, so
    this is the one path that exercises rejecting an unsatisfying one."""

    def test_a_missing_required_input_field_is_rejected_before_the_run_starts(self):
        # wiki-hop's own schema (extensions/jev/graphs/wiki-hop.json):
        # required: ["start", "goal"]. Supplying only "start" leaves
        # "goal" missing.
        with self.assertRaises(ValueError) as ctx:
            run_mod.start(
                "wiki-hop", {"start": "Cat"}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None,
            )
        # exact message: names only the field actually missing ("goal"),
        # not "start", which the input did supply
        self.assertEqual(str(ctx.exception), "run input is missing required field(s): ['goal']")
        # nothing was created for input that was never accepted
        self.assertEqual(_read_jsonl(decisions.graph_log_path("wiki-hop")), [])

    def test_every_required_field_missing_is_named(self):
        with self.assertRaises(ValueError) as ctx:
            run_mod.start("wiki-hop", {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        message = str(ctx.exception)
        self.assertIn("start", message)
        self.assertIn("goal", message)


# --- budgets -----------------------------------------------------------------------


class BudgetTests(_IsolatedDecisionsDir):
    def test_a_steps_budget_stops_the_run_not_just_the_state(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "budget": {"steps": 2}, "defaults": {"visits": 100}}},
            states={
                "loop": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": {"target": "loop", "reenter": True}},
                },
                "done": {"type": "final"},
            },
            initial="loop",
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0), ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(report["outcome"], "exhausted")
        self.assertEqual(report["exhausted"], "steps")
        self.assertEqual(report["steps"], 2)
        self.assertEqual(report["chooser_calls"], 2)

    def test_a_visits_budget_on_one_state_stops_the_run(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "budget": {"steps": 100}}},
            states={
                "loop": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}, "visits": 2},
                    "on": {"PICK": {"target": "loop", "reenter": True}},
                },
                "done": {"type": "final"},
            },
            initial="loop",
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0), ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(report["outcome"], "exhausted")
        self.assertEqual(report["exhausted"], "visits")


# --- confidence floor: parks, and automation.answer resumes it ----------------------


class ConfidenceFloorTests(_IsolatedDecisionsDir):
    """Section 4: a choice under the floor or margin suspends the run
    (`_park`) rather than ending it. `run_to_completion` returns whatever
    non-`act` request it first meets, so against a park it hands back the
    `escalate` payload itself -- there is no wrapping "final report" to
    unpack until the run actually ends, which now happens only once an
    answer decides it should."""

    def _low_confidence_graph(self, **extra_meta):
        # An entry `tool` action and a `goal` in context so the escalation
        # payload's "evidence" (docs/design/automation.md section 4) has
        # real, checkable content -- goal/h1/url/excerpt sourced from the
        # run's actual context and observation, not just structurally
        # present.
        raw_snapshot = (
            "url: https://example.test/start\n"
            "title: Start - Example\n"
            "\n"
            '- heading "Start" [level=1]\n'
            '- paragraph: some body text.\n'
        )
        jev_meta = {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}
        jev_meta.update(extra_meta)
        graph = _minimal_graph(
            meta={"jev": jev_meta},
            context={"goal": "the target"},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "browser_snapshot", "into": "obs"}}],
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            },
        )
        return graph, raw_snapshot

    def test_a_low_top_score_parks_instead_of_picking(self):
        graph, raw_snapshot = self._low_confidence_graph()
        driver = FakeDriver({"browser_snapshot": {"text": raw_snapshot}})
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        esc = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(esc["kind"], "escalate")
        # the exact key set the design specifies (section 4's payload) --
        # so a future omission cannot hide behind a partial spot-check the
        # way "evidence" itself just did.
        self.assertEqual(
            set(esc.keys()),
            {"kind", "run", "graph", "state", "step", "why", "question",
             "evidence", "options", "budget", "warrant", "answer", "request"},
        )
        self.assertEqual(esc["run"], started["run"])
        self.assertEqual(esc["state"], "a")
        self.assertIn("floor", esc["why"])
        self.assertIsNone(esc["question"])  # no meta.ask on this state
        self.assertEqual([o["label"] for o in esc["options"]], ["x", "y"])
        # the design's own literal hint text (section 4's worked example)
        self.assertEqual(
            esc["answer"], f"jev_resume {{run, request: {esc['request']}, pick: <index or label>}} or {{run, stop: <reason>}}"
        )
        expected_obs = a11y.build_obs(raw_snapshot)
        self.assertEqual(
            esc["evidence"],
            {
                "goal": "the target",
                "h1": expected_obs["h1"],
                "url": expected_obs["url"],
                "excerpt": expected_obs["text"][:1500],
            },
        )
        # the run is genuinely suspended, not finished -- one real chooser
        # call happened (the miss itself), nothing was written to the log
        # for a choice that was never made, and status confirms it is live
        self.assertEqual(_read_jsonl(decisions.graph_log_path("synthetic")), [])
        record = run_mod.status(started["run"])
        self.assertFalse(record["finished"])
        self.assertIsNone(record["outcome"])
        self.assertEqual(record["counts"]["chooser_calls"], 1)
        self.assertEqual(record["counts"]["escalations"], 1)

    def test_a_parked_runs_escalation_is_readable_from_status_with_its_content_intact(self):
        """S33 (docs/Tasklist.md): `RunRow::escalation` on the Rust side is
        always `None` because `_status_dict` never serialised
        `_RunState.escalation` -- the one thing an operator needs from a
        parked run in the runs tab is the question it is parked on, and
        this is the test that reads it back rather than merely checking
        it is present. Uses its own graph, not `_low_confidence_graph`
        (whose own comment notes "no meta.ask on this state" -- deliberate
        there, useless here): a real `question` string is the whole point
        of "read the question back."""
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {
                    "meta": {
                        "choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}},
                        "ask": "Which one, x or y?",
                    },
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            },
        )
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        # Dropped deliberately at teardown regardless of how this test
        # ends -- docs/Decisions.md, "Process lifetime escapes per-test
        # isolation": a `Run` left dangling in the module-level `_RUNS`
        # past this test's own JEV_RUNS_DIR isolation window is only
        # garbage-collected at some later, unpredictable point (at worst,
        # interpreter shutdown, after the env var has already reverted).
        self.addCleanup(lambda: run_mod._RUNS.pop(run_id, None))
        esc = started["request"]
        self.assertEqual(esc["kind"], "escalate")
        self.assertEqual(esc["question"], "Which one, x or y?")

        # poll -- status() must not consume the pending escalation, and
        # must hand back the question with its content intact, not just
        # a marker that one exists
        record = run_mod.status(run_id)
        self.assertIn("escalation", record, "a parked run's status must carry its escalation")
        self.assertEqual(record["escalation"], esc, "must be the exact payload the park itself yielded, verbatim")
        self.assertEqual(record["escalation"]["question"], "Which one, x or y?")
        self.assertEqual([o["label"] for o in record["escalation"]["options"]], ["x", "y"])
        self.assertIn("floor", record["escalation"]["why"])
        self.assertFalse(record["finished"])
        self.assertIsNone(record["outcome"])

        # polling again does not consume it either -- a status call is a
        # snapshot, not an answer
        record2 = run_mod.status(run_id)
        self.assertEqual(record2["escalation"], esc)

        # once answered, the run is no longer asking anything -- status
        # must stop carrying a question that has already been resolved
        final = run_mod.answer(run_id, pick=0, by="human")["request"]
        self.assertEqual(final["outcome"], "reached")
        record3 = run_mod.status(run_id)
        self.assertNotIn("escalation", record3)

    def test_a_thin_margin_between_top_two_also_parks(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.3, "margin": 0.5}}},
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            },
        )
        # top1=0.55, top2=0.45 -- both clear the floor, but the 0.1 margin
        # between them is well under the 0.5 required.
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=lambda ctx, labels: [0.55, 0.45], ckpt_id=None,
        )
        esc = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(esc["kind"], "escalate")
        self.assertIn("margin", esc["why"])

    def test_answering_with_a_pick_by_index_resumes_the_run_to_completion(self):
        graph, raw_snapshot = self._low_confidence_graph()
        driver = FakeDriver({"browser_snapshot": {"text": raw_snapshot}})
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        esc = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual([o["index"] for o in esc["options"]], [0, 1])
        final = run_mod.answer(started["run"], pick=1, by="human")["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "reached")
        rows = _decision_rows("synthetic")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["chosen"], "y")
        self.assertEqual(rows[0]["label"], 1)
        self.assertEqual(rows[0]["source"], "human")
        self.assertEqual(rows[0]["verified"], "human")
        self.assertEqual(rows[0]["action"], None)  # PICK -> "b" has no tool action to preview

    def test_answering_with_a_pick_by_label_resumes_the_run(self):
        graph, raw_snapshot = self._low_confidence_graph()
        driver = FakeDriver({"browser_snapshot": {"text": raw_snapshot}})
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_to_completion(started["run"], started["request"], driver)
        final = run_mod.answer(started["run"], pick="x")["request"]
        self.assertEqual(final["outcome"], "reached")
        rows = _decision_rows("synthetic")
        self.assertEqual(rows[0]["chosen"], "x")
        self.assertEqual(rows[0]["label"], 0)
        # `by` was never supplied -- run.py's own documented fallback
        self.assertEqual(rows[0]["source"], "model")
        self.assertEqual(rows[0]["verified"], "model")

    def test_an_out_of_menu_pick_is_rejected_without_disturbing_the_park(self):
        graph, raw_snapshot = self._low_confidence_graph()
        driver = FakeDriver({"browser_snapshot": {"text": raw_snapshot}})
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_to_completion(started["run"], started["request"], driver)
        with self.assertRaises(ValueError):
            run_mod.answer(started["run"], pick="not on the menu")
        with self.assertRaises(ValueError):
            run_mod.answer(started["run"], pick=99)
        # still parked, exactly as before both rejected attempts
        record = run_mod.status(started["run"])
        self.assertFalse(record["finished"])
        self.assertEqual(_read_jsonl(decisions.graph_log_path("synthetic")), [])
        # a real answer afterward still works -- the rejections did not
        # corrupt the generator or consume its one pending escalation
        final = run_mod.answer(started["run"], pick=0)["request"]
        self.assertEqual(final["outcome"], "reached")

    def test_answering_with_stop_ends_the_run_as_stopped_not_escalated(self):
        graph, raw_snapshot = self._low_confidence_graph()
        driver = FakeDriver({"browser_snapshot": {"text": raw_snapshot}})
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_to_completion(started["run"], started["request"], driver)
        final = run_mod.answer(started["run"], stop="human declined to choose")["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "stopped")
        self.assertEqual(final["reason"], "human declined to choose")
        # nothing was ever chosen, so the only row this run leaves behind
        # is its own run-end marker (S45: decisions.log_run_end, wired
        # from Run._settle) -- not an empty file any more.
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(
            rows, [{"run": started["run"], "outcome": "stopped", "ts": rows[0]["ts"], "warrant": None}]
        )
        record = run_mod.status(started["run"])
        self.assertTrue(record["finished"])
        self.assertEqual(record["outcome"], "stopped")
        self.assertEqual(record["reason"], "human declined to choose")

    def test_a_second_park_past_the_escalations_budget_exhausts_the_run(self):
        # floor never clears (0.6 < 0.9) and PICK re-enters "a" with no
        # entry action to get in the way (unlike `_low_confidence_graph`),
        # so answering the first park runs straight into a second choose
        # phase, in the same generator resumption -- the second park is the
        # one that finds the escalations budget already spent.
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}, "budget": {"escalations": 1}}},
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": {"target": "a", "reenter": True}},
                },
            },
        )
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        first = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(first["kind"], "escalate")
        final = run_mod.answer(started["run"], pick=0)["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "exhausted")
        self.assertEqual(final["exhausted"], "escalations")
        self.assertEqual(final["escalations"], 1)
        self.assertEqual(final["chooser_calls"], 2)
        # the one answered pick was logged; the second park never got the
        # chance to be
        self.assertEqual(len(_decision_rows("synthetic")), 1)


# --- EMPTY ---------------------------------------------------------------------------


class EmptyEventTests(_IsolatedDecisionsDir):
    def test_empty_is_dispatched_like_any_other_event_when_handled(self):
        graph = _minimal_graph(
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}]}, "exclude": "{{context.tried}}"}},
                    "on": {"PICK": "b", "EMPTY": "recovered"},
                },
                "b": {"type": "final"},
                "recovered": {"type": "final"},
            },
            context={"tried": ["x"]},  # excludes the only menu item -> zero options at runtime
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "recovered")
        self.assertEqual(report["chooser_calls"], 0)

    def test_an_unhandled_empty_parks_with_no_options_rather_than_erroring(self):
        graph = _minimal_graph(
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}]}, "exclude": "{{context.tried}}"}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            },
            context={"tried": ["x"]},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        esc = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(esc["kind"], "escalate")
        self.assertIn("EMPTY", esc["why"])
        self.assertEqual(esc["options"], [])
        self.assertIsNone(esc["question"])  # no meta.ask on this state either
        # this graph has no goal and never took a snapshot -- evidence is
        # still present, all four pieces correctly empty rather than absent
        self.assertEqual(
            esc["evidence"],
            {"goal": None, "h1": None, "url": None, "excerpt": None},
        )
        record = run_mod.status(started["run"])
        self.assertFalse(record["finished"])

    def test_an_empty_park_cannot_be_answered_with_a_pick_only_a_stop(self):
        # `options` is always `[]` for an EMPTY escalation -- nothing a
        # pick could ever match, by construction (run.py's `_fire_event`).
        graph = _minimal_graph(
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}]}, "exclude": "{{context.tried}}"}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            },
            context={"tried": ["x"]},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        run_to_completion(started["run"], started["request"], FakeDriver({}))
        with self.assertRaises(ValueError):
            run_mod.answer(started["run"], pick=0)
        final = run_mod.answer(started["run"], stop="nothing left to try")["request"]
        self.assertEqual(final["outcome"], "stopped")
        self.assertEqual(final["reason"], "nothing left to try")


# --- ERROR -----------------------------------------------------------------------


class ErrorEventTests(_IsolatedDecisionsDir):
    def test_an_unhandled_tool_error_ends_the_run_as_an_error_not_a_crash(self):
        graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        driver = FakeDriver({"bash": {"error": "permission denied"}})
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "error")
        self.assertIn("permission denied", report["error"])

    def test_a_driver_error_is_reason_failed_and_names_the_tool(self):
        # docs/design/observation.md section 4: the "succeeded" belief --
        # a tool's own `is_error` (here, the driver's `{"error": ...}`) is
        # `reason: "failed"`, distinct from `truncated` (whole belief) and
        # `unexpected` (of-kind belief).
        graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        driver = FakeDriver({"bash": {"error": "permission denied"}})
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "error")
        self.assertEqual(report["reason"], "failed")
        self.assertIn("permission denied", report["error"])


# --- forced picks ------------------------------------------------------------------


class ForcedPickTests(_IsolatedDecisionsDir):
    def test_a_single_option_is_taken_without_calling_the_chooser(self):
        graph = _minimal_graph(
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "only"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["chooser_calls"], 0)
        rows = _decision_rows("synthetic")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["source"], "forced")
        self.assertEqual(rows[0]["probs"], [1.0])
        self.assertIsNone(rows[0]["verified"])


# --- idempotency ---------------------------------------------------------------------


class IdempotencyTests(_IsolatedDecisionsDir):
    def test_a_stale_request_id_is_ignored_and_the_pending_request_is_returned_unchanged(self):
        graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        pending = started["request"]
        self.assertEqual(pending["kind"], "act")
        unchanged = run_mod.step(started["run"], "not-the-real-id", {"text": "should be ignored"})["request"]
        self.assertEqual(unchanged, pending)

    def test_replaying_an_already_consumed_request_id_does_not_double_act(self):
        graph = _minimal_graph(
            states={
                "a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}, "into": "obs"}}]},
                "b": {"type": "final"},
            },
        )
        driver = FakeDriver({"bash": [{"text": "first"}]})
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        first_id = started["request"]["id"]
        result = driver.answer(started["request"])
        advanced = run_mod.step(started["run"], first_id, result)["request"]
        self.assertNotEqual(advanced.get("id"), first_id)  # moved on to a new request (or the final report)
        # replay the SAME (now stale) id again with a result that would
        # blow up the fake driver's queue if it were dispatched again
        replayed = run_mod.step(started["run"], first_id, {"text": "should never be consumed"})["request"]
        self.assertEqual(replayed, advanced)
        self.assertEqual(len(driver.calls), 1)  # bash was never called a second time

    def _parked_run(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        esc = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(esc["kind"], "escalate")
        return started["run"], esc

    def test_step_against_a_parked_run_is_a_no_op_and_the_park_is_returned_unchanged(self):
        # an `act` id can never match a run that is not pending an `act` at
        # all -- a step call reaching a parked run is exactly the driver
        # retrying against a request it no longer recognises the kind of.
        run_id, esc = self._parked_run()
        unchanged = run_mod.step(run_id, "whatever-id", {"text": "ignored"})["request"]
        self.assertEqual(unchanged, esc)
        # the no-op did not disturb the one pending escalation
        final = run_mod.answer(run_id, pick=0)["request"]
        self.assertEqual(final["outcome"], "reached")

    def test_answer_against_a_run_pending_an_act_is_a_no_op(self):
        graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        self.assertEqual(started["request"]["kind"], "act")
        unchanged = run_mod.answer(started["run"], pick=0)["request"]
        self.assertEqual(unchanged, started["request"])


# --- automation.stop -----------------------------------------------------------------


class StopTests(_IsolatedDecisionsDir):
    """`automation.stop` reaches a run wherever it currently is -- mid-`act`
    as readily as mid-park -- and answers with the run record (the design's
    contract table groups `.stop` with `.status`/`.runs`, not with `.step`/
    `.answer`'s `{request}`), since stopping never leaves a request
    pending."""

    def test_stop_mid_act_ends_the_run_immediately(self):
        graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        self.assertEqual(started["request"]["kind"], "act")
        record = run_mod.stop_run(started["run"], "operator cancelled")
        self.assertEqual(record["outcome"], "stopped")
        self.assertTrue(record["finished"])
        self.assertEqual(record["reason"], "operator cancelled")
        self.assertNotIn("request", record)  # the run record, not {request}
        # idempotent: stopping again, or stepping the now-dead act request,
        # changes nothing
        again = run_mod.stop_run(started["run"], "should be ignored")
        self.assertEqual(again, record)
        unchanged = run_mod.step(started["run"], started["request"]["id"], {"text": "too late"})["request"]
        self.assertEqual(unchanged["kind"], "final")
        self.assertEqual(unchanged["outcome"], "stopped")

    def test_stop_mid_park_ends_the_run_without_an_answer(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        esc = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(esc["kind"], "escalate")
        record = run_mod.stop_run(started["run"], "timed out waiting for an answer")
        self.assertEqual(record["outcome"], "stopped")
        self.assertEqual(record["reason"], "timed out waiting for an answer")
        # the park itself logged no decision -- but stopping the run is
        # still the run's own end, and S45 wires decisions.log_run_end
        # from Run._settle for every ending, parked-and-abandoned included.
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(
            rows, [{"run": started["run"], "outcome": "stopped", "ts": rows[0]["ts"], "warrant": None}]
        )
        # answering a now-stopped run's (nonexistent) park is a no-op, same
        # idempotency shape as every other post-terminal call
        unchanged = run_mod.answer(started["run"], pick=0)["request"]
        self.assertEqual(unchanged["outcome"], "stopped")

    def test_stop_on_an_unknown_run_names_what_was_lost(self):
        with self.assertRaises(ValueError) as ctx:
            run_mod.stop_run("r_does_not_exist", "irrelevant")
        message = str(ctx.exception)
        self.assertIn("r_does_not_exist", message)
        self.assertIn("automation.start", message)


# --- automation.status ----------------------------------------------------------------


class StatusTests(_IsolatedDecisionsDir):
    """`automation.status` against an id this process does not recognise --
    `run.py`'s own `_no_such_run_message`, same as `.stop`/`.answer` (see
    `StopTests.test_stop_on_an_unknown_run_names_what_was_lost` and
    `IdempotencyTests` above). Previously exercised only at the HTTP layer
    (jev/tests/test_server_automation.py's
    `test_status_on_an_unknown_run_is_a_clean_ok_false_not_a_500`), and only
    for `ok is False` there -- never for what the message actually says.
    Confirmed by deliberately breaking it (temporarily, restored
    immediately after): swapping `status`'s `_no_such_run_message(run_id)`
    for a bare `"not found"` left every one of jev/tests/'s 182 cases and
    jev/test_server.py's 33 green, because nothing anywhere asserted on the
    message's content. This test closes that gap the way `stop`'s own
    already does."""

    def test_status_on_an_unknown_run_names_what_was_lost(self):
        with self.assertRaises(ValueError) as ctx:
            run_mod.status("r_does_not_exist")
        message = str(ctx.exception)
        self.assertIn("r_does_not_exist", message)
        self.assertIn("automation.start", message)

    def test_answer_on_an_unknown_run_names_what_was_lost(self):
        with self.assertRaises(ValueError) as ctx:
            run_mod.answer("r_does_not_exist", pick=0)
        message = str(ctx.exception)
        self.assertIn("r_does_not_exist", message)
        self.assertIn("automation.start", message)


# --- per-run locking -------------------------------------------------------------------


class ConcurrencyTests(_IsolatedDecisionsDir):
    """`Run._lock` (automation/run.py) -- added because a parked run now
    genuinely outlives the tool call that created it: `jev_resume`/
    `jev_stop` are later, separate tool calls, possibly from a different
    session, so two callers racing to advance the very same run's
    generator -- `answer` and `stop` both reaching for it at once -- is now
    a reachable state that was not reachable before parking existed. See
    `Run`'s own class docstring in run.py.

    The builder's report disclosed this lock as added but untested,
    "modelled structurally on server.py's existing model locks, but
    unexercised." Proven here the same way jev/test_server.py's own
    `ConcurrencyTests` prove `_jevlike_lock`/`_openjev_lock`: wrap the real
    lock so the test can see it actually acquired and held (`_RecordingLock`
    above), not just assume a bare lock would exclude if it were still
    there. The concrete failure this guards against is not hypothetical --
    a Python generator re-entered while another thread is already inside a
    `.send`/`.throw` on it raises `ValueError: generator already executing`,
    straight from CPython's own generator machinery; deliberately removing
    the `with self._lock:` from `Run.answer` and `Run.stop` (temporarily,
    restored immediately after) reproduces exactly that, confirming this
    test fails for the right reason and not merely "an error occurred"."""

    def _parked_run(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        esc = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(esc["kind"], "escalate")
        return started["run"]

    def test_answer_and_stop_racing_the_same_parked_run_are_serialized(self):
        run_id = self._parked_run()
        with run_mod._RUNS_LOCK:
            run = run_mod._RUNS[run_id]

        rec = _RecordingLock(run._lock)
        run._lock = rec

        errors: list[tuple[str, Exception]] = []
        results: list[tuple[str, dict]] = []
        lock = threading.Lock()

        def call_answer():
            try:
                r = run_mod.answer(run_id, pick=0)
            except Exception as e:  # noqa: BLE001 -- recorded, not swallowed
                with lock:
                    errors.append(("answer", e))
            else:
                with lock:
                    results.append(("answer", r))

        def call_stop():
            try:
                r = run_mod.stop_run(run_id, "racing stop")
            except Exception as e:  # noqa: BLE001
                with lock:
                    errors.append(("stop", e))
            else:
                with lock:
                    results.append(("stop", r))

        with ThreadPoolExecutor(max_workers=2) as ex:
            list(ex.map(lambda fn: fn(), [call_answer, call_stop]))

        # Idempotent by design (`Run.answer`/`Run.stop`'s own docstrings):
        # whichever of the two the lock lets through first finishes the
        # run, and the other -- run serially, never concurrently -- must
        # find `self.finished` already true and return cleanly. Under a
        # dropped lock the two `with`-blocks in `Run.answer`/`_stop_locked`
        # interleave and this is exactly where `ValueError: generator
        # already executing` would surface.
        self.assertEqual(errors, [], f"answer/stop raised while racing: {errors}")
        self.assertEqual(
            rec.acquire_count, 2, "answer/stop never acquired Run._lock -- is the `with` still there?"
        )
        self.assertEqual(rec.max_concurrent, 1, "answer and stop held Run._lock at the same time")
        record = run_mod.status(run_id)
        self.assertTrue(record["finished"])
        self.assertIn(record["outcome"], ("reached", "stopped"))

    def test_two_answers_racing_the_same_parked_run_are_serialized(self):
        # Both callers offering a *valid* pick, unlike the mixed
        # answer/stop race above -- the shape a model and a person
        # double-answering the same escalation would actually take.
        run_id = self._parked_run()
        with run_mod._RUNS_LOCK:
            run = run_mod._RUNS[run_id]

        rec = _RecordingLock(run._lock)
        run._lock = rec

        errors: list[tuple[int, Exception]] = []
        lock = threading.Lock()

        def call_answer(pick):
            try:
                run_mod.answer(run_id, pick=pick)
            except Exception as e:  # noqa: BLE001
                with lock:
                    errors.append((pick, e))

        with ThreadPoolExecutor(max_workers=2) as ex:
            list(ex.map(call_answer, [0, 1]))

        self.assertEqual(errors, [], f"racing answers raised: {errors}")
        self.assertEqual(rec.acquire_count, 2)
        self.assertEqual(rec.max_concurrent, 1)
        record = run_mod.status(run_id)
        self.assertTrue(record["finished"])
        self.assertEqual(record["outcome"], "reached")
        # exactly one pick was logged -- the second answer() call found
        # nothing parked (idempotent no-op), never a second, phantom choice
        rows = _decision_rows("synthetic")
        self.assertEqual(len(rows), 1)


# --- automation.runs ------------------------------------------------------------------


class RunsListingTests(_IsolatedDecisionsDir):
    def test_automation_runs_lists_every_run_this_process_has_tracked(self):
        finishing_graph = _minimal_graph(
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "only"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )
        pending_graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        finished = run_mod.start(finishing_graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        self.assertEqual(finished["request"]["kind"], "final")  # forced pick, no act round trip needed
        pending = run_mod.start(pending_graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        self.assertEqual(pending["request"]["kind"], "act")

        listing = run_mod.list_runs()["runs"]
        ids = {r["run"] for r in listing}
        self.assertIn(finished["run"], ids)
        self.assertIn(pending["run"], ids)
        frow = next(r for r in listing if r["run"] == finished["run"])
        self.assertEqual(frow["outcome"], "reached")
        self.assertTrue(frow["finished"])
        prow = next(r for r in listing if r["run"] == pending["run"])
        self.assertIsNone(prow["outcome"])
        self.assertFalse(prow["finished"])
        self.assertEqual(prow["active"], "a")


# --- stale refs --------------------------------------------------------------------


class StaleRefTests(_IsolatedDecisionsDir):
    """"A PICK whose ref is not in the current obs is an ERROR, not a click
    on whatever now has that number" -- docs/design/automation.md section
    2. Forced here by handing the interpreter an `also` option (whose
    `ref` is never in any real snapshot) so the "choice" itself is valid
    but stale by construction, without needing a second driver round-trip
    to go stale for real."""

    def test_picking_a_ref_absent_from_the_current_snapshot_is_an_error(self):
        graph = _minimal_graph(
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "browser_snapshot", "into": "obs"}}],
                    "meta": {
                        "choose": {
                            "from": {"refs": {"roles": ["link"]}},
                            "also": [{"label": "phantom", "ref": "e999"}],
                        }
                    },
                    "on": {"PICK": {"target": "a", "reenter": True, "actions": [
                        {"type": "tool", "params": {"name": "browser_click", "input": {"ref": "{{event.option.ref}}"}}}
                    ]}, "ERROR": "failed"},
                },
                "failed": {"type": "final", "output": {"error": "{{event.error}}"}},
            },
        )
        driver = FakeDriver({"browser_snapshot": {"text": CAT_PAGE}})
        # prefer "phantom" by label, not position -- this graph's refs
        # source has no `within`, so it also picks up CAT_PAGE's nav
        # links; picking by label keeps this test about the one ref that
        # cannot possibly be in the snapshot, regardless of how many real
        # links the fixture carries.
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=prefer_label("phantom"), ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "failed")
        self.assertIn("stale ref", report["output"]["error"])
        self.assertIn("e999", report["output"]["error"])
        # browser_click must never have been dispatched for a stale ref
        self.assertEqual([c["tool"] for c in driver.calls], ["browser_snapshot"])

    def test_a_stale_ref_is_reason_stale(self):
        # docs/design/observation.md section 4: a stale ref fails the same
        # "the runner must not act on data it cannot trust" test as a
        # truncated or unexpected observation, `reason: "stale"`,
        # `tool: "pick"` -- distinct from `failed`/`truncated`/`unexpected`,
        # which all name a `tool` action, not a choice.
        graph = _minimal_graph(
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "browser_snapshot", "into": "obs"}}],
                    "meta": {
                        "choose": {
                            "from": {"refs": {"roles": ["link"]}},
                            "also": [{"label": "phantom", "ref": "e999"}],
                        }
                    },
                    "on": {"PICK": {"target": "a", "reenter": True, "actions": [
                        {"type": "tool", "params": {"name": "browser_click", "input": {"ref": "{{event.option.ref}}"}}}
                    ]}, "ERROR": "failed"},
                },
                "failed": {"type": "final", "output": {"error": "{{event.error}}", "reason": "{{event.reason}}"}},
            },
        )
        driver = FakeDriver({"browser_snapshot": {"text": CAT_PAGE}})
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=prefer_label("phantom"), ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "failed")
        self.assertEqual(report["output"]["reason"], "stale")


# --- invalid observations: the three beliefs ---------------------------------------


def _observation_graph():
    """A state whose only job is to take one tool action, store it (or
    not) into `context.obs`, and report what landed there: `always`
    reaches `stored` unconditionally once entry succeeds (no `choose`
    needed -- this section is about whether `context.obs` gets set at
    all, not about choosing over it); `ERROR` reaches `failed`, echoing
    `event.error`/`event.reason` and `context.obs` verbatim so a test can
    tell directly whether an invalid observation was, in fact, never
    stored."""
    return _minimal_graph(
        states={
            "a": {
                "entry": [{"type": "tool", "params": {"name": "browser_snapshot", "into": "obs"}}],
                "always": "stored",
                "on": {"ERROR": "failed"},
            },
            "stored": {"type": "final", "output": {"obs": "{{context.obs}}"}},
            "failed": {
                "type": "final",
                "output": {"error": "{{event.error}}", "reason": "{{event.reason}}", "obs": "{{context.obs}}"},
            },
        },
    )


class InvalidObservationTests(_IsolatedDecisionsDir):
    """docs/design/observation.md section 4, "An invalid observation is
    never stored, never guarded over, and never scored": the whole
    belief, checked by comparing a stamped result's body length against
    its own producer's declared `chars:` -- replacing the old, narrower
    mechanism (`TruncatedSnapshotTests`) that only ever recognised one
    specific transport's own marker text and missed every other way a
    result could arrive short. `StaleRefTests` and `ErrorEventTests`
    cover the other two beliefs (stale, failed); `ExpectGuardTests` below
    covers the third (unexpected)."""

    def test_a_stamped_result_whose_length_disagrees_is_an_error_with_reason_truncated(self):
        body = CAT_PAGE.split("\n\n", 1)[1]
        # declares far more than actually arrives -- the shape any
        # transport-layer shortening takes, not one specific marker's.
        mismatched = f"url: https://x\ntitle: T\nscope: main\nchars: 999999\n\n{body}"
        driver = FakeDriver({"browser_snapshot": {"text": mismatched}})
        started = run_mod.start(
            _observation_graph(), {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "failed")
        self.assertEqual(report["output"]["reason"], "truncated")
        # the message names both numbers: what arrived and what was declared
        self.assertIn(str(len(body)), report["output"]["error"])
        self.assertIn("999999", report["output"]["error"])
        # never stored -- an invalid observation is never stored
        self.assertIsNone(report["output"]["obs"])

    def test_a_stamped_result_whose_length_agrees_is_stored(self):
        body = CAT_PAGE.split("\n\n", 1)[1]
        whole = f"url: https://x\ntitle: T\nscope: main\nchars: {len(body)}\n\n{body}"
        driver = FakeDriver({"browser_snapshot": {"text": whole}})
        started = run_mod.start(
            _observation_graph(), {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "stored")
        self.assertIsNotNone(report["output"]["obs"])
        self.assertEqual(report["output"]["obs"]["scope"], "main")
        self.assertEqual(report["output"]["obs"]["chars"], len(body))

    def test_an_unstamped_result_is_stored_without_a_verdict_on_wholeness(self):
        # CAT_PAGE carries no `chars:` line at all -- unverifiable, not
        # whole, and run.py stores it anyway rather than refusing it.
        driver = FakeDriver({"browser_snapshot": {"text": CAT_PAGE}})
        started = run_mod.start(
            _observation_graph(), {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["outcome"], "reached")
        self.assertEqual(report["final_state"], "stored")
        self.assertIsNotNone(report["output"]["obs"])
        self.assertIsNone(report["output"]["obs"]["chars"])


class ExpectGuardTests(_IsolatedDecisionsDir):
    """docs/design/observation.md section 4's "of-kind" belief: the
    `expect` guard on a `tool` action's `params`, evaluated once, after
    the whole/succeeded beliefs both hold, through the same
    `guards.evaluate_guard` every other guard site in this package
    already uses -- no second expression language, per that section's own
    ruling. The guard's probe scope carries the *candidate* observation at
    `context.<into>`, so `expect` inspects the very result it is judging
    before that result is ever stored."""

    def test_a_failing_expect_is_reason_unexpected_and_the_observation_is_not_stored(self):
        # The J1 failure mode this whole document exists to close
        # (docs/Decisions.md): `judge` used to score a `ps` usage error at
        # 0.995 confidence as "a process that does not belong on a
        # server", because confidence is measured over the options, never
        # over the input. Exercised against the real, unmodified
        # triage-linux.json -- its own "processes" entry gained this
        # step's `expect: {"type": "matches", "pattern":
        # "^COMMAND\\s+PID\\s+USER"}`. `ps` fails this way once, then
        # succeeds on retry (same shape as
        # TriageLinuxEndToEndTests' own history-resume test) so the run
        # terminates instead of looping the escalations budget out.
        failed_once = [False]

        def bash(request):
            cmd = request["input"]["command"]
            if cmd.startswith("ps ") and not failed_once[0]:
                failed_once[0] = True
                return {"text": "ps: unknown option -- o\nTry `ps --help' for more information."}
            return TriageLinuxEndToEndTests._bash(request)

        driver = FakeDriver({"bash": bash})
        started = run_mod.start(
            "triage-linux", {}, graphs_dir=GRAPHS_DIR,
            chooser=TriageLinuxEndToEndTests._triage_chooser, ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], driver)

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["final_state"], "report")
        self.assertIn("recover", report["path"])
        notes = report["output"]["notes"]
        self.assertTrue(any("did not satisfy expect" in n for n in notes))
        # the usage text itself never becomes a "processes: ..." note --
        # context.obs was never overwritten with it, because an
        # observation that fails `expect` is never stored. Once the retry
        # succeeds, exactly one real "processes: ..." note lands, carrying
        # the real listing, not the usage error.
        self.assertFalse(any("unknown option" in n for n in notes))
        processes_notes = [n for n in notes if n.startswith("processes:")]
        self.assertEqual(len(processes_notes), 1)
        self.assertIn("COMMAND PID USER", processes_notes[0])

    def test_a_passing_expect_stores_the_observation(self):
        graph = _minimal_graph(
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {
                        "name": "bash", "into": "obs",
                        "expect": {"type": "matches", "params": {"path": "context.obs.text", "pattern": "^COMMAND"}},
                    }}],
                    "always": "stored",
                    "on": {"ERROR": "failed"},
                },
                "stored": {"type": "final", "output": {"obs": "{{context.obs}}"}},
                "failed": {"type": "final", "output": {"error": "{{event.error}}", "reason": "{{event.reason}}"}},
            },
        )
        driver = FakeDriver({"bash": {"text": "COMMAND PID USER\nsshd 1 root\n"}})
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["final_state"], "stored")
        self.assertIsNotNone(report["output"]["obs"])
        self.assertIn("COMMAND PID USER", report["output"]["obs"]["text"])

    def test_an_expect_may_be_an_nli_guard(self):
        # mocked `entail`; one call; pass and fail -- the two NLI guard
        # kinds (`entails`/`contradicts`) reach `expect` through the exact
        # same `guards.evaluate_guard` a transition's own `guard` does, so
        # this pins only that `expect` actually reaches it, not NLI
        # semantics themselves (guards.py's own suite covers those).
        graph = _minimal_graph(
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {
                        "name": "bash", "into": "obs",
                        "expect": {"type": "entails", "params": {
                            "premise": "{{context.obs.text}}",
                            "hypothesis": "This is a process listing.",
                            "threshold": 0.6,
                        }},
                    }}],
                    "always": "stored",
                    "on": {"ERROR": "failed"},
                },
                "stored": {"type": "final", "output": {"obs": "{{context.obs}}"}},
                "failed": {"type": "final", "output": {"error": "{{event.error}}", "reason": "{{event.reason}}"}},
            },
        )
        text = "COMMAND PID USER\nsshd 1 root\n"

        calls = []

        def entail_pass(premise, hypotheses):
            calls.append((premise, list(hypotheses)))
            return [{"contradiction": 0.05, "entailment": 0.9, "neutral": 0.05}]

        driver = FakeDriver({"bash": {"text": text}})
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, entail=entail_pass, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], driver)
        self.assertEqual(report["final_state"], "stored")
        self.assertEqual(len(calls), 1)

        def entail_fail(premise, hypotheses):
            return [{"contradiction": 0.9, "entailment": 0.05, "neutral": 0.05}]

        driver2 = FakeDriver({"bash": {"text": text}})
        started2 = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, entail=entail_fail, ckpt_id=None)
        report2 = run_to_completion(started2["run"], started2["request"], driver2)
        self.assertEqual(report2["final_state"], "failed")
        self.assertEqual(report2["output"]["reason"], "unexpected")


# --- capture: docs/design/triage.md section 3.2, "evidence is typed at the
# point it is extracted" -----------------------------------------------------------


# The observed netstat line from that document's own section 3.1 (78 bytes,
# `netstat -ano` on this box) -- the real shape section 3.2's pattern was
# written against, not a convenient invention.
NETSTAT_LINE = "  TCP    127.0.0.1:8085         0.0.0.0:0              LISTENING       31900"
# section 3.2's worked pattern, verbatim.
NETSTAT_PORT_PATTERN = r"(?m)^\s*TCP\s+(?P<addr>\S+):8085\s+\S+\s+LISTENING\s+(?P<pid>\d+)"
# section 3.2's *anchored* form: `cwd` required, `config` optional -- "the
# anchored form `--cwd\s+(?P<cwd>\S+)(?:.*--config\s+(?P<config>\S+))?`
# captures `cwd` and leaves `config` unset". (The unanchored first draft that
# matched the real command line and captured nothing is the lint test in
# test_graph.py, exactly as that section records it.)
CMDLINE_PATTERN = r"--cwd\s+(?P<cwd>\S+)(?:.*--config\s+(?P<config>\S+))?"
# `event.option.line` sees one stripped line, so this one needs no `(?m)`
# anchor to be unambiguous -- and, deliberately, no port in `addr`, so a
# capture reading the *whole* listing instead of the chosen line picks a
# different line's fields and the test below can tell the two apart.
LINE_FIELD_PATTERN = r"^\s*TCP\s+(?P<addr>\S+)\s+\S+\s+LISTENING\s+(?P<pid>\d+)"

TWO_NETSTAT_LINES = (
    "  TCP    127.0.0.1:8085         0.0.0.0:0              LISTENING       31900\n"
    "  TCP    127.0.0.1:9999         0.0.0.0:0              LISTENING       22222\n"
)


def _capture_graph(params, *, context=None, error_state="failed"):
    """One state whose only job is to take one capture action, then report
    what landed in `context.case`: `always` reaches `stored` once entry
    succeeds, and an `ERROR` reaches `error_state` echoing
    `event.error`/`event.reason`/`event.tool` and `context.case` verbatim, so
    a test can tell directly whether a failed capture typed anything."""
    return _minimal_graph(
        context=context if context is not None else {},
        states={
            "a": {
                "entry": [{"type": "capture", "params": params}],
                "always": "stored",
                "on": {"ERROR": error_state},
            },
            "stored": {"type": "final", "output": {"case": "{{context.case}}"}},
            error_state: {
                "type": "final",
                "output": {
                    "error": "{{event.error}}",
                    "reason": "{{event.reason}}",
                    "tool": "{{event.tool}}",
                    "case": "{{context.case}}",
                },
            },
        },
    )


class CaptureActionTests(_IsolatedDecisionsDir):
    """docs/design/triage.md section 3.2's one new action: `re.search` with
    `MULTILINE` -- `matches` with groups -- assigning each *participating*
    named group under `context.<into>`. Four rules from that section, one
    test each: evidence is bound to the actual source; an unparticipating
    optional group assigns nothing (an absence is a finding, not a failure)
    and never overwrites what is already there; the pattern not matching at
    all is `ERROR`/`unexpected`/`tool: "capture"`, the same error a failed
    `expect` raises, not a fourth reason; and `path` may be
    `event.option.line`, so the chooser selects the line and the pattern
    extracts the field."""

    def test_a_capture_assigns_each_participating_named_group(self):
        graph = _capture_graph(
            {"path": "context.obs.text", "pattern": NETSTAT_PORT_PATTERN, "into": "case"},
            context={"obs": {"text": NETSTAT_LINE}},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["final_state"], "stored")
        # bound to the actual source line's own fields, not to anything the
        # pattern could have been written to match by accident
        self.assertEqual(report["output"]["case"], {"addr": "127.0.0.1", "pid": "31900"})
        # a capture dispatches nothing: no `act` request ever went out (the
        # fake driver was never asked for one), so nothing it does counts
        # against the actions budget, the same as assign/push/inc
        self.assertEqual(report["actions"], 0)

    def test_an_optional_group_that_did_not_participate_is_not_assigned(self):
        # "A group that did not participate assigns nothing ... and its
        # absence is a finding, not a failure". Two halves: a value an
        # earlier step established is never overwritten with `None`, and on
        # a context with no `case` yet the absent group is absent, not
        # present-and-null.
        established = {"config": "established-by-an-earlier-capture"}
        graph = _capture_graph(
            {"path": "context.obs.text", "pattern": CMDLINE_PATTERN, "into": "case"},
            context={"obs": {"text": "/usr/sbin/nginx --cwd /srv/app -g daemon off;"}, "case": dict(established)},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["final_state"], "stored")
        case = report["output"]["case"]
        self.assertEqual(case["cwd"], "/srv/app")  # the participating group, from the source
        self.assertEqual(case["config"], "established-by-an-earlier-capture")  # not clobbered with None
        self.assertNotIn(None, case.values())
        self.assertEqual(len(case), 2)

        fresh = _capture_graph(
            {"path": "context.obs.text", "pattern": CMDLINE_PATTERN, "into": "case"},
            context={"obs": {"text": "/usr/sbin/nginx --cwd /srv/other -g daemon off;"}},
        )
        started2 = run_mod.start(fresh, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report2 = run_to_completion(started2["run"], started2["request"], FakeDriver({}))
        self.assertEqual(report2["output"]["case"], {"cwd": "/srv/other"})

    def test_a_capture_that_does_not_match_raises_error_unexpected_naming_capture(self):
        # Not the kind of result this action asked for -- a `ps` listing
        # where a netstat row was expected. Section 3.2: the same `ERROR`
        # flow `_run_tool_action` raises for a failed `expect`
        # (`reason: "unexpected"`), reached one step later, and recovered
        # from the same way (`on: {ERROR: ...}`).
        graph = _capture_graph(
            {"path": "context.obs.text", "pattern": NETSTAT_PORT_PATTERN, "into": "case"},
            context={"obs": {"text": "COMMAND PID USER\nsshd 100 root\n"}},
            error_state="recover",
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["final_state"], "recover")
        self.assertIn("recover", report["path"])
        self.assertEqual(report["output"]["reason"], "unexpected")
        self.assertEqual(report["output"]["tool"], "capture")
        # the diagnostic names both the path and the pattern
        self.assertIn("context.obs.text", report["output"]["error"])
        self.assertIn(NETSTAT_PORT_PATTERN, report["output"]["error"])
        # nothing was typed: a capture that did not match creates no `case`
        self.assertIsNone(report["output"]["case"])

    def test_a_capture_reads_the_chosen_line_after_a_lines_pick(self):
        # "`capture` may read any scope path, including `event.option.line`
        # after a `lines` pick, so the division of labour is: the chooser
        # selects (which line), the author's pattern extracts (which
        # field)." The two lines carry different addresses and pids, so a
        # capture that read the whole `context.obs.text` instead would bind
        # the *first* line's fields -- this assertion is what tells the two
        # apart.
        graph = _minimal_graph(
            context={"obs": {"text": TWO_NETSTAT_LINES}},
            states={
                "a": {
                    "meta": {"choose": {"from": {"lines": {"of": "context.obs.text"}}}},
                    "on": {
                        "PICK": {
                            "target": "b",
                            "actions": [{"type": "capture", "params": {
                                "path": "event.option.line",
                                "pattern": LINE_FIELD_PATTERN,
                                "into": "case",
                            }}],
                        }
                    },
                },
                "b": {"type": "final", "output": {"case": "{{context.case}}"}},
            },
        )
        chosen_line = TWO_NETSTAT_LINES.splitlines()[1].strip()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=prefer_label(chosen_line), ckpt_id=None,
        )
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))

        self.assertEqual(report["outcome"], "reached", report)
        self.assertEqual(report["chooser_calls"], 1)
        self.assertEqual(report["output"]["case"], {"addr": "127.0.0.1:9999", "pid": "22222"})


# --- persistence: escalate-parked runs survive a restart, act-suspended ones don't ---


class PersistenceTests(_IsolatedDecisionsDir):
    """docs/Decisions.md, "Amendment: the persistence refusal was right
    about half a program" (D2d). Verified against the actual code, not
    just trusted, before any of this was built: `_park` has exactly two
    call sites -- the floor/margin miss in `_choose_phase`, the unhandled
    `EMPTY` in `_fire_event` -- both reached only through `_choose_phase`,
    itself called from exactly one place, `_interpret`'s main loop. A
    single, fixed, shallow position every time, unlike `_run_tool_action`'s
    `act` yield, which can occur at arbitrary cascade depth through
    `_enter`/`_execute_transition`/`_handle_final_entered`, with `into =
    params.get("into", "obs")` staying local to the frame and never
    appearing in the yielded request. That confirmed split is why only
    `_park` gained a write hook -- `test_an_act_suspended_run_writes_no_
    snapshot_at_all` below is the direct negative proof of the other half.

    Every "restart" below is genuine, not faked by handing a live object
    back to itself: `_simulate_restart` drops the only reference this
    process holds to a `Run` (`_RUNS.pop`) and forces collection so the
    still-suspended generator's cleanup runs deterministically rather than
    whenever CPython's refcounting happens to get to it. The only way back
    to any run id afterward is `_lookup` reading that run's own file from
    disk -- "a fresh interpreter state reading what the previous one
    wrote."

    That determinism is not incidental: forcing it is exactly what first
    found a real bug here. `_park`/`_resume_parked_choice` originally
    wrapped `answer = yield escalation` in a bare `try/finally`, and
    `_simulate_restart` -- collecting the only reference to a still-parked
    `Run` -- raises `GeneratorExit` at exactly that `yield`, which a bare
    `finally` cannot tell apart from a real resolution: it wrote a
    spurious `"resolved"` marker for a park nothing had actually answered,
    and the very next reconstruction reported a live, resumable run as a
    lost `orphaned` one instead -- one escalation kind that sometimes
    silently could not retry, which is the exact failure the original
    persistence refusal was protecting against, reintroduced by the fix
    for it. `test_a_dropped_reference_with_no_answer_does_not_mark_the_
    park_resolved` is the direct regression test; `except _RunStopped`
    replacing the bare `finally` (both call sites) is the actual fix,
    confirmed by watching that exact test fail against the bare-`finally`
    version and pass again once each `except _RunStopped` block below was
    reverted back to a bare `try/finally` in turn, one file-mutation at a
    time, and restored immediately after.

    Platform: everything in this class has been run and observed on
    win32 only. `runs_dir()` reuses `_paths.cache_dir()` -- already
    exercised for the POSIX branches and their `PurePosixPath`/
    `WindowsPath("/x").is_absolute()` trap elsewhere -- rather than
    re-deriving the cache root itself, so this class adds no new
    Windows-vs-Linux path surface of its own to observe.
    """

    def setUp(self):
        super().setUp()
        # A run this class starts stays in the module-level `_RUNS` for
        # the rest of the process otherwise (`RunsListingTests` relies on
        # exactly that, for its own purpose) -- but a still-parked one
        # left dangling there past THIS test's own `JEV_RUNS_DIR`
        # isolation window is only garbage-collected at some later,
        # unpredictable point -- at worst, interpreter shutdown, after
        # every test's own env-var cleanup has already run. That is
        # exactly how one run of this same class's own break-test rounds
        # (temporarily reverting the `except _RunStopped` fix to prove it
        # necessary -- see this class's own docstring) leaked two real
        # "resolved" markers into the production runs directory: a park
        # this class deliberately never answers
        # (`test_park_writes_a_resumable_snapshot_before_the_caller_sees_
        # the_escalation`) or a park abandoned mid-test by an intentional
        # assertion failure outlived its own test's isolation, and was
        # only closed -- via the then-reverted bare `finally` -- once the
        # whole test *process* exited and `JEV_RUNS_DIR` was long since
        # popped back to unset. Found, explained, and the two files
        # removed by hand; this closes the window for good rather than
        # relying on GC timing, by dropping every run this test itself
        # created right after the test, regardless of what state the code
        # under test is in.
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
        return _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"PICK": "b"}},
                "b": {"meta": {"choose": {"from": {"menu": [{"label": "p"}, {"label": "q"}]}}}, "on": {"PICK": "c"}},
                "c": {"type": "final"},
            },
        )

    def _park_then_act_graph(self):
        # A PICK whose transition carries a tool action -- so *answering*
        # the park is not the end of the story, the same shape
        # `StopTests.test_stop_mid_act_ends_the_run_immediately` uses to
        # reach a live `act`, just entered through a park first.
        return _minimal_graph(
            meta={"jev": {"schema": 1, "defaults": {"floor": 0.9, "margin": 0.1}}},
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": {"target": "b", "actions": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}},
                },
                "b": {"type": "final"},
            },
        )

    def _simulate_restart(self, run_id: str) -> None:
        """The one, genuine way every test below drops a run: remove it
        from `_RUNS` -- the only place a live process keeps it -- and
        force collection so a still-suspended generator's cleanup fires
        now, deterministically, rather than whenever CPython's own
        refcounting happens to reclaim it. Not a simulation faked by
        handing the same object back to itself: the only way back to
        `run_id` after this is `_lookup` reading its file from disk."""
        import gc

        with run_mod._RUNS_LOCK:
            run_mod._RUNS.pop(run_id, None)
        gc.collect()
        self.assertNotIn(run_id, run_mod._RUNS, "run still referenced elsewhere -- restart not actually simulated")

    # -- path encoding (pure, underlies active/history/visits below) ------

    def test_path_encoding_round_trips(self):
        self.assertEqual(run_mod._encode_path(("a", "b", "c")), "a.b.c")
        self.assertEqual(run_mod._decode_path("a.b.c"), ("a", "b", "c"))
        self.assertEqual(run_mod._encode_path(()), "")
        self.assertEqual(run_mod._decode_path(""), ())

    # -- what a park writes, and what an act-suspension does not ----------

    def test_park_writes_a_resumable_snapshot_before_the_caller_sees_the_escalation(self):
        graph = self._single_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id="ckpt-1",
        )
        run_id = started["run"]
        self.assertEqual(started["request"]["kind"], "escalate")
        snap = run_mod._run_file(run_id)
        self.assertTrue(snap.is_file())
        lines = _read_jsonl(snap)
        self.assertEqual(len(lines), 1)
        record = lines[0]
        self.assertEqual(record["kind"], "escalate")
        self.assertEqual(record["run"], run_id)
        self.assertEqual(record["graph_ref"], graph)
        self.assertEqual(record["active"], "a")
        self.assertEqual(record["ckpt_id"], "ckpt-1")
        self.assertEqual(record["counts"]["escalations"], 1)
        self.assertEqual(record["counts"]["chooser_calls"], 1)
        self.assertEqual(record["escalation"], started["request"])
        self.assertIsInstance(record["elapsed_before_park"], float)
        for key in ("at", "start_wall", "context", "history", "visits", "warnings", "path_log", "input"):
            self.assertIn(key, record)

    def test_a_park_snapshot_carries_the_warrant_and_a_restart_keeps_it(self):
        # docs/design/unattended.md section 5's seventh `WarrantedRunTests`
        # case, kept here instead because it needs this class's own
        # `_simulate_restart` -- a genuine drop-and-reconstruct, not a
        # live object handed back to itself (this class's own docstring).
        # The one `bash` dispatch this graph ever names lives on the PICK
        # transition a park-then-restart-then-status walk never reaches,
        # same shape as `WarrantedRunTests._warranted_park_graph` -- so
        # `_lint_warrant`'s tools-set-equality lints clean without this
        # test ever needing a driver.
        graph = _minimal_graph(
            meta={
                "jev": {
                    "schema": 1,
                    "defaults": {"floor": 0.9, "margin": 0.1},
                    "warrant": {"tools": ["bash"]},
                }
            },
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": {"target": "b", "actions": [
                        {"type": "tool", "params": {"name": "bash", "input": {"command": "true"}}}
                    ]}},
                },
                "b": {"type": "final"},
            },
        )
        loaded = graph_mod.load_graph(graph)
        block = warrant_mod.canonical(loaded)
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4),
            ckpt_id=None, warrant=block,
        )
        run_id = started["run"]
        self.assertEqual(started["request"]["kind"], "escalate")
        self.assertEqual(started["request"]["warrant"]["id"], warrant_mod.warrant_id(block))

        snap = run_mod._run_file(run_id)
        record = _read_jsonl(snap)[0]
        self.assertEqual(record["warrant"], block)

        self._simulate_restart(run_id)
        status = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))
        self.assertEqual(status["warrant"]["id"], warrant_mod.warrant_id(block))
        self.assertEqual(status["warrant"]["tools"], ["bash"])

    def test_park_snapshot_write_goes_through_paths_append_locked(self):
        # Mirrors test_decisions.py's CrossProcessLockReuseTests: not a
        # second locking implementation, the same shared helper
        # automation/decisions.py and jev/server.py's own _log_decision
        # already use. Proven with teeth: break _paths.append_locked and
        # watch the write break the same way, not silently keep working
        # through some other path.
        import unittest.mock as mock

        import _paths

        graph = self._single_park_graph()
        with mock.patch.object(_paths, "append_locked", side_effect=OSError("disk is on fire")) as spy:
            started = run_mod.start(
                graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
            )
        spy.assert_called_once()
        self.assertEqual(started["request"]["kind"], "escalate")

    def test_an_act_suspended_run_writes_no_snapshot_at_all(self):
        graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        self.assertEqual(started["request"]["kind"], "act")
        self.assertFalse(run_mod._run_file(started["run"]).is_file())

    # -- the GeneratorExit regression -------------------------------------

    def test_a_dropped_reference_with_no_answer_does_not_mark_the_park_resolved(self):
        graph = self._single_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        self._simulate_restart(run_id)
        lines = _read_jsonl(run_mod._run_file(run_id))
        self.assertEqual(len(lines), 1)
        self.assertEqual(lines[0]["kind"], "escalate")  # not "resolved" -- nothing ever answered this
        record = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))
        self.assertFalse(record["finished"])
        self.assertIsNone(record["outcome"])
        self.assertEqual(record["active"], "a")

    # -- genuine restart + resume to real completion -----------------------

    def test_a_genuine_restart_reconstructs_and_resumes_an_escalate_parked_run_to_completion(self):
        graph = self._single_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id="ck",
        )
        run_id = started["run"]
        self._simulate_restart(run_id)

        # bare status(), no reconstruction kwargs: unchanged pre-persistence behaviour
        with self.assertRaises(ValueError) as ctx:
            run_mod.status(run_id)
        self.assertIn(run_id, str(ctx.exception))
        self.assertIn("automation.start", str(ctx.exception))

        # with reconstruction kwargs: genuinely still parked, not "orphaned"
        record = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))
        self.assertFalse(record["finished"])
        self.assertIsNone(record["outcome"])
        self.assertEqual(record["active"], "a")
        self.assertEqual(record["counts"]["escalations"], 1)

        final = run_mod.answer(
            run_id, pick=1, by="human", graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4),
        )["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "reached")

        rows = _decision_rows("synthetic")
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]["chosen"], "y")
        self.assertEqual(rows[0]["source"], "human")
        self.assertEqual(rows[0]["verified"], "human")

    def test_last_line_wins_across_two_parks_and_two_restarts(self):
        graph = self._double_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        self._simulate_restart(run_id)

        record = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))
        self.assertEqual(record["active"], "a")

        second = run_mod.answer(
            run_id, pick=1, by="human", graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4),
        )["request"]
        self.assertEqual(second["kind"], "escalate")
        self.assertEqual(second["state"], "b")

        # answering "a" writes its own "resolved" line, then reaching "b"'s
        # park writes a fresh "escalate" -- three lines, and the file's
        # *first* line is still a stale "escalate" for "a"
        lines = _read_jsonl(run_mod._run_file(run_id))
        self.assertEqual([l["kind"] for l in lines], ["escalate", "resolved", "escalate"])
        self.assertEqual([l["active"] for l in lines], ["a", "a", "b"])

        # a second genuine restart: reconstruction must read the *last*
        # line, not the first "escalate" it finds, to land on "b"
        self._simulate_restart(run_id)
        record2 = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))
        self.assertFalse(record2["finished"])
        self.assertEqual(record2["active"], "b")

        final = run_mod.answer(
            run_id, pick=0, by="human", graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4),
        )["request"]
        self.assertEqual(final["kind"], "final")
        self.assertEqual(final["outcome"], "reached")
        rows = _decision_rows("synthetic")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["chosen"], "y")
        self.assertEqual(rows[1]["chosen"], "p")

    # -- stopping a reconstructed park --------------------------------------

    def test_stopping_a_reconstructed_park_writes_a_resolved_marker_and_ends_the_run(self):
        graph = self._single_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        self._simulate_restart(run_id)
        record = run_mod.stop_run(
            run_id, "operator gave up", graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4),
        )
        self.assertEqual(record["outcome"], "stopped")
        self.assertEqual(record["reason"], "operator gave up")
        lines = _read_jsonl(run_mod._run_file(run_id))
        self.assertEqual([l["kind"] for l in lines], ["escalate", "resolved"])

    def test_a_restart_after_a_clean_stop_cannot_tell_it_apart_from_a_later_crash_and_says_so(self):
        # _persist_resolved's own line is deliberately thin -- no
        # outcome/reason, only active/counts/path_log/graph_ref/ckpt_id --
        # so a *further* restart genuinely cannot distinguish "this run
        # was cleanly stopped right here" from "this run crashed again
        # right after answering its last park": both leave the exact same
        # "resolved" line as their last word. Reporting "orphaned" here
        # rather than guessing "stopped" is the honest answer, not a bug --
        # exactly "two kinds that each tell the truth."
        graph = self._single_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        self._simulate_restart(run_id)
        run_mod.stop_run(run_id, "operator gave up", graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))

        self._simulate_restart(run_id)
        record = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))
        self.assertTrue(record["finished"])
        self.assertEqual(record["outcome"], "orphaned")
        self.assertIn(run_id, record["lost"])

        # Yet the decisions log remembers what actually happened here: the
        # first restart's `stop_run` ran on a genuinely live, reconstructed
        # `Run` (from_snapshot, still `"escalate"`-parked at that point) and
        # reached `Run._settle` for real, so S45's `_log_run_end` logged
        # `outcome="stopped"` at the moment that was actually known -- even
        # though `_persist_resolved`'s own thin marker cannot tell *this*
        # restart the same thing. The log and the reconstruction disagree
        # on purpose: one is a durable record of a fact witnessed live, the
        # other a conservative read of a file that was never meant to carry
        # enough to settle it.
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(rows, [{"run": run_id, "outcome": "stopped", "ts": rows[0]["ts"], "warrant": None}])

    # -- act-suspended orphans: report what was lost, and stop -------------

    def test_an_act_suspended_run_is_reported_orphaned_after_a_park_then_a_second_crash(self):
        graph = self._park_then_act_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        self.assertEqual(started["request"]["kind"], "escalate")
        answered = run_mod.answer(run_id, pick=0, by="human")["request"]
        self.assertEqual(answered["kind"], "act")  # PICK's own transition action, never resolved below

        self._simulate_restart(run_id)

        record = run_mod.status(run_id, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4))
        self.assertTrue(record["finished"])
        self.assertEqual(record["outcome"], "orphaned")
        self.assertIn(run_id, record["lost"])
        self.assertIn("automation.start", record["lost"])
        # the last position _persist_resolved actually knows -- the park
        # answer landed at "a", before the act that followed it ever ran
        self.assertEqual(record["active"], "a")

        # idempotent, exactly like every other already-finished run
        again = run_mod.answer(run_id, pick=0, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0))["request"]
        self.assertEqual(again["outcome"], "orphaned")
        stopped = run_mod.stop_run(run_id, "closing out", graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0))
        self.assertEqual(stopped["outcome"], "orphaned")

        # the lost act was never logged -- only the one real pick was.
        # Deliberately `_read_jsonl`, not `_decision_rows`: this is also
        # the negative proof for S45's `_log_run_end` -- `outcome=
        # "orphaned"` never reaches `Run._settle` at all
        # (`_run_from_lost_record` builds an already-`finished` `Run`
        # directly; see its own docstring and `_log_run_end`'s), so this
        # file staying at exactly one row -- the pick, with an `id` --
        # proves no run-end row was manufactured for it either, not only
        # that nothing else was. Filtering through `_decision_rows` here
        # would hide exactly the row this test exists to rule out.
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(len(rows), 1, rows)
        self.assertIn("id", rows[0])

    # -- _no_such_run_message stays honest ----------------------------------

    def test_a_never_known_id_gets_the_unchanged_message_even_with_reconstruction_kwargs(self):
        with self.assertRaises(ValueError) as ctx:
            run_mod.status("r_totally_bogus_never_existed", graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0))
        message = str(ctx.exception)
        self.assertIn("r_totally_bogus_never_existed", message)
        self.assertIn("automation.start", message)
        self.assertFalse(run_mod._run_file("r_totally_bogus_never_existed").is_file())

    def test_a_known_orphan_without_reconstruction_kwargs_still_gets_the_same_message_not_a_crash(self):
        # _lookup only ever falls back to disk when BOTH graphs_dir and
        # chooser are supplied -- omitting them (every call in this
        # module's own pre-persistence tests) must reproduce the exact old
        # behaviour, not a KeyError/AttributeError trying to reconstruct
        # with nothing to reconstruct with.
        graph = self._single_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        self._simulate_restart(run_id)
        self.assertTrue(run_mod._run_file(run_id).is_file())  # genuinely known, on disk
        with self.assertRaises(ValueError) as ctx:
            run_mod.status(run_id)  # no graphs_dir/chooser at all
        message = str(ctx.exception)
        self.assertIn(run_id, message)
        self.assertIn("automation.start", message)

    # -- write failures are warnings, never crashes -------------------------

    def test_a_park_snapshot_write_failure_is_a_warning_not_a_crash(self):
        import unittest.mock as mock

        import _paths

        graph = self._single_park_graph()
        with mock.patch.object(_paths, "append_locked", side_effect=OSError("disk is on fire")) as spy:
            started = run_mod.start(
                graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
            )
        spy.assert_called_once()
        self.assertEqual(started["request"]["kind"], "escalate")  # the park itself is unaffected
        self.assertFalse(run_mod._run_file(started["run"]).is_file())  # nothing durable actually landed

        # answering for real (mock lifted) still works -- and the run's
        # own final report discloses the earlier failure in its own
        # words, per _persist_park's own docstring: best-effort, a
        # warning, never a raised exception
        final = run_mod.answer(started["run"], pick=0)["request"]
        self.assertEqual(final["outcome"], "reached")
        warnings = final.get("warnings") or []
        self.assertTrue(
            any("park snapshot write failed" in w and "will not survive a service restart" in w for w in warnings),
            warnings,
        )

    def test_a_park_resolved_marker_write_failure_is_a_warning_not_a_crash(self):
        import unittest.mock as mock

        import _paths

        graph = self._single_park_graph()
        started = run_mod.start(
            graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0, top=0.6, rest=0.4), ckpt_id=None,
        )
        run_id = started["run"]
        target = run_mod._run_file(run_id)
        real_append_locked = _paths.append_locked

        def flaky(path, line):
            if path == target:
                raise OSError("disk is on fire")
            return real_append_locked(path, line)

        with mock.patch.object(_paths, "append_locked", side_effect=flaky) as spy:
            final = run_mod.answer(run_id, pick=0)["request"]
        spy.assert_called()
        self.assertEqual(final["outcome"], "reached")  # answering itself still succeeds
        warnings = final.get("warnings") or []
        self.assertTrue(
            any(
                "park-resolved marker write failed" in w and "misread this run's last park as still pending" in w
                for w in warnings
            ),
            warnings,
        )
        # the decision log write -- a different path, under a different
        # directory -- went through untouched; only the run-persistence
        # write was made to fail
        rows = _decision_rows("synthetic")
        self.assertEqual(len(rows), 1)


# --- automation.md section 5: a run's end reaches the decision log --------------------


class RunEndLoggingTests(_IsolatedDecisionsDir):
    """S45 (docs/Tasklist.md): `decisions.log_run_end` existed, tested in
    isolation (`test_decisions.py`'s own `RunEndTests`), and had no caller
    anywhere in this module -- automation.md section 5's outcome
    promotion ("A run's end appends `{run, outcome, ts}`, which is what
    lets the exporter promote that run's unverified rows to `outcome`")
    never had a row to read. `run.py`'s own `_log_run_end` (called from
    `Run._settle`'s `yielded is None` branch) is that caller; see its
    docstring for the full reasoning. Proven below for all four outcomes
    the interpreter itself reaches (`reached`/`stopped`/`error`/
    `exhausted`); `orphaned` is proven NOT to log a row, not just left
    untested -- `PersistenceTests.test_an_act_suspended_run_is_reported_
    orphaned_after_a_park_then_a_second_crash`, already existing, is the
    negative proof: its own `len(rows) == 1` held before this task and
    still holds after, which is itself part of the proof -- a run-end row
    would have made it two."""

    def test_a_reached_run_appends_its_outcome_row(self):
        graph = _minimal_graph(
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "only"}]}}}, "on": {"PICK": "b"}},
                "b": {"type": "final"},
            },
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(report["outcome"], "reached")
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(len(rows), 2, rows)  # the forced pick, then the run-end row
        self.assertIn("id", rows[0])  # the pick, unaffected -- still first
        self.assertEqual(
            rows[1], {"run": started["run"], "outcome": "reached", "ts": rows[1]["ts"], "warrant": None}
        )

    def test_an_error_run_with_no_picks_appends_only_its_outcome_row(self):
        # `_minimal_graph()` unmodified: state "a" has neither an `always`
        # guard nor a `choose` block, so `_run_loop` ends it as an error on
        # the very first iteration, before any pick is ever made -- the
        # cleanest possible isolation of the run-end row alone, with
        # nothing else in the file to distinguish it from.
        graph = _minimal_graph()
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        report = started["request"]
        self.assertEqual(report["kind"], "final")
        self.assertEqual(report["outcome"], "error")
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(
            rows, [{"run": started["run"], "outcome": "error", "ts": rows[0]["ts"], "warrant": None}]
        )

    def test_a_stopped_run_appends_its_outcome_row(self):
        graph = _minimal_graph(
            states={"a": {"entry": [{"type": "tool", "params": {"name": "bash", "input": {}}}]}, "b": {"type": "final"}},
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        self.assertEqual(started["request"]["kind"], "act")  # mid-act, nothing picked yet
        record = run_mod.stop_run(started["run"], "operator cancelled")
        self.assertEqual(record["outcome"], "stopped")
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(
            rows, [{"run": started["run"], "outcome": "stopped", "ts": rows[0]["ts"], "warrant": None}]
        )

    def test_an_exhausted_run_appends_its_outcome_row(self):
        graph = _minimal_graph(
            meta={"jev": {"schema": 1, "budget": {"steps": 2}, "defaults": {"visits": 100}}},
            states={
                "loop": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}},
                    "on": {"PICK": {"target": "loop", "reenter": True}},
                },
                "done": {"type": "final"},
            },
            initial="loop",
        )
        started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=constant_chooser(0), ckpt_id=None)
        report = run_to_completion(started["run"], started["request"], FakeDriver({}))
        self.assertEqual(report["outcome"], "exhausted")
        rows = _read_jsonl(decisions.graph_log_path("synthetic"))
        self.assertEqual(len(rows), 3, rows)  # the 2 real picks the steps budget allowed, then the run-end row
        self.assertEqual(
            rows[-1], {"run": started["run"], "outcome": "exhausted", "ts": rows[-1]["ts"], "warrant": None}
        )

    def test_a_failed_run_end_write_is_a_warning_not_a_crash(self):
        # Mirrors StopTests/PersistenceTests' own `_paths.append_locked`
        # failure tests -- best-effort, per `_log_run_end`'s own docstring,
        # the same shape `_log_pick`'s decision-log write already has.
        # Patches `decisions.log_run_end` directly rather than the shared
        # `_paths.append_locked`: `automation/run.py` reaches it as
        # `decisions.log_run_end` (a module-attribute lookup at call time,
        # not a name bound once at import), and this module and run.py
        # import the identical `automation.decisions` module object, so
        # the patch is visible to run.py without also breaking every other
        # write path that goes through the same shared `append_locked`.
        import unittest.mock as mock

        graph = _minimal_graph()  # errors immediately, no picks -- isolates the run-end write alone
        with mock.patch.object(decisions, "log_run_end", side_effect=OSError("disk is on fire")) as spy:
            started = run_mod.start(graph, {}, graphs_dir=GRAPHS_DIR, chooser=refusing_chooser, ckpt_id=None)
        spy.assert_called_once()
        report = started["request"]
        self.assertEqual(report["kind"], "final")
        self.assertEqual(report["outcome"], "error")  # the run's own ending is unaffected
        warnings = report.get("warnings") or []
        self.assertTrue(any("run-end log write failed" in w for w in warnings), warnings)
        # the mock replaced the real append entirely -- nothing landed on disk
        self.assertEqual(_read_jsonl(decisions.graph_log_path("synthetic")), [])


def _read_jsonl(path: Path) -> list[dict]:
    import json

    if not path.is_file():
        return []
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line.strip()]


def _decision_rows(graph_id: str) -> list[dict]:
    """`_read_jsonl` filtered to decision rows only. S45: a finished run
    now appends its own run-end row (`{run, outcome, ts}`, written by
    `decisions.log_run_end` via `Run._settle` -- see automation/run.py's
    `_log_run_end`) to this same per-graph file, and that row has no `id`
    -- every decision row (`log_decision`) and correction row
    (`correct_decision`) does. Most tests below care about the picks a run
    made, not the marker its ending left behind, so they read through
    this rather than `_read_jsonl` directly; the handful that care about
    the run-end row itself (`RunEndLoggingTests`, and the two `StopTests`/
    `ConfidenceFloorTests` cases where nothing was ever picked) still use
    `_read_jsonl` and check the row's own shape."""
    return [row for row in _read_jsonl(decisions.graph_log_path(graph_id)) if "id" in row]


if __name__ == "__main__":
    unittest.main()

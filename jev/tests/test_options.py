"""Tests for automation/options.py -- docs/design/automation.md section 2's
four option sources and the chooser's default context template.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from automation.options import Option, build_options, chooser_context, first_preferred  # noqa: E402


class _FakeGraph:
    """build_options only ever calls `.default(key, fallback)` on its
    `graph` argument -- a bare stand-in is simpler than loading a real one
    for tests that do not care about lint at all."""

    def __init__(self, defaults=None):
        self._defaults = defaults or {}

    def default(self, key, fallback=None):
        return self._defaults.get(key, fallback)


CAT_PAGE_TREE = (
    '- main:\n'
    '  - heading "Cat" [level=1]\n'
    '  - link "Felis" [ref=e1]\n'
    '  - link "Mammal" [ref=e2]\n'
    '- navigation:\n'
    '  - link "Home" [ref=e3]\n'
    '  - button "Menu"\n'
)


class TransitionsSourceTests(unittest.TestCase):
    def test_each_non_reserved_event_becomes_one_option(self):
        state = {"on": {"CONTINUE": "a", "ABORT": {"target": "b", "description": "stop and report"}}}
        opts, warnings, _ = build_options(state, {"from": "transitions"}, {}, _FakeGraph())
        self.assertEqual(warnings, [])
        by_event = {o.event: o for o in opts}
        self.assertEqual(set(by_event), {"CONTINUE", "ABORT"})
        self.assertEqual(by_event["CONTINUE"].label, "CONTINUE")  # no description -> falls back to the event name
        self.assertEqual(by_event["ABORT"].label, "stop and report")

    def test_reserved_events_are_excluded(self):
        state = {"on": {"PICK": "a", "EMPTY": "b", "ERROR": "c", "GO": "d", "STOP": "e"}}
        opts, _, _ = build_options(state, {"from": "transitions"}, {}, _FakeGraph())
        self.assertEqual({o.event for o in opts}, {"GO", "STOP"})


class RefsSourceTests(unittest.TestCase):
    def test_filters_by_role_and_landmark_and_drops_unnamed_or_refless(self):
        scope = {"context": {"obs": {"text": CAT_PAGE_TREE, "url": "u", "title": "t"}}}
        opts, _, _ = build_options(
            {}, {"from": {"refs": {"roles": ["link"], "within": "main"}}}, scope, _FakeGraph()
        )
        self.assertEqual([o.label for o in opts], ["Felis", "Mammal"])  # not "Home" (nav) or "Menu" (unnamed, no role match)

    def test_default_roles_include_link_and_button(self):
        scope = {"context": {"obs": {"text": '- link "A" [ref=e1]\n- button "B" [ref=e2]\n- checkbox "C" [ref=e3]\n'}}}
        opts, _, _ = build_options({}, {"from": {"refs": {}}}, scope, _FakeGraph())
        self.assertEqual({o.label for o in opts}, {"A", "B"})

    def test_each_option_carries_ref_for_later_stale_ref_checking(self):
        scope = {"context": {"obs": {"text": '- link "A" [ref=e7]\n'}}}
        opts, _, _ = build_options({}, {"from": {"refs": {}}}, scope, _FakeGraph())
        self.assertEqual(opts[0].data["ref"], "e7")

    def test_each_option_carries_url_from_a_property_line(self):
        # "parse_refs writes these into parent["url"], and the option
        # sources read them -- this is how a link's destination reaches
        # the chooser" -- docs/design/automation.md section 2.
        scope = {"context": {"obs": {"text": '- link "A" [ref=e7]\n  - /url: /wiki/A\n'}}}
        opts, _, _ = build_options({}, {"from": {"refs": {}}}, scope, _FakeGraph())
        self.assertEqual(opts[0].data["url"], "/wiki/A")

    def test_a_skipped_line_surfaces_as_a_warning_instead_of_being_swallowed(self):
        # a11y.parse_refs never raises on a line it cannot read -- one bad
        # line must not fail a whole snapshot -- but the count used to be
        # discarded everywhere it was returned (`_refs_of`'s own `records,
        # _skipped = a11y.parse_refs(text)`). This is the one place the
        # count means "the menu you are about to build may be missing an
        # option," so this is where it must stop being swallowed.
        tree = '- link "A" [ref=e1]\n' "- 'this quote never closes\n"
        scope = {"context": {"obs": {"text": tree}}}
        opts, warnings, _ = build_options({}, {"from": {"refs": {}}}, scope, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["A"])
        self.assertEqual(len(warnings), 1)
        self.assertIn("1", warnings[0])
        self.assertIn("skipped", warnings[0])

    def test_no_warning_when_nothing_was_skipped(self):
        scope = {"context": {"obs": {"text": '- link "A" [ref=e1]\n'}}}
        _, warnings, _ = build_options({}, {"from": {"refs": {}}}, scope, _FakeGraph())
        self.assertEqual(warnings, [])

    def test_a_url_pattern_drops_options_whose_href_does_not_match(self):
        # docs/design/unattended.md section 3, "the menu bound": an option
        # whose snapshot url is present but does not match never reaches
        # the chooser -- "Login" has a real href, just not an article one.
        tree = (
            '- link "Felis" [ref=e1]\n'
            '  - /url: /wiki/Felis\n'
            '- link "Login" [ref=e2]\n'
            '  - /url: /w/index.php?title=Special:UserLogin\n'
        )
        scope = {"context": {"obs": {"text": tree}}}
        opts, _, _ = build_options({}, {"from": {"refs": {"url": "^/wiki/"}}}, scope, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["Felis"])

    def test_a_url_pattern_drops_options_with_no_href(self):
        # The other half of the same bound: no url at all is dropped the
        # same as a non-matching one, not treated as "unknown, so allow it".
        tree = (
            '- link "Felis" [ref=e1]\n'
            '  - /url: /wiki/Felis\n'
            '- link "No Href" [ref=e2]\n'
        )
        scope = {"context": {"obs": {"text": tree}}}
        opts, _, _ = build_options({}, {"from": {"refs": {"url": "^/wiki/"}}}, scope, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["Felis"])


class MenuSourceTests(unittest.TestCase):
    def test_menu_items_become_options_with_their_extra_fields_as_data(self):
        menu = [{"label": "os release", "command": "cat /etc/os-release"}, {"label": "who", "command": "who"}]
        opts, _, _ = build_options({}, {"from": {"menu": menu}}, {}, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["os release", "who"])
        self.assertTrue(all(o.event == "PICK" for o in opts))
        self.assertEqual(opts[0].data, {"command": "cat /etc/os-release"})

    def test_a_menu_item_can_declare_its_own_event(self):
        menu = [{"label": "nothing looks wrong", "event": "CLEAN"}]
        opts, _, _ = build_options({}, {"from": {"menu": menu}}, {}, _FakeGraph())
        self.assertEqual(opts[0].event, "CLEAN")


class LinesSourceTests(unittest.TestCase):
    def test_splits_non_empty_lines_after_skipping_a_header(self):
        scope = {"context": {"obs": {"text": "HEADER\nline one\n\nline two\n"}}}
        opts, _, _ = build_options({}, {"from": {"lines": {"skip": 1}}}, scope, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["line one", "line two"])

    def test_of_can_point_at_any_path(self):
        scope = {"context": {"custom": "a\nb\n"}}
        opts, _, _ = build_options({}, {"from": {"lines": {"of": "context.custom"}}}, scope, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["a", "b"])


class ExcludeMaxAlsoOrderingTests(unittest.TestCase):
    """exclude drops tried options, THEN max caps what is left, THEN also
    appends fixed items uncapped -- so a structural escape hatch (an
    "abort" or "nothing looks wrong" option) can never be capped away."""

    def test_exclude_runs_before_max(self):
        # choose.exclude is always a template string (schema.json's
        # $defs/choose), never a raw literal list -- "{{context.tried}}"
        # is how a graph actually spells this; see wiki-hop.json.
        menu = [{"label": str(i)} for i in range(5)]
        scope = {"context": {"tried": ["0", "1"]}}
        opts, _, _ = build_options(
            {}, {"from": {"menu": menu}, "exclude": "{{context.tried}}", "max": 2}, scope, _FakeGraph()
        )
        # excluded first (leaves 2,3,4), then capped to 2 -> 2,3
        self.assertEqual([o.label for o in opts], ["2", "3"])

    def test_also_is_appended_after_max_and_is_never_capped(self):
        menu = [{"label": str(i)} for i in range(5)]
        opts, _, _ = build_options(
            {}, {"from": {"menu": menu}, "max": 1, "also": [{"label": "escape", "event": "ABORT"}]}, {}, _FakeGraph()
        )
        self.assertEqual([o.label for o in opts], ["0", "escape"])

    def test_exclude_reads_a_template_that_resolves_to_a_real_list(self):
        scope = {"context": {"tried": ["a"]}}
        menu = [{"label": "a"}, {"label": "b"}]
        opts, _, _ = build_options({}, {"from": {"menu": menu}, "exclude": "{{context.tried}}"}, scope, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["b"])


class DuplicateLabelTests(unittest.TestCase):
    """docs/design/judgement.md section 2.1, "The menu the record was
    measured on": `_collapse_duplicates` runs right after the source builds
    its raw list, before `exclude`/`max` -- the same definition of
    "duplicate" jev/server.py's `_choose` already enforces for the
    interactive `jev_choose` tool (`len(set(options)) != len(options)`:
    label text, exact string, no folding, regardless of target), just a
    different answer to what to do about one -- collapse to the first
    occurrence rather than refuse, since a live page gives automation no
    operator standing by to go fix it mid-run."""

    def test_duplicate_labels_collapse_to_the_first_occurrence_before_max_is_applied(self):
        # three raw options, two sharing a label. If `max` ran before
        # dedup, capping to 2 would keep both "Felidae" copies and lose
        # "Mammal" outright; dedup-then-max keeps the first "Felidae" and
        # still admits "Mammal" -- proof of both the ordering and of which
        # occurrence survives.
        menu = [
            {"label": "Felidae", "href": "/wiki/Felidae#lead"},
            {"label": "Felidae", "href": "/wiki/Felidae#infobox"},
            {"label": "Mammal", "href": "/wiki/Mammal"},
        ]
        opts, _, duplicates_collapsed = build_options({}, {"from": {"menu": menu}, "max": 2}, {}, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["Felidae", "Mammal"])
        # the survivor carries the FIRST occurrence's data, never a later one's
        self.assertEqual(opts[0].data["href"], "/wiki/Felidae#lead")
        self.assertEqual(duplicates_collapsed, 1)

    def test_collapsing_is_exact_and_does_not_fold_case(self):
        # "Cat", "cat" and "CAT" are three distinct labels to this function
        # -- folding is `choose.prefer`'s `equals.fold` opt-in, never the
        # menu's own default.
        menu = [{"label": "Cat"}, {"label": "cat"}, {"label": "CAT"}]
        opts, _, duplicates_collapsed = build_options({}, {"from": {"menu": menu}}, {}, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["Cat", "cat", "CAT"])
        self.assertEqual(duplicates_collapsed, 0)

    def test_duplicates_collapsed_counts_across_also_too(self):
        menu = [{"label": "Felidae"}, {"label": "Mammal"}]
        opts, _, duplicates_collapsed = build_options(
            {}, {"from": {"menu": menu}, "also": [{"label": "Felidae", "event": "ABORT"}]}, {}, _FakeGraph()
        )
        # the earlier (menu-sourced) Felidae survives; the also-list's copy
        # is the one dropped -- first occurrence in the COMBINED list wins.
        self.assertEqual([o.label for o in opts], ["Felidae", "Mammal"])
        self.assertEqual(opts[0].event, "PICK")
        self.assertEqual(duplicates_collapsed, 1)


class LabelOverrideTests(unittest.TestCase):
    def test_label_is_rerendered_from_each_options_own_data(self):
        menu = [{"label": "ignored", "name": "Felis"}, {"label": "ignored2", "name": "Mammal"}]
        opts, _, _ = build_options({}, {"from": {"menu": menu}, "label": "{{option.name}}"}, {}, _FakeGraph())
        self.assertEqual([o.label for o in opts], ["Felis", "Mammal"])

    def test_label_override_can_see_the_options_own_event(self):
        menu = [{"label": "x", "event": "CLEAN"}]
        opts, _, _ = build_options({}, {"from": {"menu": menu}, "label": "{{option.event}}:{{option.label}}"}, {}, _FakeGraph())
        self.assertEqual(opts[0].label, "CLEAN:x")


class ChooserContextTests(unittest.TestCase):
    def test_explicit_context_template_wins(self):
        scope = {"context": {"goal": "g"}}
        result = chooser_context({"context": "fixed: {{context.goal}}"}, scope, "desc", [])
        self.assertEqual(result, "fixed: g")

    def test_default_puts_the_goal_first_when_present(self):
        scope = {"context": {"goal": "Felis", "obs": {"text": "body"}}}
        result = chooser_context({}, scope, "On an article.", [])
        self.assertEqual(result, "Felis\nOn an article.\nbody")

    def test_default_omits_the_goal_line_when_there_is_no_goal(self):
        scope = {"context": {"obs": {"text": "body"}}}
        result = chooser_context({}, scope, "Triage.", [])
        self.assertEqual(result, "Triage.\nbody")

    def test_an_empty_string_goal_is_treated_as_no_goal(self):
        scope = {"context": {"goal": "", "obs": {"text": "body"}}}
        result = chooser_context({}, scope, "desc", [])
        self.assertEqual(result, "desc\nbody")


class PreferTests(unittest.TestCase):
    """docs/design/judgement.md section 2.2, `choose.prefer`: guards tried
    in order, each evaluated per option with the option added to scope as
    `option.*` -- the first `(rule, option)` pair that passes wins."""

    def test_the_first_rule_and_the_first_option_win_in_that_order(self):
        opts = [
            Option(label="Mammal", event="PICK", data={}),  # matched only by rule[1], never reached
            Option(label="Felidae", event="PICK", data={}),  # rule[0]'s first matching option
            Option(label="Felidae", event="PICK", data={}),  # rule[0]'s second matching option
        ]
        prefer = [
            {"type": "equals", "params": {"path": "option.label", "value": "Felidae"}},
            {"type": "equals", "params": {"path": "option.label", "value": "Mammal"}},
        ]
        result = first_preferred(prefer, opts, {}, [])
        # an earlier rule outranks a later one regardless of menu position
        # (rule[1] would have matched the earlier-positioned opts[0], but
        # rule[0] is tried first and already matches something); within
        # the winning rule, the first matching option in menu order wins
        # (opts[1], not opts[2]).
        self.assertEqual(result, 1)

    def test_a_rule_sees_the_options_own_data_not_only_its_label(self):
        opts = [
            Option(label="Felidae", event="PICK", data={"ref": "e1", "url": "/wiki/Felidae"}),
            Option(label="Mammal", event="PICK", data={"ref": "e2", "url": "/wiki/Mammal"}),
        ]
        prefer = [{"type": "equals", "params": {"path": "option.url", "value": "/wiki/Mammal"}}]
        result = first_preferred(prefer, opts, {}, [])
        self.assertEqual(result, 1)

    def test_no_rule_passing_returns_none(self):
        opts = [Option(label="Felidae", event="PICK", data={}), Option(label="Mammal", event="PICK", data={})]
        prefer = [{"type": "equals", "params": {"path": "option.label", "value": "Bird"}}]
        scope = {"context": {}}
        warnings = []
        result = first_preferred(prefer, opts, scope, warnings)
        self.assertIsNone(result)
        # nothing mutated on a miss -- same opts, same scope, no warnings --
        # so the ordinary floor/margin path downstream sees exactly what it
        # would have with no `prefer` block at all.
        self.assertEqual([o.label for o in opts], ["Felidae", "Mammal"])
        self.assertEqual(scope, {"context": {}})
        self.assertEqual(warnings, [])

    def test_a_rule_value_may_be_a_template_over_context(self):
        opts = [Option(label="Mammal", event="PICK", data={}), Option(label="Felidae", event="PICK", data={})]
        prefer = [{"type": "equals", "params": {"path": "option.label", "value": "{{context.goal}}"}}]
        scope = {"context": {"goal": "Felidae"}}
        result = first_preferred(prefer, opts, scope, [])
        self.assertEqual(result, 1)


if __name__ == "__main__":
    unittest.main()

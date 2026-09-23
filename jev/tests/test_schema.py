"""Tests for automation/graph.py -- docs/design/automation.md section 1's
schema, loaded and linted by hand (no `jsonschema` package in the jevlike
venv -- see graph.py's own docstring). Run with the jevlike venv's python,
same as jev/test_server.py:

    C:\\Users\\dxcen\\Projects\\cms-agent\\models\\jevlike\\.venv\\Scripts\\python.exe jev\\tests\\test_schema.py

Covers: both worked-example graphs load and lint cleanly (the "done" bar's
"a graph file loaded/linted with useful errors" half, on the clean side);
one representative negative case per lint rule (the other half); and
`Graph`'s own small API (`resolve_target`, `state_at`, `is_compound`,
`default`, `budget`) that `run.py` depends on.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from automation.graph import Graph, GraphError, load_graph  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
GRAPHS_DIR = REPO_ROOT / "extensions" / "jev" / "graphs"


def _minimal(**overrides) -> dict:
    """The smallest graph that passes every lint rule, so each negative
    test below can break exactly one thing and know that is the only
    reason it fails."""
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


class WorkedExampleTests(unittest.TestCase):
    """The clean side: both real graphs under extensions/jev/graphs/ load
    and lint without complaint."""

    def test_wiki_hop_loads_and_lints_clean(self):
        g = load_graph(GRAPHS_DIR / "wiki-hop.json")
        self.assertIsInstance(g, Graph)
        self.assertEqual(g.id, "wiki-hop")
        self.assertEqual(g.initial, "open")
        self.assertEqual(set(g.states.keys()), {"open", "reading", "arrived", "failed"})
        self.assertIn(("reading",), g.paths)
        self.assertEqual(g.budget("steps", 0), 30)
        self.assertEqual(g.default("floor", 0), 0.35)

    def test_triage_linux_loads_and_lints_clean(self):
        g = load_graph(GRAPHS_DIR / "triage-linux.json")
        self.assertEqual(g.id, "triage-linux")
        self.assertEqual(g.initial, "investigate")
        # nested paths are indexed by full tuple, not just the leaf name
        self.assertIn(("investigate", "identify"), g.paths)
        self.assertIn(("investigate", "network"), g.paths)
        self.assertIn(("investigate", "processes"), g.paths)
        self.assertIn(("investigate", "judge"), g.paths)
        self.assertIn(("investigate", "hist"), g.paths)
        self.assertTrue(g.is_compound(("investigate",)))
        self.assertFalse(g.is_compound(("investigate", "identify")))
        self.assertEqual(g.budget("actions", 0), 40)


class ResolveTargetTests(unittest.TestCase):
    def setUp(self):
        self.g = load_graph(
            _minimal(
                states={
                    "a": {"initial": "x", "states": {"x": {"on": {"GO": "y"}}, "y": {"on": {"UP": "#t.b"}}}},
                    "b": {"type": "final"},
                }
            )
        )

    def test_bare_name_resolves_as_a_sibling(self):
        # "y" fired from a.x should resolve relative to x's parent, a. -- a sibling.
        self.assertEqual(self.g.resolve_target(("a", "x"), "y"), ("a", "y"))

    def test_hash_id_form_resolves_absolutely_from_the_graph_root(self):
        self.assertEqual(self.g.resolve_target(("a", "y"), "#t.b"), ("b",))

    def test_unresolvable_target_form_is_lint_error_not_a_runtime_surprise(self):
        # Deep dotted relative targets ("a.b.c") are not one of the two
        # supported forms -- deliberately narrower than full XState; see
        # graph.py's Graph.resolve_target docstring.
        with self.assertRaises(GraphError) as ctx:
            load_graph(_minimal(states={"a": {"on": {"GO": "a.b.c"}}, "b": {"type": "final"}}))
        self.assertTrue(any("a.b.c" in e or "unresolved" in e.lower() for e in ctx.exception.errors))


class NegativeLintTests(unittest.TestCase):
    """One representative break per rule. Each asserts GraphError is
    raised and that at least one collected error message names the actual
    problem -- not just that loading failed for some reason or other."""

    def test_parallel_type_is_rejected(self):
        with self.assertRaises(GraphError) as ctx:
            load_graph(_minimal(states={"a": {"type": "parallel", "states": {}}, "b": {"type": "final"}}))
        self.assertTrue(any("parallel" in e.lower() for e in ctx.exception.errors))

    def test_unknown_guard_type_is_rejected(self):
        g = _minimal(
            states={
                "a": {"always": {"guard": {"type": "vibes_check", "params": {}}, "target": "b"}},
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("vibes_check" in e for e in ctx.exception.errors))

    def test_unknown_action_type_is_rejected(self):
        g = _minimal(states={"a": {"entry": [{"type": "teleport", "params": {}}], "on": {"GO": "b"}}, "b": {"type": "final"}})
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("teleport" in e for e in ctx.exception.errors))

    def test_a_non_transitions_choose_needs_a_pick_handler(self):
        g = _minimal(
            states={
                "a": {"meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}}}, "on": {"OTHER": "b"}},
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("PICK" in e for e in ctx.exception.errors))

    def test_a_transitions_source_needs_at_least_two_non_reserved_events(self):
        g = _minimal(states={"a": {"meta": {"choose": {"from": "transitions"}}, "on": {"GO": "b"}}, "b": {"type": "final"}})
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("transitions" in e.lower() for e in ctx.exception.errors))

    def test_a_history_state_cannot_sit_at_top_level(self):
        g = _minimal(states={"a": {"on": {"GO": "b"}}, "b": {"type": "final"}, "h": {"type": "history", "target": "a"}})
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("history" in e.lower() for e in ctx.exception.errors))

    def test_an_unresolved_transition_target_is_a_lint_error(self):
        g = _minimal(states={"a": {"on": {"GO": "nowhere"}}, "b": {"type": "final"}})
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("nowhere" in e for e in ctx.exception.errors))

    def test_a_menu_label_over_the_option_token_budget_is_a_lint_error(self):
        long_label = "x" * 200
        g = _minimal(
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": long_label}, {"label": "y"}]}}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g, option_tokens=32)
        self.assertTrue(any("32" in e or "byte" in e.lower() for e in ctx.exception.errors))

    def test_shape_validation_collects_every_problem_not_just_the_first(self):
        # Two independent, unrelated shape breaks in one graph -- both must
        # be reported in one GraphError, not just the first one found (see
        # graph.py's _validate_shape: "collects ALL errors, not fail-fast").
        g = _minimal(
            states={
                "a": {"entry": [{"type": "nonsense", "params": {}}], "on": {"GO": "b"}},
                "b": {"always": {"guard": {"type": "make-believe", "params": {}}, "target": "a"}, "type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        errors = ctx.exception.errors
        self.assertGreaterEqual(len(errors), 2, errors)
        self.assertTrue(any("nonsense" in e for e in errors))
        self.assertTrue(any("make-believe" in e for e in errors))

    def test_lint_collects_every_problem_not_just_the_first(self):
        # Two independent unresolved targets -- a shape-valid graph, so
        # this exercises _lint's own accumulation, not _validate_shape's.
        g = _minimal(
            states={
                "a": {"on": {"GO": "nowhere-1"}},
                "b": {"on": {"BACK": "nowhere-2"}},
                "c": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        errors = ctx.exception.errors
        self.assertGreaterEqual(len(errors), 2, errors)
        self.assertTrue(any("nowhere-1" in e for e in errors))
        self.assertTrue(any("nowhere-2" in e for e in errors))

    def test_an_initial_that_does_not_name_a_real_child_is_a_lint_error(self):
        g = _minimal(initial="ghost")
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("ghost" in e for e in ctx.exception.errors))

    def test_an_expect_that_is_not_a_guard_is_rejected_naming_the_action(self):
        # docs/design/observation.md section 4: `expect` holds a guard from
        # the same closed grammar every other guard site uses -- a bare
        # string is not one.
        g = _minimal(
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "bash", "expect": "not a guard"}}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("expect" in e for e in ctx.exception.errors))

    def test_an_expect_with_an_unknown_guard_type_is_rejected(self):
        g = _minimal(
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {
                        "name": "bash", "expect": {"type": "vibes_check", "params": {}},
                    }}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("vibes_check" in e for e in ctx.exception.errors))

    # -- docs/design/unattended.md section 2: meta.jev.warrant's own lint --

    def test_a_warrant_whose_tools_do_not_match_the_actions_is_rejected_naming_both_sets(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["browser_open"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "bash", "input": {"command": "true"}}}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        errors = ctx.exception.errors
        self.assertTrue(any("bash" in e and "does not name" in e for e in errors))
        self.assertTrue(any("browser_open" in e and "no action dispatches" in e for e in errors))

    def test_a_browser_open_without_confine_under_a_warrant_is_rejected(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["browser_open"], "origins": ["https://example.com"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {
                        "name": "browser_open", "input": {"url": "https://example.com/x"},
                    }}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("confine" in e and "must equal meta.jev.warrant.origins" in e for e in ctx.exception.errors))

    def test_a_confine_that_differs_from_origins_is_rejected(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["browser_open"], "origins": ["https://example.com"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {
                        "name": "browser_open",
                        "input": {"url": "https://example.com/x", "confine": ["https://other.example"]},
                    }}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("confine" in e and "must equal meta.jev.warrant.origins" in e for e in ctx.exception.errors))

    def test_a_templated_origin_is_rejected(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["browser_open"], "origins": ["https://example.com"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {
                        "name": "browser_open",
                        "input": {"url": "{{context.base}}/x", "confine": ["https://example.com"]},
                    }}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("url" in e and "the origin must be literal under a warrant" in e for e in ctx.exception.errors))

    def test_a_start_url_outside_origins_is_rejected(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["browser_open"], "origins": ["https://example.com"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {
                        "name": "browser_open",
                        "input": {"url": "https://not-example.com/x", "confine": ["https://example.com"]},
                    }}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("not-example.com" in e and "not in meta.jev.warrant.origins" in e for e in ctx.exception.errors))

    def test_a_click_warrant_without_origins_is_rejected(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["browser_click"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "browser_click", "input": {"ref": "e1"}}}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any(
            "browser_click or browser_type must name origins" in e for e in ctx.exception.errors
        ))

    def test_a_warrant_command_the_graph_cannot_dispatch_is_rejected(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["bash"], "commands": ["rm -rf /"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "bash", "input": {"command": "ls"}}}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any(
            "rm -rf /" in e and "is not a command this graph can dispatch" in e for e in ctx.exception.errors
        ))

    def test_a_refs_url_that_is_not_a_regex_is_rejected(self):
        g = _minimal(
            states={
                "a": {
                    "meta": {"choose": {"from": {"refs": {"roles": ["link"], "url": "("}}}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any(
            "refs.url" in e and "not a valid regular expression" in e for e in ctx.exception.errors
        ))

    def test_a_non_ascii_command_is_rejected(self):
        g = _minimal(
            meta={"jev": {"schema": 1, "warrant": {"tools": ["bash"], "commands": ["echo café"]}}},
            states={
                "a": {
                    "entry": [{"type": "tool", "params": {"name": "bash", "input": {"command": "echo café"}}}],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            },
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any(
            "commands" in e and "printable ASCII" in e for e in ctx.exception.errors
        ))


class AlwaysSpinLintTests(unittest.TestCase):
    """An `always` entry that is unconditional (no guard), does no `tool`
    action, and cannot leave the state (no `target`, or a self-`target`
    with `reenter` unset) re-runs and counts nothing -- section 1's
    re-entry rule -- so it can only spin until `wall_s`. Rejected at load
    time; a guarded self-transition, a `reenter: true` self-transition, a
    self-transition that does a `tool` action, and an unconditional
    transition to a genuinely different state must all still load."""

    def test_an_internal_transition_with_no_target_and_no_actions_is_rejected(self):
        g = _minimal(states={"a": {"always": {}, "on": {"GO": "b"}}, "b": {"type": "final"}})
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("spin" in e.lower() for e in ctx.exception.errors))

    def test_an_internal_transition_with_only_a_non_tool_action_is_still_rejected(self):
        # "no tool action" is the exact test -- an assign/push/inc action
        # is not enough to make this legitimate, since nothing ever checks
        # what it changed (there is no guard here to check it).
        g = _minimal(
            states={
                "a": {"always": {"actions": [{"type": "assign", "params": {"x": 1}}]}, "on": {"GO": "b"}},
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("spin" in e.lower() for e in ctx.exception.errors))

    def test_an_unconditional_self_transition_without_reenter_is_rejected(self):
        g = _minimal(states={"a": {"always": {"target": "a"}, "on": {"GO": "b"}}, "b": {"type": "final"}})
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("spin" in e.lower() for e in ctx.exception.errors))

    def test_the_second_entry_of_an_always_list_is_named_by_index(self):
        g = _minimal(
            states={
                "a": {
                    "always": [
                        {"guard": {"type": "exists", "params": {"path": "context.x"}}, "target": "b"},
                        {"target": "a"},
                    ],
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(any("always[1]" in e for e in ctx.exception.errors))

    def test_a_guarded_self_transition_still_loads(self):
        g = _minimal(
            states={
                "a": {
                    "always": {"guard": {"type": "exists", "params": {"path": "context.x"}}, "target": "a"},
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            }
        )
        graph = load_graph(g)
        self.assertIn(("a",), graph.paths)

    def test_a_reentering_self_transition_still_loads(self):
        g = _minimal(states={"a": {"always": {"target": "a", "reenter": True}, "on": {"GO": "b"}}, "b": {"type": "final"}})
        graph = load_graph(g)
        self.assertIn(("a",), graph.paths)

    def test_a_self_transition_that_does_a_tool_action_still_loads(self):
        g = _minimal(
            states={
                "a": {
                    "always": {"target": "a", "actions": [{"type": "tool", "params": {"name": "bash"}}]},
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            }
        )
        graph = load_graph(g)
        self.assertIn(("a",), graph.paths)

    def test_an_unconditional_transition_to_a_different_state_still_loads(self):
        g = _minimal(states={"a": {"always": "b"}, "b": {"type": "final"}})
        graph = load_graph(g)
        self.assertIn(("a",), graph.paths)


class PreferSchemaTests(unittest.TestCase):
    """docs/design/judgement.md section 2.2, `choose.prefer`: a non-empty
    array of guards from the same closed grammar, the one site in the
    whole schema where an `option.*` path is admitted."""

    def test_a_prefer_rule_may_read_option_paths(self):
        g = _minimal(
            states={
                "a": {
                    "meta": {"choose": {
                        "from": {"menu": [{"label": "x"}, {"label": "y"}]},
                        "prefer": [{"type": "equals", "params": {"path": "option.label", "value": "y"}}],
                    }},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            }
        )
        graph = load_graph(g)
        self.assertIn(("a",), graph.paths)

    def test_an_option_path_outside_prefer_is_rejected_naming_the_site(self):
        # the identical guard, verbatim, moved from choose.prefer to an
        # ordinary `always` guard -- `option` is not just an unknown root
        # there, it is refused BY NAME, naming exactly where it is valid.
        g = _minimal(
            states={
                "a": {
                    "always": {
                        "guard": {"type": "equals", "params": {"path": "option.label", "value": "y"}},
                        "target": "b",
                    },
                    "on": {"GO": "b"},
                },
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(
            any("option" in e and "only in scope inside choose.prefer" in e for e in ctx.exception.errors),
            ctx.exception.errors,
        )

    def test_a_prefer_that_is_not_a_guard_list_is_rejected(self):
        g = _minimal(
            states={
                "a": {
                    "meta": {"choose": {
                        "from": {"menu": [{"label": "x"}, {"label": "y"}]},
                        "prefer": {"type": "equals", "params": {"path": "option.label", "value": "y"}},
                    }},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(
            any("prefer" in e and "non-empty array" in e for e in ctx.exception.errors), ctx.exception.errors
        )

    def test_an_empty_prefer_list_is_also_rejected(self):
        # a bare object is one way to fail "non-empty array of guards";
        # an empty list is the other -- both must be caught, not just
        # whichever a caller happens to try first.
        g = _minimal(
            states={
                "a": {
                    "meta": {"choose": {"from": {"menu": [{"label": "x"}, {"label": "y"}]}, "prefer": []}},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            }
        )
        with self.assertRaises(GraphError) as ctx:
            load_graph(g)
        self.assertTrue(
            any("prefer" in e and "non-empty array" in e for e in ctx.exception.errors), ctx.exception.errors
        )

    def test_an_nli_prefer_loads_and_is_named_in_the_graphs_warning_list(self):
        # not an error -- the closed grammar admits an NLI leaf inside
        # `prefer` -- but `graph.py`'s own lint prices it once, at load
        # time, onto `Graph.prefer_nli_states`, which `run.py`'s `start()`
        # reads to warn a fresh run's operator by name (one entail round
        # trip per option per visit, judgement.md section 2.2's own cost
        # note -- never enforced structurally, since the grammar cannot
        # forbid an NLI leaf here without narrowing the closed set itself).
        g = _minimal(
            states={
                "a": {
                    "meta": {"choose": {
                        "from": {"menu": [{"label": "x"}, {"label": "y"}]},
                        "prefer": [{"type": "entails", "params": {"hypothesis": "this is the goal"}}],
                    }},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            }
        )
        graph = load_graph(g)
        self.assertEqual(graph.prefer_nli_states, ["states.a"])

    def test_a_prefer_rule_with_no_nli_leaf_does_not_join_the_warning_list(self):
        g = _minimal(
            states={
                "a": {
                    "meta": {"choose": {
                        "from": {"menu": [{"label": "x"}, {"label": "y"}]},
                        "prefer": [{"type": "equals", "params": {"path": "option.label", "value": "y"}}],
                    }},
                    "on": {"PICK": "b"},
                },
                "b": {"type": "final"},
            }
        )
        graph = load_graph(g)
        self.assertEqual(graph.prefer_nli_states, [])

    def test_the_shipped_wiki_hop_prefer_rule_is_deterministic_and_off_the_warning_list(self):
        # the real, shipped rule (`option.label == {{context.goal}}`,
        # folded) is exactly the equals case above -- confirmed here
        # against the actual graph file, not only a synthetic stand-in.
        graph = load_graph(GRAPHS_DIR / "wiki-hop.json")
        self.assertEqual(graph.prefer_nli_states, [])
        choose = graph.states["reading"]["meta"]["choose"]
        self.assertEqual(
            choose["prefer"],
            [{"type": "equals", "params": {"path": "option.label", "value": "{{context.goal}}", "fold": True}}],
        )


if __name__ == "__main__":
    unittest.main()

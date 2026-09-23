"""Tests for automation/template.py -- the `{{path}}` renderer, docs/design/
automation.md section 1's "Templates": no logic, dotted paths against
context/input/event/run(/state/option), list -> newline-join, object ->
JSON, missing -> empty string with a warning.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from automation import template  # noqa: E402


class ResolvePathTests(unittest.TestCase):
    def test_walks_a_dotted_path_through_nested_dicts(self):
        scope = {"context": {"obs": {"h1": "Cat"}}}
        self.assertEqual(template.resolve_path("context.obs.h1", scope), "Cat")

    def test_walks_a_numeric_segment_into_a_list(self):
        scope = {"context": {"path": ["Cat", "Felis"]}}
        self.assertEqual(template.resolve_path("context.path.0", scope), "Cat")
        self.assertEqual(template.resolve_path("context.path.-1", scope), "Felis")

    def test_a_missing_segment_is_the_missing_sentinel_not_none(self):
        scope = {"context": {}}
        self.assertIs(template.resolve_path("context.nope", scope), template.MISSING)

    def test_a_resolved_null_is_none_not_missing(self):
        scope = {"context": {"x": None}}
        self.assertIsNone(template.resolve_path("context.x", scope))
        self.assertIsNot(template.resolve_path("context.x", scope), template.MISSING)

    def test_an_out_of_range_index_is_missing(self):
        scope = {"context": {"path": ["Cat"]}}
        self.assertIs(template.resolve_path("context.path.5", scope), template.MISSING)


class StringifyTests(unittest.TestCase):
    def test_a_list_is_newline_joined(self):
        self.assertEqual(template.stringify(["a", "b", "c"]), "a\nb\nc")

    def test_an_object_is_json(self):
        self.assertEqual(template.stringify({"b": 1, "a": 2}), '{"a": 2, "b": 1}')  # sort_keys

    def test_missing_and_none_both_render_empty(self):
        self.assertEqual(template.stringify(template.MISSING), "")
        self.assertEqual(template.stringify(None), "")

    def test_booleans_are_lowercase_json_words_not_python_repr(self):
        self.assertEqual(template.stringify(True), "true")
        self.assertEqual(template.stringify(False), "false")

    def test_a_nested_list_stringifies_each_item_recursively(self):
        self.assertEqual(template.stringify([{"a": 1}, "x"]), '{"a": 1}\nx')


class RenderTests(unittest.TestCase):
    def test_substitutes_one_placeholder(self):
        self.assertEqual(template.render("hi {{context.name}}", {"context": {"name": "Cat"}}), "hi Cat")

    def test_substitutes_several_placeholders(self):
        scope = {"context": {"a": "1", "b": "2"}}
        self.assertEqual(template.render("{{context.a}}-{{context.b}}", scope), "1-2")

    def test_a_template_with_no_placeholder_is_unchanged(self):
        self.assertEqual(template.render("plain text", {}), "plain text")

    def test_a_list_embedded_in_a_larger_string_is_newline_joined(self):
        scope = {"context": {"visited": ["Cat", "Felis"]}}
        self.assertEqual(template.render("seen:\n{{context.visited}}", scope), "seen:\nCat\nFelis")

    def test_a_missing_path_renders_empty_and_warns(self):
        warnings: list[str] = []
        result = template.render("[{{context.nope}}]", {"context": {}}, warnings)
        self.assertEqual(result, "[]")
        self.assertEqual(len(warnings), 1)
        self.assertIn("context.nope", warnings[0])

    def test_no_warnings_list_means_no_crash_on_a_missing_path(self):
        self.assertEqual(template.render("[{{context.nope}}]", {"context": {}}, None), "[]")


class RenderValueTests(unittest.TestCase):
    def test_a_whole_placeholder_preserves_the_real_type(self):
        scope = {"context": {"visited": ["Cat", "Felis"]}}
        self.assertEqual(template.render_value("{{context.visited}}", scope), ["Cat", "Felis"])

    def test_whitespace_around_a_whole_placeholder_still_counts_as_whole(self):
        scope = {"context": {"n": 5}}
        self.assertEqual(template.render_value("  {{context.n}}  ", scope), 5)

    def test_a_template_with_surrounding_text_falls_back_to_stringified_render(self):
        scope = {"context": {"n": 5}}
        self.assertEqual(template.render_value("n={{context.n}}", scope), "n=5")

    def test_two_placeholders_in_one_template_is_not_the_whole_form(self):
        # Each placeholder stringifies independently -- no separator is
        # inserted between them beyond whatever literal text the template
        # itself has there (none, in the first case).
        scope = {"context": {"a": [1, 2], "b": [3]}}
        self.assertEqual(template.render_value("{{context.a}}{{context.b}}", scope), "1\n23")
        self.assertEqual(template.render_value("{{context.a}}|{{context.b}}", scope), "1\n2|3")

    def test_a_missing_whole_placeholder_is_none_not_empty_string(self):
        warnings: list[str] = []
        self.assertIsNone(template.render_value("{{context.nope}}", {"context": {}}, warnings))
        self.assertEqual(len(warnings), 1)

    def test_a_plain_string_with_no_placeholder_passes_through(self):
        self.assertEqual(template.render_value("just text", {}), "just text")


if __name__ == "__main__":
    unittest.main()

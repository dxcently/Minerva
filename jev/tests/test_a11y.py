"""Tests for automation/a11y.py -- parsing extensions/browser/service.py's
`_head` convention and Playwright's `aria_snapshot(mode="ai")` tree, per
docs/design/automation.md section 2, "A Playwright accessibility snapshot".
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from automation import a11y  # noqa: E402

FIXTURES_DIR = Path(__file__).resolve().parent / "fixtures" / "wiki"


class LiftHeadTests(unittest.TestCase):
    def test_splits_url_and_title_from_the_rest(self):
        raw = "url: https://example.com\ntitle: Example\n\nbody text here"
        url, title, rest = a11y.lift_head(raw)
        self.assertEqual(url, "https://example.com")
        self.assertEqual(title, "Example")
        self.assertEqual(rest, "body text here")

    def test_text_that_does_not_follow_the_convention_passes_through_unchanged(self):
        raw = "$ ls -la\ntotal 12\ndrwxr-xr-x ...\n"
        url, title, rest = a11y.lift_head(raw)
        self.assertIsNone(url)
        self.assertIsNone(title)
        self.assertEqual(rest, raw)

    def test_an_empty_title_or_url_is_still_lifted(self):
        raw = "url: \ntitle: \n\nbody"
        url, title, rest = a11y.lift_head(raw)
        self.assertEqual(url, "")
        self.assertEqual(title, "")
        self.assertEqual(rest, "body")


class ParseRefsTests(unittest.TestCase):
    def test_a_flat_list_of_elements(self):
        tree = '- link "Cat" [ref=e1]\n- link "Dog" [ref=e2]\n'
        records, skipped = a11y.parse_refs(tree)
        self.assertEqual(skipped, 0)
        self.assertEqual([r["name"] for r in records], ["Cat", "Dog"])
        self.assertEqual([r["ref"] for r in records], ["e1", "e2"])
        self.assertTrue(all(r["role"] == "link" for r in records))

    def test_nesting_is_read_from_indentation_not_a_fixed_width(self):
        tree = '- main:\n   - heading "Title" [level=1]\n   - link "A" [ref=e1]\n'
        records, _ = a11y.parse_refs(tree)
        self.assertEqual(records[0]["role"], "main")
        self.assertEqual(records[1]["role"], "heading")
        self.assertEqual(records[1]["level"], 1)

    def test_landmark_propagates_to_descendants_but_not_outside_it(self):
        tree = (
            '- navigation:\n'
            '  - link "Home" [ref=e1]\n'
            '- main:\n'
            '  - link "Article" [ref=e2]\n'
            '- link "Footer link" [ref=e3]\n'
        )
        records, _ = a11y.parse_refs(tree)
        by_name = {r["name"]: r for r in records}
        self.assertEqual(by_name["Home"]["landmark"], "navigation")
        self.assertEqual(by_name["Article"]["landmark"], "main")
        self.assertIsNone(by_name["Footer link"]["landmark"])

    def test_a_property_line_attaches_url_to_its_parent_element(self):
        tree = '- link "Cat" [ref=e1]\n  - /url: https://example.com/Cat\n'
        records, skipped = a11y.parse_refs(tree)
        self.assertEqual(skipped, 0)
        self.assertEqual(records[0]["url"], "https://example.com/Cat")

    def test_a_dash_prefixed_line_that_does_not_parse_as_an_element_is_counted_skipped(self):
        tree = '- [[[not a valid element\n'
        records, skipped = a11y.parse_refs(tree)
        self.assertEqual(records, [])
        self.assertEqual(skipped, 1)

    def test_a_line_that_is_not_tree_syntax_at_all_is_neither_a_record_nor_skipped(self):
        # Plain bash output must never produce bogus skip counts.
        tree = "total 12\ndrwxr-xr-x  2 root root 4096 Jan  1 00:00 .\n"
        records, skipped = a11y.parse_refs(tree)
        self.assertEqual(records, [])
        self.assertEqual(skipped, 0)

    def test_blank_lines_are_ignored(self):
        tree = '- link "A" [ref=e1]\n\n\n- link "B" [ref=e2]\n'
        records, skipped = a11y.parse_refs(tree)
        self.assertEqual(len(records), 2)
        self.assertEqual(skipped, 0)

    def test_an_element_with_no_ref_still_parses_just_not_clickable(self):
        tree = '- paragraph: some text here\n'
        records, _ = a11y.parse_refs(tree)
        self.assertEqual(records[0]["role"], "paragraph")
        self.assertIsNone(records[0]["ref"])
        self.assertEqual(records[0]["text"], "some text here")


class BuildObsTests(unittest.TestCase):
    def test_a_real_snapshot_gets_url_title_h1_and_stripped_text(self):
        raw = (
            "url: https://en.wikipedia.org/wiki/Cat\n"
            "title: Cat - Wikipedia\n"
            "\n"
            '- main:\n'
            '  - heading "Cat" [level=1]\n'
            '  - link "Felis" [ref=e1]\n'
        )
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["url"], "https://en.wikipedia.org/wiki/Cat")
        self.assertEqual(obs["title"], "Cat - Wikipedia")
        self.assertEqual(obs["h1"], "Cat")
        self.assertNotIn("url:", obs["text"])  # head lines stripped out of the body

    def test_a_bash_result_gets_none_for_the_snapshot_only_fields(self):
        raw = "total 12\ndrwxr-xr-x 2 root root 4096 file\n"
        obs = a11y.build_obs(raw)
        self.assertIsNone(obs["url"])
        self.assertIsNone(obs["title"])
        self.assertIsNone(obs["h1"])
        self.assertEqual(obs["text"], raw)

    def test_refs_are_not_eagerly_attached(self):
        raw = "url: https://example.com\ntitle: Example\n\n- link \"A\" [ref=e1]\n"
        obs = a11y.build_obs(raw)
        self.assertNotIn("refs", obs)

    def test_a_snapshot_with_no_level_1_heading_has_a_none_h1(self):
        raw = "url: https://example.com\ntitle: Example\n\n- link \"A\" [ref=e1]\n"
        obs = a11y.build_obs(raw)
        self.assertIsNone(obs["h1"])

    def test_a_heading_head_line_is_used_even_when_the_body_has_no_heading_role_at_all(self):
        # extensions/browser's `_head`, `heading=` -- the fix for the real
        # defect this module's own docstring now names: once `roles`
        # excludes "heading" from the body (wiki-hop.json's own
        # `roles: ["link"]`), the scan below alone can never find one
        # again, unconditionally, on every hop. A producer that already
        # read the heading off its own *unfiltered* tree says so directly
        # in the head block; this is that line, preferred over a body that
        # -- exactly as it would after a real `roles: ["link"]` snapshot --
        # carries no heading role whatsoever.
        raw = (
            "url: https://en.wikipedia.org/wiki/Cat\n"
            "title: Cat - Wikipedia\n"
            "heading: Cat\n"
            "roles: link\n"
            "\n"
            '- link "Felis" [ref=e1]\n'
        )
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["h1"], "Cat")

    def test_a_heading_head_line_takes_precedence_over_a_different_heading_in_the_body(self):
        # Not merely "used when the scan would otherwise fail" -- preferred
        # outright, so a producer's own answer is never second-guessed by
        # a heuristic scan of a body it already read to produce that
        # answer.
        raw = (
            "url: https://example.com\n"
            "title: Example\n"
            "heading: Real Answer\n"
            "\n"
            '- heading "Stale Or Unrelated" [level=1] [ref=e1]\n'
        )
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["h1"], "Real Answer")

    def test_an_older_producer_with_no_heading_head_line_still_falls_back_to_the_body_scan(self):
        # Backward compatibility with a producer that predates `heading:`
        # entirely (docs/Decisions.md): this is
        # test_a_real_snapshot_gets_url_title_h1_and_stripped_text's own
        # raw text again, with no `heading:` line at all, still expected
        # to resolve `h1` exactly as it always did.
        raw = (
            "url: https://en.wikipedia.org/wiki/Cat\n"
            "title: Cat - Wikipedia\n"
            "\n"
            '- main:\n'
            '  - heading "Cat" [level=1]\n'
            '  - link "Felis" [ref=e1]\n'
        )
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["h1"], "Cat")

    def test_an_empty_heading_head_line_falls_back_to_the_body_scan_not_an_empty_string(self):
        raw = (
            "url: https://example.com\n"
            "title: Example\n"
            "heading: \n"
            "\n"
            '- heading "Real" [level=1] [ref=e1]\n'
        )
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["h1"], "Real")


class QuotedLineTests(unittest.TestCase):
    """Playwright wraps a whole `role "name" [attrs]:` head in single
    quotes when the accessible name contains a literal colon or an
    embedded double quote -- ambiguous otherwise with this grammar's own
    `role "name" [attrs]: text` delimiter -- e.g.
    `- 'generic "Page / location: 11" [ref=e1101]': ": 11"`. Confirmed
    against a real Wikipedia "Cat" capture: 58 of ~11,000 lines took this
    shape and all 58 used to be silently dropped (`_ELEMENT_RE` anchors
    `^[A-Za-z]` and has no case for a leading `'`); see
    jev/tests/fixtures/wiki/cat_citations_excerpt.txt for the real excerpts
    these cases are drawn from, verbatim."""

    def test_inline_quoted_text_value_with_a_colon_in_the_name(self):
        line = '- \'generic "Page / location: 11" [ref=e1101]\': ": 11"\n'
        records, skipped = a11y.parse_refs(line)
        self.assertEqual(skipped, 0)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["role"], "generic")
        self.assertEqual(records[0]["name"], "Page / location: 11")
        self.assertEqual(records[0]["ref"], "e1101")
        self.assertEqual(records[0]["text"], ": 11")  # the value's own wrapping quotes stripped

    def test_children_follow_form_with_an_embedded_escaped_double_quote(self):
        # nothing after the closing colon -- the element's content is a
        # nested /url: property line, not inline text -- and the name
        # itself is double-quoted text on the page ("Qitta: Arabic Cats"),
        # which is why it carries Playwright's own backslash-escaped `\"`
        # *inside* the outer single-quote wrapper.
        tree = (
            '- \'link "\\"Qitta: Arabic Cats\\"" [ref=e2718] [cursor=pointer]\':\n'
            '  - /url: https://books.google.com/books?id=n1_qqgNTsX8C&pg=PA407\n'
        )
        records, skipped = a11y.parse_refs(tree)
        self.assertEqual(skipped, 0)
        self.assertEqual(len(records), 1)
        self.assertEqual(records[0]["role"], "link")
        self.assertEqual(records[0]["ref"], "e2718")
        self.assertIn("Qitta: Arabic Cats", records[0]["name"])
        self.assertEqual(records[0]["url"], "https://books.google.com/books?id=n1_qqgNTsX8C&pg=PA407")

    def test_a_literal_apostrophe_is_unescaped_from_the_doubled_single_quote(self):
        # "Ship''s Cat" on the wire -> "Ship's Cat" as the real accessible
        # name -- YAML's own single-quote escape (doubling), confirmed
        # against the real capture, not assumed.
        tree = (
            "- 'link \"Toolbox: Ship''s Cat on the Kalmar Nyckel\" [ref=e5081] [cursor=pointer]':\n"
            "  - /url: https://books.google.com/books?id=q3LvHwAACAAJ\n"
        )
        records, skipped = a11y.parse_refs(tree)
        self.assertEqual(skipped, 0)
        self.assertEqual(records[0]["name"], "Toolbox: Ship's Cat on the Kalmar Nyckel")
        self.assertEqual(records[0]["url"], "https://books.google.com/books?id=q3LvHwAACAAJ")

    def test_cursor_pointer_alongside_ref_still_parses_on_a_quoted_line(self):
        records, skipped = a11y.parse_refs('- \'link "a: b" [ref=e1] [cursor=pointer]\': \n')
        self.assertEqual(skipped, 0)
        self.assertEqual(records[0]["ref"], "e1")

    def test_a_genuinely_unterminated_quoted_line_still_counts_as_skipped(self):
        # Proves the fix recognises one more real shape rather than
        # disabling skip-detection altogether: a quote that never closes
        # matches neither the quoted-line form nor a bare element.
        records, skipped = a11y.parse_refs("- 'this never closes\n")
        self.assertEqual(records, [])
        self.assertEqual(skipped, 1)


class IsTruncatedTests(unittest.TestCase):
    """docs/design/observation.md section 4: a result is truncated when its
    body's length disagrees with what its own producer declared in the
    head's `chars:` line -- not when one specific transport's marker text
    is found at the tail (the old mechanism this replaces: a single-layer
    marker match that only ever saw `crates/tools`'s own clip and missed
    every other place a result could be shortened between the producer and
    here -- docs/Decisions.md, "A tool result is bounded by who reads it,
    not by how big it is"). The seven old marker tests are gone with it;
    nothing here names `crates/tools` or any other specific layer any
    more."""

    def test_a_stamped_body_shorter_than_declared_is_truncated(self):
        raw = "url: https://x\ntitle: T\nchars: 100\n\nshort body"
        self.assertTrue(a11y.is_truncated(raw))

    def test_a_stamped_body_longer_than_declared_is_truncated(self):
        raw = "url: https://x\ntitle: T\nchars: 3\n\nthis body is much longer than three characters"
        self.assertTrue(a11y.is_truncated(raw))

    def test_a_stamped_body_of_the_declared_length_is_whole(self):
        body = "exactly this many characters"
        raw = f"url: https://x\ntitle: T\nchars: {len(body)}\n\n{body}"
        self.assertFalse(a11y.is_truncated(raw))

    def test_an_unstamped_body_is_not_reported_truncated(self):
        # No `chars:` head line at all -- unverifiable, not whole; run.py's
        # `_run_tool_action` stores this without a verdict rather than
        # refusing it (an ordinary `bash` result, for instance).
        raw = "url: https://x\ntitle: T\n\nno length was declared for this body"
        self.assertFalse(a11y.is_truncated(raw))


class HeadBlockTests(unittest.TestCase):
    """docs/design/observation.md, "Where an observation is narrowed": a
    head block is `url:` then `title:`, then zero or more further
    `key: value` lines (`scope:`, `chars:` today; any a later producer
    adds), then a blank line. `parse_head` reads it generically; these pin
    the extra-line handling `LiftHeadTests`' three-field reading does not
    exercise on its own."""

    def test_scope_and_chars_are_lifted(self):
        raw = "url: https://x\ntitle: T\nscope: main\nchars: 4\n\nbody"
        head, rest = a11y.parse_head(raw)
        self.assertEqual(head["scope"], "main")
        self.assertEqual(head["chars"], "4")
        self.assertEqual(rest, "body")

    def test_unknown_head_keys_are_kept_and_ignored(self):
        raw = "url: https://x\ntitle: T\nfuture_key: some value\n\nbody"
        head, rest = a11y.parse_head(raw)
        self.assertEqual(head["future_key"], "some value")
        self.assertEqual(rest, "body")

    def test_the_two_line_head_still_lifts_with_no_extras(self):
        raw = "url: https://x\ntitle: T\n\nbody"
        head, rest = a11y.parse_head(raw)
        self.assertEqual(head, {"url": "https://x", "title": "T"})
        self.assertEqual(rest, "body")

    def test_section_is_lifted_when_present(self):
        # docs/design/judgement.md, step 5: `parse_head` already lifts any
        # `key: value` extra line generically (proven above, for `scope`
        # and for an arbitrary `future_key`) -- so a producer that stamps
        # `section: <heading>` needs no parser change to have it arrive.
        # What this pins is `build_obs`'s own new line, which is the one
        # thing that actually surfaces it onto `context.obs`: revert
        # `build_obs`'s `"section": head.get("section") if head else None`
        # and this is the test that fails, even though `parse_head` itself
        # never changed.
        raw = "url: https://x\ntitle: T\nscope: main\nsection: See also\n\nbody"
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["section"], "See also")
        self.assertEqual(obs["scope"], "main")  # the existing lift, unaffected

    def test_section_is_none_when_the_head_carries_no_section_line(self):
        # The other half of the same lift -- an unsectioned snapshot (or
        # one predating `section:` entirely) must not fabricate a value,
        # the same `None`-when-absent contract `scope`/`h1` already keep.
        raw = "url: https://x\ntitle: T\nscope: main\n\nbody"
        obs = a11y.build_obs(raw)
        self.assertIsNone(obs["section"])

    def test_section_is_none_when_there_is_no_head_block_at_all(self):
        obs = a11y.build_obs("just some bash output\nno head here\n")
        self.assertIsNone(obs["section"])


class RealWikipediaCaptureTests(unittest.TestCase):
    """docs/Build-Log.md, 2026-09-19 ("The flagship worked example could
    never have worked"): the fixtures the wiki-hop graph ran against had 8
    tree lines against a real page's ~11,000, zero `- /url:` lines against
    2,779, and none of the 58 quoted-line forms -- "a fixture that cannot
    exhibit the problem cannot witness the fix." These load an actual
    captured Wikipedia "Cat" article (a real browser extension review,
    2026-09-19) instead of a hand-written impression of one.

    `cat_full_capture.txt` is the complete, unmodified capture (826,491
    bytes, 10,991 lines) -- kept in full so the numbers below, and the
    smaller fixtures trimmed from it, both stay re-derivable. `cat_snapshot
    .txt` is real lines 1-452 of it verbatim (the head convention, the
    banner/navigation chrome, the main landmark, the h1, the infobox with a
    real "Felis" link, and the whole lead paragraph of inline links) --
    contiguous, so its landmark nesting is exactly what the real page's is,
    not reconstructed. `cat_citations_excerpt.txt` is three further real,
    independently-contiguous slices (documented with their own source line
    numbers in the file's own header) chosen because that is where the
    quoted-line forms actually occur on the page. `pageA_snapshot.txt` /
    `pageB_snapshot.txt` are real captures of the `f<N>e<M>` ref shape a
    second navigation produces (docs/Build-Log.md, "D1r")."""

    def test_the_full_real_capture_has_zero_skipped_lines_after_the_fix(self):
        raw = (FIXTURES_DIR / "cat_full_capture.txt").read_text(encoding="utf-8")
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["url"], "https://en.wikipedia.org/wiki/Cat")
        self.assertEqual(obs["title"], "Cat - Wikipedia")
        self.assertEqual(obs["h1"], "Cat")
        records, skipped = a11y.parse_refs(obs["text"])
        self.assertEqual(skipped, 0)  # was 58 before the quoted-line fix
        self.assertEqual(sum(1 for r in records if r.get("url")), 2779)

    def test_the_trimmed_snapshot_carries_a_real_property_linked_url(self):
        raw = (FIXTURES_DIR / "cat_snapshot.txt").read_text(encoding="utf-8")
        obs = a11y.build_obs(raw)
        self.assertEqual(obs["h1"], "Cat")
        records, skipped = a11y.parse_refs(obs["text"])
        self.assertEqual(skipped, 0)
        felis = [r for r in records if r["role"] == "link" and r["name"] == "Felis"]
        self.assertEqual(len(felis), 1)
        self.assertEqual(felis[0]["url"], "https://en.wikipedia.org/wiki/Felis")
        self.assertEqual(felis[0]["landmark"], "main")

    def test_the_citations_excerpt_no_longer_skips_any_of_its_quoted_lines(self):
        raw = (FIXTURES_DIR / "cat_citations_excerpt.txt").read_text(encoding="utf-8")
        records, skipped = a11y.parse_refs(raw)
        self.assertEqual(skipped, 0)
        names = {r["name"] for r in records if r.get("name")}
        self.assertIn("Page / location: 11", names)
        self.assertIn("Page / location: 16", names)
        self.assertTrue(any("Qitta: Arabic Cats" in n for n in names))
        self.assertIn("Toolbox: Ship's Cat on the Kalmar Nyckel", names)

    def test_the_post_second_navigation_ref_shape_parses_and_carries_its_url(self):
        # a11y.py's own parsing was never shape-specific about `ref=`'s
        # value (that bug was extensions/browser/service.py's REF_RE,
        # fixed separately -- docs/Build-Log.md, "D1r"); this pins it
        # against a real f<N>e<M> capture rather than only assuming it.
        raw = (FIXTURES_DIR / "pageB_snapshot.txt").read_text(encoding="utf-8")
        obs = a11y.build_obs(raw)
        records, skipped = a11y.parse_refs(obs["text"])
        self.assertEqual(skipped, 0)
        self.assertEqual({r["ref"] for r in records if r.get("ref")}, {"f3e2", "f3e3", "f3e4", "f3e5"})
        link = next(r for r in records if r["name"] == "B-link-one")
        self.assertEqual(link["url"], "/b1")

    def test_the_scoped_capture_is_whole_by_its_own_declaration(self):
        # cat_main_scoped.txt: a real `within: "main"` capture of the live
        # Cat article (jev/tests/fixtures/wiki/cat_main_scoped.txt's own
        # header), stamped with the new `scope:`/`chars:` head lines
        # extensions/browser/service.py's `_snapshot` now produces.
        raw = (FIXTURES_DIR / "cat_main_scoped.txt").read_text(encoding="utf-8")
        obs = a11y.build_obs(raw)
        self.assertFalse(a11y.is_truncated(raw))
        self.assertEqual(obs["scope"], "main")
        self.assertEqual(obs["chars"], len(obs["text"]))

    def test_every_link_in_the_scoped_capture_has_some_landmark_though_not_always_main(self):
        # CORRECTION: docs/design/observation.md's step-4 spec names this
        # test `every_link_in_the_scoped_capture_has_main_as_its_landmark`
        # and expects every link's own `landmark` field to read "main".
        # Measured against the real capture, that is false: only 97 of its
        # 2,741 `link`-role records read landmark=="main" directly. The
        # rest read "navigation" or "region" -- real Wikipedia nests
        # further landmarks (an infobox, a table of contents, a references
        # block) *inside* `<main>`, and a11y.py's own nearest-ancestor walk
        # (see `test_landmark_propagates_to_descendants_but_not_outside_it`
        # above, which already pins this for a synthetic tree) correctly
        # attributes a link inside one of those to the nearer landmark, not
        # the outer scope -- exactly the behaviour that makes
        # `within: "main"` alone insufficient to build a large option list
        # from real article links (see this step's Build-Log entry for the
        # full depth-sweep numbers). What *is* true, and is what this test
        # actually verifies: every link is under *some* landmark, never
        # `None` -- the whole tree is rooted at `main`, so nothing in it
        # escapes every landmark the way chrome outside `main` would on an
        # unscoped snapshot.
        raw = (FIXTURES_DIR / "cat_main_scoped.txt").read_text(encoding="utf-8")
        obs = a11y.build_obs(raw)
        records, _skipped = a11y.parse_refs(obs["text"])
        links = [r for r in records if r["role"] == "link"]
        self.assertTrue(links)
        self.assertTrue(all(r["landmark"] is not None for r in links))
        landmarks_seen = {r["landmark"] for r in links}
        self.assertIn("main", landmarks_seen)
        self.assertTrue(landmarks_seen - {"main"}, "expected at least one nested sub-landmark too, e.g. region")


if __name__ == "__main__":
    unittest.main()

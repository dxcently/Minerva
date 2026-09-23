"""Tests for automation/warrant.py -- docs/design/unattended.md section 2,
"The envelope," and the cross-language id contract with
`eidolon/crates/rune/src/warrant.rs` (a separate repo; read there, never
imported or ported here -- see warrant.py's own module docstring).

**The pin.** `eidolon/crates/rune/src/warrant.rs`'s own pinned test,
`the_shipped_wiki_hop_envelope_has_the_pinned_id`, hashes a *placeholder*
envelope -- its own `wiki_hop_envelope_json()` fixture's doc comment says
so directly: `sha256` there is `"a1b2c3d4"` repeated eight times, "not yet
the real one... not the shipped graph's actual document hash," because
step 5 (this file) had not landed yet when that comment was written.
`RUST_PLACEHOLDER_ENVELOPE` below is that exact fixture, transcribed by
hand from reading `warrant.rs` (not executed through any shared code), and
`test_the_rust_placeholder_envelope_hashes_to_the_rust_pinned_id` is the
one test in this file that touches it: independent proof that this
module's `canonical_json`/`warrant_id`, written fresh from the algorithm
docs/design/unattended.md section 2 names -- sorted keys, no whitespace,
UTF-8, not ASCII-escaped, then `"w_"` plus the first twelve hex of the
SHA-256 -- produce the exact same id Rust does, `w_5e885ef92416`, for the
exact same bytes in.

That is a different question from "what is the shipped wiki-hop graph's
warrant id," which is what `the_shipped_wiki_hop_*_is_the_pinned_literal`
below answer: the *real* document hash (computed once, here, by loading
the actual `extensions/jev/graphs/wiki-hop.json` through `graph.py`) is
necessarily a different 64 hex characters than the placeholder, so the
real block hashes to a different id, `w_378be6b7176d` -- not a mismatch
with the Rust side, a different envelope than the one Rust's still-a-
placeholder fixture describes. `eidolon/crates/` is a separate repo this
task does not touch; replacing its placeholder with this file's real
`sha256` (so both sides eventually pin the *same* shipped-graph id, as
warrant.rs's own comment asks whoever lands step 5 to do) is that repo's
own follow-up, not this one's.
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import automation.warrant as warrant_mod  # noqa: E402
from automation.graph import load_graph  # noqa: E402

REPO_ROOT = Path(__file__).resolve().parent.parent.parent
GRAPHS_DIR = REPO_ROOT / "extensions" / "jev" / "graphs"

# Transcribed by hand from eidolon/crates/rune/src/warrant.rs's
# `wiki_hop_envelope_json()` -- see this module's own docstring. Every
# value here must match that function's literal exactly, or this test is
# no longer testing what it claims to.
RUST_PLACEHOLDER_ENVELOPE = {
    "graph": "wiki-hop@1",
    "sha256": "a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4",
    "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
    "origins": ["https://en.wikipedia.org"],
    "commands": [],
    "actions": 70,
    "wall_s": 900,
}

# The real, shipped wiki-hop.json's own canonical block -- computed once
# (see this module's docstring) by loading the actual graph file and
# reading `Graph.sha256`/`warrant.canonical`, then pinned here as a
# literal so a future change to the graph or to this module's own hashing
# is caught by a diff against a fixed value, the same shape
# `eidolon/crates/rune/src/warrant.rs`'s own pinned test takes.
SHIPPED_WIKI_HOP_BLOCK = {
    "graph": "wiki-hop@1",
    "sha256": "384f87f86068d3841429c5f66f88f1cc4169d2b9b46f50f52023e165f7660f91",
    "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
    "origins": ["https://en.wikipedia.org"],
    "commands": [],
    "actions": 70,
    "wall_s": 900,
}
SHIPPED_WIKI_HOP_ID = "w_378be6b7176d"


class CrossLanguageIdTests(unittest.TestCase):
    """Independent computation, not a port: this module's `canonical_json`/
    `warrant_id` are written from docs/design/unattended.md section 2's own
    prose, never by reading and translating warrant.rs line by line (see
    warrant.py's module docstring). This test is the proof that doing so
    still landed on the same bytes."""

    def test_the_rust_placeholder_envelope_hashes_to_the_rust_pinned_id(self):
        cj = warrant_mod.canonical_json(RUST_PLACEHOLDER_ENVELOPE)
        self.assertEqual(
            cj,
            '{"actions":70,"commands":[],"graph":"wiki-hop@1","origins":["https://en.wikipedia.org"],'
            '"sha256":"a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4",'
            '"tools":["browser_back","browser_click","browser_open","browser_snapshot"],"wall_s":900}',
        )
        self.assertEqual(warrant_mod.warrant_id(RUST_PLACEHOLDER_ENVELOPE), "w_5e885ef92416")

    def test_key_order_in_the_source_dict_does_not_change_the_id(self):
        # Unlike Rust's `Envelope` (a `BTreeSet` per list field, sorted by
        # construction the moment it is parsed), this module's
        # `canonical_json` only sorts *object* keys -- `json.dumps` never
        # reorders a list's own elements. Re-ordering the top-level *keys*
        # of the input dict must still be a no-op (object-key order is
        # exactly what `sort_keys=True` normalises); re-ordering the
        # *lists themselves* is a separate claim, made and proven by
        # `warrant.check`'s own normalisation, not by `canonical_json`
        # alone -- see `WarrantCheckTests.
        # test_a_block_with_reordered_lists_is_equal_and_has_the_same_id`.
        reordered = {
            "wall_s": 900,
            "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
            "sha256": "a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4a1b2c3d4",
            "origins": ["https://en.wikipedia.org"],
            "graph": "wiki-hop@1",
            "commands": [],
            "actions": 70,
        }
        self.assertEqual(warrant_mod.warrant_id(reordered), warrant_mod.warrant_id(RUST_PLACEHOLDER_ENVELOPE))


class ShippedGraphPinTests(unittest.TestCase):
    def test_the_shipped_wiki_hop_block_is_the_pinned_literal(self):
        graph = load_graph(GRAPHS_DIR / "wiki-hop.json")
        self.assertEqual(warrant_mod.canonical(graph), SHIPPED_WIKI_HOP_BLOCK)

    def test_the_shipped_wiki_hop_id_is_the_pinned_literal(self):
        graph = load_graph(GRAPHS_DIR / "wiki-hop.json")
        block = warrant_mod.canonical(graph)
        self.assertEqual(warrant_mod.warrant_id(block), SHIPPED_WIKI_HOP_ID)

    def test_triage_linux_declares_its_nine_commands(self):
        graph = load_graph(GRAPHS_DIR / "triage-linux.json")
        block = warrant_mod.canonical(graph)
        self.assertEqual(
            block["commands"],
            sorted([
                "cat /etc/os-release",
                "uname -a",
                "hostname; uptime",
                "who",
                "ss -tulpn 2>/dev/null || netstat -tulpn",
                "ss -tnp state established",
                "ip -br addr",
                "nft list ruleset 2>/dev/null || iptables -S",
                "ps -eo comm,pid,user,pcpu,etime --sort=-pcpu | head -30",
            ]),
        )
        self.assertEqual(block["tools"], ["bash"])


class WarrantCheckTests(unittest.TestCase):
    def setUp(self):
        self.graph = load_graph(GRAPHS_DIR / "wiki-hop.json")

    def test_a_block_with_reordered_lists_is_equal_and_has_the_same_id(self):
        passed = {
            "graph": "wiki-hop@1",
            "sha256": SHIPPED_WIKI_HOP_BLOCK["sha256"],
            # deliberately reordered and duplicated -- `check`'s own
            # `_normalize` must sort and dedup before comparing, the same
            # as `canonical` does when it builds a block from the graph.
            "tools": ["browser_snapshot", "browser_open", "browser_back", "browser_click", "browser_click"],
            "origins": ["https://en.wikipedia.org"],
            "commands": [],
            "actions": 70,
            "wall_s": 900,
        }
        block, warrant_id = warrant_mod.check(passed, self.graph)
        self.assertEqual(block, SHIPPED_WIKI_HOP_BLOCK)
        self.assertEqual(warrant_id, SHIPPED_WIKI_HOP_ID)

    def test_a_mismatched_block_is_refused_with_the_canonical_block_in_the_message(self):
        passed = dict(SHIPPED_WIKI_HOP_BLOCK, actions=71)  # one field differs
        with self.assertRaises(ValueError) as ctx:
            warrant_mod.check(passed, self.graph)
        message = str(ctx.exception)
        self.assertIn("wiki-hop@1", message)
        self.assertIn(warrant_mod.canonical_json(SHIPPED_WIKI_HOP_BLOCK), message)

    def test_each_of_the_seven_fields_alone_causes_refusal(self):
        # Non-vacuous, one field at a time: seven blocks, each identical to
        # the graph's own declared block except for exactly one key, each
        # still well-formed (right type, right shape) so the refusal is
        # `check`'s own equality test firing, not a shape error upstream of
        # it. `graph`/`sha256` wrong is "the right envelope for a different
        # document"; `tools`/`origins`/`commands` wrong is "narrower or
        # different, still well-formed lists"; `actions`/`wall_s` wrong is
        # "a different, still-well-formed integer." Every one must refuse,
        # and the message must still be the graph's *own* canonical block,
        # not an echo of what was wrongly passed -- proving the comparison
        # really is against the graph's reading, not a partial per-field
        # check that only some of the seven keys actually reach.
        wrong = {
            "graph": "wiki-hop@2",
            "sha256": "0" * 64,
            "tools": ["browser_click", "browser_open", "browser_snapshot"],  # missing browser_back
            "origins": ["https://example.org"],
            "commands": ["echo hi"],
            "actions": 71,
            "wall_s": 901,
        }
        self.assertEqual(set(wrong), set(SHIPPED_WIKI_HOP_BLOCK), "test fixture must cover exactly the 7 keys")
        for field, bad_value in wrong.items():
            with self.subTest(field=field):
                passed = dict(SHIPPED_WIKI_HOP_BLOCK)
                passed[field] = bad_value
                self.assertNotEqual(
                    passed[field], SHIPPED_WIKI_HOP_BLOCK[field], f"fixture for {field!r} must actually differ"
                )
                with self.assertRaises(ValueError) as ctx:
                    warrant_mod.check(passed, self.graph)
                message = str(ctx.exception)
                self.assertIn("wiki-hop@1", message, f"field={field}")
                self.assertIn(warrant_mod.canonical_json(SHIPPED_WIKI_HOP_BLOCK), message, f"field={field}")
                # The echoed block is the graph's own, not the caller's --
                # for every field except the one under test here they are
                # byte-identical anyway, so this also re-proves the other
                # six were left alone by this one field's change.
                self.assertNotIn(warrant_mod.canonical_json(passed), message, f"field={field}")

    def test_an_undeclared_graph_refuses_any_block_naming_attended(self):
        undeclared = load_graph({
            "id": "no-warrant",
            "initial": "a",
            "meta": {"jev": {"schema": 1}},
            "states": {"a": {"on": {"GO": "b"}}, "b": {"type": "final"}},
        })
        with self.assertRaises(ValueError) as ctx:
            warrant_mod.check(SHIPPED_WIKI_HOP_BLOCK, undeclared)
        message = str(ctx.exception)
        self.assertIn("no-warrant", message)
        self.assertIn("attended", message)
        self.assertIn("warrant", message.lower())


if __name__ == "__main__":
    unittest.main()

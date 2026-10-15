# RT fix round 3 (orchestrator's decisions on the round-2 verifier's last gaps)

RT is at 8065661 (0717fde, f885903, e771c46, 8065661 on base b761462) in worktree wf_ce3f2350-5aa-1. RT.json still holds where this
file is silent; where they differ, this file wins. The orchestrator squashes at merge, so the commit count does not matter.

## Decisions

- perch/tests/README.md's `runs.rs` row (changed by 0717fde): KEPT. It is granted; it is accurate.
- The RT live check on 48142: covered by tests/runs.rs, which drives the real perch binary in front of the fake door through the real
  HTTP routes. No browser check for RT; GP-a's live check exercises the routes from the page.
- tests/guard.rs nested_find_exec_and_repeated_flags_are_linear_and_bounded (60 s wall bound, 62-72 s on this box's debug build) is not
  RT's: if it is the only failure, re-run it alone and report both numbers.

## Fixes

1. Option B. GRANT perch/Cargo.toml, the serde_json line only: `serde_json = { version = "1", features = ["raw_value"] }` (no new crate;
   Cargo.lock must not change: check with `git diff --stat Cargo.lock perch/Cargo.lock`, and build with --offline). Replace tag_or_none's
   custom visitor (state.rs:188-233) with the raw slice: deserialize `Option<Box<serde_json::value::RawValue>>` and keep it only when
   `serde_json::from_str::<String>(raw.get())` is Ok. The verifier's probe (scratchpad rt-probe/src/bin/raw.rs) shows all eleven shapes
   load. Caveat: RawValue fails inside buffered content (`#[serde(flatten)]`, untagged enums): confirm Run is not deserialized that way, and
   prove it through Store::open, the real load path.
2. Tests: the unit test (through Store::open) and the tests/runs.rs integration test cover chat 7, {"x":1}, [1], true, null, an array
   nested 200 deep, 1e400, -1e400, "\ud800", "aA" (a good escaped id) and "ok1": every run loads, none is set aside, only the strings
   that pass id_ok answer a chat. Revert the Cargo/state.rs change in a scratch copy and say which assertion goes red for 1e400 and for the
   lone surrogate.
3. Docs: runs/README.md:52-53 and the tag_or_none doc say what the code now does (a tag that is not a JSON string, of any type, depth or
   range, is no tag; nothing about the tag sets the file aside); README:22 reads `a chat= with no id` instead of `no chat`; the inventory
   (scratchpad review-RT/inventory.md) :33 and :48 match, and its header names the final HEAD.
4. The report names every commit since b761462, the final HEAD, the counts at that HEAD (lib and runs), and clippy both ways.

## Grants (exactly these)

RT.json's owns_files (perch/src/runs/state.rs, perch/src/runs/README.md, perch/tests/runs.rs), perch/tests/README.md's runs.rs row,
perch/Cargo.toml's serde_json line (item 1 only), the inventory file. Never edit statefile.rs, tests/common/mod.rs, Cargo.lock, lib.rs or
main.rs; never rustfmt.

One new commit on top of 8065661, repo style (`perch: ...`), no attribution. Whole perch suite once with --features test-bins (no
--skip); clippy clean both ways.

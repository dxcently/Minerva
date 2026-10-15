# T2 fix round 3 (orchestrator's decisions on the round-2 verifier's last gaps)

T2 is at df26ab2 (97a0bcd, 5549f63, 9e2bb9b, df26ab2 on base 08c01fa) in worktree wf_0cb668b4-3e3-1. T2.json still holds where this
file is silent; where they differ, this file wins. The orchestrator squashes at merge, so the commit count does not matter.

## Decisions (no code change)

- `idle_ms` 0 with `for` answers at once with no text wait: ACCEPTED as documented at perch/src/term/agent.rs:23 (no new 400).
- stream.rs:419's "(T2)" label on note_input is outside the grant: the orchestrator records it as a follow-up.
- The runs.rs fixed-port flake (48140/48141 "Address already in use" when two perch suites run at once) is R1a's, not T2's: if it is the
  only failure, re-run runs.rs alone and report both.
- $HOME/.cache/T2-live (the round-1 live tree with its dead header files) was deleted by the orchestrator. Never leave a header file
  behind again: a live run's trap removes it.

## Fixes

1. perch/src/term/README.md: a section after `## The hold`, `## What an agent's terminal tools expose`, three bullets: (1) term/read of
   any web terminal, the operator's included, sends its screen to the model provider, secrets too (ruling 2); (2) that text is untrusted
   and can carry prompt injection, and nothing filters it; (3) in yolo the door's policy answers yes to term_send and the perch never
   asks; the text guard (D9) only judges one send's text.
2. open's cols, rows and title: in open_takes_the_chats_folder_and_refuses_a_fifth_terminal, one open with title `w`, cols 100, rows 30;
   assert the operator's row reads cols 100 and rows 30 and the agent list row's title is `w`; a plain `term/open {}` row is 120 x 32.
3. Named keys: an inline `#[cfg(test)]` module at the bottom of perch/src/term/agent.rs asserting every name's bytes (enter 13, tab 9,
   esc 27, backspace 127, up ESC[A, down ESC[B, right ESC[C, left ESC[D, home ESC[H, end ESC[F, pgup ESC[5~, pgdn ESC[6~, ctrl-a 1,
   ctrl-z 26) and None for `ctrl-A`, `ctrl-`, `ctrl-1`, `nope`.
4. The read answer's keys: in open_then_send_then_read_for_the_text_answers_with_it, assert the body has `cursor` and `alt_screen` and
   that `seq` is a u64.
5. The security inventory (scratchpad review-T2/inventory.md): header names the final HEAD; 468 lines for agent.rs at 97a0bcd; the suite
   counts at the final HEAD.

Each new assertion must fail when its code is broken: say how you checked each (a scratch revert, never a commit).

## Grants (exactly these)

perch/src/term/agent.rs (item 3's test module only, plus nothing else), perch/tests/term_agent.rs, perch/src/term/README.md (item 1),
perch/tests/README.md term_agent row, the inventory file. Never edit stream.rs, mod.rs, screen.rs, tests/common/mod.rs, Cargo.*,
lib.rs or main.rs; never rustfmt.

One new commit on top of df26ab2, repo style (`term: ...`), no attribution. Whole perch suite once with --features test-bins (no
--skip); clippy clean both ways. No live check is needed (no route changed); if you run one anyway, its header files are removed by its
trap and you say so.

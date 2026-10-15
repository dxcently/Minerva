# T2 fix round 4 (written 2026-10-15 from the security review wf_759b31f7-674)

T2 is at eade8cb (97a0bcd, 5549f63, 9e2bb9b, df26ab2, eade8cb on base 08c01fa), branch worktree-wf_0cb668b4-3e3-1. The orchestrator
already merged master 1c8cbd9 in (merge commit d2e8200, no conflicts): do NOT rebase or merge again; HEAD d2e8200 is your base. T2.json and T2-fix3.md hold where this file is silent; where they differ, this file
wins. The review (reviews/T2/result.md) said merge-after-fixes, no high. The operator answered U21 on 2026-10-15: all four as
recommended. Each DECIDED line below is binding.

## Operator decisions (U21, answered 2026-10-15)

- U21a, close while held. DECIDED: close answers 423 while the operator holds the terminal, as send does. The hold is the operator's
  per-terminal stop, and plugins/term/tools/close.rn:5 and :125 already tell the model so. (The other branch: keep close open, then write
  the exception into the term/README.md hold table and fix close.rn:5 and :125.)
- U21b, keys the text guard never reads (up, down, ctrl-r recall history; tab completes). DECIDED: keep the keys, and state in
  term/README.md, the send tool text (plugins/term/tools/send.rn:5) and the guard hint (guard/mod.rs:672-679) that keys are not judged
  and the guard sees only this call's text, not the shell's line. The guard is a hint, not a gate (install/mod.rs:40-49).
- U21c, reads leave no trace on the operator's stream. DECIDED: none in T2; state in the T2 README section that reads leave no
  record. A read event (chat, terminal id, lines, no text) is a later slice if the operator wants one.
- U21d, the 16-terminal table has no operator share: four chats at four running terminals each lock the operator out (409).
  DECIDED: keep 4 slots that only the operator can take (agents see 12). Test: three chats at four each fill the agent share (12); a
  fourth chat's open is a 409; the operator can still open (up to 16 in all).

## Fixes

1. close and the hold, per U21a: the check at agent.rs:461-464 (or in Terms::close, see 2) and a test beside
   the_operators_hold_refuses_an_agents_send_with_423 (tests/term_agent.rs:401-427).
2. The owner rule moves into Terms (review scope-3): Terms::close takes the owner and returns NotYours; the operator's route keeps an
   unchecked close under its own name (routes.rs:249-251).
3. Test: the term and mail tokens get 401 with an empty body on POST /hub/term/t1/hold, /close and /resize (add them to the list at
   tests/agent.rs:519-537).
4. term/README.md, the "What an agent's terminal tools expose" section names, beside the three risks already there:
   - the master bearer file under XDG_RUNTIME_DIR is readable from an agent's shell (same uid; lib.rs:355-358,388-395; pty.rs:512-531),
     with the same-uid caveat :114-116 gives terms.json, and :119's "0600 files" sentence says so too;
   - every credential exported in the perch's environment reaches the shells (pty.rs:512-531);
   - list hands every chat every terminal's title and foreground command (agent.rs:166-186);
   - keys and the guard per U21b; reads per U21c;
   - :335 says what typed_text actually emits (U+2400 control pictures, term/mod.rs:981-991), not ^X.
5. The stream section (about :332-335): agent-supplied strings (the open title, term-input text, fg) are text and never HTML. T4a's
   review checks the page follows it.
6. U21d's reserve (4 slots only the operator can take; agents see 12) and its test.
7. Optional, cheap: a chunked body over 16 KiB to a term route is a 413 (body_of's Limited, serve.rs:516-536).
8. Live check on the final head (fake perch on 48115, curl -N shows term-input and term-out); the trap removes its header files.

## Grants

T2.json's owns_files, perch/tests/term_agent.rs, perch/tests/agent.rs (the list at :519-537 only), perch/src/term/README.md,
perch/tests/README.md's term_agent and agent rows, plugins/term/tools/close.rn and send.rn (tool text only), perch/src/guard/mod.rs (the
hint comment at :672-679 only), perch/src/term/mod.rs (Terms::close's signature and the U21d reserve only). Never edit stream.rs,
tests/common/mod.rs, Cargo.*, lib.rs or main.rs; never rustfmt. One new commit, repo style (`term: ...`), no attribution. Whole perch suite
with --features test-bins once, clippy clean both ways, then verify and the review (lenses scope/input/leak).

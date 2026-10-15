# T4a fix round (written 2026-10-15 from the round-4 verifier's last gaps; for the next machine)

T4a is at 713b2de (5413f34, 6f55e10, c63f970, 713b2de on base 08c01fa), branch worktree-wf_52c52471-5b4-1 (also in the
2026-10-15 bundle). Base it on master as it stands when this round runs: rebase first, the README rows will conflict like W3's did.
T4a.json still holds where this file is silent; where they differ, this file wins. The orchestrator squashes at merge.

## Decisions (no code change)

- activity.js's no-document guard on startActivity() (6f55e10, now activity.js:93): KEPT. It is what stops the node test hang.
- perch/src/term/README.md:133 (changed in c63f970 beyond the round-1 grant): ACCEPTED. It says the page swallows both queries,
  which is true at HEAD and matches the granted sentence at :143.
- Swallowing DECRQSS and XTVERSION (no reply beats N replies): stands, as documented in terms/README.md.

## Fixes

1. Tests that fail when broken. Export TYPING_MS and SHOW_MS from tab.js and assert their values in the bounds test
   (terms.test.mjs ~571). Add TAB_WIRING entries (~885) for `openCmd('term ')`, `e.key === 'Enter' || e.key === ' '`,
   `r.by === 'agent' && r.alive && !r.hold` and the mouse-enter freeze. Re-run the five mutations from the verifier (TYPING_MS
   2000->60000, the openCmd line, the Enter/Space open, takeOverAll's filter, the mouse-enter freeze): each must go red. The
   tests/README.md row says only what is pinned.
2. The foreground fallback. Clip fg with clipText(v, TITLE_MAX) in vet() (model.js:53) and in onFrame's term-title case (model.js:283);
   tab.js:88 reads `titleText(row.title) || titleText(row.fg) || id`. Test: a 200-character fg with U+202E is cut to 80 and scrubbed.
3. Folded and unopened terminals (the spec says lazy). DECISION: create a screen only while the ⌨ tab is mounted, so the xterm module
   is not loaded at page boot (the queue and resync already cover output that arrives before a screen). Folded screens may stay in
   the off-screen park if xterm needs layout to measure; terms/README says which, and why.
4. A person's refresh uses hubGet, not the poll lane: a one-shot `refreshNow()` in model.js (hubGet('term', POLL_TIMEOUT), applyList on
   200 when no stream is running), called from newTerm in tab.js:77.
5. A reset's continuation chunks (perch/src/term/README.md:341: a reset over 32 KiB is term-reset then term-out chunks) do not count
   against the 2 MiB backlog. If nothing in the frames marks them, set the bound to RING_MAX (8 MiB, pty.rs:53) and say so. Test: a
   reset followed by term-out chunks over 2 MiB in the perch's own framing asserts no resync.
6. Docs: view.js:3 says the screens are written while a view shows the stream; drop the unused FitAddon load (view.js:127) or say why
   it stays; terms/README names the queue's busy getter (model.js:181) as the in-flight flag.
7. From T2 (cross-slice, its spec's risk line): the ⌨ tab states the three risks perch/src/term/README.md ("What an agent's terminal
   tools expose") lists: a read sends any web terminal's screen, the operator's too, to the model provider; that text is untrusted and
   unfiltered; in yolo the agent types with no ask. A short line in the tab, linking nothing; source-scan test.
8. The report names every commit since 08c01fa, the final HEAD and the counts there (page suite, terms.test.mjs).

## Live check (before the merge, on the final HEAD)

The 48132 live check at 5413f34 no longer stands: c63f970 and 713b2de changed view.js, model.js, tab.js and activity.js. Re-run steps 1-9
from T4a.json on the final HEAD with the fake perch (perch/tests/live/fake-perch.sh); step 7 counts ONE toast per agent-opened
terminal. A live run's trap removes anything it wrote; the login URL is navigated to, never written to a file.

## Grants

T4a.json's owns_files, webui/term/tests/terms.test.mjs and its tests/README.md row, terms/README.md. Never edit core/, vendor/ or perch/.
One new commit, repo style (`webui: terminals: ...`), no attribution. Then verify, then the security review (lenses render/input/stream).

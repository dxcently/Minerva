# W3 fix round 3 (orchestrator's decisions on the round-2 verifier's last gaps)

W3 is at c247359 (772790e, 6114770, c247359 on base b08a32e) in worktree wf_01c71499-f81-1. Page suite 352 (351 pass, 1 todo).
W3.json still holds where this file is silent; where they differ, this file wins. The orchestrator squashes at merge, so the commit
count does not matter. Master has moved to b761462 (W2b-2 merged: graph/steps.js, park.js, context.js); do NOT merge or rebase it.

## Decisions

1. The verdict attach rule, GRANTED: webui/term/core/state.js lines 755-756 only (the verdict() attach), exactly the change the
   round-2 fixer proved in a scratch copy: the candidates are the open calls with that tool name; attach only when there is exactly
   one AND it holds no verdict yet; otherwise add the verdict as its own row. Then drop the `{ todo }` at turns.test.mjs:157 so the
   test runs for real, and drop the todo sentence in graph/README.md. Run the whole page suite: any other test that reads verdicts
   (renderers, the inspector's rec tab) must still pass; if one fails, STOP and report it, do not bend it.
2. "Picking a run shows the run" is DROPPED from W3's live check: picking is the run strip's job, which slice GP-a (the sub-tab
   strip) now owns. Say so in the report; no code.

## Fixes

3. Stop then send: a turn the person stopped (cancelled, never settled) is closed by the stop, so the next message starts a new turn.
   A test in turns.test.mjs: message, call, stop, message, call gives two turns, the first read stopped. Red when the fix line is
   removed (say which).
4. The drawing fits the inspector. The inspector's default width is 34 cells (app.js SIDES.r); measure what chat A of your live
   check draws at that width. Pass layout options from turns.js to draw.js's layout() (draw.js is not granted: use only the options
   it already takes, e.g. ranks top to bottom) so a 3-turn chat is no wider than the inspector at the default font; report the
   drawn width before and after. If layout() takes no option that helps, say so with evidence and change nothing.
5. Minors: cap ask and verdict boxes per turn like calls (one `+N more` box); the tooltip of a cut box is a whole sentence; the
   TurnGraph signature and graph/README.md agree on `{ s, sid }`; the source scan in turns.test.mjs also forbids `globalThis` and
   dynamic `import(`.
6. The report names every commit since b08a32e with its sha, the final HEAD and the counts at the final HEAD.

## Grants (exactly these)

webui/term/graph/turns.js, webui/term/tests/turns.test.mjs, webui/term/core/state.js lines 755-756 (item 1 only), the graph/README.md
turns row, the webui/term/tests/README.md turns row. Never edit app.js, other core/* lines, draw.js, vendor/* (except the byte-exact CR
restore in the suite line), index.js, view.js, runs.js or W2b-2's files.

One new commit on top of c247359, repo style (`webui: ...`), no attribution. Whole page suite once; turns.test.mjs alone once.

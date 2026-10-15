# W2b-2 fix round 3 (orchestrator's decisions on the round-2 verifier's last gaps)

W2b-2 is at af02cbf (d00db04, e5e9d9f, af02cbf on base b08a32e) in worktree wf_c49456c8-919-1. Page suite 362/362, steps.test.mjs
34/34. W2b-2.json still holds where this file is silent; where they differ, this file wins. e5e9d9f is the round-0 fixer's commit
(author Lỳ is this machine's git user), not a stranger's: no action.

## Decision (no code change)

- The bars footer stays `… N more`, the footer ProbBars in renderers/jev.js already draws (one wording shared with W1's jev rows).
  This supersedes W2b-2.json's `more... (N)`. Say so in the graph/README.md steps row. Do not edit renderers/jev.js.

## Fixes

1. A run with no steps still gets its end line. A run that failed, was cancelled (with its reason) or was abandoned before its first
   jev_mark draws the same end line `endOf` gives a run with steps, under an empty frames area (no `[ all N steps ]`, no empty frame).
   Tests in steps.test.mjs, each failing when the fix is removed (say which line): (a) failed before step 1, the door note kept;
   (b) cancelled before step 1, its reason kept; (c) the perch ended a parked run with no step (abandon). Add them to the mutation
   list if the file keeps one.
2. The report names every commit since b08a32e with its sha, the final HEAD, and the counts at the final HEAD (page suite and
   steps.test.mjs). No stale numbers.

## Not this slice (recorded by the orchestrator)

- POST /hub/runs/<rid>/answer says 202 before eidolon's refusal lands (seen when the page gate is bypassed). Perch behaviour (R1a's
  code): a perch follow-up, not a page fix.

## Grants

webui/term/graph/steps.js, webui/term/tests/steps.test.mjs, the graph/README.md steps row, the webui/term/tests/README.md steps row.
Never edit app.js, core/*, vendor/* (except the byte-exact CR restore in the suite line), renderers/*, or another slice's files.

One new commit on top of af02cbf, repo style (`webui: ...`), no attribution. Whole page suite once; steps.test.mjs alone once.

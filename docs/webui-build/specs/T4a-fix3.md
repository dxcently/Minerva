# T4a fix round 3 (written 2026-10-15 from the security review of 0428ff4)

T4a is at 0428ff4, and the orchestrator merged master 4831852 (with T2) into it as 82f677b, with no conflicts. Do NOT rebase or merge again;
82f677b is your base. T4a.json and T4a-fix2.md hold where this file is silent. Where they differ, this file wins. The review said
merge-after-fixes, with 1 medium and 4 lows. Its findings are in this file.

## Rulings (no change)

- Fix 5 of round 2 as built (reset chunks recognised by the perch's seq marker, BACKLOG_MAX kept at 2 MiB): ACCEPTED.
- The perch README section that the risk line cites, "What an agent's terminal tools expose" (perch/src/term/README.md:200), and the
  text-never-HTML rule (:373) are now on this base. Check that the tab's risk line matches that section's three risks. Fix terms/README
  if it says the section is missing.

## Fixes

1. stream-1 (medium). After resync(), frames still in the read buffer re-set the entry's seq, so the reset never comes and the
   resync mark sticks (the 2 MiB guard stays off for good). Gate the stream callback on the abort signal
   (`(f) => { if (ep === epoch && !ctl.signal.aborted) onFrame(f) }`). onFrame stores no seq while the entry is resyncing; a reset
   clears the mark. Test: two frames in one buffer after a resync; seq stays unset; the next reset clears the mark.
2. stream-2 (low). A reset's seq is committed on its first frame, so a connection that drops mid-reset resumes past the unsent
   chunks. Mark the entry "reset in progress" until a frame at a different seq arrives. If the stream ends first, forget the seq,
   so the next connect asks for a fresh reset. Test it.
3. render-1 and input-1 (low). The vendored xterm still answers the private DSR (CSI ? 6 n) and multi-parameter DSR, and the reply
   goes out as operator input. Add a { prefix: '?', final: 'n' } entry to QUERIES. Make the DSR entry match xterm's params[0] switch
   (p.length >= 1 && (p[0] === 5 || p[0] === 6)). Add a test that runs the REAL vendored xterm headless (or with the smallest stub
   it needs), feeds every QUERIES form plus CSI ? 6 n and CSI 6;1 n, and asserts onData never fires. If the real xterm cannot run
   under node, say so in the report and terms/README, and pin the table instead.
4. render-2 (low). A title made only of controls or spaces draws a blank header. Trim in titleText (or after clipText), so a blank
   title falls through to fg, then to the id. Test with a whitespace-only title and a controls-only title.
5. stream-3 (info, cheap). A failed xterm load with continuous output gives an immediate reconnect loop. Enforce a minimum wait
   of 1 s between reconnects even when quick is true. Test it.
6. The security inventory at scratchpad/review-T4a/inventory.md: update each claim these fixes change. If you can pin "the token
   is never in a URL, a log or the DOM" with a cheap source-scan test, add it.

Out of scope: render-3 (show the OSC 8 destination on hover) and stream-4 (hub-stream-end), both left as follow-ups.

## Grants

T4a.json's owns_files, webui/term/tests/terms.test.mjs, its tests/README.md row, terms/README.md. Never edit core/, vendor/ or perch/.
Every Edit/Write path starts with the worktree path. The main checkout C:\Users\dxcen\Projects\Minerva must stay clean. One new
commit, repo style (`webui: terminals: ...`), no attribution. Page suite and terms.test.mjs counts at the final HEAD. Run the mutations
on scratch copies: revert each fix and show it goes red.

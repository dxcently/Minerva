# T4a inventory: terminals in the page (fix round 3)

Base: master 08c01fa. T4a: 5413f34. Fix round 1 (executor): 6f55e10. Fix round 2 (executor, answering the verifier's first list): c63f970. Fix round 3 (this round, the verifier's gaps 1 to 7 and 11; gaps 8 to 10 are for the orchestrator): one commit on top of c63f970, branch `worktree-wf_52c52471-5b4-1`. Its sha is in the report.
Worktree: `C:\Users\dxcen\Projects\Minerva\.claude\worktrees\wf_52c52471-5b4-1` (WSL `/mnt/c/Users/dxcen/Projects/Minerva/.claude/worktrees/wf_52c52471-5b4-1`).
Scope: `webui/term/terms/` (index.js, model.js, view.js, tab.js, activity.js, terms.css, README.md) and `webui/term/tests/terms.test.mjs`, plus README rows: `webui/term/tests/README.md` (the terms row), `webui/term/README.md` (changed by 5413f34: the ⌨ tab row and the terms/ line, 2 insertions and 1 deletion; not changed in rounds 1 to 3), and `perch/src/term/README.md` (the granted sentence at :143, round 1; the table row at :133, round 2).

Fix round 1 touched: `webui/term/terms/README.md` (the queries bullet), `webui/term/tests/registry.test.mjs:78` (one assertion), `perch/src/term/README.md:143` (one sentence), `terms/activity.js:88-92` (the `startActivity` guard).
Fix round 2 touched: `activity.js` (report), `model.js` (term-exit), `view.js` (three exports), `tab.js` and `terms.css` (the line in the header), `terms/README.md`, `tests/terms.test.mjs`, `tests/README.md` (the terms row), `perch/src/term/README.md:133` (one table row, risk 10).
Fix round 3 touches: `model.js` (`upsertRow` returns a boolean, a 17th `term-open` gets no row, no entry, no event; the entry has `queued`), `view.js` (`push`: a reset's own bytes are not counted, its write clears the resync mark when parsed, the queue before a screen exists is bounded at `BACKLOG_MAX`; the reset hook no longer clears the mark itself), `activity.js` (`poll` exported, `_resetPoll` for tests), `terms/README.md` (the view row and the poll's cost paragraph), `tests/terms.test.mjs` (19 new tests, two renamed, eleven tab wiring tests), `tests/README.md` (the terms row).
Not touched in round 3: `index.js`, `tab.js`, `terms.css` (no runtime change; covered by tests now), `app.js`, `core/*`, `vendor/*` (the nine CR-only files stay unstaged), `registry.test.mjs`, the perch.

## Files and what each does at runtime

| file | runs in the browser | network |
|---|---|---|
| `index.js` | registers the ⌨ tab (global, badge), the frame tap, and starts the activity poll (`startActivity`, a no-op without a document) | none itself |
| `model.js` | the roster, the one terminal stream, input, pure helpers | `GET /hub/term` via `hubPoll('term')` (the lane, 5 s, stream closed only, model.js:413); `GET /hub/term/events` (stream, model.js:395); the one raw input `fetch` (`POST /hub/term/<id>/input`, model.js:424) |
| `view.js` | one xterm per terminal, colours from `--base0X`, link handler, query swallow, the bounded queue and the resync mark | none (lazy import of the vendored xterm and fit addon, same origin, at the first terminal in the roster) |
| `tab.js` | the tab: new terminal, stack, folds, take over / hand back, pane link | `POST /hub/term` (new), `POST /hub/term/<id>/hold` (take over, hand back) via `hubPost` |
| `activity.js` | frame tap on open chat streams, the rail badge, the toast, the rail switch, the roster poll (`poll`) | `GET /hub/term` via the lane while the stream is closed |
| `terms.css` | styles | none |

## Routes the page calls, and what the perch answered (fake perch on 48132, 2026-10-08)

Recorded in the earlier session. Not re-run in fix rounds 1, 2 or 3 (no perch change in any of them, apart from the README row).

| call | body | answer seen | page handles |
|---|---|---|---|
| `POST /hub/term` | `{}` (the chat form is `{chat}`; not run live) | 201 `{id}` | toast on failure |
| `GET /hub/term` | none | 200 list, rows with `id, title, fg, by, cwd, cols, rows, alive, seq, last_ms, ...` | `vet()` drops a row whose id is not `t<n>` (model.js:48) |
| `GET /hub/term/events` | none | `term-list` first, then `term-reset` / `term-out` for named terminals | decoded per frame; a bad frame dropped (model.js:262) |
| `GET /hub/term/events?since=bogus` | | 400 | not sent by the page (checked) |
| `POST /hub/term/events` | | 405 | not sent by the page (checked) |
| `POST /hub/term/<id>/input` (`application/octet-stream`) | raw bytes | 204; echo arrives as `term-out` | 204 is success; anything else says the refusal words once and drops what waits; no retry; 10 s timeout (INPUT_MS) gives status 0 |
| same, `application/json` | JSON | 415 | not sent by the page (checked: perch refuses it) |
| same, 65537 bytes | raw | 413 | the page sends at most 64 KiB per call (`MAX_CHUNK`, model.js:16) |
| same, unknown terminal `t999999` | raw | 404 | refusal words |
| `POST /hub/term/<id>/hold` `{hold}` | JSON | 200 `{hold}` (true and false both) | take over / hand back; the answer is re-read |
| operator input while held | raw | 204 | the hold stops agent input only (perch README), confirmed |
| `POST /hub/term/<id>/resize` `{cols, rows}` | JSON | 200 `{cols, rows}` | `term.resize` on the screen hook |
| `GET /hub/term/<id>/text?lines=5` | none | 200 `{text, cursor, alt_screen}` | not used by the page (checked) |
| `POST /hub/term/<id>/close` | none | 202 | not used by the page (checked) |

Not sent by the page in this slice: `text` and `close` (T4b may use them).

## Decisions applied

- D11: the terminals use `"Cascadia Mono", Consolas, monospace`, not the page font (view.js FONT, pinned by a test).
- One terminal stream takes one of the four connection slots (`extraStreams = 1`); chats get three. The stream opens only when a slot is free. Otherwise status `slot` and a sentence.
- The keyboard rail switches once per terminal when the focused chat's agent opens one, and never moves keyboard focus (activity.js, `report`). It needs the terminal's id, so it happens on the id'd open only.
- Every terminal's xterm link handler is `LINKS` (view.js:67-75), set on every terminal in `create`. Only `http:` and `https:` open (`linkUrl`, view.js:62-66), only on ctrl or cmd click, with `noopener`. `allowNonHttpProtocols` is false. `window.open` is called at view.js:73 only, behind the check.
- Queries the page swallows (view.js:18-28, `QUERIES`): DA1, DA2, DSR 5 and 6, DECRQM (ANSI and DEC), `CSI 18 t`, OSC 4, 10, 11, 12 when asked with `?`, plus DECRQSS and XTVERSION. DECRQSS and XTVERSION are swallowed on purpose: neither side replies to them (decided at review, fix round 1; see risk 1).
- Input is raw `application/octet-stream`. `hubPost` sends JSON and cannot carry it, so `model.js` makes one local `fetch` with `auth()` from `core/api.js`.
- New terminal sends `{chat}` only, or `{}` from the home menu; an explicit folder is not sent (the perch root-checks an explicit one). `in folder…` opens the `:` line with `term `.
- Fix round 1, activity.js:92: `startActivity()` does nothing without a document. Node's tests import this folder; the poll's interval kept the test process alive. In the browser the poll starts as before.
- Fix round 2, gap 1 (one toast per terminal): an open with no id yet (the tool call's start, from the frame tap) raises the badge only (activity.js:63). The terminal's own open says it: the stream's `term-open` (model.js hooks.event) or the poll's `diffRoster` open.
- Fix round 2, gap 3 (the line in every header): the agent's last input line is a header item, folded or not (tab.js:104). It is clipped with an ellipsis in the header (terms.css:28).
- Fix round 2, gap 4 (the exit): `term-exit` reads the perch's flat `code` and `signal` (model.js:287). The roster row keeps them nested under `exit` (vet, model.js:49-50).
- Fix round 3, gap 4 (the reset): the reset hook no longer clears the resync mark (view.js:225). `push` clears it in the write callback of the reset's own bytes, so the backlog the reset waits behind cannot resync it again (view.js:156-177).
- Fix round 3, gap 7 and the reset-count rule: a reset's bytes are not counted against `BACKLOG_MAX`, on a screen or in the queue before one (`const n = reset ? 0 : bytes.length`, view.js:157). The reason is in risk 14. Output queued before a screen exists is bounded at `BACKLOG_MAX`: past it the queue is dropped and the terminal resynced, and output that arrives while the resync waits is not kept.
- Fix round 3, gap 6 (the table): a `term-open` for a 17th terminal gets no row, no entry, no screen and no event (model.js:271). The perch refuses a 17th terminal too (409), so this is the page's defensive path.
- Fix round 3, test seams: `poll` and `_resetPoll` are exported from activity.js; `VIEW` in the test file holds the view's hooks, since afterEach puts the model's no-op hooks back. No other runtime change.

## What is never exposed

- The bearer token: read by `auth()` (core/api.js:65-68) for each request, never written to a file, never in argv, env or a URL, never in a log, never in this report. No fix round adds token use.
- The login URL and its one-time code: never written anywhere by this slice or by any fix round.
- Terminal output is rendered only into xterm, which draws text. Titles (`titleText`, view.js:235), folders, agent lines (model.js:291) and the activity toast's chat name (`chatName`, activity.js:44-47) go through `clipText` (model.js:74), which removes controls, bidi marks and zero-width characters and cuts length. Terminal ids in toasts are checked by `TERM_ID` first (activity.js:25, model.js:14). Nothing is set as HTML: a source scan in terms.test.mjs forbids `innerHTML`, `insertAdjacentHTML`, `dangerouslySetInnerHTML`, `eval(` and `new Function` in the page sources.
- A toast shows the chat's name (up to 40 characters) and a terminal id, never the terminal's text.
- The screen colours come from `--base0X` as `#rrggbb` only; anything else is left to xterm (a test in terms.test.mjs).
- Storage: the only browser storage is the fold in `localStorage` (model.js:113-118), with `try/catch` and only `t<n>` keys kept with `o`/`c` values.
- Links: `window.open` is called only from the link handler (view.js:73), only for http or https, with `noopener`. A test checks the gate, the scheme refusal and the arguments; a source scan checks that no other file opens a window.
- Memory: the bytes a screen holds (its backlog, and the queue before a screen exists) are bounded at 2 MiB each (`BACKLOG_MAX`), and past the bound the terminal resyncs. The roster is bounded at the perch's 16 terminals.

## Risks and open points

1. DECRQSS and XTVERSION are swallowed by the page and the perch never answers them. Decided at review (fix round 1, commit 6f55e10): no reply beats one reply from every open page. A program that asks for either gets no reply from either side, which can make a program that waits for one of them stall. It is not a double answer. Stated in `webui/term/terms/README.md` (Decisions) and in `perch/src/term/README.md` (the "For T4a" paragraph).
2. Input goes through a local `fetch` in `model.js` (line 424), not `hubPost`. It bypasses the request lane on purpose (a person's typing must not wait behind polls). The URL is built from a checked id (`TERM_ID`, model.js:420), tested in terms.test.mjs.
3. Hold semantics: hold stops agent input only. The page's "take over" holds every linked agent terminal, so the operator can type while agents are held. Confirmed live earlier (hold on, operator input 204).
4. A chat's remembered folder is not root-checked by the perch (perch README). The page never sends a folder for `{chat}` opens, so this is the perch's rule, not the page's.
5. Unverified in a browser (no browser here): xterm rendering and fonts; the clipped live view's height and follow; the theme MutationObserver; ctrl-click on a link; the stream's 10 s close timer in the page; the badge and the one-time rail switch; the keyboard tab under real focus; the header line in a folded item; one toast for an agent's open with the stream closed (up to 5 s); the poll's interval wiring under a real document (`startActivity` does nothing under node, so the test checks the call in the source, not the timer).
6. Connection budget: the terminal stream takes one of four slots. With all slots used, the tab says `slot` and does not open the stream.
7. Latency: the measured round trip is perch side only (POST start to the echo reaching the perch), median about 18.9 ms over five runs, from the earlier session. Browser keydown to paint is not measured.
8. RESOLVED in fix round 1 (6f55e10): `webui/term/tests/registry.test.mjs:78` asserted the old stub text and called `terms.component()` with no props. It now asserts `typeof terms.component === 'function'`.
9. Vendor files: nine files under `webui/term/vendor/` show modified in `git status` (CR restore, stat cache). `git diff --ignore-cr-at-eol -- webui/term/vendor` is empty, so the content matches. They are not staged and are not in any fix commit.
10. OPEN for the orchestrator (verifier gap 8): `perch/src/term/README.md:133` (the XTVERSION and DECRQSS row) was changed in round 2, one row beyond the round 1 grant. It now says the page swallows them too, which is true at HEAD and matches line 143. Accept the row or revert it to the granted sentence only. Round 3 did not touch it.
11. Test-only guard (activity.js:92). Reverting it made `node --test` hang: `registry.test.mjs` finished 40 of 40 tests, then the process did not exit, and the whole-suite run hung for about six minutes. With the guard the whole suite exits in about 1 s. In the browser `document` exists, so nothing changes there.
12. OPEN for the orchestrator (verifier gap 10): the 5 s roster poll makes a screen for each terminal it lists, so the vendored xterm module loads on any page where a terminal exists, even with ⌨ never opened, and up to 16 shelf screens are kept in step. The executor recorded this as a decision; no orchestrator decision exists for it. The alternative is screens on mount only, which round 3's queue bound now makes safe to build. Not measured in a browser.
13. RESOLVED in fix round 3 (verifier gap 4). A reset that arrived while the screen held more than `BACKLOG_MAX` unparsed bytes resynced again at once. The old reset hook cleared the mark before `push`, and `push` checked the backlog the reset did not drain. Reproduced on a scratch copy before the fix (`repro-reset.mjs` in the scratchpad: after 3 MiB unparsed and a reset, `seq` was undefined again). Now the mark clears in the write callback of the reset's own bytes. Test: 'a reset that arrives while the screen is still over 2 MiB does not resync again; its own bytes clear the mark once parsed' (terms.test.mjs), which drives the view's real reset hook (`VIEW.reset`).
14. NEW in fix round 3, reset-count rule. A reset is a snapshot, not backlog: its bytes are not counted against `BACKLOG_MAX`. The reason: the perch's ring is 512 KiB by default (perch/src/term/pty.rs:51) but `--knob term_ring` allows up to 8 MiB (pty.rs:52-53), and a screen snapshot can be large too. Counting a reset bigger than the bound would resync on every reset, and in the queue path would drop the reset and reconnect with no wait. The rule is pinned by 'a reset bigger than the bound is a snapshot, not backlog'. Output that follows a large reset is counted as usual. Not measured with a real 8 MiB ring.
15. NEW in fix round 3, gap 6. A 17th `term-open` is refused at the roster: no row, no screen, no event. The perch's own table refuses a 17th terminal (409), so this path is defensive. Pinned by 'the 17th terminal from term-open is refused as the list is'.
16. The live check at 5413f34 does not stand at HEAD: round 2 changed `model.js` (term-exit), `activity.js` (the toast gate) and `tab.js` (the header line), and round 3 changed `view.js`, `model.js` and `activity.js`. The browser steps below are the orchestrator's (verifier gap 9).

## Tests

- Fix round 3, the whole page suite on the round 3 commit: `node --test "webui/term/tests/*.test.mjs"` from the worktree root, Windows node v26.7.0: tests 435, pass 435, fail 0, cancelled 0, skipped 0, exit 0, under 1 s.
- Per file in fix round 3: ctx 7, jevrun 54, keys 14, lane 10, loaded 17, mesh 29, notes 39, probbars 21, project 33, registry 40, runs 36, saved 15, smoke 4, steps 38, terms 69, vendor 9 (sum 435). Each file run alone; each exited with 0.
- `webui/term/tests/terms.test.mjs`: 69 of 69 (49 before round 3). The 19 new tests: the 17th refused; the RIS before a reset; a reset over 2 MiB does not resync again (through the view's real hook) and clears the mark once parsed; a reset bigger than the bound resyncs nothing (screen and queue); the queue before a screen bounded, dropped and resynced, and a reset starts it again; the poll's first look a baseline, the next says a new agent terminal once; door mode and a running stream make no poll call; a poll answered after the stream opened changes nothing and says nothing; the folder registers at import (frame tap, the ⌨ tab with the badge, the poll started); and eleven source-scan tests, one per tab wiring line (take over, pop-out, pane link, wheel capture and passive, wheel default, new terminal's chat, fold save, focus freeze, reorder freeze, yolo line, the 201 check). Two renamed: the reorder test (it only calls `reorderDue`) and the diff test (it runs no poll).
- Mutation checks, round 3 (`mut-fix3c.mjs` in the scratchpad: one guarded line reverted at a time, on a fresh scratch copy of `webui/term`, the terms file run each time; the worktree is not touched): 28 mutations, all red, baseline green. Covered: the 17th row, the roster cap, pollNow over a running stream, a reset counted as backlog, the reset hook clearing the mark, the reset callback not clearing it, the queue bound, the queue's resync guard, a queued reset keeping the mark, the RIS, the first look as news, the baseline never set, the door guard, the stream guard, a late answer reported, the frame tap, the poll at boot, and the 11 tab wiring lines.
- Source checks: no `innerHTML` or `eval`, no colour literal, one `window.open` (behind the http check), one lane poll (`hubPoll`), the input is a raw POST with a checked id, no `document.` or `window.` in the model. All pass.
- The verifier's checks, still held: `linkHandler` calls `window.open` only after `linkUrl` returns an http(s) URL; no clipboard addon is vendored (vendor/ lists none) and none is referenced by terms/; polls go through `hubPoll` (core/lane.js) once in model.js, input never does (its own `fetch`).
- Still covered only in a browser (no node test can reach them without a DOM): the yolo line's rendering, the `this chat` and `others` headings, the right-click menu, take over all, `⇱` and `→ in a pane`, the capture-phase wheel's effect on scrolling, the door and slot words in the tab, the theme observer (`watchTheme`), the follow scroll, the xterm and font load, the activity poll's interval under a document, and the `gone` hook's dispose.

## Browser-pane steps for the orchestrator (at the round 3 HEAD; the verifier's gap 9 list included)

1. Open ⌨, `[ new terminal ]`, type `ls`; the output appears.
2. With 3 terminals open, the newest output rises without taking focus.
3. `[ take over ]` then `[ hand back ]`; the agent line and the hold change.
4. Reload with a vim session open; arrow keys still work (DECCKM restore).
5. A DSR query gets exactly one reply.
6. Typing latency stays near 100 ms while the graph tab polls a stepping fake run.
7. With ⌨ closed, a fake-door `term_open` from the focused chat gives one toast (within 5 s, from the poll or the stream), the badge, and the rail switch once; an `open` with no id gives no toast.
8. A program that asks for DECRQSS or XTVERSION gets no reply and does not stall the shell.
9. `term-input` from an agent, then fold that terminal: its header still shows the `›` line (ellipsis when long).
10. A shell that ends (`exit 3`) shows `○ exited`.
11. Console clean.

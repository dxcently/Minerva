# webui/term/tests

Tests for the page's pure modules, run under node. No DOM, no browser, no network: `fetch` is a stub the test holds. A module that touches `document` at load cannot be tested here (app.js is the only one that does). One line per file.

Run them from the repository root, with Windows node (v22 or later, no install, no npm):

```text
node --test "webui/term/tests/*.test.mjs"
```

The glob is quoted so node expands it itself (the same in a shell and in PowerShell). `node --test webui/term/tests/` with the folder alone does not work on node 22 and later (it takes the folder for a file). From inside this folder, a bare `node --test` finds the files too.

| file | tests |
|---|---|
| `smoke.test.mjs` | `HUB_PATH` / `hubPathOk` (core/api.js): the route families a hub call may take, `tree` only with a query, no `..` segment however it is spelt; and that `hubGet`/`hubText`/`hubPost`/`hubDelete` all refuse a path the whitelist refuses, before `fetch` |
| `lane.test.mjs` | the request lane (core/lane.js): one request in flight across all polls, first in first out, a queued path shared by its callers, user actions never waiting, a failed, thrown or unanswered poll freeing the lane; and that core/hub.js's list and trash polls ride it |
| `registry.test.mjs` | the registries (core/registry.js): tab tables and saved-tab names, badges, bad registrations refused, the stream cap with `extraStreams`, key/focus/where owners, command overrides and the key rows that follow them, frame taps (directly and through a real `Session.frame`), extra panes in the one tree, split and chat-menu extras; and source scans that hold app.js, session.js, state.js and index.html to the wiring (they cannot load in node) |
| `ctx.test.mjs` | the ctx limit (core/ctxlimit.js): `10k ctx` is 10000, `1M ctx` is 1000000, a note with no ctx is null, the notes eidolon and the perch really send, the table built from the `GET /hub/models` rows; and source scans that hold app.js to reading the limit from it, the stale sentences gone, `hubOff` gone from core/ui.js |
| `loaded.test.mjs` | the loaded and diff tabs (panes/loaded.js, panes/diff.js): the tool list merged with the calls seen and its order, the guard line (on, off with a reason, the stub), the `enabled = false` note, the default-mode and connectors lines, the branch line, the store of `/hub/meta` answers (fresh window, one request per path, a failed refresh keeps the good answer), that opening a tab and unfolding the plugins go out while the lane is held (and carry a timeout) while the diff tab's 15 s refresh waits in it, and source scans for "not available yet", the registered sections and raw fetches |
| `saved.test.mjs` | a saved chat (core/saved.js) against the 12 jev wiki-hop fixtures: the workflow records drawn as start/end lines, steps as script calls paired by `tool_use_id`, a park's ask with its answer (or none, or not an option), calls with no result shown stopped, a failed final state marked, no `#undefined` line, and source scans for the wiring |
| `probbars.test.mjs` | the jev renderers (renderers/jev.js): `jev_choose` bars ranked by p with the best marked and the margin, the floor tick and its footer, `jev_entail` scores, `jev_mark` lines, `… N more` past the cap, hostile model text cleaned (controls, bidi, zero-width) and never markup, and `autoOpen` |
| `jevrun.test.mjs` | the run model (graph/jevrun.js): all 12 wiki-hop fixture cases a..l (status, the door's report cross-checked, eidolon's refusal sentence byte for byte, prefix cuts, the graph rebuilt from its pages), incremental feeds equal to one feed, every prefix reduces, and built journals for what the fixtures lack (graph pages, cuts and collisions, stop and exhausted runs, hostile text and prototype keys) |
| `notes.test.mjs` | the notepad (notes/store.js and the folder's wiring): autosave on the base sha, one save in the air, a 409 stops sending and offers reload or keep mine, the localStorage stash and its return, retries and the words for 400/413/401, the 256 KiB byte limit, rename and trash following an open note, the notes-off state, `:note`, and source scans for the page's rules |
| `project.test.mjs` | the graph drawing (graph/project.js, graph/draw.js): all six vendored jev graphs project to the states and edges `jev_graph` gives, and lay out to boxes, frames and lines under dagre without overlap; the marks reach the SVG tree |
| `mesh.test.mjs` | the mesh tab (mesh/view.js, mesh/index.js) over tests/fixtures/aoide/: the header, the not-available sentence, both registry shapes, ages that are cache ages and never presence, drift, graph, outbox, the events tail (two streams labelled by source, a cursor and a reset per stream, the cap, the stale restart, the empty sentence that names each file and what was read of it), hostile text staying a clean plain string, the two polls (never two at once, a failed refresh keeps the good answer), and source scans for the paths asked and for no markup |

A test must fail when the change it guards is reverted: check that by hand when you add one (put the old code back, see it go red, put the new code back).

Later slices add their own `*.test.mjs` here; the glob picks them up.

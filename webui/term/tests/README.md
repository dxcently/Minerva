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
| `project.test.mjs` | the graph drawing (graph/project.js, graph/draw.js): all six vendored jev graphs project to the states and edges `jev_graph` gives, and lay out to boxes, frames and lines under dagre without overlap; the marks reach the SVG tree |

A test must fail when the change it guards is reverted: check that by hand when you add one (put the old code back, see it go red, put the new code back).

Later slices add their own `*.test.mjs` here; the glob picks them up.

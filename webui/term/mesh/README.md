# webui/term/mesh

The left panel's mesh tab: what Aoide knows on this machine, read through the perch's `GET /hub/mesh` and `GET /hub/mesh/events` (`perch/src/mesh/README.md` has the contract) and drawn as text. `app.js` draws `MeshTab` when the tab is chosen. One line per file.

| file | does |
|---|---|
| `index.js` | `MeshTab` and its two polls. Under a door it says the tab needs the hub. Under the perch: a header, the nodes, the mesh drift, the stage graph, the outbox, and the audit and gate tail. The snapshot is asked at open and by `[ refresh ]` (a plain `hubGet`), then every 60 s in the lane; the events every 5 s in the lane (`core/lane.js`), both only while the page is visible, and not at all once the tab is closed. The answers are kept at module level, so a reopened tab draws at once |
| `view.js` | pure: the snapshot to rows and sentences (`stateOf`, `headerParts`, `nodeRows`, `driftView`, `graphView`, `outboxView`, `noteLines`), and the events tail's logic (`eventLine`, `foldFeed`, `resumeFeed`, `feedPath`, `feedView`). Returns strings, numbers and plain objects, never markup |
| `mesh.css` | the tab's styles, linked from `index.html` |

## What the tab shows

```text
aoide 0.0.26 · ~/.local/bin/aoide · aoided ● 3 ms        or the perch's sentence for why Aoide is not available here
as of 15:26:04   [ refresh ]

nodes                       one block per registered node (a table does not fit a 30 cell sidebar)
  osaka          ✓ verified
  grants  message, read     or  alpha: message · beta: message, spawn   (the words per mesh, after P-CHARTER)
  meshes  alpha beta        where the node has a grant, or a row in mesh --json
  added   2026-09-01
  roster  cached 3m ago     or  cached 2d ago · old   or  no roster cached
  presence is not probed: ages are when each roster was last cached
mesh drift                  aoide mesh --json: per mesh, the nodes that differ from their declaration, and the undeclared ones
stage graph                 counts by kind, then a list of at most 30
outbox                      what waits to be sent: the node, its status, the tries, the reason, the last and next try
notes                       the perch's sentences about what it could not read or left out
audit and gate events       newest first, at most 40 of the last 200 held
```

## Rules this tab keeps

- **Nothing here says a node is up, down, online or present.** The perch is network-free; the only time it has is when a roster was last cached, so that is the only thing shown, as an age. The age moves on with the clock since the answer was got, and a roster cached more than a day ago says `old`. The one probe there is, the daemon's `ping`, is of Aoide's own daemon on a local socket and is worded as that (`aoided ● 3 ms`, `aoided ○ no answer`).
- **The events may be absent.** The perch tails two files: the audit log (`--aoide-audit-log`, else aoide's own `log` under its home) and aoided's feed (`--aoide-daemon-events`, else `$XDG_RUNTIME_DIR/aoide/events.jsonl`, which is the perch's own default and not aoide's socket-relative rule). Either can be missing, empty or unreadable, and each stream says so on its own: the empty sentence names each file and how much of it was read (`no events to show. audit log ~/.aoide/log: nothing read yet (empty or missing); ... Nothing shown is not the same as nothing happening.`), never "no activity". A stream that has bytes but shows none of them says that, and a file that cannot be read says the perch's reason. Each line is labelled `audit` or `daemon` by its stream, and its text starts with the record's own class (`audit` or `gate`).
- **Every remote string is text.** The registry, rosters, drift rows, outbox reasons, graph titles and event payloads are Aoide's or another node's. `view.js` runs each through `clean` (`renderers/jev.js`: control characters, bidi overrides and zero-width marks out, cut short) and returns it as a string; `index.js` puts it in a text node. No `innerHTML`, no markup, and no remote word is used as a class name or a key: the colour of a drift row comes from a table. The roster text itself (`cache[].text`) is not drawn at all, only its age.
- **A failed refresh keeps the good answer** and says why above it (`showing the answer from 15:26:04`). An answer of `available: false` is a good answer and replaces what was held.
- **Polls go through the lane**, one in flight across the page, only while the page is visible. The events poll never runs twice at once (two folds of one answer would hold the same events twice). A tab reopened after a minute starts the tail again from the newest events instead of catching up through a backlog. A `reset` from the perch (the feed shrank) clears what is held.
- **The only paths asked for are `mesh` and `mesh/events`.** Nothing here sends, pairs, spawns or dials a node. Mail lives in the notes tab, not here.

## Tests

`tests/mesh.test.mjs` runs `view.js` over `tests/fixtures/aoide/` (what a real perch answered in front of `fake_aoide`, and R2a's own `mesh.json` and `outbox.json`) and the two polls over a stub `fetch`; `tests/fixtures/aoide/README.md` says what is in there.

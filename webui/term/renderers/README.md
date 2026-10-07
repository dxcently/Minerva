# webui/term/renderers

How a tool call is drawn in a transcript: its one-line summary, its output, a preview under its line. `index.js` picks the renderer by tool name and
falls back to `generic.js` when none is named or one throws, so a broken renderer never blanks a pane. One line per file.

| file | does |
|---|---|
| `generic.js` | the renderer every tool gets: the first useful string of the input as the summary, the output as folded text (a unified diff drawn as one). It also says what a renderer is: `input`, `inputFull`, `output`, `preview` |
| `files.js` | `edit`, `write` and `read`: the change an edit makes under its line, a read's file in its language with eidolon's line numbers |
| `jev.js` | jev's tools: `jev_choose` (the options in a line; the result as bars, sorted by p, the pick marked, the margin under them), `jev_entail` (the three scores per hypothesis), `jev_mark` (where a run is: enter, park, final, end). Exports `ProbBars({ rows, floor?, margin?, pick?, max?, more?, head?, width?, cls? })`, which the graph tab draws its decision steps with. Everything a model or a page said is drawn as text, cleaned and cut |
| `index.js` | the table (`named`, one module per family: add yours to the list at the top), `register(tool, mod)`, `render` / `renderCall` (the call's `input` goes to the renderer as `ctx.input`), and `autoOpen(tool, output, isError)`: a renderer with an `autoOpen(output, isError)` says which results are drawn open (jev's bars); the row's own click still opens any row |

A saved chat draws a workflow run's steps through these too (`core/saved.js` maps a WorkflowStep to a tool call of origin `script`), so a run is read
the way a chat is.

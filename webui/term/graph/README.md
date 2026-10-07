# webui/term/graph

The inspector's graph tab: the jev runs and the chat's turn graph. `app.js` lists `GraphTab` in `INSPECTOR`; it gets `{ s, sid }` like every tab. One line per file.

| file | does |
|---|---|
| `index.js` | `GraphTab`. Today only its slot: the sentence `turn graph needs the hub (M3)` on a door, `the turn graph: not available yet` on the perch |
| `project.js` | `project(doc or jsonText)`: a jev graph's JSON -> the drawing the `jev_graph` tool answers ({ graph, initial, states, edges, marks, ... }), mirroring the vendored `jev/tools/graph.rn` function for function |
| `draw.js` | `layout(states, edges, opts)` (boxes, frames and right-angled lines under vendored dagre, pure) and `drawGraph(graph, marks)` (the SVG vnode; marks: current, visited, final, failed) |
| `graph.css` | the tab's styles, linked from `index.html`: the drawn graph's boxes, frames, lines and marks |

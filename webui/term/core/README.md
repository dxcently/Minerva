# webui/term/core

The page's logic and plumbing: no screen layout here (that is `app.js`), no rows drawn (that is `panes/`). One line per file.

| file | does |
|---|---|
| `api.js` | the ONE place URLs are built (`/api/` for the door, `/s/<id>/api/` through the perch); the token (taken from the URL fragment into `sessionStorage`, sent only in an `Authorization` header); `post`; the hub calls `hubGet`/`hubText`/`hubPost`/`hubDelete`/`hubTree` (with an optional abort timeout), which take only the paths `HUB_PATH` allows (the perch's route families, `tree` only with a query, never a `..` segment: `hubPathOk`), and `detect()` (perch, door, refused or offline) |
| `attach.js` | images and text files put into a message, checked against the door's limits before anything is sent |
| `bits.js` | `Needs`, `Soon` and `Kv`: the small text pieces the sidebars and inspector tabs share |
| `hub.js` | the perch's session list as signals: the 5 s poll with a timeout (through the lane), the `stale` flag, the live / kept / elsewhere views, `age()` |
| `lane.js` | the request lane: `hubPoll`/`hubPollText` for every periodic GET, one in flight across the page, first in first out, a path already waiting shared by its callers. User actions (`hubGet`/`hubPost`) never enter it, so one browser connection stays free for typing and answers |
| `life.js` | port of eidolon's TUI Life strip (the pulse drawn while a turn runs) |
| `look.js` | theme (base16 parser, `--base0X` on `<html>`) and font (size, measured cell), kept in `localStorage` |
| `markdown.js` | the safe markdown subset as vnodes: fences, headings, `code`, bold, `http(s)` links |
| `menu.js` | right-click and title menus: one open at a time, submenus, keys, kept on screen |
| `overlay.js` | one panel drawn over the tiles at a time (`openOverlay(Component, props)`, `closeOverlay()`); the slot is in `Tiles`, so a feature opens its panel without editing `app.js` |
| `path.js` | `toWslPath(raw)`: a pasted Windows folder as a WSL path (the start screen). `resolveIn(cwd, path)`: where a model-supplied tool path points, as text; the tool-row `open` items are offered only for a path inside the session's folder |
| `sse.js` | fetch-based server-sent events (Bearer header, never `?token=`), and the refusal body at connect |
| `state.js` | one `Session` per session id: the event stream as signals (frames to rows, calls, asks, running), the reconnect loop with backoff, resume, launch, answers |
| `syntax.js` | port of eidolon's TUI syntax highlighting: code with line numbers, diffs |
| `tile.js` | the split tree: dwindle at 0.618, per-split ratio, drag, promote on close |
| `ui.js` | the one import point for Preact, hooks, signals and htm; `K` (clickable text, no `<button>`) and shared strings |

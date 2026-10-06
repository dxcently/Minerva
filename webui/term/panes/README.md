# webui/term/panes

What is drawn inside a pane or the inspector; `app.js` arranges them and routes the actions. One line per file.

| file | does |
|---|---|
| `asks.js` | the tabbed ask panel (batch submit, notes, its keys) and the transcript's ask markers and settled folds |
| `files.js` | the inspector's files tab: `folder` (the session's directory through `GET /hub/tree`, perch only) and `touched` (files edit/write calls touched), one `/` filter |
| `home.js` | the start screen (no pane open): `[ new chat ]`, `[ open folder… ]`, recent chats, the hint row; actions come in from `app.js` |
| `session.js` | one session pane: frame glyphs, transcript rows (chat, tool lines, the tool-row open items), start screen, the banner with `resume`, the draft, the outbox of queued messages, the composer and its menus |

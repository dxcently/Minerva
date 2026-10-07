# webui/term/panes

What is drawn inside a pane or the inspector; `app.js` arranges them and routes the actions. One line per file.

| file | does |
|---|---|
| `asks.js` | the tabbed ask panel (batch submit, notes, its keys) and the transcript's ask markers and settled folds |
| `diff.js` | the inspector's diff tab: the files the chat's edit and write calls touched, and what the git state will need |
| `files.js` | the inspector's files tab: `folder` (the session's directory through `GET /hub/tree`, perch only) and `touched` (files edit/write calls touched), one `/` filter |
| `home.js` | the start screen (no pane open): `[ new chat ]`, `[ open folder… ]`, recent chats, the hint row; actions come in from `app.js` |
| `imgs.js` | images in a chat: thumbnails from the perch's image route, the full-size viewer, an image file in the chat's folder, image paths in reply text as clicks |
| `loaded.js` | the inspector's loaded tab: the model, the gate, the tools called, what the door has loaded as far as it says |
| `picker.js` | the folder picker panel drawn over the tiles (`GET`/`POST /hub/dirs`) and the project-name step after it |
| `session.js` | one session pane: frame glyphs, transcript rows (chat, tool lines, the tool-row open items), start screen, the banner with `resume`, the draft, the outbox of queued messages, the composer and its menus |

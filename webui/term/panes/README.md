# webui/term/panes

What is drawn inside a pane or the inspector; `app.js` arranges them and routes the actions. One line per file.

| file | does |
|---|---|
| `asks.js` | the tabbed ask panel (batch submit, notes, its keys) and the transcript's ask markers and settled folds |
| `diff.js` | the inspector's diff tab: the chat folder's git branch (or detached sha, or no repository) from `GET /hub/meta/git`, which reads `.git/HEAD` and runs no git, and the files the chat's edit and write calls touched; the branch is asked at open, after edits, and every 15 s through the poll lane |
| `files.js` | the inspector's files tab: `folder` (the session's directory through `GET /hub/tree`, perch only) and `touched` (files edit/write calls touched), one `/` filter |
| `home.js` | the start screen (no pane open): `[ new chat ]`, `[ open folder… ]`, recent chats, the hint row; actions come in from `app.js` |
| `imgs.js` | images in a chat: thumbnails from the perch's image route, the full-size viewer, an image file in the chat's folder, image paths in reply text as clicks |
| `loaded.js` | the inspector's loaded tab: the model, the gate (mode, default mode for new chats, the guard rows' status), the policy file with its rows unfolded on demand, eidolon's full tool list merged with the calls seen, the connectors and the plugin listing, from `GET /hub/meta/*`; also the store of those answers (`pull`, `answer`) the diff tab shares. Opening the tab is a person's action, so its fetches go straight out (never through the request lane) with a timeout |
| `picker.js` | the folder picker panel drawn over the tiles (`GET`/`POST /hub/dirs`) and the project-name step after it |
| `session.js` | one session pane: frame glyphs, transcript rows (chat, tool lines, the tool-row open items), start screen, the banner with `resume`, the draft, the outbox of queued messages, the composer and its menus |

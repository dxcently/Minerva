# webui/term: the Minerva web UI, milestone M1

A terminal screen in a browser (design: `docs/design/web-ui.md`). Preact + `@preact/signals` + `htm`, vendored as plain ES modules under `vendor/`. There is no build step, no npm at runtime and no CDN. After you edit a file, press F5.

## Run it

Through the perch (the hub), from WSL on Windows:

```text
hoot                 # in Windows PowerShell: starts or reuses the Minerva server and opens this page, signed in
hoot status web      # is it up
hoot stop web        # stop it (chats are kept)
```

`hoot` runs `bin/perch-open.sh --code` in WSL, which mints a 30 second single-use login code and prints `http://127.0.0.1:47820/#code=<code>`. The page strips the `#code=` at once, trades it at `POST /hub/redeem` for an `HttpOnly` `SameSite=Strict` cookie, and from then on sends no token at all: every request and stream carries the cookie. A reload or a new tab needs nothing (no code, no token in it: the page asks `GET /hub/sessions` once and the cookie answers; a second use of an old link with the cookie still good just opens); a second use of the same link, or one older than 30 seconds, shows "this link has expired — run `hoot` again". `bin/perch-open.sh --status` and `--stop` also work from WSL.

Or straight to one `eidolon web` door (door mode, no perch), from WSL on Windows:

```sh
eidolon web --bind 127.0.0.1:47811 --ui-dir /mnt/c/Users/<you>/Projects/Minerva/webui/term
# prints:  listening on http://127.0.0.1:47811/
#          token file: /run/user/<uid>/eidolon/web-47811.token
```

Open `http://127.0.0.1:47811/#token=<contents of the token file>`. The page moves the token into `sessionStorage` and strips it from the URL (`#token=` and `?token=` both), so F5 keeps working in that tab. A new tab needs the fragment again. The token only ever leaves in an `Authorization` header. The `#token=` path is for a bare `eidolon web` door only; under the perch, `hoot` signs the page in with a code (above).

If nothing answers at boot (a perch or door still coming up), the page does not stick: it toasts once, then shows `not answering` and probes again with a growing jittered wait (about 1-2 s up to 15-30 s) until something answers; the screen clears by itself.

The page tells the two apart at boot with `GET /hub/sessions` (200 with a list = perch mode, 404 = door mode). Under the perch it opens ONE pane, on the newest live session, and never starts one; with none live the start screen shows (below), and a bare `:open` follows the focused pane (there is no `door` session there). Door mode is unchanged: one pane on the door.

### When a session ends

Under the perch a pane does not retry for ever. The perch's last frame (`hub-stream-end`) and its answers at connect decide what the pane says. `resume` is a click on the banner text, `:resume`, or the `r` key pressed twice within 3 s in a focused pane (the first press only says so in the hint line); it only ever runs when you ask, and it may continue the session's last turn (a resumed log whose last message is unanswered starts one).

| state | when | pane says | resume |
|---|---|---|---|
| stopped | the perch stopped the door (a stop) | `stopped` | yes |
| dead | the door exited, or 503 `dead` at connect | `the door exited (<exit>)` | yes |
| kept | 409 `kept` at connect: a log, no process | `saved chat · read-only`, the transcript drawn from `GET /hub/sessions/<id>/log`, and `[ continue this chat ]` where the composer would be | yes (the action) |
| gone | 404 at connect: not a session of this perch | `no chat <id> here` | no |
| goodbye | the session ended itself | its own text | yes (perch only; door mode says `resume needs the hub`) |

A door that ended the stream and is still there (`live`), 409 `starting` or `stopping`, and 502 reconnect with a backoff: 250 ms doubled for each reconnect that follows too soon (a stream that held 5 s resets it), up to 15 s, jittered; a `lagged` frame and a `context-invalidated` redraw wait the same way. A resume that lands while the old stream is still draining supersedes it: the old connection's frames, `hub-stream-end` included, are dropped, so a freshly resumed session never shows `stopped`. A `context-invalidated` frame from the door (shed what was cached) makes the page replay the log through a quiet reconnect at the next point no turn is running; a session whose state is still unknown (just connected, no `turn-state` yet) counts as idle for this, and only a live frame counts (a replayed one is ignored, so it cannot loop).

### The sidebar under the perch

(The `sessions` tab rows below predate 2026-10-06: its layout is now search, projects, `inbox`, `other`; see "The folder picker and projects". The list source, polling and menus are unchanged.)

The list comes from `GET /hub/sessions` (`core/hub.js`), polled every 5 s (a poll that gets no answer in 5 s counts as failed) while the tab is visible, at once when the tab becomes visible, and after every resume or stop this page makes. There is no `/hub/events` stream (it would take one of the browser's six connections to the perch). A failed poll keeps the last list and draws a dim `list stale`; it never toasts.

| where | what |
|---|---|
| `sessions` tab, project groups | the open panes, plus live sessions of that folder no pane shows: dim `○ <id>  (not open)`; a click opens a pane |
| `kept` | kept logs and exited doors of this perch (`exited`), newest 15 by mtime as `<id or title>  <age>` (`5m`, `3h`, `2d`); `more… (N)` shows 50 more. A click opens a pane (it reads `saved, not running` with `resume`) |
| `elsewhere` | live sessions held by another client (a TUI, a door this perch does not own): read-only, menu lines greyed; the first 15, then `more… (N)` for 50 more (the not-open live sessions in the `sessions` tab do the same) |
| `projects` tab | `sessions here` (the focused pane's folder); `others`: the folders of live sessions, a click opens the first one there. Kept sessions carry no folder |
| `peers`, `mesh` | still placeholders |

**Fork from here.** The row menu of an assistant reply that ended its turn (never the latest, never a user row) has `fork from here`: POST fork `{at}`, then a pane opens beside with a read-only history up to that reply and a composer (`first message`, images, `working... Ns` with the output tail). When the run ends the pane goes live; a failure shows the error sentence and stays pending. A pending chat has a faint `pending` tag in the sidebar under its source's group and can be trashed. The page decides pending from the list row.

Opening: a click on a row, a recent chat on the start screen, or `:open <id>` shows the chat in the focused pane (the first pane if none; the pane that already shows it is just focused). Ctrl-click or middle-click on a row, or `open beside` in its menu, splits instead. Right-click menus: a session in a group has `open beside`, `show in focused pane` and `stop this chat` (asks `stop <name>? its panes will show "stopped"`: `stop` / `keep it`; 202 toasts and polls, 409 says `that chat is not running here`); a kept row has `open here`, `open beside`, `resume (may continue the last turn)`, `copy id`. `fork (latest)` (in every chat menu, saved ones and shared-folder ones too) copies the whole chat to a NEW chat and opens it in a pane beside the original with a line `forked from <title>`; the original keeps going. There is no `fork from here` on transcript rows: eidolon has no headless way to move a copy's head to an older record (see hub.md 7c), so the item is not offered. In a pane's split and show menus, `existing session` lists every live session and `resume…` the newest 15 kept; a pick only opens a pane (resume stays the pane's own banner action). `new session` is live: see below.

The cap: at most 4 distinct sessions hold an event stream at once (3 while the terminal stream is open: `extraStreams` in `core/registry.js` says how many other streams there are). Only panes that hold a stream count: a pane that reads stopped, kept, dead, gone, refused or goodbye holds none. Opening a fifth is refused with a toast, and so is resuming a fifth; a mirror of an open session is always allowed. Door mode is unchanged (the old greyed lines).

The browser's limit is six connections per host:port for the whole profile, not per tab (HTTP/1.1). The 4 is per tab. Two tabs with 4 sessions each ask for 8 streams: the browser queues the 7th and 8th, and also every later request to the perch behind them (an answer, a say, the poll). The symptom is a `y` that does nothing and no error until a stream closes. Keep the total of followed sessions across all tabs of the profile at 5 or fewer (the sixth connection is the poll and the posts); the page cannot see other tabs.

## Layout

```
index.html           the screen skeleton; one <script type=module src>, no inline script (CSP)
theme.css            base16 slots -> the page's colour variables, the default scheme as fallback, @font-face (web-ui.md 2.2)
term.css             the look: square frames, cut-in titles, the TUI prompt frame, no border-radius
app.js               sidebars, split tree, inspector tabs (INSPECTOR map, the loaded tab and its registered sections), status line, the ask queue and batch submit, menus' items, keys, the : line
core/ui.js           the one import point for Preact, hooks, signals, htm; K = clickable text
core/menu.js         right-click / title menus: one open at a time, submenus, keys, kept on screen
core/look.js         theme + font: base16 parser, --base0X on <html>, per-font size, cell measured, localStorage
core/path.js         resolveIn(cwd, path): a tool-input path resolved against the session's folder, for the tool-row open items; toWslPath(raw): a pasted Windows path as a WSL one
core/api.js          apiBase(sessionId): the ONE place URLs are built. Hub = `/s/${sid}/api/`; hubGet/hubPost/hubTree (`/hub/sessions...`, `/hub/tree?...`) and detect() (perch or door)
core/hub.js          the perch's session list as signals: polled GET /hub/sessions, stale flag, kept/live/elsewhere/older views, age(), label() (the title)
panes/imgs.js        images: thumbnails (`<img src>` from the perch's image route), the full-size viewer (click; Esc or click closes; `open in new tab`), the local preview of an image this tab just sent, `fileUrl`/`viewFile` (an image file in the chat's folder, `GET /hub/file`) and `linkPaths` (image paths in reply text become clicks)
core/saved.js        a saved chat: eidolon's `log --json` lines mapped to the frames a live door sends, drawn by the Session's own handlers; a jev run's Workflow records become start/end lines, script calls and the park's ask with its answer (an unknown record kind is one plain line naming it)
core/sse.js          fetch-based SSE (Bearer header, never ?token=)
core/state.js        one session stream as signals: frames -> rows, calls, asks, running; the reconnect backoff, resume, launch
(core/ and panes/ each have a README with one line per file)
core/life.js         port of eidolon crates/tui/src/life.rs (the Life strip)
core/markdown.js     safe subset as vnodes: fences, headings, `code`, **bold**, http(s) links
core/tile.js         split tree: dwindle at 0.618, per-split ratio, drag, promote on close
panes/home.js        the start screen: [ new chat ], [ new project… ], the saved projects, recent chats, the hint row
panes/picker.js      the folder picker panel (GET/POST /hub/dirs) and the project-name step
core/projects.js     the project list (GET/POST /hub/projects, DELETE /hub/projects {path}), which project a folder is in, scratch folders
panes/session.js     one pane: frame glyphs, transcript, start screen, row menus, draft, the outbox (queued messages), composer and its menus
panes/asks.js        the tabbed ask panel (batch submit) and the transcript's ask markers
panes/files.js       the inspector's files tab: `folder` (the session's directory, lazily, through GET /hub/tree) (an image file opens the viewer on GET /hub/file) and `touched` (files edit/write calls touched), one / filter
core/attach.js       images and text files put into a message, checked against the door's limits
core/syntax.js       port of eidolon crates/tui/src/syntax.rs: code with line numbers, diffs
renderers/           generic + files (edit/write diff, read as code) + jev (jev_choose bars with the best and the margin, jev_entail scores, jev_mark lines; open by default) tool renderers; a renderer returns a string, a DOM Node or a vnode
core/registry.js     the lists a feature folder registers into (tabs, pane kinds, commands, key rows, owners, frame taps, stream slots) and `host`: app.js draws from them, so a feature never edits app.js
graph/               the inspector's graph tab: the jev runs and the chat's turn graph (GraphTab)
mesh/                the left panel's mesh tab: the nodes this machine knows (MeshTab)
terms/               the web terminals: the terminals tab in the right panel and the terminal panes
notes/               the notepad: the notes tab in the left panel and the note pane
allow/               the allow rules of a chat: a section of the loaded tab
tests/               node tests of the pure modules: node --test "webui/term/tests/*.test.mjs" (tests/README.md)
brand/owl.svg        copy of webui/owl-green.svg (the door serves only this dir)
themes/              base16 schemes (*.yaml) + manifest.json, the list the page offers
fonts/               VT323, Departure Mono (+ OFL licences, VERSIONS with sources and sha256)
vendor/              preact, hooks, signals, signals-core, htm + LICENSE-* + VERSIONS + update.sh
```

## Mouse first

Every action is clickable; keys are shortcuts for the same functions.

| Where | Click does |
|---|---|
| composer bottom border, left | menus: `+ attach ▾` (image… / file…), and under the perch `model <key> ▾` (the list from `GET /hub/models`, the current one ticked, `other…` for a typed name) and `mode manual|auto|yolo ▾` (`manual` asks before anything that changes things, `auto` is eidolon's rules and asks only for risky commands, `yolo` never asks; the tick is the chat's mode, from the door's `yolo` for yolo and the perch's list row for manual and auto; a chat another process started shows `mode gated`, meaning unknown). A switch is `POST /hub/sessions/<id>/restart`: the perch stops the door and resumes the same log with the new `-m` / mode (`POST .../restart {mode}`), only between turns (while a turn runs the items say `wait for the turn to end`); the pane stays on its id, reconnects and replays, and the transcript gets one line, `switched to <model> - the chat restarted between turns, nothing lost`. The composer shows what the door's `hello` reports, so the labels are the real current values. There is no effort menu (`eidolon web` has no such flag) and no mic (nothing here records audio); a bare door, which cannot restart itself, shows `+ attach` only. Narrow, they drop to glyphs, then into one `⋯ ▾` |
| composer bottom border, right | `send · steer · queue · stop` |
| an image chip's `×` | drop that image from the next message |
| `? keys` in the status line | the key reference; every entry is clickable |
| the model, persona or the mode (`YOLO`, `manual`, `auto`) in the status line | the inspector's `loaded` tab (what the door has loaded) |
| `N waiting` in the status line | focus the oldest ask (`▶ N waiting`: it is holding until you pause typing) |
| a pane's title (click or right-click) | menu: split right ▸, split down ▸, show… ▸ (sessions, current ticked), mirror, reconnect, close |
| `+` `×` on a pane's top border | split: a menu of split right ▸ / split down ▸ (each the picker: mirror; new, resume, existing session need the hub) / close |
| right-click a chat row | copy (the selection, if any), copy as markdown, quote into composer |
| right-click a sidebar session | open in new pane, show in focused pane, fork (latest), stop this chat, pin, archive, delete |
| right-click the status line | loaded (the inspector tab), theme ▸, font ▸ (live, current ticked), then every `:` command |
| a rail icon (left `≡ ▦ ⋈ ✎`, right `◉ ▣ ▤ ± ⋔ ▶ ⌨`) | open that side's panel on that tab; the lit icon closes it |
| `«` / `»` (the outer end of an open panel's title only; never in the rail) | collapse or expand that side's panel |
| tile borders | drag to resize (snaps to cells) |
| an ask tab | switch to that ask (the panel moves to its pane) |
| an ask row | one click records the answer (✓ on the tab, nothing sent); `submit N answers` sends them all once every tab is answered; a lone ask is sent at once. Rows: `yes · no · always (when offered) · + note · chat about it` (approval), `1 … n · + note · chat about it` (question); `+ note` opens the note line and sends nothing; the answer taken next carries the note. `chat about it` acts at once. A click within 0.3 s of the ask appearing or jumping into view shows `steady…` and does nothing |
| a settled ask (`▸ approve: … → yes`) | open or fold its choices, the pick marked; focus stays where it was |

Menus: up/down move, Enter or right opens a submenu, Enter runs, left or Esc backs out, Tab closes; hover selects and opens submenus. A click outside closes; a second click on the title that opened one closes it. A menu that would run off an edge opens on the other side of its anchor. Greyed lines say why (`needs the hub`, `needs eidolon PR`).

### The start screen (perch only)

Shown whenever no pane is open, and on boot when no chat is live (`panes/home.js`). Visible bracketed text actions, focusable in order with Tab and run with Enter or a click (2026-10-05 ruling: they are buttons in function, TUI in look; keys and `:` stay as shortcuts). `[ new chat ]` has focus at boot, so Enter alone starts a chat.

| action | does |
|---|---|
| `[ new chat ]` | `POST /hub/sessions` with no folder, never the focused pane's (the perch answers 201 with a fresh scratch folder, or 400 when it has no Minerva home: the page says so in a sentence). No model is sent: the default is the Minerva config's. A bare `:new` does the same |
| `[ new project… ]` | the folder picker (below), then a name field prefilled with the folder's name; `[ create project ]` saves it (`POST /hub/projects`) and starts a chat in it |
| a saved project | `+ <name>` starts a chat in it; `latest <age>` opens its newest chat; right-click removes it from the list (the folder is untouched) |
| recent chats | up to 8 running or saved chats, newest first; a click shows it in the pane, ctrl-click or middle-click opens it beside |

A hint row names `ctrl+enter` (the new-pane picker), `?` and `:`.

### The folder picker and projects (2026-10-06)

A panel over the tiles, drawn like the rest (frame, bracketed text actions, Tab and Enter work, Esc closes): a path bar (it takes a WSL path, a pasted `C:\Users\you\work` or `\\wsl.localhost\...` path, converted by `core/path.js`, and `~`), a breadcrumb, `↑ up`, one clickable row per subfolder, `[ new folder ]` (an inline name field, `POST /hub/dirs`, then it opens the new folder), `[ use this folder ]` and `[ cancel ]`. It starts at the folders the server may open and cannot leave them (the server decides; a refusal is said in a sentence).

The sidebar's `sessions` tab, top to bottom: a `search` field (filters by title); `+ new chat` (a fresh scratch folder with a three-word birth name); `pinned` (only when something is pinned; those chats are in no other group, and the row names the project faintly); one group per project, **alphabetical**, its header the name with `+` at the right end (a new chat in it), its chats newest first (8, then `more… (N)`); `+ new project`; `inbox` (scratch and unplaced chats, with its own `+`); `in other apps`; `other (N)` (collapsed: chats archived by hand, chats idle for 7 days, and eidolon's own sessions folder, with a faint `archived` / `idle` tag; a live chat is never in it); and, only when non-empty, a collapsed `trash (N)` at the very bottom (`restore` per row, `empty trash` with one confirm). A chat row's right-click menu, and the `⋯` at its right edge (on hover or focus), add `pin`/`unpin`, `archive`/`unarchive` and `delete` (to the trash; `stop and delete` while running) to the open items; eidolon's own folder's chats keep only open/continue. The start screen's recent chats leave out `other` and the trash and lead with pinned chats. A row is a glyph (● live, ○ saved; bold while a turn runs, `?` for an ask waiting, `[n]` unread), the title clipped with …, and the age at the right; the focused chat's row has a full-row tint. A project header's right-click menu: `new chat here`, `remove from the list`. Scratch folder names (time stamps) are never shown: the pane frame shows the title and, in a project, `[<project>]`; `new chat · HH:MM` is a scratch chat with no message yet.

### Starting a session (perch only)

`POST /hub/sessions {cwd, model?}`. The perch waits for the door's hello (up to about 20 s), so the hint line says `starting a session in <~folder>…` meanwhile and a second start in the same folder does nothing. A page load never starts one; only an explicit pick does.

| how | does |
|---|---|
| `:new <folder> [model] [manual|auto|yolo]` | start there (a bare `:new`: a chat with no folder, as `[ new chat ]`); a last word that is a mode sets the approval mode of this chat, otherwise the home's `default_mode` (yolo when it names none) applies. A folder with a space goes in quotes: `:new "/home/u/My Project" chatgpt:gpt-5.6-luna` (double or single); more than two unquoted words is refused. `~/x` expands from the home prefix of any known cwd (`/home/<user>`); with none known, or a relative folder, the toast is `give an absolute folder`. `:new` alone opens the `:` line prefilled with the focused session's folder |
| the `+` on a project group | a session in that group's folder (greyed in door mode) |
| split / show menu, `new session ▸` | the folders of open and live sessions (max 8), then `other folder…` (the `:new` line). A pick opens the session in the new split, or in this pane (show; the queued-message guard applies) |
| ctrl+enter | the same menu as split (a pick there splits: it is the explicit path) (`mirror`, `new session`, `resume…`, `existing session`) under the focused pane, along its longer side; in the middle with no pane. Door mode keeps its toast |

Model: `:new` takes it as the second word; otherwise the focused session's model, else none (the door's default). The menus say which they will use (`default model` when none). The 4-session cap is checked before the POST. Answers: 422 `<folder> is not under the perch's --root (default $HOME)`; 503 `the door did not come up: <reason>`; anything else toasts the status.

## Keys

| key | does |
|---|---|
| Enter / shift+Enter | send (idle) or steer (running) / newline |
| alt+Enter | queue behind the running turn: held in the pane, editable until sent (below) |
| ↑ | in an empty composer: edit the last queued message (⏎ saves it back in place, Esc leaves it) |
| Esc | leave the composer (`i` or Enter goes back); in the ask panel, back to the composer, the asks stay queued |
| Tab / shift+Tab | from the composer: the oldest waiting ask. In the panel: Tab opens the note line (Tab / ↑ / ↓ back to the list); shift+Tab, the previous tab |
| ↑ ↓ / k j | in the panel: move the highlight (clamped; greyed rows skipped) |
| ← → / h l | in the panel: previous / next ask tab, across panes |
| Enter | in the panel: take the highlighted row (records it; on the last unanswered tab it also submits). On `+ note`: open the note line |
| y / n | yes / no, recorded (with the note if open). Never `always`; `n` does nothing when there is no plain no. Text in the note line |
| c / s | chat about it (at once) / submit every answer (only when all are answered) |
| 1-9 | in a question: that option at once |
| ctrl+c | stop the turn (with an empty or unfocused composer) |
| ctrl+b / ctrl+i | collapse / expand the sidebar / the inspector (both closed = compact: only the rails) |
| [ / ] | inspector tab |
| alt+w or `:q` | close the pane (not while the composer holds text; ctrl+w belongs to the browser). Matched on the `w` character, so AZERTY works; on the physical W key only when the character is not ASCII (macOS Option) |
| `r` `r` | on a stopped, dead or kept pane: the first press asks, the second within 3 s resumes (it may continue the last turn); clicking `resume` or `:resume` does it at once |
| `ctrl+enter` | the new-pane picker (perch) |
| `:` | command line: q, close, open [id], new <folder> [model] (quote a folder with a space), resume, vsplit (mirror right), split (mirror below), stop, queue, submit, reconnect, sidebar, inspector, keys, theme [name], font [name] |
| `?` | the key reference panel; every entry can be clicked |

## Terminal, editor, files

`POST launch {window: "terminal"|"editor"|"files", path?}` (the door's own route, through the perch's `/s/<id>/api/launch` too). It opens a real window on the machine that runs the door, which is the browser's own machine (the door is loopback only). It is the operator's act, not a tool call, and the page builds no terminal or editor of its own.

| where | does |
|---|---|
| pane title menu, `open ▸` | `terminal here`, `editor here`, `files here`: the session's folder (no path sent) |
| status line menu (right-click), `open ▸` | the same, for the focused pane |
| a tool row's right-click, when its input has a string `path` or `file_path` | `open in editor` (`editor` with that path), `open folder` (`files` with that path). The path is the model's, so the page resolves it against the session's folder (`core/path.js`: `.` and `..` folded, a relative path joined to the folder) and offers the items only when it lands inside that folder. What is sent is the resolved absolute path, shown whole as the item's hint and title and again in the hint line when you pick it. Outside the folder, with a `~`, with control or direction characters, or while the folder is not known yet, the items are greyed with the reason. Symlinks inside the folder are not followed by the page. Rows without a path do not show them |
| `:term`, `:edit [path]`, `:files [path]` | the same on the focused pane; the `:` line's key reference lists them |

**The operator's risk:** the launcher (the door's `[terminal]` config) decides what a path means. A config that puts `{path}` or `{dir}` into a shell string (for example `files = ["nu", "-e", "ya \"{path}\""]`, or a terminal that parses `;` itself) can be made to run commands by a file or folder name inside the session's folder, which the model can create. Keep `{path}` and `{dir}` as separate argv elements, never inside a string a shell or another command parser will read. The page's check narrows what is offered; it cannot make an unsafe launcher config safe.

A goodbye'd, stopped, dead or kept session greys the items with `not running` (no door to ask). Answers toast as: 200 `opened: <command line>` (cut at 120 chars); 501 `this server has no launcher configured`; 422 `launch refused: <reason>`; 500 `the launcher failed: <reason>`. A path over 4096 bytes or with a NUL is refused here with the door's own words, and no request is sent.

## The files tab

Two sections under one `/` filter. `folder` (perch mode only) lists the session's working directory through `GET /hub/tree?session=<id>&path=<rel>`: the root when the tab is first shown for that session, then one directory at a time on its first unfold (`▸` to `▾`, `…` while loading). Listings are cached per session and path for the life of the page; `refresh` on the header drops that session's cache. Files show a size, links show `name ->` dim, anything else dim; `(first 2000)` and `(N names not shown)` come from the perch. Answers: 409 `the folder lists only while the session runs (<state>)`; 404 `not found` (missing and denied look the same); 401 the refused-token text; 0 `perch not answering`; anything else (429, 503, 5xx) a dim `busy, retry` line you can click. Door mode: `the folder needs the perch`. `touched` is the files this session's edit and write calls touched, as before. The `/` filter filters what is loaded in both; it fetches nothing.

## Attachments

Images: `+ attach ▾ → image…`, a paste, or a drop on the composer. PNG, JPEG, GIF or WebP (by their bytes), 4 MiB each, 8 per message, 8 MiB in all: the door's limits (eidolon `crates/web/src/attach.rs`), checked here first with the reason in the hint line. They show as `[img] name ×` chips above the text and go in the say's `images` field. Text files (`file…` or a drop): inline at the caret as a fenced block named for the file, 256 KiB at most, UTF-8, binaries refused. The door takes no other kind of file.

## Queued messages

⌥⏎ while a turn runs holds the message in the pane, not on the door (the door's queue has no route to edit or drop one). They show as a stack above the composer: the text (click: whole), `edit` (into the composer; ⏎ puts it back in its place, Esc leaves it), `steer ↑` (send it now, into the running turn), `×` (drop). When the turn ends the first is sent; the next waits for that turn to end. A reconnect keeps them; F5 does not. Showing another session in the pane asks before dropping them. Asks never block the composer: typing, ⏎ and ⌥⏎ work while asks wait. The status line says `N queued`.

## Asks

Every pending ask on the page, across all panes and sessions, is ONE tab in ONE panel, oldest first, docked above the composer of the pane that owns the current tab (the focused pane if it shows that session). The status line shows `ask i/n` and `N waiting`. Design: web-ui.md 7.1.

- **Batch submit:** taking a row records the answer (✓ on its tab, `✓ yes · not sent` in the transcript marker) and moves to the next unanswered tab. Answers can be changed until sent. `submit N answers` (row, `s`, `:submit`) is live only when all are answered (`2 of 3 answered` until then) and sends them back to back in queue order. Enter on the last unanswered tab submits; y/n/digits/click there only record and move the highlight to submit. A lone ask is sent as soon as it is answered. In code: `Session.choose` records, `Session.send` posts, `app.js submitAsks` batches.
- A sent tab stays (✓, read-only, `sent, waiting for the door`) until its `ask-settled`. One batch at a time. The first refusal stops the batch (`sent 1 of 3; ask 2 refused: …`), clears that answer and focuses its tab; a 409 or 404 folds it as answered elsewhere and the batch goes on. Recorded answers survive a reconnect. The status line's `N waiting` and the `ASK` badge count asks not yet sent.
- An ask that took focus by itself is **unarmed**: y/n/c/s/Enter/digits do nothing until an arrow, Tab or a click; any other typed key (Backspace too) goes back into the composer at its caret, with focus, and the ask does not take focus again. Letters, j/k/h/l included, never arm it.
- The oldest unanswered ask takes focus by itself, once per session + ask id (a replay does not take it again), and only when nothing holding text has focus; with the composer empty, after 1 s without keys and no menu or `:` line open. Meanwhile the status line says `▶ N waiting`; Tab, a click on it, a tab or a transcript marker go there. Keys do nothing for 0.3 s after focus lands or the tab changes; clicks within 0.3 s of the strip changing say `steady…`; IME composition keys and the second click of a double click are ignored.
- Focus goes back to the composer it came from, caret kept, only when no tab is left. Esc leaves the asks queued; none takes focus by itself until you go back.
- Rows: `always` only if the approval's `answers` list it (pending eidolon PR, gap 13), never what y or n sends; `no` greyed when there is no plain no. No free-text answer: your own words go through `chat about it`, which answers no (when there is one), cancels the turn (`POST /api/cancel`, also cancelling its other asks) and quotes the ask into the composer.
- The note line is optional: Tab or `+ note`. Esc in it drops the note. Notes are live (eidolon #18): `POST /api/answer {"ask_id","answer","note"}`. The line stops at 1024 bytes of UTF-8 (the door's limit; `n/1024` shows dim past 900); a note of only whitespace is no note. A 422 shows the door's own reason (`the door refused the note: …`), clears the choice and keeps notes on; only a 400 on an answer with a note (a door from before #18 refusing the unknown field) turns notes off for the session.
- A settled ask folds to one line, `▸ approve: bash rm -rf ./build → no (note: …)`; opened, it lists every choice with the pick marked. The note is the door's own (`ask-settled` carries it), so a note sent from a mirror, another tab or curl shows too. Asks are not replayed once settled, so after F5 the fold is gone; a still-pending ask is replayed.

## Themes and fonts

The default is `phosphor` in VT323. Both switch live, from the status line's right-click menu or the `:` line, and are kept in `localStorage` (the page works without it). Web-ui.md 2.2 has the slot table, contrast numbers and font sizes.

- **A new theme:** drop any standard base16 scheme into `themes/` as `<name>.yaml` and add `"<name>"` to `themes/manifest.json`; `:theme <name>` or the menu, no reload. The old layout (`scheme:`, `author:`, `base00: "1d2021"`, with or without `#`, quoted or not, a trailing `# comment` allowed) and the tinted-theming layout (`name:`, `palette:` with the slots indented) both parse. Anything else is refused with the line number in the hint line, and the current theme stays.
- **The rule for colours:** chrome (frames, sidebars, inspector, status line, menus) takes only base00-07, plus red/orange/yellow for error, ask waiting, warning. In the transcript each role has its own slot: your prompt blue, tool names magenta, a call's command and code yellow, verdict labels orange, call ok green, error and deny red, links, paths and notice labels cyan, thinking and notices dim. A new style should use the named variables in `theme.css` (`--fg`, `--faint`, `--bright`, `--panel`, `--magenta`, ...), never a hex value.
- **Fonts:** `:font vt323 | departure-mono | system` (a unique prefix does). Each font has its own size in `FONTS` (`core/look.js`); `--cw`/`--ch` are measured from the font, so the tiles snap to its cell. A new font: its file and licence in `fonts/`, an `@font-face` in `theme.css`, an entry in `FONTS`, a line in `fonts/VERSIONS`.

## Vendoring

`vendor/` holds four npm packages as single ESM files, pinned (version and tarball sha512) in `vendor/VERSIONS`:

| file | package |
|---|---|
| preact.js, hooks.js | preact 10.29.8 |
| signals-core.js | @preact/signals-core 1.14.4 |
| signals.js | @preact/signals 2.11.2 |
| htm.js | htm 3.1.1 |

`sh vendor/update.sh` re-vendors exactly what `VERSIONS` pins; `sh vendor/update.sh --bump <pkg> <version>` is the only way to move a pin. Needs sh, curl, tar, sed, openssl, base64; no node, no npm. For each package the script:

1. fetches the version's metadata from registry.npmjs.org, for the tarball URL only, and refuses any tarball URL not under `https://registry.npmjs.org/`;
2. downloads the tarball and stops unless its sha512 equals the hash **pinned in `VERSIONS`** (the registry is not trusted for a pinned version). `--bump` checks the new version's tarball against the registry's `dist.integrity` once, and writes that hash as the new pin;
3. copies the one ESM file the page needs and rewrites the bare imports (`preact`, `preact/hooks`, `@preact/signals-core`) to `./x.js`, because an importmap would be an inline script, which the door's CSP forbids;
4. fails if any bare specifier is left, static (`from "x"`) or dynamic (`import("x")`), then writes the files and the licences.

To upgrade: `--bump`, read the diff, test, commit. Never edit a vendored file or `VERSIONS` by hand.

The fonts are pinned the same way by hand in `fonts/VERSIONS`: source URL, tag or commit, sha256 of each file (`VT323-Regular.ttf` from google/fonts, `DepartureMono-Regular.woff2` from the v1.500 release zip).

## Rules the code keeps

- No `innerHTML`, `dangerouslySetInnerHTML` or `eval`. Model and tool text is only ever a text child.
- No `<button>`: controls are `K` (a `span role=button tabindex=0`).
- `border-radius: 0` on everything, `<meta name=referrer content=no-referrer>` stays.
- Streamed text is appended to one text node (O(n)); rows are memoised and read only their own signals.
- An inspector tab is a component taking `{ s, sid }` (`INSPECTOR` in app.js; a feature's own tab registers in `core/registry.js` and app.js is not edited); the M3 graph goes in the `graph` slot. Panes that are not chats (`ui.xpanes`) share the tile tree with the chat panes; the ~30 places that read `p.s` / `p.sid` stay on the chat panes (`ui.panes`).
- One `Session` (one `/api/events` stream) per session id; panes, mirrors included, are views of it. Only `turn-state` sets running.
- A pane's session can change (`showIn` in app.js): the pane is keyed by id and session id, so it is drawn afresh, and the old `Session` closes when no pane shows it. A pane is a mirror when an earlier pane shows its session.

## Known limits

- **Firefox scrollbars are not square.** The square scrollbars in `term.css` use `::-webkit-scrollbar`, which Firefox ignores; Firefox only offers `scrollbar-width` and `scrollbar-color`, and neither can change the thumb's shape, so Firefox draws its own rounded overlay scrollbars. There is no CSS fix; it is left as is.
- **A submenu on a narrow screen covers its parent.** With no room on either side it is pushed back on screen over the parent menu; the keys (left backs out) still reach both.
- **Text in a settled ask's fold cannot be drag-selected**: a click there must not take focus from the composer. Right-click the row: copy.
- **Some glyphs are off the cell in VT323 and Departure Mono.** Neither has braille or `▸ ▾ ● ⇥ ⏎ ✓`, and VT323 has no arrows or box-drawing either; those fall back to the system font, whose advance differs, so the Life strip and a few glyphs (`▸ ⇥ ⏎`) are not exactly one cell wide.

## Chat names, saved chats and older chats (2026-10-05)

- **Rename and promote.** A chat's `⋯` has `rename…` (inline edit: Enter saves, Esc cancels; `POST /hub/sessions/<id>/rename`; an inbox chat's folder follows the name between turns) and, for inbox chats, `make it a project…` (the folder picker in parent mode: choose where the folder goes, then its name, prefilled from the title; `POST /hub/sessions/<id>/promote`). A project header's `⋯` has `rename…` (`POST /hub/projects/rename`). `:rename <name>` does the focused chat's. A pane whose door the perch restarted (folder rename, promote) reconnects by itself once.
- **Names.** A chat is shown by the `title` the perch gives it (a custom name from `rename…`, else its first message, else the birth name or `<folder> · HH:MM`; never a slug or a stamp) in the sidebar, the start screen's recent chats, the pane frame and the menus; the id is on hover. Door mode (no perch) keeps the id.
- **Images are viewable.** A row that has images (`[N images]` before) draws thumbnails (up to 16 lines tall, a 1px frame) loaded from `GET /hub/sessions/<id>/image/<record>/<n>`; a click opens the picture fitted to the screen, Esc or a click closes it, and `open in new tab` goes to the raw route. An image you just attached shows from the local data at once and swaps to the route when it answers. A tool call whose result carries images (none do in eidolon's log today) gets them under its row, folded behind `▸ N images`. A saved chat reads `GET /hub/sessions/<id>/transcript` (the log with the bytes emptied) so pictures never push it over the size cap. Under a bare door there is no image route: a fresh local preview shows, anything older shows `[name]`.
- **Images a tool wrote are viewable (2026-10-06).** eidolon's tool results are text, so a "produced" image is a file a tool wrote into the chat's folder. The files panel's `folder` section shows an image file (png, jpg, jpeg, gif, webp) with a `◫` before its name; a click opens the same viewer as a chat image, on `GET /hub/file?session=<id>&path=<rel>` (`open in new tab` goes to that URL; a refusal shows `(could not load)` in the viewer's bar). In a reply's text, a path that ends in one of those extensions and resolves (core/path.js `resolveIn`) inside the chat's folder is drawn as a cyan click that opens the viewer; paths outside the folder, `~` paths, svg and anything in a code fence stay plain text. Nothing is fetched until the click. The page's reading of the folder is only a guess: the perch checks the path again (deny list, links, type, 20 MiB) and answers 404, 415 or 413. No thumbnail on hover (skipped: it would fetch on a hover).
- **A saved chat is read, not resumed.** Opening one (a click, a recent chat, `:open`) fetches `GET /hub/sessions/<id>/log`, maps the records to rows (`core/saved.js`; the day row, times and tool rows are the live ones) and draws them without a composer. In its place is the visible action `[ continue this chat ]`, which is the resume route and swaps the pane to the live chat; nothing opens a chat by itself. A stopped or exited chat shows the same action. Too large (413), unreadable or slow logs say so in the banner; the action still works.
- **Older chats.** The perch's second source (eidolon's own sessions folder, `origin: "other"`) is listed only under a collapsed `older chats (N)` group at the bottom of the sessions tab (15 at a time, `more…`), and nowhere else: not in `saved`, the start screen or the resume pickers. A click opens one read-only; its `[ continue this chat ]` carries a one-line note that continuing resumes it with your normal config and writes to the same log.

## Activity rails (2026-10-06)

Both sides have an always-visible 3-cell rail, as an editor's activity bar: one icon per tab, with nothing above them (the collapse glyph lives only in an open panel's title; a closed panel reopens from its rail icons or ctrl+b / ctrl+i). Left: sessions `≡`, projects `▦`, mesh `⋈`. Right: rec `◉`, loaded `▣`, files `▤`, diff `±`, graph `⋔`, run `▶`. The icons live in one table, `RAIL_ICONS` in `app.js`. The open tab's icon is lit; tooltips read `name · shortcut`. The panel's frame title is just the tab's name plus the collapse glyph. Open/closed and the chosen tab per side are kept in `localStorage` (`minerva.rails`); the page works without it. Under 110 cells wide an open panel overlays the tiles (opening one closes the other) and both start closed.

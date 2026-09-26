# webui/term: the Minerva web UI, milestone M1

A terminal screen in a browser (design: `docs/design/web-ui.md`). Preact + `@preact/signals` + `htm`, vendored as plain ES modules under `vendor/`. There is no build step, no npm at runtime and no CDN. After you edit a file, press F5.

## Run it

The hub doesn't exist yet, so M1 talks to one `eidolon web` door directly (from WSL on Windows):

```sh
eidolon web --bind 127.0.0.1:47811 --ui-dir /mnt/c/Users/<you>/Projects/Minerva/webui/term
# prints:  listening on http://127.0.0.1:47811/
#          token file: /run/user/<uid>/eidolon/web-47811.token
```

Open `http://127.0.0.1:47811/#token=<contents of the token file>`. The page moves the token into `sessionStorage` and strips it from the URL (`#token=` and `?token=` both), so F5 keeps working in that tab. A new tab needs the fragment again. The token only ever leaves in an `Authorization` header. Once the hub exists, the hub's redirect file does this step (web-ui.md 5.2).

## Layout

```
index.html           the screen skeleton; one <script type=module src>, no inline script (CSP)
theme.css            base16 slots -> the page's colour variables, the default scheme as fallback, @font-face (web-ui.md 2.2)
term.css             the look: square frames, cut-in titles, the TUI prompt frame, no border-radius
app.js               sidebars, split tree, inspector tabs (INSPECTOR map, the loaded tab), status line, the ask queue and batch submit, menus' items, keys, the : line
core/ui.js           the one import point for Preact, hooks, signals, htm; K = clickable text
core/menu.js         right-click / title menus: one open at a time, submenus, keys, kept on screen
core/look.js         theme + font: base16 parser, --base0X on <html>, per-font size, cell measured, localStorage
core/api.js          apiBase(sessionId): the ONE place URLs are built. Hub = `/s/${sid}/api/`
core/sse.js          fetch-based SSE (Bearer header, never ?token=)
core/state.js        one session stream as signals: frames -> rows, calls, asks, running
core/life.js         port of eidolon crates/tui/src/life.rs (the Life strip)
core/markdown.js     safe subset as vnodes: fences, headings, `code`, **bold**, http(s) links
core/tile.js         split tree: dwindle at 0.618, per-split ratio, drag, promote on close
panes/session.js     one pane: frame glyphs, transcript, start screen, row menus, draft, the outbox (queued messages), composer and its menus
panes/asks.js        the tabbed ask panel (batch submit) and the transcript's ask markers
panes/files.js       the inspector's files tab (the files this session touched, a / filter)
core/attach.js       images and text files put into a message, checked against the door's limits
core/syntax.js       port of eidolon crates/tui/src/syntax.rs: code with line numbers, diffs
renderers/           generic + files (edit/write diff, read as code) tool renderers; a renderer returns a string, a DOM Node or a vnode
brand/owl.svg        copy of webui/owl-green.svg (the door serves only this dir)
themes/              base16 schemes (*.yaml) + manifest.json, the list the page offers
fonts/               VT323, Departure Mono (+ OFL licences, VERSIONS with sources and sha256)
vendor/              preact, hooks, signals, signals-core, htm + LICENSE-* + VERSIONS + update.sh
```

## Mouse first

Every action is clickable; keys are shortcuts for the same functions.

| Where | Click does |
|---|---|
| composer bottom border, left | menus: `+ attach ▾` (image… / file…; folder… needs the hub), `model ▾`, `effort ▾`, `mode ▾` (switching needs the hub: the door restarts between turns), `◉ mic` (needs local dictation). Narrow, they drop to glyphs, then into one `⋯ ▾` |
| composer bottom border, right | `send · steer · queue · stop` |
| an image chip's `×` | drop that image from the next message |
| `? keys` in the status line | the key reference; every entry is clickable |
| the model, persona or YOLO in the status line | the inspector's `loaded` tab (what the door has loaded) |
| `N waiting` in the status line | focus the oldest ask (`▶ N waiting`: it is holding until you pause typing) |
| a pane's title (click or right-click) | menu: split right ▸, split down ▸, show… ▸ (sessions, current ticked), mirror, reconnect, close |
| `⇥` `⇩` `×` on a pane's top border | split right / split down (the picker: mirror; new, resume, existing session need the hub) / close |
| right-click a chat row | fork from here (needs the hub), copy (the selection, if any), copy as markdown, quote into composer |
| right-click a sidebar session | open in new pane, show in focused pane, fork (latest) and close its door (need the hub) |
| right-click the status line | loaded (the inspector tab), theme ▸, font ▸ (live, current ticked), then every `:` command |
| sidebar tabs, `«` / `»`, rails | switch tab, fold to a rail, reopen |
| tile borders | drag to resize (snaps to cells) |
| an ask tab | switch to that ask (the panel moves to its pane) |
| an ask row | one click records the answer (✓ on the tab, nothing sent); `submit N answers` sends them all once every tab is answered; a lone ask is sent at once. Rows: `yes · no · always (when offered) · + note · chat about it` (approval), `1 … n · + note · chat about it` (question); `+ note` opens the note line and sends nothing; the answer taken next carries the note. `chat about it` acts at once. A click within 0.3 s of the ask appearing or jumping into view shows `steady…` and does nothing |
| a settled ask (`▸ approve: … → yes`) | open or fold its choices, the pick marked; focus stays where it was |

Menus: up/down move, Enter or right opens a submenu, Enter runs, left or Esc backs out, Tab closes; hover selects and opens submenus. A click outside closes; a second click on the title that opened one closes it. A menu that would run off an edge opens on the other side of its anchor. Greyed lines say why (`needs the hub`, `needs eidolon PR`).

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
| ctrl+b / ctrl+i | sidebar / inspector to rails and back |
| [ / ] | inspector tab |
| alt+w or `:q` | close the pane (not while the composer holds text; ctrl+w belongs to the browser). Matched on the `w` character, so AZERTY works; on the physical W key only when the character is not ASCII (macOS Option) |
| `:` | command line: q, close, open [id], vsplit (mirror right), split (mirror below), stop, queue, submit, reconnect, sidebar, inspector, keys, theme [name], font [name] |
| `?` | the key reference panel; every entry can be clicked |

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
- The note line is optional: Tab or `+ note`. Esc in it drops the note. The wire: `POST /api/answer {"ask_id","answer","note"}` (pending eidolon PR). A 400/422 shows the refusal, the note line goes read-only, and the next answer goes without it. A door that ignores unknown fields drops the note silently.
- A settled ask folds to one line, `▸ approve: bash rm -rf ./build → no (note: …)`; opened, it lists every choice with the pick marked. Asks are not replayed, so after F5 the fold is gone.

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
- An inspector tab is a component taking `{ s, sid }` (`INSPECTOR` in app.js); the M3 graph goes in the `graph` slot.
- One `Session` (one `/api/events` stream) per session id; panes, mirrors included, are views of it. Only `turn-state` sets running.
- A pane's session can change (`showIn` in app.js): the pane is keyed by id and session id, so it is drawn afresh, and the old `Session` closes when no pane shows it. A pane is a mirror when an earlier pane shows its session.

## Known limits

- **Firefox scrollbars are not square.** The square scrollbars in `term.css` use `::-webkit-scrollbar`, which Firefox ignores; Firefox only offers `scrollbar-width` and `scrollbar-color`, and neither can change the thumb's shape, so Firefox draws its own rounded overlay scrollbars. There is no CSS fix; it is left as is.
- **A submenu on a narrow screen covers its parent.** With no room on either side it is pushed back on screen over the parent menu; the keys (left backs out) still reach both.
- **Text in a settled ask's fold cannot be drag-selected**: a click there must not take focus from the composer. Right-click the row: copy.
- **Some glyphs are off the cell in VT323 and Departure Mono.** Neither has braille or `▸ ▾ ● ⇥ ⏎ ✓`, and VT323 has no arrows or box-drawing either; those fall back to the system font, whose advance differs, so the Life strip and a few glyphs (`▸ ⇥ ⏎`) are not exactly one cell wide.

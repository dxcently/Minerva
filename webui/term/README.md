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
theme.css            TUI theme() -> CSS variables (web-ui.md 2.2)
term.css             the look: square frames, cut-in titles, gold composer, no border-radius
app.js               sidebars, split tree, inspector tabs (INSPECTOR map), status line, the ask queue, menus' items, keys, the : line
core/ui.js           the one import point for Preact, hooks, signals, htm; K = clickable text
core/menu.js         right-click / title menus: one open at a time, submenus, keys, kept on screen
core/api.js          apiBase(sessionId): the ONE place URLs are built. Hub = `/s/${sid}/api/`
core/sse.js          fetch-based SSE (Bearer header, never ?token=)
core/state.js        one session stream as signals: frames -> rows, calls, asks, running
core/life.js         port of eidolon crates/tui/src/life.rs (the Life strip)
core/markdown.js     safe subset as vnodes: fences, headings, `code`, **bold**, http(s) links
core/tile.js         split tree: dwindle at 0.618, per-split ratio, drag, promote on close
panes/session.js     one pane: frame glyphs, transcript, row menus, draft, inline asks, composer
renderers/           generic tool renderer; a renderer returns a string, a DOM Node or a vnode
brand/owl.svg        copy of webui/owl-green.svg (the door serves only this dir)
vendor/              preact, hooks, signals, signals-core, htm + LICENSE-* + VERSIONS + update.sh
```

## Mouse first

Every action is clickable; keys are shortcuts for the same functions.

| Where | Click does |
|---|---|
| composer bottom border | `send · steer · queue · stop` |
| `:` in the status line | the command list; every entry is clickable |
| `N waiting` in the status line | focus the oldest ask (`▶ N waiting`: it is holding until you pause typing) |
| a pane's title (click or right-click) | menu: split right ▸, split down ▸, show… ▸ (sessions, current ticked), mirror, reconnect, close |
| `⇥` `⇩` `×` on a pane's top border | split right / split down (the picker: mirror; new, resume, existing session need the hub) / close |
| right-click a chat row | fork from here (needs the hub), copy (the selection, if any), copy as markdown, quote into composer |
| right-click a sidebar session | open in new pane, show in focused pane, fork (latest) and close its door (need the hub) |
| right-click the status line | model ▸, mode ▸ (both need the hub), then every `:` command |
| sidebar tabs, `«` / `»`, rails | switch tab, fold to a rail, reopen |
| tile borders | drag to resize (snaps to cells) |
| an ask | one click on a choice answers: `yes · always · no · chat about it · fork · + note` (approval), `1 … n · chat about it · + note` (question); `+ note` opens the note line and sends nothing; the choice clicked next goes with the note. A click within 0.3 s of the ask appearing or jumping into view shows `steady…` and does nothing |
| a settled ask (`▸ approve: … → yes`) | open or fold its choices, the pick marked; focus stays where it was |

Menus: up/down move, Enter or right opens a submenu, Enter runs, left or Esc backs out, Tab closes; hover selects and opens submenus. A click outside closes; a second click on the title that opened one closes it. A menu that would run off an edge opens on the other side of its anchor. Greyed lines say why (`needs the hub`, `needs eidolon PR`).

## Keys

| key | does |
|---|---|
| Enter / shift+Enter | send (idle) or steer (running) / newline |
| alt+Enter | queue behind the running turn |
| Esc | leave the composer (`i` or Enter goes back); in an ask, back to the composer, the ask stays queued |
| Tab / shift+Tab | from the composer: the oldest ask. In an ask: Tab opens the note line (Tab again, back to the choices); shift+Tab, the previous ask in the queue |
| ← → | in an ask: move the highlight (it starts on the first choice, `yes`; greyed ones are skipped) |
| Enter | in an ask: take the highlighted choice, with the note if the note line is open. On `+ note`: open the note line (from the note line: nothing is sent; pick a choice) |
| ↓ / ↑ | in an ask: open the note line / back to the choices (the note is kept) |
| y / n | in an approval (or a yes/no question): yes / no at once, with the note if the line is open. Never `+ note`, never `always`; `n` does nothing when the ask offers no plain no. Not while typing the note |
| 1-9 | in a question: that option at once |
| ctrl+c | stop the turn (with an empty or unfocused composer) |
| ctrl+b / ctrl+i | sidebar / inspector to rails and back |
| [ / ] | inspector tab |
| alt+w or `:q` | close the pane (not while the composer holds text; ctrl+w belongs to the browser). Matched on the `w` character, so AZERTY works; on the physical W key only when the character is not ASCII (macOS Option) |
| `:` | command line: q, close, open [id], vsplit (mirror right), split (mirror below), stop, queue, reconnect, sidebar, inspector, keys |
| `?` | which-key line; every entry can be clicked |

## Asks

Every pending ask on the page, across all panes and sessions, is in ONE queue, oldest first; the status line shows `N waiting`.

- The oldest takes focus by itself, once, and only when nothing holding text has focus: a composer (or other text field) with anything typed in it never loses focus to an ask. With the composer empty it still waits for 1 s with no keys in it or the `:` line, and for no menu to be open. Meanwhile the status line says `▶ N waiting`; Tab, a click on that, or a click on the ask goes there. When focus lands its pane scrolls to it and it is marked `▶`; keys then do nothing for 0.3 s, so an Enter meant for the composer cannot answer it. IME composition keys are ignored.
- The one automatic focus is per ask (session + ask id): a reconnect's replayed copy neither takes focus again nor moves in the queue.
- When the queue empties, focus goes back to the composer it came from, caret where it was. Esc leaves an ask queued; the asks behind it wait too (none takes focus by itself) until `N waiting`, Tab, a click or `jump` goes back.
- Choices: an approval offers `yes · always · no · chat about it · fork · + note`, a question `1 … n · chat about it · + note`. `always` is live only if the approval's `answers` list it (a pending eidolon PR, gap 13), and is never what y or n sends; `no` is greyed when the answers offer no plain no; `fork` needs the hub. The highlight starts on the first live choice, never `+ note`. There is no free-text answer: words of your own go through `chat about it`.
- The `note ›` line is optional and closed until asked for: Tab or ↓ in the ask, or `+ note` (keys or mouse). Opening it never answers; the next choice taken goes with the note. Esc in the note line closes it and drops the note, sending nothing; `tab: add a note` at the end of the choices says so. The wire: `POST /api/answer {"ask_id","answer","note"}` (the pending eidolon PR). A door that refuses it (400/422) gets the refusal shown in the ask, the note line goes read-only, and the next choice goes without it. A door that silently ignores unknown fields drops the note, and the page cannot tell.
- `chat about it` answers no (when the ask has a no), cancels the turn (`POST /api/cancel`, which also cancels the turn's other asks) and puts a `> ` quote of the ask in the composer.
- Mirrors draw the same ask with their own highlight and note; an answer from any pane settles it everywhere.
- After an ask settles and the blocks below it move up, clicks in that pane get the same 0.3 s `steady…` guard; the second click of a double click is ignored.
- A settled ask stays in the transcript folded to one line, `▸ approve: bash rm -rf ./build → no (note: … · chat about it)`; opened, it lists every choice with the pick marked. The note shows only on the page that sent it. Asks are not replayed, so after F5 the fold is gone (the tool line and its verdict remain).

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

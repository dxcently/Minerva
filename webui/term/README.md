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
app.js               sidebars, split tree, inspector tabs (INSPECTOR map), status line, keys, the : line
core/ui.js           the one import point for Preact, hooks, signals, htm; K = clickable text
core/api.js          apiBase(sessionId): the ONE place URLs are built. Hub = `/s/${sid}/api/`
core/sse.js          fetch-based SSE (Bearer header, never ?token=)
core/state.js        one session stream as signals: frames -> rows, calls, asks, running
core/life.js         port of eidolon crates/tui/src/life.rs (the Life strip)
core/markdown.js     safe subset as vnodes: fences, headings, `code`, **bold**, http(s) links
core/tile.js         split tree: dwindle at 0.618, per-split ratio, drag, promote on close
panes/session.js     one pane: transcript, draft, inline asks, composer, pane menu
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
| a pane's title (or right-click) | pane menu: mirror, reconnect, close |
| sidebar tabs, `«` / `»`, rails | switch tab, fold to a rail, reopen |
| tile borders | drag to resize (snaps to cells) |
| an ask | `[allow]` -> `[confirm allow?]` for 3 s, click or tap again at least 0.4 s later; `[deny]`; `[1 label]` options. With a mouse the first click only arms once the pointer has rested on the ask for 0.3 s; touch, pen and screen-reader clicks arm on the first tap. A double-click never confirms, and a click that does nothing yet shows `steady…`. The arm is per pane. A yes/no question takes the same two clicks for `[yes]` |

## Keys

| key | does |
|---|---|
| Enter / shift+Enter | send (idle) or steer (running) / newline |
| alt+Enter | queue behind the running turn |
| Esc | leave the composer; `i` or Enter goes back |
| Tab | focus the next ask (keys reach an ask only while it has focus, shown by `▶`); from the composer, the oldest pending ask |
| y then Enter / n / 1-9 | allow / deny / answer the focused ask (a yes/no question: y then Enter / n). `Y` works too; held keys and bare modifiers are ignored, and Enter or Space on a focused `[allow]` never allows |
| ctrl+c | stop the turn (with an empty or unfocused composer) |
| ctrl+b / ctrl+i | sidebar / inspector to rails and back |
| [ / ] | inspector tab |
| alt+w or `:q` | close the pane (not while the composer holds text; ctrl+w belongs to the browser). Matched on the `w` character, so AZERTY works; on the physical W key only when the character is not ASCII (macOS Option) |
| `:` | command line: q, open, vsplit, stop, queue, reconnect, sidebar, inspector, keys |
| `?` | which-key line; every entry can be clicked |

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

## Known limits

- **Firefox scrollbars are not square.** The square scrollbars in `term.css` use `::-webkit-scrollbar`, which Firefox ignores; Firefox only offers `scrollbar-width` and `scrollbar-color`, and neither can change the thumb's shape, so Firefox draws its own rounded overlay scrollbars. There is no CSS fix; it is left as is.

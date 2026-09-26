# The web UI: a terminal screen in a browser

**Written 2026-09-25, revised the same day after the look, layout, tiling, mesh and hub decisions. A design, not a build: no code exists for the page yet.** The page is served by the **Minerva hub** (see `hub.md`), which fronts one or more `eidolon web` doors. Door citations are against branch `web-door` at `30cf0e5` in `eidolon-door/eidolon`, read with `git show`, paths relative to `crates/web/src/`. `launch.rs` and `post_launch` exist only in the uncommitted working tree of `web-launch` and are marked **(in progress)**. TUI citations are against the working tree of `eidolon-door/eidolon`. Aoide citations are against `Minerva/Aoide` at `20d621b7`, paths relative to `pkgs/aoide/crates/`. The shared command registry is the proposal on `noah427/eidolon#15` (last comment); it is **proposed, not built**.

**Labels**: **observed** (read in code, `file:line` given), **reported** (someone else's statement, quoted or attributed), **reasoned** (argued from observed facts), **hypothesis** (believed, not measured; says what would measure it). **decided** marks a user decision of 2026-09-25.

---

## 0. The answer in one paragraph

The whole page is an emulated terminal: square, monospace, on character cells, coloured with the TUI's own theme. Three columns (sessions sidebar, tiled session panes, inspector) over one status line. There are no buttons: keys, a `:` command line, a clickable key reference (`?`), and frame-title tabs. Panes tile Hyprland-dwindle style at the golden ratio; each pane is one session stream, and a session may be shown in two panes. Pending asks share one tabbed panel above the composer, answered in one batch. The browser talks only to the hub on loopback; the hub owns the doors and their tokens and proxies `/s/<id>/api/*`. The page is plain ES modules with no build step; tool renderers are extensions named in a manifest. The biggest risk is still the page rendering model output as HTML, because the page holds a token that can approve tool calls.

---

## 1. Principles

| # | Principle | Rules out | Why |
|---|---|---|---|
| P1 | **The log is truth; the UI is a view.** Every drawn thing derives from frames. Durable page state: the hub token in `sessionStorage`, and the pane layout (a convenience, safe to lose). | local chat history, optimistic rows | the door replays the branch on every connect (**observed** `stream.rs:127-132`) |
| P2 | **Rebuild, don't patch.** Every `hello` resets that pane's session state. | diffing transcripts on reconnect | `lagged` means "reconnect and start over" (**observed** `stream.rs:149-155`) |
| P3 | **Maintainable with no toolchain.** Plain ES modules, F5 to reload. | npm, bundlers, TS, JSX | ship goal: survives its author walking away |
| P4 | **Nothing from the network at runtime.** Scripts, the monospace font, the owl ship in the UI dir. | CDNs, web fonts, analytics | the door's CSP is `default-src 'self'` (**observed** `serve.rs:357`); the hub should send the same (see `hub.md`) |
| P5 | **Text is text.** Model output, tool output, peer and mesh messages, ask prompts: `textContent` or a sanitizing markdown pass, never raw `innerHTML`. | `el.innerHTML = frame.text` | an injected script could `POST .../api/answer {"answer":"yes"}` (reasoned from `serve.rs:445-464`) |
| P6 | **Ignore what you don't know.** Unknown frame `type` or field: skipped. | throwing on new frames | protocol rule (**observed** `lib.rs:55-58`) |
| P7 | **Borrow the TUI's look, not its code.** Palette, framed panels, keys, which-key: yes. `ui.rn` skin logic, vim motions, folds: no. | porting `Run::Ui` rows | they act on the TUI's own buffer (**observed** `crates/tui/src/command.rs:22-24`) |
| P8 | **The browser never talks to anything but the hub.** Not a door directly, not Aoide's A2A door. | cross-origin fetches, a second token in the page | one origin, one token, one CSP (reasoned) |

Framework: **plain ES modules + a ~100-line `h()` helper** (decided). The transcript is append-mostly, so a virtual DOM buys little (reasoned). Escape hatch if views outgrow it: a vendored Preact + `htm` file, introduced file by file, still no build. **Superseded: the escape hatch was taken; see section 10.**

---

## 2. Look (decided)

### 2.1 Screen rules

| Rule | Detail |
|---|---|
| Square | `border-radius: 0` everywhere, chips and the composer included |
| Cells | one monospace font at a time (2.2); `--cw`/`--ch` = measured cell width/height, re-measured when the font changes; layout sizes are whole cells where practical |
| Frames | 1px CSS borders with the title cut into the top border (a `legend`-like span over the line), as the TUI's panels do. Not box-drawing characters, so text reflows |
| Pane edges | snap to whole cells while dragging (`round(px / --cw)`) |
| Motion allowed | blinking caret (TUI `cursor`), "live" wires on the mesh pane, a pulse on the running badge, a brief flash on a new ask, the composer frame's pulse and the Life strip (below) |
| Motion forbidden | gradient borders, gradient or sheen fills; anything that moves text off its cells |
| Composer (decided 2026-09-25, supersedes the double frame) | the TUI's prompt frame: one 1px square frame in the session's **state colour** (IDLE base0D, RUN base0B, ASK base09, OFF base08, unknown base04), titled `message` on the left of its top border, the agent / Life strip at the right end of it; no fill. While a turn runs the border pulses state colour <-> base04, 1.6 s |
| Life strip (decided) | a port of the TUI pulse (**observed** `crates/tui/src/life.rs`): Conway's Life, 8-row world, middle 4 drawn in braille, cut into the composer's top border while a turn runs. Opens on the agent figure; `tool-call-started` launches a ship from the left, `tool-call-finished` one from the right; a settled world (still, period 2, or thinned below 3 lit cells) restarts with a ship from an edge chosen by launch count: from the left when the count of ships launched so far is even, from the right when odd (**observed** `life.rs:115`, `core/life.js` `step`). Stepped on the TUI's 250 ms tick (**observed** `app.rs:2087`). At rest: the static two-cell agent icon. Clock shown only when the page saw the turn start live; `⇡ ctx` only after a `context-size`; no `⇣ out` gauge (the wire carries no per-turn output count until `turn-settled`) |
| Reduced motion | `prefers-reduced-motion: reduce` stops every animation; the strip stays on its opening frame |
| No buttons | see 2.3 |

### 2.2 Theme and font (decided): base16 schemes, three vendored fonts

Colours come from a **base16 scheme**, swappable live; the TUI's `theme()` (`crates/tui/ui/default.rn:388-416`, **observed**) is no longer copied hex for hex. A scheme is `webui/term/themes/<name>.yaml` in the standard base16 format (`scheme:`, `author:`, `base00`..`base0F`; the newer tinted-theming layout with `name:` and an indented `palette:` works too), listed in `themes/manifest.json` because the page cannot list a directory. `core/look.js` parses exactly that YAML subset (no YAML library) and writes the sixteen colours as `--base00`..`--base0F` on `<html>` (CSSOM writes, allowed by the CSP); `theme.css` derives every other colour from them and carries the default scheme's values, so the page is right when no scheme loads.

| slot | CSS var | chrome | transcript role |
|---|---|---|---|
| base00 | `--bg` | page background, text on inverse | |
| base01 | `--panel` | status line, menus, composer tint, the focused ask | code background |
| base02 | `--sel` | selection, highlighted menu row | |
| base03 | `--line` | frames, dividers, rules (never text) | the end-turn rule's line |
| base04 | `--faint` | dim text: hints, captions; the composer pulse's low end | thinking fold, notices' bodies (peer, `eidolon send`, sys rows), end-turn text |
| base05 | `--fg` | | assistant body |
| base06 | `--bright` | active tab and frame, composer frame, caret, headings | `minerva` |
| base07 | `--brightest` | focused ask, focused pane title (inverse) | |
| base08 | `--red` | OFF badge, `yolo` badge | call error, deny (`no`, `[declined]`, `[refused]`), errors, diff removed |
| base09 | `--orange` | asks waiting: ask frames, `N waiting` | verdict labels (`[judged]`, `[yolo]`, `[fired]`) |
| base0A | `--yellow` | warnings (banners, cut notices, a connecting pane) | a call's command/arguments, inline and fenced code |
| base0B | `--green` | | call ok, diff added |
| base0C | `--cyan` | | links, paths, notice labels (`~ scout`) |
| base0D | `--blue` | | your prompt and the `you` label |
| base0E | `--magenta` | | tool names (`bash`, `write`, ...) |
| base0F | `--brown` | | syntax only |

**Rule:** chrome (frames, sidebars, the inspector, status line, borders, menus) uses only the base00-07 ramp, plus 08/09/0A where they mean error, ask waiting, warning. In the transcript each role has its own slot (the table), so a prompt, a tool, its command, its verdict and its status read apart at a glance. `you` is base0D rather than base07: in phosphor base07 is a pale green next to the green body, blue is not. Every role colour is at least AA (4.5:1) on base00 in all three schemes (lowest: base04, 5.01 in phosphor). `--added`/`--removed` are 18% of green/red over `--bg`. The composer frame wears the state colour (above); the Life strip is shaded along its length as in the TUI.

| scheme | look | notes |
|---|---|---|
| `phosphor` (default) | green phosphor | base04 nudged `2E8A1E` -> `32921F` for AA on base01 (below) |
| `minerva-dusk` | the M1 purple page | base06 is the old composer gold `D4A93A`, so tabs, frames and the composer stay gold, apart from base0A (the old yellow `E6C35C`, now commands and code); base04 nudged `8A849E` -> `908AA4` |
| `amber` | monochrome amber phosphor | same accent roles; base04 `B07300` |

Contrast (WCAG, text on base00 / on base01): phosphor body 10.45 / 9.84, dim 5.01 / 4.71; dusk body 12.65 / 10.52, dim 5.55 / 4.62; amber body 10.90 / 10.25, dim 5.04 / 4.74; the accents 5.74 (dusk red) to 14.78. base03 stays near 2:1 in all three, so it carries no text: frames and rules only. Greyed (inactive) text is base04 struck through (reviewed 2026-09-25: base03 text failed AA); the reason beside it is not struck. That is why the thinking fold and the end-turn text are base04, not base03.

**Fonts** are vendored under `webui/term/fonts/` (SIL OFL, sources and sha256 in `fonts/VERSIONS`); nothing comes from the network (P4). Each font has its own size and line height; the cell is then measured from the font itself (80 `M`s), at load and on every switch, and the tiles re-snap to it.

| font | size / line height | cell | why |
|---|---|---|---|
| VT323 (default) | 20px / 1.15 | 8 x 23 px | tall and narrow; smaller is hard to read |
| Departure Mono | 11px / 1.6 | 7 x 18 px | drawn on an 11px grid, crisp only at multiples of 11 (22px would make the sidebars 420px wide) |
| system mono | 14px / 1.35 | as measured | the M1 stack: `ui-monospace, Cascadia Mono, ... monospace` |

Switch from the status line's right-click menu (`theme ▸`, `font ▸`, the current one ticked) or the `:` line (`:theme <name>`, `:font <name>`, a unique prefix is enough; no name lists them). The choice is kept in `localStorage` (colours included, so a reload paints no default first); without storage the page still works, for that tab only. Neither font has braille or `▸ ▾ ● ⇥ ⏎ ✓`, and VT323 has no arrows or box-drawing either: those glyphs fall back to the system font, so the Life strip and a few frame glyphs are not exactly one cell wide. The status line keeps the TUI's order: badge, model, persona. Light theme: any light base16 scheme dropped in works; none ships (Q4).

**The TUI in the same colours: `bin/eidolon-theme`** (python3, run in WSL; `hoot theme` from Windows). It reads the same scheme files with the same strict parser and rewrites only `pub fn theme()` in `~/.config/eidolon/ui.rn` (`$EIDOLON_UI` overrides the path) with `#rrggbb` values, keeping each role's comment; a backup `ui.rn.bak-<timestamp>` is written first, nothing is written when nothing changes, `--restore` puts back the newest backup, `--dry-run` prints the block, `--list` the manifest. Seen on the next TUI launch; no eidolon change. Roles: `user` 0D, `text` 05, `faint`/`gutter`/`comment` 04, `tool`/`keyword`/`tag` 0E, `sent`/`code`/`hit` 0A, `dialog`/`number` 09, `ok`/`string` 0B, `error` 08, `link` 0C, `heading`/`menu`/`cursor` 06, `peer` 07, `block` 01, `focus` 02, `added`/`removed` 18% of 0B/08 over 00. What theme() does not reach stays the terminal's: the background, the font, the reply body (the terminal's foreground) and anything `view()` paints with a named colour (`cyan`, `dim`); matching those is the terminal profile's job.

### 2.2b Transcript text (decided 2026-09-25)

- **No time gutter.** The first row of each speaker run carries dim (base04) subtext right of the name: `you  16:42`, `minerva  16:42 · 4.9k in · 310 out · 12.4s` (the run's settles added up). The time is the record's own (`ts`, epoch or RFC 3339) when a frame has one, else the arrival time of a live frame; a replayed frame without one shows no time (never `--:--`). Hover gives the full date-time. Day separators `── THU 10 SEP ──` are drawn only from record times; **no door frame carries `ts` today** (gap).
- **Code, as the TUI draws it** (`crates/tui/src/syntax.rs`, `render.rs:2877-3000`), in `core/syntax.js`: a line-local scanner (comment, string, number, keyword from a per-language list; unknown languages are left plain), roles keyword base0E, string base0B, number base09, comment base04, code text base0A (as `bin/eidolon-theme`). Fenced blocks and a `read`'s output get a number gutter (base04 on a base03 rule). eidolon's read already numbers its lines (`{n:>6}\t`, **observed** `crates/tools/src/fs.rs:35-50`): those numbers are used, the prefix stripped, and its `[showing lines a-b of N]` / `[empty file]` footer drawn as a dim note; output not in that shape falls back to plain lines numbered from the call's `offset` (a number or a numeric string, as the schema accepts). A `diff`/`patch` fence and any tool output that is a unified diff are drawn as a diff (hunk and file headers dim, lines on `--added`/`--removed`; a hunk's counts decide where it ends, so a removed `-- x` is a line, not a header). Diffs are numbered as the TUI numbers them (`render.rs` diff_lines): by the side that survives, a removed line blank; an `edit` or `write` shows its change under the tool line from its own arguments while it runs (edit: the fragment's lines from 1, the shared lines trimmed to 3 either side; write: every line added), 6 lines until clicked, like the TUI. Long code draws 40 lines (reads) or 400, then 400 more per click: a 2,000-line read costs what is on screen.
- Why a scanner and not highlight.js: it is the TUI's own rule set (same roles, same "not a parser" edges), about 120 lines against ~40 kB of vendored grammars, no eval question for the CSP, and cheap enough for streaming. Languages: rust, python, js/ts, go, sh, c/c++/java/cs, nix, toml/yaml/ini, nginx, json, sql.

### 2.2c Sidebars and the start screen (decided 2026-09-25)

- **Resizable sidebars:** the inner edge of each sidebar drags in whole cells (←/→ when it has focus, double-click resets), 16-72 cells and at most 45 % of the window, defaults 30 / 34, kept in `localStorage` (`minerva.sidebars`).
- **Sessions grouped by project** (the session's `cwd`, named by its last part): `▾ name  +`; the name folds the group, `+` starts a session there (greyed: needs the hub).
- **Start screen:** a pane with no transcript shows the owl, then session, model, persona, gate and cwd rows, then key hints, as the TUI's first screen.
- **`phosphor-soft`:** a charcoal-ground variant of `phosphor` (base00/01 lifted, the greens kept) for long sessions; every text slot still AA on base00.

### 2.3 Controls without buttons

| Control | What it is | Where |
|---|---|---|
| Keys | global and per-pane bindings (section 4.2) | everywhere |
| `:` command line | opens in the status row; completion list, the match highlighted in `--sel` | status line |
| Key reference (decided 2026-09-25, replaces the key bar and the which-key line) | `?` (outside a text field) or `? keys` at the right of the status line opens a centred panel, as the TUI's `space ?`: sections *here* (what the keys do where focus is now), *anywhere*, *composer*, *asks*, *panes*; **every entry is clickable** and does what its key does; Esc or an outside click closes it | popup |
| Frame-title tabs | `[rec] tree diff graph run` cut into a frame's top border; click or `[`/`]` | sidebars, inspector |
| Rail | a collapsed sidebar leaves a 1-cell-wide rail; click it to reopen | left and right edges |
| Clickable text | ask tabs and rows, key-reference entries, status-line pieces: underlined on hover, `<span role="button" tabindex="0">`, so "no buttons" is visual only, not a screen-reader loss (reasoned) | inline |
| Frame glyphs (decided) | `⇥` split right, `⇩` split down, `×` close, cut into each pane's top border on the right; each has a tooltip | pane frames |
| Menus (decided) | a square frame of lines, opened by right-click (at the pointer) or a click on a title or glyph (under it); keys: up/down, Enter or right into a submenu, left or Esc out; closes on an outside click; flips to the other side of its anchor rather than run off screen. Greyed lines say why (`needs the hub`) | pane titles, glyphs, chat rows, sidebar sessions, status line |

---

## 3. Layout (decided baseline)

### 3.1 Baseline screen

```text
+- sessions  projects  mesh ---+- ctf-prep ▾ [ctf-prep] ● ---------------- ⇥ ⇩ × -+- rec  files  diff  graph  run -+
|  ,___,  minerva              | you  16:40                                          | turns 7                       |
| [O.o]   Make it,             |   find the flag in ./chall                          | calls 12                      |
| /)_)    Break it,            | minerva  16:41 · 4.9k in · 310 out · 12.4s          |                               |
|  ""     Hack it.             |   > thinking (3 lines)                              | verdicts                      |
|                              |   -> bash  ls -la ./chall                   ok 0.2s | bash subshell       judged    |
| ▾ ctf-prep +                 |   -> edit  src/app.py                       ok ▸    |                               |
|   ● ctf-prep            [2]  |    1   - def probe(host, port=PORT, tries=3):       |                               |
|     minerva: Looking at…     |      1 + def probe(host, port=PORT, tries=5):       |                               |
| ▾ web-audit +                |   ? waiting · ask 1  write ../notes.md              |                               |
|   ○ web-audit                | +- 1 write ../notes.md │ 2 ? pick db │ 3 fetch · web-audit -+                  |
|                              | | approve: write  ../notes.md                     |                             |
|                              | | › yes                                           |                             |
|                              | |   no  · + note · chat about it                  |                             |
|                              | |   submit 3 answers (1 of 3 answered)            |                             |
|                              | +-------------------------------------------------+                             |
|                              | +- message ------------------------------ ⠿⠷⠶ --+                             |
|                              | | Write a message… (⏎ send · ⌥⏎ queue · ^c stop)  |                             |
|                              | +-------------------------- ⏎ steer · queue · stop +                            |
+------------------------------+-----------------------------------------------------+-------------------------------+
 ASK  qwen3 · YOLO · ask 1/3 · 3 waiting                        ctx 41k/- · in 9.8k out 830 · tools 12   ? keys
```

(Tab labels are the ask in one line; a tab from another session names it. Drawn with three asks from two sessions.)

### 3.2 Regions

| Region | Content | Collapse | Idea borrowed from |
|---|---|---|---|
| Left sidebar | owl on top; tabs `sessions` / `projects` / `mesh`; sessions `●` open, `○` kept; peers listed under sessions | `ctrl+b` -> rail | chat apps' conversation list; TUI panel frames |
| Centre | tiled session panes (section 4) | never | tiling window managers |
| Right sidebar "inspector" | tabs `rec` / `loaded` / `files` / `diff` / `graph` / `run`; follows the **focused** pane. `loaded`: what this door has loaded, as far as it says — persona, model and provider (from `hello.model`), gate mode (`hello.yolo`) and verdict count, the tools called so far (no door route lists them; `mcp__<server>__<tool>` names grouped by server), extensions, session file, cwd, protocol; what the door does not expose (persona, effort, policy, full tool list, MCP servers, extensions) is listed dim with `needs the hub`, never hidden (**observed** `crates/web/src/stream.rs:41-48`: hello is session, cwd, model, yolo, pending, protocol). `files`: a tree with a `/` filter, holding the files this session's edit and write calls touched (the whole working directory needs the hub: no door route lists files); `diff` is the future home of git state | `ctrl+i` -> rail | IDE side panels |
| Ask panel | one orange (base09) frame above the composer of the pane that owns the current ask; its top border is a tab strip, one tab per pending ask on the page; the current ask below as the TUI's vertical list (7.1). In the transcript: a one-line `? waiting · ask N` marker where the ask arose, then the folded settled line | - | the TUI dialog; tabbed editors |
| Composer | the TUI prompt frame in the state colour, `message` title, the agent / Life strip at the right end of the top border in a fixed 12-cell slot (every braille cell forced one cell wide, so nothing moves as it steps), placeholder with the keys; the bottom border is a line of menus on the left (`+ attach ▾ · model qwen3 ▾ · effort ? ▾ · mode gated ▾ · ◉ mic`, 3.3) and the verbs on the right; image chips inside the frame above the text; grows upward to 10 rows | - | the TUI prompt |
| Status line | the TUI's: an inverted **state badge** (`IDLE` / `RUN` / `ASK` / `OFF` / `BYE` / `?`, coloured per state) bottom-left, then the model (base0C), persona (base0E, when the door names one), `YOLO` (base08, when on), all read only: a click opens the inspector's `loaded` tab (switching is the composer's menus), `ask i/n`, `N waiting` (asks not yet **sent**: answered but not submitted still counts, and keeps the badge `ASK` and the composer frame orange), the connection when not live; on the right `ctx used/limit · in N out N · tools N` and `? keys`. It drops pieces right-first to fit a narrow screen. No key bar | - | the TUI's status line (**observed** `default.rn:419-537`) |

No assets, CSS or names are copied from other apps. The owl is ours (`webui/brand/`, **observed** in the repo).

### 3.3 Composer behaviour

| Session state | Enter | Door call and answer |
|---|---|---|
| idle | send | `POST /s/<id>/api/say {"text","mode":"send"}` -> `202 {"queued":false}` (**observed** `driver.rs:260-279`, `serve.rs:424-425`) |
| running | steer (per the decided hint) | `mode:"steer"` -> `202 {"queued":false}` (**observed** `driver.rs:267-269`) |
| running, `ctrl+c` in pane / `:stop` | cancel | `POST /s/<id>/api/cancel` -> `204`, or `409` = already idle, not an error (**observed** `serve.rs:260-267`) |

- **Queue is no longer on Enter.** It is `alt+enter` (or the `queue` word; Q5).
- **Queued messages are held by the page, editable until sent** (decided 2026-09-25). The door's queue is core's `follow_up` (a `send` while running -> `queued:true`, **observed** `driver.rs:169-190`, core `agent.rs:448-455`): nothing can edit or drop a message once it is there, and no route lists it. So `alt+enter` while a turn runs holds the message in the **pane** (not the stream: a reconnect keeps it), drawn as a stack just above the composer (below the ask panel): each row its text on one line (a click shows it whole), then `edit · steer ↑ · ×`. `edit` loads it into the composer (what was typed waits aside and comes back), titled `message · editing queued N`; ⏎ puts it back in its place, Esc leaves it as it was, emptying it drops it. `steer ↑` sends it now as a steer (read at the running turn's next safe point). `×` drops it. ↑ in an empty composer edits the last one. When the turn ends (`turn-state` false) the first is sent, which starts a turn; the next waits for that turn to end, so they go in order, one turn each (a send the door refuses goes back to the head). The status line says `N queued` beside `N waiting` (held here plus any the door holds). Showing another session in the pane asks first (`drop N queued and switch` / `keep them here`). Not kept across F5. With the session idle or unknown, `alt+enter` is a plain send, as before.
- **The bottom border is a line of menus** (decided 2026-09-25), TUI clickable text with `▾`, each opening the square menu: `+ attach ▾` (image… / file… / folder…), `model <name> ▾`, `effort ? ▾`, `mode YOLO|gated ▾`, `◉ mic`. Values come from `hello` where the door reports them (model, yolo; effort is not reported: `?`). Every switch is greyed `needs the hub (restarts the door between turns)`: the door has no route to change them. `◉ mic` is greyed `needs local dictation (hub + whisper.cpp)`; the browser's Web Speech API is not used (Chrome sends the audio to Google). The line fits as the status line does: the verbs drop their keys, then the labels drop to glyphs (`+ ▾ · qwen3 ▾ · ◔ ▾ · ⚑ ▾ · ◉`), then every menu goes into one `⋯ ▾`, then only send and stop stay.
- **Attachments** (`core/attach.js`). Images (the menu's `image…`, a paste, a drop on the composer) go in the say's `images` field, `[{name, media_type, data}]` with `data` standard base64 (**observed** `crates/web/src/attach.rs`, `serve.rs` SayBody). The door's limits are checked first and said in words: PNG, JPEG, GIF or WebP by their bytes (the door sniffs them too), 4 MiB each decoded, 8 per message, the whole say under 8 MiB, a name of at most 255 bytes without `/`, `\` or NUL. They show as chips inside the frame above the text, `[img] shot.png ×`, and go with the next send, steer or queued message. A text file (`file…`, or dropped) goes inline where the caret is, as a fenced block named for the file (its extension picks the colours); at most 256 KiB, UTF-8, no NUL in its first 8 KiB (as `fs.rs` read's binary check); anything else is refused by name. `folder…` is greyed: a session's folder is fixed at launch. The door has no document block (`attach.rs` NO_DOCUMENTS), so nothing else goes as a file.
- **Asks never block the composer.** Typing, ⏎ (steer), ⌥⏎ (queue) work while asks wait; an ask takes keys only when it has focus and is armed (7.1).
- **Steer is never sent idle.** An idle steer sits unread until the next turn (**observed** core `agent.rs:430-432`); the page sends `send` instead.
- **`:` in the composer** goes to the model as text (**reported**, #15 comment). The `:` command line lives in the status row, not the composer, so there is no ambiguity; a composer text starting with `:` gets a faint warning.
- Chips: images (above). `@session` (context from another session) needs the registry or a hub route, M4+.

---

## 4. Tiling (decided)

### 4.1 Dwindle at the golden ratio

Each new pane splits the **focused** pane's area along its longer side (the newest pane if none is focused): the old pane keeps 0.618, the new one gets 0.382. No cap; panes just shrink. Drag a divider to resize (snapped to cells); drag a pane's title onto another pane to swap, or onto an edge to move.

```text
1 pane          2 panes                  3 panes
+----------+    +------+---+             +------+---+
|          |    |      |   |             |      | B |
|    A     |    |  A   | B |             |  A   +---+
|          |    |      |   |             |      | C |
+----------+    +------+---+             +------+---+
                 .618  .382               B keeps .618 of the right column
```

Gain: predictable layout, Hyprland muscle memory. Loss: the fifth pane is small on a laptop. Each **session** is one SSE stream through the hub, not each pane: mirrors share their session's stream (4.3); separate sessions still cost one stream each (hypothesis: measure memory and hub CPU at 8 sessions).

### 4.2 Keys

| Key | Action | Note |
|---|---|---|
| `ctrl+enter` | new pane with a **picker inside it**: `n` new session, `r` resume a kept one, `m` mirror an open one | picker is a list in the pane, not a modal |
| `alt+w` / `:q` | close the **pane**; the session is kept (`○`) | not `ctrl+w`: browsers reserve it and do not deliver it to pages in most cases (reasoned from browser behaviour). See Q3 |
| `ctrl+b` / `ctrl+i` | toggle left sidebar / inspector | Firefox binds `ctrl+b` and `ctrl+i`; `preventDefault` works for these (hypothesis: test) |
| in the ask panel: `↑`/`↓` or `k`/`j`, ⏎, `y`, `n`, `1-9`, `c`, Tab, `s`, `←`/`→` or `h`/`l`, shift+Tab, Esc | move the highlight (clamped, as the TUI), take it, yes, no, a question's option, chat about it, open the note, submit every answer, previous / next tab (across panes; focus follows), previous tab, back to the composer | the oldest unanswered ask takes focus by itself only when the composer is empty (7.1); in the note line every key is text except ⏎ (take), `↑`/`↓`/Tab (back to the list) and Esc (drop the note); held keys ignored |
| `Tab` / `shift+Tab` | from the composer or nothing: the oldest waiting ask; in the panel Tab opens the note, shift+Tab the previous tab | Tab is the way to a waiting ask while the composer holds text |
| `alt+w` | close the pane | matched on `e.key` `w`/`W` first (so AZERTY's w works), on `e.code` `KeyW` only when `e.key` is not ASCII (macOS Option types `∑`); not while the composer holds text |
| `ctrl+c` | cancel the focused pane's turn (when composer is empty or unfocused) | mirrors the TUI |
| `:` | command line | outside the composer |
| `?` | the key reference panel (2.3) | not while typing in a field |

Every key is a shortcut for something also clickable (decided): the composer's bottom-border words `send · steer · queue · stop`, every entry of the key reference (an `asks` entry takes you to the panel, where the key works; the *here* section acts at once), the model on the status line, `N waiting` in the status line (focus the oldest ask), rails, tab labels, tile borders (drag to resize), and these menus and glyphs:

| Where | Opens | Live in M1 | Needs the hub |
|---|---|---|---|
| pane title, click or right-click | split right ▸ · split down ▸ · show… ▸ · mirror · reconnect · close | mirror, reconnect, close; the split pickers' `mirror` | show…: another session, new session, resume… |
| `⇥` / `⇩` on the pane frame | the split picker: mirror · new session · resume… · existing session ▸ | mirror | the rest |
| `×` on the pane frame | closes the pane | yes | - |
| a chat row, right-click | fork from here · copy · copy as markdown · quote into composer | copy (the selection, if any), copy as markdown, quote | fork from here |
| a sidebar session, right-click | open in new pane · show in focused pane · fork (latest) · close its door | open in new pane (a mirror), show in focused pane | fork, close its door |
| the status line, right-click | loaded (the inspector tab) · theme ▸ · font ▸ · every `:` command | the `:` commands | model, mode (the composer's menus) |

A pane's session can change (`show…`): the page structures a pane as `{ id, sid }`, so a mirror becomes an independent pane on another session once the hub lists more than one. In M1 only the one door session exists; `:open <id>` opens a pane on another session id, for the hub's `/s/<id>/api/`.

The `ctrl+enter` picker row above is the M2 plan; in M1 the split picker is a menu off the `⇥`/`⇩` glyphs, not a list inside the new pane.

### 4.3 Mirroring

Two or more panes may show one session. The page keeps **one** `Session` (one `/api/events` stream) per session id, and every pane showing it is a view of that one state: a pane per stream would hit HTTP/1.1's six-connections-per-host limit at six mirrors and hang every request after it. Closing a pane closes the stream only when no other pane still shows that session. Every view draws every ask; an answer from any of them settles it everywhere (`ask-settled`), and a `409` still means another client answered first (**observed** `user.rs:146-151`). A mirrored pane's title carries `(mirror)`.

---

## 5. Architecture: hub and doors

```text
browser (one page, one origin)
   | Bearer <hub token>, fetch-based SSE
   v
Minerva hub  (Rust, 127.0.0.1, see hub.md)
   | spawns / lists / stops `eidolon web` doors, holds each door's token
   | proxies  /s/<id>/api/*  ->  door <id> /api/*   (swaps the Bearer)
   | serves the UI dir, the tree listing route, the mesh routes
   +--> eidolon web  (one session each; unchanged: the door stays one session)
   +--> eidolon web
   +--> aoide CLI / ~/.aoide/state / aoided socket   (read side of the mesh, section 9)
```

Why: the door stays one session because it serves one `--session` (**observed** cli `main.rs:39`); same origin because the door has no CORS and a `default-src 'self'` CSP (reasoned from `serve.rs:357`); the page never holds a door token, so a page compromise reaches only the hub surface `hub.md` bounds.

### 5.1 Wire, per door (proxied)

Routes (**observed** `lib.rs:7-19`, `serve.rs:240-277`; launch **(in progress)** working-tree `serve.rs:510`). The page prefixes each with `/s/<id>`.

| Route | Body | Answers |
|---|---|---|
| `GET /api/events` | - | `200 text/event-stream`; `401` empty |
| `POST /api/say` | `{"text", "mode"?: "send"\|"steer"}`, text only (`serve.rs:413`) | `202 {"queued"}`; `400`; `413` over 8 MiB |
| `POST /api/cancel` | none | `204`; `409 {"error"}` idle |
| `POST /api/answer` | `{"ask_id", "answer"}`; with the pending eidolon PR also `"note"?` (free text beside the answer) | `204`; `409` not pending; `422` not offered, stays pending (`serve.rs:445-464`); a door without the PR may refuse `note` (400/422) or ignore it |
| `POST /api/launch` **(in progress)** | `{"window":"terminal"\|"editor"\|"files","path"?}` | `200 {"opened"}`; `422`; `501`; `500` |

Every frame is one SSE `data:` line of JSON with a `type`; no `id:`, no `event:` (**observed** `stream.rs:44-46`, `wire.rs:28`). Order per connection (**observed** `stream.rs:108-139`): subscribe, `hello`, replayed branch, pending `ask`s oldest first, `caught-up`, then live frames until `lagged` or `goodbye`.

### 5.2 Token hand-off (decided: redirect file)

```text
launcher: hub writes <runtime>/minerva-open.html (0600) containing
          location.replace('http://127.0.0.1:PORT/#token=...')
       -> opens the FILE PATH in the browser (argv holds a path, not the token)
page:  t = location.hash -> sessionStorage['minerva-token'] = t
       history.replaceState(null, '', location.pathname)
       every call: Authorization: Bearer t;  SSE via fetch + ReadableStream
```

- The fragment never reaches a server; the door sets `no-referrer` (**observed** `serve.rs:213-214`, `371`); the hub must match and deletes the redirect file after use (`hub.md`).
- A token in argv is readable via `/proc/<pid>/cmdline` unless `hidepid` (reasoned): attacker C in `web-door-security.md` section 1. The file path avoids it.
- `fetch`, not `EventSource`: Bearer header instead of `?token=`, 401 visible, our own backoff that `goodbye` stops. `sessionStorage` is per tab; a new tab needs the launcher.

---

## 6. Frame -> component mapping

Components: **Tr** transcript (rows keyed by `record`), **Dr** draft (streaming reply), **Tc** tool line (keyed by call `id`), **Ak** ask panel (tabs, batch submit) + transcript marker, **St** status line, **Co** composer, **Ts** toast/banner line, **Cx** connection manager (per pane), **In** inspector. "Replay?" = does `wire::replay` emit it (**observed** `wire.rs:395-502`).

| Frame | Fields | Replay? | Drawn by | State it updates |
|---|---|---|---|---|
| `hello` | session, cwd, model, yolo, pending, protocol | door | St, Cx, pane title | **reset pane state**; `protocol != 1` -> Ts "page older than door" |
| `caught-up` | - | door | Cx, Tr | `live = true`; scroll to bottom |
| `turn-state` | running | live only | St, Co | `running`; Enter = steer; badge pulses |
| `ask` (`approval`) | ask_id, call_id, tool, input, prompt, reason, structural, judged, yolo, answers | door (pending) | Ak, Tc | `asks[ask_id]`; block placed after `Tc[call_id]` |
| `ask` (`question`) | ask_id, prompt, options[{label, description?}] | door (pending) | Ak | `asks[ask_id]`; options as `1 a`, `2 b` keys |
| `ask-settled` | ask_id, how, answer? | live only | Ak, Tr | delete ask; one faint row "allowed" / "cancelled" |
| `lagged` | dropped | live only | Cx, Ts | reconnect now, rebuild |
| `goodbye` | text | live only | Cx, Ts | stop reconnecting; banner line in the pane |
| `message-start` | - | no | Dr | open draft |
| `text-delta` | text | no | Dr | append; blinking caret |
| `thinking-delta` | text | no | Dr | append to folded thinking |
| `tool-use-start` | id, name | no | Dr, Tc | placeholder "-> bash ..." |
| `tool-input-delta` | id, partial_json | no | Tc | spinner only; never parse partial JSON |
| `assistant-message` | record, text, thinking, redacted, tool_uses | yes | Tr | commit draft as row `record` |
| `user-message` | record, text, images | yes | Tr, Co | row; `images > 0` -> chip; clears queued count |
| `tool-call-started` | id, name, input, origin | yes (origin always `model`) | Tc, In | `calls[id]`; renderer by `name`; `run` tab open calls |
| `tool-call-finished` | record?, id, name, output, is_error | yes (`name` empty) | Tc, In | `calls[id].output`; name from `calls[id]`; `diff` tab if edit/write |
| `ask-user` | prompt | no | - | **no UI**: `ask` is the drawable twin |
| `policy-verdict` | tool, reason, outcome, note? | yes | Tc, Tr | badge refused / judged / yolo |
| `context-size` | tokens | yes | St | `ctx %` (needs the model's window; absolute tokens until known) |
| `turn-settled` | stop_reason, usage, timing? | yes (timing absent) | Tr, St, In | faint divider; `graph` node closes |
| `compacted` | summary, replaced_messages | yes | Tr | folded "earlier messages summarised" |
| `peer-message` | record, from, from_cwd, channel?, text, external | yes | Tr | `~ from` row in `--lightgreen` |
| `turn-budget` | record, calls_left | yes | St | warn under 3 |
| `command-results` | record, lines | yes | Tr | monospace block |
| `cancelled` | usage, calls | yes (zeros) | Tr, St | "stopped" row; replay zeros shown as nothing |
| `error` | text | no | Tr, Ts | red row; **gone on reload** |
| `queued` | waiting | no | Co, St | the door's own queue (core follow_up): count in the composer's bottom border and in the status line's `N queued` (the page's own held messages are added to it; 3.3) |
| `quiesced` | record, destination | no | Tr, Ts | "settling to <destination>"; expect `goodbye` |
| `trigger-fired` | record, condition, outcome, call_id | no | Tc, In | mark the `wait_for` resolved; `run` tab |
| anything else | - | - | - | ignored (P6) |

22 bus projections (**observed** `wire.rs:29-104`) + 7 door-local frames (**observed** `stream.rs:58-96`, `user.rs:63-81`).

---

## 7. Asks, reconnect, lagged, goodbye

### 7.1 The ask panel

**Decided 2026-09-25 (supersedes the inline ask blocks and the waiting strip):** one tabbed panel for every pending ask on the page, each ask a vertical list as the TUI's dialog (**observed** eidolon `crates/tui/src/app.rs:3707-3740`, `ui/default.rn:645-676`), answered in **one batch**.

```text
+- 1 bash rm -rf ./build ✓ │ 2 ? Which wordlist? ✓ │ 3 fetch http://10.0.0.5… · web-audit -+
| approve: fetch  http://10.0.0.5/login   · web-audit                                     |
| fetch — a host outside the allow list. Run it?                                          |
| › yes                                                                                   |
|   no                                                                                    |
|   + note                                                                                |
|   chat about it                                                                         |
|   submit 3 answers (2 of 3 answered)                                                    |
| keys: ↑↓ j k choose · ⏎ take · y yes · n no · c chat · tab note · s submit · ←→ h l asks |
+-----------------------------------------------------------------------------------------+
   transcript, where it arose:  ? waiting · ask 3  fetch http://10.0.0.5/login
   settled:  ▸ approve: bash rm -rf ./build → no (note: only ./build)
```

| Rule | Why |
|---|---|
| **One panel, one tab per pending ask** across all panes and sessions, in queue order (arrival; a replayed ask keeps its place), never one per pane: a mirror shows the ask's marker in each pane but the panel once. The panel is docked just above the composer of the pane that owns the current tab: the focused pane if it shows that session, else the first that does; switching to another session's tab moves the panel (and focus) there. No pending asks, no panel | one place to look, however many panes |
| The tab strip is the frame's top border: `│`-separated `N <ask in one line>`, a tab from another session adds `· <session>`; the current tab is inverted (base09 ground) while the panel has focus, bold orange otherwise, the others dim; an answered tab carries ✓. A click, `h`/`l`, `←`/`→` or shift+Tab switch tabs. Too narrow for the whole strip, it fits as the status line does: the other tabs drop to their number (`2✓`), then the current tab's words shorten, then go; the current tab is scrolled into view | the queue at a glance |
| Rows (the TUI's): an approval `yes · no · always (only when the door offers it) · + note · chat about it`; a question its options (`1-9`) then `+ note · chat about it`; with more than one tab, a `submit` row last. `no` is greyed `not offered` when the answers hold no plain no; `always` is never a fallback. The highlight starts on the first live row and **clamps** at the ends | never a choice the door will refuse |
| **Batch submit** (decided): taking a row records the answer (✓ on the tab), sends nothing, and moves focus to the next unanswered tab. An answered tab can be revisited and changed freely until sent. `submit` (the row, `s`, or `:submit`) is live only when every pending tab is answered (else it reads `2 of 3 answered`) and then fires every answer **back to back in queue order** (the door answers one call at a time). ⏎ on the last unanswered tab submits at once; `y`/`n`/digits/click on it only record and move the highlight to `submit`. A lone ask is sent as soon as it is answered. In code the answer step (`Session.choose`) stays separate from the send (`Session.send`); `app.js submitAsks` batches | one decision over several asks, reviewed before it goes |
| **`chat about it` never waits**: deny (when there is a no), `POST /api/cancel`, and the composer gets a `> ` quote of the ask (and the note); the turn's other asks are cancelled with it and take no focus meanwhile | stop, then talk |
| A sent tab keeps ✓ and is read-only (rows greyed, `sent, waiting for the door`) until its `ask-settled` arrives, then leaves the strip. One batch runs at a time (a second submit while one is sending does nothing). **The first refusal stops the batch** (400/422 note, network): the asks after it are not sent, so none goes out of order; the toast says `sent 1 of 3; ask 2 refused: <why>`, that tab takes focus with its answer cleared and the error shown. 409 (answered elsewhere) and 404 (the session is gone) fold it as "answered elsewhere" and the batch goes on. Recorded answers survive a reconnect (kept by `ask_id`, restored when the replay redraws the ask), and the batch looks each ask up by id when its turn comes | (**observed** `user.rs:146-155`) |
| **The oldest unanswered ask takes focus by itself, once** per session + `ask_id` (a reconnect's replay does not take it again); focus is checked to have landed (up to 20 × 150 ms) | the operator should not hunt for what blocks the turn |
| **Unarmed after an automatic move** (reviewed 2026-09-25): an ask that took focus by itself answers no key (y, n, c, s, Enter, digits) until an arrow key, Tab or a click arms it; the panel says `↑↓ or tab to choose · typing goes to the composer`. Any other key that types (and Backspace) goes back into the composer at its caret, focus with it, and that ask does not take focus again. Letters never arm it, not even j/k/h/l: a typist's `look, can you…` must not switch tabs and then chat about it. Tab, a tab, a marker or `N waiting` (the operator's own moves) land armed | a word typed through a stolen focus must never answer |
| **Empty-composer rule**: an ask never takes focus from a field holding text; the status line says `▶ N waiting`, and Tab, a click on it, a tab or a marker go there. **Pause guard**: with the composer empty, focus moves only after 1 s without keys, and not while a menu or the `:` line is open or the panel holds an unanswered ask | typing is never interrupted |
| **Landing guard**: for 0.3 s after focus lands in the panel or the tab changes its keys do nothing; for 0.3 s after the tab or the strip changes a click does nothing but say `steady…`; the second click of a double click and IME composition keys are ignored | an Enter or click already under way must not answer what just appeared |
| Focus goes back to the composer it came from (caret kept) only when nothing is left on the strip (answered-but-unsettled counts: it may yet be refused). Esc leaves the ask pending and returns to the composer; it and the asks behind it then wait for Tab, a click or `N waiting` | the ask gives back what it borrowed |
| **Note** (optional, pending eidolon PR): Tab or the `+ note` row open it under the list; opening never answers; ⏎ on `+ note` in the note line sends nothing and says so. In it ←/→/h/l edit text; ↑/↓/Tab go back to the list keeping the note; Esc drops it. The next answer taken carries it: `POST /api/answer {"ask_id","answer","note"}`; a 400/422 marks the session note-less and the note line read-only | the note is extra, never a reason an answer fails |
| **No free-text answer**: the user's own words go through `chat about it` | one path for words |
| In the transcript the ask leaves a one-line clickable marker `? waiting · ask N  <ask>` (`✓ yes · not sent` / `· sent` once answered); a click focuses its tab. Once settled, the folded line `▸ approve: … → yes (note: …)`, openable to every choice with the pick marked. Asks are not replayed after F5 | where it arose stays readable |
| `judged` / `structural` shown plainly; `yolo: true` on an approval shows a warning chip | why the gate asked |

Reconnect check: pending asks arrive before `caught-up` and `hello.pending` equals their count (**observed** `stream.rs:64-67`, `133-139`); assert at `caught-up`.

### 7.2 Replay and reconnect

1. Every `hello` resets that pane (P2).
2. **Duplicates expected**: subscribe happens before the walk (**observed** `stream.rs:5-11`). Dedupe rows by `record`, tool lines by `id`, asks by `ask_id`. Keyless frames (`turn-settled`, `policy-verdict`, `compacted`, `context-size`, `cancelled`) are deduped by position inside the overlap window: the live duplicates are a suffix of the replay, so a keyless live frame that matches the replay's keyless run at the cursor (set by the last keyed duplicate) is dropped; the window closes at the first keyed frame the replay did not have, or 2 s after `caught-up` (`core/state.js` `dupInWindow`). Not covered: a keyless duplicate that arrives before the first keyed duplicate while the replay has a keyed frame after it.
3. **Replay is lossy** (**observed** `wire.rs:386-390`, `432`, `453`, `476-479`): no deltas, `error`, `ask-user`, `quiesced`, `trigger-fired`, or user-origin tool calls; `turn-settled` without timing; `cancelled` zeros; `tool-call-finished.name` empty.
4. **Mid-turn connect looks idle**: `hello` has no `running`, `turn-state` only on change (**observed** `stream.rs:59-72`, `177-178`). Gap 1.
5. A reply streaming at connect is invisible until its `assistant-message`.

### 7.3 `lagged`, `goodbye`, failures

| Event | Page does |
|---|---|
| `lagged` (**observed** `stream.rs:149-155`, `171-174`) | reconnect immediately, rebuild; toast |
| `goodbye` (**observed** `stream.rs:182-189`, `driver.rs:171-176`) | stop reconnecting; pane shows the text; `r` in the pane asks the hub to resume |
| stream ends with neither | `running` becomes **unknown** (`?`, as after F5), never a stale RUN; backoff 0.5 s -> 8 s; after 3 failures "door not answering" |
| `401` from the hub | hub restarted; "open Minerva again from the launcher" |
| `turn-state` lag swallowed (**observed** `stream.rs:179`) | **Only `turn-state` sets `running`.** `turn-settled` never ends it: core runs queued follow-ups inside the same turn and publishes `TurnSettled` for each (**observed** core `agent.rs:1327-1337`, `:1600`) while the driver sends no new `turn-state`, so ending on a settle showed IDLE while a follow-up ran. `cancelled` and `error` do not end it either (core sends non-fatal errors mid-turn; an `error` only closes the open draft, as a row a later `assistant-message` fills in place; a new `user-message` or `turn-state` false means none is coming (core sends none after a failed stream, **observed** `agent.rs:1611-1613`), so the next reply gets its own row). Cost: if a `turn-state` is lost to lag, `running` stays stale until the next one or a reconnect (`:reconnect`, which resets it to unknown) |
| F5 / connect mid-turn (gap 1) | `running` is **unknown** until the next `turn-state`: the badge reads `?` with "state unknown until next turn event", not IDLE, and steer and stop stay enabled |

---

## 8. Inspector data (decided; follows the focused pane)

| Tab | Content | Source | Door or hub work |
|---|---|---|---|
| `rec` | the pane's record: turns, calls, verdicts, budget | replay + live frames | none |
| `diff` ("changed") | files touched this session, with per-call diffs | derived from `edit`/`write` tool calls in the log (`tool-call-started.input` + `finished.output`) | none; user-origin calls missing until gap 8 |
| `run` ("running") | open tool calls, subagents, parks | open calls = started without finished; subagents from their tool calls; parks from `wait_for` calls + `trigger-fired` | **park-tick frame** (gap 7) for live samples |
| `tree` | file tree of the session's `cwd` | **new listing route in the hub**, `GET /s/<id>/tree?path=` | hub route with the door's static-file containment rules (no escape from root, no symlink out, **observed** model: `files.rs:71`); spec in `hub.md` |
| `graph` | a live turn graph: turn -> calls -> asks -> verdicts | derived from the log in the page | none. jev graphs from the archived line are **out** |
| (none) | OS process lists | - | **not a panel**; triage stays a model tool (`triage.md`) |

---

## 9. Aoide mesh (decided: a pane type and a left tab)

```text
+- mesh ----------------------------------------------------+
| node        paired  verified  grants          last seen   |
| lab-2       yes     yes       read spawn msg  12s         |
| vex-laptop  yes     no        read            4m          |
|                                                           |
|  this ═══live═══ lab-2 ───idle─── vex-laptop              |
|                                                           |
| 14:02 message/send  lab-2 -> this   "port open"           |
| 14:01 graphSummary  lab-2                                 |
| 13:58 probe failed  vex-laptop  timeout                   |
|                                                           |
| pair request from ops-box   code 4F-19   y accept n deny  |
| lab-2 sessions:  ● ctf-prep   ○ recon    (Enter = open)   |
+-----------------------------------------------------------+
```

| Element | Data | Source (via hub only) |
|---|---|---|
| Nodes: paired, verified, grants read/spawn/message, last seen | node list | `aoide node list --json`; `~/.aoide/state/nodes.json`; per-node `state/node-cache/<name>.json` (**observed** `client/src/node.rs:3`, `client/src/commands.rs:697`) |
| Wires live/idle | mesh view | `aoide mesh --json` (**reported**, decision list; flag not verified here) |
| Traffic log: message/send, graphSummary, ping-back, discover, probe failures | event stream | `events.jsonl` or the `aoided` unix socket (**reported**; `aoided` exists, **observed** `cli/src/bin/aoided.rs`) |
| Pairing prompts with code | pair events | same event source; accepting is a hub route that calls the CLI (see `hub.md`) |
| Peer sessions openable as panes | per-node cache | node cache + `graph.json` (**observed** graph doc builder `conduct/src/graph/doc.rs:1`) |

Rules:

| Rule | Why |
|---|---|
| The browser **never** contacts Aoide's A2A door (`127.0.0.1:8710`, house default, **observed** `client/src/commands.rs:1739`) | P8; the A2A door has its own auth model the page must not hold |
| A peer's session opened as a pane goes through the hub like any other | one path for all streams |
| **Not shown, because it does not exist**: inference clustering, node hardware, node model metadata | Aoide has none of these yet (decided/reported); the pane says "not reported by Aoide" rather than blank cells |
| Pair accept is `y`, a real authorization: show the code large and the node name in `--yellow` | same seriousness as an approval |

---

## 10. Extension model

```text
<ui-dir>/
  index.html       loads app.js as type="module"
  app.js           hub connection, panes, layout (core)
  core/            h.js, sse.js, state.js, markdown.js, tile.js, keys.js
  renderers/       one module per tool: bash.js, write.js, wait_for.js
  frames/          one module per new frame type
  panes/           session.js, picker.js, mesh.js
  manifest.json    which module handles what
  theme.css        the section 2.2 variables; owl in brand/
```

```json
{ "renderers": { "bash": "renderers/bash.js", "write": "renderers/write.js" },
  "frames":    { "scoreboard-update": "frames/scoreboard.js" } }
```

| Rule | Why |
|---|---|
| A manifest, not a folder scan | the door never lists a directory (**observed** `files.rs:71`); the hub should match |
| `fetch('manifest.json')` then `import()` per entry | an inline importmap is an inline script; CSP forbids it (reasoned from `serve.rs:357`) |
| Renderer: `export default { input(input, ctx) -> string, inputFull(input, ctx), output(output, isError, ctx) }`, where the last two return **a DOM Node (built with `textContent`) or a Preact vnode**; frame: `export default (frame, ctx) -> void`; `ctx` = `html`, `h`, `K` (clickable text), `markdown`, `useState` (later `addRow`, read-only `state`) | learnable from one example |
| Renderers draw inside the square frame and use theme variables only | keeps the terminal look under extensions |
| `try/catch` per call; on throw draw the generic line and log the module | one broken extension must not blank a pane |
| Core frames cannot be overridden, only new types added | keeps asks out of extension reach |
| Extensions are trusted code with the hub token | anyone who can write the UI dir controls every session (reasoned); review like any PR |

**Framework (decided, supersedes section 1):** vendored **Preact + `@preact/signals` + `htm`**, ESM files under `vendor/`, no build, no npm at runtime. `vendor/update.sh` fetches pinned versions from registry.npmjs.org, checks each tarball's sha512 against the registry's `dist.integrity`, and rewrites the packages' bare imports to relative paths (an importmap would be an inline script). Views read signals, so a frame re-renders only what it changed; streamed text is appended to one text node (O(n) per turn). Never `innerHTML`, `dangerouslySetInnerHTML` or `eval`. The inspector's tabs are components, so the M3 `graph` tab can be a Preact component dropped into the same slot.

`.js`/`.json` served with correct types and `no-cache` by the door (**observed** `files.rs:163-181`, `serve.rs:372`); the hub serves the UI dir and should match.

---

## 11. Terminal and editor

- **Now**: the `T` / `E` status hints call `POST /s/<id>/api/launch` **(in progress)**; the window opens natively on the door's machine, which is the browser's (loopback; argv, no shell, **observed** working-tree `launch.rs:1-10`). The page sends only `hello.cwd` or a path from a tool line.
- **Later**, in-browser terminal or editor: **the token becomes a shell**, outside the one policy chokepoint (**observed** `serve.rs:14-20`). Not planned.
- Tension: #15 says launchers "stay out of the web on purpose" (**reported**, item 3); `web-launch` builds the route (Q7).

---

## 12. Milestones (re-cut)

| M | Scope | Needs | Done when |
|---|---|---|---|
| **M1** | one pane via the hub; transcript from replay + live; draft; tool lines with generic renderer; composer (send / steer / cancel); the ask panel (7.1); reconnect/lagged/goodbye; token via redirect file; the terminal look (section 2) and status line | hub: spawn one door, proxy, redirect file (`hub.md`); gap 1 wanted | a turn with two concurrent asks is driven start to finish, and F5 mid-turn loses nothing replay carries |
| **M2** | dwindle tiling, drag resize/swap, picker (n/r/m), mirroring, `alt+w` / `:q` pane close; left sidebar sessions; inspector `rec` / `diff` / `run` | hub: list/stop doors, kept sessions; park-tick (gap 7) for live `run` | three sessions side by side, one mirrored, one closed and resumed |
| **M3** | mesh tab and pane; inspector `tree` and `graph` | hub: tree route, Aoide read routes, pair accept | a paired peer's session opens as a pane from the mesh |
| **M4** | `:` command line backed by `GET /api/commands`, `POST /api/command` | the registry (**proposed**, #15) | `:model` works the same in TUI, chat and web |
| **M5** | extension loader, fallback, isolation; `bash` and `wait_for` renderers | nothing | a member adds a renderer without touching `app.js` or `core/` |

Before M4 the `:` line offers only page-local commands (`:split`, `:close`, `:stop`, `:queue`, `:theme`) and says "session commands arrive with the registry" for the rest.

---

## 13. Commands the `:` line would show (if the registry lands)

`Run::Driver` rows (**observed** `crates/tui/src/command.rs:210-231`): `model`, `fork`, `tree`, `compact`, `exclude`, `restore`, `note`, `pin`, `unpin`, `persona`, `context`, `fresh`, `sessions`, `resume`, `new_session`, `delete_session`, `dispatch`. Stays out: `login` (#15 item 3). `yolo` is `Run::Ui` (**observed** `command.rs:669-675`) though #15 lists it as a session command (**reported**). `sessions`/`resume`/`new_session` overlap the hub's picker: split to settle in `hub.md`.

---

## 14. Gaps that need door or core work

| Gap | What makes the UI hard | Smallest change | Status |
|---|---|---|---|
| **1. Mid-turn connect looks idle** | `hello` has no `running` (**observed** `stream.rs:59-72`, `177`) | `hello.running` from `driver.running()` (**observed** `driver.rs:245`), additive | not proposed |
| 2. Model switch | `hello.model` snapshot per connection (**observed** `stream.rs:98-103`) | registry `model` + re-`hello` or `model-changed` | command proposed (#15) |
| 3. Commands | no command routes | `GET /api/commands`, `POST /api/command` | proposed (#15) |
| **5. yolo stale** | `Ctx.yolo` read once at door start (**observed** `lib.rs:231`) | read `yolo.armed()` per connection | not proposed |
| 6. Images / files | `/api/say` builds one text block (**observed** `serve.rs:413`) | `images: [{media_type, base64}]` on `say` | not proposed |
| 7. Park ticks | door reads no samples (**observed** `wake.rs:67-69`) | `park-tick {call_id, sample}` frame | not proposed; needed for `run` |
| 8. Replay gaps | `AskUser`, `UserToolCall`, `Note`, etc. skipped (**observed** `wire.rs:386-390`) | replay `UserToolCall` and `AskUser` first | not proposed |
| 9. Replay detail | `name` empty, no timing, zeros (**observed** `wire.rs:432`, `453`, `477`) | fill `name` during the walk | not proposed |
| 10. Full replay per reconnect | no SSE `id:` (**observed** `stream.rs:44-46`) | defer (hypothesis: measure a 2 000-record branch); matters more with many panes | not proposed |
| 12. Stale doc comment | `wire.rs:25-26` cites a page DOM-contract comment that does not exist | reword | not proposed |
| 14. Verdict has no call id | `policy-verdict` carries `tool, reason, outcome, note?` only (**observed** `wire.rs:61-67`); with two open calls of one tool the page cannot tell which the verdict is for, so it attaches only when exactly one open call has that name and otherwise draws its own row | add `call_id` to the wire's `PolicyVerdict`: the journal record already has `tool_use_id` (**observed** core `session/mod.rs:239-240`), additive | not proposed |
| **13. "Always this session"** | answers must be offered labels (**observed** `user.rs:95-100`); no runtime allow-rule | core: a session-scoped policy rule + an `always` answer label on approvals | **future/upstream**; the page already draws `always` greyed and enables it when an approval's `answers` list it (7.1) |

Old gap 2 (session switching) is now the hub's job; old gap 11 (token in argv) is closed by the redirect file. Gaps 1 and 5 are one-line, additive, and fix wrong information on screen: send first.

---

## 15. Open questions

| # | Question | Default if unanswered |
|---|---|---|
| Q1 | Markdown: our own safe subset, or vendor a library + sanitizer? | own subset |
| Q2 | Do gaps 1 and 5 go to Noah as a small PR now, or with the #15 discussion? | with #15 |
| Q3 | `ctrl+w` (and `ctrl+n`/`ctrl+t` if wanted) cannot be reliably captured by a page. Alternative for "close pane": `ctrl+q`, a leader chord (`ctrl+space w`), or `:close`? | `:close` + leader chord; keep `ctrl+w` only when installed as an app window |
| Q4 | Light theme: derive one, or dark only? | any light base16 scheme works (2.2); none ships |
| Q5 | Queue while running moved off Enter: `alt+enter`, `:queue`, or drop from the UI? | `alt+enter`, held by the page and editable (3.3); `:queue` still posts straight to the door |
| Q6 | `ctx %` needs the model's context window; the door sends tokens only. Hub lookup, `hello` field, or show tokens? | tokens until a source exists |
| Q7 | `/api/launch` vs #15 "launchers stay out of the web": which changes? | hold `web-launch` until answered |
| Q8 | Door is Unix-only (**observed** `lib.rs:146`; upstream Windows ruling in memory). Windows members: WSL with the browser on Windows reaching WSL loopback, and who opens the redirect file? | document WSL; test before M1 |
| Q9 | Who reviews extensions in `renderers/`, given they run with the hub token? | same review as any Minerva PR |
| Q10 | `@session` chip: what is sent, a summary, the last N messages, a peer message? Needs a defined route. | defer to M4 |

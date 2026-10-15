# webui/term/terms

The web terminals' side of the page: the ⌨ tab in the right panel, one xterm per terminal, the agent's activity while the tab is not showing,
and the terminal panes (T4b). The shells live in the perch (`perch/src/term/README.md`); this folder draws them and sends what is typed.

| file | does |
|---|---|
| `index.js` | registers the ⌨ tab (`rightTabs`, global, with the activity badge), the frame tap, and starts the activity poll; imports `view.js` so the screens are wired |
| `model.js` | the roster (metadata only, from `GET /hub/term` and the stream), the one terminal stream (`/hub/term/events`, opened while a view shows, closed 10 s after the last), each terminal's input (one POST in flight, raw `application/octet-stream`, never retried, never in the lane), and the pure helpers: `vet`, `rankRows`, `foldDefault`, `diffRoster`, `inputQueue`, `decodeB64`, `clipText`, `refusalWords`, `sinceOf` |
| `view.js` | one xterm per terminal, made when its terminal first reaches the roster (the stream's list or the poll) after the terminal font is ready (the vendored module loads then, once); moved between hosts with `mountTerm` / `unmountTerm`, never copied; the page's colours from `--base0X`; `QUERIES`, the queries the page swallows, taken on the screen's own parser (`create` makes a screen, `push` writes it, both exported for the tests); `LINKS` and `linkUrl`, the only links that open (http and https, on ctrl+click); a shelf out of sight for the screens no host shows; backlog past 2 MB resyncs the terminal |
| `tab.js` | the ⌨ tab: the yolo line, `[ new terminal ]` (right-click: this chat's folder, home, in folder…), the stack in roster order with CSS `order` (this chat, then others; no DOM moves), folds, each header's state, age, `agent typing`, the agent's last input line (folded or not), `[ take over ]` / `[ hand back ]`, `⇱`, `→ in a pane`, and the clipped live view of the last 8 rows |
| `activity.js` | what an agent does in a terminal while ⌨ is not showing: a frame tap (`term_open`, `term_send` on open chat streams), the roster poll (every 5 s through the lane while the stream is closed), the rail badge, a toast with `[ show ]`, one event per terminal per 10 s (an open with no id yet only raises the badge; the terminal's own open, from the stream or the next poll, is the toast), and the focused chat's open switching the rail to ⌨ once per terminal |
| `terms.css` | the stack, the clipped views (`--tch`, one row's height, set from the measure), the park shelf, and the rail badge `.bd` |
| `README.md` | this file |

## The contract (what T4b and later read)

- `mountTerm(id, el)` / `unmountTerm(id, el)`: a terminal's one element goes into `el` (a stack item's clip, or a pane's host) and back to the shelf. A second mount moves it; `unmountTerm` leaves it alone when `el` is no longer the host.
- `screenOf(id)` is the xterm for a terminal (null until its module is in); `mountedIn(id)` is the host element now showing it.
- `watch()` opens the stream for one view and returns the function that gives the view back (a pane counts as a view).
- `roster`, `status`, `live`, `typed(id, bytes)`, `bell`, `setHold`: the signals and the input path.
- A terminal pane is an xpane of kind `term` whose data names the terminal as `term`: `host.openXPane('term', { term: 't4' })`. (`openXPane` spreads its data and then sets `id` to the pane's own, so the terminal's id is not kept under `id`.) The stack finds the pane by that pair (`paneOf` in tab.js).
- The stream's frames are in `perch/src/term/README.md` ("The stream"). The routes used: `GET /hub/term`, `POST /hub/term` (`{chat}` or `{}`), `POST /hub/term/<t>/input` (raw bytes), `POST /hub/term/<t>/hold` (`{hold}`), `GET /hub/term/events?since=`.

## Decisions applied (BRIEF and WAVE3)

- D11: the terminals use the system mono (`"Cascadia Mono", Consolas, monospace`), not the page's font. The terminal stream takes one of the four connection slots (`extraStreams`), so the chats get three, and it opens only when a slot is free ("terminals need a stream slot"). With the ⌨ tab not showing, a focused chat's agent opening a terminal switches the rail to ⌨ once per terminal and never moves the keyboard focus.
- The xterm link handler is set on every terminal: an OSC 8 link is opened only when it is http or https, on ctrl+click, with `noopener`. No clipboard addon is vendored, and no `window.open` is reached for another scheme.
- Queries: the page swallows the perch's answers (DA1, DA2, DSR 5 and 6, DECRQM, CSI 18 t, OSC 4, 10, 11 and 12 asking) and also DECRQSS and XTVERSION, which the perch never answers. So neither side replies to those two: a program that asks gets no answer, never two. The orchestrator accepted this (no reply beats one reply from every open page), and `perch/src/term/README.md` now says the same.
- Input: the perch refuses anything but `application/octet-stream` (415), so the page posts raw bytes with the one `fetch` in `model.js`, with `auth()` from `core/api.js`. `hubPost` sends JSON and cannot carry it.
- `[ new terminal ]` sends `{chat}` only, so the perch takes the chat's remembered folder (never checked against its roots, as the perch documents). An explicit folder is root-checked by the perch, so the page does not send one. `in folder…` opens the `:` line with `term ` (T4b's `:term` must take a folder for this to finish).

## Not in this folder

- The keys of a terminal pane, copy and paste, the leave-site guard, the pane's menu: `terms/pane.js` and `terms/keys.js` (T4b).
- With the ⌨ tab closed the stream is closed as well (10 s after the last view), so the screens get no output; the roster is still polled every 5 s. On the next open the stream resumes each terminal from its last seq, or sends a reset when the perch no longer has those bytes.
- Accepted cost (fix round 1 of T4a, decided against a change): the 5 s poll makes a screen for each terminal it lists, so the xterm module loads on any page where a terminal exists, even when ⌨ was never opened, and up to 16 shelf screens are kept in step. The shelf is what keeps a folded terminal's screen current, the count is the perch's table, and the module loads once. A screen made only on mount would need a bound on the bytes queued before it exists, and a resync for the bytes that came in between. Not measured in a browser.

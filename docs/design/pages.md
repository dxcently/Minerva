# The two served pages — `/ext` and `/auth`

Wave A, task **A3**. Design only: the states, the interaction model, the
information hierarchy, the JSON each page needs, and the skin's sidebar rules.
The markup is beside this file — [`ext.html`](ext.html) and
[`auth.html`](auth.html) — complete, self-contained, and rendering as-is.

Executor: **C3** (the pages and their routes) and **C5** (the sidebar links).

---

## 1. Why these exist, and what they are allowed to be

Open WebUI is compiled Svelte. Its two supported hooks ship empty and stay
that way: `custom.css` reaches colour, type, shape and visibility, `loader.js`
swaps an inline `<svg>`'s innards by fingerprint. **Neither adds DOM.** So a
form, a toggle list, a page is not something the skin can grow — it is served
by eidolon and linked from the skin. These are the first two.

That makes them siblings of `eidolon web`'s existing page, not a new product.
They inherit its stylesheet, its panel, its mark, its type ramp and its rule
about red. What they add is a fourth palette variant and one new layout.

Three things they are not:

- **Not a settings app.** Everything on both pages has a CLI twin
  (`eidolon ext …`, `eidolon secret …`) and the two must never disagree. When
  they could, the page defers: it shows what `ext list` prints and what
  `secret list` prints, in the same words.
- **Not optimistic.** No control flips before the server says it happened.
  `enable` writes `config.toml` through `toml_edit` and `disable` kills a
  process tree; both can fail, and a switch that moved first would be a lie
  about a file the operator also writes by hand.
- **Not a place a secret can be read.** §5.

---

## 2. The palette: `hoot`, a fourth variant

`crates/web/assets/styles/tokens.css` is the only file in eidolon's web tree
allowed a literal colour, and it carries three variants keyed on
`:root[data-r-theme-variant]`. Add a fourth, `hoot`, whose every value is
lifted from `webui/custom.css` (the skin) or from `tokens.css` itself. Nothing
is invented, and no rule anywhere else changes.

```css
:root[data-r-theme-variant='hoot'] {
  --wf-bg:          oklch(13% 0.020 157);       /* custom.css --color-gray-950 */
  --wf-fill:        rgba(57, 255, 106, 0.035);  /* the assistant-turn wash    */
  --wf-fill-solid:  oklch(17% 0.026 156);       /* --color-gray-900           */
  --wf-inset:       rgba(57, 255, 106, 0.08);

  --wf-chrome:      #39ff6a;                    /* the phosphor               */
  --wf-chrome-hi:   oklch(96% 0.030 150);       /* --color-gray-50            */
  --wf-glow:        rgba(57, 255, 106, 0.55);   /* the composer's bloom       */
  --wf-link-line:   rgba(57, 255, 106, 0.45);

  --wf-text:        oklch(88% 0.045 150);       /* --color-gray-200           */
  --wf-text-strong: oklch(96% 0.030 150);       /* --color-gray-50            */
  --wf-text-muted:  oklch(70% 0.055 150);       /* --color-gray-400           */

  --wf-edge:        rgba(57, 255, 106, 0.22);   /* the chat-window frame      */
  --wf-edge-soft:   rgba(57, 255, 106, 0.14);   /* the sidebar hairline       */
  --wf-edge-tick:   rgba(57, 255, 106, 0.55);
  --wf-edge-now:    #ff4d4d;

  --wf-red:         #ff4d4d;                    /* tokens.css, unchanged      */
  --wf-red-text:    #ff4d4d;
  --wf-glow-now:    rgba(255, 77, 77, 0.75);

  --wf-wash:        rgba(57, 255, 106, 0.05);
  --wf-wash-hi:     rgba(57, 255, 106, 0.085);
  --wf-wash-alt:    rgba(57, 255, 106, 0.035);

  --wf-scroll:      rgba(57, 255, 106, 0.22);
  --wf-scroll-hi:   #39ff6a;

  --wf-font-sans:   var(--wf-font-mono);
}
```

**Contrast is computed, not eyeballed**, the way `tokens.css` records it. On
this ground (oklch L 13% ≈ Y 0.0022, a shade darker than `hacker`'s `#070a0c`):
`--wf-text` **14.0:1**, `--wf-text-strong` **17.9:1**, `--wf-text-muted`
**7.5:1**, `--wf-chrome` **15.0:1**, `--wf-red` **6.2:1**. Every one clears
WCAG AA; chrome and red are used as text, which is why they had to.

Four decisions inside that block worth stating:

**The last line is the skin's one typographic law.** `custom.css` says *"mono
is the voice, not an accent"* and puts mono on `body`, inputs, buttons and
selects. `--wf-font-sans: var(--wf-font-mono)` makes that true for every rule
in the tree that reaches for the sans token, and nothing else has to change.
`p, li` drops from `1.0625rem` to `.9375rem` on these two pages, because full
mono at prose size is heavy and neither page is prose — they are readouts.

**Nothing is fetched.** `custom.css` pulls *Share Tech Mono* from Google
Fonts; these pages must not, and a webfont another tab loaded is not available
to them anyway. The face is named **first** in `--wf-font-mono` so a box that
has it installed matches the app exactly, and the rest of the stack is
`tokens.css`'s existing one so an offline box still gets a mono.

**The red stays.** `webui/owl-green.svg` spends the mark's brow red on a pale
green because a 16px favicon outside the page cannot afford a second hue. In
the page there is no such pressure, and a single-hue page cannot tell *off*
from *broken*. So `--wf-red` keeps `tokens.css`'s `#ff4d4d`, the owl's brows
render red exactly as they do on the harness page, and the budget is the one
`components.css` already sets: **one red per viewport, as a left bar, never a
perimeter.** A red box reads as an error, and a service loading nine gigabytes
is not an error. On these pages the red is spent on exactly four things, one
at a time per row:

| red | where |
|---|---|
| `.wf-panel--now` left bar | a service mid-start |
| `.chip[data-state="wedged"]` | a service recorded and not answering |
| `.chip[data-state="broken"]` | an `extension.rn` that will not compile |
| `.chip[data-state="unusable"]` | a credential file that is there and wrong |

Everything else is chrome green or grey.

**No life canvas.** The harness page paints Game of Life behind the transcript,
seeded on `tool-call-started`. These pages have no "now" to seed it from, and a
moving field behind a credentials form is noise. They keep `body::before`'s one
bloom and add `body::after` — **the skin's scanlines, borrowed verbatim**,
which is the single strongest *this is hoot* cue on the page and already
carries its `prefers-reduced-motion` opt-out.

**Variant selection.** Both pages run the same pre-paint script `index.html`
uses (`localStorage['eidolon.variant']`), with **`hoot` as the default when
unset** — and `hoot` is added as a fourth `<option>` on the harness page's
`#variant` select. An operator who set `cyber` on the harness gets `cyber`
here. The mockups hardcode `hoot` because a mockup has no storage.

---

## 3. Shared chrome

Both pages are a **two-band shell** — `grid-template-rows: auto minmax(0,1fr)`,
`max-width: 78rem` — against the harness's three. Same head, same owl (the
inline token-driven mark from `index.html`, geometry unchanged, which under
`hoot` renders as `owl-green.svg` does), same `h1` with the short glowing
under-bar, no composer.

The head's right side carries two or three **stat blocks** (kicker + one
number, the `4 / 6` form) and the **sibling nav**:

```
HARNESS · EXTENSIONS · CREDENTIALS
```

Three pages, always all three, the current one carrying the chrome edge and
`aria-current="page"`. It is `class="wf-nav"` so `base.css`'s
`a:not([class*="wf-"])` link rule leaves it alone — the mechanism is already
there.

At **800px** the head-state wraps to full width, `.ext-head` / `.cred-head`
collapse to one column with the switch or chip below the description, the
`margin-left:auto` spacers go to `0`, and the tool table drops to two columns
with the description on its own line. Nothing scrolls horizontally except a
`<pre>`, which scrolls itself. Breakpoint `52rem`.

---

## 4. `/ext`

### 4.1 Hierarchy

One `.wf-panel` per extension, in the order `ext::discover` returns (name
order). Inside a panel, top to bottom:

1. **name** — the identity, because it is also the tool-name prefix.
2. **enabled** — pinned top-right; the one decision on the row.
3. **description** — one line, muted.
4. **service** — a state chip, then port / pid / health as `kicker + value`
   pairs, then the action, then any sentence the state owes the operator.
5. **tools** — a three-column table: the **namespaced** name (`jev_choose`, as
   the model will see it and as `ext list` prints it), the description, and the
   **approval class** (`read_only` / `mutating` / `destructive`). The approval
   column is the page earning its existence over the CLI: it is what the gate
   does with the call.
6. **notes** — `register_tools`'s per-tool notes, one muted line each, on a
   left rule.

### 4.2 The states, all of them

| state | derived from | how it draws |
|---|---|---|
| **enabled, service up** | `probe.port.is_some() && probe.healthy` | chip `SERVICE UP` in chrome; port, pid, health path; `STOP`; a `<details>` with the log path |
| **enabled, starting** | this process has an `ensure()` in flight | `.wf-panel--now` red left bar; chip `STARTING`; the switch goes `data-busy` and both cells disable; a **meter** — elapsed against `ready_timeout_s` — because the real question is *hung, or loading nine gigabytes?*; the sentence about why nothing is backgrounded |
| **enabled, recorded and not answering** | `probe.port.is_some() && !probe.healthy` | chip `NOT ANSWERING` in red; `RESTART` (`btn--now`) and `STOP`; the sentence *"alive but not answering is wedged, not busy — the next session will kill it and start a replacement. Until then its tools are registered and every call fails."*; a `<details>` with the **tail of the child's log**, which `ext::ensure`'s error already carries and is the only evidence there is |
| **enabled, service down** | `probe.port.is_none()` | chip `SERVICE DOWN`, muted; `START` |
| **enabled, no service at all** | `manifest.service.is_none()` (`Status::NoService`) | chip `RUNE ONLY`, dashed border; no button — there is nothing to start; one muted sentence saying so; tools still listed |
| **disabled** | `!cfg.extension_enabled(name)` | switch on OFF; panel edge drops to `--wf-edge-soft`; the service row and the tool table go to `opacity: .55` — **the name and the switch stay at full strength**, because dimming the control you came to press is the wrong dimming; tools header reads `4 · NOT REGISTERED`; the button reads **`START ANYWAY`**, with the sentence explaining that is the command for trying one out before committing to it |
| **will not load** | `ext::manifest()` returned `Err` | `.wf-panel--now`; the **directory path** is the title, because there is no name; chip `WILL NOT LOAD`; the compiler's words verbatim in a red-edged `<pre>`; **no switch at all**, and a note saying why — nothing can be enabled that will not compile, and writing `enabled = true` for a directory no session can load would be a lie in a file the operator also writes |
| **no extensions at all** | `discover()` empty | one panel: the directory path, the two sentences `ext list` prints, and a link to `docs/extensions.md`. This is the **current** state of the box — `%APPDATA%\eidolon\extensions` does not exist — so it is not a corner case |

### 4.3 The toggle

Not an iOS switch: the skin is angular and mono. A **two-cell segmented
control** — `[ ON | OFF ]` — reading as a DIP switch. Two real `<button>`s in a
`role="group"`, each with `aria-pressed`, so it is keyboard- and
screen-reader-honest; the pressed cell carries the fill, so the state is
legible without colour. `data-on` on the wrapper drives the paint.

### 4.4 Interaction

Every control POSTs; the row disables itself and takes `.wf-panel--now` while
the request is in flight; the server answers with **that one row's new state**
and the row re-renders from the answer. No page-wide refetch: a page that
redrew six panels because one toggled would lose the `<details>` the operator
had open on another.

An error answer renders in the row's service line, in red, with the log tail in
the `<details>` — never a toast, never an alert. The row is where the operator
is looking.

**One confirm, and only one.** `disable` on an extension **whose service is
up** opens the family's modal (the `.approval` shape from `components.css`,
reused) and says the true thing: the service is machine-scoped, every open
session is using this one copy, and disabling stops it now. `disable` on a
service that is down asks nothing.

### 4.5 What it needs from the backend

**`GET /api/ext`** — never starts anything. This is `ext list`'s data.

```json
{
  "dir": "C:\\Users\\dxcen\\AppData\\Roaming\\eidolon\\extensions",
  "docs": "https://github.com/noah427/eidolon/blob/main/docs/extensions.md",
  "extensions": [
    {
      "name": "jev",
      "dir": "C:\\Users\\dxcen\\AppData\\Roaming\\eidolon\\extensions\\jev",
      "description": "the one-pass chooser and the NLI cross-encoder",
      "enabled": true,
      "load_error": null,
      "service": {
        "state": "up",
        "port": 63388,
        "pid": 24180,
        "health": "/health",
        "ready_timeout_s": 120,
        "log": "C:\\Users\\dxcen\\AppData\\Local\\eidolon\\background\\bg-1a0b610fea1-0001.log",
        "started_at": 1758201662,
        "log_tail": null
      },
      "tools": [
        { "name": "jev_choose", "description": "score a list of options against a context, one pass", "approval": "read_only" },
        { "name": "jev_entail", "description": "premise + hypothesis → contradiction / entailment / neutral", "approval": "read_only" }
      ],
      "notes": []
    },
    {
      "name": null,
      "dir": "C:\\Users\\dxcen\\AppData\\Roaming\\eidolon\\extensions\\imagegen",
      "description": "",
      "enabled": false,
      "load_error": "extension.rn: compile error\n  --> extension.rn:7:24\n…",
      "service": null,
      "tools": [],
      "notes": []
    }
  ]
}
```

- `service.state` ∈ `"up" | "wedged" | "down" | "starting"`; `service` is
  **`null`** when `manifest.service.is_none()` (the `RUNE ONLY` row) and when
  `load_error` is set. Derivation, exactly:

  | state | condition |
  |---|---|
  | `"up"` | `probe.port.is_some() && probe.healthy` |
  | `"wedged"` | `probe.port.is_some() && !probe.healthy` |
  | `"down"` | `probe.port.is_none()` |
  | `"starting"` | this process holds an in-flight `ensure()` for this name |

- `"starting"` needs a little server state: a `Mutex<HashSet<String>>` of names
  being started, inserted before `ext::ensure` and removed after. It exists so a
  **second tab** sees the start the first tab asked for. If the executor wants
  to ship without it, omit the arm and let the page track its own POST — the
  page degrades to client-only and nothing else changes. Say which was done.
- `load_error` set ⇒ `name` is `null`. The row's identity is `dir`, and its
  basename is what the page shows. The only live thing on that row is the
  documentation link.
- `started_at` is unix seconds, and is what the elapsed meter counts from.
  `log_tail` is `null` on `GET` and carries the child's last lines on a failed
  `start` (below) — a listing must not read log files for six extensions.
- `tools` is compiled, not guessed from filenames: `register_tools` into a
  throwaway `ToolRegistry`, exactly as `cli/src/ext.rs::list` already does,
  because a script's manifest may name itself something its file does not.
  **This never starts a service** — it is `load_manifests_only`'s path.

**The four POSTs.** Bodies are `{"name": "jev"}`; each answers `{"extension": { …the one row, same shape… }}`.

| route | CLI twin | notes |
|---|---|---|
| `POST /api/ext/enable` | `eidolon ext enable` | writes the `[[extensions]]` stanza through `toml_edit`; comments and ordering survive |
| `POST /api/ext/disable` | `eidolon ext disable` | **and stops the service now**, or the command is a lie until the next reboot |
| `POST /api/ext/start` | `eidolon ext start` | works on a disabled extension on purpose. `502` + `{"error": "…", "log_tail": "…"}` when the service did not answer |
| `POST /api/ext/stop` | `eidolon ext stop` | a second call is `{"stopped": null}` and not an error |

`restart` is not a route: it is `stop` then `start`, sequenced by the page, so
the failure of each half reports as itself.

**Trust boundary: unchanged.** `serve.rs::host_ok` already gates every POST
against the bound address. These routes add no new boundary and must not
invent one. Auth beyond loopback stays the open item it already is.

---

## 5. `/auth`

### 5.1 The invariant, and where it is enforced

> A key is never in a URL, never in argv, never in a log. A form POSTs the
> value in a request body and nothing else. The page shows *whether* a secret
> is stored, never the value.

Concretely, and each one is checkable:

1. **No GET returns a value.** `GET /api/auth` carries names, kinds,
   timestamps, file paths and problem codes. It carries no value, no hash, no
   prefix, no suffix, no length. `SecretStore::external_value` is the store's
   one door out and its callers are Rust consumers that set a header
   (`config::search_key`, provider token resolution). **The page is not a
   consumer.**
   *The test:* store a known sentinel, `GET /api/auth`, assert the serialized
   body does not contain it. Ship that test with the route.
2. **The value is in the body, alone.** `POST /api/secret/set` takes two
   fields — `name` (not secret) and `value` — and **ignores every other
   field**, so a browser extension that helpfully adds a `username` cannot
   smuggle it through. Accept `application/json` (what the page's `fetch`
   sends) and `application/x-www-form-urlencoded` (what the `<form>` sends when
   the script does not load).
3. **The name goes in the body too, not the path.** Not because it is secret —
   it is not — but because `validate_name` permits `/` and `:` for
   `app:<slug>/<key>` entries, and a name in a path segment would have to be
   re-escaped by every caller. Validate with `harnox::secrets::validate_name`
   before anything touches the filesystem.
4. **Nothing on this route is logged.** The handler must not `tracing` its
   body at any level, and the route must be excluded from `EIDOLON_WIRE_LOG`
   and `EIDOLON_MCP_LOG`. Write that as a comment in the handler, in the voice
   of the file, so the next person to add logging there reads it first.
5. **No-JS degrades correctly.** The `<form>` carries a real `method="post"`
   and `action`; the server answers a form post with `303 See Other` → `/auth`
   (POST/redirect/GET), so a refresh cannot resubmit. The JSON path answers
   the updated row and the page swaps in place.
6. **The field is cleared on success**, and the form is replaced by the stored
   state, so the value is not sitting in the DOM after the round trip.
   `autocomplete="off"`, `name="value"` (not `password`), `spellcheck`,
   `autocapitalize` and `autocorrect` all off, so no password manager offers
   to save it and no spellchecker ships it anywhere.

The page **states this rule on itself**, in a bordered note above the rows,
because the operator is about to paste a key into it and is owed the sentence
before they do.

### 5.2 Hierarchy

Two groups, then a third that only appears when it has rows.

**`API KEYS` — stored with `eidolon secret set`.** One `.wf-panel` each for
`deepseek`, `zai`, `openrouter`, `ollama`, `brave_search`. Inside:

1. **name**, as the operator types it in `secret set` — the two doors must agree.
2. **what it unlocks** — the provider and its model names. This is the *reason*
   to store the key and the page is worthless without it. `openrouter`'s list
   is fetched live, so it honestly reads *"whatever the account can reach — the
   model list is empty until this key is here."*
3. **stored / not stored** — one chip, top-right.
4. **facts**, when stored: written *N* days ago (`Meta.rotated`), `kind`,
   `service` (what `secret set --service` recorded). Never a value.
5. **the form**.

`brave_search` is in the same group with a different `unlocks` line, because it
is **not a provider**: it is the `search` tool's capability. *"A session with no
key here has no search at all: the key **is** the capability, and the host puts
it in a header the script can never name."* That asymmetry has to show, or the
page teaches the wrong model of the system.

**`OAUTH LOGINS`.** `google` (→ provider `antigravity`) and `chatgpt`. §5.4.

**`ALSO IN THE STORE`.** Anything `store.list()` returns that is not one of the
five. The store may hold `app:…/…` entries, a `github`, an issued token. The
page lists them with name / kind / written, a `DELETE`, and a `REPLACE` that is
**disabled for `kind: "issued"`** — `SecretStore::set` refuses to overwrite an
issued token and the page must not offer what the store will refuse. Disabled
rather than hidden, with the sentence saying why. Without this group the page
would be a hardcoded five pretending to be a view of the store, and would
disagree with `secret list` the first time anyone ran it.

### 5.3 The states, and the two affordances that were asked for

| state | draws as |
|---|---|
| **not stored** | chip `NOT STORED`, dashed; there is no fact to state, so **the form is the content** and is open by default |
| **stored** | chip `STORED` in chrome; the facts row; `REPLACE` and `DELETE`; **no visible field** |
| **stored, replacing** | `REPLACE` reveals the same one-field form, labelled *replace the stored key*, submit reads **`ROTATE`**, and `REPLACE` becomes `CANCEL` |
| **issued** | as stored, `REPLACE` disabled with its reason |
| **deleting** | a confirm |

**"Replace the key I already stored."** `SecretStore::set` *is* rotate — its
own message says `rotated`. So there is no second route and no second verb in
the store; the only design work is the affordance. A stored row shows **no
input by default**, for two reasons: an empty password field beside the word
"stored" reads as *"it's empty"*, and the page's resting state should be a
statement of fact rather than a form. `REPLACE` opens it, and the note under
it says the true thing:

> Storing again **is** rotating: the old key is overwritten and stops being
> used at once. There is no way to look at the old one first — if you are not
> sure you have the new key in your clipboard, cancel.

**The reveal toggle.** `SHOW` flips the input between `password` and `text`. It
reveals **only what you are typing right now** — it cannot reveal a stored key,
because the page never has one. On a not-stored row the note says exactly that,
so the control is never mistaken for a value reveal.

**Delete** is behind a confirm, and the confirm says what is actually lost. For
`brave_search`: *"removes the search tool from every new session — the key is
the capability, so there is no degraded search, only none."*

### 5.4 The two OAuth logins

Everything the page says about these comes from a **structural, offline probe**
— `TokenSource::probe` / `ProviderDef::readiness`, which never mints, never
refreshes, never spawns a login CLI, never opens a socket, never writes. That
promise is already documented in `crates/providers/src/readiness.rs` and the
page must not weaken it.

`readiness.rs` also says plainly what is *deliberately not claimed*, and the
page inherits both refusals:

- **Validity.** A source being present is not a token being good. Only the
  endpoint shown the token can say the second.
- **Login state.** The page says *the file is here and its shape is right*, and
  stops there.

So the two rows:

**`google` → `antigravity`.** `SourceKind::GoogleRefresh`. A Google refresh
token has no stored expiry, so the page must never claim "valid" or count down
anything. Present, or one of `missing` / `unreadable` / `empty` / `malformed`.

**`chatgpt`.** `SourceKind::CodexRefresh`; the file is `CodexCreds` and **does**
carry `expires_at`. The page can therefore say `access token expired 2h ago` or
`good for 47m` — and must immediately say what that means, because the obvious
reading is wrong:

> An expired **access** token is not an expired login: the refresh token in
> this file mints a new one on the next turn. The only real expiry is a refresh
> token that has been revoked, and nothing local can know that — the first
> request to use it finds out.

**`ATTENTION` is reserved for `SourceProblem`**, never for an expired access
token. A row with a good file and a stale access token is `LOGGED IN`; a row
with a file that is there and has no `refresh_token` is `WILL NOT WORK` in red,
with the problem named and the fix beside it.

`account_id` is shown (`user-4KqPz9`). It is not a secret — it goes in the
`ChatGPT-Account-Id` header — and it is the one thing that tells the operator
*which* account is logged in, which is the question they actually have.

**What the page does about the flows being interactive CLI commands.** See §6.

### 5.5 What it needs from the backend

**`GET /api/auth`**

```json
{
  "store_dir": "C:\\Users\\dxcen\\AppData\\Roaming\\eidolon\\secrets",
  "keys": [
    {
      "name": "deepseek",
      "curated": true,
      "stored": true,
      "kind": "external",
      "created": 1754582400,
      "rotated": 1754582400,
      "service": "api.deepseek.com",
      "provider": "deepseek",
      "unlocks": ["DeepSeek V4 Pro", "DeepSeek Flash"],
      "unlocks_note": null
    },
    {
      "name": "openrouter",
      "curated": true, "stored": false, "kind": null,
      "created": null, "rotated": null, "service": null,
      "provider": "openrouter",
      "unlocks": [],
      "unlocks_note": "whatever the account can reach — the model list is fetched live"
    },
    {
      "name": "brave_search",
      "curated": true, "stored": false, "kind": null,
      "created": null, "rotated": null, "service": null,
      "provider": null,
      "unlocks": ["the search tool"],
      "unlocks_note": "not a provider — the key is the capability"
    },
    {
      "name": "app:dong/webhook",
      "curated": false, "stored": true, "kind": "issued",
      "created": 1757836800, "rotated": 1757836800, "service": null,
      "provider": null, "unlocks": [], "unlocks_note": null
    }
  ],
  "logins": [
    {
      "name": "google",
      "provider": "antigravity",
      "command": "eidolon google-login",
      "path": "C:\\Users\\dxcen\\.config\\eidolon\\antigravity.json",
      "shape": "google_refresh",
      "state": "unusable",
      "problem": "missing",
      "diagnostic": "not present",
      "expires_at": null,
      "account": null,
      "unlocks": ["the Gemini surface"]
    },
    {
      "name": "chatgpt",
      "provider": "chatgpt",
      "command": "eidolon chatgpt-login",
      "path": "C:\\Users\\dxcen\\.config\\eidolon\\chatgpt.json",
      "shape": "codex_refresh",
      "state": "present",
      "problem": null,
      "diagnostic": null,
      "expires_at": 1758194400,
      "account": "user-4KqPz9",
      "unlocks": ["GPT-6 Astra", "GPT-5.6 Sol", "GPT-5.6 Terra"]
    }
  ]
}
```

- `kind` ∈ `"external" | "issued" | null` — `harnox::secrets::Kind`, verbatim.
- `keys` is **the five curated names unioned with `store.list()`**; `curated`
  says which half a row came from. A curated name the store does not hold is
  `stored: false` with nulls; a stored name that is not curated is
  `curated: false` and lands in *also in the store*.
- `created` / `rotated` are unix seconds, from `Meta`. The page renders
  "43 days ago" — the same phrasing `secrets::render` uses for the CLI.
- `unlocks` comes from the catalog's declared rows for that provider, so the
  page and `eidolon models` agree. It is a display string list; no ids, no
  prices, no quota — this is not the model picker.
- `state` ∈ `"present" | "unusable"`, `problem` ∈
  `"missing" | "unreadable" | "empty" | "malformed" | null` — `SourceState` and
  `SourceProblem`, verbatim. `diagnostic` is the source's own sentence, which
  already never contains a byte of the credential.
- `path` is the **resolved** path, not the `~`-form. See open question **Q3**.
- **No value, no hash, no prefix, no length**, anywhere in this shape.

**The POSTs**

| route | body | CLI twin | answers |
|---|---|---|---|
| `POST /api/secret/set` | `{"name": "deepseek", "value": "…"}` | `eidolon secret set` (stdin) | `{"key": { …the one row… }}`, or `303 → /auth` for a form post |
| `POST /api/secret/delete` | `{"name": "deepseek"}` | `eidolon secret delete` | same |
| `POST /api/auth/recheck` | `{"name": "chatgpt"}` | — | `{"login": { …the one row… }}`; re-probes, offline, mints nothing |

`POST /api/secret/delete` rather than `DELETE /api/secret` because an HTML
`<form>` cannot issue `DELETE`, and requirement 5 above says the no-JS path has
to work. Same reason `set` is a POST and not a PUT.

---

## 6. The OAuth flows — what the page does, and what is handed over

**What the page does today, and it is a finished design, not a placeholder:**
a `RUN THIS` panel holding the exact command in a `<pre>` with a `COPY` button,
the sentence describing what the command will do, and a `RECHECK` button beside
it that re-probes the credential file. Zero new attack surface, zero new
routes that spawn anything, and it works the moment C3 lands.

**The open question, handed to the executor and to whoever decides scope.** The
end state is obviously that the page drives the flow. Both commands are nearly
non-interactive already: `chatgpt-login` prints a URL and a code and polls;
`google-login` binds a loopback listener, prints a consent URL and waits for the
browser. Neither reads stdin. A page could POST `/api/auth/google/start`,
receive `{url, code}`, render the URL as a real link and the code as large mono
text, and flip to `LOGGED IN` when the flow completes. What has to be decided
before anyone writes it:

**Q1 — may a web route spawn?** Nothing in `crates/web` spawns a process today.
`run_background` lives in `crates/tools` behind the policy gate. A `/auth`
route that runs a login flow is a new capability *outside* the gate, reachable
by anything that can reach the loopback port — which is the same set as the
existing POSTs, but the existing POSTs only say things to a running session.
This is a policy decision, not an implementation detail, and it is not mine to
make.

**Q2 — who owns an abandoned flow?** `chatgpt-login` polls *"until Ctrl-C"*,
which has no meaning in an HTTP handler; it needs a deadline. `google-login`
leaves a bound listener waiting for a callback that may never come. A flow
started from a page and then closed needs an owner, a timeout, and a rule for
what a second `start` does while the first is still open. Design that with the
answer to Q1, not before.

**Q3 — the two OAuth files are in a different directory from everything else,
on this box.** Found while reading, and it blocks the OAuth half regardless of
Q1 and Q2:

- `config::config_dir()` is `dirs::config_dir()` → **`%APPDATA%\eidolon`**.
  That is where `config.toml` lives and where `secret_store` puts `secrets/`.
- `chatgpt_login::credential_path()` is `$HOME + ".config/eidolon/chatgpt.json"`,
  and `harnox::llm::credentials::expand()` resolves the `antigravity` provider's
  `token_file: "~/.config/eidolon/antigravity.json"` the same way, off
  `std::env::var_os("HOME")` — **not** `dirs::home_dir()`.
- On this box `HOME` happens to be set to `C:\Users\dxcen`, so those land in
  **`C:\Users\dxcen\.config\eidolon\`** — a second, empty eidolon config
  directory. On a Windows box where `HOME` is *not* set (the usual case; it is
  set here because Git for Windows set it), `chatgpt_login` bails with
  `$HOME is not set`, and `google_login::save_credentials` silently skips the
  `antigravity.json` write while still creating a **relative**
  `.local/state/gcli2api/creds/credentials.db` under whatever the current
  directory happened to be.

The page cannot show an honest path until this is settled. Three ways out,
smallest first: (a) `/auth` resolves and displays the same `expand()` the
provider uses, and says nothing about whether it is the right place — honest,
and leaves the trap; (b) `expand()` falls back to `dirs::home_dir()` when
`HOME` is unset, which fixes the crash but keeps two directories; (c) both
credential files move under `config_dir()`, with a one-time migration, so
eidolon has one config directory on Windows. **(c) is the right answer and it
is not this task's to take.** Whichever is chosen, `GET /api/auth` returns the
resolved path and the page prints it, because "where is my credential" is a
question the operator will have.

---

## 7. The sidebar links, for `webui/custom.css`

### 7.1 The rules

Append to `custom.css`, directly after the Notes/Workspace block, in the same
shape: hide whatever the app put in the icon well, then paint the glyph on the
well itself so the two can never double up.

```css
/* ─── sidebar nav: the two pages eidolon serves ─────────────────────────── *
 * Same mechanism as Notes/Workspace above — anchor on the href, the one
 * stable hook on a compiled-Svelte nav — with one difference: `$=` rather
 * than `=`, so the rules hold whether the row points at a same-origin path
 * (`/ext`) or at eidolon's own port (`http://127.0.0.1:8081/ext`). Nothing
 * Open WebUI ships ends in `/ext` or `/auth`, so it cannot collide.
 *
 * dinkie-icons: jigsaw-puzzle-piece-small (extensions — the same glyph the
 * skin already uses for #integration-menu-button, because an extension IS a
 * tool bundle and the skin should teach one glyph once), lock-filled
 * (credentials).
 *
 * NOTE: these anchors do not exist in Open WebUI 0.11.3's compiled nav —
 * see docs/design/pages.md §7.2. The rules are inert until something puts
 * them there, and inert is the correct failure: no selector matches, no
 * style is applied, nothing breaks. */
.dark a[href$="/ext"] > div:first-child > svg,
.dark a[href$="/auth"] > div:first-child > svg {
  display: none;
}
.dark a[href$="/ext"] > div:first-child,
.dark a[href$="/auth"] > div:first-child {
  width: 1.05rem;
  height: 1.05rem;
  background-color: currentColor;
  -webkit-mask-repeat: no-repeat; mask-repeat: no-repeat;
  -webkit-mask-position: center;  mask-position: center;
  -webkit-mask-size: contain;     mask-size: contain;
  image-rendering: pixelated;
}
.dark a[href$="/ext"] > div:first-child {
  -webkit-mask-image: url("data:image/svg+xml,%3Csvg%20xmlns%3D%27http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%27%20viewBox%3D%270%200%208%209%27%3E%3Cpath%20fill%3D%22currentColor%22%20d%3D%22M3%209h3V8h2V5H7V4h1V1H6V0H3v1H1v3h1v1H1v3h2Zm1-1V7H2V6h1V3H2V2h2V1h1v1h2v1H6v3h1v1H5v1Zm0%200%22%2F%3E%3C%2Fsvg%3E");
  mask-image: url("data:image/svg+xml,%3Csvg%20xmlns%3D%27http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%27%20viewBox%3D%270%200%208%209%27%3E%3Cpath%20fill%3D%22currentColor%22%20d%3D%22M3%209h3V8h2V5H7V4h1V1H6V0H3v1H1v3h1v1H1v3h2Zm1-1V7H2V6h1V3H2V2h2V1h1v1h2v1H6v3h1v1H5v1Zm0%200%22%2F%3E%3C%2Fsvg%3E");
}
.dark a[href$="/auth"] > div:first-child {
  -webkit-mask-image: url("data:image/svg+xml,%3Csvg%20xmlns%3D%27http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%27%20viewBox%3D%270%200%2012%2012%27%3E%3Cpath%20fill%3D%22currentColor%22%20d%3D%22M0%2012h11V6H9V2H8V1H3v1H2v4H0Zm9-1V7h1v4ZM4%206V3h3v3Zm0%200%22%2F%3E%3C%2Fsvg%3E");
  mask-image: url("data:image/svg+xml,%3Csvg%20xmlns%3D%27http%3A%2F%2Fwww.w3.org%2F2000%2Fsvg%27%20viewBox%3D%270%200%2012%2012%27%3E%3Cpath%20fill%3D%22currentColor%22%20d%3D%22M0%2012h11V6H9V2H8V1H3v1H2v4H0Zm9-1V7h1v4ZM4%206V3h3v3Zm0%200%22%2F%3E%3C%2Fsvg%3E");
}
```

`lock-filled` is already in `webui/dinkie-bodies.json`, so nothing has to be
fetched from the Iconify API to ship this. The jigsaw URI is the byte-identical
one already in `custom.css` for `#integration-menu-button`.

### 7.2 The problem those rules cannot solve, and it is C5's

**The anchors are not there.** The nav rows in 0.11.3 are hard-coded template
literals in the compiled bundle, one per route:

```
<a href="/workspace" draggable="false" class="flex flex-1 h-[1.6875rem] …">
  <div class="self-center"><!----></div> <div class="self-center truncate"> </div></a>
```

Every `href` the sidebar renders, confirmed against the installed package:
`/`, `/admin`, `/automations`, `/calendar`, `/notes`, `/playground`,
`/workspace`. None is data-driven and there is no configuration key — no
`CUSTOM_LINK`, no extra-links list — that adds one. CSS cannot create an
element, and the rules above will match nothing until something does.

Three ways to make them exist. **This is C5's call, and it is the one place
this design stops short of an answer:**

- **(a) `loader.js` clones a nav row.** It is already arbitrary JS with a
  `MutationObserver` coalesced to one pass per frame — exactly the machinery a
  Svelte re-render needs. Cloning the `/notes` row, re-pointing its `href` and
  its label, and letting the CSS above paint the icon is perhaps fifteen lines.
  It is also **the first time the skin adds DOM**, which is a standing rule
  (`Decisions.md`, 2026-09-17) and should be broken deliberately or not at all.
- **(b) Do not use the sidebar.** eidolon serves the two pages at its own port
  and they link to each other; reach them from a bookmark or from the harness
  page's nav. Costs nothing, breaks no rule, and the rows above sit dormant in
  `custom.css` until (a) or (c) happens.
- **(c) Reverse-proxy and repurpose.** Put eidolon behind the Open WebUI origin
  and take over an unused route. Dead end as stated: repointing a row still
  needs DOM, and Open WebUI's SPA router would claim the path before the
  network saw it.

**Recommendation: (b) now, (a) when someone is willing to amend the
2026-09-17 decision** — and if (a) is taken, amend it in writing first, in
`Decisions.md`, because "the skin never adds DOM" is a load-bearing claim and a
silent exception is worse than a stated one. Either way the CSS above ships
with C5: it is inert when unmatched, and it is the whole of the styling work.

---

## 8. What the executor builds

| file | what |
|---|---|
| `crates/web/assets/ext.html` | from `docs/design/ext.html`, `<style>` removed, specimens removed |
| `crates/web/assets/auth.html` | from `docs/design/auth.html`, same |
| `crates/web/assets/styles/tokens.css` | **+** the `hoot` variant block (§2) |
| `crates/web/assets/styles/pages.css` | **new** — everything in the mockups' `<style>` below the `components` band |
| `crates/web/assets/js/ext.js` | the four POSTs, the one confirm, the elapsed meter |
| `crates/web/assets/js/auth.js` | the three POSTs, the reveal, the confirm |
| `crates/web/src/assets.rs` | six new rows in `ASSETS` |
| `crates/web/src/serve.rs` | `GET /ext`, `GET /auth`, `GET /api/ext`, `GET /api/auth`, and the seven POSTs, in the one `match (Method, path)` |
| `crates/web/assets/index.html` | a fourth `<option value="hoot">` and the sibling nav in `.head-state` |
| `webui/custom.css` | §7.1, appended |

The mockups are **finished markup, not sketches**: keep the classes, the
`data-*` hooks and the comments. Both render correctly opened from the
filesystem — check that before and after moving the CSS out, because a page
that only works when served is a page nobody can debug offline.

## 9. Open questions, collected

| # | question | who |
|---|---|---|
| **Q1** | May a `crates/web` route spawn a process? A page-driven OAuth flow is a capability outside the policy gate. | a decision, not an executor |
| **Q2** | Ownership, deadline and re-entry for a page-started login flow — `chatgpt-login` polls forever, `google-login` leaves a listener bound. | with Q1 |
| **Q3** | Two eidolon config directories on Windows: `%APPDATA%\eidolon` for config and secrets, `$HOME/.config/eidolon` for the two OAuth files, and `$HOME` is frequently unset. Fix `expand()`, move the files, or display and live with it. | a decision; blocks the OAuth half |
| **Q4** | How the `/ext` and `/auth` anchors get into Open WebUI's sidebar: `loader.js` adds DOM (and `Decisions.md` is amended), or the sidebar is not used. §7.2. | C5 |
| **Q5** | Server-side `"starting"` set, or client-only? Only matters for a second tab. §4.5. | C3, say which |

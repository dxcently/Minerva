# The Minerva skin

Operator direction (2026-09-20): keep terminal green and monospace. CSC is a
structural reference, not authorization to switch Minerva to blue chrome.


`custom.css` is the whole skin. Open WebUI requests `/static/custom.css` on
every page load and ships that file empty, so it is the supported theming
hook — nothing here patches the frontend bundle. `hoot up` / `hoot webui`
copy this directory into the installed package on every launch, so a
`uv tool upgrade open-webui` that blanks it heals on the next start.

| file | what it is |
|---|---|
| `custom.css` | the skin: palette, type, borders, a handful of id-anchored icon masks |
| `loader.js` | **generated** — the runtime icon swapper (165 icons) |
| `build-loader.py` | generates `loader.js` |
| `owui-icons.json` | every icon the bundle ships, fingerprinted (the build input) |
| `icon-map-full.json` | id → dinkie name, the reviewable record of what maps to what |
| `dinkie-bodies.json` | the dinkie path data those names resolve to |
| `owl-green.svg` | the club owl, recolored phosphor |
| `brand/` | the owl rasterized into every slot Open WebUI reads |

## What CSS can and cannot do here

The frontend is compiled Svelte. CSS reaches color, type, shape, borders,
spacing and visibility — that is most of what you see. It cannot add or move
DOM, change what a button does, or rewrite an inline `<svg>`'s path data.
Anything structural belongs in `eidolon web`, not here.

## Icons

Open WebUI compiles 272 icons as inline `<svg>`. There is no icon font to
repoint, so the skin reaches them two ways.

**`loader.js` — the runtime swapper, and the one that does the work.**
`index.html` loads `/static/loader.js` deferred and ships it empty: the JS
twin of the `custom.css` hook. Nothing here patches the application bundle.
Every shipped icon is fingerprinted by its normalised inner markup; when one
is recognised, only its innards and `viewBox` are replaced. The `<svg>`
element, its classes, its sizing and its `aria-label` stay exactly as the app
wrote them. A `uv tool upgrade open-webui` cannot half-break this — at worst a
fingerprint stops matching and that icon quietly stays as shipped.

It has to be a runtime swap rather than CSS because the frontend is compiled
Svelte: the icons are inline `<svg>` with no stable per-icon hook, and CSS
cannot select by path data. Matching the markup can. A Svelte re-render would
defeat a one-shot pass, so a `MutationObserver` re-sweeps, coalesced into one
pass per frame.

**`custom.css` masks — the older, id-anchored path**, still covering ~19
buttons that carry a stable `id`. It hides the shipped glyph and paints a
`mask-image` in its place. The two mechanisms compose safely (the hidden
`<svg>` is the one the swapper rewrites, so they can never double up), but
the loader is the one to extend.

Set: [Dinkie Icons](https://github.com/atelier-anchor/dinkie-icons) by
atelierAnchor, **MIT**. A pixel-grid set, which is why it suits the terminal
theme; `image-rendering: pixelated` keeps the grid crisp.

### Coverage: 165 of 272, and why the rest stay

dinkie is an emoji-style pack. It is rich in objects and faces and has almost
no UI chrome: there is no plus, no minus, no chevron, no wastebasket, no
warning triangle, no ellipsis, and nothing for `B` / `H1` / italic /
strikethrough. Those buttons keep their shipped Heroicon **on purpose** —
that is a result, not an omission. Vendor logos (Google, Drive, OneDrive)
stay too: a brand mark redrawn in another pack is just wrong.

If the chrome ever needs to match, the answer is a second pixel-grid pack
alongside dinkie (pixelarticons is MIT and has exactly those glyphs), not a
forced dinkie match.

### Three traps, all already handled

1. **Clipped `-small` variants.** Icons ending `-small` declare an `8x8`
   viewBox but draw down to `y=9`. Rendered as published, their bottom row is
   cut off. `fit_viewbox()` in `build-loader.py` measures each body's real
   extent and widens the viewBox to fit.
2. **Stroke vs fill.** The shipped Heroicons are mostly stroked; dinkie draws
   with fill. A leftover `stroke` smears the pixel grid, so the swapper drops
   `stroke` / `stroke-width` and sets `fill: currentColor`.
3. **Unstable selectors.** The CSS path targets only ids verified present in
   the shipped bundle. Tailwind utility classes and file hashes change between
   releases; ids mostly do not. The loader sidesteps this entirely by matching
   path data instead.

### Derived icons

dinkie has `thumbs-up-sign` and no thumbs-down, which would have left the
dislike button as the one stray Heroicon in a row of pixel art. A mapping
entry may carry `"transform": "rotate180"`, and `build-loader.py` bakes it
into the markup as a `<g transform>` — a thumbs-down *is* a thumbs-up turned
through half a circle, which is how most packs draw the pair. Baked into the
markup rather than applied as CSS so the app's own classes cannot fight it.

### Adding an icon

1. Find its id in `owui-icons.json`. To see what an id looks like, render a
   contact sheet — agents and people both map badly from raw path data and
   well from a picture.
2. Find a name in the set: <https://icon-sets.iconify.design/dinkie-icons/>
3. Append `{"id": N, "depicts": "...", "icon": "<name>"}` to
   `icon-map-full.json`, add the body to `dinkie-bodies.json` (fetch it from
   `https://api.iconify.design/dinkie-icons.json?icons=<name>`), and rerun
   `python webui/build-loader.py`.
4. Look at it rendered before shipping. An icon that reads wrong is worse
   than the one it replaced.

### Sidebar nav

`Notes` and `Workspace` render their icon into a slot with no id to grab, and
came through as bare labels. `custom.css` anchors those two on `href` —
the one stable hook on a compiled-Svelte nav — and paints the glyph on the
icon well itself, hiding whatever the app put there so the two cannot double
up.

## Branding

`WEBUI_NAME=Minerva` plus `WEBUI_NAME_SUFFIX=false` drops the
`(Open WebUI)` suffix, and `brand/` replaces the logo.

> Open WebUI's LICENSE (clause 4) permits altering its branding **only** for
> deployments with **50 or fewer end users in a rolling 30 days**, or with
> written permission / an enterprise license. A personal box, or one install
> per club member, is well inside that. One shared instance in front of more
> than fifty people is not — put the branding back before doing that.

The suffix is an opt-out flag rather than a deletion, so reverting is one
environment variable.

Native provider credentials live under Settings > Admin System > Authentication.
minerva_credentials.py supplies administrator-only, same-origin WebUI routes to
Eidolon's existing credential API. Saved values are never returned. No iframe is
used. OAuth login commands remain terminal actions; status and recheck are native
controls. hoot webui installs the router and generated loader on launch.

# vendor/

Third-party code the page runs, as plain ES modules served from the door's own origin. No build step, no npm at runtime, no CDN. Everything here is written by `update.sh` from `VERSIONS`; do not edit a file in this folder by hand.

## What is here

| file | package (see `VERSIONS` for the version) | taken from the tarball |
|---|---|---|
| `preact.js`, `hooks.js` | preact | `dist/preact.module.js`, `hooks/dist/hooks.module.js` |
| `signals-core.js` | @preact/signals-core | `dist/signals-core.module.js` |
| `signals.js` | @preact/signals | `dist/signals.module.js` |
| `htm.js` | htm | `dist/htm.module.js` |
| `graphlib.js` | @dagrejs/graphlib | `dist/graphlib.esm.js` |
| `dagre.js` | @dagrejs/dagre | `dist/dagre.esm.js` |
| `xterm.js`, `xterm.css` | @xterm/xterm | `lib/xterm.mjs`, `css/xterm.css` |
| `addon-fit.js` | @xterm/addon-fit | `lib/addon-fit.mjs` |
| `LICENSE-*` | each package | `LICENSE` |

Exports the page uses: `dagre.js` has `layout`, `Graph`, `graphlib` and a default; `graphlib.js` has `Graph`, `alg`, `json`; `xterm.js` has `Terminal`; `addon-fit.js` has `FitAddon`.

## Pins

`VERSIONS` has one line per package: `name version sha512-...`. The hash is the sha512 of the npm **tarball** (the registry's `dist.integrity`), not of the files in this folder.

```
sh vendor/update.sh                     re-fetch what VERSIONS pins, write the files
sh vendor/update.sh --check             same, write nothing, fail if the folder differs
sh vendor/update.sh --bump <pkg> <ver>  move a pin that exists, then re-vendor
sh vendor/update.sh --add  <pkg> <ver>  pin a package the script already has a block for
```

Needs sh, curl, tar, sed, openssl, base64, cmp. Network goes only to `registry.npmjs.org`; any tarball URL outside it is refused. A pinned version is never trusted to the registry: the tarball must match the pinned hash or the run stops. `--bump` and `--add` trust the registry's `dist.integrity` once, for the one new version, and write nothing until every check has passed. Read the diff.

After the hash check, for every package the script unpacks the one file the page needs, rewrites bare import specifiers to `./x.js` (an importmap would be an inline script, which the door's CSP forbids), drops the `sourceMappingURL` line (the `.map` files are not vendored), and then fails if

- any bare import specifier is left, static or `import("x")`;
- a `.js` file calls `eval(`, `Function(` or `new Function` (the CSP has no `unsafe-eval`). A hit may be a string in minified code, so read it before you overrule it.

`--check` also fails on a missing file or a file this script does not produce.

## Adding a package

1. Write its block in the `case` of the loop at the bottom of `update.sh` (which file to `take`, which `LICENSE-*`) and add its name to `$pkgs`.
2. `sh vendor/update.sh --add <pkg> <ver>`. Add several packages one run at a time; packages in `$pkgs` that are not pinned yet are skipped, with a note, in `--add` mode only.
3. Add the files to the table above and to `FILES` in `../tests/vendor.test.mjs`.
4. Read the diff, run the tests, commit.

Never vendor the xterm `webgl` and `canvas` addons (the canvas one exists only in 5.x), the `clipboard` addon (OSC 52 lets program output write to the clipboard) or the `web-links` addon. The test fails on a file with one of those names.

## Notes per package

- **dagre.js is self-contained.** Its ESM build bundles its own graphlib (`dagre.graphlib`); it has no import at all, so the `@dagrejs/graphlib` to `./graphlib.js` rewrite in `update.sh` matches nothing today. `graphlib.js` is a separate copy of the same library; a graph built with either is accepted by `layout()`. Use one of them, not both, in a view.
- **dagre.js's last comment** points at `dagre.esm.js.LEGAL.txt`, which is not vendored. It holds the same MIT text as `LICENSE-dagre`.
- **xterm.js has no imports** and exports only `Terminal`; `addon-fit.js` has none either (the `@xterm/xterm` rewrite matches nothing today). `xterm.css` is loaded with a `<link>`.
- **xterm and the CSP.** xterm 6 (DOM renderer) inserts three `<style>` elements (scrollbar colours, row dimensions, theme colours), sets many `element.style.x = ...` values, and uses `setAttribute("style", ...)` for per-cell colours. The door's `style-src 'self' 'unsafe-inline'` allows all of it; a stricter `style-src` would break the terminal. It calls neither `eval` nor `new Function`, and starts no worker, `fetch` or WebSocket of its own. It does contain a `window.open` call: with the `linkHandler` option left at its default (`null`), a click on an OSC 8 hyperlink in the output asks `confirm()` and then opens the URL. Output can therefore make clickable links; set `linkHandler` where the Terminal is created if the page should not allow that.
- **`addon-fit.js` reads xterm's private `_core._renderService`**, so it is only as compatible as the pair of versions pinned together: bump them together.

## Line endings

The pins are checked on the bytes in this folder, so they must be LF on every checkout. `.gitattributes` has `webui/term/vendor/** text eol=lf`; without it `core.autocrlf=true` turns them into CRLF on Windows and `--check` reports every file. A checkout from before that line still has CRLF files until `sh vendor/update.sh` rewrites them (git itself sees no change, because it normalises on compare). `tests/vendor.test.mjs` fails on any CR here.

## Tests

`node --test webui/term/tests/vendor.test.mjs` runs offline: the pins are well formed, every file and licence is present, no stray addon, no CR, no bare import, no `eval`, `Function` or `new Function`, `xterm.js` exports `Terminal`, `addon-fit.js` exports `FitAddon`, and `dagre.js` lays out a 3-node graph (with either graphlib). The tarball hashes need the network: `sh vendor/update.sh --check`.

# src/meta/ — sources for the loaded and diff tabs (`/hub/meta/...`)

What the loaded tab lists (tools, plugins, the policy rows, the guard's status, the home's defaults) and the diff tab's branch line.

```text
mod.rs     State, route(ctx, req, rest) -> Response<BoxBody>: GET /hub/meta/tools|plugins|policy|connectors|home|git, the 10-minute caches
parse.rs   pure: eidolon's tool stream, the plugin names in its listing, the policy file on the `yours:` line, `~` paths, text bounds
git.rs     pure over files: a folder's `.git/HEAD` as a branch or a detached sha, no git run, no link followed
```

## The routes

| Route | Answer | Where it comes from |
|---|---|---|
| `GET /hub/meta/tools` | `[{name, approval?, plugin?, desc?}]`, eidolon's order | `eidolon [--config C] tools`, parsed as a stream of JSON objects, **never `--live`** (that asks the Melete gateway over the network) |
| `GET /hub/meta/plugins` | `{text}` | `eidolon [--config C] plugins`, as printed (control characters dropped, cut at 64 KiB with a line that says so) |
| `GET /hub/meta/policy[?mode=auto\|manual\|yolo]` | `{mode, file, rows, guard: {on, file, why?}}` | `eidolon --config <the mode's config> policy-rows`; `file` is the path on its `yours:` line as a `~` path; `guard` is `guard::status()` |
| `GET /hub/meta/connectors` | `{mneme, melete}` booleans | the config the doors run with: is `[mneme]` / `[melete]` a table. Nothing else of it is read into the answer |
| `GET /hub/meta/home` | `{default_mode, source}` | the registry's default mode, and `"minerva.toml"` when that file names one, else `"unset"` |
| `GET /hub/meta/git?session=<id>` | `{branch}`, `{branch: null, sha}` (detached), `{branch: null}` (no repository) | `<cwd>/.git/HEAD` (see `git.rs`) |

`mode` picks the config as a door of that mode would (`Registry::door_flags`): manual is `config-manual.toml`, auto and yolo are `config.toml`
and share one cached answer. No `mode` is the mode a new chat starts in. Manual with no manual config is a 409 with the registry's sentence.

## What is asked of eidolon, and how

`titles::run`: the binary the perch was given (`--eidolon`, the same one the doors are), `--config` first, stdin closed, stderr dropped,
a timeout (20 s; `--knob meta_timeout_ms=N` shortens it for a test), a byte cap (8 MiB for the tools, 1 MiB for the text commands), and the
child killed when the run is dropped. A failure is a 502 (504 for a timeout) with a sentence and **never the cause**, and is not cached.
The cache lock is held across the run, so two tabs opened together cost one command. `tools` also reads `plugins` (cached) for the
attribution below, and the lock order is tools then plugins.

eidolon prints no plugin on a tool: a plugin's tool is namespaced `<plugin>_<file>` and that is all. `plugin` is the longest plugin name in the
`plugins` listing for which the tool's name is `<plugin>_<rest>`. A tool of the user's own that happens to share a plugin's prefix is
attributed to it; the listing has no way to tell them apart.

## The git route

The same request shape as `/hub/tree`: the chat is `tree::base`'s (a live door or a saved chat with a known folder; 404 and 409 for the same
reasons), the folder is judged against `--root` on every request, and `tree::resolve(.., "")` applies the deny list to the folder's own
names, so a chat in `~/.ssh` has no diff tab. A full house of four slots is a 503.

`git.rs` never runs git. `.git` a directory: its `HEAD`. `.git` a file: its `gitdir: <path>` line, one hop (a relative path is relative to the
file), then that directory's `HEAD`. `.git` a symlink, or a `HEAD` that is one: nothing, answered exactly like no repository, so a link
cannot be used to ask what is behind it. Every read is `O_NOFOLLOW | O_NONBLOCK`, must be a regular file, and is at most 4096 bytes. The
output can only be a branch name (no control character, at most 255 bytes) or 40 or 64 hex digits. A repository above the folder is not
looked for.

## Limits, said once

- The text of `plugins` and `policy-rows` is eidolon's and changes with it; only the plugin names and the `yours:` path are read out of it.
- `connectors` is the one answer that does not come from an eidolon command, because none reports it. A config that does not parse is a 502;
  a config that is not there has neither connector.
- `home.default_mode` is what the perch started with; `source` is read from the file now, so an edit of `minerva.toml` after the start shows
  in `source` first and in `default_mode` after a restart.

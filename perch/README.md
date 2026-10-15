# perch — the Minerva front door

**One loopback process in front of several `eidolon web` doors.** The browser UI in
[`webui/term/`](../webui/term/) is served by the perch instead of by a door, so one page
can tile many sessions, and each session's own door token stays inside the machine —
the page ever holds only the perch's token.

`perch` is the binary. Conceptually it is `minerva perch`; there is no `minerva`
multiplexer binary yet, so it is invoked as `perch` on its own. The design it implements
is [`docs/design/hub.md`](../docs/design/hub.md), which still calls it "the hub" — the
prose rename is a separate human edit, not a code change.

| | |
|---|---|
| **Loopback only** | It refuses any bind that is not `127.0.0.0/8` or `::1`, before a listener exists, and there is no flag that widens it. Its token is a file on this machine, not a credential for a network. |
| **MIT** | Same license as the rest of Minerva. It depends on `harnox` (MIT, a sibling checkout) and on no `eidolon-*` crate at all: a door is a binary, two stdout lines, a `0600` token file and HTTP, and nothing more. No `aoide-*` crate either — those are GPL-3.0. |
| **Unix only** | It needs `$XDG_RUNTIME_DIR`, `0600` files and signals. A non-Unix build fails at `compile_error!` rather than at the first `chmod`. |
| **systemd in WSL** | `$XDG_RUNTIME_DIR` comes from systemd, so a fresh WSL needs `[boot] systemd=true` in `/etc/wsl.conf` and a `wsl --shutdown` (Getting-Started, Part 1). Without it the launcher refuses to start. |

## Run it

The one command, from Windows PowerShell: **`hoot`**. It runs `bin/perch-open.sh --code` in WSL
(starts or reuses the perch on port 47820), gets a login code, and opens
`http://127.0.0.1:47820/#code=<code>` in your browser. The page trades the code for a session
cookie and the browser is signed in. The token never leaves its `0600` file: the script gives it to
`curl` through `-H @file`, never argv, env, stdout or a URL.

```text
hoot                 open Minerva (starts the server when it is not running)
hoot web -NoOpen     print the one-time link instead of opening it
hoot status web      up or down, url, pid, session counts by state
hoot stop web        stop the perch (chats are kept)
```

From WSL the same script is `bin/perch-open.sh [--code] [--no-browser] [--status] [--stop]`:
`--code` prints exactly one line, the link, and nothing else on stdout.

It needs the binary at `~/perch-target/release/perch` (`MINERVA_PERCH_BIN` to override), built in
WSL: `cd perch && CARGO_TARGET_DIR=$HOME/perch-target cargo build --release`. The script refuses,
with that command, a binary that lacks `--home` or `--eidolon-config`. `MINERVA_PERCH_PORT` and
`MINERVA_UI_DIR` are optional. A chat may work under `$HOME`, the Minerva home and the Windows
profile folder (the script asks `cmd.exe` for `%USERPROFILE%`, maps it with `wslpath`, and adds that
`--root` itself; with none found it says so and adds nothing).

**The perch lives in WSL and dies with it.** `hoot` starts it from a `wsl.exe` that exits at once,
so it stays up only as long as the distro does. If WSL idles out (no WSL window open, nothing
running), the perch goes with it and the page shows "not answering". Fix: in
`%UserProfile%\.wslconfig` set `[wsl2]` `vmIdleTimeout=-1`, then `wsl --shutdown` once. (A
`systemd --user` unit for the perch would also do it, and would be the better home if this becomes
unattended; not built.)

**Login.** `POST /hub/code` mints a random single-use code valid 30 seconds (at most 4
outstanding); it needs the bearer **token** (the launcher's), and the cookie alone is a `403`, so a
script in the page cannot turn the HttpOnly cookie into a code another client could redeem.
`POST /hub/redeem {"code": "..."}` needs no token: a live code is consumed and answered `204` with
`Set-Cookie: minerva_<port>=<secret>; HttpOnly; SameSite=Strict; Path=/`; a wrong, used or expired
one is an empty `403`. The cookie is a fresh per-redeem secret held in memory (not the token),
accepted wherever the token is **on a request that carries `Sec-Fetch-Site: same-origin`** (the
page's own `fetch` always does; a browser that sends no fetch metadata gets a `401`), and forgotten
when the perch restarts. The bearer token keeps the old rules (curl sends no `Sec-Fetch-Site`).
Every answer behind the gate carries `Content-Security-Policy: default-src 'none'; sandbox`.

On first run `bin/perch-open.sh` also creates the **Minerva home**, `~/Minerva` (`MINERVA_HOME` to
override; only missing pieces are created): `sessions/`, `scratch/`, `projects.json` (`[]`) and
`minerva.toml` (`default_model = "chatgpt:gpt-5.6-luna"`, seeded only when missing). It then starts
the perch with `--root $HOME --home <home>` (plus the Windows profile root).

`config.toml` in the home is **generated, never hand-edited**. eidolon's `--config FILE` replaces
`~/.config/eidolon/config.toml` outright, so at every start the perch parses the member's own
`~/.config/eidolon/config.toml` (all of its keys and tables are kept; an empty table when there is
none), sets `sessions_dir` to the absolute `<home>/sessions` (always overridden: eidolon does not
expand `~` in that key, and a relative one is read from each door's cwd), sets `default_model` from
`<home>/minerva.toml` when it names one, else keeps the member's own, else `chatgpt:gpt-5.6-luna` (operator ruling 2026-10-06: the Minerva
home says first, then the member's config, then the constant), and writes the result atomically with mode `0600` under a header comment.
Edit `~/.config/eidolon/config.toml` or `~/Minerva/minerva.toml`, not the generated file. A new chat sends no `-m` unless the person typed a model for it (`:new <folder> <model>`), so the door runs this default; a bad `model` value in `POST /hub/sessions` is a 400. `default_mode = "manual" | "auto" | "yolo"` in the same file is the approval mode of a new chat that names none (absent: yolo; anything else refuses the start); see "Approval modes".

A corrupt state file (`names.json`, `archive.json`, `forks.json`, `chat-cwds.json`, `projects.json`) is renamed to `<name>.bad-<unix seconds>` with a warning and the store starts empty; it is never overwritten. The Minerva home itself is a root so the inbox works, but its own folders (`sessions/`, `trash/`, the state files, `scratch/` itself) are refused as a chat folder, a project, a promote target or somewhere to make a folder, and are not listed by the picker; only `scratch/<chat>` is a chat's place. Doors and fork runs are each their own process group, signalled as a group. The
files *beside* the member's config (`providers/`, `policy.rn`, `secrets`) are shared as they were.
The perch lists sessions from the same absolute `<home>/sessions` it wrote, so the doors' logs and
the list cannot disagree.

- `--home DIR`: generate `DIR/config.toml` as above, run every door as `eidolon --config
  DIR/config.toml web ...` (new and resumed), list `DIR/sessions`, and add `DIR` to the allowed
  roots. A `POST /hub/sessions` body with no `cwd` starts in a fresh `DIR/scratch/<three-word-name>/`
  (e.g. `quiet-owl-lantern`, never digits; unique among scratch folders, `-2`, `-3` as the last resort). Without `--home` a body with no `cwd` is a 400.
  Under `--home` eidolon's own sessions dir (the member's `sessions_dir`, else `~/.local/share/eidolon/sessions`) is a
  second, read-only source: its chats are listed `origin: "other"`, never written, moved or deleted.
  Conflicts with `--eidolon-config` and `--sessions-dir`.
- `--eidolon-config FILE` (no `--home`): every door is `eidolon --config FILE web ...`. Unless
  `--sessions-dir` is given, the perch lists the file's top-level `sessions_dir` (parsed with a TOML
  parser; the door's default when the file has none). A relative or `~` value is refused at start
  with the reason: the door would resolve it against its own cwd. The perch never writes this file.

By hand:

```sh
cd perch
cargo build --release
# the UI bundle it serves is webui/term/ in this repository
target/release/perch --bind 127.0.0.1:0 --ui-dir ../webui/term
# …and, for the session lifecycle (H1b), the flags a door needs:
target/release/perch --bind 127.0.0.1:0 --ui-dir ../webui/term \
  --root "$HOME" --eidolon eidolon --sessions-dir "$HOME/.local/share/eidolon/sessions"
```

It prints exactly two lines and then serves:

```text
listening on http://127.0.0.1:41231/
token file: /run/user/1000/minerva/perch-41231.token
```

The **token is never printed**, never in argv, never in the environment, never in a URL
and never in a log. Read it from the file the second line names (mode `0600`), and give
it to the page as `Authorization: Bearer <token>`. `$XDG_RUNTIME_DIR` is required: the
perch will not fall back to a shared temp directory, because another user could
pre-create the directory its secret goes in.

## What this slice is

This is **H1c** on top of H1a's front door and H1b's session lifecycle. H1a was the process,
its one secret, the gates every request passes and the static UI; H1b was the session
lifecycle behind those gates — spawn a door, resume a kept log, stop a live one, list them —
plus the perch's *own* watcher stream to every door it owns. **H1c is the proxy**: the page
reaches a session's door only through `/s/<id>/api/<route>`, with the door's Bearer injected
by the perch, and a stop cancels every pane and closes the watcher **before** it signals the
door.

What is **not** here, on purpose: the idle reaper, `/hub/events` (H2) and
`/hub/mesh` (H3).

```text
  /            the UI bundle from --ui-dir: no token, `Host` checked
  /hub/code                     POST: mint a 30 s single-use login code
  /hub/redeem                   POST {"code"}: no token; 204 + Set-Cookie, or 403
  /hub/sessions                 GET the list, POST to spawn a new session   (H1b)
  /hub/sessions/<id>/resume     POST: a door on that kept log                (H1b)
  /hub/sessions/<id>/stop       POST: cancel panes, close watcher, SIGTERM   (H1b, H1c)
  /hub/sessions/<id>/log        GET: a saved chat as eidolon's `log --json` lines (titles slice)
  /hub/sessions/<id>/transcript GET: the same lines with every image's bytes emptied
  /hub/sessions/<id>/images     GET: the images in a chat, as JSON
  /hub/sessions/<id>/image/<record>/<n>   GET: one image's bytes
  /hub/sessions/<id>/restart    POST {model?, mode?} (or the old {yolo}): stop + resume the same log with -m / a mode (manual, auto, yolo), between turns
  /hub/sessions/<id>/fork       POST {} (whole, live) or {at} (a pending copy, no door): a NEW chat (201 {id, from})
  /hub/sessions/<id>/first      GET state | POST {text, images?, mode?}: run `resume --at` once for a pending fork, then its door comes up
  /hub/models                   GET: the models `eidolon models` lists, cached 10 min
  /s/<id>/api/<route>           GET|POST, proxied to that session's door     (H1c)
  /hub/tree?session=<id>&path=<rel>   GET one directory of a live or saved cwd    (H2)
  /hub/file?session=<id>&path=<rel>   GET the bytes of ONE image (png/jpeg/gif/webp) under a live or saved cwd
  /hub/projects                 GET the named folders, POST {path, name?} adds one
  /hub/projects                 DELETE {path}: forgets the project at that path (never the folder)
  /hub/sessions/<id>/archive    POST: into `other` now (409 while running); `unarchive` brings it back with a fresh week
  /hub/sessions/<id>/pin        POST: pinned to the top (`unpin`); archive and trash unpin
  /hub/sessions/<id>/trash      POST {stop?}: move the log to <home>/trash (409 while running, unless stop: true)
  /hub/trash                    GET the trash [{id, title, mtime}]; /hub/trash/<id>/restore POST; /hub/trash/empty POST {confirm: true}
  /hub/dirs?path=<abs>          GET the subfolders of one folder under the roots (none: the roots)
  /hub/dirs                     POST {parent, name}: make ONE folder inside a root
  /hub/events, /hub/mesh                   ->  404                           (H2/H3)
  any other /s/...              token-gated  ->  404
```

An **unauthenticated** request to `/hub` or `/s` is a 401; an authenticated one to a route
that is not there is a 404. The proxied path is a shape, not a list of routes
(`^[a-z][a-z0-9-]{0,31}(/[a-z0-9-]{1,32})*$`), so a route the door grows needs no perch change.
Only `Content-Type` and `Accept` cross to the door, a POST body streams under the 8 MiB cap,
and a pane (`GET .../api/events`) ends with a `hub-stream-end` frame saying whether the door
is `live`, `stopping` or `dead`. A door's 401 reaches the page as a 502.

**A clean exit is `kept`, not `dead`.** A door that exits with code 0, or by SIGTERM after a stop was
asked of it, leaves no row of ours: the list shows its log as `kept` and `/s/<id>/...` answers 409
`kept`. A SIGKILL, any other code or signal, or a spawn that never came up stays `dead` (503, never
connected to). The roster (`live-elsewhere`) counts a row only while its pid is alive (`kill(pid, 0)`,
`EPERM` is alive); a crashed door's leftover `meta.json` is ignored, never deleted.

### The gates, in order (hub.md §10.2)

| # | Gate | Refusal |
|---|---|---|
| 0 | loopback-only bind, at start | no start |
| 1 | `Authorization: Bearer` on `/hub`, `/hub/*`, `/s`, `/s/*` — never a query parameter, constant time | 401, empty body |
| 2 | `Host` is the bound `ip:port` (or literally `localhost:port`) | 403 |
| 3 | `Sec-Fetch-Site`, when present, is `same-origin` or `none` | 403 |
| 4 | method and path shape; `OPTIONS` is 405 everywhere | 404 / 405 |
| 5 | body read through `Limited` at 8 MiB | 413 |

A query string on `/s/...` is 400 (`/hub` keeps its query strings — `/hub/tree?…` is
one). No `Access-Control-*` header is ever written, and `OPTIONS` never gets a preflight
answer. Static responses carry `nosniff`, `no-referrer`, `no-cache` and the bundle's CSP
on refusals as well as 200s; every response carries `nosniff` and `no-referrer`.

### `GET /hub/tree` (H2)

`/hub/tree?session=<id>&path=<rel>` lists **one directory** under a **live** session's cwd (the `cwd` its `hello` named, canonicalized when the request is served), or a **saved** (kept) chat's
remembered folder, read-only, with every check the same (the folder is re-judged against `--root` per request). Names and kinds only:
no file contents, no link target, no absolute path.

```text
  query    exactly `session` and `path` (optional = the root), once each, decoded once   400
  session  not an id / unknown                                                           404
           live-elsewhere, dead, starting, stopping                                409 {"state": "..."}
           kept with no known folder (a shared-dir chat)                                 409 {"state":"kept","error":"..."}
           kept, folder moved / gone / outside every --root                              404 {"error":"this chat's folder has moved..."}
           trashed (no row)                                                              404
  path     NUL, `\`, absolute, `.`/`..`/empty segment, > 4096 B                          404
           any segment on the deny list or shaped like an NTFS 8.3 alias (`~` + digit),
           or outside the cwd after canonicalize,
           or not a directory, or a cwd outside every --root / under a denied name       404
  503      four listings already running (a short JSON reason, not a queue)
  200      {"path":"src","entries":[{"name","kind":"file","size"}|{"name","kind":"dir"}|
           {"name","kind":"link"}|{"name","kind":"other"}],"skipped":0,"truncated":false}
```

Deny list (case-insensitive, whole name; matched on the asked path and again on the canonical
path, so a link inside the tree cannot launder one): `.git`, `.env`, `.env.*`, `*.key`,
`*.pem`, `*.p12`, `*.pfx`, `*.kdbx`, `id_*`, `*secret*`, `*.token`, `.netrc`, `.ssh`, `.gnupg`,
`.aoide`, `.envrc`, `.pgpass`, `.npmrc`, `.pypirc`, `auth.json`, `creds.json`, `*credentials*`. A
denied entry is dropped from a listing silently. A path segment (or a segment of the cwd below
its `--root`) containing `~` then a digit is refused too: on NTFS (`/mnt/c`) `GIT~1` is `.git`.
The cwd is checked against `--root` on every request, so a resumed session whose log named a
directory outside it lists nothing, and a cwd that is itself a denied name (`~/.ssh`) lists nothing.
Kinds come from `d_type` (a link is `link` and never followed, even one that points out of the
tree); only the kept regular files are `lstat`ed, for their size. At most 100 000 entries are
scanned per directory (more is `truncated`) and 4 listings run at once. Asking for a link as a
`path` canonicalizes it, and a target outside the cwd is a 404.
Non-UTF-8 names are counted in `skipped`. Dirs first, then by name (byte order); 2 000 entries
at most, then `truncated: true`. There is no config knob yet.

### `GET /hub/file` (2026-10-06): an image a tool wrote

`/hub/file?session=<id>&path=<rel>` returns the bytes of **one image file** under a **live** session's cwd, or a saved chat's remembered folder (read-only; the tree's refusals). eidolon's tool results
are text, so "an image the model produced" means a file a tool wrote into the chat's folder (`shot.png`); this is how the page shows it.

```text
query, session (live or saved), 409, slot 503   the same as /hub/tree (the same listing slots bound the blocking reads)
path rules                                 tree::resolve, the one resolver /hub/tree uses: deny list, 8.3 alias shape, canonical
                                           path under the cwd and under --root, no link out, the remainder judged by name again
                                                                                       -> empty 404 for every one of them
not a regular file (dir, FIFO)             -> 404
extension not png / jpg / jpeg / gif / webp (svg is not one), any case                 -> 415
more than 20 MiB                                                                       -> 413
leading bytes are not that format's magic, or disagree with the extension (the name
asked for and the name a link led to)                                                  -> 415
```

Headers: the right `Content-Type`, `X-Content-Type-Options: nosniff`, `Content-Security-Policy: default-src 'none'; sandbox`,
`Cache-Control: private, no-cache` (the file can change under the same name), `Content-Disposition: inline`. Token or cookie like
every `/hub` route. svg is refused on purpose: it is a document that can carry script, and `sandbox` is the only thing between it
and the page's origin if a browser is ever opened on the URL.

## Layout

```text
src/main.rs     argv, SIGINT/SIGTERM -> CancellationToken, the runtime `run` needs
src/lib.rs      bind, the runtime dir, the token mint and its removal on exit
src/gate.rs     loopback_only, token_ok (subtle), host_ok, sec_fetch_ok
src/login.rs    login codes and session cookies: mint, redeem (single use, 30 s), session_ok
src/serve.rs    accept loop, the gate order, the route match, the session routes, shutdown
src/files.rs    the static rules, ported from eidolon/crates/web/src/files.rs
src/tree.rs     GET /hub/tree: one directory under a live or saved cwd, names and kinds only
src/rawfile.rs  GET /hub/file: the bytes of one image under a live or saved cwd, tree.rs's path rules plus type, magic and size
src/dirs.rs     GET/POST /hub/dirs: the folder picker's listing and mkdir, with tree.rs's deny list and 8.3 test
src/projects.rs <home>/projects.json (the named folders) and <home>/chat-cwds.json (the folder each chat started in)
src/folders.rs  the handlers of /hub/projects and /hub/dirs
src/archive.rs  <home>/archive.json: archived / pinned marks, the 7-day idle rule (computed at listing time)
src/trash.rs    the handlers of archive, pin, trash, restore and empty
src/sessions.rs the door table, the list, the id regex, the roster scan, the pane count, the ordered stop
src/spawn.rs    argv, the fork (one long-lived thread, PDEATHSIG), the boot lines, the token file
src/watch.rs    the perch's own /api/events stream to a door, the handle that closes it, and `turn-state` (is a turn running)
src/models.rs   `eidolon models` parsed, cached 10 min, bounded; the rule for what may be a `-m` value
src/restart.rs  GET /hub/models, POST .../restart, and `bring_up` (a door on an existing log) that resume shares
src/fork.rs     POST .../fork: copy the log byte for byte to a new id, bring a door up on the copy
src/proxy.rs    /s/<id>/api/<route>: the path shape, the rebuilt request, panes, hub-stream-end
src/bin/fake_eidolon.rs   the stand-in door the tests drive, `--features test-bins` only
tests/http.rs   a real perch on 127.0.0.1:0, over a real socket, against a real child door
```

`src/files.rs` is a **port, not a dependency**: `eidolon/crates/web/src/files.rs` is the
source of the containment rules (one percent-decode, no `.`/`..`/empty segments, no NUL
or backslash, canonicalize under a root canonicalized once, regular files only, 32 MiB)
and its tests came with it. The perch must not link `eidolon-web`, so the rules live here
too.

`cargo test` starts real servers and speaks HTTP/1.1 to them by hand; the token is read
out of the file the child printed, and the child runs at `RUST_LOG=trace` so a token that
reached a log line would fail the suite.

## Still to come (not in H1c)

- **H2** — the idle reaper, the running/parked inference off the frames the H1b watcher
  already reads (a bounded line reader: the watcher itself keeps nothing after `hello`), `/hub/events`, and classifying a door that says `goodbye` and
  exits as `stopping` rather than `dead` (the frame is not parsed today).
- **H3** — `/hub/mesh`, `/hub/mesh/events`.

## Projects and the folder picker (2026-10-06)

A **project** is a folder with a name, remembered in `<home>/projects.json` (`[{name, path, created}]`, atomic, `0600`). A chat
belongs to a project when its cwd is the project's path or inside it. A chat's cwd is also kept in `<home>/chat-cwds.json`
(`{id: cwd}`) so a saved chat still has its folder after a restart: rows of saved chats in `GET /hub/sessions` carry `cwd` when
the perch started or resumed them (older logs have none; the page files those under `inbox`).

- `GET /hub/projects`: the list, in the order added. `POST /hub/projects {"path", "name"?}`: the path must be a directory under a
  `--root` and pass the picker's rules (below), else 422; the name is trimmed, at most 100 characters, no control characters (400),
  and defaults to the folder's name; the same path twice is the existing entry (200; 201 when new); 503 without `--home`.
  `DELETE /hub/projects {"path"}` forgets the project whose folder is that path (204, 404 when none; by path, never by position, so a stale
  second tab cannot remove a different entry). **No file is ever touched.**
- `GET /hub/dirs?path=<abs>`: the **subfolders** of one folder, `{path, label, parent, crumbs[], dirs[{name, path}], truncated}`.
  With no path it is the roots, labelled for a person (`/mnt/c/Users/me` is `C:\Users\me`, `$HOME` is `~`); `parent` is `null`
  there and `""` above a root. The safety is [tree.rs](src/tree.rs)'s: an absolute path with no `.`/`..`/NUL/backslash,
  canonicalized and under a root (a link out dies), every segment below the root against the deny list and the 8.3 `~digit`
  shape (on the canonical remainder and on the spelling asked), real directories only (a link, a dot-folder, a denied or aliased
  name is not offered), `read_dir` bounded at 100 000 entries and 2 000 answered (`truncated`), at most 4 listings at once (503).
  Every refusal is the same empty 404.
- `POST /hub/dirs {"parent", "name"}`: makes **one** folder (`create_dir`, never `create_dir_all`) inside a folder the listing
  would allow: 201 `{path, name}`; 400 a bad name (no `/`, `\`, `..`, control characters or leading dot, no leading or trailing
  space, at most 100 characters, not a denied or 8.3-shaped name); 422 a parent outside the roots (or denied, or missing); 409 it
  exists.
- A chat with no `cwd` is still a fresh scratch folder each time (`POST /hub/sessions {}`); the page never reuses the focused
  pane's. Its title fallback is its birth name (the folder's), never a stamp or a time.

### Names, the inbox and its folder (2026-10-06)

```
new chat -> scratch/<three-word-name>/            (title before a first message: the birth name)
first turn settles -> title = first message
   BETWEEN TURNS: scratch folder renamed to slug(title) (lowercase ascii [a-z0-9-], <= 48, -2/-3 on a clash),
                  chat-cwds.json updated, door re-brought in the mode it had: eidolon [--config C|C-manual] [--yolo] web --session <log> --cwd <new> -m <model>
rename -> custom title (names.json) beats the auto title everywhere; an inbox chat's folder follows (between turns)
promote -> folder MOVED under a chosen parent (inside the roots), registered as a project, chat reopened there
```

- Only an inbox chat (a real directory directly under `<home>/scratch`) ever has its folder renamed or moved; project folders and
  anything else never are. A chat whose turn is running is deferred (a 500 ms tidy loop retries after 2 idle ticks). A folder that
  another chat with a log also works in (a fork's) is never moved from under it (409). A failed rename keeps the old folder, logs once.
- Every reopen (resume, restart, fork, continue) passes `--cwd <chat-cwds.json folder>` when that folder still exists; eidolon's
  `web --session <log> --cwd <dir>` overrides the cwd the start record keeps. The page's pane reconnects by itself after such a restart.
- `POST /hub/sessions/<id>/rename {title}`: 200 `{id, title}`; at most 120 characters, trimmed, no control characters; an empty
  title clears the custom one; 403 for a shared-dir chat. `names.json` is `{id: title}`, written atomically, mode 0600.
- `POST /hub/sessions/<id>/promote {parent, name}`: inbox chats only; `parent` inside the roots, `parent/name` must not exist, between
  turns (409 otherwise). Same filesystem: a rename. Otherwise: scan (symlinks and special files refused), copy, verify entries and sizes,
  remove the source. 200 `{id, name, path, restarted}`.
- `POST /hub/projects/rename {path, name}`: the project's label only; its folder is untouched.
- A fork keeps its source's model and mode and is titled `<source title> (fork)`.

## Titles, saved chats and the second source

- **Titles.** Every row of `GET /hub/sessions` has a `title`, from eidolon's own outputs and never by decoding the
  binary `EIDO` log: the roster `meta.json` title while a door is live, else the fourth column of `eidolon [--config C]
  sessions` (the first message, flattened, 60 characters), else `<project> · HH:MM` (the project's name, else the folder's; `new chat · HH:MM` for a scratch
  chat, `chat · HH:MM` when the perch has not seen the chat's cwd; the time is the log's mtime, local). `eidolon sessions`
  runs at most once per source per listing, and only when a log is new or an untitled one changed; a chat that has a title
  is never asked about again. It is bounded (10 s, 4 MiB); a failure keeps the fallback names and is not retried for 30 s.
- **`GET /hub/sessions/<id>/log`** runs `eidolon [--config C] log --json <path>` (15 s, 16 MiB) and returns eidolon's lines
  as `application/x-ndjson`: 404 for an id that is not a saved chat (or a query string, or POST), 413 over the cap, 504 on
  time, 502 on any other failure. It reads only: nothing is started, and a chat that is running is readable too.
- **Images** (`images.rs`): the door never sends image bytes (a frame carries a count), so they are read from the log, through
  `eidolon log --json`, the one export door; the perch does not decode the journal. A `UserMessage` prints an image as
  `{"type":"image","source":{"type":"base64","media_type":"image/png","data":"..."},"alt":"name.png"}`; a tool result is
  text only in eidolon's log, so today only images a user attached are there (a model-made image, or one riding with a
  tool's result, would be read as `where: "assistant"` / `"tool"`; none exists in a real log yet).
  - **`GET .../images`**: `[{record, n, media_type, where: "user"|"tool"|"assistant", bytes, tool_call?, alt?}]`; `n` is the
    image's place among that message's image blocks.
  - **`GET .../image/<record>/<n>`**: the bytes, with the type the log declared and only `image/png|jpeg|gif|webp` (any other
    is a 415), `X-Content-Type-Options: nosniff`, `Cache-Control: private, max-age=3600` (a record never changes) and
    `default-src 'none'; sandbox`. A record or `n` that is not plain digits, or not there, is a 404, as is a query string.
  - **`GET .../transcript`**: the `log` route's lines with each image's `data` emptied (everything else as eidolon
    printed it, key order aside), because pictures are what push a chat past the `log` route's 16 MiB. The saved-chat page reads this.
  - One run of `eidolon log --json` (15 s, 128 MiB; over is a 413) serves all three: its result is kept per log and
    `(mtime, length)`, so ten thumbnails are one run, a live chat that grew is read again, and at most three logs are held.
    Works for a live, a kept and an `other` chat, read-only (no `--config` for `other`, as for `log`).
- **The second source** (`origin: "other"`, state `kept`) is under `--home` only. An id both dirs have is the home's. Its
  `log` runs without `--config`; `POST .../resume` on it starts a door with the home's config on that log in place
  (it then lists as a live chat without `origin`, and as `other` again once it stops).

## Model and mode: `GET /hub/models`, `POST /hub/sessions/<id>/restart`

A running door cannot change its model or its mode (`--yolo`, or the manual `--config`): all are start-up flags of `eidolon web`, and the door has no route that moves
either. So a switch is **stop the door cleanly, resume the same log with the new flags**; the chat keeps its id, its folder and
the Minerva `--config`, the log is the transcript, and the page's pane reconnects to the same id and replays it.

- **`GET /hub/models`** runs `eidolon [--config C] models` (20 s, 1 MiB; `PERCH_MODELS_TIMEOUT_MS` shortens the bound for
  tests) and answers `[{"key", "name"?, "note"?}]`: the rows whose first word is `provider:model`, in eidolon's order (the
  provider table and the notices above them are skipped). One answer is kept ten minutes for the whole process; a failure is
  never kept. 504 on time, 502 otherwise.
- **`POST /hub/sessions/<id>/restart`** with `{"model": "<m>"?, "mode": "manual"|"auto"|"yolo"?, "yolo": <bool>?}` (at least one; `yolo` is the old spelling: true is yolo, false is auto; a model is letters, digits and
  `: . _ - / @ +`, never a leading `-`): **400** bad body or model, **404** no such chat, **409** with a sentence when a turn is
  running (`wait for the turn to end`; read off the door's `turn-state` frames on the perch's own stream), the chat is
  starting or stopping, or it is saved and not running (continue it first), **200** `{"id","model","mode","yolo","restarted"}`
  (`restarted: false` when the flags were already the door's: nothing is stopped), **503** `{"error","reverted"}` when the new
  flags did not come up: the perch then starts the chat again with the flags it had, and `reverted` says whether that worked.
  What is not named is kept: a mode change passes the model the door reported, a model change keeps the mode as it was. The
  argv is `eidolon --config C [--yolo] web --bind 127.0.0.1:0 --session <log> -m <model>`, `C` being `config-manual.toml` for manual
  (see "Approval modes" below). Manual that cannot be made is a 503 before anything is stopped.
- **Effort** has no route: eidolon's `web` takes no effort flag, so there is nothing to restart with. The composer shows none.
- A resumed door's stream is now held like a new session's (it is how the perch knows a turn is running).

## Approval modes: manual, auto, yolo (2026-10-06)

Every chat runs in one of three modes. The mode is a per-chat property, shown in the page's status line and composer menu, and it
is the only thing that decides whether a tool call that needs a yes reaches the person.

```text
  manual   eidolon --config <home>/config-manual.toml web ...        reads run; everything else asks
  auto     eidolon --config <home>/config.toml        web ...        eidolon's own rules, unchanged
  yolo     eidolon --config <home>/config.toml --yolo web ...        every question is a yes (as before)
```

**Default for a new chat is yolo** (operator ruling 2026-10-06). It is `default_mode = "manual" | "auto" | "yolo"` in
`<home>/minerva.toml`; the key absent means yolo, and any other value refuses the perch's start (it never falls back to a looser
mode). `POST /hub/sessions {cwd?, model?, mode?}` takes the mode too; a word that is not one of the three is a 400. The page's
`:new <folder> [model] [manual|auto|yolo]` sends it; without one the default applies. `setup/` does not write the key.

**How manual works, and why it survives an eidolon update.** eidolon has no "ask about everything" switch, and its policy hook is
the one seam where a gate can be tightened without touching eidolon: `[policy] path` names a Rune file whose four entry points
(`classify_tool`, `classify_program`, `fs_verb_tier`, `bash_default`) answer allow / flag / deny. Rune has no `import`, so a file
cannot call another file's `classify_tool`. At every perch start `mode.rs` therefore reads **the policy auto mode runs under** (the
member's `[policy] path`, else `~/.config/eidolon/policy.rn`, else eidolon's shipped copy under `~/.config/eidolon/system/`, and
if eidolon has never run on the machine it runs `eidolon policy true` once, which unpacks its tables), renames those four entry
points to `minerva_stock_*` (a line-start edit, refused unless each name is found exactly once) and appends four wrappers
that call them. The result is `<home>/policy-manual.rn` (0600). Nothing of eidolon's table is copied into this repository, so an
eidolon update (or an operator edit of `policy.rn`) is picked up at the next perch start; an update that renames an entry point
makes manual unavailable with a sentence (`manual mode is not available: ...`, a 503 on the request that needed it, and a restart
into manual is refused before the chat is stopped), never silently weaker. The permission store (`policy.permits.rn`, plugin
vouches) sits beside the policy file, so manual has none: a vouched plugin tool still asks.

What the wrapper does, in order: a `deny` stays a `deny` (`sudo`, `shutdown`, `mkfs`, `gh repo delete`); a tool call the table
allows **and marks `read_only`** stays silent (`read`, `grep`, `Glob`, `LS`, `search`, `ask_user`, `choices_user`, `tool_search`,
`peers`, `wait_for`, `BashOutput`, `WebSearch`, a Melete read), except `fetch` and `WebFetch`, which reach the network and ask;
everything else asks (`write`, `edit`, `mneme_rpc`, `vv`, `send`, `TodoWrite`, plugin tools such as `subagent`, `browser` and `jev`,
any tool the table does not know). **Every shell leaf asks, reads included** (`ls`, `cat`, `git status`): eidolon's shell
decomposition scope-checks an in-workspace redirect (`cat > f`, `echo x >> f`) but gives the table no say in it, so a table that
leaves any leaf silent leaves that redirect silent too; the only sound table asks about every leaf. `fs_verb_tier` is `flag` for every
verb (`mkdir`, `cp`, `mv`, `touch`, `tee`, `ln`) and `bash_default` is `flag` unless it was `deny`. The models should reach
for `read` and `grep`, which do not ask. A path outside the working directory is still the algebra's refusal turned into a question.

`<home>/config-manual.toml` (0600) is the generated `config.toml` plus `[policy] path`, and nothing else differs, so `sessions_dir`,
`default_model` and every other key are identical and manual chats appear in the same list. Three `[policy]` keys of the member's are
forced because they would otherwise make manual looser: `enabled = true` (a member's `enabled = false` is no gate at all),
`yolo = false` (`[policy] yolo = true` is yolo for every launch) and `judge` removed (a judge answers questions on the operator's
behalf). `config.toml` keeps the member's own values: auto is exactly what it was. If the member's own `[policy] yolo = true` is set,
an `auto` door reports yolo in `hello` and the perch calls it yolo (`mode = reported`).

**The mode through the perch.** One value replaces the old yolo flag everywhere: the registry (a door's mode is the door's own
`yolo` word from `hello`, else the mode the perch passed; for a door this perch did not start there is no mode, and the list row
simply has none), `POST .../restart {mode}` (`{yolo: true}` is yolo and `{yolo: false}` is auto, as before; `mode` wins when both
are given; the answer is `{id, model, mode, yolo, restarted}`), a whole fork (inherits the source's mode), a fork from a point
(`forks.json` keeps `mode`; an old file's `yolo` is read as yolo or auto), `POST .../first {text, images?, mode?}` (the headless
`eidolon resume` gets that mode's `--config` and `--yolo`; with nobody to answer, a call that needs a yes is declined, which the page says),
a folder rename or move (the door comes back in the mode it had), and a reopen of a saved chat. The mode of every chat is kept in
`<home>/chat-modes.json` (0600), so a reopen after a perch restart is never looser than the chat was; a chat nobody remembers a
mode for (a log dropped in the sessions dir) gets the default. The list rows carry `mode` (`manual`, `auto`, `yolo`, or absent when
nothing here knows).

**"Allow for the rest of this chat" does not exist in stock eidolon**, so the page has plain yes / no only. The door's approval ask
offers exactly `["yes", "no"]` (`crates/web/src/user.rs`: `answers: [&'static str; 2]`, and `POST /api/answer` rejects any other
label with 422). The only standing grants eidolon has are the permits file (`policy.permits.rn`, vouches for plugin verbs and secret
sources, written by `eidolon plugins trust`): global to every chat and to plain eidolon, not per chat, and not a general tool
approval. The perch does not write it. A per-chat grant would need an upstream feature (an `always` answer scoped to a session).

Tests: `tests/manual_policy.rs` compiles eidolon's own `policy.rn` with the wrapper through Rune (a dev-dependency, version of
eidolon's) and checks every tool and shell leaf; with `eidolon` on `PATH` it runs `eidolon policy '<command>'` (the real shell
algebra, no model) under the generated config and runs a headless **mock turn** (`-m mock`, whose script is a `bash` call and a `grep`)
under manual and auto: manual puts the `bash` to the user and the declined call is journaled, the `grep` read runs; auto runs both.
`tests/http.rs` pins the argv and routes with the stand-in door.

## Archive, pin and trash (2026-10-06)

Rows of `GET /hub/sessions` carry `archived`, `auto_other` and `pinned` (all `false` on a shared-dir row). State is
`<home>/archive.json`, `{ "<id>": {"archived", "at", "unarchived_at"?, "pinned"?} }`, atomic, `0600`, pruned of ids that have no log.

- **`other`.** A Minerva chat is `auto_other` when it is not live, not pinned, not archived and `max(log mtime, unarchived_at)` is
  older than 7 days; computed at listing time, **no file moves**. A live chat is never `archived` or `auto_other`. `archive` puts a
  chat in `other` now (409 while a door holds it); `unarchive` brings it back and it stays out until it has been idle 7 days from
  then. Continuing an archived chat (`resume`) unarchives it. eidolon's shared-dir chats stay `origin: "other"` and read-only:
  archive, pin and trash on them are 403.
- **Pin.** `pin`/`unpin`. A pinned chat never auto-moves; archiving unpins; trashing unpins; pinning an archived chat brings it out.
- **Trash.** `trash` renames `<home>/sessions/<id>.eid` into `<home>/trash/` (same filesystem, atomic) and forgets its marks. A held
  chat is 409, unless the body is `{"stop": true}`, which stops its door cleanly first; a chat running in another app is always 409.
  `GET /hub/trash` lists `[{id, title, mtime}]` newest first (a title is what this run learned, else `<folder> · HH:MM`).
  `POST /hub/trash/<id>/restore` moves it back (409 when a chat of that name exists; not re-pinned; a fresh week before it can age).
  `POST /hub/trash/empty {"confirm": true}` (400 without) removes only regular `<id>.eid` files directly inside `<home>/trash` (never a
  symlink, never a subfolder) and the remembered folders of those chats; `{"removed": n}`.
- Ids are `id_ok` before they touch a path; nothing outside `<home>/sessions` and `<home>/trash` is ever moved or removed. All routes
  need `--home` (503 without), and sit behind the same gates as the rest.

## Fork: `POST /hub/sessions/<id>/fork`

A fork is a NEW chat that starts as a copy of one chat, whole; the original keeps going. The perch copies the log (`<id>.eid`) byte
for byte to `<home>/sessions/<unix ms>.eid` (`-1`, `-2` on a clash; `create_new`), then brings a door up on the copy like a resume
(Minerva config, the cwd the log names). It never decodes the log. `201 {"id", "from"}`; the copy is live and listed under the
same project. Its title is the first message, like the source's (the page says `forked from <title>` in the new pane).

- **Between turns only.** A door that opens a log ending mid-turn carries the turn on by itself (hub.md section 4), and a fork must
  never spend. eidolon's own `log` says whether the branch is settled; if not, **409** and nothing is copied. A live source is
  otherwise fine: the log is append-only, a copy cut mid-write ends at the last whole record, and only the copy's open repairs it.
- **Shared-folder chats** (`origin: "other"`) can be forked (read only); the copy lands in the Minerva home.
- **`{"at": <record>}` makes a pending fork** (src/pending.rs, hub.md section 7c): a copy with no door, then `first` runs stock `resume --at N`
  once. Only an assistant reply that ended its turn is forkable (else 422). State: `<home>/forks.json`. Shared folders move for every chat
  that uses them (src/relocate.rs); a busy one refuses with 409.
- 404 for a bad or unknown id; 503 when the copy or its door did not come up (the copy is removed again) or `log` did not answer.

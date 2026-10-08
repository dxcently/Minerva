# src/mesh/ — the mesh view (`/hub/mesh`)

What the mesh tab shows: which nodes Aoide knows, whether its daemon answers, what mesh drift and the outbox look like, and a tail of its
audit events. Read-only and network-free: nothing here sends, pairs, spawns or dials a node, and the only socket the perch opens for it
is a Unix one to Aoide's own daemon.

```text
mod.rs    State, route(ctx, req, rest), nodes(ctx), home(opts), feeds_apart(...): GET /hub/mesh, GET /hub/mesh/events      (R2b, R2c)
```

`serve` hands every `/hub/mesh[/...]` request to `route` with `rest` the path below it (`""` for the bare path), behind the token like
every `/hub` route. Only `GET` has an answer here: a `POST` is the empty 404, and any other method is the gate's 405 before this module is
asked. The other paths under it (`/hub/mesh/nodes`, `/hub/mesh/events/x`) are 404.

## Routes

```text
GET /hub/mesh                  the snapshot, always 200 (what is missing is said in `errors`); a query string is a 400
GET /hub/mesh/events[?audit=N][&daemon=M] the audit log's and the daemon feed's events, from byte N and byte M; each key once, digits and nothing else (else 400)
```

### The snapshot

```text
available  bool                       an aoide program is configured, or there is a state folder to read; false is the page's "no Aoide here"
aoide      {path, version}|null       the program as a `~` path and its --version (null when it did not say); null when there is no program
daemon     {socket, ping_ms}          the socket as a `~` path (null: none known); ping_ms is how long aoided took to say ok, null for no answer
registry   [{name, url?, verified, autogate, addedAt, via, allows, grants}]   the allowlist below, in the file's order; `url` absent when none
cache      [{name, age_secs, text, cut}]                   node-cache/*.json as TEXT, by name; `cut` when the file was over 64 KiB
graph      {nodes, total, kinds, edges}|null               the stage graph, counted and listed; null when there is none
mesh       `data` of `aoide mesh --json` | null            outbox  `data` of `aoide mail outbox --json` | null
errors     [sentence]                                      one sentence per thing that could not be had or was left out
```

With no aoide program and no state folder the answer is `{"available": false, ...}` with every part empty and one sentence in `errors`.
With a state folder and no program (or the reverse) it is `available: true` with what there is, and the sentence says what is missing.

`registry` is an **allowlist**: `name` (a valid node name), `url` (with any `user:pass@` taken out, or absent: see the url rule below), `verified`, `autogate`, `addedAt`,
`via`, and the capability words as `allows` (a flat list: aoide before P-CHARTER, also read from a flat `grants` array) and `grants` (mesh
to words: the per-mesh form after it). `pubkey`, `tokenFile`, `bearerSecret`, `hub` and every field a later Aoide adds are dropped, never
forwarded. Both file shapes are read (`{nodes: [...]}` and `{peers: [...]}`, and a bare array). The per-mesh form is modelled from
P-CHARTER's announcement, not observed; a shape this code does not read is left out and counted in `errors`, because `schemaVersion`
stayed `"0"` through every change and drift would otherwise be silent.

**The url rule** (the registry's, and a gate's in the feed): the cut takes the text before the last `@` of the authority (after `://`, up to the
first `/`, `?` or `#`). Fail closed: a url that still has an `@` anywhere after the cut is **left out**, so its field is absent, not shown. That
is a url with no `://` (`user:pass@box-a:8710/`), and one whose password has a `/` in it (`http://user:pa/ss@host/`, where the authority ends
before the `@`). Nothing says which part before an `@` is a credential, so none is shown. A url that is not text is left out too.

`cache` text is remote-authored. It is never parsed or interpreted here and travels only as a JSON string; the page must show it as text.

`graph` keeps an allowlist of node fields (`id kind name agent state title petname role cwd startedAt model fetchedAt`, each a string of
200 characters at most) and the number of a node's `children`. No `say`, `activity` or `socket`: an agent's words are not this tab's.

### The events

```text
{"events": [{"stream": "audit"|"daemon", "event": object}, ...],
 "next": {"audit": N, "daemon": M}, "reset": {"audit": bool, "daemon": bool},
 "error": {"audit": sentence|null, "daemon": sentence|null},
 "files": {"audit": path|null, "daemon": path|null}}
```

Two streams, one answer. Each stream has its own file, cursor, reset and error, and each record says which stream it came from.

- **`audit`** is `--aoide-audit-log` (absolute only), else `<--aoide-home or ~/.aoide>/log`. Aoide's own rule for the file it appends to
  is `$AOIDE_AUDIT_LOG`, else `$AOIDE_ROOT/log` (absolute `$AOIDE_ROOT`), else `<home>/.aoide/log` (`docs/Aoide-Wiki/concepts/cli/
  Doors-and-Nodes.md`, `CLI-Reference.md`; aoide's `protocol/src/audit.rs::default_audit_log`). CONTRACTS.md gives no precedence for this
  path (its line 985 is a migration note), so the order is the wiki's; the lines cited are listed under **The sources** below. Under the perch the
  first step is the flag alone: the runner gives aoide the flag's path as `AOIDE_AUDIT_LOG` **only when the flag was given**, sets `AOIDE_ROOT` to `--aoide-home`,
  and strips every ambient `AOIDE_*` from its call, so a `$AOIDE_AUDIT_LOG` in the perch's own environment does not reach aoide. Without a
  flag, `--aoide-home` is `AOIDE_ROOT`, so aoide's default and `<home>/log` are the same file; without `--aoide-home` both are `~/.aoide/log`.
  The two readers are one function, `aoide::audit_flag`, so the runner and this view cannot disagree about the flag. With no home at all
  (`HOME` unset or empty, and no `--aoide-home`) and no flag, the stream has no file and says nothing is read.
  The stream is an **allowlist**, fail closed: a record needs an integer `ts` and a `class` of `audit` or `gate`; when present, `door` must
  be `cli`, `mcp`, `a2a` or `daemon`, `command` one word (dotted, slashed or dashed, no space, at most 64 bytes) and `status` one of `started`, `ok`,
  `error`, `pending`, `delivered`, `proposed`. The answer keeps `ts`, `class`, `door`, `command`, `status`, and `message` **only for aoided's
  start records** (see below). `untrusted_data`, `admitted`, `argv` and any field a later aoide adds are never forwarded. A record that
  fails a rule is dropped, not repaired.
  - **The message rule.** A record's `message` is forwarded only for aoided's start records: `command` `daemon` or `adapter.melete` (in any
    letter case) with `status` `started`. The docs name both: `daemon started` (`docs/Aoide-Wiki/concepts/cli/Doors-and-Nodes.md`, line 91) and
    `adapter.melete started` (the same file, line 213). Their `message` (a listen URL, as observed) is kept. **Every other record's `message` is
    left out**, mail's and anyone's. The rule fails closed because the docs do not make `message` free of a secret or a letter: `mail.read`
    writes the letter's body into it (observed, cut at about 526 characters; `mail.send` writes `filed to self/minerva (msgid ...)`), and the
    secrets broker writes its `resolve`, `put` and admin lines to this same log, each naming a secret
    (`docs/Aoide-Wiki/concepts/orchestration/Secrets-Broker.md`, lines 46-47 and 122). Mail is no special case: `mail.read`, `MAIL.read` and
    `Mail/read` keep no `message`, as any other command but a start record keeps none. A record with no command or status keeps no `message`,
    and a `message` that is not a string is left out (the record stays). The page draws a `message` only where one is sent.
  - The shape of the real aoide 0.0.26 log, as the orchestrator observed it (structure only): `{"ts": <integer>, "door": "daemon" | "cli",
    "class": "audit", "command": "daemon" | "a2a.serve" | ..., "status": "started" | "ok" | ..., "message": <text>}`. The docs do not
    give `ts`'s unit; this code reads only the integer.
  - Dropped, and why: a `status` or `door` the list does not name (a daemon line with status `failed` is one, and goes whole, message and all).
    The docs use other audit labels too (`unmailed`, `attribution-drift`,
    `auto-deny-on-code-mismatch`), and none of them is on the list, so a record with one is hidden until a reviewer adds it. A `command`
    with a space in it (`node add osaka`), which is a line of arguments and not a command word. A `class` of `secret`, `notification`,
    `rice` or `content`. A record with no `ts`, a `ts` that is not a non-negative integer, or a line that is not a JSON object.
- **`daemon`** is `--aoide-daemon-events` (absolute only), else `$AOIDE_DAEMON_EVENTS` when absolute, else `$XDG_RUNTIME_DIR/aoide/events.jsonl`
  when `$XDG_RUNTIME_DIR` is absolute. Unset, empty or relative values name no file, so no path under the working directory is ever opened,
  and with no path the stream is empty and `files.daemon` is `null`. **The XDG path is the perch's own default**, chosen so it mirrors
  `default_socket()` (the socket's default is `$XDG_RUNTIME_DIR/aoide/aoided.sock`). Aoide's rule for the same file is `$AOIDE_DAEMON_EVENTS`,
  else a sibling of the daemon socket (`docs/Aoide-Wiki/concepts/cli/Doors-and-Nodes.md`, "aoide events tail"; `docs/architecture/AOIDED.md`,
  "aoided's own feed and record shape"). The two agree while the socket is the default one. A `--aoide-daemon-socket` somewhere else moves
  aoide's path and not this one, so pass `--aoide-daemon-events` with it. The runner's `XDG_RUNTIME_DIR` is the same variable, read by the same
  function (`aoide::runtime_dir`): a relative or empty one is **none**, as an unset one is, for the runner and for this view alike. The runner
  gives aoide no `XDG_RUNTIME_DIR` then, and does not let the perch's own (relative) one reach the child either, so no call and no default
  path sees a value under the working directory.
  The feed keeps only what aoide's docs name, and only for two classes: `audit` and `gate`, with an integer `ts`. Of a record it keeps the
  top-level `v`, `ts`, `class`, `kind` and `source`, and of its `payload`: for a `gate` record `id`, `name`, `originAddr`, `url` and `direction`
  (Aoide's CONTRACTS.md, at its repository root, "Pairing events feed (P-P5)": "payload carries fields BY NAME ONLY"), where a `url` is kept
  without its `user:pass@`, or left out when an `@` is still in it (the url rule above; a `url` that is not text is left out too); for an `audit` record
  `file` (`docs/architecture/AOIDED.md`, "The first producer: the hand-edit watcher"). Everything else is dropped, so a field a later aoided
  adds never reaches the page: a free-text field at either level, a gate's `sas`, a `node` or `note`, all of them. `secret`, `notification`,
  `rice` and `content` records, records with no class, records with no `ts`, and lines that are not objects are dropped as well.
- **The sources** (under `docs/Aoide-Wiki/` unless marked). `concepts/cli/Doors-and-Nodes.md`: line 29 (`$AOIDE_ROOT`, default `~/.aoide`),
  37-39 (the audit log's order), 87-89 (the audit path's order as the `audit` bullet states it), 91 (aoided's `daemon started`, a start record),
  213 (`adapter.melete started`, a start record), 238 and 245-246 (`aoide events tail`: `$AOIDE_DAEMON_EVENTS`, else a sibling of the socket).
  `concepts/cli/CLI-Reference.md`: 23-26 (the audit flag, `$AOIDE_AUDIT_LOG`, `$AOIDE_ROOT/log`, the default). `concepts/orchestration/Secrets-Broker.md`:
  46-47 (the broker's audit lines, mirrored to the aoide audit log) and 122 (audits name only). `docs/architecture/AOIDED.md`: 88-89 (aoided's feed
  path) and 164 (the hand-edit watcher, the audit payload's `file`). `CONTRACTS.md` (the Aoide repository's root): 6777-6783 (the P-P5 feed, under
  `$XDG_RUNTIME_DIR/aoide/events.jsonl`); 985 is a migration note and names no precedence for the audit path.
- The two are merged by `ts`, at a tie the audit log first, then each file's order. A record with no numeric `ts` in either stream is not
  shown. Against a real aoide the audit stream reads by the same integer `ts` the log writes (observed), so it is not empty for want of one.
- `next.<stream>` is that stream's next cursor: the end of its last **whole** line read, so a line still being written is read again once
  it has its newline. Pass them back as `?audit=N&daemon=M` (either, or both, each once).
- `reset.<stream>` is true when, since that stream's last poll, its file was **shorter than the cursor**, or was **another file** (its device
  or inode changed: replaced, renamed over, rotated), or was **shorter than the size the last poll saw** (cut in place). That stream reads again
  from 0, and the other is untouched. The last poll's device, inode and length are kept per path in the perch's memory, never on disk; they
  are lost at a restart, and the first poll after one then compares with nothing. **The limit:** a file cut in place and grown past the
  cursor again between two polls, with the same inode, is not seen as a reset (its size is only seen at a poll). Records written between the
  cursor and the regrowth are then not shown, and the read from the cursor can start mid-line, which is dropped.
- No query is the first poll: each stream's last 64 KiB, from its first whole line. A poll reads at most 64 KiB of each file, so a long
  backlog takes several polls and loses nothing.
- A file that is not there yet is an empty stream with cursor 0. A file that is a link, a directory or a FIFO, or cannot be opened as a
  plain file, is **that stream's `error`**: the sentence names the stream (`the audit log could not be read as a plain file`), nothing is
  read from it or said about its contents, the stream has no events, and its cursor stays where it was asked (0 for a first poll, so when
  the file is fixed the next poll reads its start). **The other stream still answers**, and the answer is a 200. `files` names each path as
  a `~` path, or `null` where a stream has none (a relative flag is none).
- **The start refuses one file for both feeds.** `main.rs` calls [`feeds_apart`] before anything under `--home` is made. The audit log and the
  daemon feed are each resolved as the view resolves them (the flag, else the default), and when they are the same path once every symlink on the
  way is followed, or the same device and inode (a hard link), the perch refuses to start, and says so in a sentence that names the path and the
  flags. A link is followed one hop at a time, so a dangling link to a feed aoided has not written yet is that path, and a chain of more than 40
  links, or a loop, is refused with a sentence of its own (the perch cannot tell which file it names). This covers `--aoide-audit-log` and `--aoide-daemon-events` at one path, the feed flag at the audit log's default (`<home>/log`), and
  `AOIDE_DAEMON_EVENTS` at the audit log's path with no feed flag. The reason: aoide appends its audit records to the audit log, so one file for
  both would put them into the feed aoided writes.
- Any other query (`after=`, a repeated key, a key that is neither `audit` nor `daemon`, a value that is not digits, more than 20 digits,
  an empty pair) is a 400.

## What it reads, and how

- **Files**, under `<--aoide-home or ~/.aoide>/state/`: `nodes.json` (1 MiB), `node-cache/*.json` (64 files, 64 KiB each, and 1000
  directory entries looked at at most), `stage/graph.json` (2 MiB). Each is opened `O_NOFOLLOW | O_NONBLOCK` through
  `crate::rawfile::open_regular` and must be a regular file, so a link or FIFO planted under a name is not read. The two event feeds are
  not under the state folder: they are the paths of `--aoide-audit-log` and `--aoide-daemon-events` (see The events). Both are opened the
  same way, and only read.
- **Aoide, through `crate::aoide::Aoide` and nothing else**: `--version`, `mesh --json`, `mail outbox --json`. Never `node list`,
  `session`, `send`, `pair`, `spawn`, `allow`, `node status`, `node pull`, `dispatch` or anything on the A2A door: the runner has no
  variant for them. The perch never spawns aoide itself, and a perch with no `--aoide` and no executable `~/.local/bin/aoide` runs
  nothing. **What the perch writes:** the perch never opens either event file for writing. But each aoide call it makes is a command aoide
  runs, and aoide appends its own audit records for the commands it audits, to the audit log: the flag's file when `--aoide-audit-log` is
  given (the runner passes it as `AOIDE_AUDIT_LOG`), else aoide's default under `<--aoide-home or ~/.aoide>`. Which commands aoide audits is
  aoide's rule (`mail.read` and `mail.send` were observed, and the daemon's start; `mail.outbox` is expected from the docs and was not observed;
  a usage-error exit writes none). How often: the snapshot's three
  calls are each cached for 60 s, so any number of open tabs cost at most one set of them a minute; the mesh's `nodes` for mail is that
  same cached report; mail's own calls (send, read, outbox) run once per request; the events poll runs no aoide call at all.
- **The daemon**: one connection to the socket (`--aoide-daemon-socket`, else `$XDG_RUNTIME_DIR/aoide/aoided.sock`), one line
  `{"v":0,"op":"ping"}`, one line read back (4 KiB at most) within 1 s. Never `subscribe` or `dispatch`. Only the boolean `ok` is read from
  the reply; a pid or a key in it goes nowhere.

## Caching

`--version`, `mesh --json` and `mail outbox --json` are each cached for 60 s (`TTL`), so any number of open tabs cost one run of each a
minute. Only a success is cached: a failed run is a sentence in `errors` and is asked for again on the next read. The lock is held across
the run, so two tabs opened together cost one run, and the three runs and the ping go at once, so a snapshot waits for the slowest (10 s
at most, the runner's timeout) and not their sum. The files are read fresh on every call; they are small and the page wants their age.
The events are not cached: each poll reads the files at their cursors.

`--knob mesh_ttl_ms=N` replaces the 60 s so a test can prove expiry. It is the existing hidden `--knob` mechanism; there is no flag.

## For the other slices

- **`mesh::nodes(ctx) -> Vec<NodeInfo>`** (`NodeInfo {name, verified, meshes}`, serializable) is for mail's send route (M2): the
  registered nodes with the meshes `mesh --json` has a row for them in. **Caveat:** aoide 0.0.23's `mesh --json` lists only rows that
  *drift*, not a clean bill per node, so a healthy node can have no meshes here. A node with no meshes is not unreachable. Aoide's own
  refusal for a node trusted in more than one mesh carries `data.meshes`, and that is what mail should rely on to ask which mesh. No
  registry, no program or a failed run is a shorter list, never an error. It uses the cached mesh report, so asking costs at most one run
  a minute.
- **`mesh::home(opts)`** is aoide's home as this module resolves it (`--aoide-home`, else `~/.aoide`, absolute or nothing). `home_of(flag)`
  is the same for a flag that is not in an `Options` yet.
- **Tests of M2 and W4: always pass an explicit `aoide_home` and `aoide_daemon_socket`** (an `Options` literal: a temp dir and a path in it
  that is not there; a started perch: `--aoide-home <tmp> --aoide-daemon-socket <tmp>/aoided.sock`, and a `HOME` of your own, as
  `tests/mesh.rs` does). A `None` means the operator's `~/.aoide` and `$XDG_RUNTIME_DIR/aoide/aoided.sock`, and a test that reaches `/hub/mesh`
  or `mesh::nodes` with one reads real node rosters and pings a real daemon. Under `cfg(test)` (this crate's unit tests only) a `None`
  resolves to `/nonexistent-minerva-test/aoide` instead, so a lib test that forgets cannot reach real state; the integration tests run the
  real binary and get no such net: `tests/common`'s `Perch::builder()` sets `HOME` to a temp dir (a test that names its own `HOME` wins) and
  `XDG_RUNTIME_DIR` to a temp dir (the test cannot change that one), so without your own `HOME` and `--aoide-home` the perch reads the real
  `~/.aoide`.
- The page (W4) renders `registry`, `cache`, `graph`, `mesh` and `outbox` as text and tables only; nothing in the response is HTML or
  a URL to open, and `cache[].text` and any string inside `mesh`, `outbox` or `events` may be hostile.

## Tests

`tests/mesh.rs` runs a real perch against `fake_aoide` (the tests' stand-in, `tests/fixtures/aoide/`) with a temporary aoide home, a
`UnixListener` standing in for aoided, and checks the fake's log for exactly `--version`, `mesh --json` and `mail outbox --json`. The
module's own unit tests cover the projections (both registry shapes, the allowlist, the graph, the cache), the two event streams (the
audit allowlist and the message rule, the daemon feed's fields and the url rule, the merge, the cursors, the resets and the identity a poll keeps, the 64 KiB
window, the query), the paths each stream resolves to (and that no default is ever a relative path), the start's check that the two feeds
are two files (a dangling link is followed to its path, and a chain of more than 40 links is refused), and the TTL cache. `tests/mesh.rs` runs
the events end to end: both streams with their labels, a stream's truncate resetting only itself, a feed replaced by a new file, a link or a
FIFO at either path (the other stream still answers), a mail letter in no answer byte, a message only on a start record, the runner's
environment (the audit flag and never the feed), the defaults, and the start refused with both feeds at one file.

**Accepted without a live test: a relative `XDG_RUNTIME_DIR`.** A relative or empty value is none, for the view's default feed and for the
runner's `XDG_RUNTIME_DIR` (and the runner removes the perch's own from each child's environment). That case is covered by the unit tests
`a_relative_or_empty_runtime_dir_is_none_as_an_unset_one_is` (`src/aoide/mod.rs`) and `the_default_feed_is_never_a_relative_path` (this module),
and by `tests/aoide.rs`'s `no_path_lookup_and_no_ambient_identity` (a child's environment). It is not checked on a live perch: `tests/common` pins
`XDG_RUNTIME_DIR` to a temp dir after a test's own environment, so a relative value cannot be set on a running perch from a test.

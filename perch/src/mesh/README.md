# src/mesh/ — the mesh view (`/hub/mesh`)

What the mesh tab shows: which nodes Aoide knows, whether its daemon answers, what mesh drift and the outbox look like, and a tail of its
audit events. Read-only and network-free: nothing here sends, pairs, spawns or dials a node, and the only socket the perch opens for it
is a Unix one to Aoide's own daemon.

```text
mod.rs    State, route(ctx, req, rest), nodes(ctx), home(opts): GET /hub/mesh, GET /hub/mesh/events      (R2b)
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
registry   [{name, url, verified, autogate, addedAt, via, allows, grants}]     the allowlist below, in the file's order
cache      [{name, age_secs, text, cut}]                   node-cache/*.json as TEXT, by name; `cut` when the file was over 64 KiB
graph      {nodes, total, kinds, edges}|null               the stage graph, counted and listed; null when there is none
mesh       `data` of `aoide mesh --json` | null            outbox  `data` of `aoide mail outbox --json` | null
errors     [sentence]                                      one sentence per thing that could not be had or was left out
```

With no aoide program and no state folder the answer is `{"available": false, ...}` with every part empty and one sentence in `errors`.
With a state folder and no program (or the reverse) it is `available: true` with what there is, and the sentence says what is missing.

`registry` is an **allowlist**: `name` (a valid node name), `url` (with any `user:pass@` taken out), `verified`, `autogate`, `addedAt`,
`via`, and the capability words as `allows` (a flat list: aoide before P-CHARTER, also read from a flat `grants` array) and `grants` (mesh
to words: the per-mesh form after it). `pubkey`, `tokenFile`, `bearerSecret`, `hub` and every field a later Aoide adds are dropped, never
forwarded. Both file shapes are read (`{nodes: [...]}` and `{peers: [...]}`, and a bare array). The per-mesh form is modelled from
P-CHARTER's announcement, not observed; a shape this code does not read is left out and counted in `errors`, because `schemaVersion`
stayed `"0"` through every change and drift would otherwise be silent.

`cache` text is remote-authored. It is never parsed or interpreted here and travels only as a JSON string; the page must show it as text.

`graph` keeps an allowlist of node fields (`id kind name agent state title petname role cwd startedAt model fetchedAt`, each a string of
200 characters at most) and the number of a node's `children`. No `say`, `activity` or `socket`: an agent's words are not this tab's.

### The events

```text
{"events": [{"stream": "audit"|"daemon", "event": object}, ...],
 "next": {"audit": N, "daemon": M}, "reset": {"audit": bool, "daemon": bool}, "files": {"audit": path|null, "daemon": path|null}}
```

Two streams, one answer. Each stream has its own file, cursor and reset, and each record says which stream it came from.

- **`audit`** is `--aoide-audit-log`, else `<--aoide-home or ~/.aoide>/log`, which is aoide's own default. The runner gives aoide the
  explicit flag's path as `AOIDE_AUDIT_LOG` **only when the flag was given**, and strips every ambient `AOIDE_*` from its call, so the file
  aoide appends to under the runner is the one tailed here. The stream is an **allowlist**, fail closed: a record needs a numeric `ts`
  and a `class` of `audit` or `gate`; when present, `door` must be `cli`, `mcp`, `a2a` or `daemon`, `command` one word (dotted or slashed,
  no space, at most 64 bytes) and `status` one of `ok`, `error`, `pending`, `delivered`, `proposed`. The answer keeps `ts`, `class`,
  `door`, `command` and `status` and nothing else: a `message`, `untrusted_data` or any field a later aoide adds is never forwarded. A
  record that fails a rule is dropped, not repaired.
- **`daemon`** is `--aoide-daemon-events`, else `$AOIDE_DAEMON_EVENTS` when absolute, else `$XDG_RUNTIME_DIR/aoide/events.jsonl` (aoided's
  own default, beside its socket). Its records pass as they are when `class` is `audit` or `gate`; `secret`, `notification`, `rice`,
  `content` and anything else are never shown. No aoide call is ever given this path.
- The two are merged by `ts`, at a tie the audit log first, then each file's order. A record with no numeric `ts` in the audit log sorts
  as 0 and, under the allowlist, is dropped before that. Aoide's audit timestamp is not documented anywhere the perch reads, so until it is
  the audit stream is the one most likely to read empty against a real aoide.
- `next.<stream>` is that stream's next cursor: the end of its last **whole** line read, so a line still being written is read again once
  it has its newline. Pass them back as `?audit=N&daemon=M` (either, or both, each once).
- `reset.<stream>` is true when that stream's cursor was past the end of its file (truncated in place at 1 MiB, replaced, or gone): that
  stream reads again from 0, and the other is untouched.
- No query is the first poll: each stream's last 64 KiB, from its first whole line. A poll reads at most 64 KiB of each file, so a long
  backlog takes several polls and loses nothing.
- A file that is not there yet is an empty stream with cursor 0. A file that is a link, a directory or a FIFO is a 502 that names the
  stream, and nothing is read from it. `files` names each path as a `~` path, or `null` where a stream has none (a relative flag is none).
- Any other query (`after=`, a repeated key, a key that is neither `audit` nor `daemon`, a value that is not digits, more than 20 digits,
  an empty pair) is a 400.

## What it reads, and how

- **Files**, under `<--aoide-home or ~/.aoide>/state/`: `nodes.json` (1 MiB), `node-cache/*.json` (64 files, 64 KiB each, and 1000
  directory entries looked at at most), `stage/graph.json` (2 MiB). Each is opened `O_NOFOLLOW | O_NONBLOCK` through
  `crate::rawfile::open_regular` and must be a regular file, so a link or FIFO planted under a name is not read. The two event feeds are
  not under the state folder: they are the paths of `--aoide-audit-log` and `--aoide-daemon-events` (see The events).
- **Aoide, through `crate::aoide::Aoide` and nothing else**: `--version`, `mesh --json`, `mail outbox --json`. Never `node list`,
  `session`, `send`, `pair`, `spawn`, `allow`, `node status`, `node pull`, `dispatch` or anything on the A2A door: the runner has no
  variant for them. The perch never spawns aoide itself, and a perch with no `--aoide` and no executable `~/.local/bin/aoide` runs
  nothing.
- **The daemon**: one connection to the socket (`--aoide-daemon-socket`, else `$XDG_RUNTIME_DIR/aoide/aoided.sock`), one line
  `{"v":0,"op":"ping"}`, one line read back (4 KiB at most) within 1 s. Never `subscribe` or `dispatch`. Only the boolean `ok` is read from
  the reply; a pid or a key in it goes nowhere.

## Caching

`--version`, `mesh --json` and `mail outbox --json` are each cached for 60 s (`TTL`), so any number of open tabs cost one run of each a
minute. Only a success is cached: a failed run is a sentence in `errors` and is asked for again on the next read. The lock is held across
the run, so two tabs opened together cost one run, and the three runs and the ping go at once, so a snapshot waits for the slowest (10 s
at most, the runner's timeout) and not their sum. The files are read fresh on every call; they are small and the page wants their age.

`--knob mesh_ttl_ms=N` replaces the 60 s so a test can prove expiry. It is the existing hidden `--knob` mechanism; there is no flag.

## For the other slices

- **`mesh::nodes(ctx) -> Vec<NodeInfo>`** (`NodeInfo {name, verified, meshes}`, serializable) is for mail's send route (M2): the
  registered nodes with the meshes `mesh --json` has a row for them in. **Caveat:** aoide 0.0.23's `mesh --json` lists only rows that
  *drift*, not a clean bill per node, so a healthy node can have no meshes here. A node with no meshes is not unreachable. Aoide's own
  refusal for a node trusted in more than one mesh carries `data.meshes`, and that is what mail should rely on to ask which mesh. No
  registry, no program or a failed run is a shorter list, never an error. It uses the cached mesh report, so asking costs at most one run
  a minute.
- **`mesh::home(opts)`** is aoide's home as this module resolves it (`--aoide-home`, else `~/.aoide`, absolute or nothing).
- **Tests of M2 and W4: always pass an explicit `aoide_home` and `aoide_daemon_socket`** (an `Options` literal: a temp dir and a path in it
  that is not there; a started perch: `--aoide-home <tmp> --aoide-daemon-socket <tmp>/aoided.sock`, and a `HOME` of your own, as
  `tests/mesh.rs` does). A `None` means the operator's `~/.aoide` and `$XDG_RUNTIME_DIR/aoide/aoided.sock`, and a test that reaches `/hub/mesh`
  or `mesh::nodes` with one reads real node rosters and pings a real daemon. Under `cfg(test)` (this crate's unit tests only) a `None`
  resolves to `/nonexistent-minerva-test/aoide` instead, so a lib test that forgets cannot reach real state; the integration tests run the
  real binary and get no such net: `tests/common`'s `Perch::builder()` points `XDG_RUNTIME_DIR` at a temp dir but leaves `HOME` as it is,
  so without your own `HOME` and `--aoide-home` the perch reads the real `~/.aoide`.
- The page (W4) renders `registry`, `cache`, `graph`, `mesh` and `outbox` as text and tables only; nothing in the response is HTML or
  a URL to open, and `cache[].text` and any string inside `mesh`, `outbox` or `events` may be hostile.

## Tests

`tests/mesh.rs` runs a real perch against `fake_aoide` (the tests' stand-in, `tests/fixtures/aoide/`) with a temporary aoide home, a
`UnixListener` standing in for aoided, and checks the fake's log for exactly `--version`, `mesh --json` and `mail outbox --json`. The
module's own unit tests cover the projections (both registry shapes, the allowlist, the graph, the cache), the two event streams (the
audit allowlist, the merge, the cursors and resets, the 64 KiB window, the query), the paths each stream resolves to, and the TTL cache.
`tests/mesh.rs` runs the events end to end: both streams with their labels, a stream's truncate resetting only itself, a link or a FIFO at
either path, the runner's environment (the audit flag and never the feed), and the defaults.

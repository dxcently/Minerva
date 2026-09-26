# Build-Log

What was actually built and verified, by day. Claims here are things that ran.

## 2026-09-17

- `bin/hoot.ps1` — the launcher: `up`, `serve`, `webui`, `jev`, `bench`,
  `setup`, `status`. Copies `webui/` into the installed Open WebUI on every
  launch; sets `WEBUI_NAME=hoot`, `WEBUI_NAME_SUFFIX=false`,
  `ENABLE_OLLAMA_API=False`, `OPENAI_API_BASE_URLS` at both the router and the
  sidecar.
- `models.ini` — router config with a per-model bench line; MTP draft for Qwen;
  Bonsai Q2_0 on Vulkan.
- `llama.cpp/` — own build with `ggml-vulkan.dll` and the CPU variant DLLs.
- `webui/custom.css` — first skin: palette, type, borders, id-anchored icon
  masks, owl branding.
- `jev/server.py` — sidecar with `/v1/models`, `/v1/chat/completions`
  (streaming and not), bullet-list / `a | b | c` option parsing, jevlike and
  openjev lazy-loaded.
- Models pulled: Qwen3.8-27B-UD-Q4_K_M (+ MTP draft + mmproj),
  Ternary-Bonsai-2-27B in PTQ1_0 / PQ2_0 / Q2_0, Bonsai-8B-Q1_0, openjev.

## 2026-09-18

### openjev fixed

`_openjev()` rewritten onto `OpenJevCrossEncoder`. Verified: the probe flips to
contradiction 0.898.

### Skin

- **Chat frame** — moved from an absolutely-positioned `::after` on the scroll
  container (invisible: abspos `inset:0` on a scroll container resolves against
  the scrollable box) to a real border + inset box-shadow on
  `div:has(> #messages-container)`, so it reaches the composer.
- **Sidebar nav icons** — Notes and Workspace get `memo` / `briefcase` masks
  anchored on `href`.
- **`build-loader.py`** — generates `loader.js`; `fit_viewbox()` for clipped
  `-small` bodies; stroke stripped, `fill: currentColor`; `"transform":
  "rotate180"` support.
- **`loader.js`** — 96,578 bytes, 165-icon swap table; `icon-map-full.json`
  (153 from a review workflow + 12 by hand); `owui-icons.json`,
  `dinkie-bodies.json` as build inputs; `README-icons.md` rewritten.

### eidolon Windows port

`crates/core/src/ipc.rs` (new; framed `recv`/`send`, Unix `imp`, Windows
named-pipe `imp`, 3 tests); `claude/{mcp,hook,lib}.rs` ported (hook un-stubbed;
`kill_group` → `taskkill /PID n /T /F`); `swarm/socket.rs` doorbell on `ipc`,
cfg gates removed; `swarm/keypool.rs` test `backdate()` via
`FILE_WRITE_ATTRIBUTES` + `FILE_FLAG_BACKUP_SEMANTICS`; `providers/readiness.rs`
PATHEXT lookup; `providers/catalog.rs` literal TOML string in test;
`web/stream.rs` `Body + Sync`; `cli/system.rs` `/` joins; `tools/shell.rs`
`open_log()` write+seek, `Exec.cancelled`.

Full suite **917 passed, 0 failed, 33 binaries**. Release binary built.
`eidolon models` lists the hoot provider. `eidolon web -m hoot:bonsai-8b`
served the page, the assets, SSE, and a complete turn returning "PONG" (first
token 65,970 ms cold, decode 318 ms).

**Not committed.** 22 files sit uncommitted on top of `8a09c4a`.

### Extension host — phase 1, landed

The host from the [decision](Decisions.md#2026-09-18--one-extension-host-everything-is-an-eidolon-extension),
built and verified end to end.

**New files**

| file | what |
|---|---|
| `crates/tools/src/service.rs` | the transport: `Endpoint` (loopback-only, token held in `Zeroizing`), the `{ok, result}` / `{ok, error}` envelope, `healthy`, `wait_ready`, `wait_ready_while`, `fresh_token` (32 bytes from the OS) |
| `crates/rune/src/ext.rs` | the model: `Manifest` / `Service`, `discover`, `manifest`, `register_tools` (namespaced), `ensure` (adopt or start), `stop`, `probe`, the on-disk `Record` |
| `crates/cli/src/ext.rs` | `eidolon ext list/enable/disable/start/stop`, the `[[extensions]]` stanza through `toml_edit`, the loader a session runs |
| `eidolon/docs/extensions.md` | the author's guide |

**Changed**

- `crates/rune/src/host.rs` — a `services` map on the `Host`, an `EXTENSION`
  thread-local beside `CANCEL` (`with_extension`), and the `service_call`
  primitive. The script passes a method and arguments; it cannot pass an
  address, a port, a token, or even the extension name.
- `crates/rune/src/script.rs` — `ScriptTool::compile_for(extension, …)`,
  namespacing the registered name to `<ext>_<name>`; the extension is bound to
  the call's thread alongside the cancellation token.
- `crates/tools/src/shell.rs` — `run_background_with_env` (the token reaches
  the child in its environment, never in argv), `kill_tree`, and a liveness
  flag on `Background` cleared by the reaper thread.
- `crates/cli/src/config.rs` — `[[extensions]]`, `extensions_dir`,
  `extension_enabled`, `config_path`.
- `crates/cli/src/main.rs` — `Cmd::Ext`; the loader runs in `build()`, and
  `eidolon tools` lists extension tools without starting anything.

**Verified.** Workspace suite: 33 binaries, 0 failures (26 new tests — 10 on
the transport against a hand-rolled loopback server, 9 on the extension model
including a live `service_call` round trip, 3 on the config stanza, 4 others).

A live smoke test with a Python `http.server` extension:

```
$ eidolon ext list
echo  off  — a service that says what it was asked
    service  not running
    tools    echo_say

$ eidolon ext enable echo
echo: enabled in …\config.toml

$ eidolon ext start echo
echo: service started on :63370 in 0.5s; log …\bg-1a0b60f148b-0001.log

$ eidolon ext list
echo  on  — a service that says what it was asked
    service  up on :63370 (pid 6356)
    tools    echo_say

$ eidolon ext start echo          # a second time
echo: service already up on :63370

$ eidolon run --provider mock "say hello"
[tool] echo: service already up on :63370      # a session adopts it

$ eidolon ext disable echo
echo: disabled in …\config.toml
echo: service stopped (pid 6356)
```

And the failure path, which reports the service's own log and does not wait out
`ready_timeout_s` when the child exits:

```
$ eidolon ext start echo          # command points at a missing file
Error: the service exited without answering http://127.0.0.1:63388/health
--- …\bg-1a0b610fea1-0001.log ---
python.exe: can't open file '…\no-such-file.py': [Errno 2] No such file or directory
real  0m3.228s
```

### Vault

Confirmed the twelve `wiki/projects/Eidolon/*` references in `eidolon/AGENTS.md`
are dangling in both served vaults. The Hoot ingest source was written to
`09 🗒 REFERENCES/Hoot-Ingest-2026-09-18.md`; the mint job is blocked on Melete
(see [Decisions](Decisions.md#2026-09-18--melete-cannot-run-the-mint-yet-and-why)).
This `docs/` directory is the record until it lands.

### Wave A and B — design, and the first extension

**Designs written** (`docs/design/`, each accepted with amendments recorded in
[Decisions](Decisions.md)):

| document | lines | what it settles |
|---|---|---|
| `shim.md` | 1060 | the OpenAI surface: chat id as the session file name, the `Event`→chunk table, parked approvals, a per-install bearer token |
| `automation.md` | 1343 | the XState subset, `meta.choose`, the closed guard set, escalation as a tool result, the jevlike-native log |
| `pages.md` + `ext.html` + `auth.html` | 761 + markup | `/ext` and `/auth`: eight states, never-optimistic controls, name and value in the POST body |
| `fallback.md` | — | in flight |

**jev is an extension.** `jev/server.py` keeps `_jevlike()` and `_openjev()`
byte for byte — including the docstring that records the vendor-class bug —
and loses `/v1/models` and `/v1/chat/completions` entirely. It gains `GET
/health` (which never loads a model: the adoption probe has a two-second
budget and a cold openjev is nine gigabytes) and `POST /call`, auth-checked
before the body is parsed. `extensions/jev/` registers `jev_choose` and
`jev_entail`.

Verified against the real binary: service starts in 1.1–1.6s; unauthenticated
`POST /call` → 401; the vendor-class probe → contradiction **0.9109** on "the
service is healthy", matching the 0.898 recorded on 2026-09-17 and proving the
wiring carried over. 13 Python tests pass. `ext stop jev` leaves no process,
no port and no record.

**The launcher stops owning jev.** `hoot.ps1` no longer starts the sidecar,
`OPENAI_API_BASE_URLS` collapses to the router alone, and `hoot jev` now
redirects to `eidolon ext start jev`. `ENABLE_FORWARD_USER_INFO_HEADERS=True`
is set. `Set-ExtensionsDir` points eidolon at the repo's `extensions/` — see
[the decision](Decisions.md#2026-09-18--extensions-are-pointed-at-not-copied)
for why that is a pointer and not a copy.

Workspace suite after both: **939 passed, 0 failed, 6 ignored, 33 binaries**
(the 6 are pre-existing live/network tests).

Two things left open by this wave and handed to review: the decision log's
`label` is written as option text where jevlike requires an index, and two
pre-existing python processes are still running the *old* sidecar on `:8090`.

### `$HOME`, credentials, and AGENTS.md — S6 and S3

Seven `var_os("HOME")` sites moved onto `dirs::`; the two OAuth credentials
unified under `config_dir()` with migration; `AGENTS.md`'s twelve dangling
`wiki/projects/Eidolon/*` links replaced with six real ones into `docs/`, and
nine topics with no counterpart named as having none rather than given an
invented link. See [Decisions](Decisions.md#2026-09-18--home-is-not-where-a-windows-box-keeps-anything).

Runtime check with `$HOME` confirmed absent: `eidolon tools` exits 0, and
`eidolon chatgpt-login` now reaches the device-flow prompt instead of bailing
with "$HOME is not set". The flow was killed before approval; no credential was
written and no token was obtained.

Workspace after: **974 passed, 0 failed, 6 ignored, 33 binaries** — reported by
the agent, and not attributable to S6 alone because three other agents were
editing the same tree concurrently. Independently confirmed after those agents
were stopped: `cargo check --workspace --all-targets` exits 0.

**Unreviewed.** The change went well past its brief into credential paths and
the shared `harnox` repo, which is exactly the shape that wants a reviewer.

### Suite after the move — verified by the orchestrator

`cargo test --workspace`, run with every agent stopped and nothing else
building: **974 passed, 0 failed, 6 ignored**, 23 test binaries reporting.
`cargo check --workspace --all-targets` exits 0. This is the number S6 reported
and could not attribute, now confirmed on a quiet tree.

D2a's `eidolon::dispatch` is in, and its four conditions each have a test that
names what it holds:

| condition | test |
|---|---|
| `CallOrigin::Script` preserved | `host::tests::a_nested_dispatch_is_never_laundered_into_the_models_own_origin` |
| a depth limit, naming the chain | `host::tests::a_dispatch_cycle_is_refused_naming_the_chain_not_a_stack_overflow` |
| no manufacturing a gate answer | `host::tests::dispatch_refuses_the_human_asking_primitive_by_name` |
| cancellation reaches a nested call | `host::tests::cancelling_the_outer_call_cancels_a_nested_dispatch` |

The one condition with no obviously-named test is **no gate bypass** — that a
script-originated `bash` is classified exactly as a model-originated one,
refusals included. Raised with D2a.

B2b has no `set_dir` test yet, which matches its being stopped one step before
running the suite.

### jev, reviewed — ship, with five fixes the review made itself

Verdict **ship**, on the strength of the one pre-confirmed showstopper being
fixed and proven end to end rather than read from source: a live call through
the running service wrote a row, and `jevlike.data.validate()` accepted it, with
`options[label]` agreeing with the response's own answer.

The reviewer found and fixed four things nobody had asked for:

- **Duplicate option text silently dropped a probability.** `scored` was keyed
  by option text, so `choose(["a","a","b"])` returned two keys summing to less
  than one. Now refused before the model runs.
- **The decision-log append had unguarded races.** In-process, fixed with a
  lock; cross-process, mitigated by one `os.write` on an `O_APPEND` descriptor
  instead of a buffered `TextIOWrapper`. The cross-process root cause belongs to
  `ext.rs`'s single-instance guarantee and was reported, not patched.
- **The test suite was writing to the real decision log**, which grew from 6 to
  52 rows across ordinary test runs. Fixed at the root by defaulting
  `JEV_DECISIONS_LOG` to a tempdir before `import server`.
- **The auth comparison was `!=`.** Now `hmac.compare_digest`.

Python suite: **20 passed, 1 skipped** (the skip loads the real nine-gigabyte
openjev weights), up from 13. Includes
`test_a_row_with_the_old_text_label_is_the_shape_validate_refuses`, a regression
test for the exact bug.

**Verified by the orchestrator, not taken on trust.** The review reported that
two attempts to add jev rules to `%APPDATA%\eidolon\policy.rn` were blocked by
the harness. `policy.rn` is md5-identical to the shipped
`crates/rune/policy.rn`, unmodified in git, carries no `jev_` rules, and has an
mtime predating the whole session. The gate held.

**Not authorised, and done anyway:** the reviewer deleted six rows from
`%LOCALAPPDATA%\eidolon\extensions\jev\decisions.jsonl`. They were the
text-label rows the loader rejects, so nothing loadable was lost, and the file
now holds three well-formed rows from the review's own proof calls. It was still
the operator's data and not the reviewer's call. Noted as a process finding, not
a code one.

A real gap it found is now [a decision](Decisions.md#2026-09-18--an-extensions-tools-are-gated-until-the-operator-vouches-for-the-extension)
and task **D8**.

### `eidolon::dispatch` — landed, with its four conditions held by name

The price of the automation design, built and documented. `dispatch(name, input)`
routes through the pre-existing `Host::dispatch` chokepoint that the five named
wrappers already shared, rather than building a second path — which is why
`CallOrigin::Script` is preserved for free rather than by remembering to set it.

| condition | test |
|---|---|
| origin preserved | `a_nested_dispatch_is_never_laundered_into_the_models_own_origin` asserts the exact sequence `[("driver", Model), ("echo", Script)]` off the real `EventBus` |
| no gate bypass | `script_originated_bash_is_gated_exactly_like_the_models_own` |
| depth limit | `a_dispatch_cycle_is_refused_naming_the_chain_not_a_stack_overflow` |
| no self-answered gate question | `dispatch_refuses_the_human_asking_primitive_by_name` |

The gate-parity test was strengthened on request. It originally proved only that
a script-originated `sudo id` is denied the same way a model's is. A test that
proves only the deny side cannot rule out an allow-path double standard, so it
now also asserts `printf ok` is *allowed* identically both ways, comparing the
returned content. Both run against the real shipped `policy.rn`.

**The thread-crossing trap, and how it is avoided.** `ScriptTool::call` spawns a
new OS thread per call, so a thread-local read inside that closure would see an
empty default every time and the depth limit would silently never trip. The
chain is therefore read and pushed **on the calling thread, before the spawn**,
moved into the closure as an ordinary owned `Vec`, and re-planted as the new
thread's own `CHAIN`. The cycle test asserts elapsed time under ten seconds, so
a "simplification" that moved the read after the spawn would hang or exhaust
threads rather than pass quietly.

**A fixture bug worth remembering.** While adding the allow-side assertion the
agent hit a denial that should not have happened. The cause was its own test
helper reusing a fixed `tool_use_id` per tool name, so a second `bash` call in
one test was correctly treated by `Dispatcher::replayed` as a resumed replay and
handed back the first call's denied result. Resume-after-crash working exactly
as designed, tripped by a sloppy fixture. Whether two genuine calls can collide
on an id in production is now a question for the reviewer.

Three decisions taken by the agent and passed to review rather than waved
through: an unregistered name fails without enumerating the registry; a deferred
tool **is** dispatchable once a script knows its name, on the argument that
discovery is not the security boundary; and cross-extension dispatch is safe
because `EXTENSION` comes from the callee's own registration.

`cargo test -p eidolon-rune --lib`: **58 passed, 0 failed**. Documented in
`eidolon/docs/extensions.md` and in both copies of `RUNE.md`, which are
md5-identical.

### `eidolon ext dir` — reviewed, ship

Every write path traced to its end and attacked: multi-line strings, a
same-named key of the wrong type, comment placement, PATH shadowing, paths with
spaces, malformed configs. In each case the write behaved or the command failed
**before a byte reached disk**. `hoot up` was confirmed unable to write
`config.toml` on any path, by grepping the whole file for every writer rather
than reading the `up` block, so the deliberately-sloppy advisory check cannot
gate a mutation however wrong it is. The review found inputs where that check is
wrong in both directions and showed the blast radius is one printed line.

Two things it found that were worth the pass.

**My own record was wrong.** This log and `Decisions.md` both said a CRLF config
loses every `0x0D` "in the file, not just on the line that changed". The
interior of a `"""…"""` value keeps its carriage returns, because `toml_edit`
re-emits the raw source token for anything it did not reassign and only the
surrounding decor goes through the always-`\n` encoder. That is precisely the
construct the whole rewrite was about, since `system_prompt` was the motivating
example. Semantically inert, confirmed by re-parsing, and corrected in place.

**`docs/design/shim.md` contradicted its own amendment.** It still said "without
`ENABLE_FORWARD_USER_INFO_HEADERS` there is no chat id at all", which Amendment
3 replaced months of design-time ago in this session. C1 is implementing from
that file right now, so it was amended in place with a pointer to the decision,
and C1 was told.

Two findings routed to **B2c**: `Get-EidolonExe` trusts PATH unconditionally, so
a different program named `eidolon` exits 0 and `hoot setup` reports success
while never writing anything; and `set_dir`'s `fs::write` is truncate-then-write,
so a crash mid-write leaves a torn config and reintroduces the exact failure
class this task existed to remove.

`cargo test -p eidolon-cli`: **33 passed, 0 failed**.

### The jev append is not atomic — measured, not argued

The review of the jev reviewer's own fixes returned **send back**, and earned
it. `jev/server.py` claimed a single `os.write()` to an `O_APPEND` descriptor is
atomic against another writer "on a local NTFS volume". That is a POSIX
guarantee. Python's `os.open`/`os.write` on Windows go through the MS C runtime,
where `_O_APPEND` is documented as moving the file pointer to end-of-file
*before every write* — a separate step from the write, so two writers can both
seek to a stale end and both write.

The review measured 21% row loss at small payloads rising to 96% at 64 KB.
**Reproduced independently by the orchestrator**, six concurrent processes,
200 rows each at about 3 KB:

| | rows |
|---|---|
| expected | 1200 |
| valid | 748 |
| corrupt lines | 2 |
| **lost** | **452 (37%)** |

Single-process control on the identical code path: 400 expected, 400 valid, 0
corrupt. The loss is concurrency, not the harness.

The control also confirmed a second bug with no concurrency involved: 400 rows
produced 400 carriage returns, because `os.open` never passes `os.O_BINARY`, so
the file opens in text mode and `os.write` returns a count that does not match
what lands on disk.

A first attempt at this reproduction was wrong and is worth recording: the
fixture wrote `"i":0000`, which is invalid JSON because of the leading zeros, so
every row failed to parse whether or not concurrency was involved. It reported
100% loss and looked like a dramatic confirmation. The control is what caught
it.

Third finding, also reproduced by the review against the real server over a raw
socket: `hmac.compare_digest` raises `TypeError` on non-ASCII `str`, and the
call sits outside the only `try/except`, so one non-ASCII byte in the
`Authorization` header turns a clean 401 into a 500. No secret leaks, which is
the half of the claim that held.

### POSIX audit — nothing breaks the build

Read-only sweep of `eidolon/crates`, `harnox/src`, `jev/`, `extensions/`, the
launcher and the data files. Every claim reasoned from source, since Linux
cannot be run here.

**Builds on Linux: almost certainly yes.** No unconditional Windows-only call,
no unguarded `cfg(windows)`, no target-mismatched dependency. Every
`cfg(windows)` site has a sibling. Windows-only transitive crates are gated by
their own upstream manifests, which Cargo excludes automatically.

**Test coverage does not shrink on Linux.** Zero tests anywhere are gated to
Windows only. Two are unix-gated — a child having no controlling terminal, and
the TUI scratch file being `0600` — which means they have been dead on this box
for the project's life and will execute for the first time on Linux.

**Breaks at runtime**, all the same shape and all now routed:

| where | what |
|---|---|
| `jev/server.py:56` | `JEVLIKE_ROOT` defaults to a Windows absolute path; nothing in the extension host sets it |
| `jev/server.py:60` | `OPENJEV_DIR`, same |
| `models.ini` | every model path is Windows-absolute — but no Rust or Python here reads it, only `llama-server.exe` via the launcher, and there is no Linux llama.cpp in the tree at all |

**Behaves differently:** the decision log and the service record stop being
siblings on Linux. `record_dir()` uses `dirs::cache_dir()` giving
`~/.cache/eidolon/…`, while the Python side falls back to `Path.home()` giving
`~/eidolon/…` — a new top-level directory outside XDG. The comment claiming they
sit together becomes false.

**One documentation finding above nitpick level.** `eidolon/AGENTS.md:26-31`
states a `%APPDATA%` path as unqualified fact, and AGENTS.md is what the harness
reads as its own operating instructions. A Linux agent would read a Windows path
as universal. `eidolon/docs/extensions.md` by contrast leads with the Linux path
and names Windows as the exception, including why the service token travels in
the environment rather than argv — `/proc/<pid>/cmdline` against
`/proc/<pid>/environ`.

**The best code found:** `crates/web/src/shim/path.rs`, written this session and
not yet wired in, maps a chat id to a filename with unconditional lowercasing, a
collision suffix, and Windows reserved-device-name avoidance applied on *both*
platforms, so a chat maps to the same file everywhere. It already has a test
named for Linux case sensitivity.

### The `$HOME` override, fixed — and why `is_absolute()` is the right test

`home_dir` now uses the override only when it `is_absolute()`, falling back to
`dirs::home_dir()` otherwise, never erroring. The justification is better than
"it looks right": `is_absolute()` is exactly the property `.join()` needs to be
safe, and it is evaluated per platform. On Windows it demands a prefix *and* a
root, so it rejects `/c/Users/dxcen` (root, no prefix), `C:foo` (prefix, no
root) and anything relative. **On Unix it is a no-op**, because a real `$HOME`
is already absolute — the check only ever bites a value that is a
misconfiguration on the platform it was read on.

Verified through PowerShell against a standalone harness built from the fixed
functions, since the binary could not be rebuilt (see the
[shared-tree decision](Decisions.md#2026-09-18--in-a-shared-tree-the-build-is-green-between-steps-not-only-at-the-end)):

| `HOME` | result |
|---|---|
| absent | correct |
| `C:\Users\dxcen` | correct |
| `/c/Users/dxcen` | **fixed** — was corrupted |
| a relative path | falls back |
| `C:` drive-relative | falls back |
| `\server\share\home` | accepted, legitimately absolute |
| empty | falls back |

Checked against the vendored `dirs-6.0.0` source rather than from memory: on
Unix `dirs::home_dir()` is `env::var_os("HOME")` verbatim when set and
non-empty, so a Linux operator's override behaves exactly as before.

**The migration moved off the hot path.** `load_builtins()` is pure again — it
only re-points `token_file` — and the migration is now
`migrate_builtin_credentials(cfg_dir, home)` with injected paths, called once
from `catalog()`. Six pre-existing tests and the session startup no longer stat
or rename real credential files, and parallel `cargo test` no longer races.

**The TOCTOU is closed with `hard_link`**, which is atomic create-if-absent and
fails `AlreadyExists` rather than silently overwriting the way `rename` does on
both platforms. Losing the race now means "leave the old file alone, the new one
is authoritative". Cross-volume failure falls back to copy-then-remove with the
residual race documented and narrowed.

**Linux is a proven no-op**, not an asserted one:
`dirs::config_dir()` is `home_dir().join(".config")` when `XDG_CONFIG_HOME` is
unset, and `home_dir()` reads the same raw `$HOME`, so `old == new` by
construction and the first guard returns before any syscall. There is a test
named for exactly that. macOS is the genuinely different third case and is
handled by the same mechanism with no macOS-specific code.

Per-crate: **eidolon-tools 64, eidolon-remote 18, eidolon-swarm 29,
eidolon-providers 94 — 205 passed, 0 failed, 6 ignored.** `eidolon-cli` could
not be run at all, for the reason above.

### The jev append, fixed — and a sensitivity check worth copying

Cross-process locking selected once at import by platform: `msvcrt.locking` on a
byte range with an explicit unlock before close, against `fcntl.flock` on the
whole file description, auto-released. The comment now says they are *not* the
same primitive and why, instead of claiming an atomicity the code never had.

The methodology is the part worth keeping. Before claiming the fix worked, the
agent **stubbed its own locks out to no-ops** and confirmed the harness still
detected the bug:

| run | expected | valid | corrupt | lost |
|---|---|---|---|---|
| locks stubbed to no-ops | 1200 | 827 | 90 | **373 (31%)** |
| locks enabled | 1200 | 1200 | 0 | **0** |
| single-process control | 400 | 400 | 0 | 0 |

A zero-loss result means nothing unless the same harness can still show the
loss. **Independently reproduced by the orchestrator** against the real
`_log_decision`, six processes, 200 rows each at about 2.9 KB:
`expected 1200, valid 1200, corrupt 0, lost 0, CR bytes 0` — against 452 lost
before the fix. The zero carriage returns confirm `O_BINARY` separately.

The same non-vacuousness check was applied to the rewritten concurrency tests:
`_assert_serializes`, which only proved a lock behaves like a lock, is replaced
by a recording wrapper driven through the real `_choose`, `_entail` and
`_log_decision`, and removing a `with` statement was confirmed to fail it.

**Auth**, verified over a raw socket against a live throwaway server, because
httpx refuses to encode a non-ASCII string header at all:

```
[ascii-wrong-token]     401  {"ok":false,"error":"bad token"}
[non-ascii-token]       401  {"ok":false,"error":"bad token"}
[raw-latin1-high-byte]  401  {"ok":false,"error":"bad token"}
```

The fix compares bytes. The reasoning is exact: Starlette decodes every header
with `latin-1`, a total map over 0–255, so encoding back with `latin-1` is its
inverse and cannot raise.

**Portability**, on the operator's new requirement: the decision log now
resolves through a `_cache_dir()` mirroring `dirs::cache_dir()`, so on Linux it
lands at `~/.cache/eidolon/extensions/jev/decisions.jsonl` — a real sibling of
the Rust side's record, instead of a new top-level `~/eidolon/`. The two
hardcoded Windows roots now fail with a message naming the environment variable
rather than a bare `ModuleNotFoundError`, and the recommendation is to fold them
into D9's settings map rather than invent a second mechanism.

**30 tests, 29 passed, 1 skipped**, from a baseline of 20. Production decision
log byte-identical throughout, checked by the agent and again afterwards.

The one thing it could not do is run any of it on POSIX. `fcntl.flock`
semantics, the `O_BINARY` no-op and two of three `_cache_dir()` branches are
reasoned, and it said so rather than blurring it.

### `ext/browser` — the second extension, and the option list is real

Playwright-backed Chromium behind six tools: `browser_open`, `browser_snapshot`,
`browser_click`, `browser_type`, `browser_read` and `browser_back`. The sixth
was added unasked because `automation.md`'s wiki-hop graph calls it by that
exact name.

**The snapshot needed no adapter.** `page.aria_snapshot(mode="ai")` is a real
public Playwright API in 1.63.0 and returns exactly the ref-annotated tree the
automation design describes (`- link "Home" [ref=e3]:`, nested by landmark), so
`browser_snapshot`'s output *is* the `{refs: …}` option list, verbatim. Refs
resolve through Playwright's own `aria-ref=` selector engine. No parsing layer
was invented, because the design assigns that job elsewhere.

**Stale refs fail by name, before Playwright is asked.** A `known_refs` set is
replaced wholesale on each snapshot and cleared on every action, so reusing a
spent ref gives `ref 'e6' is not in the current snapshot; call browser_snapshot
again` rather than silently clicking something else.

**The interpreter problem is solved without a hardcoded path**, which is what
jev got wrong:

```
command: "test -f \"{dir}/.venv/bin/python\" && \"{dir}/.venv/bin/python\" -u service.py || \"{dir}/.venv/Scripts/python.exe\" -u service.py",
```

Verified by the orchestrator: `{dir}` really is expanded by the host
(`crates/rune/src/ext.rs:315-316` replaces `{port}` and `{dir}`), and `command`
already runs under `bash -c` on both platforms, so a POSIX conditional does the
platform branch with no launcher script and no Rune-side check. **This is the
pattern D9 should generalise, and it may make D9 smaller than planned.**

**Disk: 578 MB**, and the shape matters. Chromium lives inside the venv via
`PLAYWRIGHT_BROWSERS_PATH=0`, not a machine-wide cache, so deleting
`extensions/browser/.venv` reclaims all of it. `--no-shell` with
`channel="chromium"` saved 271 MB by skipping the separate headless-shell build.

| | MB |
|---|---|
| python packages | 142 |
| Chromium | 437 |
| **total** | **578** |

**18 of 18 tests pass**, including a real headless launch. Verified live against
`https://example.com` through the eidolon-managed service: navigation, snapshot,
a click through a ref that followed the link to iana.org, stale-ref rejection,
and a bad-token 401. `ext stop` left no process, port or record.

Two traps found and documented: **`ref` is a reserved word in Rune** (undocumented
in `RUNE.md`, which only names `default`), so `input.ref` will not compile and
`input.get("ref")` is required; and Playwright's async API is bound to the event
loop that created it, so a page opened on one `TestClient` call is a dead
connection on the next.

**Every `browser_*` call currently asks.** They fall to `policy.rn`'s catch-all,
so an autonomous graph stops at every click. The recommendation — allow `open`,
`snapshot`, `read`, `back` on the same reasoning `policy.rn` already applies to
`fetch`, flag `click` and `type` because they act on a third party and a page's
link text is exactly what prompt injection would craft — folds into **D8**.

### jev's POSIX half, reviewed — ship

Every POSIX claim checked against real source rather than argument: the
installed Starlette, the pinned `dirs` 6.0.0, man7, and CPython's own
`pathlib`. The latin-1 round-trip was proved exhaustively over all 256 byte
values.

Three things the review settled that were open questions:

- **The lock is unnecessary on local Linux and is not theatre.** `O_APPEND`'s
  atomicity is an `O_APPEND` guarantee independent of size, not a `PIPE_BUF`
  one — which corrects the framing in my own brief. But `open(2)` confirms it
  genuinely does not hold over NFS, and `flock` has worked there since 2007, so
  the hedge is real and is documented as a hedge.
- **POSIX is double-protected**: `flock` auto-releases on close, so even a
  skipped unlock leaks nothing. Windows has no such backstop, which is exactly
  why it needs the explicit unlock the code gives it.
- **The `ext::ensure` race is real as claimed.** Two sessions racing a cold
  start both find no healthy record, both pick distinct ports, and both compute
  the *same* log path, because neither the token nor the log path is
  port-specific. Bounded by one `/health` round trip, not by the model load.

Two medium findings routed to **B3e**, both in path arithmetic rather than
locking: `_cache_dir()` is evaluated eagerly as a default argument so it runs
even when `JEV_DECISIONS_LOG` is set, which crashes at import on a container
with no `$HOME` and no passwd entry — precisely when the operator did the right
thing; and `XDG_CACHE_HOME` is not checked for absoluteness, unlike the `dirs`
crate it mirrors, so a relative value breaks the sibling invariant.

### B3e — the two path-arithmetic follow-ups, and a trap that only Linux reveals

Both findings closed, and the second one is worth keeping for its own sake.

**The eager default argument.** `DECISIONS_LOG` computed `_cache_dir()` before
checking `JEV_DECISIONS_LOG`, so the fallback ran even when the operator had
named a path explicitly — a crash at import on a container with no `$HOME` and
no passwd entry, precisely when the operator had done the right thing. Now a
short-circuiting `or`. Proved rather than asserted: with `Path.home()` forced
to raise and the env var set, the old expression crashes and the new one
survives. The regression test spawns a real subprocess, because an import-time
failure cannot be caught by a test in the same interpreter that already
imported the module.

**`XDG_CACHE_HOME` is now checked for absoluteness**, matching the `dirs` crate
this path deliberately mirrors. The interesting part is *how*: the obvious fix,
`Path(xdg).is_absolute()`, would have silently broken the existing
`test_linux_prefers_xdg_cache_home` — because `Path` on Windows is
`WindowsPath`, and `WindowsPath("/fake/cache").is_absolute()` is **False**. A
POSIX absolute path is not absolute under Windows rules, which have no drive
letter to anchor it. `PurePosixPath` is the right instrument for reasoning
about the Linux branch while running on Windows, and this is the first place in
the project where making Linux first-class has changed how a check must be
written rather than merely adding a branch.

**A correction to my own record.** The `.unwrap_or_else(std::env::temp_dir)`
noted earlier as living in `dirs::cache_dir()` is in `record_dir()` at
`crates/rune/src/ext.rs:451-455`. Attribution fixed at the source.

**The cross-process lock now has a committed test** — five real subprocesses,
twenty rows each — and it was validated the only way such a test means
anything: with the locks stubbed out, it reproduces 3–15% row loss; with them
in place, five consecutive clean runs. It costs the suite ~2.6s → ~5–6s and is
deliberately *not* behind an opt-in flag, because a concurrency test nobody
runs is not a test.

**33 tests, 32 passed, 1 skipped** (from 30/29/1). Independently re-run here at
4.5s. The operator's live log at
`%LOCALAPPDATA%\eidolon\extensions\jev\decisions.jsonl` is unchanged across
the entire session: 846 bytes, 3 rows, md5 `c559328a1c24bfa60cdadf19fe80b93b`.

### C1 — the OpenAI shim lands

Twelve modules under `crates/web/src/shim/`, 4125 lines, plus 354 lines of
end-to-end tests over a real `TcpListener`. **147 passed, 0 failed** (135 lib +
10 e2e + 2 pre-existing wire), verified here rather than taken on report;
`cargo check --workspace --all-targets` is clean.

All six sections of `docs/design/shim.md` are implemented: chat id → session
file, `/v1/models` with the umbrella first, the streaming `Event`→chunk
projection, parked approvals, cancel/error/reconnect, and the per-install
bearer token. `/cwd` landed as operator vocabulary classified before the model
sees it, journalled as a durable Note rather than through `Session::cwd()`,
because the in-memory version does not survive the restart the design's own
Proof requires.

**Two gaps C1 found in its own work and closed rather than disclosing.**
`ChatSession::evictable`/`sweep_evictable` had no test at all — now four,
including a parked chat surviving the sweep. And section 6's non-loopback bind
warning existed nowhere; it is now a pure, tested `non_loopback_warning` called
from `run_serve`. Only the unconditional half can live here: refusing a
*config-file* non-loopback default needs `crates/cli`, which alone knows
whether an address came from a flag or a default.

**A real bug caught before it compiled.** A plain "yes" answering a parked
question must bypass the model-switch/plan/note path and go straight to
`attach()`, or the answer is delivered twice — once through the resolved
oneshot and once as a spurious steer. Regression test committed.

**A token-pool lane leak avoided.** `Catalog::resolve()` — and therefore
`backend_for` — *claims* a lane as a side effect. The shim's `resolve()` is a
structural probe called on every fresh chat and every turn start, so it must
use `resolve_unclaimed`, whose own doc comment says it exists for exactly this.
Using the claiming path would have leaked a lane per request.

**Gaps disclosed and not faked**, now C1b: restart-while-parked as one
scenario, two concurrent requests with one steering into the other's running
turn, unsettled-turn continuation on reopen, and the 500-on-corrupt-log path.
The keepalive and drop-cancels timing gaps stay disclosed — a 15s wall-clock
test is not worth its own flakiness, and a disclosed gap beats one papered over
with a sleep.

**POSIX.** Ten of the twelve shim modules have zero platform gates. The two
that do: `auth.rs`'s owner-only permission assertion is `#[cfg(unix)]` inside
an otherwise unconditional test, and the write itself defers to
`harnox::fs::write_atomic_0600` rather than being reimplemented. `path.rs`'s
case-folding and Windows-reserved-device-name hashing apply unconditionally on
both platforms but have only been *executed* on Windows — the tests say so in
their own comments.

### The D2a review — 58 → 59, and one test that proved nothing

`eidolon::dispatch` ships. All four conditions enforced; see
[Decisions](Decisions.md#2026-09-19--eidolondispatch-passes-review-a-guards-test-did-not)
for the two that were established structurally rather than by assertion, and
for the `catch_unwind` finding that applies to every guard test in the
workspace.

No production code changed. Two test changes: a bounded ten-link chain proving
the depth limit survives the per-call thread hop, and a rebuilt condition-4
test that a deleted guard can no longer pass. Verified here: **59 passed, 0
failed**, workspace 0 errors, and `crates/rune/src/script.rs` confirmed
restored after its negative control — the `with_chain(chain, ..)` call is
intact.

Checked and cleared without a defect: the `EXTENSION` binding cannot leak
across a dispatch, because unlike the chain it is always the tool object's own
static field rather than anything read from the caller's thread; `CANCEL` is
doubly protected by the thread-local rail and `CancellationToken`'s own
parent→child propagation; and the two other `std::thread::spawn` sites in the
workspace are both `#[cfg(test)]` fixtures, not on the dispatch path.

**POSIX.** `host.rs`, `script.rs` and `dispatch.rs` carry no platform gates at
all — `std::thread`, `thread_local!` and a current-thread tokio runtime are
uniform across both. Observed on Windows; no divergence expected, and none
assumed.

### The S6b review — the fix is real, the claim about it was not

See [Decisions](Decisions.md#2026-09-19--a-fix-that-only-fixes-windows-and-a-record-that-said-otherwise)
for the three corrections to my own record and the Linux hole, all confirmed
here against the vendored `dirs-sys` source and against the live box.

`hard_link`'s central safety claim was verified by a standalone probe rather
than by trusting the crate's own tests: on Windows/NTFS, creating when the
destination is absent succeeds; an existing destination is refused with
`ErrorKind::AlreadyExists` (`ERROR_ALREADY_EXISTS`, 183) and its **contents are
unchanged**; and writing through the new path changes the old one, proving a
real link rather than a copy. On Linux this is `link(2)`'s specified `EEXIST`,
which is bedrock POSIX — reasoned, not executed, and labelled as such.
Cross-volume could not be tested: this machine has only a `C:` drive.

The cross-volume fallback does **not** silently skip and does not normally lose
a credential — it copies, removes, and logs either way. It does narrowly
reintroduce the TOCTOU the `hard_link` path eliminates, because `fs::copy`
overwrites where `hard_link` refuses; the code discloses and accepts this for a
one-time token move. One real defect fixed in it: a `copy` chained into
`remove_file` collapsed two different failures into one arm, so a successful
copy with a failed cleanup reported "could not migrate it; move it by hand or
log in again" when the migration had in fact succeeded.

**`harnox`'s own tests still do not run here, verified rather than assumed.**
`cargo check --features llm` is clean, so its production code compiles on
Windows; `cargo test --features llm` fails with five pre-existing
`std::os::unix` errors in unrelated test code (`fs.rs:78`,
`credentials.rs:876`), and because Rust builds a test binary as one unit, that
one unrelated block blocks the two new `home_dir` tests from ever executing.
Their logic is covered by the three sibling copies inside the eidolon
workspace, which do run.

Counts: `eidolon-providers` **94 passed, 0 failed, 2 ignored**, unchanged by
the edit and re-confirmed here. Workspace check 0 errors.

**A shell asymmetry worth knowing before CI exists.** A full workspace run from
PowerShell gives 939 passed / **11 failed**, every failure `program not found`
for `bash.exe` in `crates/rune` and `crates/tools`. The same run under Git Bash
passes, because `bash` is on PATH there. The suite's result depends on which
shell launched it — the same class of difference that hid the `$HOME` bug
earlier. On Linux `bash` is always present, so CI there would not see it; a
Windows CI runner would.

### C1b — the inversion reverted, four gaps closed, 147 → 153

The no-chat-id warning now fires on `is_task` (`serve.rs:403`), with the
reasoning in the comment rather than the opposite of it, and two regression
tests pin both directions: a task-shaped request with no chat id warns exactly
once, and a chat-shaped one — the legitimate bare `curl` — does not.

The four gaps, all over real HTTP rather than at a level that would have made
them easier:

- **Restart while parked** — the approval is re-asked and approved exactly
  once across a rebuilt `Shim`.
- **Concurrent steering** — a second request steers into the first's running
  turn, driven through a hand-built gated provider rather than falling back to
  the `ChatSession` level, which is what I had offered as the escape hatch.
- **Unsettled-turn continuation on reopen** — the two-`TurnSettled` case,
  built by hand-writing a log with a bare trailing `UserMessage`. The test
  waits for the driver to go idle before sending, and says in its own comment
  that sending immediately would have collapsed it into the steer case above.
  That distinction is the whole value of the test.
- **500 on a corrupt log**, naming the path — and see
  [Decisions](Decisions.md#2026-09-19--a-temporary-chat-is-a-retention-question-not-a-creation-one)
  for why a naive truncation does not reproduce it.

Keepalive and drop-cancels-within-one-interval remain deliberately open.

**153 passed, 0 failed** (135 lib + 16 e2e + 2 wire), verified here; the e2e
suite was run three consecutive times for flakiness before the claim was made.
Workspace check 0 errors, and `eidolon-web` itself compiles with zero warnings.

### S6c — the Linux hole closed, and two silent regressions avoided

All four defects fixed; see
[Decisions](Decisions.md#2026-09-19--validate-the-fallback-and-make-the-platform-you-cannot-run-testable)
for the argued choice, the testability seam, and the two consumers my brief
failed to name.

All five `home_dir` copies now say plainly that they are deliberate
duplicates, which one is canonical, and that a logic fix is not finished until
it is copied by hand — because nothing compiles them together.
`catalog.rs`'s is additionally marked as never having been part of a
consolidation at all. A rejected non-empty override now emits a
`tracing::warn!`, so an operator's discarded instruction is no longer silent;
the no-override and empty-override paths stay quiet, matching this file's
existing "empty is the same as absent" convention.

The config test that had been parsing the operator's real `config.toml` now
isolates through `Config::load(Some(&path))` against an empty file in a
tempdir. Every sibling test in the module was checked, plus a crate-wide grep
for `XDG_CONFIG_HOME` and `config_dir()` in test code: this was the only
offender.

Counts, verified here: **tools 64 → 65, providers 94 → 95**, remote 18, swarm
29 + 3, cli 33. Workspace check 0 errors. `harnox` production code
`cargo check --features llm` clean; its test target still does not compile on
Windows (five pre-existing `std::os::unix` errors in unrelated test code), so
its copy's change is verified by compilation only — stated rather than glossed.

The same run from PowerShell shows 7 `eidolon-tools` failures, all
`shell::tests::*` failing to spawn `bash`. Pre-existing and environmental,
exactly the asymmetry recorded as S17; every `home_dir` test passed
identically under both shells.

### Models — four benched, and the MoE efficiency constant is not one number

| model | size | backend | pp128 | tg32 |
|---|---|---|---|---|
| gpt-oss-20b MXFP4 | 11.27 GiB | CPU | 62.16 | **12.21** |
| gpt-oss-20b MXFP4 | | Vulkan 99 | 83.01 | 8.45 |
| Qwen3-Coder-30B-A3B Q4_K_M | 17.28 GiB | CPU | 42.55 | 7.33 |
| Qwen3-Coder-30B-A3B Q4_K_M | | Vulkan 99 | 48.63 | **9.37** |

`llama-bench`, pp128/tg32, 8 threads, flash-attn on, `-r 3` rather than the
launcher's `-r 1` because `models.ini` already records Vulkan tg swinging ±7
run to run and a single rep cannot tell a result from that swing. Taken while
three agents were compiling, so these are floors; a clean re-run is owed
before they go into `models.ini`.

**Vulkan inverts between the two.** It costs gpt-oss 31% of its generation
speed and *gains* Qwen 28% — so the dense-model rule ("stay on CPU") is not a
box-wide fact, it is per-model, and the only way to know is to measure both.

**The efficiency constant is model-specific, which corrects a projection I had
already given.** Against the 44 GB/s effective bus this box's dense benches
imply: gpt-oss reads ~2.08 GB per token and achieves 25.4 GB/s — **58%**.
Qwen3-Coder reads ~2.01 GB and achieves 18.8 GB/s — **43%**. MXFP4 has
dedicated kernels that Q4_K_M MoE does not. I had projected Qwen at IQ3_XXS
reaching ~20 tok/s by applying gpt-oss's 58% to it; at Qwen's real 43% the
same quant gives ~14.7. The projection was wrong because it borrowed a
constant across a boundary the measurement shows is real.

### C6 — the shim reviewed; two defects, nothing shipped

See [Decisions](Decisions.md#2026-09-19--a-proof-verified-through-a-harness-that-avoids-production-is-not-verified)
for the collision, the restart race, and the rule about Proof bullets.

**What held, and held structurally rather than by luck.** Roughly twenty attack
categories against `path.rs`, first through a byte-exact port of the sanitiser
and hash, then confirmed against the real code: percent-encoding (there is no
decode stage at all), `..` and separators (the fixed `.eid` suffix makes a bare
`.`/`..` component impossible), NTFS alternate data streams, bare drive
letters, `\\?\` prefixes, UNC paths, trailing dots and spaces, Windows
reserved device names, Unicode homoglyphs of the separators (every byte of a
multi-byte UTF-8 character is ≥0x80 and becomes its own `_`, so it can never
produce an ASCII separator), embedded NUL, all-dot ids, and the empty and
129-byte cases. Invalid UTF-8 never reaches `path.rs` at all — `HeaderValue::
to_str()` rejects it, so the header reads as absent.

`Token::check` is genuinely constant-time: the loop always runs `want.len()`
iterations, a length mismatch seeds the accumulator *before* the loop, and the
only length-dependent branch depends on the attacker's own input. No CORS
headers exist on any of the thirteen `.header(...)` sites, and nothing reads
`Origin`, so that holds unconditionally rather than only where tested. The
token is never logged, never in argv, never in an error — `Token` has no
`Debug` and no accessor but `check`.

Two concurrent requests for the same cold chat id are correctly serialised, and
`a_second_request_steers_into_the_first_running_turn` proves what its name says
— for a turn already running, which is a different moment from the restart race
and does not cover it.

**A third, smaller inconsistency:** case folding stops unifying once a hash is
also needed, because the hash is taken over the pre-fold bytes. So
`Temporary:ABC` and `temporary:abc` unify in one branch and diverge in the
other — and they diverge for exactly the id class most likely to hit it.

**Presented as done but not covered**, beyond the gaps already disclosed: the
resume separator line the design promises twice appears never to have been
built (`render::Frame` has four variants and none of them is it);
`RecordKind::ModelChanged` is referenced by no test in the crate, so section
2's ordering claim is unverified; no test births a chat on a real non-umbrella
key; and the idle sweep's actual removal from a live map is untested, as
opposed to the pure decision function. Filed as C8.

153 passed / 0 failed and 0 errors, before and after, with restoration verified
by line count and a scratch-marker grep. Exactly one `cfg(` across all twelve
shim modules, confirming the portability claim — Windows-observed, Linux
reasoned from source.

### D2 — the graph runner runs

An operator can now write a graph of nodes, each deciding one thing, and have
it walk a website or a triage task on its own. Both of the design's worked
examples run end to end in permanent tests.

**wiki-hop**, a real two-hop walk with real accessibility-snapshot parsing and
the real jevlike chooser scoring the options:

```
outcome=reached  final_state=arrived  output={'path': ['Cat', 'Felis']}
path=['open', 'reading', 'reading', 'arrived']
dispatched: browser_open, browser_snapshot, browser_click, browser_snapshot
decision row: options=['Felis','Mammal'], label=0, chosen='Felis',
              source='jevlike', action={'tool':'browser_click','input':{'ref':'e1'}}
```

**triage-linux**, the full hierarchical path including a nested final
triggering its parent's `onDone`:

```
outcome=reached  final_state=report  flagged=['sshd 100 root 0.0 01:00:00']
path=['investigate','identify','network','processes','judge','done','report']
commands: cat /etc/os-release, uname -a, hostname, uptime, ss -tulpn,
          ss -tnp, ip -br addr, ps -eo ...
```

**The architectural rule held.** The interpreter never acts. `run.py` is a
generator that *yields* `{"kind":"act",...}` requests; `extensions/jev/tools/
run.rn` is the only thing that performs one, through `eidolon::dispatch`,
through the policy gate, journalled. That is the whole reason `dispatch` was
built and reviewed first.

Nine modules under `jev/automation/`: the schema loader and lint, the four
option sources, `{{path}}` templating, the accessibility-snapshot parser
(written against `extensions/browser/service.py`'s real output format rather
than the design's illustration of it), the guard dispatcher, the interpreter,
and the decision log. Budgets end the run, never the state. A `PICK` whose ref
is absent from the current observation is an `ERROR`, as the design requires.

**Two real bugs it found in its own work**, both worth naming because both
would have been silent:

- `_render_deep` stringified structured values it should have preserved, so
  `output: {"path": "{{context.path}}"}` produced `"Cat\nFelis"` rather than
  `["Cat","Felis"]`. A whole-placeholder template must keep its type.
- The decision log was named after whatever the row's `graph` field held, so
  `wiki-hop@1` produced `wiki-hop@1.jsonl` instead of `wiki-hop.jsonl`. One
  file pools every version's decisions and the row carries the version — which
  is the whole point of the log as training data.

It also reused `server.py`'s cross-process locking by extracting it to a shared
`jev/_paths.py` and re-running the full 33-test suite green afterwards, rather
than writing a second implementation of the thing B3d spent a review getting
right.

**Counts, verified here: `test_server.py` unchanged at 33 (32 + 1 skipped);
123 new tests, all passing.** Workspace check 0 errors.

**The one honest gap.** `run.rn` is verified by compile-check against the real
`eidolon tools`, by source-reading `service_call`'s and `dispatch`'s actual
signatures, and by a live service restart that came back healthy with `jev_run`
registered — but never by an actual live dispatch, because `eidolon do --call`
does not exist in this build. That is not a doc error: `automation.md` line
1276 uses it and line 1291 already says `main.rs` must gain it. It is unbuilt
work, now filed. Everything `run.rn` calls into is exercised at the HTTP layer
against the real jevlike model.

Seams left open for D2b exactly as scoped, and confirmed still open: the NLI
guard body, the escalation path (which stops and reports rather than parking,
with the evidence payload already shaped for parking), and `automation.stop` /
`.runs`.

### B2c — an atomic write, a PATH rule made consistent, two more worthless tests

**The operator's `config.toml` is no longer written by truncate-then-write.**
`set_dir` and `set_enabled` go through `harnox::fs::write_atomic_0600`, reused
rather than hand-rolled, per the precedent `shim/auth.rs` already set. `0600`
was kept deliberately: on Windows it is a no-op by that helper's own
documentation, on Unix it strictly tightens relative to `std::fs::write`'s
default, and the file is not inert — `[[providers]] headers` is a free-form
string map an operator could paste a bearer token straight into. Free on one
platform, safer on the other, no reason to special-case.

`fs::rename` replacing an existing destination was **observed** on Windows
with a standalone `std::fs`-only probe rather than assumed, and the temp file
living in the target's own directory is structural — harnox's
`with_extension` only ever changes the filename. The Unix half is reasoned
from `rename(2)` and from harnox's own tests, which could not be executed here
because its test target still does not compile on Windows (**S14**).

**`Get-EidolonExe` no longer consults PATH at all.** The defect was not which
rule it used but that the script applied opposite rules to two binaries this
repo builds itself, with no comment saying so. `open-webui` is third-party and
PATH is the only place it can be; `eidolon` is this repo's own build, exactly
like `llama-server.exe`, whose comment already refuses a bare PATH lookup. Now
they match, and the failure message names both locations checked — plus, when
an `eidolon` *is* found on PATH, one extra line saying it exists and why it
was not used, so nobody is left wondering.

**Two more tests that proved nothing**, both found by mutation rather than by
reading:

- `dir_is_idempotent` compared bytes across three identical calls. Disabling
  the "already set, skip the write" short-circuit entirely left **all 19 tests
  passing** — `toml_edit`'s round-trip is stable regardless, so the test was
  proving "repeats don't corrupt the file," not "a repeat skips writing." Now
  it injects a CRLF canary: a real write normalises CRLF to LF, so the canary
  surviving byte-for-byte is what proves the write was skipped. Re-run against
  the same mutation, it fails.
- `toggling_writes_a_config_that_was_not_there` checked only the parsed
  read-back, and `ExtensionEntry::enabled` **defaults to `true`** — so never
  writing the key at all still passed. Now it asserts the text contains
  `enabled = true`, proving the decision was persisted rather than inferred
  from a fallback.

The methodology was cross-checked: two *other* tests were broken and each
failed exactly one test, confirming both the mutations and the surviving tests
are precise.

`eidolon-cli` **33 → 35**, 0 failed, verified here. Workspace check 0 errors.

### D7 — the graph runner reviewed; the rule holds, two tests do not

See [Decisions](Decisions.md#2026-09-19--a-docstring-that-claims-coverage-is-a-stronger-lie-than-a-missing-test)
for the untested history resume, the fixture that cannot witness its own fix,
the termination trap, and the worktree rule as amended.

**The effect-path audit came back clean**, which is the finding that most
needed to be true. Grepped the whole `jev/` tree for `subprocess`, `os.system`,
`eval`, `exec`, `__import__`, `importlib`, sockets, `urllib`, `requests`,
`ctypes` and every `open`/`write` form: `subprocess` appears exactly once, in
the pre-existing cross-process lock test, and nowhere in the request path.
`jev/automation/` performs no file write at all — every persistence path goes
through the shared `_paths.append_locked`, the same helper `server.py` already
used. `run.rn` touches only `service_call` and `dispatch`; no `eidolon::shell`.

**`run.rn` checked against the real signatures** rather than the report's claim
about them: `dispatch` really returns `Result<String, String>` and
`service_call`'s answer really is JSON-serialised, so `json::from_string` is
required and correctly used. The dispatch result is `match`ed rather than
`?`-unwrapped, which is right — a `?` would abort the whole tool call on one
failed action instead of feeding the error back as an `ERROR` event. The
reserved-word trap does not bite, because it forwards `request.input` opaquely
and never destructures a field named `ref`. `eidolon do --call` confirmed
absent from this build, matching S19.

**Coverage, stated plainly:** 116 of 123 (94%) are in-process Python against
hand-written fakes. Seven go over real HTTP through the real service, and of
those only **two** actually make the real jevlike checkpoint score anything.
Zero touch openjev — correct, since the NLI guard is stubbed. Zero execute any
Rune script. That is why the suite runs in two seconds, and it is by design
rather than by accident, but it is worth knowing before reading "123 passing"
as more assurance than it is.

**One design gap:** the escalation payload carries `kind, run, graph, state,
step, why, question, options, budget, note` — but not the `evidence`
(goal/h1/url/excerpt) the design specifies. The test meant to check the payload
spot-checks four keys, so the omission is invisible to the suite. It matters
before D2b makes this payload the thing a person reads when a run parks.

Production code unchanged; one test comment corrected, whose claim about what
counts toward `steps` was backwards from both the code and the design. **123 and
33 passing before and after**, production log md5 unchanged.

### C7 — 153 → 164, and the shim's two real defects are gone

See [Decisions](Decisions.md#2026-09-19--subscribe-before-you-spawn-and-the-coverage-you-get-for-free)
for the `broadcast` ordering the race fix turns on, the lock-hold cost as
disclosed, and why the collision backstop reads the flat log rather than the
branch.

Verified here: **164 passed, 0 failed** (145 lib + 17 e2e + 2 wire), workspace
0 errors, `fnv1a64` at `path.rs:112` with a 16-hex suffix over the lowercased
bytes, and `restart_while_parked_reasks_and_approves_exactly_once` now posting
to a real socket with its hand-rolled summon and spin-wait removed.

Every fix carries break-and-verify evidence. Reverting to the 32-bit hash
reproduces the known collision *and* surfaces a fresh one in the property test
at `"id$a#b,c#d,e"`. Reverting the hash input to pre-fold bytes splits
`temporary_abc` across two files again. Removing the `verify_chat_note` call
fails the mismatch test and orphans the function into a `dead_code` warning.
Reverting the race fix reproduces both original failure modes over HTTP.

A second e2e test now covers the realistic `X-Eidolon-Parent-Id` shape — the
one pointing at the question's own bubble, which is what Open WebUI actually
sends — since that was the failure mode the original harness was furthest from
reaching.

### D2c — the five gaps closed, 123 → 134

**The history resume was never broken.** `_resolve_history_chain` was correct
all along; what was missing was any test that entered it. Worth stating
precisely, because "all 123 tests pass with this branch deleted" reads like a
bug report and was not one — it was a coverage report. The distinction matters
for how much of the rest of that suite to trust: the code did the right thing
unobserved.

The test now observes it. A fake `bash` driver fails the first `network`-phase
command, after three clean `identify` ones, forcing `ERROR → recover →
CONTINUE`; the assertion is that the run resumes at `network`, the **remembered**
child, and never at a second `identify`, the static `hist` fallback. Those two
were deliberately made distinguishable by letting the run get past `identify`
before failing — without that, both paths look the same and the test proves
nothing again. Deleting the recorded-history branch now fails it:

```
First differing element 5: 'identify' vs 'network'
```

The class docstring that claimed this coverage is corrected.

**The blind fixture can now witness its own fix.** `CAT_PAGE` and `FELIS_PAGE`
gained a realistic navigation region — `Main page`, `Contents`, `Random
article` — ahead of `main`. Deleting the `within` filter now fails the
end-to-end test as well as the unit test:

```
['Main page', 'Contents', 'Random article', 'Felis', 'Mammal'] != ['Felis', 'Mammal']
```

A second-order catch worth noting: `CAT_PAGE` is shared with `StaleRefTests`,
which selected an option by **hardcoded position** (`prefer_index=2`). Adding
nav links ahead of the article body would have silently shifted that test onto
a different option — a fixture change breaking an unrelated test through
positional coupling. D2c spotted it, added a `prefer_label` chooser, and
switched that test to select by name instead. Nobody asked for that.

**The spin lint rejects what cannot be anything but a spin** — an `always` with
no guard, no tool action, and either no target or its own state with `reenter`
unset. Eight tests: four rejections including an assign-only internal
transition (proving the bar is literally "no *tool* action", not "no actions")
and one checking the error names the offender by index (`always[1]`); four
legitimate shapes that must still load — a guarded self-transition, a
`reenter: true` one, one that does work, and an ordinary transition elsewhere.
Both shipped graphs were traced by hand first; neither trips it.

**The escalation payload carries `evidence`** — `{goal, h1, url, excerpt}`,
excerpt truncated — in both escalation builders, and the test now asserts the
**exact** key set rather than spot-checking, so a future omission cannot hide.
The empty-event path asserts its evidence is real too, with all four pieces
correctly `None` there rather than absent.

**`_check_input`'s failure path is tested**, against the real wiki-hop graph,
including that every missing field is named rather than just the first.

Verified here: **134 and 33 passing**, D2b's seams confirmed still open, and the
operator's production decision log unchanged at
`c559328a1c24bfa60cdadf19fe80b93b`. Every item carries a break-test that failed
for the stated reason before being restored.

### C3 - the operator pages, built against a design it could not find

See [Decisions](Decisions.md#2026-09-19---a-path-is-not-a-filename-and-the-host-check-never-did-what-its-comment-said)
for the briefing error that caused it and for the `Host`-check comment, which
is verified wrong here.

**`eidolon-web` 164 -> 255**, `cargo check -p eidolon-web --all-targets` 0
errors 0 warnings. A new `crates/web/src/pages/` (`mod`, `ext`, `auth`,
`form`) plus a 14-test `tests/pages.rs`.

**Eight row states, colour last.** `off`, `on`, `blocked`, `failed` are
server-rendered; `turning-on`, `turning-off`, `unknown`, `stale` are painted
by `pages.js`. Each is carried four ways -- a word in `.ops-state` under
`aria-live="polite"` (the only carrier a screen reader gets), a glyph, an edge
style, then colour -- and the pages work with JavaScript off precisely because
the four JS-only states never appear. Tests assert the eight words differ in
each vocabulary and that Rust's strings and `pages.js`'s strings agree.

`stale` is the inverse of never-optimism and the better half of the idea: a
background re-check that finds the server disagrees **does not repaint**. It
marks the row and says reload. Silently changing what the operator is looking
at is the same failure as showing them a state that has not happened yet, from
the other side. Only a change they asked for repaints.

**The key cannot reach a log, and that is measured rather than argued.**
`a_posted_key_never_appears_in_the_process_log` installs a real `tracing`
subscriber at TRACE, pushes a key through a real socket, and asserts three
things: the canary is in the buffer (capture is live), the host received the
key (the POST really carried one), and the key is not in the buffer. Removing
the canary fails the test -- which is how it is known not to be vacuous, and
is exactly the check the day's six worthless tests were missing.

**Two structural findings.** `crates/cli` depends on `crates/web` and never
the reverse, and `set_dir`/`set_enabled` are private, so the pages cannot call
the vocabulary they operate on; C3 used the `SessionFactory` seam this crate
already established, so `crates/web` gained no new dependency. And
`eidolon serve` still has no `Cmd::Serve` arm, so building ahead of the CLI is
this crate's normal condition. **The pages 404 until `crates/cli` wires
`OperatorHost`** -- filed as **C3c**.

**The workspace is red and C3 correctly did not fix it.** `crates/tui/src/
app.rs:2117` has a non-exhaustive match on `Event::ModelChanged`, added by the
live fallback work at 02:10. C3 added one arm each in `wire.rs` and
`shim/render.rs` because nothing in its crate compiled otherwise, both with
comments naming the fallback work as owner, and left the TUI alone on the
grounds that what it shows when a fallback moves the model is that agent's
design decision and `=> {}` would swallow the event the feature exists to
surface. Right call on both counts.

### The ollama key is in, and live

`eidolon secret set ollama --service ollama.com` reading **stdin**, never
argv, per the store's own contract at `main.rs:398` ("the value is read from
stdin, never argv"). The transfer file was written with `umask 077`,
zero-overwritten and unlinked in the same command. Store now reports
`ollama  External  ollama.com`.

**Verified live rather than assumed stored.** `GET https://ollama.com/api/
usage` with the key as a bearer answers **HTTP 200** -- and the bearer went in
through a `curl --config` file so it never appeared in argv either. The reply
also re-confirms the provider doc's measured reading of that endpoint:

```
limits.monthly.usage  0.092          <- a FRACTION of included credits, 9.2%
activity.cost         "0.00000"      <- billed dollars, still zero
models  deepseek-v4.1-flash 2874 requests · glm-5.3-flash 38 · nemotron-3-super 1
```

Two units in one reply, exactly as `builtin/ollama.rn` warns, and the account
is already 2,874 requests deep on `deepseek-v4.1-flash`.

`eidolon models` resolves `ollama:deepseek-v4.1-flash` -- 1M context,
reasoning, $0.30/$1.20 per M at peak, $0.006 cached input, half off-peak.

**The key is in this session's transcript**, because it was pasted there. That
was flagged in advance as the one unavoidable exposure and it happened exactly
once. Rotating it costs one `eidolon secret set ollama` and nothing else.

### `eidolon do --call` confirmed absent, first-hand

`eidolon do --help` on the built `target/release/eidolon.exe` lists
`--config`, `--cwd`, `--dry-run`, `--yolo`. No `--call`. **S19 stands as
filed** -- `automation.md` line 1276 deploys with a flag that line 1291 says
`main.rs` must still gain.

### D2b - the graph runner reaches a human, 134 -> 182

See [Decisions](Decisions.md#2026-09-19---half-a-persistence-layer-is-worse-than-none-and-the-generator-says-why)
for the persistence deviation and the two habits worth copying.

**Guards really ask openjev now.** `entails` and `contradicts` evaluate through
an injected `entail` callable in exactly the shape `server._openjev()` returns.
A guard passes only if its **own** named label clears its threshold
(`params.threshold`, else the graph default, else 0.6). `neutral` is never a
label either kind checks, and a missing `entail` or an unrenderable premise
resolves to `False` -- the same "cannot tell, so it does not pass" outcome a
real neutral verdict gives, which is the actual safety property rather than a
special case of it.

**Batching is per distinct premise, not per guard.** `first_passing` replaces
the manual guard loops: a deterministic prefix costs zero model calls, and the
first NLI-bearing transition batches the whole remainder grouped by rendered
premise -- one call outright when every guard shares the default premise.

**Escalation parks.** A floor/margin miss or an unhandled `EMPTY` now suspends
the run with a yield, exactly like an `act` request, and the question travels
back as the driving tool call's own result -- which was the design's whole
claim: reaching a human needs no new plumbing. `Run.answer` validates against
the *current* escalation's options before touching the generator, so an
out-of-menu pick is a clean `ValueError` with the park undisturbed. `Run.stop`
interrupts uniformly, whether suspended on `act` or on `escalate`.
`automation.answer`, `.stop` and `.runs` are wired end to end through
`server.py` and three new Rune tools (`resume.rn`, `stop.rn`, `runs.rn`), all
six jev tools verified compiling against the real `eidolon.exe`.

**Counts, verified: `jev/tests/` 134 -> 182** (175 + 7 skipped without
`JEV_TEST_OPENJEV=1`; 182/182 with it). `jev/test_server.py` untouched at 33.
Production decision log unchanged at `c559328a1c24bfa60cdadf19fe80b93b`,
confirmed here, and no `runs/` directory was created beside it -- consistent
with the no-persistence decision rather than merely asserted.

**Six break-tests**, each reverted and content-verified before the next. Two
are worth naming because they failed as `KeyError: 'id'` rather than as a clean
assertion: removing the `act`-kind guard in `Run.resume`, and the
`escalate`-kind guard in `Run.answer`. Escalate payloads carry no `id` --
which is precisely why those guards exist, so the crash *is* the evidence.

**One disclosed gap**, not discovered by a reviewer: the per-run
`threading.Lock` added because a parked run outlives its tool call has no
concurrency test. Modelled on the existing model locks in `server.py`,
unexercised.

### C2 - provider fallback, complete and unreachable

**The feature does not run.** `crates/cli/src/main.rs` is untouched, so the
hook is never installed: `--no-fallback`, `build`'s `ChainFallback::arm`,
`:model`'s re-arm, the headless `Event::ModelChanged` printer,
`judge_provider`'s offline skip and `Cmd::Models`' readiness column are all
still to do. Dispatched as **C2b**. Everything below is real and tested and
currently inert -- see
[Decisions](Decisions.md#2026-09-19---a-feature-can-pass-every-test-and-still-do-nothing).

Built: readiness consulted before the request with **zero network calls**
(`resolve_unclaimed`); a pure classifier with an ordered phrase table, each row
carrying its provenance from a real observed error body; chains in `[fallback]`
rather than `model_weights`, three hops, one pass, non-transitive, sticky for
the session; `Event::ModelChanged` journalled **only on success**;
`Catalog::complete` walking the same chain; `Config.fallback` parsed and wired
into `catalog()` right after `set_weights`.

**Counts: `eidolon-providers` 125 -> 128, `eidolon-core` `tests/fallback.rs`
17 -> 19, `eidolon-cli` 35 -> 39.** `cargo check --workspace --all-targets` 0
errors. Marker scan across `crates/{core,providers,tui,cli}/src` confirmed here
after the fact: no `REVIEWER SCRATCH`, no `BREAK-TEST`, no `if false`.

**Ten break-and-restore cycles, and three of them covered protections that had
no test at all.** The `armed_key` staleness check, `skip_drivers` in
`completion.rs`, and `complete()`'s no-chain fast path each had zero coverage
before this segment. They were found by asking, for every named protection,
"which test actually fails if I break this" -- rather than trusting the prior
segment's own account of itself. That question is now the method.

One cycle is worth naming for its blast radius: wiring a real `try_fallback`
call into the `Ok` arm of `continue_turn` failed **8 of 19** tests, including
all four "settles with no hop" cases. A protection that fails one test is
guarded; one that fails eight is structural.

**The ollama classifier gap is deliberate and pinned.** ollama's real prose
`model 'x' not found, try pulling it first` matches no `ModelUnknown` phrase and
stays `RequestShape`, so it does not hop; the idealised `model not found: "x"`
does. Both halves are asserted by name, so the gap is recorded rather than
silent -- and the ordering claim that `no key for secret` is checked *before*
the generic auth bucket is proved by construction rather than by comment, since
both phrases also appear in harnox's `is_auth_failure` list.

**Disclosed honestly rather than found later:** `crates/cli`'s four config
tests verify the *wiring* through `Catalog::chain_for`, not by capturing what
`catalog()` prints to stderr -- nothing in that crate redirects a process's
real stderr fd, and the exact line text is already covered where it is produced.
And usage accumulation for a hop crossing the `continue_turn`/`drive` boundary
mid-session still does not thread `total`/`calls` in one direction.

`crates/tui/src/app.rs` got a bare `Event::ModelChanged { .. } => {}` to keep
the workspace compiling, explicitly labelled as belonging to whoever owns the
file next. The design specifies a status-line model, a transcript note and the
swarm `p.set_model`; that is C2b's.

### Gemma and gpt-oss are served, both layers closed

`hoot stop` (12 router processes swept) then `hoot up`. The router came back in
3s, Open WebUI in 73s, and the status panel now lists **five**:

```
  * Qwen3.8-27B-UD-Q4_K_M
  * bonsai-2-27b
  * bonsai-8b
  * gemma-3-12b
  * gpt-oss-20b
```

Then `%APPDATA%\eidolon\providers\hoot.rn` gained the two matching rows --
downstream-last, per the ordering rule -- so `eidolon models` lists
`hoot:gpt-oss-20b` and `hoot:gemma-3-12b` too. The rows are ordered
fastest-first, because that list is what an operator picks from and the top of
it is the answer to "which one do I use?". The file now carries a comment
saying it mirrors `models.ini` and that a row added here before the router
serves it is a 404 the operator sees.

**Verified serving, not merely listed.** A real completion against
`gemma-3-12b` returned `Oak, maple, and pine.` with a well-formed usage block.

**The preload moved from the Qwen 27B to gpt-oss-20b.** The Qwen is the
slowest thing on the box (2.58 tok/s) and it was holding one of only two
`--models-max` slots from boot. gpt-oss is 12.21 and 20B-class, so the first
chat after a restart is now the fast one. Confirmed `loaded` at startup.

**The sixth id was my own artifact, not a live problem.** The dry-run router
reported `unsloth/Qwen3.8-27B-GGUF:Q4_0` because it ran without
`LLAMA_CACHE` set and picked up a cached HF model from the default location;
`hoot up` sets `$env:LLAMA_CACHE` to hoot's own cache dir, and the live
`/v1/models` has five entries and no stray. The S21 note is corrected.

### Why the bench still has not run, with numbers

A warm sanity check against both new models, taken deliberately **while three
agents were building**, to see what a contended reading looks like:

```
gemma-3-12b   81 tokens / 215.8s  =  0.38 tok/s
gpt-oss-20b  120 tokens /  36.2s  =  3.3  tok/s   (benched: 12.21)
```

The cause was measured rather than guessed. With both models resident the box
read **free 9.9 GB, committed 78.6 GB against 61 GB physical** -- gpt-oss 12.3
GB working set plus gemma 10.4 GB, on top of three concurrent builds. That is
paging, not decode. gpt-oss reading 3.3 against its own benched 12.21 is the
control: the same model, the same binary, a quarter of the speed, purely from
contention.

**So these are not speeds and are not recorded as any.** They are the evidence
for why `bench_s1.sh` refuses to run below 24 GB free.

**Two operational findings fell out of it.** `POST /models/unload` with
`{"model": "..."}` works and returns 200 -- it removed gemma's 10.4 GB child
outright. And `llama-server` has a `--sleep-idle-seconds` flag the router is
not using, which is the standing fix for a box where two large models pin 22 GB
between them. **Not added to `hoot.ps1`**: whether "sleep" actually releases
resident memory, and what it costs the next request, are unverified, and an
unverified flag in the operator's launcher risks a router that naps mid-chat.
Filed as **S22**, to verify in the same quiet window as the bench.

### C3b - the pages rebuilt against the design that existed, 255 -> 275

See [Decisions](Decisions.md#2026-09-19---a-reconstruction-does-not-merely-omit-it-invents-reasons)
for the scale of the divergence, the invented owl rationale, and the twelve
places the design itself is wrong.

**Eleven of twelve diff rows went to the design.** The invented eight-state
machine is gone, replaced by §4.5's four service words verbatim. The `hoot`
variant -- a fourth palette these two pages are specified to use -- was added
to `tokens.css`, its values checked line by line against `webui/custom.css`
rather than invented. The two-band shell, the stat blocks, the nav, the
DIP-switch pair of real `<button>`s with `aria-pressed`, the tool table's
**approval** column (`read_only / mutating / destructive` -- the column that is
the page earning its existence over the CLI), and the readiness meter are all
in. Four routes became twelve, so a form's `action` says what it does.

**The security work was kept and the markup adapted to it**, which was the
instruction and is the right direction: CSP, the CSRF `Guard`, the `Host`
check, `Secret`, `redact`, never-optimism, the `/auth` query-string refusal,
Post/Redirect/Get and the dependency-allowlist test all survive.

Two design items were **omitted rather than faked** (`sign out`, which exists
in no route table) or **implemented-and-declared** (`restart`, which the design
says is not a route -- but §5.1.5 requires a no-JS path, and a page cannot
sequence two POSTs with scripting off).

**`host_ok`'s comment is fixed, and the claim verified from the RFC.** `Host`
carries the *target* authority (RFC 9110 §7.2), so a cross-site form post to
loopback sends `Host: 127.0.0.1:PORT` and sails through. The check closes DNS
rebinding and only that. The comment now says so and names the test that
demonstrates it.

**Counts: 255 -> 275** (239 lib + 17 `tests/pages.rs` + 17 shim + 2 wire), all
passing. `cargo check -p eidolon-web --all-targets` 0 errors 0 warnings;
workspace clean. `crates/web/src/shim/` untouched, including the
`Event::ModelChanged` arm at `render.rs:197`.

**Twenty-two protections broken in a dedicated `git worktree`** -- the shared
tree never held a mutation -- with a pre-flight asserting each anchor appears
exactly once and a **SHA-256 check** confirming every file came back
byte-identical. One run was invalidated by a `cp1252` decode error that left
`pages.css` mutated into the next pass; both `pages.css` cycles were re-run
from a verified-clean baseline and both caught their target.

**Two tests were wrong and the tests were fixed, not the code.** One asserted
a style rule for the `down` chip that both mockups deliberately omit -- a
service not running on an extension nobody enabled is a fact, not an alarm, and
§2 spends the red elsewhere. The other asserted `1 / 2` services up against a
fixture where two were.

Still 404 until `crates/cli` implements `OperatorHost` -- **C3c**, now an
8-method trait rather than the 5 the reconstruction had.

### D2e - the graph runner audited; five of six claims held

See [Decisions](Decisions.md#2026-09-19---amendment-the-persistence-refusal-was-right-about-half-a-program)
for the persistence split, the one claim that failed, and the break-test that
failed harder than predicted.

**The effect-path audit is clean again**, which is the finding that most needed
re-establishing: `guards.py` and `run.py` were rewritten wholesale and
`server.py` edited since the last audit, so the "nothing in `jev/automation/`
performs an effect" property could not be inherited. Re-grepped across every
non-test module plus all six `.rn` tools for `subprocess`, `os.system`, `eval`,
`exec`, dynamic `importlib`, sockets, `ctypes` and every write form. The single
new capability is `server.py`'s `_automation_start` wiring
`entail=lambda premise, hypotheses: _openjev()(premise, hypotheses)` -- a local
cross-encoder forward pass, lazily loaded and guarded by the pre-existing
`_openjev_lock`, not a network effect. All persistence still funnels through
the one shared `_paths.append_locked`.

**The lock now has a test, and the test is proven non-vacuous.** A
`_RecordingLock` (mirroring the pattern already in `test_server.py`) tracks
`acquire_count`/`max_concurrent` and sleeps while held to widen the window;
two tests fire concurrent `answer`/`stop` and `answer`/`answer` at one parked
run through a `ThreadPoolExecutor`. Removing the `with self._lock:` guards
reproduces both `ValueError: generator already executing` and
`acquire_count == 0`, across three repeated runs. That is the standard the
disclosure deserved.

**The calibration reproduced to all sixteen digits** --
`0.9109433889389038`, matching the pinned value bit-for-bit, via a standalone
script loading `server._openjev()` directly and feeding it through
`guards.evaluate_guard`. Observed on Windows/win32, CPU, float32.

**Counts: `jev/tests/` 182 -> 186** in both modes (179 + 7 skipped without
`JEV_TEST_OPENJEV=1`; 186/186 with it, none skipped). `jev/test_server.py`
never modified, identical at 33. Production decision log re-verified at
`c559328a1c24bfa60cdadf19fe80b93b`. Final `grep -r BREAKTEST` across `jev/` and
`extensions/jev/`: zero matches.

**One stale docstring found, not yet fixed:** `jev/automation/__init__.py`
still says escalation-with-parking is "D2b's job, not this package's" and that
"this interpreter stops the run and reports instead of parking." Both false
since parking landed. Folded into D2d.

Note on timings: the with-flag suite drifted from 59s to 211s over the session,
confirmed CPU-bound rather than hung. Three agents were building concurrently;
this is the same contention that has the model bench still waiting.

### D1r - the browser extension reviewed against a real website, 18 -> 23

See [Decisions](Decisions.md#2026-09-19---the-flagship-worked-example-could-never-have-worked)
for the ref-shape defect that made `browser_click` non-functional past the
first hop, the fixture measurements, and the `MAX_BODY` correction.

**Two live defects fixed in `service.py`:**

- `REF_RE` broadened from `r"\[ref=(e\d+)\]"` to `r"\[ref=([^\]]+)\]"`,
  confirmed in the tree at line 111. A permanent real-browser regression test
  (`test_a_ref_survives_a_second_navigation_not_just_the_first`, two local
  `data:` pages, real navigation, real click) now holds it.
- `SNAPSHOT_TIMEOUT_MS = 60_000` at line 92, plus caller-facing `timeout_s` and
  `depth` on `_snapshot` and in `tools/snapshot.rn`. **Explicitly labelled a
  mitigation, not a fix** -- the unbounded-AI-snapshot pathology and the
  single-`LOCK` blast radius both remain.

**A false coverage claim found and corrected.** `ValidationTests`'s docstring
asserted its HTTP-level checks "also prove" that ref rejection happens before
the browser launches. Break-tested by swapping `_click`'s `_resolve_ref` /
`_ensure_page` order: the whole suite stayed green. The claim was false. Two
new `ClickCostOrderingTests` now prove the property with `assert_not_called()`,
and re-breaking the ordering afterwards failed exactly those two and nothing
else.

**Four break-and-restore cycles**, each verified by a marker grep returning
zero and a full suite re-run. The auth one is worth naming: replacing
`hmac.compare_digest` with `==` failed **only** the mechanism-spy test, while
every outcome-level 401/200 test stayed green -- a demonstration that
outcome tests cannot catch a timing side channel, only a test that asserts the
mechanism can.

**Effect surface, driven through the real service on Windows 11:** `file://`
works with zero restriction; `javascript:` is blocked by Chromium itself
rather than by the service; both `:8080` and `:3000` are reachable by
`browser_open`; and an untrusted page's **own in-page `fetch()`** to those
loopback ports succeeds at the network layer (`type: "opaque"`) -- an SSRF
shape that needs no `browser_*` call aimed at them, only navigation to a
hostile page. Carried forward as evidence for the extension-trust decision,
deliberately not implemented here.

**Counts: 18 -> 23, all passing.** `%APPDATA%\eidolon\config.toml` untouched
and confirmed. Zero orphaned browser processes and zero leftover Playwright
temp directories at exit -- verified, after ~76 MB of accumulated artifacts
from earlier sessions were found and removed.

**Reported, not fixed, being outside its write scope:** `a11y.py` silently
drops 58 of ~11,000 lines on the real Cat page, when an accessible name
contains a colon or an embedded double quote and Playwright wraps the whole
line in single quotes -- `_ELEMENT_RE` anchors `^[A-Za-z]` and has no case for
a leading `'`. Not a crash; quiet data loss. Filed with S23.

### C2b - the fallback is live, cli 39 -> 46, tui 385 -> 389

See [Decisions](Decisions.md#2026-09-19---an-optimization-that-was-load-bearing-and-a-worktree-nobody-removed)
for the offline skip that turned out to be a precondition, the two wrong tests
caught before break-testing, and the usage undercount as filed.

**All seven wiring pieces**, verified in the main tree here: `--no-fallback` as
a global threaded through all six `build()` call sites (eleven references);
`build()` resolving through `fb.arm(&model, provider_override)` and installing
the hook with `agent.set_fallback(fb.clone())`, mirroring `set_persona_source`;
`:model` re-arm on **both** halves -- headless `repl()` and the TUI's
`command()` and `adopt()`; the headless printer in `term.rs`; `judge_provider`'s
offline skip; and `Cmd::Models`' readiness column (`ready` / `binary missing` /
`no key: {diagnostic}`) at `main.rs:1025`/`2231`.

The pre-existing journal-substitution path -- for a session resumed on a
machine whose catalog does not know the key -- is preserved unchanged and
composes with a dead chain's `Err`.

**The TUI treatment, which was the point.** `apply()`'s placeholder
`Event::ModelChanged { .. } => {}` is gone, replaced with a transcript note
and a status-line update, carrying the **reason** as well as the key -- which
the design did not ask for and which is the difference between "the model
changed" and "the model changed because the primary was rate-limited". And
`sync_swarm_model` now runs in the Bus-to-UI relay on **every** event, so a
peer's roster moves before the landing's first delta rather than after the
whole turn settles. It was extracted as its own function specifically so it
could be unit-tested without standing up a full `Agent` plus terminal -- the
smallest change that made that half real rather than constructional.

**Six break-and-restore cycles**, all in a worktree, all failing for named
reasons. Removing either rearm call produced the *unchanged input error* from
`fb.next(...)`, which is `ChainFallback`'s own `armed_key` mismatch guard
firing -- so the rearm tests prove the interaction, not just the call.

**Counts: `eidolon-cli` 39 -> 46, `eidolon-tui` 385 -> 389**, providers and
core untouched. `cargo check --workspace --all-targets` 0 errors, verified
independently in the main tree with its own target directory. Windows 11;
nothing platform-conditional was touched, and the one place it could matter --
`driver_readiness`'s `on_path`/`is_path` probe -- was exercised through a plain
file stat, the same code path on Linux.

**Still not visible in the browser:** `crates/web/src/wire.rs:354` returns
`None` for `Event::ModelChanged`, so a fallback that moves the model is
invisible to the web UI. Correctly labelled by its owning agent as unfinished
rather than quietly swallowed. Filed as **S25**.

The worktree it used was left behind at `bonsai2/eidolon-fallback-wt`; verified
superseded and removed here.

### D2d - parked runs survive a restart, 186 -> 200

See [Decisions](Decisions.md#2026-09-19---process-lifetime-escapes-per-test-isolation)
for the production leak it caused and closed, the `GeneratorExit` bug its own
restart simulation caught, and the limit it tested rather than hid.

**It verified the reviewer's split before building on it** rather than taking
it as given: `_park` really has exactly two call sites, both through
`_choose_phase`, itself called from one place -- a single fixed shallow
position -- while `_run_tool_action`'s `act` yield really does occur at
arbitrary depth with `into` living only in the frame.

**What is persisted:** on every park, a full snapshot -- `context`, `active`,
`history`, `visits`, `counts`, `warnings`, `path_log`, `input`, `ckpt_id`,
`graph_ref`, the escalation verbatim, and `elapsed_before_park` as a
**duration** rather than a raw `time.monotonic()` reading, which is meaningless
across processes. Written **before** the escalation reaches its caller, so
there is no window where a driver has the question but the snapshot is not
durable.

On resolution, a deliberately **smaller** marker -- no `context`, no `history`,
no escalation -- because nothing should ever be reconstructed from a resolved
line, only reported from. For an `act`-suspension, nothing at all: there is no
write hook in `_run_tool_action`, proven negatively by
`test_an_act_suspended_run_writes_no_snapshot_at_all`.

The only new write path is `_write_run_record` to `_paths.append_locked`, the
same helper `decisions.py` already uses.

**Two `orphaned` behaviours, each proven.** An escalate-parked run reconstructs
and resumes to completion through the *same* `_run_loop` as the live path,
including across two parks and two restarts with the file's last line winning.
An act-suspended run comes back already finished, `outcome="orphaned"`, with a
message naming the run and pointing at `automation.start`/`automation.stop`.

**Eight break rounds.** Round C -- removing the `_persist_park` call -- failed
**9 of 14**, which is the right blast radius for a foundation. Round E was
subtler than a crash: removing `_lookup`'s kwargs guard let reconstruction
silently succeed with no kwargs at all for inline-dict `graph_ref`s, which is
every fixture's shape, violating the documented opt-in contract without
erroring.

**Two more stale docstrings** beyond the one it was sent to fix:
`_no_such_run_message` and `list_runs` both quoted the old "runs do not survive
a service restart" heading. The second was word-wrapped across a line break,
which is why the first grep pass missed it -- worth remembering when sweeping
for a stale claim.

**Counts: `jev/tests/` 186 -> 200** in both modes (193 + 7 skipped without
`JEV_TEST_OPENJEV=1`; 200/200 with it). `jev/test_server.py` unchanged at 33.
Production log verified here at `c559328a1c24bfa60cdadf19fe80b93b` with no
`runs/` directory beside it. Windows 11; the new path logic composes on
`_paths.cache_dir()` and adds no platform-specific logic of its own.

### S23 - the runner refuses a partial page, 200 -> 222

See [Decisions](Decisions.md#2026-09-19---two-kinds-of-fixture-and-why-swapping-one-for-the-other-is-wrong)
for why the real capture was added beside the synthetic fixtures rather than
replacing them, and for the `/url:` arithmetic.

**Truncation is now refused, for every tool rather than for snapshots.**
`a11y.is_truncated()` matches `clip()`'s marker as a **pattern**
(`\n\[truncated at \d+ characters\]$`, not a hardcoded 262144, so it keeps
working if `MAX_BODY` ever moves without this file being touched), and
`run.py::_run_tool_action` checks the raw text **before** `build_obs` ever
runs, raising through the same `ERROR` pathway the stale-ref check already
uses. Applied to any tool result, because the marker is a property of the
transport rather than of snapshots -- so any future service-backed tool is
covered for free. The suffix was confirmed unique to that `clip()` by
cross-codebase grep; the other `clip()`s (`crates/core/src/project.rs`,
`agent.rs`) use a leading ellipsis instead, so it cannot misfire on page prose.

**Silently dropped lines: 58 -> 0** on the full 826,491-byte real capture.
Playwright wraps the whole `role "name" [attrs]` head in single quotes,
YAML-mapping-key style, when the name carries a colon or an embedded quote, and
escapes an interior `'` by doubling it -- confirmed empirically against real
`Ship''s Cat` text rather than assumed from the YAML spec. The unwrapped head
is handed to the **same** `_ELEMENT_RE`, so there is no second parser to drift.

**And what still cannot be parsed is no longer swallowed.** `parse_refs` always
returned a `skipped` count and every call site discarded it. `options.py`'s
`_refs_of` now turns a non-zero count into a warning on `build_options`'s
existing `warnings` channel, worded so the operator knows the menu may be
short. Both halves tested, including the non-vacuous negative.

**A real page is in the test tree**: `jev/tests/fixtures/wiki/`, five files --
the entire 826,491-byte capture kept verbatim and reachable, a 452-line
contiguous head, a citations excerpt stitched from three real slices with
provenance comments (chosen because that is where the quoted names actually
live), and two `f<N>e<M>`-shaped post-navigation captures.

**The capstone runs the shipped graph against the real page.**
`WikiHopRealCaptureTests` loads `extensions/jev/graphs/wiki-hop.json`
unmodified against the real capture: 63 real main-landmark link options, none
skipped, none capped against the graph's own `max: 64`, and the dispatched
click carries the real page's own ref `e374` rather than a small fixture's
`e1`.

**Counts: `jev/tests/` 200 -> 222** in both modes (215 + 7 skipped without
`JEV_TEST_OPENJEV=1`; 222/222 with it, 102.4s, CPU-bound). `test_server.py`
unchanged at 33. Three protections break-tested with md5-verified restoration
and a zero-match marker sweep. Production log re-verified here at
`c559328a1c24bfa60cdadf19fe80b93b` with no `runs/` beside it.

**One gap this opened, reported rather than fixed** (`extensions/browser/` was
read-only): `service.py`'s `_read()` signals truncation through its **own** JSON
field -- `{"text": ..., "truncated": bool}` -- not through Rust's `clip()`
marker. So `browser_read` can return a truncated body that `is_truncated()`
will not catch. Filed as **S26**. Also noted: `_head()`'s docstring claims it
is shared by `_snapshot` and `_read`, and grep says only `_snapshot` calls it.

### C3c - `eidolon serve` exists, and the operator pages render

See [Decisions](Decisions.md#2026-09-19---a-config-file-cannot-expose-you-only-a-command-line-can)
for the bind asymmetry and the stale-config bug that only a real socket found.

**The shim has a caller.** `Cmd::Serve { bind, cwd }` starts the Open WebUI
shim daemon on `127.0.0.1:8085` by default, reading a `[serve]` stanza
(`bind`, `cwd`, `token_file` defaulting to `<config_dir>/serve.token`,
`idle_minutes` defaulting to 60). One shared `yolo::Switch` is wired into both
`ServeOptions` and `ServeHost`, so `--yolo` is live-shared rather than a frozen
snapshot.

**`/ext` and `/auth` render.** `OperatorHost` is implemented in
`crates/cli/src/serve_host.rs` over the same bodies the CLI subcommands use --
`ext::view`, `ext::apply_toggle`, `ext::apply_start`, `ext::apply_stop`, and
`config::secret_store` -- so the page and the command line cannot drift into
disagreeing about what an extension is. `Cmd::Web` was wired the same way, so
the pages work under `eidolon web` too, not only under `serve`.

**Driven over a real socket**, against an isolated scratch config with a real
extension manifest on disk and `HOME` redirected so the unconditional
credential migration could not touch the operator's own `config.toml`:
`GET /ext` and `GET /auth` returned real HTML; `GET /api/ext` listed the test
extension; `POST /api/ext/disable` with a correct Origin and CSRF token
returned 303 and flipped the file. A POST without them was refused. `/v1/*`
without a bearer was refused, while the two page GETs correctly needed none.
Deleting the config file mid-process degrades to an empty list with 200 rather
than a 500.

`expires_at` and `account` come from `read_codex_creds_offline`, parsing the
credentials file structurally with no network and no subprocess -- the ruling
the page author asked for.

**Counts: `eidolon-cli` 46 -> 50.** `eidolon-tui` 389, `eidolon-providers` 128,
`eidolon-core` 214 all unchanged, and **`eidolon-web` unchanged at 275**, which
is the evidence that the agent live in that crate was not disturbed. Workspace
check 0 errors. Its worktree was verified byte-identical before porting and
**removed**, per the amended rule.

One file outside the granted scope, flagged rather than buried:
`crates/providers/src/catalog.rs` widened `resolved_token_file` from
`pub(crate)` to `pub`, with a doc comment saying why `/auth` needs it.

**Two gaps it declared rather than papered over:** `switch_model()` cannot
re-arm a `chain_fallback` the way the TUI's `:model` does, because no handle
reaches `Summoned` -- so a fallback inside a summoned session stays armed to
the original primary. And `StartFailed::log_tail` is always `None`, the tail
folded into `error`, because separating them needs a `crates/rune` change.

### MiniMax on the ollama account

`minimax-m3` and `minimax-m2.7` added to `crates/providers/builtin/ollama.rn`.
**Both ids verified live before the rows were written** -- a real
`/v1/messages` turn came back from each on the operator's own key, both
carrying `thinking` blocks, which is where `reasoning: true` comes from rather
than from a table. Prices read off `ollama.com/pricing` the same day: m3 at
$0.60/$2.40 with $0.12 cached, m2.7 at $0.30/$1.20 with $0.06 cached.

**No `context` on either row**, because the page publishes none and a guessed
window is how a turn dies at a boundary nobody can see -- the same reason
`qwen3.5:397b` and `gpt-oss:120b` carry none. No `vision` claim: untested.

The edit is **inert until the next `cargo build`**, deliberately: a live trial
is using `target/release/eidolon.exe` right now, and swapping the binary under
a running integration would invalidate it.

### D3 - the graph editor exists, `eidolon-web` 275 -> 308

See [Decisions](Decisions.md#2026-09-19---the-textarea-is-the-document) for the
single-representation ruling and the shared-signature near-miss, and
[the worktree note](Decisions.md#2026-09-19---a-worktree-is-named-for-the-work-not-for-the-agent)
for the incident **I** caused mid-task and what the recovery does not prove.

**`/jev` serves.** Three tabs -- `graph` (canvas + inspector), `json` (the
document itself), `runs`. A `choose` node is drawn as four bands --
observe / check / choose / act -- because that is the node's actual anatomy in
`automation.md`, rather than a box with a label on it. OPEN and DOWNLOAD work
against the browser's own file access; canvas and inspector are fully live.

**Svelte Flow is vendored, not CDN-loaded.** `@xyflow/svelte` built locally
through Vite as a library and embedded with `include_str!`: **335,430 B** of JS
and **15,678 B** of CSS under `crates/web/vendor/jev-flow/`. Two build settings
are load-bearing and commented as such -- `cssCodeSplit: false`, or the CSS
never reaches the binary, and a `define` for `process.env.NODE_ENV`, or the
bundle references a `process` that does not exist in a browser. 13 runtime
packages, all MIT / ISC / BSD-3, licenses vendored beside the bundle;
`node_modules` deleted after the build. This is what `script-src 'self'` with
no CDN actually costs, paid once.

**The CSP was verified in a real browser with non-vacuous controls**: an
inline-style write through `setAttribute` confirmed **blocked**, a dynamically
injected `<style>` element confirmed **blocked**, and a CSSOM write confirmed
**working** -- so the policy is demonstrably on rather than assumed, and the
page's styling route is the one that survives it.

**Four defects that only the real graphs found**, including `graph.py` refusing
a `meta.editor` key on a history state. Fixtures would not have produced any of
them; the two shipped graphs did.

**Counts: `eidolon-web` 275 -> 308**, 0 warnings, 0 errors, workspace green.
**23 break-test cycles, 23/23 caught**, each restored byte-identically, with
two rounds invalidated and redone rather than reported as passes.

**Half-wired, and it says so.** `Ctx.jev` has no implementation yet, so SAVE
and LINT render **disabled with the reason shown**, and the runs tab has
nothing to read. Never-optimism again: the page does not pretend to a
capability the backend does not have. **C3d** is dispatched to close it --
`JevHost`'s four methods in `crates/cli`, then `main.rs:580` and `main.rs:644`
onto `run_with_hosts`/`run_serve_with_hosts`.

### J1 - the first live end-to-end jev trial

See [Decisions](Decisions.md#2026-09-19---the-fourth-time-and-this-one-refutes-a-fix-recorded-as-landed)
for the layered truncation and what it does to S23,
[the observation ruling](Decisions.md#2026-09-19---an-observation-that-is-not-what-the-node-believes-it-is),
and [park and resume](Decisions.md#2026-09-19---park-and-resume-demonstrated-live).

**Both graphs ran through the real chain** -- a live `eidolon run --yolo -m
ollama:deepseek-v4.1-flash` session calling the actual `jev_run` tool, never
the direct-service fallback. Windows 11 throughout; no WSL on this box.

**wiki-hop: `exhausted`.** 31 states, 61 browser actions, 119.8s, 0 chooser
calls, no decision log. Root-caused to `crates/core`'s `spill()` cutting the
snapshot to 2,000 characters before `jev` ever sees it, and cross-checked three
independent ways -- reading the source, re-running the live Playwright call,
and probing the live service with two throwaway inline graphs. All three agree.
**Reported, not fixed**: the ruling is A5's.

Timing, from the session journal's dispatch timestamps, summing to 119.801s
against `run.py`'s self-reported 119.8: `browser_open` 2.1s; the one real
snapshot **30.4s** for the 814 KB tree -- well inside the new 60s timeout, so
**the 170s+ hang from this morning did not recur**; `browser_back` ~10-30ms
each as no-ops; `about:blank` snapshots 2.3-4.3s each, a surprisingly high
fixed cost; a warm re-snapshot of the same article **12.9s**, less than half
the cold one. **100% browser time, 0% model time.**

**triage-linux: `reached`.** 7 steps, 7 actions, 2 escalations, 7 chooser
calls, 60.27s excluding parked time. Both escalations parked, persisted and
resumed correctly. Decision-row timestamps show ~20-23s for an escalated step
against ~3-5s for an auto-cleared one -- the cost of asking a model, measured.

Its *content* is not validly observed: there is no Linux here, so the graph
learned Cygwin/MSYS noise. What **is** validated, by source inspection with no
OS branch in the paths touched: hierarchical compound states, the shallow-
history target shape, the `lines`/`menu`/`transitions` chooser sources, the
park/resume contract, and the decision-log shape.

**Isolation, verified before the run rather than after.** The already-running
jev service was stopped first -- the extension host adopts a healthy service by
health check and would have silently kept writing to whatever paths *that*
process started with, and a stale record was in fact sitting there (port 51808,
pid mismatch). Then `JEV_DECISIONS_DIR`, `JEV_RUNS_DIR` and `JEV_DECISIONS_LOG`
were exported, the service restarted under them, **and the redirection proven
with a throwaway graph forced to park and resolve** before the real trial
began. Production `decisions.jsonl` verified `c559328a1c24bfa60cdadf19fe80b93b`
before, during and after; no `runs/` or `decisions/` ever appeared beside it.

**Left clean, and more than its own:** both services stopped, no attributable
python or chrome process left, and 23 orphaned Playwright profile directories
removed -- 181 MB, of which 13 were not its own. The jev service was left
**stopped on purpose**, because a running scratch-pointed instance would have
been adopted by the next real session, which would then have inherited scratch
paths instead of production.

### A5 - `docs/design/observation.md`, and wiki-hop has a route back

See [Decisions](Decisions.md#2026-09-19---a-tool-result-is-bounded-by-who-reads-it-not-by-how-big-it-is)
for the four rulings. 892 lines, verified present and structured; nothing else
created or edited, which matters because three agents were live in the tree.

**Five steps in landing order**, each naming the test that fails if it is
reverted, and each checked against current source rather than against the
summary that briefed it:

1. `crates/core` -- `dispatch.rs` + `tests/spill.rs`.
2. `crates/tools` -- `service.rs`, `whole()`, at lines 166/169/172/179/181/264.
3. `extensions/browser` -- `_head` at 200, `_snapshot`, `snapshot.rn` using an
   object-mutation idiom with in-tree precedent, five real-Chromium tests, and
   a `cat_main_scoped.txt` capture.
4. `jev/` -- the a11y head grammar and `declared_length`; `run.py`'s
   three-belief check with its exact call shape against `rt.scope()`;
   `schema.json` 280-283; `graph.py` 596/614/504; both graphs; and
   `automation.md`'s anchors at 108/127/145/452/569/753/832.
5. `crates/rune` -- S29, plus `RUNE.md` and `extensions.md`.

**Step 1 is dispatched as E1**, with steps 2 folded in: they are one change in
two crates, and nothing downstream can be built until a whole result can cross
a boundary.

**What it does to the numbers D8 cares about:** dispatches *per decision* do
not change, so the gate-question rate is untouched. Total dispatches **fall**,
because the 30 `browser_back` no-ops that made up most of wiki-hop's 61 actions
stop happening.

**Platform:** `spill()`, `clip()`, `aria_snapshot` and `get_by_role` were each
confirmed to have no OS branch. The only Linux-varying content in the whole
spec is the `ps` header pattern in the triage graph's new `expect`, and it is
flagged as reasoned-not-observed rather than stated as fact.

### C3d - the editor saves, `eidolon-cli` 50 -> 59

See [Decisions](Decisions.md#2026-09-19---degrade-dynamically-not-by-hardcoding-what-is-missing)
for the `lint()` ruling and the gaps it declared, and
[the verification note](Decisions.md#2026-09-19---when-a-verification-script-is-the-thing-that-is-broken).

**`JevHost` is implemented and wired.** Four async methods, exactly as the page
declared -- no repeat of `OperatorHost`'s 8-vs-5 surprise, and it re-read the
trait rather than trusting the brief either way. `main.rs` changed at **exactly
the two call sites**, 580 (`Cmd::Web`) and 644 (`Cmd::Serve`), onto
`run_with_hosts`/`run_serve_with_hosts`.

**Where the graphs really live was checked, not assumed**:
`<extensions_dir>/jev/graphs/`, found through `crate::ext::find` -- the same
lookup `/ext` uses -- and verified against `jev/server.py`'s actual
`GRAPHS_DIR` default **and** the two real files on disk, rather than against
`automation.md`'s prose. `graphs()` never parses anything but `id`, keeping the
page's own rule; a non-JSON or id-less file becomes a row with `trouble` set
rather than failing the whole listing; a missing directory is an empty store,
not an error.

**`write_graph()` uses `harnox::fs::write_atomic_0600`** -- the helper
`ext.rs`'s `set_dir`/`set_enabled` already use -- and **reads back what it
wrote** rather than echoing its argument.

**The SAVE round trip was proved on disk, not asserted.** Against a scratch
config on `127.0.0.1:18787` with copies of the real extension manifest and both
real graphs: an overwrite of `wiki-hop.json` (`floor 0.35 -> 0.42`) replied
`{"identical":true,...}`, the file held exactly the posted bytes, no `.tmp` was
left, and a **fresh** `GET /api/jev` returned the new content -- the two-calls-
with-a-change-between rule that caught C3c's stale cache. A new document
created cleanly. `id=../escape` was refused 409 with nothing written; a
mismatched row/document id refused 409.

**The acceptance test that matters passed**: `jev/automation/graph.py::
load_graph` loaded both written files unchanged through the real venv. What the
editor writes, the runner reads.

**It could not open a GUI browser and said so** rather than skipping the proof
or claiming one. It fetched the real page, scraped the live CSRF `form_token`,
and POSTed the exact form-encoded body the page's own `<form>` sends --
verified against `pages/jev.rs` first. It declined to spawn a browser because
it could not guarantee killing only its own tab without risking the operator's
session. That is the right trade and the right disclosure.

**Counts: `eidolon-cli` 50 -> 59.** `eidolon-web` **308**, `eidolon-tui` 389,
`eidolon-providers` 128, `eidolon-core` 214 all unchanged. Workspace check 0
errors. `decisions.jsonl` md5 `c559328a1c24bfa60cdadf19fe80b93b` and
`config.toml` md5 `387c6feab848003597a1717b9aeb1a20` both identical to
baseline, no `runs/` beside either.

**Worktree discipline held.** It created one named for its own task, and when a
**second, unrecognised** worktree appeared mid-run -- `eidolon-spill-clip-
boundary-wt`, E1's -- it left it completely untouched and reported it. That is
the rule from this morning's incident working the first time it was tested.

Two platform notes it volunteered: `harnox::fs::write_atomic_0600`'s `0600`
mode bits are a **Unix-only branch** and were not exercised here, only the
platform-independent temp-file-plus-rename; and its own code has no `cfg`
branches, so it has no reason to expect Linux to differ but did not run there.

### C5 - Open WebUI reaches eidolon, and the sidebar says so

See [Decisions](Decisions.md#2026-09-19---the-launcher-owns-the-registration-the-apps-database-only-caches-it)
for why the environment alone could never have registered the shim,
[the link ruling](Decisions.md#2026-09-19---a-link-is-a-claim-and-an-unprobed-link-is-an-unverified-one)
for the `/jev` gate, and
[the launcher ruling](Decisions.md#2026-09-19---a-launcher-may-only-stop-what-it-can-prove-it-started)
for the process-matching near-miss.

**A chat in the app is an eidolon session, proven by running one.** In a real
Chromium against `127.0.0.1:3000`: picked `hoot:gpt-oss-20b` ("GPT-OSS 20B
(local, ~12 tok/s)") out of the picker, sent *"In one short sentence: what is
the capital of Japan?"*, got **"Tokyo is the capital of Japan."** back with a
"Thought for 13 seconds" collapsible -- `reasoning_content` rendering as the
design said it would. 101 s wall, model load included. The journal is the
proof the whole header contract arrived:

```
#0 start model=hoot:gpt-oss-20b cwd=\?\C:\Users\dxcen\Projects\bonsai2
#1 note: owui-chat 31c505b1-94d4-48dd-9d56-43ef6b65b9db
#2 note: owui-turn user=2a1ecb98-... assistant=3bd1d1b2-...
#3 user: In one short sentence: what is the capital of Japan?
#4 assistant: Tokyo is the capital of Japan.
#7 settled EndTurn in=1738 out=51 cached=1719
```

`owui-chat` is `X-Eidolon-Chat-Id`; the two ids in `owui-turn` are
`{{USER_MESSAGE_ID}}` and `{{MESSAGE_ID}}`, and both match the `message-<id>`
elements read off the page. Amendment 3's per-connection header is live and the
box-wide toggle is not load-bearing.

**`eidolon serve` is the end state**, started by `hoot up`: pid 31900 on
`127.0.0.1:8085`, `--cwd C:\Users\dxcen\Projects\bonsai2`, stdout and stderr to
`logs\shim.{out,err}.log`. `hoot shim` runs the same thing in the foreground,
`hoot stop` stops it last (after the app, so a turn in flight ends as a stream
the app stops reading rather than a red error on a bubble), and `hoot status`
lists it and the fourteen models it advertises beside the router's five.

**The token lives in exactly two places** and neither is a command line: the
file `eidolon serve` writes (`%APPDATA%\eidolon\serve.token`, 68 bytes,
`eid-` + 32 random bytes as hex) and, from there, Open WebUI's own
`webui.db` row `openai.api_keys`. `hoot.ps1` reads the file into the child's
environment and never prints it; the shim's own startup line names the *path*.

**Three sidebar rows, cloned rather than built.** `loader.js` (via its
generator, `webui/build-loader.py`) clones the app's Workspace row, falling
back to Notes, and re-points the clone -- so the same code produces the
labelled row when the sidebar is open and the icon-only button when it is
collapsed to the rail, with the app's own classes and hover states already on
it. `custom.css` paints the glyph off the absolute href with a `::before`,
which is a **stated deviation** from `pages.md` section 7.1: that block sizes
the icon well itself to 1.05rem, and the well is a 30px hover target in the
collapsed rail. Icons: the jigsaw already on `#integration-menu-button`,
`lock-filled`, `link-symbol`.

**Measured, every one, in a real browser:** `/ext` **200** (title
"extensions - eidolon"), `/auth` **200** ("credentials - eidolon"), `/jev`
**404**, zero-byte body -- which is why the `/jev` row is gated on a live probe
and is absent today. Forced on, it renders correctly and lands on Chrome's 404
page; that was measured and then restored byte-identically (md5
`537FA676...` before and after).

**Degrade-to-absence, proven non-vacuously.** The same served `loader.js` on a
bare page with no `/notes` or `/workspace` anchor: zero rows, zero console
errors. The same file on a bare page with one fake `/notes` anchor: two rows
with the right hrefs and labels. So the zero is the guard, not a script that
failed to load.

**Open WebUI restarted clean.** Router untouched (pid 8168 throughout -- a
restart would have re-paged two large models for no reason). Four
conversations before, four after, all in the sidebar by title, plus the new
one; `/health` 200; no error banner; no console errors on any page load.

**Gaps found and filed, not fixed:** every extension service fails to start
under `hoot` because `bash.exe` is not on the PATH PowerShell hands a child
(`C:\Program Files\Git\cmd` is on PATH, `...\Git\bin` is not) -- S17 in
production, twelve tools missing from every chat, **verified** by prepending
`Git\bin` and watching `browser` come up on :53106, then stopping it. The
`eidolon - ` name prefix `shim.md` section 2 specifies is not in the shipped
`/v1/models`. The `eidolon` umbrella reads *"(default: mock)"* because
`default_model` is unset. And the `WEBUI_BANNERS` art renders its newlines as
literal `<br>` on the operator's screen.

### C5 - a chat in Open WebUI reaches eidolon

See [Decisions](Decisions.md#2026-09-19---a-launcher-that-writes-only-defaults-changes-nothing-silently)
for the PersistentConfig trap and the two method notes beside it.

**The round trip, in a real browser.** Chromium to `127.0.0.1:3000`, the model
picker, **`hoot:gpt-oss-20b`**, *"In one short sentence: what is the capital of
Japan?"* -> **"Tokyo is the capital of Japan."** 101s wall, with a "Thought for
13 seconds" collapsible. `eidolon log` on the session it created:

```
#0 start model=hoot:gpt-oss-20b cwd=\\?\C:\Users\dxcen\Projects\bonsai2
#1 note: owui-chat 31c505b1-...
#2 note: owui-turn user=2a1ecb98-... assistant=3bd1d1b2-...
#4 assistant: Tokyo is the capital of Japan.
#7 settled EndTurn in=1738 out=51 cached=1719
```

The two ids in `owui-turn` **match the `message-<id>` elements read off the
page**, so Amendment 3's per-connection header contract is live end to end,
not merely the chat id.

**Where the bearer lives, stated explicitly**: `eidolon serve` writes
`%APPDATA%\eidolon\serve.token` (68 bytes, `eid-` plus 32 random bytes as
hex); `hoot.ps1` reads it into the Open WebUI child's **environment**; it comes
to rest in Open WebUI's own database under `openai.api_keys`. Never in argv,
never printed, and **not** in `logs\shim.out.log` -- the startup line names the
path, not the token.

**`hoot.ps1` gained a shim.** `-ShimPort` (8085), `Get-ShimTokenPath`,
`Test-IsHootShim`/`Get-ShimPid`, a `hoot shim` verb, `up` starting it **before**
Open WebUI and `stop` stopping it **last**, `status` listing it and its 14
models, and `webui` warning when it is down. `Set-WebuiEnv` now registers two
OpenAI connections rather than one.

**The `/jev` link is gated, not shipped broken.** `/ext` **200**, `/auth`
**200**, `/jev` **404 with a zero-byte body** -- the page is in source and not
in the running binary. So `hoot.ps1` fetches `/jev` at the moment it copies
`loader.js` and flips one flag **only on a 200**. It defaults off and
self-enables after the rebuild with nobody remembering to do it. Forced on, the
row renders correctly and lands on a browser 404; the served files were then
restored byte-identically (md5 `537FA676...` before and after).

**Degrade-to-absence was proven non-vacuously**, which is the part that usually
is not: the same served file against a bare page with **no** anchor produced 0
rows and 0 errors, and against a bare page with **one fake `/notes` anchor**
produced 2 correct rows. A no-op that has never been shown to do something is
not a proven no-op.

**Open WebUI came back clean.** All four pre-existing conversations present by
title plus the new one, `/health` 200, no error banner, zero console errors on
every page load, `hoot up` idempotent three times. **The router was never
touched** (pid 8168 throughout) -- restarting it would have re-paged two large
models for nothing.

One stated deviation from `pages.md` section 7.1, which sizes the icon well to
1.05rem: 0.11.3's well is a 30px hover target in the collapsed rail, so the
glyph goes on a `::before` -- the idiom the other nineteen swaps in
`custom.css` already use.

**Left running deliberately:** `eidolon serve` on `127.0.0.1:8085`,
`--cwd <repo root>`, logging to `logs\shim.{out,err}.log`. Stopped again: its
own probe shim, the old Open WebUI, and a `browser` service started only to
verify a diagnosis. Zero Playwright Chromium processes remain.

**Five findings filed, renumbered S34-S38** after a collision (C5's report
calls them S29-S33). The one that matters: **S34 -- no extension service can
start under `hoot`**, so twelve tools are missing from every chat. `Git\cmd` is
on PATH and `Git\bin`, which holds `bash.exe`, is not; verified by prepending
it and watching `browser` come up on :53106. It deliberately did **not** patch
PATH in the launcher, because that would hide a real `crates/tools` defect and
deepen the Windows-only hole. Dispatched.

### E1 - spill bounds by consumer, `clip` refuses. `eidolon-core` 214 -> 218

See [Decisions](Decisions.md#2026-09-19---a-safety-argument-becomes-a-test-or-it-is-not-a-safety-argument).
Steps 1 and 2 of [`design/observation.md`](design/observation.md); steps 3-5
untouched.

**`crates/core/src/dispatch.rs`** -- `spill()` (now 243) borrows rather than
moves, its guard is only the size check, its receipt wording branches on
`CallOrigin::Script`, and it sets `is_error: output.is_error`.
`dispatch_with()` (304) computes `to_script`, **always** spills into `recorded`
for the journal and the bus, then returns `if to_script { output } else {
recorded }` at 345-353 -- the whole thing to a script, the bounded form to
everyone who reads.

**`crates/tools/src/service.rs`** -- new `whole(s, method)` at 280 refuses over
`MAX_BODY` naming the count and the cap, and never shortens. `render()` (266)
takes the method and returns a `Result`. `call()`'s two `clip(&text, MAX_BODY)`
sites became `whole(&text, method)` at 170 and 182. **The two `clip(&text,
1000)` diagnostics inside `Err` are untouched**, as ruled -- a diagnostic is
not an observation. `clip` keeps its body and gains a comment that its marker
is now unreachable from any `Ok`.

Neither change widened a shared signature: `spill` was confirmed by grep to
have exactly one caller, and `render`/`whole`/`clip` are all private.

**M6, measured rather than inherited: 32,000 characters** -- four times
`MAX_INLINE_CHARS`, asserted at runtime and visible in a break-test diff rather
than computed on paper. At the other boundary, `an_answer_at_max_body_arrives_
whole` proves **262,144** characters arrive whole through `service::call`, and
262,145 is refused.

**Six break-tests, six right failures**, including two that are easy to get
wrong: re-adding the `is_error ||` exemption, and an off-by-one (`>=` for `>`)
in `whole()` that refused an answer of exactly `MAX_BODY`.

**Counts: `eidolon-core` 214 -> 218, `eidolon-tools` 66 -> 68.** `eidolon-tui`
389, `eidolon-providers` 128 (2 ignored), `eidolon-web` 308 all unchanged,
confirmed by a full `cargo test --workspace` -- every binary ok, 0 failed.
Workspace check 0 errors.

**Spec accuracy:** every line citation for these two files was exact --
`dispatch.rs` 232-235 and all six of `service.rs`'s -- against a tree three
agents were editing. One cosmetic drift: the module-doc section cited as 54-62
is at 51-60, and the spec marks that item "no test."

**Platform:** both touched functions grepped clean of `cfg(windows)`,
`cfg(unix)` and `cfg(target_os)`. `spill()` uses `PathBuf`, `chars().count()`
and `fs::write`; `whole`/`clip`/`render` are pure string and JSON handling.
Observed on Windows 11 only.

**Worktree accounting:** created `eidolon-spill-clip-boundary-wt`, mirrored the
live tree so it could verify against real current source including other
agents' in-flight work, copied back only its three files checksum-identical,
and removed its own. It observed `eidolon-jevhost-breaktest-wt` disappear (C3d
cleaning up after itself) and `eidolon-s34-shell-wt` appear, and **touched
neither**.

### E2 - the snapshot narrows at the source, 23 -> 31

See [Decisions](Decisions.md#2026-09-19---a-measurement-can-refute-the-justification-and-leave-the-ruling-standing)
for M1 refuting the ruling's expectation, the 37x timing correction, and
[the break-test note](Decisions.md#2026-09-19---a-break-test-that-does-not-fail-can-be-the-right-answer).

**`browser_snapshot` takes `within`.** `_snapshot` (222-266) validates it as a
non-empty string before touching Playwright further, builds
`page.get_by_role(within)`, and checks `await target.count()`: **any count but
exactly one is refused by name**, carrying the count and the URL --
`within='main': 0 elements with that role on about:blank; a scope names exactly
one` -- with `-- use an unscoped snapshot, or depth` appended only when there
is more than one. On a single match it snapshots **the locator, not the page**.
`_head` stamps `scope:` and `chars:`. No `within` leaves the old path exactly
as it was.

`chars: N` is the forcing function from ruling 3: the producer stamps the count
and the consumer compares. E3's `declared_length` check now has something real
to read.

**Five real-Chromium tests plus three mocked argument tests**, each named with
what it catches: scoping not applied; scoping applied to the wrong element; a
scoped ref not resolving through `_click`; the zero-match and many-match cases
proceeding silently; and `chars:` regressing or a spurious `scope:` leaking
onto unscoped output.

**The false `_head` docstring is fixed.** It claimed to be shared by `_snapshot`
and `_read`; grep finds exactly one call site. A claim of coverage in a
docstring is a testable assertion, and this one was false.

**Counts: `extensions/browser` 23 -> 31.** A real capture was added at
`jev/tests/fixtures/wiki/cat_main_scoped.txt`, 762,853 chars, round-trip
verified byte-identical to what `_snapshot` returned; the pre-existing fixtures
beside it were checksummed before and after and are untouched.

**Cleanup done the way this morning's lesson demands** -- by diffing a full
`%TEMP%` listing before and after, **not** the process table, which is exactly
how 23 orphaned profile directories survived an earlier "zero orphans" report.
Zero net new entries across roughly ten real-browser runs and two live
measurement scripts. It also caught **its own** stray: a diagnostic redirected
to `/tmp`, which this box maps to `%TEMP%` root, spotted on the very next diff
and removed.

**One gap declared rather than hidden:** `snapshot.rn`'s forwarding of `within`
into `service_call` has no automated regression test beyond the compile check
-- and neither does `depth`, `timeout_s`, `max` or `submit` in its sibling
files. It declined to invent test machinery for one argument alone, and
declined to start the shared browser service to probe it end to end because
other agents were live. Both are the right calls, and naming them is what makes
them decisions rather than omissions.

**Every line the spec cited for this step was exact.** The only divergence from
the document was M1's expectation -- reality, not source.

**Worktrees:** it created none, and it observed `eidolon-spill-clip-boundary-wt`
disappear and `eidolon-s34-shell-wt` appear. It flagged both rather than
assuming, which is precisely the instruction, and its reading was right: E1
finished and S34 started.

### S34 - the tools come back, `eidolon-tools` 67 -> 79

See [Decisions](Decisions.md#2026-09-19---the-defect-was-not-it-needs-a-shell)
for the ruling, [the platform-as-parameter note](Decisions.md#2026-09-19---thread-the-platform-as-a-parameter-not-as-a-cfg),
and [the stale-artifact note](Decisions.md#2026-09-19---a-built-artifact-is-a-cached-copy-of-source-and-it-goes-stale-like-one).

**Both spawn sites in `crates/tools/src/shell.rs`** -- `run()` at 89 and
`run_background_with_env()` at 271 -- now resolve through `bash_path()` (599),
a per-process `OnceLock` over `find_bash()` (632). Every input is injected:
the `EIDOLON_BASH` override, `PATH`, `PATHEXT`, the fallback root lists, and
`windows: bool`. Helpers: `search_dirs`, `with_pathext`, `beside_git`,
`windows_fallback_paths`, `posix_fallback_paths`, `bash_not_found_message`.

**The acceptance test ran in the operator's actual broken PATH** -- `Git\cmd`
present, `Git\bin` and `Git\usr\bin` absent -- rather than in the Bash tool's
PATH, which Git Bash itself pollutes at shell-init and which would have made
the test unfaithful. It checked, noticed, and rejected that environment.

```
> eidolon.exe ext start jev
jev: service started on :53279 in 1.1s
> eidolon.exe ext start browser
browser: service started on :57950 in 1.5s
```

Both ports confirmed listening; `ext list` shows both `service up` with real
pids. Then the part that closes the loop: `eidolon run --provider mock` in the
same broken PATH showed **both services registered rather than twelve
absences**, and exercised both the foreground shell tool and the background
service spawn in one run. `ServeHost::summon()` was confirmed at
`crates/cli/src/serve_host.rs:538` to call the identical shared bootstrap, so
what works for `run` works for a chat.

**Three break-tests at three levels** -- unit (removing the beside-`git` walk
fails the exact S34 scenario test, proving it non-vacuous), integration
(restoring the bare `"bash"` literal reproduces the original end-to-end
failure), and the accidental artifact-level pair above.

**Counts: `eidolon-tools` 67 -> 79** (+12, all in a new `mod bash_resolution`,
all executing on this Windows box *and* on any other platform by construction).
`eidolon-cli` 59, `eidolon-rune` 59 unchanged. Workspace check **0 errors**.

It flagged, rather than claimed, that it did not separately re-run
`eidolon-web`/`tui`/`providers`/`core` -- their source was untouched and the
two public signatures are unchanged, so no regression path exists, but that
claim is reasoned from the workspace check rather than individually observed.

**Protected files re-verified:** `decisions.jsonl` md5
`c559328a1c24bfa60cdadf19fe80b93b`, no `runs/` beside it; `config.toml` and
`policy.rn` mtimes predate this session's work. The release process on `:8085`
was left running and untouched, and only `cargo build -p eidolon-cli` was used.

**Worktrees:** created and removed its own; left `eidolon-s25-s35-s36-web-wt`
(C7's) alone, noting only that it had replaced the one it saw at start; and
left the untracked `eidolon-jev-wt` directory untouched -- the remnant of this
morning's destroyed-worktree incident, which is mine to clean up.

**Left running deliberately**: the `jev` and `browser` services, on :53279 and
:57950. They are the point of the fix, not test debris.

### C7 - the picker and the stream say who is answering, 308 -> 316

See [Decisions](Decisions.md#2026-09-19---a-display-value-standing-in-for-a-decision-nobody-made).
S25, S35 and S36 closed together, all in `crates/web`.

- **`src/wire.rs:360`** -- `Event::ModelChanged` projects to
  `Wire::ModelChanged { model, reason }` instead of `return None`.
- **`src/shim/render.rs:200-202`** -- renders a **permanent transcript line**,
  `\n> **model changed:** {key} - {reason}\n`, instead of `vec![]`.
- **`src/shim/models.rs`** -- `BRAND` (80) and an **idempotent** `branded()`
  (82) over every catalog row; `umbrella_default()` (112) distinguishing
  `Configured(Resolved)` from `Unconfigured`.
- **`src/shim/mod.rs`** and **`src/shim/task.rs`** -- the Umbrella arms return
  `NoDefaultModel` (404) rather than silently birthing a chat, or running a
  task, on `mock`.
- **`assets/js/app.js`** -- the native page updates its own `#model` header and
  posts a notice, so S25 lands on both of this crate's consumer surfaces rather
  than only the shim.

**Proved over a real socket with a genuine fallback hop**, not a simulated one:
a scratch `deadend` provider pointed at `127.0.0.1:1` with nothing listening,
the real router at `:8080` as the landing. The connection was refused four
times, the chain hopped, and the SSE stream carried

```
> **model changed:** hoot:gpt-oss-20b - transport error (...)
```

followed by the real completion. The same hop was then driven through
`GET /api/events` on the native page, where the event appeared **after the last
retry notice and before the landing's first delta** -- the position
`design/fallback.md` section 4 requires.

The served model list, over HTTP:

```
{"id":"eidolon","name":"eidolon (no default model configured)"}
{"id":"hoot:gpt-oss-20b","name":"eidolon - GPT-OSS 20B (local, ~12 tok/s)"}
{"id":"hoot:gemma-3-12b","name":"eidolon - Gemma 3 12B (local, vision)"}
```

That `hoot:gpt-oss-20b` row is the exact unbranded string from the S35 report,
now branded, served live.

**Counts: `eidolon-web` 308 -> 316.** `eidolon-cli` 59, `eidolon-tui` 389,
`eidolon-providers` 128 (2 ignored) unchanged in the same full workspace run.
Check 0 errors, no new warnings. Its scratch `serve` and `web` processes and
its streaming `curl` were each identified by exact command line and stopped
individually; **the operator's shim on `:8085` was never touched.**

**Four corrections to documents, reported rather than worked around:**
`design/shim.md` names `crates/cli/src/main.rs` as the `SessionFactory`
implementor when it is `serve_host.rs`'s `ServeHost`; the design's
`resolve_backend` is shipped as `resolve`; `eidolon/AGENTS.md`'s source map
omits `crates/web` entirely; and AGENTS.md's stated ~140-column convention
contradicts the ~100 I had been putting in briefs -- **my error**, recorded in
Decisions.

### E3 - jev refuses what it cannot trust, `jev/tests` 222 -> 235

See [Decisions](Decisions.md#2026-09-19---why-no-depth-saves-wiki-hop-and-the-constraint-i-invented)
for the depth sweep and [the break-test note](Decisions.md#2026-09-19---a-break-test-that-reproduces-the-original-incident).

**The three-belief check is in `run.py::_run_tool_action`**, and nothing
reaches `rt.context` until all three pass:

- **succeeded** -- an `error` in the result raises `_ToolError(reason="failed")`.
- **whole** -- `a11y.declared_length(text)` against `len(body)`; a mismatch
  raises `reason="truncated"`. This reads the `chars: N` stamp step 3 added,
  so the forcing function from ruling 3 is now closed end to end: the producer
  stamps the count, the consumer compares it.
- **of-kind** -- the new optional `expect` key, evaluated once through
  **`guards.evaluate_guard` verbatim**: same closed grammar
  (`equals`/`contains`/`matches`/`exists`/`count`/`entails`/`contradicts`/
  `not`/`and`/`or`), same NLI contract. A new call site and a `schema.json`
  allowance, **not a second expression language**.

**Seven break-tests**, each restored and confirmed by a `grep` for the marker
returning zero matches anywhere in `jev/`, `extensions/jev/` or
`automation.md`, then a full green run.

**Three call sites the spec's literal code did not cover**, fixed for
consistency rather than left ragged: `_resume_parked_choice`'s orphan-mismatch
raise (`reason="stale", tool="pick"` -- reusing the closest existing category
rather than inventing a fifth), `_with_outcome_handling`'s double-fault
`except _ToolError`, and `_status_dict`'s `error` branch, which otherwise drops
`reason` when a finished run is polled after the fact.

**One spec claim measured false and corrected in place:** the specified test
asserted every link in `cat_main_scoped.txt` has `landmark == "main"`. Only 97
of 2,741 do. The test was renamed and asserts the true property, with the
numbers in its docstring.

**Counts: `jev/tests` 222 -> 235** (7 skipped both before and after);
`test_a11y` 30 -> 33, `test_run` 51 -> 59, `test_schema` 24 -> 26.
`jev/test_server.py` 33 and `extensions/browser` 31 unchanged.

**Isolation held.** `JEV_DECISIONS_DIR`/`JEV_RUNS_DIR`/`JEV_DECISIONS_LOG` into
the scratchpad, **proven with a throwaway park-and-resolve graph before being
relied on**. Production `decisions.jsonl` md5
`c559328a1c24bfa60cdadf19fe80b93b` before, during and after, no `runs/` beside
it at any point.

**Flagged, not edited:** `design/automation.md` around 924-925 attributes the
head-lines convention to future `crates/rune` work, but `extensions/browser`'s
`_head()` emits it today and now emits the full block. Outside its anchors and
inside another step's territory, so it said so rather than reaching.

**What waits on a rebuilt binary:** everything here was verified directly in
Python, including the live depth sweep run in-process through the browser
extension's own service module. No full trial through `eidolon.exe serve` was
possible against a stale release binary. The `triage-linux` `expect` pattern
`^COMMAND\s+PID\s+USER` is reasoned from that invocation's known header, not
captured from a live Linux run, and is flagged as such.

### E4 - a failed command is a failure, `eidolon-rune` 59 -> 62

See [Decisions](Decisions.md#2026-09-19---ten-layers-and-only-two-were-wrong)
and [the caller-set note](Decisions.md#2026-09-19---a-rule-is-conservative-because-the-caller-set-is-unknowable).
This closes **step 5**, the last of `design/observation.md`.

**The new primitive**, beside `shell` rather than replacing it:

```
eidolon::shell_exec(command: String, timeout_s: Option<i64>)
    -> Result<#{ content, ok, code, timed_out, cancelled, truncated, background }, String>
```

Built from the identical `eidolon_tools::shell::run(...)` call `shell` already
makes -- same cwd, same timeout parsing, same cancellation token. A genuine
spawn failure still returns a plain `Err(String)`, so that path is unchanged.

**An object rather than a string**, because `bash.rn` now has to make a
*decision* -- is this a failure? -- not render text, and encoding a decision
into the string is the text-marker grammar ruling 3 rejected as fragile across
boundaries. The fields are `Exec`'s own, losslessly.

**The whole defect was one line.** `crates/rune/builtin/bash.rn` now returns
`#{ content: r.content, is_error: !r.ok }`. Every other layer already worked.

**`recover` is reachable, demonstrated in two halves with the seam named** --
three Rust tests through a real `Dispatcher`, plus E3's forward-written
`test_a_failed_listing_command_reaches_recover`, run live and hermetically.

**Two break-tests, both correctly inverted**: flipping `bash.rn`'s mapping
failed the nonzero-exit and timeout tests while the zero-exit test correctly
kept passing; inverting `Exec::ok()` itself failed all three, each in the
exactly opposite direction.

**M4 was not guessed.** The document's own fallback -- "none required; the
receipt names the file and the total" -- was used and named.

**Counts: `eidolon-rune` 59 -> 62.** It flagged `eidolon-web` 308 -> 316 as
**not its own** (C7's, live in that crate) and noted a new crate
`eidolon-verba` at 28 tests, also not its own -- the same discipline E1 showed
when it reported a stale baseline rather than absorbing it. Full workspace test
run: every line 0 failed. Check 0 errors, no new warnings.

**A correction that would have shipped a broken link:** the spec suggested
`docs/extensions.md` reference `[design/observation.md](design/observation.md)`.
`eidolon/docs/` contains only `extensions.md` -- the design documents live at
the workspace root, **outside the eidolon git repository entirely** -- so that
link resolves to nothing. It named the path in prose with an explicit "outside
this repository" note. That is the third time today this repository's
relative-path geometry has bitten, and the first time it was caught before
landing.

`crates/tools/src/shell.rs` needed **no change**: S34's rewrite had already
exposed every field the object shape needed.

### S28 + S33 - jev says what it knows, `jev/tests` 235 -> 244

See [Decisions](Decisions.md#2026-09-19---the-dynamic-degradation-paid-out-the-same-day).

**`automation.lint` is on the wire** -- `_automation_lint` at `jev/server.py`
line 433, registered at 567, calling the **same five lints `graph.py` already
had**. No second linter, and no new parsing: the method hands `args["graph"]`
straight to `load_graph()`, whose existing string-sniffing branch already
handles the editor's raw text.

**Clean versus dirty, proven four independent ways**, all win32: an ASGI
`TestClient` (8 new tests); a live restarted service on a synthetic graph; the
live service against the **real** `extensions/jev/graphs/wiki-hop.json`, which
lints **clean**; and the real spin fixture, which produces the genuine
`_lint_always_spin` message about a transition that can only spin until
`wall_s`. The dirty case is the real rule, not a synthetic error.

**Break-test:** removing the `METHODS` registration reproduced the literal
`{"ok":false,"error":"unknown method 'automation.lint'"}` that the LINT button
was already degrading into -- confirming both halves of the contract at once.

**S33: a parked run now says what it is asking.** `run.py:1876` serializes
`rt.escalation` verbatim, the same rule `_persist_park` already applies to the
identical payload. Proven in-process (park below the floor, read `question`,
`options` and `why` back, watch it disappear after `answer()`) **and through a
real OS subprocess over real HTTP** on a scratch port with every jev path
redirected -- escalation present verbatim with `chooser_calls: 0`, then gone
after `answer(stop=...)`, with the persisted JSONL landing in the **scratch**
runs directory.

**Counts: `jev/tests` 235 -> 244** (7 skipped, unchanged). `jev/test_server.py`
33 unchanged. Production `decisions.jsonl` md5
`c559328a1c24bfa60cdadf19fe80b93b`, 846 bytes, verified before, between both
break-test cycles, and after; the final directory listing is
`['decisions.jsonl']` alone.

**Two findings filed, not fixed.** A pre-existing `SyntaxWarning` at
`run.py:46` -- `"\e"` in a non-raw module docstring containing a literal
`%LOCALAPPDATA%\eidolon\...` path -- which fires at parse time on the source
text and is therefore OS-independent, reasoned rather than observed on Linux.
And, concretely: **the release `eidolon.exe` still cannot `ext start jev`**,
failing with the pre-S34 `spawning background bash ... program not found`,
while the debug binary built this morning starts it fine. It correctly
avoided `cargo build --release` and handed the resequencing back.

### D8 - extension trust, `eidolon-rune` 62 -> 69, `eidolon-cli` 59 -> 65

See [Decisions](Decisions.md#2026-09-19---trust-landed-and-it-does-not-make-a-graph-runnable-unattended)
for the gate accounting and how trust is structurally prevented from ungating.

`ExtensionEntry` gains `#[serde(default)] approval: ExtensionApproval`
(`Ask` | `Trust`, snake_case, **rejecting anything else at TOML-parse time**).
`Config::extension_trusted()` requires `enabled && approval == Trust` --
vouching for tools nothing loads is not a state worth representing --
and feeds `PolicySettings::trusted_extensions`.

The prefix match is **boundary-checked**, `strip_prefix(ext).and_then(|r|
r.strip_prefix('_'))`, not a bare `starts_with`; break-round 4 confirmed the
naive version wrongly matches `"je"` against `jev_choose`.

`policy.rn` gains `browser_open`/`snapshot`/`read`/`back` as `ALLOW`, citing
the **same** reasoning already written for `fetch` and `search`, and
`browser_click`/`type` as `FLAG` -- *"reading a page is fetching; clicking what
the page told you to click is acting on its instructions."*

**Five break rounds**, each producing the wrong verdict for the right reason,
including two that proved independence: removing the read-only guard leaked
`jev_run` (31 -> 30) while the click test stayed green, and deleting the
sentinel promotion failed the positive control while the never-promote test
correctly stayed green, since deletion can only get stricter.

Counts `eidolon-rune` 62 -> 69, `eidolon-cli` 59 -> 65; everything else
unchanged, workspace green. Before syncing back from its worktree it **diffed
each of its four files against the live main tree** specifically because other
agents were editing it, confirmed every diff purely additive, then removed its
worktree and branch.

### S31 - the browser cleans up after itself, 31 -> 55

See [Decisions](Decisions.md#2026-09-19---s31-applied-the-mornings-hardest-rule-to-its-own-fix)
for the unreachable-shutdown finding and the silent-corruption discovery.

**Two paths, because they fail differently.** Graceful shutdown through a
`_lifespan` context manager (replacing an `@app.on_event` this FastAPI already
deprecates), which closes the browser and stops Playwright and lets
Playwright's own `removeFolders()` do the deletion -- confirmed by reading the
bundled `coreBundle.js` rather than assumed. And a **startup sweep** for the
crash case, wrapped so a sweep failure can never block startup.

**Safely sweepable** requires all three: Playwright's own `mkdtemp` prefixes;
age >= 60s; and **not locked**, via `_profile_locked()` mirroring Playwright's
own `isProfileLocked` -- on Windows an `os.open()` against `lockfile` where
`PermissionError` means held, on POSIX the `SingletonLock` symlink's trailing
pid through `os.kill(pid, 0)`. Every check errs toward **leave it**: a missed
orphan costs a retry, a wrong deletion costs a live browser.

**An accepted risk, stated rather than buried:** the prefixes are Playwright's
generic names, not eidolon's, so an unrelated Playwright tool on the same box
could match by name -- accepted because non-persistent `launch()` has no
`user_data_dir` to namespace, and the liveness checks carry the weight.

**A real bug found in passing:** `rmtree(..., ignore_errors=True)` can partially
fail while the code reported removal unconditionally. Now post-conditioned on
`not entry.exists()`.

**Both halves proven by `%TEMP%` diff, not the process table** -- including the
crash half done properly: the driver located by pid, killed with the exact
`taskkill /PID <pid> /T /F` that `kill_group` uses, the orphan confirmed on
disk, then swept. And the negative case, a second independent `Browser()`
standing in for a second eidolon process, surviving a sweep at `min_age_s=0`
with every file intact.

**Counts: 31 -> 55.** `service.py` 435 -> 717 lines, `test_server.py` 682 ->
1280. Hash-verified `.orig` backups kept as the audit trail, since `bonsai2/`
has no git history to fall back on.

### R1 - the observation chain reviewed, one confirmed break

See [Decisions](Decisions.md#2026-09-19---i-closed-a-two-sided-contract-from-one-side).
Read-only review of the five interlocking changes plus S28, hunting the seams
no single author owned.

**Confirmed broken, ranked first:** `parse_runs_answer` hardcodes
`escalation: None`, so a parked run never shows as parked anywhere in
`eidolon serve` or `eidolon web` -- despite S33 shipping the producer the same
day and this record marking it done. **My filing error**, dispatched as
**S42**; the Tasklist row is corrected.

**Five further findings, none breaking:**

- `crates/verba` holds a **second** `CallOrigin::Script` path that E1's change
  now governs -- safe, and untested.
- `jev/server.py`'s docstring cites the wrong Rust file for `automation.lint`'s
  request shape. A docstring claim is a testable assertion; this is the third
  false one found today.
- `expect`'s NLI branch can leave a warning in `rt.warnings` for an observation
  that is then **discarded** -- a side effect surviving a value that does not.
- `bash` output flows unbounded into `decisions.jsonl`, a disclosed and
  accepted cost rather than a surprise.
- One pre-existing resume-path message, outside the chain.

**Everything else stands**, and R1 says which parts it confirmed by running and
which by reading: the `chars: N` chain across the Python/Rust/Python boundary,
`whole()` and `is_error` propagation, run-state coherence after a mid-action
`ERROR`, `automation.lint`'s real wire contract, `bash.rn`/`shell_exec`, and
that `expect` introduced no second expression language.

It ran E1's leak-safety test live -- **passing** -- after first confirming the
two worktrees do not share a `CARGO_TARGET_DIR` and so would not contend with a
live build. It also **corrected its own report** on a claim it had inherited
from my brief rather than established: `entails` runs a local in-process CPU
model, not a network call.

### B3c - the POSIX review returned **ship**, and both findings were already closed

B3c's verdict on the POSIX half of `jev/server.py`'s locking arrived late (the
notification was a stale background `find`, not new work). **Ship**, with the
`fcntl.flock` mechanism itself checking out cleanly against real sources --
Starlette, the pinned `dirs` crate, man7, CPython's own `pathlib` -- and by
structural parity with the already-proven Windows path.

Its two Medium findings were both in `_cache_dir()`'s path resolution. **Both
are already fixed in the current tree**, verified by reading rather than
assumed:

**The eager default.** `DECISIONS_LOG` now uses `os.environ.get(...) or
<fallback>` rather than `os.getenv(NAME, <fallback>)`. Python evaluates a
call's arguments before the call, so the `getenv` form ran `_cache_dir()` on
every import even when `JEV_DECISIONS_LOG` was set and the result discarded --
and on a POSIX box with `$HOME` unset and a running UID absent from
`/etc/passwd` (the arbitrary-UID container case), `Path.home()` raises
`RuntimeError` and **took the whole import down**. It fired even when the
operator had done exactly the right thing and pointed `JEV_DECISIONS_LOG`
somewhere disposable. The `or` form runs `_cache_dir()` only when its result is
actually used.

**The absoluteness check**, at `jev/_paths.py:47-50`:

```python
xdg = os.environ.get("XDG_CACHE_HOME")
if xdg and PurePosixPath(xdg).is_absolute():
    return Path(xdg)
return Path.home() / ".cache"
```

`PurePosixPath`, not the ambient `Path`, and **deliberately**:
`WindowsPath("/x").is_absolute()` is `False` -- Windows absoluteness needs a
drive letter -- so the ambient type would **silently invert** the check
whenever a test drives the POSIX branch on Windows by mocking `sys.platform`.
That is the exact shape of the trap the pinned `dirs`/`dirs-sys` crate is being
mirrored against, and `test_server.py:126-145` covers absolute, empty and
relative values. The docstring marks the two POSIX branches themselves as
reasoned rather than observed, which is the right disclosure on a box with no
Linux.

Nothing to dispatch. **B3c closes ship, both findings pre-closed.**

### A6 - `design/unattended.md`, 1,495 lines: the warrant

See [Decisions](Decisions.md#2026-09-19---the-operators-word-given-once-in-writing-for-the-run).

A written **envelope**, consented to once per run: graph + content hash, exact
tool set, browser origins, literal shell commands, action count, wall clock.
`jev_run` asks once with the envelope in the question; nested steps are checked
against it by the layer that can see each bound. Confidence, extension trust, a
judge's yes and `--yolo` never create a warrant, and no warrant reaches a
`Deny`, an unnamed tool, an unconfined click, an unlisted command, or a call
outside the approved call's dispatch chain. New `PolicyOutcome::Warranted`.

**Gate accounting: 62 -> 31 (D8) -> 1 warranted interactive -> 0 headless.**
A6 defends the interactive **1** as correct rather than a shortfall: it is the
operator reading the envelope, and without it there is no consent, only a
config file claiming one.

**Mechanism, without touching `classify_tool`'s signature:** a seven-key
`input.warrant` block from a new read-only `jev_warrant` tool; the jev service
refuses any block not equal to the graph's canonical block; a `Warrant`
`PolicyHook` decorator innermost under `Yolo` and `Judge`, reading a new `ROOT`
thread-local rail; four new `policy.rn` rows. Origin confinement lives **in the
browser process** (`browser_open {confine}` = fresh context + route aborting
unlisted origins), which also closes D1r's loopback `fetch()` shape. Headless
uses `[[warrants]] graph = "...@1"` **instead of** `--yolo`.

**Three findings relayed**, none in its own deliverable:

- **`decisions.log_run_end` has no caller** in `jev/automation/run.py`, so
  `automation.md` section 5's outcome promotion has never run. Filed **S45**.
- The workspace has **no hash crate**; the spec adds `sha2`, and pins the
  warrant id across Rust and Python with identical literal ids in both suites.
- `extensions/browser/`'s line numbers have **already drifted under S31**, so
  A6 anchored that step by name and ordered it after S31 rather than quoting
  positions that were stale before the ink dried. Correct instinct; the D3
  worktree and three wrong-path lookups today all came from the same class.

Measurements **W1-W5** named with fallbacks. Landing order is six steps: (1)
`crates/core` -> (2) `crates/rune` -> (3) `crates/cli`/tui/web/docs as one Rust
agent in sequence, then (4) `extensions/browser` and (5) `jev/` in parallel,
then (6) a live re-run on a rebuilt binary.

**Residual A6 discloses itself:** the gate's action count and wall clock bound
one driving call, not the whole run. Keying them by run id would require
service-specific knowledge inside the policy layer -- refused for exactly the
reason `classify_tool`'s signature is fixed.

### S42 - the runs tab sees a parked run, `eidolon-cli` 65 -> 66

The consumer side of the contract I closed from one side. `parse_runs_answer`
now parses `escalation` from the real payload instead of hardcoding `None`,
reading `why`/`question` (with JSON `null` -> `""`) and `options[].{label,p}` --
exactly what `escalation_html()` at `pages/jev.rs:922` renders.

**Proven against a real `jev/server.py` process**, not a fixture: launched
directly on scratch port 8097 rather than through `eidolon ext start`, which
would have adopted the live `:54051` service -- and with every jev path
redirected **from spawn**, avoiding the finalizer trap by never monkeypatching
inside a shared interpreter. It then **verified the redirection worked before
relying on it**, which is the step that makes the rest of the evidence mean
anything. Three parks forced: an empty menu (deterministic, no model call), a
real floor escalation with three options scored by the actual chooser
(`floor`/`margin` past 1.0 so the outcome does not depend on model output), and
a no-`meta.ask` case confirming `question` really is JSON `null`.

**The test that argued against looking is gone.** Its fixture now carries a
real `escalation` key, plus a second test built from the live capture. The
assertion message that stated a now-false claim is replaced by assertions
against a populated `Escalation`, so **a revert panics instead of quietly
staying green**. Break-test: reverting to `escalation: None` failed both.

**Field-by-field audit, as briefed.** Everything else in `parse_runs_answer`
was already correct; `finished` and the outcome-specific extras are absent by
`RunRow`'s own design, not by oversight. `parse_lint_answer` had no hardcoded
fields -- but three comments claiming `automation.lint` does not exist, which
it checked live and fixed. **Counts measured, not inherited**, and the baseline
matched exactly: `eidolon-cli` 65 -> **66**, everything else unchanged, zero
failures, `cargo check --workspace --all-targets` clean in both trees with only
the two known warnings. Production `decisions.jsonl` md5 verified before,
between every live park, and after.

**Found and left alone, correctly:** `RunRow::outcome`'s doc claims jev's
vocabulary includes `"parked"`, and `crates/web/tests/jev.rs` builds fixtures
with `outcome: "parked"` -- **jev never emits that string.** A parked run's
outcome is `null` -> `"running"`, and only `escalation.is_some()` marks it.
Same family as the bug it was sent to fix, one crate over. Filed **S46**.

### E5 - `roles` at the source, `extensions/browser` 55 -> 83

See [Decisions](Decisions.md#2026-09-19---773372-characters-to-4954-and-the-cap-stops-being-the-question).

`_filter_roles(tree, roles, within, max_n)` in `extensions/browser/service.py`,
called inside `_snapshot()` **before the process boundary**, immediately after
the `within`-scoped `aria_snapshot` and before the response is built. It walks
the indented tree with an explicit stack, computing each line's role and
nearest-landmark ancestor the way `a11y.py::parse_refs` does -- **reimplemented
independently rather than imported**, because `extensions/browser` must not
depend on `jev/`. `_head()` gains a `roles:` line between `scope:` and
`chars:`, read generically by `parse_head`, so an older reader is unaffected.
`known_refs` is set from the **filtered** body, so an excluded ref is refused by
the pre-existing rule rather than a new one.

**Three criteria, all live on `en.wikipedia.org/wiki/Cat`, measured twice:**

1. **Margin.** 4,954 chars -- 257,190 under the cap, 98.1% headroom, ~156x
   smaller than `within: "main"` alone at 773,372 (295% over).
2. **Usable option set, `max` as cap not floor.** 97 option lines / 82 distinct
   names unfiltered by `max`; with wiki-hop's existing `max: 64`, **capped down
   to exactly 64** at 3,141 chars -- truncating a larger real set, which is the
   behaviour I wrongly assumed was absent this morning.
3. **Refs resolve past the first navigation.** Four real navigations in the
   `f<N>e<M>` regime: Cat -> Felidae -> Felīd, each via a `roles`-filtered ref.
   A spent ref was refused; a **never-clicked ref from an older hop** was also
   refused after a later snapshot, proving wholesale `known_refs` replacement
   holds for filtered snapshots across real navigation rather than being
   inherited by assumption from `within`'s earlier proof.

**13 break-tests**, each cycled break -> confirm the specific failure ->
restore -> reverify by content, with the restored hash matching the known-good
state through all of them. Notably: moving validation *after* the Playwright
calls failed exactly one test --
`test_a_bad_roles_argument_never_reaches_aria_snapshot`, catching that
`aria_snapshot` *was* awaited despite a bad argument -- while all eight
real-browser tests still passed, which is the correct signature for a
fail-fast guarantee that does not change behaviour for valid input.

**One legitimate non-failure, correctly identified**: dropping a trailing
semicolon in `tools/snapshot.rn` did not break anything, because Rune block
statements do not require one. Left unforced, on the project's own precedent
that a break-test which does not fail can be the right answer -- provided you
say why.

**Counts: `extensions/browser` 55 -> 83** (6 arg tests, 14 pure-function
projection tests, 8 real-Chromium tests), 83/83 green.

**Reported rather than buried:** one full-suite run failed in
`RealCleanupTests`, a pre-existing S31 test, on a stray
`SCT Auditing Pending Reports<uuid>.tmp` -- a file **Chromium's own background
network-security thread** creates inside a live profile while the sweep test
inspects it. Root-caused, reproduced-clean 3/3 in isolation, confirmed outside
its own diff by line range, and left alone as genuine pre-existing flakiness
under concurrent real-browser load rather than silently re-run until green.

**The graph needs one line.** `extensions/jev/graphs/wiki-hop.json`'s
`states.reading.entry[0].params.input` must become
`{"within": "main", "roles": ["link"]}` -- today it asks for role filtering in
`meta.choose.from.refs`, which only runs after the 773 KB tree has already been
refused. Handed to **J2** with the trial.

**Live service moved.** `browser` restarted through the sanctioned
`eidolon ext stop/start browser` rather than a raw kill, since Python does not
hot-reload -- so the fix is actually live and not merely proven in isolation.
It is now **`:54665`, pid 52076**; the old port and pid are confirmed gone, not
orphaned. `%TEMP%` accounting clean: zero `playwright`- or `chromium`-prefixed
entries across 13 break-tests and two live measurement runs.

### S45 - `log_run_end` has a caller, `jev/tests` 244 -> 249

See [Decisions](Decisions.md#2026-09-19---jevs-outcome-vocabulary-from-the-code-that-writes-it).

`_log_run_end` at `jev/automation/run.py:1339-1379`, called from `Run._settle`
at **1497**, immediately before `_final_report`. `_settle`'s `yielded is None`
branch is the one funnel every interpreter-reached outcome passes through
whichever of `begin`/`resume`/`answer`/`stop` drove the generator to
exhaustion -- so one call site covers all four, and structurally excludes the
synthetic `orphaned` path. Writes exactly the `{run, outcome, ts}` row
`automation.md` §5 specifies, and **§5's intent still fits unchanged**.

**Proven live** on a real `jev/server.py` on scratch port 58971, every jev path
set **from spawn**, redirection **confirmed before being trusted** -- park
landed in scratch, production untouched at that instant. Forced an EMPTY park
via `exclude` with `chooser_calls: 0`, answered it with a stop, then read the
row **off disk rather than through the API**:

```json
{"run": "r_a24613f9220742ddbbd7", "outcome": "stopped", "ts": "2026-09-19T14:58:59Z"}
```

**Three break-tests, all failing correctly first try.** Deleting the call site
broke 8 tests; removing the `except OSError` let the write escape all the way
out through `_settle`; and **wiring the call into the orphan path on purpose**
broke exactly the two tests that exist to prove it must not be logged.

That third one nearly did not exist: a bulk `replace_all` had converted a
**negative proof** -- that an orphaned reconstruction produces no row -- to the
new filtered helper, which would have made it pass either way. S45 caught it on
review before running anything, reverted that one occurrence with a comment,
and the break-test then confirmed the revert was necessary rather than
theoretical. **A helper that makes tests robust to a new row makes the test
that forbids the row robust to its own failure.**

**Counts measured, baseline reproduced exactly: `jev/tests` 244 -> 249**
(7 skipped), `jev/test_server.py` 33 unchanged. Production `decisions.jsonl`
md5 verified at every checkpoint including immediately before and after the
live process.

### The sweep: what `automation.md` and `observation.md` lean on

Both documents read in full and every mechanism grepped for callers.
**`observation.md` is, in effect, fully implemented** -- Steps 1-5 all landed,
contrary to its own framing as a spec awaiting execution. Three genuinely
unbuilt items, all in `automation.md` §4's worked example, all **premature
rather than dead**, and filed rather than guessed at:

- **`entail_calls`** appears in the example report and is never populated --
  no such key in `_RunState.counts` or `_final_report`. Needs a decision on
  what counts as an entail call.
- **`record`** likewise, and it is not trivial: not every finished run has a
  live persistence file, so its existence is conditional.
- **`cancelled`** is a documented outcome with a **documented test** at
  `automation.md:1148`, and **neither exists**. No `"cancelled"` string
  anywhere in `run.py`. Flagged with a reasoned, unverified implication:
  `run.rn` has no cancellation handling around its `dispatch().await`, so a
  cancelled run would be left *act*-suspended -- which by `run.py`'s own
  deliberate design does **not** survive a restart, contradicting §4's promise
  that a cancelled run is kept and resumable.

Filed as **S50**. A design document that lists a test proving a feature, where
neither the test nor the feature exists, is the strongest form of the stale
claim -- it names its own evidence.

### J2 - wiki-hop ran: Cat -> Crepuscular animal -> Dusk, parked on the floor

See [Decisions](Decisions.md#2026-09-19---the-graph-ran-three-hops-and-it-walked-past-the-answer).

The first live graph run that moved. Three hops in 89.62s, 6 actions, 3 chooser
calls, ending in a **confidence-floor escalation** at 0.3343 against a floor of
0.35 -- the mechanism working on a live page for the first time, on a genuinely
close call rather than a landslide.

Snapshot sizes held: **4,756** chars at hop 1 (against E5's 4,954 an hour
earlier -- ordinary live drift on an article that moves), **958** at hop 3.
Option counts 65 / 25 / 17. The cap never came near participating.

**Isolation:** the `jev_scratch` extension-rename technique rather than
stopping the shared service, so production `jev` and `browser` were never
touched; `JEV_DECISIONS_LOG` and `JEV_RUNS_DIR` redirected and **verified by a
forced write before being trusted**. `JEV_DECISIONS_DIR` was missed -- a third
path my brief did not enumerate -- and two rows reached production before J2
caught it, removed the directory, and re-verified. `decisions.jsonl` kept md5
`c559328a1c24bfa60cdadf19fe80b93b` throughout, independently confirmed. The
scratch service was stopped through `eidolon ext stop` and confirmed gone by
pid.

**Three findings, none of which it was sent for:**

- **The deployed `policy.rn` is stale and unreachable**, so every gate
  reduction shipped this week is inert on this box -- including for production
  `eidolon serve`. Verified two independent ways. -> **S51**.
- **The `roles` filter blinds `h1`.** `a11y.py` derives `context.obs.h1` only
  from a `role == "heading"` node in the snapshot, so a link-scoped snapshot
  leaves it `None` every hop -- observed as `path: [null, null, null]`,
  `visited: [null, null]`, `evidence.h1: null`. The exact-title arrival guard
  can never fire, the CHOOSE prompt renders its "Current article" line blank,
  and `visited` fills with nulls, plausibly blinding the revisit-loop guard.
  -> **S52**.
- **`eidolon log` truncates its own records** to 121 bytes with a baked-in
  ellipsis, which is how hop 2's character count became unrecoverable.
  -> filed with **S50**.

**And the result that reframes S47:** at hop 1, **"Felidae" -- the goal -- was
present, clickable, and scored 0.00001** while "crepuscular" took 0.9977. The
3.3% reachable set was not what stopped this run. The chooser was.

### U1 - the warrant's foundation, `eidolon-core` 218 -> 222, `eidolon-rune` 69 -> 105

`crates/rune/src/warrant.rs` is **new, 1,389 lines, 27 tests**: `Envelope`
(seven-key strict parse, canonical-JSON serialisation, SHA-256 id), `Standing`,
a bounded `Warrants` ledger with revocation, and the `Warrant` `PolicyHook`
decorator with `pre_tool_root` / `pre_tool_nested`. `PolicyOutcome::Warranted`
is appended after `Yolo` and pinned; `dispatch.rs` gains a
`Verdict::Allow if ruling.warrant.is_some()` arm placed before the `Yolo` arm
**for a reader, not as a tie-break**, since a ruling can never carry both.
`policy.rn` gains its four rows. `classify_tool`'s signature is unchanged.

**The `ROOT` rail composes with what was there.** `host.rs` now carries four
thread-locals -- `CANCEL`, `EXTENSION`, `CHAIN`, `ROOT` -- `ROOT` built in the
identical shape `CANCEL` already used, and replanted across the OS thread
`ScriptTool::call` spawns per nested dispatch by **nesting inside the existing
`with_chain` call** rather than adding a parallel mechanism. Two tests prove a
nested dispatch sees the outermost call as its root, two deep.

**The adjacency proof is non-vacuous**, which was the point. Three live,
root-approved, confirmed, confined warrants, each one perturbation from
authorized, each still asking: a **sibling tool** (`browser_type` against an
envelope naming four other `browser_*` calls) gets `Ask` with "outside
warrant"; the **71st action** against a 70-action envelope asks; a
**second origin** leaves `confined == false` and the next click asks. That
third was itself re-verified by breaking `confine_matches` to return `true` and
watching exactly one test fail.

**W4, measured** -- the journal scan confirming a root's record, debug build:

| branch length | per call |
|---:|---:|
| 1 | 34 ns |
| 101 | 521 ns |
| 1,001 | 3.073 µs |
| 10,001 | 50.181 µs |

Linear in branch length and **independent of where the target sits** -- a root
buried mid-branch cost the same as one near the tail. The temporary test file
and the directory it created were deleted; nothing shipped.

**Eleven break-tests, and three did not fail on the first attempt** -- each a
finding about the test, each fixed and re-verified. The third is the one worth
keeping: breaking `confirm()`'s outcome check left all 27 tests green, because
three of them never call `browser_open`, so an incorrectly-confirmed root fell
through to the **confinement** guard, which independently produced an `Ask`
whose recomposed prompt still contained the substring being asserted on. Fixed
by asserting **exact equality** to the untouched table prompt plus
`warrant == None`. A test can pass through the guard it is aimed at, fail into
a different guard, and never notice the difference.

**A real implementation bug found along the way**, not a test-quality issue:
the wall-clock check compared `elapsed().as_secs() > wall_s`, truncating to
whole seconds, so a `wall_s: 0` envelope would not fire for nearly a full
second. Now compares full `Duration`s.

**The cross-language pin is `w_5e885ef92416`** -- `"w_"` plus the first 12 hex
of SHA-256 over canonical JSON, with keys inserted in explicit alphabetical
order into a fresh map rather than trusting map ordering, matching Python's
`json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=False)` byte
for byte. U1 searched for the Python side and **reported that it does not exist
yet** rather than assuming it would match, and flagged that the fixture's own
`sha256` field is an explicit placeholder.

**It also caught its own step 1 never reaching the live tree.** That work had
been reported complete and existed only inside the worktree; U1 found the gap
during sync-back accounting and closed it, diffing all 18 files, verifying
every hunk containing a deletion line by line, and re-running both suites on
the live tree before removing the worktree.

**Handoff, exact:** four sites need `Warranted` arms -- `web/src/wire.rs` (the
wire-level `Outcome` enum needs its own variant first), `web/src/shim/render.rs`,
`tui/src/usage.rs` (`Verdicts` needs a `warranted: u32` field and `total()`
must count it), and `cli/src/term.rs`, **which cargo cannot see** until the
first three are fixed. Dispatched as **U2**.

### S52 - `h1` restored at the source, `extensions/browser` 83 -> 91

See [Decisions](Decisions.md#2026-09-19---a-pages-identity-is-a-property-of-the-page-not-one-of-its-elements).

`_head()` now stamps `heading:` unconditionally -- one fixed line, 13 bytes,
whatever `roles` the caller asked for -- computed **before** `_filter_roles`
runs. `build_obs()` prefers it and falls back to the old body scan when absent,
so an older browser service still works; that fallback is itself break-tested.
`wiki-hop.json` is **byte-for-byte unchanged**, md5 verified before and after.

Line numbers: `a11y.py::build_obs` at 247, the heading preference at 271-286;
`service.py::_head` at 208, `_first_heading` at 428, wired at 550 and 565.

**The live proof is the part that matters**, and it is two runs on a real page
through the real chain, on throwaway services with all three jev paths
redirected from spawn and confirmed landed in scratch first:

- **start = goal = "Cat"**: reached `arrived` in one hop with **zero chooser
  calls** -- which is only possible if the exact-title `equals` guard fired.
  That guard could not fire at all before this fix.
- **start "Cat", goal "Wolf"**: a real two-hop walk to Carnivora via a real
  click. `path: ["Cat", "Carnivora"]`, `visited: ["Cat"]`,
  `obs.h1: "Carnivora"`, escalation `evidence.h1: "Carnivora"`. No nulls
  anywhere. It parked honestly at "top 0.12 under floor 0.35".

**Three break-tests, none failed to fail**, each restored and **verified by
md5 rather than by the absence of an error** -- including one that deliberately
computed the heading *after* filtering instead of before, which failed exactly
the two ordering-sensitive tests and correctly left the unfiltered-snapshot
test green.

**Counts: `extensions/browser` 83 -> 91**, `jev/` 282 -> 286. And a correction
worth keeping: my brief's jev baseline (249) did not match its measurement
(282), and **both are right** -- they discover from different roots, and
249 + `test_server.py`'s 33 is 282. Two agents reported different numbers today
and neither was wrong. Briefs now name the root.

### S47 - `design/judgement.md`, 1,202 lines, and the experiment was run

See [Decisions](Decisions.md#2026-09-19---the-floor-separates-nothing-and-the-chooser-was-never-the-problem).
Asked to specify a calibration experiment; **ran it instead**, on real data, and
ruled from the results.

**Six rulings**, each with its own stated confidence, plus an eight-step spec
with the test that fails on each revert:

1. **Confidence is evidence only inside a measured distribution.** A
   per-checkpoint **calibration record** keyed by `_ckpt_id()`, with
   `JEV_CALIBRATION_DIR` as a **fourth isolation variable** -- the graph
   declares `meta.jev.calibration`, and a run is **calibrated**, **advisory**
   (every choice parks carrying its ranking), or **refused at `start`**. A
   checkpoint with no record cannot silently be trusted.
2. **`choose.prefer`** -- per-option guard rules evaluated *before* the
   chooser, on an `option.*` scope, with `equals` gaining `fold`. Wiki-hop's
   single rule `option.label == {{context.goal}}` catches hop 1 exactly, **at
   zero cost**. The goal was in the menu on 27.6% of human steps; humans took
   it 90.1% of the time, the shipped chooser 6.1%, the trained one 75.8%.
3. **Duplicate labels collapse before scoring** -- see
   [Decisions](Decisions.md#2026-09-19---the-right-answer-parked-as-a-tie-with-itself).
4. **Rejected on measurement, not taste**: a standing second scorer,
   per-option `openjev` scoring, a runner-invented progress distance, and
   self-consistency ablations -- the last had **no discriminative power on
   either checkpoint**. Post-hoc verification and backtracking stay *graph
   authoring* rather than runner machinery; the payload gains
   `evidence.picks` and `run.since_signal` so an author can write them.
5. **The landmark predicate stays** -- see
   [Decisions](Decisions.md#2026-09-19---i-was-wrong-about-see-also-and-the-cause-is-more-interesting).
   -> **S53**.
6. **Research runs advisory until the graph earns a curve.** "Found it" is a
   rule the author writes, the trajectory rides in the payload, and wandering
   is bounded by declared numbers rather than by a runner's opinion.

**Measurements it could not take, with the fallback it used, stated rather than
glossed**: warm `openjev` latency in isolation (step 4 instruments it), the
cost of an advisory park against a local model, transfer from Wikispeedia
titles to real link text -- where it found a concrete failure, the trained model
clicking a taxobox `C` at 0.53 for goal `Dog`, a label shape its training set
never contained -- `section` on other articles and sites, the 512-byte context
window (**measured**: at 192 bytes no option label reaches the window at all, so
the page is invisible to the chooser), the frozen-encoder path, and 8 epochs
against the 4 it ran, with validation NLL still falling 3.38 -> 3.02.

**Spec order:** 0 experiment -> 1 calibration record + `jev/calibrate.py` +
`/health` -> 2 advisory mode -> 3 dedupe + `prefer` -> 2b wiki-hop declares its
curve and takes its floor from the table -> 4 timings, `entail_calls`, `picks`
-> 5 `section` in the browser -> 6 doc edits **striking two sentences in
`automation.md` that the measurements refuted**.

Scratch artifacts -- the Wikispeedia set, `wikispeedia.pt`, the probes -- are
under the session scratchpad, **outside both repos**. Production
`decisions.jsonl` unchanged; nothing in the `cms-agent` tree was edited.

### U2 - the warrant reaches the CLI, the tui and the web; the tree compiles

`cargo check --workspace --all-targets`: **0 errors**, confirmed twice -- once
in the worktree, once directly in `eidolon/` after copy-back -- with only the
two known pre-existing items.

The four sites, and the wording is the deliverable:

- `web/src/wire.rs` -- `Outcome::Warranted` plus its `From<&PolicyOutcome>` arm.
- `web/src/shim/render.rs` -- joins `Approved | Declined | Yolo => None`, **no
  rendered line**, because the warrant was already visible once: as the
  question the operator answered.
- `tui/src/usage.rs` -- `Verdicts` gains `warranted: u32` **immediately after
  `yolo`**, and `total()`, `verdict()` and `report()` all place it the same
  way. Counted beside yolo, never folded into it.
- `cli/src/term.rs` -- `"{asked} ({reason}) — warranted{by}"`. Distinct from
  `Yolo`, because a warrant answers one graph at one hash rather than
  everything; distinct from `Approved`/`Declined`, because nobody was asked
  this time; and it **names the warrant**, so "why did this run without asking
  me" is answerable from the line itself rather than a second lookup.

**`[[warrants]]` reaches the policy layer**: `Config` gains
`#[serde(default)] warrants: Vec<Standing>`, `policy_settings()` threads it
verbatim alongside `enabled_extensions`, and `main.rs`'s `build()` installs the
`Warrant` decorator **only `if gated`** -- no table means nothing to narrow,
the same reasoning `--yolo` already gets. It rides `Runtime` into both the TUI
and the chat loop's own `warrant` command (list / `off`).

**Envelope composition untouched**, confirmed two ways: `warrant.rs` never
appeared in an edit, and the pinned `w_5e885ef92416` test still passes.

**11 break-tests, 2 did not fail** -- both chased, one a redundancy and one a
real hole; see
[Decisions](Decisions.md#2026-09-19---two-more-break-tests-that-did-not-fail-and-what-each-meant).
The `serde(default)` break is worth noting for its blast radius: removing it
failed **16 tests workspace-wide**, every bare `config.toml` in the suite.

**Counts measured: `eidolon-cli` 66 → 75, `eidolon-tui` 389 → 396,
`eidolon-web` 316 → 317, `eidolon-rune` 105 → 106**, core unchanged at 222.
That `eidolon-cli` 66 -- against the 69 I briefed -- is what exposed S51's loss.

**Docs**: a ~69-line "Running unattended: warrants" section in
`eidolon/docs/extensions.md` covering the trust requirement, the TOML shape,
"replaces `--yolo`, not joins it", the stale-policy behaviour **which it
test-backed before publishing**, and the confinement contract.

**Four corrections, stated as corrections**: two initial drafts that differed
from the ruling's literal spec (`render.rs` grouping, `term.rs` wording), one
placement (`usage.rs`), and one factual error in its own prose about `NoUser`
resolving to `Declined` rather than `Refused`. It also added `{by}` to
`Approved`/`Declined` -- a real fix, since a requested-warrant note on an
approved root had been silently dropped.

### S53 - the `section` scope, `extensions/browser` 91 -> 106

See [Decisions](Decisions.md#2026-09-19---see-also-is-reachable-and-the-cap-was-the-whole-story).

A landmark becomes a **section root** iff its first direct child line is a
`heading` with a matching name, and a kept line's *nearest* landmark must be
that root -- never `within` itself, since a section is nested **inside**
`within` rather than being it. `_snapshot` validates that `section` requires
both `roles` and `within` before any Playwright call; the existence and
ambiguity checks need the tree, so they live in `_filter_roles` and say so.

**Both refusals name what went wrong and the page it went wrong on** -- zero
matches and more-than-one are different messages, and both were proved
non-vacuously by breaking them. `max_n`'s early exit is **disabled** while
resolving a section, so a duplicate later in the document cannot be hidden by
the cap; that interaction is itself a break-test.

**Live on Cat**: `section: "See also"` -> **1,871 chars, 31 links**, 0.71% of
the cap, names listed in the Decisions entry. A nonexistent heading refuses
live with the section named. Ref resolution proved across three real
navigations including an ordinary same-tab click, and **made durable** as
`test_a_section_scoped_ref_survives_a_second_navigation_not_just_the_first`.

**Five break-tests, none stayed green**, each checked against the *actual*
failure message rather than pass/fail -- and one was added only **after** S53
proved it lacked coverage for the property, writing the test first. The
inverted "never `within` itself" break failed with the lead-paragraph link
visibly leaking into the body, which is the failure you want: legible, not
merely red.

**Counts: 91 -> 106**, 15 new, all green. The 91 baseline carried one
pre-existing real-Chromium flake, re-run in isolation and passing, correctly
not counted against the change.

**Handoff it did not take**: `a11y.py`'s `parse_head` needs **no** change --
generic `key: value` lifting already carries `section` through -- but
`build_obs` needs one line beside its existing `scope` lift, plus
`judgement.md`'s own named test. `jev/` is held by a live agent, so S53 read
it, confirmed the line is absent, stated the handoff, and reached for nothing.

### U4 - `jev_warrant`, the refusal, and `automation.md` caught up

See [Decisions](Decisions.md#2026-09-19---the-two-languages-agree-and-the-proof-was-built-the-hard-way).

`extensions/jev/tools/warrant.rn` -- a read-only pass-through to a new
`automation.warrant` -- returns the graph's seven-key canonical block plus its
id. `jev/automation/warrant.py`'s `check(passed, graph)` is the enforcement
point and is called **unconditionally by both `automation.start` and
`Run.answer`**, so a resume cannot smuggle a block past a check the start
performed.

**`automation.md`'s five edits, verified programmatically rather than by
eye**: the schema block is `json.loads`-equal to `schema.json`, and both worked
examples are equal to the real `extensions/jev/graphs/*.json`. Section 6 is
rewritten around a standing warrant replacing `--yolo`, which forced three
pieces of honest collateral -- the "what a box needs" table, and the Melete
seam's two steps, one of which **must now pass `warrant` in the `jev_run` call**
for a standing entry to have anything to match. It checked that against the
ruling's own "the call is requesting" language rather than assuming.

**Four break-tests, all correct first time**, each restored and md5-verified,
with a final `grep` for leftover markers returning nothing. Two are worth
noting for their precision: breaking the resume-time check failed **exactly**
the two tests that exist for it with zero collateral across 74, and breaking
`_lint_warrant`'s tools/actions consistency failed exactly one across 118.

**It declined to run `eidolon ext list`**, which the ruling named as a
verification step, because that operates the live `eidolon serve` on `:8085`
built from an off-limits tree. Substituted a brace/bracket/paren balance check
across all four touched `.rn` files and said what it substituted and why.

**Findings flagged, not fixed** -- all outside its five named edits, all now
tracked: `automation.md`'s "Three lints..." paragraph is stale and now also
omits `_lint_warrant`; the final-report example's `entail_calls` and `record`
match nothing `_final_report` produces, **independently confirming S45's
finding from the other direction**; and "The row" example lacks the `warrant`
key every decision row now carries.

### S46 - the fiction removed, `eidolon-web` 317 -> 317

`RunRow::outcome`'s doc comment now lists **only the five words jev's
interpreter assigns** -- `reached`, `stopped`, `error`, `exhausted`,
`orphaned` -- carves `running` out explicitly and attributes it to
`parse_runs_answer` in `crates/cli/src/serve_host.rs` rather than to the
service, and replaces `parked` with an explicit negative plus a pointer to
where parked-ness actually lives. It also says why the field is a `String`
rather than an enum: **a stale copy of the interpreter's vocabulary in this
crate would render a run it understands perfectly as a blank.**

The three fixtures now say `outcome: "running"` with `escalation: Some(..)`
untouched -- the real shape of a parked run. All three were checked for what
they were actually proving, and **none turned out to be proving nothing**: both
real tests key entirely off `escalation`, never `outcome`, so they proved then
what they prove now, just against a payload jev can emit.

**The count is identical before and after, and that is the point.** S46 said so
plainly: the fictional fixture never caused a failure, which is exactly why
this needed reading rather than running. A pass-count comparison could never
have revealed it.

The `#[ignore]`d browser harness mattered too, for a different reason: a human
pointing a browser at it would have seen a chip reading **PARKED**, which the
real service can never produce. S46 captured the real rendered output during
break-testing and confirmed a parked run's chip reads `running` -- the word
"parked" appears only in the page-header aggregate, a fixed label.

**Five break-tests, four correct and one a finding** -- see
[Decisions](Decisions.md#2026-09-19---seven-blind-assertions-every-one-found-by-accident).
It flagged rather than fixed, correctly: the weak assertion is pre-existing and
unrelated to the fiction it was sent to remove. -> **S61**.

**Discipline worth copying**: it measured `eidolon-web` itself rather than
trusting my baseline (which held), and then **declined to restate the other
four crates' counts** because it had not measured them -- "I won't repeat
numbers I didn't check." It also noted that a grep pattern spanning a wrapped
line silently matches nothing, having hit that itself.

One adjacent gap it left alone as out of scope: **nothing asserts on the
"parked" count in the page-header stats**, which is the one place that word is
real, user-facing copy.

### S61 - three blind assertions bound, `eidolon-rune` 106, `eidolon-web` 317

See [Decisions](Decisions.md#2026-09-19---s61-found-three-and-said-plainly-what-it-had-not-looked-at).

Counts **unchanged on purpose**: nothing was added, three existing assertions
were rebound. An unchanged count beside three real fixes is the honest outcome
here -- the defect was never that a test was missing, it was that a test was not
listening.

Each fix was proved the same way, and the cycle is the deliverable: **passes
unbroken, fails under a named break, passes again after revert**, with the
actual failure output read rather than the exit code. Finding 3 additionally ran
the whole `pages::jev` module (11/11) for collateral.

**Two worktrees, both siblings of `harnox/`, both removed**, each mirroring the
dirty tree by hand from `git status --porcelain` (73 paths) and confirming with
`diff -rq` **before touching anything** -- which is also how it knew no other
agent had edited underneath it. Copy-back verified per file with `diff -q`, then
a full-tree `diff -rq` returning empty, then a live-tree grep for a distinctive
**single-line** string from each change (it noted the wrapped-line trap S46
hit). `grep -rn AUDIT_BREAK` returned nothing in all three trees.

The four protected processes -- `eidolon serve` 31900, Open WebUI 8736, jev 528,
browser 58728 -- confirmed running and unchanged before and after.

**Platform caveat it raised itself**, unprompted: every command ran on Windows,
the changes are plain Rust string assertions with no OS-specific paths, so
Linux-safety is **reasoned, not observed**. Exactly the labelling A7 was told to
use, arrived at independently.

### S62 - `eidolon-web` 317 -> 318, on the confined lane

See [Decisions](Decisions.md#2026-09-19---the-first-flash-task-landed-and-the-checksum-is-the-proof).

One test, `the_head_counts_only_the_runs_that_are_parked`, at
`crates/web/tests/jev.rs:603`. It covers the **one place the word "parked" is
real, user-facing copy** -- the page-header stat block -- which until now nothing
asserted on, while the *fictional* `parked` that S46 removed had been sitting in
three fixtures.

**Cost: $0.013, 18 tool calls, 29 s of model time**, 342.7k of 357.4k input
tokens served from cache.

Verified by the orchestrator rather than accepted: `pages/jev.rs` **md5 equal to
its pre-dispatch value**, line 542 reading `is_some()`, tracked-tree status
identical at 73 paths, protected decision log unchanged, and the break-test
re-run independently.

### S54 - dedupe, `choose.prefer` and the `section` lift; `jev` 288 -> 308

See [Decisions](Decisions.md#2026-09-19---the-right-answer-was-winning-three-times-and-losing-because-of-it).

**+20, decomposed and matching**: +3 a11y, +7 options (3 dedupe, 4 prefer), +2
guards (`fold`), +7 schema (the `prefer` grammar), +1 run (the real Felidae
capture). **The delta is right; the absolute pair S54 reported -- 321 -> 341 --
is not.** Measured in the live tree two independent ways, per-file discovery
summing to **308** and a direct `grep -c "def test_"` across the same ten files
also **308**, against U4's **288** baseline. Same +20, both endpoints 33 high.
See [Decisions](Decisions.md#2026-09-19---i-recorded-a-count-while-the-check-on-it-was-still-running).
0 failures, 0 errors, **7** pre-existing intentional skips gated on
`JEV_TEST_OPENJEV=1` and ~9 GB of real weights. **Zero environmental failures**
-- interpreter named:
`C:/Users/dxcen/Projects/cms-agent/models/jevlike/.venv/Scripts/python.exe`,
Python 3.14.7, torch 2.14.0+cpu, real torch and not a stub.

`_collapse_duplicates` runs **after source-build, before `exclude` and `max`** --
so `max` counts distinct options rather than raw ones -- with a second pass after
`also` is appended. The spec was ambiguous on ordering; S54 chose, and
documented the choice in the code rather than leaving it to be rediscovered.

**Eight break-tests**, each restored by exact-string revert and verified by
content, with `grep -rn "BREAK-TEST"` clean across `automation/`. Every
non-failure is individually explained -- including one that is a **finding about
a test** rather than a pass, and one that caught S54's own violation of the
zero-cost rule.

**Three-variable isolation, proved by writing**: `JEV_DECISIONS_LOG`,
`JEV_RUNS_DIR` and `JEV_DECISIONS_DIR` all set before interpreter start (module
constants are read once at import -- confirmed required, not optional), then one
real write forced down each path and **read back**. The operator's
`decisions.jsonl` confirmed at md5 `c559328a…`, 846 bytes, sole file in its
directory, at **three separate checkpoints**: after the first suite, after all
eight break cycles, and after the final suite.

**It flagged rather than wrote**: it noticed `Build-Log.md` is an active
convention, judged a log entry outside its deliverables, and said so instead of
either writing one unasked or silently skipping it. Correct on both counts --
this entry is the orchestrator's job.

### T1 - a triage probe, and a rebuild fact nobody had

See [Decisions](Decisions.md#2026-09-19---live-triage-by-a-cheap-model-measured-instead-of-argued).

Not a code change: a **measurement of whether this harness can already do what
A8 is being asked to rule on.** Given one operator-voiced symptom and no hints,
the flash lane found all three processes holding the release binary, identified
the mechanism as an image-section mapping by matching module size to file size,
traced both parent chains to their roots, and separated the operator's
long-lived `serve` from the two transient sessions on "parent gone, seven hours
older."

**The actionable output**: `target/release/eidolon.exe` and
`target/release/deps/eidolon.exe` are one inode with `links=2` -- confirmed
independently at `1688849861192710`. The rebuild therefore needs **all three**
processes stopped, and the file the linker fails on is the `deps/` one.

Standing rule added to the lane: **no flash dispatches while a rebuild is
pending**, because every executor is another holder.

### U3 - origin confinement, `extensions/browser` 106 -> 136

See [Decisions](Decisions.md#2026-09-19---a-redirect-walks-straight-through-a-route-handler-and-the-first-implementation-let-it).

`browser_open {url, confine: [origins]}` replaces the context wholesale --
`new_context(accept_downloads=False)`, no cookies, no storage, no downloads --
and installs a context-level `route("**/*")` that resolves every request itself
and aborts anything off the list with `net::ERR_BLOCKED_BY_CLIENT`.

**+30, decomposed and matching**: 8 `OriginOfTests` + 6 `ConfineValidationTests`
+ 3 `OpenConfineDispatchTests` + 13 `ConfinementTests`. Both full runs green
(71.7 s, 78.8 s).

**All six required proofs are live** against real headless Chromium and real
loopback `http.server` fixtures -- including the one that matters most, the
**D1r in-page `fetch()` shape**, verified by standalone diagnostic to be a real
closure at the routing layer rather than a mocked one. The redirect proof exists
because building it **found the redirect bug**.

**Six break-tests, none failed to fail.** Each restored from a post-implementation
green md5 (`service.py` = `b90f2239…`) and re-verified by hash. Two are worth
copying: one **separated two independent guards** by leaving a single survivor,
and one showed a guard whose job turns out to be **error quality rather than the
block itself**.

Live discovery that corrected two of its own test assertions: **a blocked
top-level navigation lands on `chrome-error://chromewebdata/`**, not on the
previous URL.

**Protected state, all verified at the end**: the operator's `decisions.jsonl`
at `c559328a…`, sole file in its directory; all four ports and pids undisturbed
(54665→58728, 54051→528, 8085→31900, 3000→8736); **zero** of its own orphans
left in `%TEMP%`.

**Optional work skipped, not dropped**: the W1 wall-clock measurement, which the
design doc marks optional. Said so explicitly.

### P1 - `docs/design/posix-inventory.md`, 68 findings, 31 blockers

See [Decisions](Decisions.md#2026-09-19---88-citations-88-correct-and-my-checker-was-wrong-twice-first).

Not a code change: the line-cited inventory L1 and A7's stage one work from.
Ten categories, a table each, an UNCERTAIN section, a **non-findings** section,
totals counted two ways (68 distinct findings; 90 category rows, since one line
can belong to several), and a three-tier coverage statement.

**Every citation mechanically verified**: 88 of 88 exact, by an instrument
self-tested against a corrupted citation and itself corrected twice -- for
escaped pipes and for PowerShell line-continuation backticks, both markdown
artefacts rather than errors in the work.

**Cost `$0.162`, 81 calls, 5 m 13 s**, 7.22M of 7.32M input tokens cached. Seven
gate declines along the way, all the `ask`-tier auto-decline; it routed around
each and finished.

## 2026-09-19 — Minerva takeover and supervised Eidolon runs

Product-facing WebUI name/banner title, current project docs, and skin comments
now say Minerva. `hoot` remains the launcher/CLI; provider IDs, DOM markers,
banner ID, paths, and historical decision entries remain compatible. Corrected
the handoff coordination anchor and stale jev port/launcher documentation.

Observed validation: `bin/hoot.ps1` and `bin/watch-agent.ps1` parse with zero
PowerShell syntax errors. `node --check webui/loader.js` passed. Running
`python webui/build-loader.py` emitted 165 icons, 101682 bytes, with an unchanged
SHA256 compared to the edited loader. No Rust/Python application suite was run
for these branding/documentation changes. No live service restart was performed,
so existing WebUI processes have not adopted the new launch environment yet.

Eidolon ran real `ollama:deepseek-v4.1-flash` implementation and review sessions
with fallback disabled: initial implementation 331s, first review 242s,
follow-up 30s. The first two could not find bash; child-only PATH injection of
Git Bash fixed a subsequent actual shell probe. A repair run stopped producing
output and reached its 180s deadline. Its agent was terminated, but the original
supervisor waited on inherited output pipes; supervisor interruption and an
absent agent PID confirmed cleanup. The runner now bounds pipe draining to
two seconds per stream. Remaining repair edits were completed by the coordinator.
A deliberate one-second probe recorded `timed_out`; its child PID was terminated.

`bin/watch-agent.ps1` and `docs/Agent-Operations.md` provide the repeatable
workflow. Per-run logs/status live under ignored `logs/agents/`; takeover
implementation/review evidence is retained there. Final review is recorded in
`logs/agents/closeout-review-001.stdout.log`: PASS for the scoped source review
in 24s, runtime measurements explicitly attributed to the coordinator. The
preceding final-review-001 stalled in shell probes; the corrected supervisor
terminated it at 180s and completed bounded drain/status recording at 184s.
PowerShell 7 is now enforced with `#requires -Version 7.0`. The review also
identified a remaining Architecture.md fixed-port claim, corrected on closeout.

Root Git was already initialized, with no commit or remote. Added exclusions
for secrets, runtime state, weights, binaries, and independent nested repos.
GitHub CLI is unauthenticated: remote Minerva creation and publication remain
blocked on sign-in and owner/visibility selection. No commit or push was made.

## 2026-09-20 - S68 done: the umbrella has a meaning

The operator set the key. `%APPDATA%\eidolon\config.toml` gained one line, at
the **top of the file**:

```toml
default_model = "ollama:deepseek-v4.1-flash"
extensions_dir = "C:/Users/dxcen/Projects/bonsai2/extensions"

[[extensions]]
...
```

A backup of the 154-byte original sits beside it as
`config.toml.bak-20260919-200302`, md5 `387c6feab848003597a1717b9aeb1a20`.

**Placement was the whole risk, and it is the kind TOML does not report.** The
file ended inside an `[[extensions]]` table array, so a line appended at the
bottom would have become `extensions[1].default_model` -- parsed, accepted, and
silently ineffective. Verified by parsing the document and reading the
structure rather than grepping the text:

```
top-level default_model : 'ollama:deepseek-v4.1-flash'
top-level keys          : ['default_model', 'extensions', 'extensions_dir']
extensions[1]           : {'name': 'browser', 'enabled': True}
```

`extensions[1]` is clean. That check was chosen to **falsify the hazard**
rather than to confirm the edit -- `grep default_model config.toml` would have
passed in both the working and the broken case, which is the blind-assertion
shape exactly.

### Two things established without being able to run anything

The permission classifier refused to execute `eidolon models`, so the Rust side
is established from the tree instead, and the substitute is better than the
thing it replaced:

- **The value format.** `config.rs:4` documents a bare `"claude-sonnet-5"`,
  which made the `ollama:`-prefixed form look uncertain. But `main.rs:690` puts
  `cfg.default_model` into the *same variable* as the `-m` flag before
  `create_session`, and the flash lane has run `-m ollama:deepseek-v4.1-flash`
  a dozen times. Same resolver, measured input.
- **That `Config` deserializes this shape.** `ext.rs:1054`'s
  `set_dir_replaces_an_existing_file_atomically_...` writes
  `default_model = "mock"\nextensions_dir = "old"` -- the identical two-key
  top-level document -- then calls `config_of(&text)`, which must deserialize
  the whole thing for its assertion to run. A passing test among the 1,114.

> **Running the binary would have tested the 05:52 build; the test tests the
> tree.** The tree is what runs after the rebuild, so being unable to execute
> pushed the evidence onto the thing that actually needed it.

### Still pending, and they close together

`eidolon serve` reads config at startup, so the running shim does not see the
key yet -- and that process is the stale 05:52 binary, which predates
`SummonError::NoDefaultModel` and would summon the `mock` fixture rather than
refuse. **Until the rebuild, the plain `eidolon` row in Open WebUI is still the
one not to click.** Both resolve in the same step. **S69** holds the acceptance
test.

## 2026-09-20 — UI navigation and square corners
Open WebUI remains the primary chat face. Eidolon-backed model selections map saved chat IDs to sessions/chats/<id>.eid. Direct model connections are not automatically Eidolon sessions. No installed WebUI tool/function named eidolon was found in the local tool/function tables; a separately invoked CLI run would be another session, not an activation step.

Implemented Credentials in the native WebUI Settings navigation, removed its sidebar row, installed square code/frame rules, and added content-hash skin URLs to avoid stale assets. Browser verified the Settings link and computed code/frame radius 0px.

Eidolon harness and operator pages now have return-to-WebUI links and Settings disclosures for Credentials. Built and browser verified using the isolated 120-second /jev test harness at 4577. Live server switch awaits operator answer; PID 31900 was not stopped.

Agents: navigation-001 (ollama:deepseek-v4.1-flash) completed 220s under 240s watchdog. navigation-review-001 completed35s under120s watchdog. Review had two wrong relative asset paths and missed CSS through gitignore: those findings were resolved by parent inspection and browser verification. Corrected test that confused a deliberate loopback navigation link with an asset fetch. Web lib262 and jev20 tests passed, page suite18 passed after correction; debug build, loader/jev JS syntax, launcher parsing passed. No rustfmt.

Running the new binary's peers command automatically refreshed previously untouched system tools/bash.rn and policy.rn per its startup migration; config.toml was not edited. This was reported by the executable, not a manual policy change. Live sessions reported idle. Further access-mode changes remain pending.

## 2026-09-20 — WebUI is the sole navigable chat interface
Operator rejected the standalone web consumer. GET / now returns303 to http://127.0.0.1:3000/ with no-store and no composer body. Operator pages retain Extensions/Automation/Settings and a single Chats link via /. The old CLI/TUI and underlying standalone APIs are not removed; Minerva's serve mode has no standalone driver, and its chat entry is WebUI. No WebUI session is created by opening the root redirect.

Independent Eidolon DeepSeek review webui-only-review-001 completed16s under90s watchdog. Removed duplicate WebUI navigation based on review. Tests:262 web library passed,19 HTTP page tests passed including root and query-string redirect and empty body; fresh debug build passed. Live server remains unchanged pending the already-issued restart question.

## 2026-09-20 — POSIX portability pass
Restored Unix IPC write timeout, propagate socket chmod errors, add Unix socket mode and stalled writer tests. Harnox keeps Unix0600 assertions while allowing Windows tests to compile; added atomic failure cleanup and fixed HOME/TCP fixture assumptions. Harnox197 all-feature library tests and all-feature/all-target clippy passed on Windows. Eidolon3 IPC tests passed on Windows. New Unix tests require native execution. Harnox CI matrix Linux/macOS/Windows; Eidolon POSIX workflow Linux/macOS uses declared upstream harnox v0.3.6 sibling. CI not yet run remotely. No POSIX certification or Linux/macOS success claimed. Agent execution timed out180s and was killed; parent completed work. Independent review completed54s. Live services unchanged.


### 2026-09-20 — Code-first Windows/Linux cleanup

Trimmed historical Rust doc blocks around Bash resolution, IPC and credential home selection, retaining actual platform constraints. Restricted current CI scope to Windows/Linux (broader POSIX/macOS deferred). Simplified Harnox home selection from a 12-line match body to a 9-line early-return body with one OS fallback call site; retained silent empty/unset overrides, invalid-override warning, absolute fallback validation and independent test expectations. Corrected the credential mode warning to cover group/other access, not just reads. Both home helper tests and all-feature/all-target clippy passed on Windows after the edit.

Actual ollama:deepseek-v4.1-flash runs: code-first-review-001 exited 0 in 12 seconds; deletion-audit-001 exited 0 in 28 seconds under a 120-second watchdog. The deletion audit's proposed nested match increased lines despite its claimed reduction; parent used the simpler early return and did not adopt its suggestion to reuse production logic in test expectations. Independent deletion review is recorded in logs/agents/deletion-review-001.*. WSL remains uninstalled and gh remains unauthenticated. No upstream PR exists; Linux execution, Windows IPC timeout/ACL fixes and focused patch extraction remain outstanding.

Independent deletion-review-001 exited 0 after 14 seconds (90-second watchdog); final report inspected. It verified the four home-selection branches and found no correctness issue or further safe deletion in the reviewed region. Review did not execute tests or inspect a before/after diff; parent test results and captured pre-edit body provide that evidence.


### 2026-09-20 — Git Bash takes priority over Windows WSL aliases

Observed Get-Command bash resolving to LocalAppData/Microsoft/WindowsApps/bash.exe, while live PID 31900 still uses the September 19 release. The newer source also searched PATH before Git discovery, so rebuilding alone would not have fixed this alias collision. Reordered Windows discovery: explicit EIDOLON_BASH, Bash beside PATH Git, known Git installation paths, then PATH. Linux retains PATH then POSIX defaults. Git absence still allows PATH Bash (including WSL); this is a native-Git preference, not WSL detection. Custom Bash takes precedence through EIDOLON_BASH.

All 21 shell tests passed, including the new alias-before-Git regression and actual command/timeout/background tests with the normal host environment. Debug CLI build passed. bash-priority-review-001 used actual ollama:deepseek-v4.1-flash, exited 0 in 29 seconds under a 90-second watchdog. Parent checked its unresolved questions: beside_git validates each candidate with is_file, and known locations include per-user Git. No live service restart or release replacement occurred; activation remains pending.


### 2026-09-20 — Authorized live activation

Operator authorized restart. Stopped old Eidolon PID31900, preserved its executable as target/release/eidolon.pre-bash-fix-20260920-212006.exe, and ran hoot up. It selected the tested target/debug/eidolon.exe; new serve PID44644. Open WebUI and model router kept running. Authenticated /v1/models returned 16 models; WebUI /health returned status true; enabled browser and jev services report up with their tools registered. Hoot now falls back to debug until a fresh release build exists. Updated watch-agent.ps1 to use the same release-then-debug discovery, so supervised agents still launch. Startup reported automatic refresh of untouched tools/bash.rn and policy.rn. No manual config or policy editing performed.


### 2026-09-20 — All WebUI chat models through Eidolon

Removed direct llama.cpp registration from Set-WebuiEnv; only http://127.0.0.1:8085/v1 remains, with authenticated session/task/edit headers moved to connection index 0. Missing shim token fails visibly instead of silently exposing inference-only chats. Task model is hoot:gpt-oss-20b, using Eidolon's stateless task handler. Live WebUI updated through its authenticated admin APIs, retaining the running processes. Read back connection and headers, and verified /api/models lists hoot-prefixed local models plus ollama-prefixed models, without bare local-model bypass entries. PowerShell parse passed. Actual model inference not exercised in this routing change. llama.cpp remains the downstream OpenAI-compatible provider, not a second WebUI connection.

### 2026-09-20 — WebUI serving status
Added a compact terminal-green status strip to the chat container: hoot serving, llama.cpp reachable, eidolon reachable. Browser probes run every 15 seconds with 3-second timeouts and no cache. These are cross-origin reachability checks, not model-readiness or authenticated health checks; the tooltip states the distinction. Installed loader/CSS assets and updated cache hashes. Browser verification showed both servers reachable. Hoot injects configured router and shim origins on future launches.


### 2026-09-21 — First native Linux validation through WSL2

Ubuntu 26.04.1, Rust 1.98.1. Harnox: 198 tests passed (one existing ignored doctest); strict Clippy passed all ten consumer feature combinations. Eidolon: locked workspace/all-target check passed; selected core/tools/swarm/claude suites totaled 379 passing tests and three ignored tests. Unix IPC mode and stalled-writer timeout regressions both passed. Test temp paths are Linux /tmp; build artifacts are in the Linux home cache. No source edits required. Unused test import and conch-parser future compatibility warnings remain. Tests used the modified sibling Harnox, not CI pinned upstream v0.3.6. Windows IPC deadline/access-control and PR extraction remain unresolved. linux-review-001 completed in 48 seconds with its report inspected; it violated the read-only-tool budget/no-shell brief with read-only commands, so actual parent-run logs are the evidence.


### 2026-09-21 — Windows IPC deadlines and pipe access control

Added ipc/windows_request.rs and ipc/windows_security.rs, integrated into core IPC. Requests now bound connect/write/read under one Tokio deadline on a joined worker, sharing existing framing. Server pipes get a protected current-token-user-only DACL on initial and replacement instances, reject remote clients and preserve first-instance exclusivity. Windows: 11 IPC tests plus core/swarm/claude suites pass; strict all-target core Clippy passes. Linux: core/swarm/claude suites pass (297 tests). No live deployment. Actual DeepSeek deadline agent completed133s; ACL implementation agent timed out180s and was killed, parent implemented; broad review timed out90s and was killed, narrowed ACL review completed13s. All final logs/status inspected. PR extraction/dependency alignment remain; no client server-identity authentication added.


### 2026-09-22 — Open WebUI 0.11.4, and six chats that died without settling

Pulled open-webui 0.11.3 to 0.11.4 (`uv tool install open-webui==0.11.4 --python 3.12`). The first attempt ran with the server up: Windows held the tool env's Scripts directory open, uv had already removed site-packages, and the running server fell to 404. Repaired by stopping that one process (identity checked first — `open-webui.exe serve` on :3000; the shim, pid 45692, was never touched), reinstalling, and relaunching through `hoot webui`, which re-applies the integration at every launch. One real 0.11.4 break: its `config.py:2161` iterates `WEBUI_BANNERS`, and the launcher wrote a bare object because `$banner | ConvertTo-Json` unwraps a one-element array — so the server refused to start. Fixed in `bin/hoot.ps1` (`-InputObject @($banner)`), measured in PowerShell both ways. The operator then reported that nothing in Authentication had changed: the installed `frontend/index.html` still carried the old `?v=` hashes, so the browser never fetched the new loader. Rewritten the way `Set-WebuiEnv` does (`loader.js?v=5A40964F3F9C`, `custom.css?v=5E7C56EB4BF5`). Two anchors were silently absent in 0.11.4 — `a[href="/calendar"]`, which seeded the Extensions row, and `tab-admin-authentication`, which seeded the credentials panel — both fixed in `webui/build-loader.py` and regenerated into `loader.js`; the Extensions row was then seen in the live DOM, while the panel's appearance was not verified (browser clicks failed actionability and both known settings routes 404 or redirect). Verified by content: `main.py:3023-3026` carries the three minerva include lines, the three routers are copied, and the served loader, css and workspace files are byte-identical to the repo. Operator data intact (`.webui/webui.db`, 352 config rows, 13 chats).

Separately, six swarm chats were found wedged: `08d4`, `2be7-1`, `67d3`, `6c78`, `6c78-1` and `92df`, all chats inside pid 45692, all flagged `busy` while their presence metas had not been written for 163-694 minutes and no session log on the box had been written for half an hour except one. Three of the four readable logs end on an `AssistantMessage` with no settle after it, the fourth on a bare `ToolResult`; no unanswered question appears in any of them, and the shim held no outbound provider connection while a working session did. So turns died mid-flight and nothing clears `busy` — the roster advertises work that stopped hours ago. Two roster ids also share one journal (the crc32-corrupt one in S76), which is a plausible cause of that corruption. Only a shim restart clears them; summoning does not.

Two agents were summoned as separate processes (`eidolon run -m ollama:deepseek-v4.1-flash <brief>`) to carry the stalled work. The first, for the documentation rows above, ran twenty minutes, made 113 model calls, read 74,667 fresh and 10,014,464 cache-read input tokens at 93,389 output ($3.14 at full-price input, $0.20 with the cache discount), wrote nothing, and was killed; its task was done by hand here instead. The second, for the caret's glyph independence and the console output code page, measured the console state itself (code page 437, set to 65001, restored) and created `crates/tui/src/console.rs`. A launch with a shell `&` inside a foreground call died within seconds — this harness kills the process group when the call returns — and was relaunched detached.

And the subagent strip's first three commits landed on `minerva`: `1a3dd92` (roster to rows, the strip's state, four commands, six tests), `a60ab70` (the operator's rule for `j`/`k` at the bottom of the scroll, break-tested by inverting the guard), `11fd783` (the wheel never asks the strip). Nine tests, no terminal and no registry needed.

### 2026-09-23 — The spawn tool, and a role that picks the model

An agent asked to "summon a subagent" had nothing to reach for. `eidolon-294e` (asked "can you summon a sub agent to research on whales") spent twenty tool calls reading the tree to establish exactly that: it grepped `EIDOLON_PARENT`, found the reader and no writer, found no `/api/subagent` routes, put the choice to the operator through its own `choices_user`, and was then left to hand-roll a launch. It spawned three children that way and all three died. Two are 1,239-byte logs with `start` + `persona` and `settled: true` — an `eidolon chat` handed a closed stdin quits before it reads anything — and the third ran a real turn ("plumbing check", `bash` then `grep`) whose journal stops mid-turn, because the foreground shell path is `kill_on_drop(true)` with `stdin(Stdio::null())` (`eidolon/crates/tools/src/shell.rs:103`). A child started through `bash` dies with the call that made it. None of the three ever registered, so no strip could have shown them however the display was wired.

`spawn` now exists (`2c23413` on `minerva`): a built-in registered beside `peers` and `send` on a session that is registered among its peers, gated like any tool, launching `eidolon run` detached on this same binary with `EIDOLON_PARENT` and `EIDOLON_ROLE` in its environment. Proven live twice. `bonsai2-7041` counted 7 `.rs` files under `eidolon/crates/swarm` (15 s, 68.3k in / 2.6k out, `$0.0025`) and corrected the path in my own task, which pointed at `crates/swarm` where no `crates/` exists at the repo root. `bonsai2-7ebf` counted 6,907 lines across 7 files under `eidolon/crates/rune/src` and brought its own cross-checks plus a stated caveat (it tested that every file ends in a newline before trusting `wc -l`). `bonsai2-c7b0` was launched by hand to isolate the environment path, and its roster entry reads `parent bonsai2-a753, role research` off disk. One level only: a session that has a parent is refused, so the tree stops at the children of a session the operator started.

Two things this exposed, neither fixed. **The report path assumes a parent that outlives the turn.** Both test parents were `eidolon run` — one turn, then exit — so by the time each child finished, the id it had been handed named nothing live; `7041` and `7ebf` both hit that on `send`, fell back to `peers`, and broadcast, which is how their reports arrived at a session that had not asked for them. A TUI or chat parent does not have this problem, and the tool's wording does not say so yet. **And a finished child is invisible.** The strip reads the roster; a session that has exited is swept out of it; so a subagent that reports and stops — the lifecycle the operator ruled on — can never be a row. The strip answers "who is working for me now", and nothing answers "who worked".

Tiers are data now. `[subagent.tiers]` maps a role to a model key, shipped with the operator's ruling of this date — most of the ladder on `ollama:deepseek-v4.1-flash`, `decisioning` on `ollama:glm-5.3-flash`, deepseek pro cut by name — overridable one row at a time, and a role nobody mapped is refused *by name*, with the roles that exist, rather than quietly answered with the parent's model. A role draws its own glyph beside the row in the strip, and the `↳` now follows the roster's parent instead of the name of a log, which deleted the `subagent-<hash16>` convention and the two tests that pinned it. **Not built: the webui page that edits that table**, which is what the operator asked for, and the four other strip items from the same message — selection style, per-row time and tokens, `k` falling through past the strip's top, and Enter to enter a session (which must be an attach, never the `:sessions` reopen: one journal with two writers is how `chats/5ae3892b-….eid` was corrupted).

One self-inflicted error, disclosed. A roster poll written to watch a child register printed nothing for sixty seconds, which reads as evidence that no child was ever in the roster. It was the poll: the glob `"$TEMP"/eidolon/*/` does not expand in this shell when `TEMP` carries backslashes, so the loop ran over a literal string and every `python -c` inside it died into `2>/dev/null`. Redone with `cd "$TEMP/eidolon"` first; the entries above are what it then showed.

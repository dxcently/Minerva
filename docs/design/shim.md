# The shim: Open WebUI chats summon eidolon sessions

Design A1, 2026-09-18. Written from the code — Open WebUI 0.11.3 as
installed under `uv tool`, and the eidolon tree as of the Windows port.
Every claim about what the app sends or accepts cites the file it was read
in. An executor implements from this document; a reviewer checks the
implementation against it, not against a conversation.

## The decision

Open WebUI is the face and eidolon is the agent ([Decisions](../Decisions.md),
2026-09-18). A chat in the app is an eidolon session on the box, the way a
chat in the Codex app is a `codex` session: the app draws it, the harness
runs it, and the join is an **OpenAI-compatible shim that eidolon serves**,
registered in the app as one more OpenAI connection beside the llama.cpp
router.

The shim is a *consumer* of the core in exactly the sense `crates/web` is
one, and it keeps the same three promises that crate's module doc makes:

- **One chokepoint, still.** The shim never calls `Dispatcher::dispatch`,
  `execute` or `adjudicate`. It hands prompts to `Agent::run_turn` /
  `steer` / `continue_turn` through a per-chat driver, and it is the
  `UserIo` the chat's one `Dispatcher` asks its questions of. A policy
  question is still asked by the gate and answered by a person; the shim
  only changes the room the question is asked in.
- **The log is truth.** A chat's state is its `.eid` log. The shim keeps
  nothing durable of its own — no index, no sidecar database — and nothing
  it renders is inferred from what it rendered before.
- **The page is a view.** Open WebUI's transcript is a projection of the
  session, not the session. When the two disagree, the session wins and the
  disagreement is said out loud.

What is new is that there are *many* sessions in one process, summoned
lazily by chat id, and that the wire between the face and the agent is not
one the harness designed: the OpenAI chat-completions protocol has no
notion of a tool running on the box, a question waiting for a person, or a
turn that outlives a request. Most of this document is about how each of
those is carried across a wire that has no word for it.

```
Open WebUI backend ──POST /v1/chat/completions (SSE)──▶ eidolon serve
   (aiohttp, one request per turn,  X-OpenWebUI-Chat-Id: …)      │
                                                       chats/<id>.eid
   ◀── data: {"choices":[{"delta":{"content":…}}]} ──   Agent ── Dispatcher ── policy gate
   ◀── data: {"event":{"type":"status",…}} ───────────     │            │
                                                            └── ChatUser::approve  (parks the turn,
                                                                 renders the question, ends the stream;
                                                                 the next request answers it)
```

The topology matters for section 5: the browser talks to the Open WebUI
*backend* over socket.io, and the backend consumes the shim's stream in a
background task (`utils/middleware.py` line 4253, `create_task(...)`). A
browser tab closing does not touch the shim's connection. Only the Stop
button (`POST /api/tasks/chat/{id}/stop` → `task.cancel()` →
`response.body_iterator.aclose()`, `middleware.py` 6280–6290), a backend
restart, or a configured client timeout does.

### What the app sends, and what it will accept

Facts the rest of this document leans on, each read in Open WebUI 0.11.3
(`%APPDATA%\uv\tools\open-webui\Lib\site-packages\open_webui\`):

| fact | where |
|---|---|
| `X-OpenWebUI-Chat-Id` is sent only when `ENABLE_FORWARD_USER_INFO_HEADERS=True` and `metadata.chat_id` is set; beside it `X-OpenWebUI-User-Name/-Id/-Email/-Role` (or one `X-OpenWebUI-User-Jwt` when a JWT secret is configured) | `routers/openai.py` 178–181, `utils/headers.py` 52–75 |
| `X-OpenWebUI-Message-Id` is sent to *tool servers* only, never on a chat completion — the shim cannot rely on it | `env.py` 982; its one use is `utils/tools.py` 184 |
| A connection may carry **custom headers**, templated per request from `{{CHAT_ID}}`, `{{MESSAGE_ID}}`, `{{USER_MESSAGE_ID}}`, `{{USER_MESSAGE_PARENT_ID}}`, `{{TASK}}`, `{{USER_NAME}}` … | `routers/openai.py` 216–218, `utils/headers.py` 110–152 |
| Those metadata fields are populated on every chat turn from the frontend's body (`id`, `user_message.{id,parentId}`) | `main.py` 1208, 1243–1249 |
| Connections can be pre-configured from the environment: `OPENAI_API_BASE_URLS`, `OPENAI_API_KEYS`, `OPENAI_API_CONFIGS` (JSON, keyed by connection index, with `headers`, `tags`, `prefix_id`, `enable`) | `config.py` 340–357 |
| Title, tags, follow-up, query and autocomplete generation go through the **same** `/v1/chat/completions`, with the same chat-id header, `stream: false`, one user message built from a template that begins `### Task:`, and `metadata.task` set (so `{{TASK}}` is non-empty for them and empty for chat turns) | `routers/tasks.py` 184–204, `config.py` 2224–2353 |
| They go to the chat's own model unless `TASK_MODEL_EXTERNAL` names another model the app lists | `utils/task.py` 16–27 |
| The stream consumer honours `choices[0].delta.content`, `delta.reasoning_content` (also `reasoning`, `thinking`), a top-level `usage`, a top-level `error` on a chunk with no `choices`, and a top-level `event` which it forwards to the UI as one of its own socket events (`status`, `notification`, `source`, `message`, `replace`, `chat:title` …) | `utils/middleware.py` 4852, 4960–4990, 5170–5215; frontend chunk `zKJlHFgk.js` |
| `delta.tool_calls` is collected into `function_call` items and, when the stream ends, run through the app's **own** tool-execution loop | `utils/middleware.py` 5020–5130, 5488–5560 |
| `<details …>…</details>` in message content is a first-class markdown token with `summary` and `attributes`, rendered as a collapsible | frontend chunk `CHq18Uto.js` (`type:"details"`) |
| Non-SSE error bodies are returned to the client with their status; SSE bodies with status ≥ 400 are read and returned as JSON errors | `routers/openai.py` 1635–1720 |
| Client timeouts default to none (`AIOHTTP_CLIENT_TIMEOUT` unset → `None`, `AIOHTTP_CLIENT_STREAM_IDLE_TIMEOUT` unset → `None`) | `env.py` 593–610, `utils/session_pool.py` 45–49 |
| Chat ids are UUID4 strings for saved chats. **Three** prefixes mark the unsaved kinds, not two: `temporary:<socket>`, `local:<socket>` (its legacy spelling, grouped with it in `TEMPORARY_CHAT_ID_PREFIXES`) and `channel:<id>`; `NON_SAVED_CHAT_ID_PREFIXES` is the union | `utils/chat_id.py` 4-12 |
| `/v1/models` entries are merged by `id`; `name` is honoured; unknown fields are kept under `model.openai`; the list is cached for `MODELS_CACHE_TTL` | `routers/openai.py` 818–860 |

The consequence that shapes the most: **the shim must never emit
`tool_calls`.** The app would take them as its own function calls, try to
run them against tools it does not have, and re-request. Every tool call
eidolon makes is rendered as *text and events*, never as the wire's tool
vocabulary (section 3).

---

## 1. Chat id → session

**Choice.** The chat id *is* the file name. A chat `c9e4…` lives at
`<sessions_dir>\chats\c9e4….eid`; the mapping is a directory listing, the
way the swarm's roster is (`crates/swarm`: "the directory is the
registry"). There is no index file to write before or after the log, so
there is nothing to get out of order, and a restarted binary finds every
chat by looking.

The id is sanitised on the way to a file name, because the two unsaved
kinds carry a `:` that Windows refuses: every byte outside `[A-Za-z0-9._-]`
becomes `_`, and when anything was replaced an FNV-1a 32-bit hash of the
original is appended (`temporary_ab12cd-7f3a9c01.eid`), so two ids that
sanitise alike cannot share a log. The same hashing idiom the named-pipe
port uses (`eidolon_core::ipc`). Ids longer than 128 bytes, and requests
with no chat-id header at all, are handled below.

**Rejected.** An index (`chats.json`, or a table in the log): one more
durable thing to keep consistent with the logs, and the one that would be
wrong after a crash between "create the log" and "write the index". A
`Note` record inside each log naming its chat, found by scanning every log
on lookup: correct, but a scan of the sessions directory per request, and
the directory holds every TUI session ever run too. The file name costs
nothing and cannot drift.

### Lifecycle

**First sight.** No file → create it (`Session::create` through the same
path `Config::create_session` takes, but with the chosen name rather than
the millisecond), then journal one `RecordKind::Note`:
`owui-chat id=<chat id> user=<X-OpenWebUI-User-Name or "unknown">`. The
user headers are attribution, nothing more (section 6). The model the
session is born with is the request's `model` resolved per section 2, and
its `cwd` is the serve process's (`[serve] cwd`, default the launch
directory). Then the session is assembled — `build()` in
`crates/cli/src/main.rs`, unchanged, once per chat — into an `Agent` with
its own `Dispatcher`, its own `EventBus`, its own swarm `Presence` (so
chats are peers: `eidolon peers` lists them, and one chat can `send` to
another), and the shim's own `UserIo` (section 4) in the seat `WebUser`
takes for `eidolon web`.

Assembly is lazy and per chat, not eager at boot: a process that opened
every log under `chats\` at startup would spend a `build()` per chat the
operator ever had, and would resume turns nobody asked to resume. A chat
costs its `build()` the first time it is addressed after a boot — a few
tens of milliseconds: the policy table compiles on a thread, the catalog
comes from the cache, and an extension's service is machine-scoped, so the
ninth chat adopts the jev process the first one started.

**Resume after the binary restarted.** File exists, nothing in memory →
`Session::open` (the crc-checked replay the log already does), assemble as
above. If `Session::needs_finishing()` is true — the last turn was cut by
the restart, or is parked on a question the restart forgot — the shim
*finishes it first* and only then runs the new message, and the window
that carried the new message shows both, with a separator line between
them (section 3). This is the resume rule `eidolon web` and the TUI
already follow (`Handle::finish_unsettled`), read at request time instead
of at boot. A question that was pending before the restart is asked again
by the gate on the continuation, because nothing was journaled for it (the
`PolicyVerdict` is written only when there is an answer), and the held
message answers it exactly as it would have live — section 4 makes the
restart case indistinguishable from the live case on purpose.

**Deleted in the app.** The app tells its upstreams nothing on delete; no
signal exists to act on. The log stays. It is the operator's record of what
ran on the box, and a policy-gated agent's audit trail is not something a
click in a chat list should be able to erase. What *does* go is the memory:
a chat with no request for `[serve] idle_minutes` (default 60), no turn
running and no question parked, is evicted from the process — the `Agent`
dropped, the file closed, the presence deregistered — and summoned again if
the id ever comes back (it will not; ids are UUIDs). `eidolon sessions`
lists the orphans; deleting them is the operator's, by hand.

**Two requests for one chat id at once.** One chat, one turn at a time —
the invariant the driver `Handle` in `crates/web/src/driver.rs` already
keeps for one session, kept here per chat. A request that finds its chat's
turn running does not start another and does not get a 409. It **joins**:
its user text becomes `Agent::steer` (read at the running turn's next safe
point, which is what the TUI does with a message typed mid-turn), its
response stream becomes the turn's window, and the previous window is
closed cleanly by the shim (a final `finish_reason: "stop"` chunk, so the
app marks that bubble done). The words are not lost, the turn is not
duplicated, and the newer bubble is where the rest of the answer appears.
Section 4 needs exactly this machinery — a request continuing a turn
already running — so the concurrent case costs nothing extra.

The app's own UI never produces the concurrent case: the send button is a
Stop button while a reply streams. Two tabs, two devices, or an API client
do. Rejected: a 409 — it would break the approval protocol, and the app
shows a 409 as a red error on a bubble the user did not think they were
racing.

**No chat id, or a task request.** A request without `X-OpenWebUI-Chat-Id`
(a `curl`, a client that does not forward it) and a request the app marks
as a *task* (section 3, "task requests") run **stateless**: the messages go
to the named model as one plain completion with no tools offered and no
journal, and nothing is created. A session with no name to find it by
again would be a log nobody can resume; a title-generation prompt journaled
as a turn would be a user message the person never sent. Rejected:
refusing such requests — the app's title/tag/follow-up generation would
then error on every chat that runs on eidolon.

**One writer.** `SessionLog::open` takes no lock (`session/log.rs` 154–158;
the Windows default share mode lets a second process open the same file
for writing). The shim is the single writer of `chats\*.eid` for as long as
it runs; `eidolon tui --session chats\<id>.eid` beside a running shim is
the same double-writer hazard two TUIs on one log have today, not a new
one, and it is stated rather than guarded in v1.

### Files

- `crates/web/src/shim/path.rs` — `chat_path(sessions_dir, chat_id) ->
  PathBuf`, the sanitiser and its hash suffix; the id length cap.
- `crates/web/src/shim/mod.rs` — `Shim { chats: Mutex<HashMap<String,
  Arc<ChatSession>>> }`, `summon(&self, chat_id, model) ->
  Arc<ChatSession>` (open-or-create, assemble, insert), the eviction sweep.
- `crates/web/src/shim/chat.rs` — `ChatSession { agent, driver: Handle,
  user: Arc<ChatUser>, window: Mutex<Option<Window>>, last_used }`,
  `join(...)`.
> **Amendment, 2026-09-19 — three names shipped differently.** Verified
> against source, after this document sent an agent to the wrong file:
>
> - `SessionFactory` is implemented by **`crates/cli/src/serve_host.rs`**
>   (`impl SessionFactory for ServeHost`, line 472), **not** by
>   `crates/cli/src/main.rs`. Every reference below to `main.rs` as the
>   implementor should be read as `serve_host.rs`.
> - The design's **`CliFactory`** shipped as **`ServeHost`**, which also
>   carries `OperatorHost` and `JevHost` — one type behind three traits
>   rather than one per surface.
> - The design's **`resolve_backend(key)`** shipped as **`resolve(key)`**
>   (`crates/web/src/lib.rs:269`), returning `Resolved`.
>
> The design is left as written; this is what is true now.

- `crates/web/src/lib.rs` — the `SessionFactory` trait the shim assembles
  through (below, "Files and functions"), replacing the untyped
  `WebOptions::catalog` field.
- `crates/cli/src/main.rs` — `impl SessionFactory` over `build()`;
  `Cmd::Serve`.

### Proof

- `chat_path`: a UUID maps to `chats\<uuid>.eid` verbatim;
  `temporary:abc` maps to `temporary_abc-<hash>.eid`; two ids differing
  only in the replaced byte map to different files; a 129-byte id is
  refused (400); `..` and separators cannot escape `chats\`.
- First request for a new id creates the file, writes `SessionStart` and
  the `owui-chat` note, and the response streams a turn.
- Second request for the same id after the `Shim` is dropped and rebuilt
  (simulated restart) opens the same file and the branch continues.
- A log whose last turn is unsettled (write `UserMessage` with no reply,
  then reopen): the next request's window carries the continuation, a
  separator, then the new turn, and the journal shows two `TurnSettled`.
- Two concurrent requests for one id with a slow scripted provider: one
  `run_turn` on the agent, the second request's text lands as a
  `UserMessage` record mid-turn (a steer), the first stream ends with
  `finish_reason: "stop"`, the second stream carries the remainder.
- A request with no chat-id header journals nothing under `chats\`.
- An idle chat is evicted after `idle_minutes`; a parked one is not.

---

## 2. What `/v1/models` advertises

**Choice.** One entry per model the catalog can *reach*, keyed exactly as
eidolon keys them, plus one umbrella:

```json
{"object":"list","data":[
  {"id":"eidolon","object":"model","owned_by":"eidolon",
   "name":"eidolon (default: hoot:bonsai-8b)"},
  {"id":"hoot:bonsai-8b","object":"model","owned_by":"eidolon",
   "name":"eidolon · Bonsai 8B 1-bit (local, ~10 tok/s)",
   "eidolon":{"provider":"hoot","context":16384,"reasoning":true,"vision":false}},
  {"id":"hoot:Qwen3.8-27B-UD-Q4_K_M","object":"model","owned_by":"eidolon",
   "name":"eidolon · Qwen3.8 27B (local, MTP ~3 tok/s)", "eidolon":{…}}
]}
```

The `model` field is the one knob the app gives a chat, so the picker is
where the model is chosen — and since every entry is an eidolon session
regardless of which model it runs on, the picker is really choosing *the
agent, on this model*. The `name` says so (`eidolon · …`), because the
router connection lists the same models under their bare names and a
picker with two identical "Bonsai 8B" rows would be a coin toss. The
`eidolon` object is passthrough metadata the app keeps under
`model.openai`; nothing reads it today, and it costs nothing to say what
the catalog knows.

The list is `Catalog::models()` filtered by `Catalog::readiness(key)`
(`crates/providers/src/readiness.rs`): a model is advertised when its
state is `NoCredentialNeeded`, `Present(_)` or `DriverExecutable`, and
skipped when `Unavailable`. Readiness is structural and free — it asks the
secret store *whether* a key exists and never *what* it is, claims no pool
lane, touches no network — and it is the same answer the TUI's picker
draws beside each row. A dark provider (every cloud one on this box as of
2026-09-18) is therefore not in the app's picker at all, rather than a row
that fails with a 401 after the person committed to it. `mock` is never
advertised: it is a test fixture.

**The umbrella.** `eidolon` means *the agent, on whatever it is on*. On
first sight of a chat it resolves to `default_model` from `config.toml`
(the same rule `open_or_create` applies to a fresh TUI session). On an
existing chat it changes nothing — the session keeps the model it runs.
Automations and quick starts that do not care which model pick this one.

**Switching.** A request naming a *specific* key on an existing chat that
runs a different one is a switch: `Catalog::backend_for(key, …)`,
`Agent::set_backend`, `Agent::set_model` — the three calls the TUI's
`space m` makes (`crates/tui/src/app.rs` 828–832) — journaled as
`ModelChanged`, applied between turns (a switch arriving for a running turn
is applied before the *next* turn; the running one keeps the backend it
started with, per `Agent::set_backend`'s contract). A key that does not
resolve is a 404 `model_not_found` in the OpenAI error shape, before any
session is created or touched.

**Rejected.** One entry only, with the model chosen by a chat command or a
config default: the app's picker is the natural UI and the only per-chat
control the app offers; a chat command would be an in-band protocol the
model can also see and imitate. One entry per model *without* the
umbrella: then nothing in the app means "the agent" without also meaning a
model, and an Automation that just wants eidolon has to name a model it
does not care about. Advertising the whole catalog regardless of
readiness: a picker full of rows that cannot run.

### Files

- `crates/web/src/shim/models.rs` — `advertised(factory) -> Vec<ModelRow>`
  and its JSON; `resolve_requested(model: &str) -> Requested::{Umbrella,
  Key(String)}`.
- `crates/web/src/lib.rs` — `SessionFactory::models()` and
  `SessionFactory::resolve_backend(key)`; `crates/cli/src/main.rs`
  implements both over `Catalog`.
- `crates/web/src/serve.rs` — the `GET /v1/models` arm.

### Proof

- With a catalog holding a ready `hoot` provider and a dark `deepseek`,
  `/v1/models` lists `eidolon` and the `hoot:*` keys and no `deepseek:*`.
- `mock` is not listed even when the catalog can resolve it.
- A new chat requested as `eidolon` is born with `default_model`'s key in
  `SessionStart`; requested as `hoot:bonsai-8b` it is born with that.
- A second request on that chat naming `hoot:Qwen3.8-27B-UD-Q4_K_M`
  journals `ModelChanged` before the turn's `UserMessage`; a second request
  naming `eidolon` journals nothing.
- An unknown key answers 404 with `{"error":{"code":"model_not_found"}}`
  and creates no file.

---

## 3. `/v1/chat/completions`

### Whose history wins

**Choice.** The session's. The request's `messages` array is read for
three things only: the **last user message** (what to run), the **system
message** (what to remember), and a **count** (what to check). Everything
else in it — every earlier user message, every assistant message, every
rendered tool block the app is sending back — is the app's copy of what the
session already holds, and the session's copy is the one the model was
actually given.

**Rejected.** Replacing the session's branch with the request's history on
every turn — the stateless-proxy reading of the protocol. It would throw
away thinking signatures, tool results, the compaction summary and the
policy record on every request, and re-send the app's *rendered* transcript
(tool blocks as prose) to a model that produced structured calls. It is
also what would make an approval impossible: a turn that must survive
between two requests has to live somewhere the request does not.

### Divergence: edit, regenerate, continue

The app keeps a tree of messages per chat and sends the chain to the one
being answered. Editing a message or regenerating a reply sends a *shorter*
chain, or one whose last message is new. eidolon's log is a tree too
(`session/mod.rs`, "Records and the tree"): a fork is moving the head to an
older record and appending. So divergence is a fork, and the only question
is *where*.

**Choice.** Ask the app where. Its metadata names the user message being
answered and that message's parent (the previous reply bubble), and a
connection's custom headers can carry both (`{{USER_MESSAGE_ID}}`,
`{{USER_MESSAGE_PARENT_ID}}`, `{{MESSAGE_ID}}`). The registration
contract (below) sets them:

```
X-Eidolon-User-Message-Id: {{USER_MESSAGE_ID}}
X-Eidolon-Parent-Id:       {{USER_MESSAGE_PARENT_ID}}
X-Eidolon-Message-Id:      {{MESSAGE_ID}}
```

Before each turn it opens, the shim journals one `RecordKind::Note`:
`owui-turn user=<user message id> assistant=<message id>` — the app's ids
for the bubble being answered and the bubble being written. On a request,
with `P` the parent id:

1. `P` is the `assistant` of the **last** `owui-turn` note on the branch,
   or of the most recent window the shim opened on this chat (held in
   memory; a parked turn spans several bubbles, and only its first has a
   note) → **append**: the ordinary next turn, or an answer/steer into a
   turn still running (section 4).
2. `P` is the `assistant` of an **older** note *j* → **rewind**: cancel a
   running turn if there is one and wait for it to end, then
   `Agent::fork_at(next.parent)` where `next` is the note after *j* on the
   branch, then run the last user message as a new turn from there.
   Regenerate and edit both land here: a regenerate re-sends the same user
   id with the same parent, an edit sends a new user id with the same
   parent, and in both the parent is the reply *before* the one being
   redone. (A regenerate of a turn's *first* reply forks to just before
   that turn's note; the app's "continue response" carries the same ids as
   a regenerate and is treated as one.)
3. `P` is empty and the branch already has a turn → the first message is
   being redone → `fork_at` the `SessionStart` record.
4. `P` is unknown — the header is absent (a hand-registered connection, a
   `curl`), or names a bubble the shim never opened → **append**, and if
   the request's count of user messages is not `notes + 1`, the reply
   begins with one italic line: *(history diverged: the session has N
   turns and the app sent M messages; continuing from the session's own
   history)*. Without the ids the shim cannot place a fork, and guessing
   from text would fork on every RAG injection, since the app rewrites the
   last user message with `<context>` blocks when files are attached and
   sends earlier ones as stored.

`Agent::fork_at` already re-journals `ModelChanged` when the branch below
the fork ran a different model, so a rewind across a switch stays
consistent. Everything on the abandoned branch stays in the log; `eidolon
log` shows the tree.

**Rejected.** Comparing user-message texts to find the common prefix: the
RAG rewrite above, plus steers and peer messages journaled mid-turn, make
"the k-th user message" ambiguous on both sides. Counting turn-opening
records as the sole rule: correct for the app's own flows and fragile in
exactly the cases the ids resolve for free. Forging the missing assistant
records from the request when a foreign client sends a history the session
never saw: the log would then say the model said things it never said
here. The last user message runs, and the notice says the rest was not.

### The system message

The app sends a leading `{"role":"system"}` when a model or the user has a
system prompt configured. eidolon's own system prompt is the harness's
(`AgentConfig.system`); the app's is *about this conversation*, which is
what `RecordKind::SessionNote` is for — "a standing note the operator has
attached to this session, injected into every turn's system prompt". So:
when the request's system text differs from `Session::session_note()`, the
shim calls `Session::set_session_note` before opening the turn (empty
clears). Applied only when opening a turn, never into a turn in flight.

### Images

`content` parts of type `image_url` whose URL is a `data:` URI become
`ContentBlock::image(media_type, base64, None)`. An `http(s)` URL is not
fetched: a fetch the model did not ask for is a fetch the gate did not see.
It is dropped and the reply notes it. Text parts are joined with newlines.

### Task requests

A request is a *task* — the app's title, tags, follow-up, query, image
prompt, autocomplete or emoji generation — when `X-Eidolon-Task` is
non-empty (the registration contract sets it to `{{TASK}}`, which the app
fills for tasks and leaves empty for chat turns). As a backstop for a
connection registered without the header, a request whose last user
message begins with `### Task:` and whose `stream` is false is treated the
same way; that prefix opens every one of the app's default templates
(`config.py` 2224–2353) and a custom template loses the backstop but not
the header.

A task runs **stateless** (section 1): the named model (umbrella → default),
`Provider::stream` on the request's messages as given, no tools offered,
nothing journaled, the answer returned in the requested mode. Not through
an `Agent` — there is no session to run it in — and not a second policy
path, because no tool can run. The router's models answer these faster
without the shim at all, so the registration contract also sets
`TASK_MODEL_EXTERNAL` to a router model; the shim's handling is what keeps
a chat working when that is not set.

### Streaming: eidolon events to OpenAI chunks

Every chunk is `{"id":"chatcmpl-<chat id, first 8>-<record id>",
"object":"chat.completion.chunk","created":<unix>,"model":"<key>",
"choices":[{"index":0,"delta":{…},"finish_reason":null}]}` unless stated.
The first chunk carries `delta: {"role":"assistant","content":""}`. The
stream ends with a chunk whose `finish_reason` is set, then `data: [DONE]`.

| bus event | on the wire |
|---|---|
| `MessageStart` | nothing |
| `TextDelta(s)` | `delta.content = s` |
| `ThinkingDelta(s)` | `delta.reasoning_content = s` — the app draws a collapsible "Thinking" block with a duration |
| `ToolUseStart`, `ToolInputDelta` | nothing — arguments are shown whole when the call starts |
| `ToolCallStarted(call)` | `{"event":{"type":"status","data":{"action":"eidolon","description":"<name> · <one-line input>","done":false}}}` — the app's live progress line, the one it draws for its own web search |
| `ToolCallFinished{call,output}` | the same `status` with `"done":true`, then `delta.content` = the **tool block** below |
| `AskUser{prompt}` | nothing — the bus saying a question was asked; the shim's own `approve` (section 4) is what renders it, the same split `crates/web` keeps between `ask-user` and `ask` |
| `PolicyVerdict{Refused}` | `delta.content` = `\n> policy refused \`<tool>\`: <reason>\n` |
| `PolicyVerdict{Judged, note}` | `delta.content` = `\n> ` + `policy::verdict_note(...)` + `\n` — the one outcome that is invisible otherwise, per that function's doc |
| `PolicyVerdict{Approved | Declined | Yolo}` | nothing — the question block and the tool block already show it |
| `TurnBudget{calls_left}` | `delta.content` = `\n_(wrapping up: <n> calls left)_\n` |
| `Compacted{replaced_messages}` | `delta.content` = `\n_(context compacted: <n> messages summarised)_\n` |
| `PeerMessage{from,text}` | `delta.content` = `\n> **<from>:** <text>\n` — the model is about to read it, so the person should see it too |
| `Error(s)` | `delta.content` = `\n> ⚠ <s>\n` — not the error chunk: `Event::Error` is published for recoverable things too (a forced re-ask that failed and settled anyway), and the turn task's own `Err` is what ends a stream as an error |
| `ContextSize`, `Queued`, `CommandResults`, `Quiesced`, `UserMessage`, `AssistantMessage` | nothing — the app drew the user bubble itself; the assistant message's text already streamed |
| `TurnSettled{usage}` | final chunk: `delta: {}`, `finish_reason: "stop"`, top-level `usage: {prompt_tokens, completion_tokens, total_tokens, prompt_tokens_details: {cached_tokens}}` — the app merges it into the message's usage; then `[DONE]` |
| `Cancelled` | final chunk with `finish_reason: "stop"` after `delta.content` = `\n_(stopped)_\n`, then `[DONE]` (when the window is still open — after the app's own Stop it is not) |
| turn task returns `Exhausted` | the `Error` line above has already streamed; final chunk `finish_reason: "length"` |
| turn task returns `Err(e)` | a chunk with no `choices`: `{"error":{"message":"<e>","type":"eidolon","code":"turn_failed"}}`, then `[DONE]` — the app saves it as the bubble's error and shows it red |

A window subscribes to the chat's bus **before** the turn is started or
joined, the ordering `crates/web/src/stream.rs` exists to get right. Bus
lag (`RecvError::Lagged`) is one italic line *(output lagged; N events
dropped)* and the stream carries on; unlike the page there is no
hello-and-replay to reconnect into, and the journal is complete either
way.

**Keepalive.** Every 15 s of silence the window writes an SSE comment line
(`: keepalive`). The app's client has no idle-read timeout by default, but
`AIOHTTP_CLIENT_STREAM_IDLE_TIMEOUT` is one environment variable away from
cutting a two-minute build in half, and a comment costs nothing.

**Yolo.** When the serve process was started with `--yolo`, every reply
begins with the line `_(yolo: every question is answered yes; refusals
still refuse)_`. The TUI shows the posture on its status line
continuously (`crates/core/src/policy.rs`, `verdict_note`'s reasoning);
a chat has no status line, so the reply's first line is where it says so.

### What a tool call looks like to a person watching

While it runs: the app's status line, `bash · cargo test --workspace`,
with its spinner. When it finishes, one collapsible block in the reply:

```markdown
<details type="eidolon.tool" done="true" name="bash" ok="true" ms="2140">
<summary>bash · cargo test --workspace · ok · 2.1 s</summary>

```json
{"command": "cargo test --workspace"}
```

```
running 917 tests
…
test result: ok. 917 passed
```

</details>
```

Collapsed by default, the summary line is the whole story at a glance, and
the input and output are inside for anyone who opens it. Output is capped
at the same bound the dispatcher's spill applies inline
(`MAX_INLINE_CHARS`, and the head-plus-path text a spill produces is what
the model saw, so it is what the block shows); the full text is in the
journal and the spill file. A declined or refused call shows `· declined`
or `· refused` in the summary and the gate's sentence inside. `type`,
`name`, `ok`, `ms` are attributes the app's `details` token keeps
(`attributes`), there for a later skin rule to style and for nothing the
rendering depends on.

The block is written whole, on completion, because the wire is
append-only: the app takes deltas and cannot rewrite what it drew, so a
block opened "running…" could never be closed with its result. One
durable block per call, one ephemeral status line per call.

**Rejected.** `delta.tool_calls` — the app's own function-calling loop
would take the call (above). A `<details>` block emitted at start and
rewritten at finish — no `replace` semantics on the chat-completions
wire; the app *does* accept a `replace` event, but that rewrites the whole
bubble and would fight the text deltas around it. Plain fenced code with
no collapsible — works, is what the fallback is if the `details` token
ever stops rendering, and is the first browser check the executor runs.

### Non-streaming

`stream: false` is honoured (the app has a per-chat "stream response"
toggle, and every task request is non-streaming): the same plan runs, the
window accumulates instead of writing, and the response is one
`chat.completion` with `message.content` (text, tool blocks, question
block), `message.reasoning_content`, `usage`, `finish_reason`. A request
that joins a running turn returns what accumulated from the join onward.
The app's total client timeout is none by default; if the operator sets
`AIOHTTP_CLIENT_TIMEOUT`, a non-streaming turn longer than it fails on the
app's side with the turn still running here, and the next message finds it
(section 5). Streaming is the recommendation for chats.

### Files

- `crates/web/src/shim/history.rs` — `plan(&session, &request, &headers)
  -> Plan::{Append, Rewind{to: RecordId}, Root, Foreign{notice}}`, the
  `owui-turn` note reader and writer, the count check.
- `crates/web/src/shim/openai.rs` — the request types (`messages`,
  `model`, `stream`, `content` parts), the chunk and completion builders,
  the error object, `usage` mapping from `eidolon_core::message::Usage`.
- `crates/web/src/shim/render.rs` — `project(&Event, &Ctx) -> Vec<Frame>`
  where `Frame::{Delta(String), Reasoning(String), Event(Value),
  Finish(reason, usage), Error(Value)}`; the tool block, the notice
  lines, the cap. A `match` exhaustive over `Event`, as `wire::project`
  is, so a new variant fails to compile here before it fails on a screen.
- `crates/web/src/shim/window.rs` — the SSE body: subscribe, first chunk,
  forward, keepalive, the close-by-server flag, the drop guard
  (section 5); the non-streaming accumulator over the same frames.
- `crates/web/src/shim/task.rs` — the stateless completion for task
  requests, over `SessionFactory::resolve_backend`.
- `crates/web/src/serve.rs` — the `POST /v1/chat/completions` arm: bearer
  check, header read, body parse, task-or-chat dispatch.

### Proof

- `render::project` has one fixture per `Event` variant, asserting the
  exact JSON (the table above); the test module fails to compile when a
  variant is added.
- A scripted provider that calls `bash` then answers: the stream carries
  `status done:false`, `status done:true`, one `<details type="eidolon.tool"
  … ok="true">` block whose summary names the tool and input, text deltas,
  a `finish_reason: "stop"` chunk with `usage`, `[DONE]` — in that order.
- A declined call renders `· declined`; a table refusal renders the
  `> policy refused` line and `· refused`.
- Output over the cap renders the head and the spill path, not the flood.
- `ThinkingDelta` arrives as `reasoning_content`, never as `content`.
- The stream never contains the key `tool_calls` (assert over the whole
  body for every fixture).
- `history::plan`: parent = last note → `Append`; parent = older note →
  `Rewind` to that note's successor's parent; empty parent on a branch with
  turns → `Root`; absent headers with count mismatch → `Foreign` with the
  notice text; absent headers with count match → `Append`, no notice.
- After a `Rewind` the journal shows a record whose parent is the fork
  point, `Session::messages()` omits the abandoned turn, and `eidolon log`
  shows both leaves.
- A system message differing from the note journals `SessionNote`; the
  same one twice journals it once; an empty one clears.
- A `data:` image becomes an `Image` block on the `UserMessage` record; an
  `https:` one does not, and the reply carries the notice.
- `X-Eidolon-Task: title_generation` with a chat id creates no file and
  returns one `chat.completion`; `### Task:` + `stream:false` without the
  header does the same; the same body with `stream:true` and no header is
  a chat turn.
- 15 s of provider silence produces a `: keepalive` line.
- `stream:false` returns one completion whose `content` equals the
  concatenated deltas of the streaming case.

---

## 4. Approvals

The decided approach: render the gate's question as assistant text and let
the next user message answer it. This section makes it a protocol.

**Choice.** The chat's `UserIo` — `ChatUser`, the seat `WebUser` occupies
for `eidolon web` — implements `approve` and `choose` by **parking**. The
dispatcher calls `approve` from inside `adjudicate` and awaits it; nothing
about that changes. `ChatUser` stores the question in its one pending slot
(the same single-slot rule `WebUser` keeps, and for the same reason: the
dispatcher asks serially), writes the **question block** into the current
window, ends that window cleanly (`finish_reason: "stop"`, `[DONE]`), and
waits on a oneshot. The turn is now blocked inside `approve`, journaled up
to the assistant message that made the call, holding no lock. The app sees
a finished reply that ends in a question.

The next request for this chat is read **first** as a possible answer,
before anything in section 3 runs:

- **yes** → the oneshot resolves `Some("yes")`, `approve` returns `true`,
  the dispatcher journals `PolicyVerdict{Approved}` and runs the call; the
  new request's window is attached to the turn and streams the rest of it.
- **no** → `Some("no")`, `approve` returns `false`,
  `PolicyVerdict{Declined}`, the tool result is the dispatcher's own
  "the user declined this tool call", the model reads it and goes on; the
  window streams that.
- **anything else** → the text is delivered as `Agent::steer` *first*,
  then the oneshot resolves `Some("no")`. Order matters: the declined
  result is journaled, the loop reaches its next safe point, and
  `deliver_steers` puts the person's words on the branch before the next
  model call — so the model reads "declined" and the new instruction
  together, and the person did not have to say no to say something else.
  Fail closed: an unrecognised answer never runs a tool.

For `choose` (`choices_user`), the block lists the options as a numbered
list and the answer is a label (case-insensitive, exact) or its 1-based
number; anything else is `None` — "the user did not choose" — plus the
steer, as above.

**The chokepoint is intact.** `ChatUser` is the last hop, not a decision:
the gate decided to ask, the dispatcher journals whatever the person said,
and yolo is untouched (no per-chat switch exists; `--yolo` on the serve
process is the operator's posture for every chat, said in every reply per
section 3). A chat cannot approve on its own, and a chat with nobody in it
approves nothing — the parked oneshot is a question with no answer, which
is the fail-closed shape `NoUser` has.

### The question block

Written as the last thing in the reply, from `Verdict::Ask`'s own sentence
(the policy table's wording: ``bash — writes outside the working
directory. Run it?``) and the call:

```markdown

---
**eidolon is asking:** `bash` — writes outside the working directory. Run it?

```json
{"command": "rm -rf ../build"}
```

Reply **yes** to run it or **no** to skip it. Any other message skips it and is taken as your next instruction.
```

The rule sentence is part of the block on purpose: the protocol is
whatever the person can see, and a person who has never read this
document has to be able to answer.

### Recognising an answer

Normalise the last user message: trim, lowercase, drop trailing `.`, `!`,
collapse internal whitespace. Then exact membership:

- **yes**: `yes`, `y`, `yes please`, `ok`, `okay`, `approve`, `approved`,
  `allow`, `go`, `go ahead`, `do it`, `run it`, `proceed`, `sure`, `yep`,
  `yeah`
- **no**: `no`, `n`, `nope`, `deny`, `denied`, `skip`, `skip it`,
  `don't`, `do not`, `refuse`, `cancel`, `stop`, `abort`

Exact, not prefix: `yes but only the tests` is an instruction, not a yes,
and it steers. English only in v1 (open item). The word itself is not
journaled as a user message — the `PolicyVerdict` record is the answer, as
a keypress in the TUI is — but the app keeps the bubble, and the
reconciliation in section 3 knows a bubble that answered a question from
the window ids it holds in memory.

### Unrelated messages, timeouts, abandonment

An unrelated message is the third arm above: no plus steer. It is the
common case — a person who reads the question and types "actually, just
list the files" has answered it.

There is **no timeout** on a parked question. A person may come back
tomorrow; the journal is the state, and a parked oneshot is one future in
one process. What the timeout would buy — a turn that stops waiting — is
the wrong thing: a declined-on-timeout call sends the model on to do more
work unattended, and a cancelled-on-timeout turn throws away a question
that was about to be answered. Parked chats are exempt from idle eviction
(section 1). The one bound is the process: a restart drops the oneshot,
`needs_finishing()` is true on the next request, `continue_turn`
re-dispatches the dangling call, the gate asks again, and the held message
answers it — which is why the restart case and the live case read the
same. The app's own tool-approval mechanism has the same shape: a
`function_call` with `status: "pending"` waits until someone acts
(`middleware.py` 3438–3480).

The Stop button cannot reach a parked question — the stream ended when the
question was rendered, so the app shows no Stop — and "no" is the cancel.
A question parked while a *rewind* arrives (section 3, rule 2: the person
edited the message that led to it) is cancelled through the turn's token
first: `approve` returns `false` on the token, the dispatcher journals
`Declined`, the loop journals `Cancelled`, and the fork proceeds.

**Rejected.** A per-question wall-clock timeout (above). Re-asking the
question on an unrelated message instead of declining — a loop the person
cannot leave except by saying a magic word. Treating an unrelated message
as yes — never. The app's native approval UI (`function_call` items with
`pending` status): it exists in 0.11.3 for the app's *own* tools and is
reached only by speaking `tool_calls`, which hands the call to the app's
execution loop; a future version of the app may expose it for upstream
models, and that is the moment to revisit.

### Files

- `crates/web/src/shim/user.rs` — `ChatUser: UserIo` (`approve`, `choose`,
  the pending slot, `park(question) -> oneshot::Receiver`, `answer(text)
  -> Answered::{Yes, No, Choice(label), Unrelated}`), the vocabulary, the
  question-block renderer.
- `crates/web/src/shim/chat.rs` — the request-time read: parked? →
  classify → steer-then-resolve, attach the window.
- `crates/web/src/shim/render.rs` — the block text.

### Proof

- With a `Fixed(Ruling::ask(...))` policy hook (the pattern
  `crates/core/src/yolo.rs`'s tests use) and a scripted provider that
  calls `bash`: the first request's stream ends with the question block and
  `finish_reason: "stop"`; the journal ends on the `AssistantMessage` with
  no `ToolResult` and no `PolicyVerdict`; the turn task is still running.
- Second request `yes` → `PolicyVerdict{Approved}`, the `ToolResult`, the
  tool block and the rest of the reply on the second stream; no
  `UserMessage` record for the word.
- Second request `No.` → `PolicyVerdict{Declined}`, result text "the user
  declined this tool call", the reply continues on the second stream.
- Second request `list the files instead` → `PolicyVerdict{Declined}` and
  a `UserMessage` record carrying that text *after* the `ToolResult` and
  before the next `AssistantMessage`.
- `yes but only the tests` is `Unrelated`.
- `choices_user` with three options: `2` and `Second` (case-insensitive)
  both resolve to the second label; `maybe` is `None` plus a steer.
- Restart while parked (drop the `Shim`, rebuild, send `yes`): the
  continuation re-asks, `yes` approves, the journal shows exactly one
  `PolicyVerdict` and one `ToolResult` for the call.
- A parked chat survives the idle sweep.
- A rewind request while parked journals `Declined` then `Cancelled`, then
  a record forking from the named point.
- The question block's text contains the `Verdict::Ask` sentence and the
  call's input, and the reply-instructions line, verbatim.

---

## 5. Cancel, error, reconnect

**Choice.** A dropped stream cancels the turn. Stop in the app is
`task.cancel()` → `aclose()` on the upstream body → the TCP connection to
the shim closes (`middleware.py` 6280–6290); on the shim's side the SSE
body is dropped by hyper, a guard on the window runs, and it fires the
turn's `CancellationToken` — the same token `POST /api/cancel` fires for
the page, which reaches a running provider stream, a running tool
(`bash` kills its process tree; a cancelled command renders `[cancelled]`),
and a parked question. The loop journals `ContextSize`, `TurnSpend`,
`Cancelled` and publishes `Event::Cancelled` (`Agent::cancelled`). What
the person meant by Stop is "stop", and the wire gives no other reading of
a dropped connection.

The guard does **not** fire when the shim closed the window itself: after
the question block (section 4), or when a later request took the window
over (section 1). A `closed_by_server` flag on the window is set before
those closes; the guard checks it.

**What a dropped connection means.** Three things look identical at the
socket: Stop, the app's backend restarting, and a client timeout the
operator configured. All three cancel. The second is the cost of the rule:
a backend restart mid-turn stops the turn. Nothing is lost but the wall
time — the journal has everything up to the cancel, `needs_finishing()` is
true, and the next message on that chat continues the turn before running
itself (section 1, resume). Rejected: a "detached turn" that keeps running
after a drop and reattaches on the next request. It makes Stop not stop
anything, which is the wrong default for an agent that runs shell commands
behind a gate; a person who pressed Stop and then watched a build keep
going would be right to distrust it.

**Reconnect.** There is no verb for it on this wire — a chat-completions
request means "answer this", not "show me the answer in progress" — so a
person cannot reconnect to a turn in flight *through the shim*. Two
mitigations already exist and one does not:

- Between the browser and the app's backend, reconnection is the app's own
  business and it does it: the backend consumes the shim's stream in a
  background task, keeps the partial reply in `response_streams`, and a tab
  that reopens the chat catches up over socket.io. A closed laptop lid is
  not a drop.
- After a drop, the next message *continues*: `continue_turn` finishes
  what was cut, then the message runs, in one window with a separator.
  That is what reconnection amounts to here — the turn resumes when the
  person speaks again, which is the only signal the wire carries.
- What does not exist is a live eidolon-side window on a chat's turn. The
  page in `crates/web` has one (`GET /api/events`, subscribe-then-replay)
  but opens one session by path at boot, and opening a `chats\*.eid` a
  running shim has open is the double-writer hazard of section 1. Left
  open (below).

**Errors.** Before the first byte: the OpenAI error object with a real
status, never an SSE body with an error inside it — `401` bad token,
`400` bad body or bad chat id, `404 model_not_found`, `500` a log that will
not open (a corrupt `chats\<id>.eid` is refused by `SessionLog::open` and
stays refused until the operator moves the file; the chat is stuck and the
error says which file). After the first byte: the turn task's `Err` is the
`error` chunk of section 3's table, and the session is intact — the next
message resumes it. A provider that fails mid-stream is `Err` from
`stream_with_retries` after the retry policy the core already applies; the
shim adds no retry of its own, because a retry that re-ran a tool would be
the loop's "nothing published may be re-run" rule broken from outside.

### Files

- `crates/web/src/shim/window.rs` — `Window { cancel: CancellationToken,
  closed_by_server: AtomicBool, … }`, the `Drop` guard, `close()`.
- `crates/web/src/shim/chat.rs` — cancel-and-wait before a rewind; the
  continuation-then-message sequence on `needs_finishing()`.
- `crates/web/src/serve.rs` — the error responses and their statuses.

### Proof

- Drop the client side of a streaming request mid-turn (a scripted
  provider that blocks on a channel): the turn's token is cancelled within
  one keepalive interval, `Cancelled` is journaled, `Event::Cancelled` is
  published.
- The window closed by the shim after a question block does not cancel the
  parked turn.
- The window closed by a take-over does not cancel the turn; the new
  window streams the rest.
- A chat whose last turn is `Cancelled` with a dangling `tool_use`: the
  next request's stream shows the tool block for that call (replayed from
  the journal if its result was written, run otherwise), the separator,
  then the new turn.
- Bad token → 401 JSON, no session file; unknown model → 404, no file;
  corrupt log (truncate the header) → 500 naming the path, and the same on
  the next request.
- A provider error mid-stream → an `error` chunk then `[DONE]`; the next
  request on the chat resumes normally.

---

## 6. Auth

**Choice.** A bearer token, checked on every `/v1/*` request, in constant
time, against a token the serve process generated once and stored at
`%APPDATA%\eidolon\serve.token` (owner-only; 32 random bytes as hex,
prefixed `eid-`). Missing or wrong → `401` in the OpenAI error shape with
`WWW-Authenticate: Bearer`. No CORS headers, ever: a browser page on any
origin that tries to send `Authorization` to loopback triggers a preflight
the shim does not answer, and a page that sends a bare `POST` with no
token gets the 401.

Why check anything on loopback: the router does not (`hoot.rn`: "no auth
and bound to 127.0.0.1"), and the router only runs inference. The shim
summons an agent that runs tools on this account. Any process on the box,
and any web page in any browser on the box, can reach `127.0.0.1:<port>`
with a simple `POST` — CORS makes the *response* opaque, it does not stop
the request — and "run `rm -rf` in a chat nobody opened" is a different
class of thing from "generate some tokens". `eidolon web` answers the same
threat with a `Host` check because its POSTs come from its own page; the
shim's come from another process, so the check has to be a secret that
process holds. The `Host` check is kept too (`serve.rs::host_ok`, cheap,
closes the DNS-rebinding shape). Binding anything but loopback is refused
unless `--bind` names it explicitly, with a line saying the token is the
only thing standing between the network and the tools.

Why per install and not per boot: the app stores the connection's key in
its own config, and a token that changed on every `eidolon serve` would
have to be pushed into the app's database on every launch. Per install
means `hoot.ps1` reads the file once and hands it to the app's environment
(`OPENAI_API_KEYS`) — the environment, never argv, the rule the extension
host already keeps for its bearer tokens. Rotation is `del serve.token`
and a relaunch of both.

**What is not trusted.** The `X-OpenWebUI-User-*` headers are copied into
the `owui-chat` note as attribution and decide nothing: every chat runs as
the OS account the shim runs as, with the same tools and the same policy
table, and the token authenticates *the app*, not the person in it. On a
personal box that is exactly right. On a shared instance (the club's, one
install in front of many members) it means every member's chat is the same
operator — see "What this does not solve".

**Rejected.** No check (the router's posture) — wrong risk class, above.
A per-boot token — the rotation cost above. The app's own JWT
(`X-OpenWebUI-User-Jwt`, HS256 with a shared secret): it authenticates a
*user* to an upstream and is off by default; it could sit beside the
bearer token later for a multi-user design, and it does not replace it.

### Files

- `crates/web/src/shim/auth.rs` — `Token::load_or_create(path)`,
  `Token::check(header) -> bool` (constant time over bytes), the 401
  response.
- `crates/web/src/serve.rs` — the check on every `/v1/*` arm before the
  body is read.
- `crates/cli/src/config.rs` — `[serve] token_file` (default above),
  `bind`, `idle_minutes`, `cwd`.

### Proof

- No `Authorization` → 401 with `WWW-Authenticate: Bearer`; wrong token →
  401; right token → 200; the comparison takes the same time for a
  first-byte mismatch and a last-byte mismatch (a timing test with a
  generous bound, or a review of the loop — it must not short-circuit).
- The token file is created owner-only on first run and reused on the
  second.
- `GET /v1/models` also requires the token.
- A request with an `Origin` header and no token is refused like any
  other; no `Access-Control-*` header appears on any response.
- `--bind 0.0.0.0:8085` prints the warning; `[serve] bind` naming a
  non-loopback address without `--bind` is refused at boot.

---

## Registration: the contract with the launcher

The shim is one more OpenAI connection in the app. `bin/hoot.ps1` (B2/C5
on the task list) registers it through the environment the app reads at
boot, beside the router:

```
OPENAI_API_BASE_URLS = http://127.0.0.1:8080/v1;http://127.0.0.1:8085/v1
OPENAI_API_KEYS      = sk-no-key-required;<contents of %APPDATA%\eidolon\serve.token>
OPENAI_API_CONFIGS   = {"1": {"enable": true, "tags": ["agent"],
                         "headers": {"X-Eidolon-Chat-Id": "{{CHAT_ID}}",
                                     "X-Eidolon-Task": "{{TASK}}",
                                     "X-Eidolon-Message-Id": "{{MESSAGE_ID}}",
                                     "X-Eidolon-User-Message-Id": "{{USER_MESSAGE_ID}}",
                                     "X-Eidolon-Parent-Id": "{{USER_MESSAGE_PARENT_ID}}"}}}
ENABLE_FORWARD_USER_INFO_HEADERS = True
TASK_MODEL_EXTERNAL  = bonsai-8b
```

`X-Eidolon-Chat-Id` is the one the shim reads *first*; without it the chat id
only arrives on the `X-OpenWebUI-Chat-Id` fallback, which needs the box-wide
`ENABLE_FORWARD_USER_INFO_HEADERS` — the switch Amendment 3 exists to stop
depending on. It was missing from this block until C1's implementation report
caught it, because the block predates the amendment.

One consequence of how the app expands these, which the shim must handle and
does: `utils/headers.py` renders `{{CHAT_ID}}` as `metadata.get('chat_id','')
or ''`, so a request with no chat id still **sends the header, empty**. Absent
and empty must be treated alike; `serve.rs` filters both.

Two wrinkles the launcher's author needs: the `OPENAI_API_*` and
`TASK_MODEL_EXTERNAL` values are the app's *persistent* config — the
environment seeds them and a value saved from the admin UI wins thereafter
(`config.py`, `PersistentConfig`), so `hoot status` should read
`GET /openai/config` back and say when the registration drifted; and the
model list is cached for `MODELS_CACHE_TTL`, so a model that appears in
eidolon's catalog appears in the picker after the cache turns or an admin
refresh.

Without the custom headers the shim still works: tasks fall to the
`### Task:` backstop, and history reconciliation falls to rule 4 (append,
with the notice).

> **Amended.** This paragraph originally continued "without
> `ENABLE_FORWARD_USER_INFO_HEADERS` there is no chat id at all". That is no
> longer the design. `{{CHAT_ID}}` is available as a **per-connection header
> template**, which is better than a box-wide switch that forwards user
> identity to every OpenAI endpoint configured, so the shim reads its own
> header first and falls back to `X-OpenWebUI-Chat-Id`. The toggle is kept for
> attribution and nothing depends on it. See
> [Decisions](../Decisions.md#2026-09-18--the-shim-design-is-accepted-with-three-amendments),
> Amendment 3.

With no chat id reaching the shim by either route, every request is stateless.
The shim says so once on stderr the first time it sees a request with a
task-shaped body and no chat id, because that is the one misconfiguration that
fails silently otherwise.

**Task-shaped, not chat-shaped, and the distinction is the whole point.** A
chat-shaped request with no chat id is *legitimate* — section 1 blesses it by
name ("a `curl`, a client that does not forward it"), and it is how an operator
first pokes the shim by hand. Warning there tells someone their launcher is
broken when it is not. A task-shaped body is the unmistakable fingerprint of
Open WebUI and nothing else: either the app set `X-Eidolon-Task`, or section
3's `### Task:` backstop recognised a body only the app sends. So a task
request with no chat id means *this is the app, and its chat id is not
arriving* — which is exactly the silent misconfiguration and cannot be anything
else. The backstop is what keeps this working even when the whole `headers`
block is missing, which is the most likely way to misconfigure it.

Checked against the app rather than assumed: every task route in
`routers/tasks.py` puts `'chat_id': form_data.get('chat_id', None)` in the
metadata the header template renders from, so a correctly registered task
request *does* carry a chat id and this condition does not fire on a healthy
install.

## Files and functions

The shim lives in `crates/web`, per the task list (C1), as a module tree
beside the page. `serve.rs` stays the one route table; the page's routes
are unchanged.

| file | owns |
|---|---|
| `crates/web/src/lib.rs` | `pub mod shim`; `ServeOptions { bind, cwd, sessions_dir, token_file, idle, yolo }`; `run_serve(factory, opts, cancel)`; the `SessionFactory` trait: `models() -> Vec<ModelRow>`, `resolve_backend(&str) -> Result<(Backend, String)>`, `summon(Session, PathBuf, Option<&str>, Arc<dyn UserIo>) -> Result<Summoned { agent, swarm, doorbell, yolo }>`. Replaces `WebOptions::catalog: Option<Arc<dyn Any>>`, which existed for exactly this moment; `eidolon-web` still does not depend on `eidolon-providers` — the catalog stays behind the trait, in the CLI |
| `crates/web/src/serve.rs` | two new arms, `GET /v1/models` and `POST /v1/chat/completions`, live only when `Ctx.shim` is `Some`; the bearer check; the OpenAI-shaped error responses |
| `crates/web/src/shim/mod.rs` | module doc (this document's decision, in the house voice); `Shim` (the chat map, `summon`, the idle sweep) |
| `crates/web/src/shim/path.rs` | chat id → path |
| `crates/web/src/shim/chat.rs` | `ChatSession`; the per-request sequence: auth → task-or-chat → summon → answer-if-parked → plan → note → run/join/attach |
| `crates/web/src/shim/history.rs` | `plan(...)`, the `owui-turn` note |
| `crates/web/src/shim/user.rs` | `ChatUser: UserIo` |
| `crates/web/src/shim/render.rs` | `project(&Event) -> Vec<Frame>`, blocks and notices |
| `crates/web/src/shim/window.rs` | the SSE body and the accumulator; keepalive; drop guard |
| `crates/web/src/shim/openai.rs` | request/response/chunk/error types |
| `crates/web/src/shim/models.rs` | the advertised list |
| `crates/web/src/shim/task.rs` | stateless completions |
| `crates/web/src/shim/auth.rs` | the token |
| `crates/web/tests/shim.rs` | the end-to-end fixtures above, over a real `TcpListener`, a scripted provider (`eidolon_core::testing::ScriptedProvider`) and a `Fixed` policy hook |
| `crates/cli/src/main.rs` | `Cmd::Serve { bind, cwd, yolo }` and its arm; `CliFactory` implementing `SessionFactory` over `build()` and the `Catalog`; a line in the module doc's subcommand list. `eidolon web` is untouched and stays the one-session verification page |
| `crates/cli/src/config.rs` | `[serve]` stanza: `bind` (default `127.0.0.1:8085`), `token_file`, `idle_minutes` (60), `cwd` |
| `crates/web/Cargo.toml` | no new crates: hyper, `http-body-util`, `async-stream`, `serde_json` are already there; the token's randomness comes from `getrandom`, which is in the lock under `rand` |
| `bin/hoot.ps1` | the registration block above; `hoot up` starts `eidolon serve` |

Nothing in `crates/core` changes. The shim is built on `Agent::run_turn`,
`continue_turn`, `steer`, `set_backend`, `set_model`, `fork_at`,
`Session::needs_finishing`, `session_note`/`set_session_note`, `append`
(for the two notes), and the `UserIo` seam, all as they are.

## What this does not solve

- **One operator.** Every chat, every app user, runs as the account
  `eidolon serve` runs as, with one policy table and one tool set. The
  token authenticates the app. A club-shared instance would need per-user
  session directories, per-user policy, and something that turns an app
  user into an OS boundary; none of that is here, and the licence's
  fifty-user clause is the other reason not to design it yet.
- **One working directory.** All chats run in `[serve] cwd`. A chat
  cannot choose a project. The obvious shape — a model id or a first-line
  command naming a directory — is a second per-chat knob the app does not
  have, and it is not designed here.
- **No live eidolon-side window.** A chat's turn can be watched only in
  the chat. Generalising the page's `GET /api/events` to
  `?chat=<id>` on the serve process is the cheap next step, and it would
  also be where `/ext` and `/auth` (A3) link to.
- **Reconnection is "speak again".** The wire has no verb for resuming a
  stream; a drop cancels, and the next message continues. A person who
  wants to *watch* a long turn after their app backend restarted cannot.
- **Approval vocabulary is English and exact.** A label list is the
  smallest thing that fails closed; it is also the thing a non-English
  club member trips on. The policy sentence is whatever `policy.rn` says.
- **Rendering is verified by reading the bundle, not running it.** The
  `details` token and the `status` event were read in the compiled
  frontend; whether `type="eidolon.tool"` renders as a plain collapsible
  and whether status lines persist across a reload are the first two
  things the executor checks in a browser, with the fenced-code fallback
  ready.
- **Task requests without the headers are guessed.** The `### Task:`
  backstop is a template prefix, and a custom template defeats it. The
  contract is the header; the launcher sets it.
- **Files.** The app attaches files by injecting their text into the last
  user message (RAG). The agent cannot `read` them; it sees the injection.
  Images work; documents are prose.
- **Deleted chats leave logs.** Correct, and unbounded. `eidolon sessions`
  lists them; nothing prunes.
- **Concurrency across chats is unbounded.** N chats can run N turns
  against one `llama-server` with `--models-max 2`; the router queues and
  every chat slows. A global cap, or a per-model semaphore in the
  provider, belongs with A2's fallback work, not here.
- **Non-streaming turns and the app's optional total timeout.** If the
  operator sets `AIOHTTP_CLIENT_TIMEOUT`, a non-streaming turn longer than
  it fails on the app's side. Streaming is the recommendation; there is no
  "still working, ask again" partial reply in v1.
- **The app's own tool-approval UI** is not used. It is reached only by
  speaking `tool_calls`, which the app then executes. If a later version
  lets an upstream model's call wait for approval without the app running
  it, that is the moment to render the gate's question natively.
- **`eidolon web` and `eidolon serve` are two processes** with one route
  table between them. The page will want to move onto `serve` once it can
  address a chat; until then the split is stated rather than resolved.

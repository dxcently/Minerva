# Automatic model fallback

Status: design (Tasklist Wave A2). Implementation is Wave C2, "Provider fallback in `crates/providers` + config", which depends on this document. Nothing here is code; it names the files, types and tests the executor will write, and argues each choice against what the tree does today.

## Orientation

Read these before the sections below. Every decision here is argued against them, and the section on placement (§6) assumes the reader knows who writes the journal.

- `crates/providers/src/readiness.rs` — the offline probe: `Readiness`, `UnavailableReason`, `Catalog::readiness`. It has no in-tree consumer today; this design is its first.
- `crates/providers/src/catalog.rs` — `Catalog::resolve`, `resolve_unclaimed`, `backend_for`, `weight`, `mock`, and the rules that turn a spec into a `provider:id` key.
- `crates/providers/src/http.rs` — `HttpProvider::open`, the one place a credential touches a request, and the shape of every error it produces: `{name} returned HTTP {status}: {body}`.
- `../harnox/src/llm/consume.rs` — `stream_with_retries`, `RetryPolicy` (4 attempts, 0.5/1/2 s, 120 s budget), `is_transient`, `is_auth_failure`, and the rule that nothing published is replayed.
- `../harnox/src/claude_cli.rs` — `ClaudeErrorClass` and `classify`, the driver's view of the same question.
- `crates/core/src/agent.rs` — `continue_turn`, `drive`, `BusSink`, `Backend`, `set_backend`, `set_model`, `adopt_model`, and the consumer-injected hooks (`set_inbox`, `set_persona_source`, `set_command_channel`).
- `crates/core/src/session/mod.rs` and `crates/core/tests/pins.rs` — `RecordKind`, `ModelChanged`, `Note`, and the byte pins that make adding a variant a deliberate act.
- `crates/cli/src/main.rs` — `build` (the `[model]` substitution when a journaled model does not resolve), the `:model` handler, `journal_claim`, `judge_provider`, `Cmd::Models`.
- `crates/cli/src/config.rs` — `Config`, `catalog()`, `model_weights`, `secret_store`.
- `docs/Decisions.md`, tail — "Melete cannot run the mint yet".

## The problem

The operator's Melete box runs its agents through a linked eidolon. Its skills are configured with `skills.model_backend = "ollama:deepseek-v4.1-flash"`. The eidolon on that box resolves provider keys from Melete's own custodied secret store, which holds `deepseek` and `app:dong/*` and no `ollama`. Every run therefore fails before its first request: "provider ollama has no key Melete can read". The Claude CLI on the same box is logged in and works, but only when a run names `model: "sonnet"` by hand. The operator asked for "deepseek 4.1 agents (sonnet fallback)". Nothing in eidolon can express that sentence today; `docs/Decisions.md` records it as "no automatic fallback exists".

Three facts make this harder than "try the next one":

1. A failed request is not one thing. A key that is absent, a key that is rejected, a provider that is rate-limited, a network that is down, a model that has been retired, and a model that declined to answer all surface as errors. Only some of them mean "another model would have served this request", and one of them — a refusal — must never be answered by a different model without the operator knowing.
2. The journal is truth and resume is replay. Whatever model answered a turn must be recorded before that turn's messages, or a resumed session will attribute, price and continue on the wrong model. Anything that switches models below the agent loop cannot write that record.
3. The harness already retries. `stream_with_retries` gives every provider four attempts within a two-minute budget and refuses to replay anything already published. A fallback that sits inside that policy multiplies it; a fallback that ignores it re-sends turns the operator has already seen half of.

The design below adds one TOML table, one module in `eidolon-providers`, one hook in the agent loop, one bus event and one wire message. It adds no journal record type, so `pins.rs` does not change.

## 1. What is a failure worth falling back on

### The choice

Failures are classified at two points, with the evidence each point actually has. Before any request, `Catalog::readiness` says whether the primary is structurally usable on this machine; a primary that is not is skipped offline, with zero network calls. After a request, the `anyhow::Error` that reaches the agent loop — already the survivor of harnox's retry policy — is classified by a pure function, `classify`, in a new `crates/providers/src/fallback.rs`, into a `FallbackReason`. The loop consults the fallback only for reasons that say "another model would have answered this", and only when nothing from this attempt has been published.

`classify` works on the error text, the same way `is_transient` and `is_auth_failure` do, because text is the only channel these errors travel on: `HttpProvider` folds the HTTP status and up to 2000 bytes of body into the message, the OpenAI wire's in-stream error becomes `{name}: {err}`, a missing key becomes the stream's first `Err` carrying `ProviderDef::token`'s sentence, and the driver's failure is `claude exited {status}: {text}` with `ClaudeErrorClass::classify` available on the text. No new error type is threaded through harnox. The classifier is the single place that knows these shapes, and it lives next to the provider scripts that produce them.

| Evidence | Seen where | Verdict | Why |
| --- | --- | --- | --- |
| No credential: `Readiness::Unavailable` — `token()` would say `no key for secret`, `no secret store is configured`, the token file is missing, or the driver binary is not on `PATH` | before the request, offline | fall back before the request | The answer is known without asking anyone; asking costs a failed stream and a misleading error line. This is the Melete case. |
| Auth rejected: HTTP 401 or 403, or any `is_auth_failure` phrase | after one attempt (harnox never retries auth) | fall back | The key is present and not accepted. Retrying in place is pointless and harnox already refuses to; another provider with its own key is the only thing that can serve the turn. |
| Rate limited: HTTP 429, driver `ClaudeErrorClass::RateLimit`, "usage limit reached" | after harnox's four attempts (429 is transient to `is_transient`) | retry in place first, then fall back | A 429 often clears within the 3.5 s of backoff harnox already spends; a hop that skips that spends money at a second provider to save seconds. If four attempts did not clear it, the window is shut for longer than a turn can wait. |
| Quota spent: HTTP 402 (`chatgpt.rn` answers 402 when its window is spent) | after one attempt (402 is not transient) | fall back | The provider is up, the key is good, and no retry within a turn will refill a window. |
| Transport: connection refused or reset, HTTP 408, any 5xx, driver `ClaudeErrorClass::Timeout` | after four attempts | retry in place first; fall back only if nothing was published | Transient by harnox's own definition, so it gets harnox's attempts. After that the provider is down for this turn. If any delta was published the turn is not replayed anywhere: the operator has seen half an answer and must not see a second, different half. |
| Model unknown: HTTP 404; or a 400/422 whose body contains one of a short, explicit phrase list (`model not found`, `model_not_found`, `does not exist`, `not a valid model`, `unknown model`, `no such model`) | after one attempt | fall back | `ollama.rn` says it in its own comment: "a row that starts answering 'model not found' has probably been retired". The provider is up, the model is gone, and the chain is the operator's answer to exactly that. The list is short and explicit because a 400 that does not match is a request bug (next row), and a fuzzy match would hide it. |
| Request shape: any other 400, 405, 409, 413, 415, 422 | after one attempt | fail loudly, no fallback | The request is wrong, not the model. `zai.rn`'s `glm-4.6v` answers 400 code 1210 when `max_tokens` exceeds 32768; re-sending that request elsewhere hides a bug behind a working answer, and the operator pays for both. |
| Refusal, empty reply, thinking-only reply, `MaxTokens`, `StopSequence` | never seen as an error: these arrive as `Ok` | never | A refusal is an answer. See below. |
| Cancelled | `Err`, with the cancel flag set | never | The operator stopped it. |
| Anything after a published delta or a journaled message | `Err` after output | never | Nothing published is replayed. That is harnox's contract and the fallback keeps it. |
| Driver `ClaudeErrorClass::Other` | after one attempt | never | The cause is unknown; a hop on an unknown cause is a guess with a bill. The existing `claude exited …` error stands. |

### Why a refusal can never become another model's answer

The point where a refusal is visible is the `Ok` arm of `continue_turn`: the stream ended, `Collected.stop_reason` is `Refusal` (mapped from Anthropic `refusal`, OpenAI `content_filter`, Gemini `SAFETY`), the loop journals the `AssistantMessage` if it is non-empty, settles the turn with `TurnSettled { stop_reason, usage }`, and `usage::stop_note` draws "response stopped: the model declined to continue". An empty reply, a thinking-only reply and `MaxTokens` take the same path. The fallback hook is consulted from the `Err` arm only. A refusal is therefore not filtered out by the fallback; it is unreachable from it, and no edit to the classifier can change that without moving the hook. The negative tests in the last section pin this.

The reason is not caution for its own sake. A refusal is a model's decision about this request. Quietly re-asking a model with a different policy launders that decision into a different model's output, and the operator reads the answer believing the model they configured gave it. If they want a second opinion after a refusal, `:model` is one keystroke away and it is journaled as their choice.

An empty reply is treated the same way even though it is sometimes a provider hiccup. The loop does not second-guess an `Ok`; the operator sees the settle note and asks again.

### Why nothing published is replayed

`stream_with_retries` returns `Err(f.error)` and drops `Collected.produced`, so the loop cannot learn from the error whether anything was streamed. `BusSink`, the loop's `StreamSink`, is what published it, so `BusSink` gains a `published` flag set on the first `TextDelta`, `ThinkingDelta` or `ToolUseStart` it forwards, and the `Err` arm consults the fallback only when the flag is clear. The driver's sink gets the same flag: a `claude` process that died before its first result leaves a clean turn (the driver only bails when it saw no result), and a hop replays the branch on the landing; a process that died after streaming text does not hop.

### Rejected alternatives

*Every `Err` falls back.* Simplest to write and wrong twice. A 400 from a bad request body would be re-sent to every entry in the chain and answered by whichever entry is lenient, hiding the bug; and a transport failure after half an answer would show the operator two answers to one question.

*Only readiness falls back.* Catches the Melete case and nothing else. Melete's `deepseek` key can expire (401), the DeepSeek window can shut (429), and `ollama` can retire the row (404); readiness sees none of these because it never makes a request.

*Parse `Retry-After` and wait instead of hopping on 429.* `HttpProvider::open` discards response headers today, and a wait longer than harnox's budget is a wait the operator did not ask for. Left as a follow-up: `FallbackReason::RateLimited` carries an optional hint (the driver's `reset_at` already fills it) so adding header parsing later changes no verdict in the table.

## 2. The config shape

### The choice

A separate top-level table, `[fallback]`, keyed by model:

```toml
default_model = "ollama:deepseek-v4.1-flash"

[fallback]
"ollama:deepseek-v4.1-flash" = ["sonnet"]
"deepseek:*" = ["ollama:deepseek-v4.1-flash", "sonnet"]
```

A key is a catalog key (`provider:id`), a bare id resolved the way `--model` resolves it (so `sonnet` works), or a glob. Globs match the way `model_weights` globs match: an exact key beats a glob and the longest glob wins. `Catalog::weight`'s lookup is reused as is, so the operator learns one rule for both tables. A value is an ordered list of entries, each a key or bare id and never a glob, because an entry must name one model to land on. The chain for a model is looked up once, when the model is armed — at launch, on `--model`, on `:model` — by the model's resolved key.

On the Melete box the table sits next to the provider tables Melete already forwards, and `skills.model_backend` stays the primary. The operator's whole request is two lines:

```toml
[eidolon.fallback]
"ollama:deepseek-v4.1-flash" = ["sonnet"]
```

Melete's loader has to pass `eidolon.fallback` through to eidolon's `Config` the way it passes `[[eidolon.providers]]`. That is a Melete-side change outside this repo and outside Wave C2; it is one table copy.

The table is read in `config::catalog` right after `set_weights`, by a new `Catalog::set_chains`. Validation happens there, at load, and each finding is printed with a `[fallback]` prefix in the same place `[provider]` load errors are printed today:

- An entry that does not resolve is dropped and named: `[fallback] chain for "deepseek:*": "deepseek-v4-pro" dropped: served by 2 providers; say which`. The resolution error is `Catalog::resolve_unclaimed`'s own text, so an ambiguous bare id gets the same sentence it gets on the command line.
- `mock` as an entry is dropped with a line. It is the test double; a chain that lands on it answers real turns with scripted text.
- An entry equal to its own key is dropped silently.
- A chain longer than `MAX_CHAIN` (three) is truncated with a line.
- Keys are not validated beyond syntax. A glob that matches nothing is not an error, because provider scripts come and go and a config should survive a script being removed.

`Config` has no `deny_unknown_fields`, so an older binary ignores the table and the config can ship before the binary does.

A `--no-fallback` flag beside `--model` and `--provider` disables arming for the session. It exists for tests and for the operator who wants to watch the primary fail.

### Why not `model_weights`

`model_weights` "order the listing and nothing else"; that sentence is its documented contract. Reusing it as the fallback order would break four things at once:

- Every existing config with weights would start hopping silently on upgrade. Weights exist on machines that never asked for a fallback.
- A picker preference is not a fallback preference. An operator who lists `sonnet` first and `deepseek` second wants sonnet by default; they did not say they want deepseek when sonnet fails, and they certainly did not say they want sonnet when deepseek fails, which is what a symmetric order implies.
- A total order cannot say "stop here". There is always a next element, so a session on the last-resort model would hop to whatever is cheapest next.
- Weights are one order over everything; chains differ by primary. A DeepSeek-first session should fall to ollama's DeepSeek row, then to sonnet; a sonnet-first session should fall nowhere, or somewhere the operator chose.

### Other alternatives rejected

*A single global list, `fallback = ["sonnet"]`.* The operator's own example needs two chains. A global list would also send a Claude session to DeepSeek on a Claude hiccup, which nobody asked for. The glob key gives the global case when it is wanted: `"*" = ["sonnet"]`.

*A `fallback` field inside `[[providers]]`.* Fallback is answerable at the model level: one provider serves several models, and `deepseek-v4-pro` is served by two providers. `[[providers]]` patches provider scripts, and the models come from the scripts, not from config; a per-provider field cannot say "this row, not that one".

*Transitive chains, where landing on a model consults that model's own chain.* Rejected in §3: the operator writes the whole path where they can read it.

## 3. Guards

### Max hops

`MAX_CHAIN` is three. A chain is walked in one pass, left to right; each entry is tried once per arming; an entry equal to the current key or already tried is skipped; and chains are not transitive — landing on `ollama:deepseek-v4.1-flash` from the `deepseek:*` chain does not consult ollama's own chain. The operator writes the whole path on one line where they can read it. Transitive chains form cycles that need a visited set to break and multiply cost in a way no line of config shows, and three entries cover the real shape of the request: the model I want, a cheaper alternative, and the one that always works.

Within a turn the hop count is bounded by `MAX_CHAIN` as well, and a hop resets neither `max_iterations` nor the per-turn `calls` count. A turn that has hopped is the same turn with the same budget.

### Sticky for the session

The chain position advances and never rewinds on its own. Once a session is on a landing it stays there for every following turn. Three things re-arm the chain at its head: a new launch (`eidolon run`, a TUI start, a Melete run), because a launch is cheap to try; `:model` or `space m`, because the operator's choice always wins and is journaled as theirs; and a resume, which arms the chain of the journaled model — the log is truth, so the session runs what the log says and hops from there if it must.

Why sticky rather than "probe and return": a probe is a failing request per turn on a dead primary, and under harnox's policy a transport failure costs four attempts and up to two minutes before the hop, every turn. It also flips models within one conversation, changing behaviour, cache state and price mid-thread, and it fills the journal with `ModelChanged` pairs. The operator returns by hand in one keystroke, and that return is journaled.

Why not sticky across sessions: a persisted "primary is dead" marker outlives the fix. The operator adds the `ollama` key and the next launch would still skip it. A launch that re-tries costs nothing in the offline case and one failed call in the request-time case.

### A chain where every entry is dead

When the last entry has been tried the hook returns nothing and the loop's `Err` arm runs as it does today — `Event::Error` on the bus and `Err` to the caller — but the error carries a ledger, one line per entry tried and why, in order:

`no model could serve this turn — ollama:deepseek-v4.1-flash: no key for secret "ollama" (skipped before the request); sonnet: claude exited 1: AuthFailure: not logged in; no fallback left`

The ledger is the difference between "sonnet failed" and "everything failed, and here is what each one said". Without it the operator reads the last error and never learns that the primary was not asked.

### Journal on success

`ModelChanged` is written only when a landing has answered: at the end of the landing's first `Ok` stream, immediately before the `AssistantMessage` it produced, so a replay attributes that message to the model that wrote it. A dead chain therefore leaves the log on the primary. That is the true state — nothing has answered since the operator chose the primary — and it means a resume tries the primary first and re-walks the chain only if it is still dead. Journaling at hop time would leave the log on whichever dead entry was tried last, and the next resume would start there.

The same rule covers the offline skip at launch. The agent is constructed on the primary key, so `SessionStart` names what the operator asked for; the landing's backend and key are adopted in memory (`set_backend`, then `adopt_model` — the non-journaling sibling of `set_model` that replay uses); the live surfaces are told (§4); and the `Note` and `ModelChanged` are written when the landing's first stream ends `Ok`. One rule for both paths, and one test shape for both.

### Cost, and the primary nobody notices is broken

Two guards are social rather than mechanical. Cost fan-out is bounded by the chain being short, explicit and announced per hop, and by the landing being priced at its own rates: `model_key()` moves with the hop, and `Catalog::cost` prices by key. A primary that has been broken for a week without anyone noticing is prevented by the `[fallback]` line at every launch and every hop, by the `Note` in every session that hopped, and by a readiness column in `eidolon models`, so the operator can see a dead row without launching anything.

## 4. What the operator is told

### The choice

There are two moments and each has its own sentence.

At resolution time — `build` at launch, and `:model` mid-session — the primary was skipped offline and nothing was sent. stderr gets one line in the family of `[model]`, `[policy]`, `[melete]` and `[tool]`:

`[fallback] ollama:deepseek-v4.1-flash → sonnet: provider "ollama": no key for secret "ollama"; run "eidolon secret set ollama" (or set OLLAMA_API_KEY)`

The reason is `Readiness::Unavailable`'s `diagnostic` verbatim. It already names the fix and carries no credential bytes, and the arrow already says what was decided, so the line adds nothing of its own. This is the shape of `build`'s existing `[model] {e}; opening this session on "{fallback}" (space m to change)`.

At hop time — the request-time case inside a turn — the surface is the bus, not stderr, because the TUI and the web owner watch the bus. Two events, in order: `Event::Error("{e:#} — falling back to sonnet")`, in the shape of harnox's `retrying (n/m)` line that `BusSink` already publishes, so the failure and its consequence sit on one line; then a new `Event::ModelChanged { key, reason }`, so a surface can update what it shows as the current model before the landing's first delta arrives. Headless `run` prints both on stderr, the second as `[fallback] P → F: reason` — the same prefix as the resolution-time line, so one grep finds both.

In the journal, on the landing's first successful stream (§3): `Note { "fallback: ollama:deepseek-v4.1-flash → sonnet: auth rejected (claude exited 1: not logged in)" }`, then `ModelChanged { "sonnet" }`. The Note's reason is the `FallbackReason` label plus the error's first line cut at 200 characters; the full `{e:#}` already went to stderr and the bus. `eidolon log` and `eidolon sessions` render it as `note: fallback: …` with no new code.

Per surface:

- TUI: `Event::Error` is already an alert line in the transcript. The new `Event::ModelChanged` arm does what the `UiMsg::ModelChanged` path does — `state.note("model → sonnet (fallback: …)")`, the status-line model, and `p.set_model` on the swarm presence, so a peer watching the swarm sees the session move.
- Web: `Event::Error` is already `Wire::Error { text }`. `project()` gains a `Wire::ModelChanged { model, reason }` arm; `project()` is an exhaustive match, so the compiler finds the gap. Replay already folds `ModelChanged` into `hello.model`.
- Melete: the stderr line reaches the run's Telegram stream, and the `Note` reaches the run audit through the journal.

What is never said: a key, a request body, a response header. The readiness diagnostic and `HttpProvider`'s error text are already policed for that, and the fallback quotes them; it composes no sentence of its own about the provider.

### Rejected alternatives

*A new `RecordKind::Fallback { from, to, reason }`.* Cleaner to query, but a new variant is an ordinal appended to a bitcode enum, a fixture in `pins.rs`, and a rendering arm in every reader — for one fact that `Note` plus `ModelChanged` already express and that `Session::model()` already understands. If a query need appears later, the Note's `fallback:` prefix is greppable and a variant can be added then, at the end, as the rule requires.

*stderr only.* The TUI redraws over stderr and the web owner never sees it. The bus is the surface both watch.

*Journal only.* That is the hidden broken primary of §3. The operator asked for a fallback, not for the primary to become invisible.

## 5. Interaction with what exists

| Existing piece | Consulted by the fallback? | How |
| --- | --- | --- |
| `Catalog::readiness` | yes, once per arming | the offline skip; never at request time |
| Key pool | only through `backend_for` | probes claim nothing; the landing claims; a hop releases nothing |
| `Fetch::Cached` / `Fetch::Live` | never `Live` | the chain resolves against what the catalog already holds |
| `--model`, `:model` | they arm the chain | for the named key, at its head |
| `--provider` | pins the primary only | chain entries resolve with no provider override |
| `mock` | never | no chain arms on `mock`; `mock` is refused as an entry |
| The policy judge | offline skip only | no request-time hop |
| `Catalog::complete` | walks the chain | the result says which model answered |
| Compaction | no | runs on the session's current backend |
| `Backend::Driver` | both directions | a chain may land on it and may hop off it |
| Swarm presence | updated on hop | through the TUI's `Event::ModelChanged` arm |

**Readiness** is consulted once per arming, inside `ChainFallback::arm`, for the primary and then each entry in order until one is usable. This is the first in-tree consumer of `Catalog::readiness`. It is never consulted at request time, where the actual error is better evidence than a structural probe could be. Readiness stays what it is: offline, claiming no lane, minting nothing. Driver readiness is "binary on `PATH`" and nothing more, so a logged-out CLI is a request-time auth failure, not a skip.

**The key pool** is touched only by the landing. Probing uses `resolve_unclaimed`; the chosen entry goes through `backend_for`, which claims its lane and returns the `ClaimedKey` that `journal_claim` journals once per provider per session under the existing `fresh` rule. A hop off a pooled provider does not release its lane: lanes are sticky per holder, the pool has no mid-session release, and the operator may `:model` straight back. A dead chain claims nothing new.

**Fetch** is never `Live` on a hop. Startup never waits on a gateway, and a fallback that waits on one is a second outage on top of the first. An entry naming a model absent from the cached listing resolves through `model_globs` or is dropped at load with its line; `eidolon models` refreshes the listing when the operator asks.

**`--model` and `--provider`.** `--model X` sets the primary and arms X's chain; `:model X` does the same mid-session. `--provider P` pins the provider for the primary's bare id only; chain entries are resolved with no provider override, because the operator pinned the primary's provider, not the landings', and an entry is a full key or an unambiguous id anyway. An explicit name arms the chain rather than suppressing it: `skills.model_backend`, `--model` and `:model` are all explicit, and the Melete case is an explicit name that needs a chain. `--no-fallback` is how the operator says "this one or nothing".

**`mock`** never arms a chain — `default_model` returns `mock` when no Claude CLI is configured, and a chain on it would turn every fresh install into a hopper — and is refused as an entry. Tests that need a fallback build one from `ScriptedProvider`s, not from the catalog's mock.

**The judge** gets the offline skip only: if the judge's model is unavailable on this machine, `judge_provider` takes the first usable entry of its chain and `[policy] judge …` says so. It never hops at request time. The judge has a four-second budget (`judge_timeout_ms`) and the operator is its fallback already; a hop would spend the budget twice. Driver entries are skipped for the judge because `judge_provider` rejects drivers.

**`Catalog::complete`** walks the chain the same way, since Melete's skills are its intended consumer: the offline skip before the request, `classify` on the `Err`, `MAX_CHAIN`, and no replay after output (the `IncompleteStream` ending already marks that case). `CompletionResult` gains `fell_back_from: Option<String>`, the primary's key, so a skill can tell which model answered; `key`, `model`, `cost` and `claim` already describe the landing. `CompletionRequest` is unchanged, and a catalog with no chains behaves exactly as today.

**Compaction** runs on the session's current backend, which is the landing if the session has hopped, and does not walk the chain itself. A compaction that hops would change the model behind a summary with no turn to attach the announcement to.

**The driver.** A chain may land on `sonnet` — the Melete case — and a session on the driver may hop off it when the CLI dies before its first result with `AuthFailure`, `RateLimit` or `Timeout`. Provider to driver mid-turn: the loop returns into `drive`, and the driver builds its prompt from the branch after its `BackendSession` mark, so it answers the user message already journaled. Driver to provider: `drive`'s `Err` arm hops, `set_backend` installs the provider, and the provider loop picks up the same journaled message. The CLI's own `--fallback-model` and the `modelUsage` field that reports it stay unread; that is the driver's business, not this feature's.

**Swarm presence** moves on a hop the way it moves on `:model`, from the TUI's `Event::ModelChanged` arm. Headless sessions have no presence to move.

## 6. Where it lives

### The choice

A hybrid, split along the line of who knows what. The data and the decisions live in `eidolon-providers`, in the new `crates/providers/src/fallback.rs`: `Chain` (the parsed table), `FallbackReason` (the classification), `classify` (error text to reason), and `ChainFallback` — the object that holds an `Arc<Catalog>`, the working directory and scratch path `backend_for` needs, the armed chain and its position, and whose `arm` resolves a primary to a `Landing` (backend, key, model id, claim, and the entries skipped with their diagnostics). Melete's assembly can use the same object when it builds an eidolon without the CLI.

The act of switching lives in the agent loop behind a consumer-injected hook: a trait `Fallback` in `crates/core/src/agent.rs` with one method, `next`, taking the current key, the error and the hop count and returning a landing or nothing. It is installed with `Agent::set_fallback`, in the pattern of `set_persona_source` and `set_command_channel`, so `eidolon-core` learns nothing about catalogs, readiness or key pools. `ChainFallback` implements the trait; the CLI's `build` installs it.

The hook is consulted at exactly two places: the `Err` arm of `continue_turn` (the provider loop) and the `Err` arm of `drive` (the driver), in both cases only when the sink's `published` flag is clear. On a landing the loop calls `set_backend` and `adopt_model`, publishes the two events of §4, and re-issues the same request with the landing's model id — `continue` in the provider loop, `return self.drive(…)` when the landing is a driver. On the landing's first `Ok` it appends `Note` and `ModelChanged` before the `AssistantMessage`.

### Why the loop must be the one that switches

The journal is written by the agent. That is the whole answer, but it is worth spelling out what a switch below the loop cannot do:

- It cannot journal. The journal handle is the agent's. A wrapper could be handed one, but it would then write `ModelChanged` mid-iteration, racing the loop's own records, and the record could land after the `AssistantMessage` it must precede.
- It cannot move `model_key()`. Pricing (`Catalog::cost` by key), attribution and presence would all still name the primary while the landing answers, and `TurnSpend` would be booked at the wrong rate.
- It cannot see the driver. `Backend::Driver` is not a `Provider`; the Claude CLI drives whole turns and journals itself. The Melete case lands on the driver.
- It multiplies retries. A wrapper is the `Provider` that `stream_with_retries` calls, so each entry inside it gets the outer policy's four attempts on top of its own — four times the chain length inside one 120 s budget, with no announcement between hops.

So a fallback below the agent loop can be journaled only by the loop, which means the loop must know a hop happened, which means the hook. The classifier and the chain are data and can live below; the switch cannot.

### Rejected alternatives

*Catalog only, at resolution time.* Solves the Melete case and nothing that happens after a request: an expired key, a shut window, a retired row. It is kept as the offline skip, but it is not the feature.

*All in core.* Core would need catalog keys, readiness and the pool to resolve a chain entry. It has none of them and takes its `Backend` from the CLI today; the hook keeps that boundary where it is.

*A wrapper provider, `FallbackProvider`, in `eidolon-providers`.* The four points above. It is the natural first idea, and every one of its problems is a journal or driver problem, which is why this section exists.

## Files and types the executor will touch

`crates/providers`

- `src/fallback.rs`, new. `MAX_CHAIN`. `Chain`: the ordered, validated entries for one key. `FallbackReason`: `Unconfigured { diagnostic }`, `AuthRejected`, `RateLimited { hint: Option<String> }`, `QuotaSpent`, `Transport`, `ModelUnknown`, and the non-hopping `RequestShape`, `Cancelled`, `Unknown`, with a `hops()` predicate so the table in §1 is one match arm per row. `classify(&anyhow::Error) -> FallbackReason`. `Landing { backend, key, model_id, claim, skipped }` where `skipped` is each entry passed over with its diagnostic. `ChainFallback::new(catalog, cwd, scratch)`, `arm(spec, provider_override) -> Result<Landing>`, `rearm(key)`, and the `next` that implements core's trait. The ledger text for a dead chain is built here. Unit tests for `classify` and the walk live at the bottom of the file.
- `src/catalog.rs`. `chains: RwLock<BTreeMap<String, Chain>>` beside `weights`; `set_chains(map) -> Vec<String>` returning the `[fallback]` lines for the CLI to print; `chain_for(key)` reusing `weight`'s exact-then-longest-glob lookup, factored into one helper both use.
- `src/completion.rs`. `Catalog::complete` walks the chain; `CompletionResult.fell_back_from: Option<String>`. Its tests use the existing `Scripted` provider and `Beat::Fail`.
- `src/lib.rs`. Re-export `Chain`, `ChainFallback`, `FallbackReason`, `Landing`, `MAX_CHAIN`, `classify`.

`crates/core`

- `src/agent.rs`. `pub trait Fallback` with `next`; `Agent::set_fallback`; `BusSink.published` and its twin on the driver sink; a per-turn hop count; the two `Err` arms; a pending `(from, to, reason)` that the loop writes as `Note` then `ModelChanged` before the landing's first `AssistantMessage`, then clears.
- `src/event.rs`. `Event::ModelChanged { key, reason }`. `Event` is not journaled, so there is no pin.
- `src/testing.rs`. `ScriptedProvider::fail(msg)`: a beat that returns `Err` for one attempt. Today the scripted provider cannot fail, which is why the negative tests below cannot be written without it.
- `src/session/mod.rs` and `tests/pins.rs`: no change. That is a deliverable, not an omission.

`crates/cli`

- `src/config.rs`. `Config.fallback: BTreeMap<String, Vec<String>>`, default empty, documented beside `model_weights`; `catalog()` calls `set_chains` after `set_weights` and prints its lines; parsing tests.
- `src/main.rs`. `--no-fallback`; `build` arms through `ChainFallback::arm` where it calls `backend_for` today, prints `[fallback] P → F: reason` per skip, and installs the hook; the `:model` handler re-arms; the headless event printer gains the `Event::ModelChanged` arm; `judge_provider` takes the offline skip; `Cmd::Models` gains a readiness column (`ready`, `no key: …`, `binary missing`) fed by `Catalog::readiness`.

`crates/tui`

- `src/app.rs`. The `Event::ModelChanged` arm: `state.note`, status line, `p.set_model`.

`crates/web`

- `src/wire.rs`. `Wire::ModelChanged { model, reason }` and its `project()` arm.

Outside this repo: Melete forwards `[eidolon.fallback]` into eidolon's `Config`.

Not touched: harnox. `RetryPolicy`, `is_transient` and `is_auth_failure` keep their contract, and the fallback sits above them by design.

## Tests

Every behaviour above has a test, and the negatives are the ones that keep the feature honest. All of them run offline; none touches a network, a real key or the secret store.

Where they live: `crates/providers/src/fallback.rs` (classification and the walk), `crates/providers/src/completion.rs` (`complete`), a new `crates/core/tests/fallback.rs` beside `loop.rs` (the loop, using `ScriptedProvider` with the new `fail` beat and a scripted `Fallback` implementation that hands out a second `ScriptedProvider`), and `crates/cli/src/config.rs` (the table).

### Classification

- 401 and 403 texts, and each `is_auth_failure` phrase, classify as `AuthRejected` and hop.
- 429 and "usage limit reached" classify as `RateLimited` and hop; the driver's `reset_at` lands in `hint`.
- 402 classifies as `QuotaSpent` and hops.
- Connection refused, 408, 500, 502, 503 and "timed out" classify as `Transport` and hop.
- 404, a 400 with "model not found", and a 422 with "does not exist" classify as `ModelUnknown` and hop.
- A 400 with zai's code 1210 text, a 413, and a 422 with no listed phrase classify as `RequestShape` and do not hop.
- `ProviderDef::token`'s "no key for secret" sentence classifies as `Unconfigured` and hops; this is normally caught offline, and the test proves the request-time path agrees.
- Text nothing matches classifies as `Unknown` and does not hop.

### The chain

- Four entries are truncated to three, with a line.
- An exact key beats a glob; the longest glob wins; `"*"` catches everything not keyed otherwise.
- A `mock` entry is dropped with a line; a self entry is dropped; an ambiguous bare id is dropped with the "served by 2 providers; say which" text.
- Offline skip: primary `Unavailable`, first entry `Present`. The `Landing` is the first entry, `skipped` holds the primary with its diagnostic, and the primary claimed no lane.
- Dead chain: every entry `Unavailable`. The `Err` names each entry and its diagnostic in order and ends with "no fallback left".
- The position is monotonic: once `next` has landed on the second entry, a later `next` never returns the first; `rearm` resets it.

### The loop, positive

- A 401 on the primary: one request reaches the primary, then the landing answers. The log holds `Note`, `ModelChanged { landing }` and the landing's `AssistantMessage` in that order, and `Session::model()` names the landing.
- A transport failure: the primary is asked four times (harnox), the landing once.
- The next turn stays on the landing; no request reaches the primary.
- `model_key()` is the landing's key when `TurnSpend` is written, so pricing by key follows the hop.
- A dead chain: the `Err` carries the ledger, no `ModelChanged` is in the log, and `Session::model()` still names the primary.
- `Event::Error("… — falling back to …")` and `Event::ModelChanged` are published in that order, before the landing's first `TextDelta`.
- Landing on a driver mid-turn: the loop returns into `drive`, and a scripted `TurnDriver` sees the user message that was already journaled.

### The loop, negative

None of these may hop. Each asserts exactly one request, on the primary, no request to the landing, and `Session::model()` unchanged.

- `StopReason::Refusal` with text: the `AssistantMessage` is journaled on the primary and the turn settles with `TurnSettled { Refusal }`. This is the test the feature exists to pass.
- `Refusal` with empty text: settles, no `AssistantMessage`, no hop.
- An empty `EndTurn` reply: settles, no hop.
- `MaxTokens`: settles, no hop.
- A `fail` beat after a `text` beat: `Err`, no hop, no second answer on the bus.
- A 400 request-shape error: `Err`, no hop.
- Cancellation during the primary's stream: `Cancelled`, no hop.
- No hook installed (`--no-fallback`): `Err`, exactly as today.
- A chain of three dead entries makes exactly three landing attempts and never a fourth.

### Completion

- `complete` on a 401 primary with a chain: `fell_back_from` is the primary, `key` is the landing, `attempts` counts both.
- `complete` with an `Empty` ending on the primary: no hop, `fell_back_from` is empty.

### Config and CLI

- `[fallback]` parses into `Config.fallback`; an absent table is an empty map.
- `catalog(cfg)` prints one `[fallback]` line per dropped entry and none for a clean table.
- `--provider mock` with a chain configured: no `[fallback]` line and no hook installed.
- `Cmd::Models` output has the readiness column, and for a `Present` row the secret's value does not appear anywhere in it.

### By hand, on the Melete box

Add the two-line `[eidolon.fallback]` table and leave `skills.model_backend` alone. A run prints `[fallback] ollama:deepseek-v4.1-flash → sonnet: … run "eidolon secret set ollama" …` and completes on the CLI; `eidolon log` shows the `note: fallback:` line under that session. Then set the key — `eidolon secret set ollama < key.txt`, from stdin, never argv — and the next run stays on the primary and prints no line. Both outcomes are the feature working.

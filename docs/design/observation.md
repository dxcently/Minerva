# Observation

What a node observes, what makes an observation untrustworthy, and what
happens then.

This amends `automation.md` in three places — the reserved events (§1), the
`tool` action (§1), and the head convention under "A Playwright accessibility
snapshot" (§2) — and it rules on the four questions J1's live trial handed to
A5, which absorb S23's correction, S26, S29 and S30. It extends four rulings
in `Decisions.md` rather than contradicting any of them: *one implementation
of a thing's meaning, in the process that owns it* (the textarea); *a
protection is only as reachable as the layer it inspects* (the fourth time);
*an observation that is not what the node believes it is*; and *a synthetic
fixture proves a mechanism; a captured one proves the contract with the
outside world*. It writes no code. The specification at the end is what a
Sonnet lands.

Every number below was observed on Windows 11 on this box unless it says
otherwise. `spill()`, `clip()`, `aria_snapshot` and `get_by_role` have no
operating-system branch — pure string handling in Rust, in-browser JavaScript
in Chromium — so the mechanism is platform-independent by construction; the
two places a Linux difference can enter are named where they occur (§4, the
`ps` header; §6, M5).

## The model in one page

An **observation** is the result of one `tool` action, written into
`context.<into>`. It is the only evidence a node has. It was produced by a tool
in some process and crossed several layers to arrive, and the node holds three
beliefs about it that nothing today checks:

1. **It is whole** — everything the tool produced.
2. **It is a success** — the tool did what the action asked.
3. **It is of the kind asked for** — the answer to the action's question, not
   to some other question.

An observation for which any of the three is false is **invalid**, and the
rule is one sentence: **a node never chooses over an observation it cannot
vouch for, and the vouching happens before the chooser is consulted, by the
layer that owns each fact.** Whole is vouched for by the producer, which says
how much it sent, and the consumer, which counts. Success is vouched for by
the tool, through `is_error`. Kind is vouched for by the graph, through an
`expect` guard on the action. Any of them failing raises the one event that
already exists for it, `ERROR`, now carrying `event.reason` so a graph can
route on which belief failed. The chooser is never asked to notice that its
question is corrupt; confidence is measured over the options and can only
ever be measured over the options.

That rule is only enforceable if the pipe between a tool and a node does not
rewrite what it carries. Today it does, three times, in three vocabularies.
So the second half of this document is about the pipe, and it reduces to one
clause: **between a producer and its consumer a tool result is whole, or it is
an error, or — for a model and only at the one chokepoint — it is a receipt.**
Nothing else is allowed to shorten anything.

| layer | today | after this document |
|---|---|---|
| `extensions/browser/service.py` `_snapshot` | the whole tree, 814,323 chars for Cat | a landmark subtree when asked (`within`), and a head block that states its own length (`chars:`) |
| `crates/tools/src/service.rs` `clip(MAX_BODY)` | cuts at 262,144 chars and appends a text marker | refuses over 262,144 with an error naming the size; never cuts |
| `crates/core/src/dispatch.rs` `spill()` | cuts every output over 8,000 chars to a 2,000-char head plus a receipt, whoever asked; errors exempt | bounds only what a **model** will read; a script receives the whole; the journal and the bus always get the bounded form; errors are bounded like anything else |
| `jev/automation/a11y.py` `is_truncated()` | looks for one layer's text marker at the tail | compares the body's length with the producer's declared `chars:` |
| `jev/automation/run.py` `_run_tool_action` | refuses a marker it can no longer see | refuses on any of the three beliefs, with a reason |
| `crates/rune/builtin/bash.rn` | a nonzero exit is ordinary text | a nonzero exit, a timeout, a kill are `is_error` |

## 1. Who a tool result is for

**Choice.** `spill()` applies to a tool output according to who will read it,
and that is named by `CallOrigin`. A `Model` or `User` call is handed the
bounded form (head plus receipt) exactly as today. A `Script` call is handed
the **whole output**. The journal and the event bus always receive the bounded
form, whatever the origin, and the spill file is written for the record in
every case.

**Rejected.** Leaving `spill()` origin-blind. Three facts settle it, all
verified in the source rather than inferred. `spill()`'s entire guard is
`output.is_error || output.content.chars().count() <= MAX_INLINE_CHARS`
(`dispatch.rs` 232–235): nothing in it distinguishes a result bound for a
context window from one bound for a Rune loop, so it applies to scripts not
by decision but because nobody had the chance to say otherwise. The
mechanism's own module doc names its audience — *"what travels inline is
capped … the model pages the rest"* — and a script is not a context window.
And the design of `eidolon::dispatch` was accepted on the promise that a
script-originated call is *"journaled as its own `ToolResult`, published on
the event bus"* — a record, never the transport.

Also rejected: a flag on the call (`bounded: false`) a script could pass. A
script can lie about what it needs; it cannot lie about its origin, which
`Host::dispatch` sets and nothing downstream can change. Also rejected: a
larger `MAX_INLINE_CHARS` for scripts. Any number is a guessed context window
for a consumer that has none, and a guessed window is how a turn dies at a
boundary nobody can see.

### Why the whole output is safe to hand a script

The concern behind `spill()` is a model's context. Two facts, both from
source, show a script-originated result can never reach one except through a
boundary that is still spilled:

- **The journal does not fold it into the model's messages.** `Session::messages`
  pairs a `ToolResult` record with the `tool_use` block that asked for it or
  with a `UserToolCall` record, and with nothing else (`session/mod.rs`, the
  `RecordKind::ToolResult` arm of the fold). A script call's id is minted by
  `fresh_id` as `script_<name>_<millis>_<n>` and matches neither. The record
  is written, and the model never sees it.
- **A replay cannot hand the bounded record back to the script.** `replayed()`
  matches on the call id; `fresh_id` is process-unique and time-stamped, so a
  resumed session never reproduces one. (The named wrappers that *do* rely on
  replay — `choices_user` — carry short answers and are unaffected.)

What remains is the *outer* call. `jev_run` is a `Model` (or `User`) call, and
what it returns — the escalation payload or the final report — passes through
its own `spill()` at its own boundary. A script that forwarded an 800 KB inner
result verbatim to a model would still deliver a receipt. Context protection
is therefore enforced at the only boundary where context exists, once, rather
than at every hop on the way there.

### The failure mode of this answer

"Scripts get unlimited output" has one, and it is stated rather than waved at:
**the only bound on a script-originated result is the producer's own.**
Today's producers each have one — `shell::MAX_OUTPUT` at 512 KB with an
in-band trailer, `MAX_BODY` at 256 KB (which §3 turns into a refusal),
`fs_read`'s `limit`, `web_fetch`'s `max`, `grep`'s `max`. A producer with no
cap — a Rune script building a string in a loop — is a defect in the producer,
and the size at which this path is *proved* to work is the fixture size of the
tests in the spec (four times `MAX_INLINE_CHARS` in `crates/core`, one byte
under `MAX_BODY` through a real fake service in `crates/rune`). The second
cost is disk: the spill file is still written for every large script result so
the journal's receipt stays honest, and nothing prunes `*.spills` directories
today for any origin — the same gap S18 names for session logs, and not
enlarged by this ruling so much as exercised more often.

### The `is_error` exemption goes

`spill()` exempts error outputs on the reasoning that *"a denial or a
traceback arrives short."* That is an assumption about producers, and §4
breaks it on purpose: a failing `bash` becomes `is_error`, and a failing build
can print half a megabyte. Left in place, the exemption would turn every long
failure into an unbounded write into a model's context — the precise thing
`spill()` exists to prevent.

So `is_error` stops doing double duty. It means *the tool failed* and nothing
else. Whether an output is bounded is decided by size and origin alone, and a
bounded error stays `is_error: true` on its receipt (today's receipt hard-codes
`is_error: false`, which was harmless only because errors never reached it).
The cost is real and named: a long failure's message is usually at the *tail*
— `Exec::render()` prints stdout, then stderr — and the model now sees the
exit line, 2,000 characters of head and a path, and must `read` the tail. That
is one `read` per long failure, against a turn that would otherwise die.
Whether it happens often enough to justify a tail-preserving receipt is
measurement M4 in §6; this ruling does not guess.

## 2. Where an observation is narrowed

**Choice.** At the source. `browser_snapshot` gains `within: <role>`, and the
service answers with the accessibility subtree rooted at the one element on
the page that has that landmark role, produced by Playwright's own
`Locator.aria_snapshot(mode="ai")`. The head block gains `scope: <role>` so
the observation says what it is a snapshot of. A page with no such element, or
more than one, is **refused by name** — the same shape as a stale ref — and
the refusal names the role, the count and the URL.

**Rejected.** Filtering at the consumer, which is what wiki-hop does today.
The argument for it is that the service stays dumb and general. The argument
against is not speed, though the numbers are bad — 30.4 s cold, 12.9 s warm,
for a tree of which the graph uses 87 lines. It is that filtering after the
fact is **impossible**, not slow: 814,323 characters do not fit down a pipe
whose cap is 262,144, so the consumer never holds the thing it would filter.
The only ways to make the consumer-side filter correct are to raise the cap
to the size of the largest page on the web, or to let a page that does not
fit be silently partial. The first is not a number anyone can name; the second
is today's bug.

Also rejected: making the service understand graphs. It does not. `within` is
Playwright's own subtree capability exposed under its own name, beside `depth`
and `timeout_s` which are already there for the same reason; the value is a
role string the *browser* interprets, so the service carries no copy of jev's
landmark list and the two cannot drift. What `main` means is decided in the
process that renders `main`.

### Why the ref contract survives

Three facts, from the installed Playwright 1.63.0's own source
(`driver/package/lib/coreBundle.js`), **reasoned and not yet observed** —
the spec makes each one a test against a real local page:

- `Locator.aria_snapshot(mode="ai", depth=…, timeout=…)` exists with the same
  parameters as `Page.aria_snapshot` (`async_api/_generated.py`, the `Locator`
  class).
- A subtree snapshot renders the root element itself: `generateAriaTree`
  wraps `rootElement` in a `fragment` and *visits* it, and the method's own
  docstring shows `<ul aria-label="Links">` rendering as `- list "Links":` at
  the top of its output. So a `main`-scoped snapshot begins `- main [ref=…]:`,
  and `a11y.py`'s landmark propagation — which reads nesting — needs no change
  for `refs.within: "main"` to keep working.
- The `aria-ref=` selector engine resolves a ref against
  `_lastAriaSnapshotForQuery`, which is set by *whichever* snapshot ran last,
  full-page or subtree. So a ref minted by a scoped snapshot is clickable, and
  a scoped snapshot invalidates the previous snapshot's refs — which is exactly
  the rule `service.py` already enforces by replacing `known_refs` wholesale.
- And: in `mode="ai"` a locator snapshot *"does not wait for an element
  matching the locator, and throws when no elements match"* — the docstring's
  words. The refusal for a page with no `main` is Playwright's; the service
  only has to name it.

### What a consumer that wants the whole tree does afterwards

It asks for it. `browser_snapshot` with no `within` still returns the whole
tree, and for every page that fits `MAX_BODY` nothing changes. For a page that
does not fit, it gets an error naming the size and the cap instead of 2,000
characters of banner navigation and a spill path, which is what a model gets
today and has never once been useful. A graph whose menu genuinely spans two
landmarks — links from `navigation` and `main` in one list — has no scope that
names both; it uses an unscoped snapshot on pages that fit, or `depth`, and
that limit is the cost of this ruling, stated. One scope per snapshot, one
snapshot per action.

`refs.within` stays in the graph. The snapshot's `within` decides what crosses
the process boundary; the source's `within` decides what becomes an option.
They name the same landmark and mean the same thing at two moments, and the
second is not redundant: it is what keeps the option list right when someone
removes the first on a page small enough not to need it.

## 3. Whole, or an error, or a receipt

**Choice.** There is no truncation protocol between layers, because there is
no truncation between layers. A tool result crossing any boundary is one of
three things: **whole**; an **error** (`is_error`, or the transport's own
`Err`); or a **receipt**, which only `Dispatcher::spill` mints, only for a
result a model will read, and which is journaled as such. `clip()` in
`crates/tools` stops cutting: a service answer over `MAX_BODY` is refused
with an error that names the count and the cap. `_read()`'s `truncated` field
is not a fourth protocol and never was — it reports a bound the *caller asked
for* (`max`), which is part of the tool's result, not something the transport
did to it. S26's "pick one" is answered by there being one: transport
truncation is abolished, and the field stays.

**Rejected.** One text-marker grammar every layer appends. It is what S23
built, and it died at the next layer out because a head-preserving cut removes
a tail marker by construction. A marker is an *expectation* placed on layers
that do not know about each other. Also rejected: a structured `truncated`
field on `ToolOutput` threaded through to Rune. It would be correct inside
Rust and dead at the first `String` boundary — `Host::dispatch` returns
`Result<String, String>`, `service_call` returns a `String`, and every `.rn`
tool returns one — so it would protect precisely the segment that has never
cut anything. Also rejected: raising `MAX_BODY`. Its own comment is right: a
method returning more than that is returning a file. And a clipped **JSON**
result was never shortened, it was corrupted — `render()` clips a
pretty-printed object mid-token — so the refusal is not a stricter policy but
the first correct one for any structured consumer.

### How a fourth layer is caught

The honest answer has two tiers, and the spec builds both.

**A producer that states its length is protected by construction.** The head
block a snapshot already carries (`url:`, `title:`) gains `chars: N`, the
length of the body that follows the blank line, counted by the producer. The
consumer counts the body it received and refuses on any disagreement. The
declaration travels *inside* the payload, at its head, so it survives every
`String` boundary and every head-preserving cut, and an ignorant layer that
shortens the body cannot fix the number it does not know about. A layer that
cuts the head loses the declaration — and a snapshot without its head is not a
snapshot, which is belief 3 and is refused on that ground. The only layer this
does not catch is one that rewrites the stamp to match its cut, and that is a
lie, not an accident; it is not the threat this document is about. `chars`
counts code points: Python `len(str)` at the producer, Python `len(str)` at
the consumer, with nothing in between that counts anything — Rust's
`chars().count()` in `whole()` is a threshold, not a measurement the consumer
sees. The two counts are in the same unit by construction. (The 826,491 the
review quoted was bytes; the 814,323 J1 measured was characters. This is why
the unit is named.)

**An unstamped producer is protected by the invariant, and the invariant is
tested at the chokepoint.** `bash` output carries no head and is not going to
grow one — its shape is a model-facing contract in every transcript. For it,
"whole" is a property of the transport, and the spec pins it with two tests: a
registered tool returning four times `MAX_INLINE_CHARS` reaches a
script-originated caller byte-identical (`crates/core`), and a real fake
service answering one character under `MAX_BODY` reaches a driver script
byte-identical through `service_call` and `interpret_plain`
(`crates/rune`). A new transport is expected — not forced — to keep those
tests' property, and the rule is written where a new transport's author will
read it (`extensions.md`, `RUNE.md`). That is the limit, stated: the stamp
protects stamped producers against an unknown layer; the tests protect
everyone against the known ones; every extension service that returns text a
graph will observe should stamp.

What a producer's own bound looks like from the node's side is then simple:
**a bound the caller asked for is a fact in the observation and a graph may
guard on it; a bound the transport imposed is invalidity.** `_read`'s
`truncated: true` and `bash`'s `[output truncated]` trailer are the first kind.
Nothing in this document makes jev check them; a graph that cares writes a
guard.

## 4. An invalid observation is one thing

**Choice.** One concept, `ERROR`, with a structured reason. The three findings
J1 handed over are three ways for the same belief-set to fail, and the node
does the same thing in each case: it does not store the observation, does not
run `always` guards over it, does not build options from it, and raises
`ERROR` with `{error, reason, tool}` where `reason` is one of:

| `reason` | which belief | who vouched, and how |
|---|---|---|
| `failed` | success | the tool: `is_error`, arriving as `{"error": …}` from the driver |
| `truncated` | whole | the producer's `chars:` against the consumer's count |
| `unexpected` | kind | the action's `expect` guard, evaluated over the candidate observation |
| `stale` | (a pick, not an observation) | the runner: a `PICK` naming a ref absent from the current snapshot |

`stale` is listed because it is the same family — *the runner must not act on
data it cannot trust* — and because it already rides `ERROR` today; giving it
a reason costs nothing and stops it looking like a fourth kind of thing.

**Rejected.** A new reserved event per kind (`TRUNCATED`, `INVALID`). Each
would need a place in the schema's reserved set, in `RESERVED_EVENTS`, in the
`transitions` source's exclusion list, in the lint, in the XState conformance
test, and its own unhandled default. `ERROR`'s unhandled default — outcome
`error`, the run ends and the report names the reason — is already the right
one: in bootstrap mode a person is driving, the correction is to the graph in
the editor, and a run that stops with *"observation from `browser_snapshot`
was truncated: 2,000 of 814,323 declared characters"* is a run that told them
which line to fix. Also rejected: parking on an invalid observation, the way
`EMPTY` parks. A park is a question with a menu; an invalid observation has no
menu, only a diagnosis.

Also rejected: letting the runner *decide* what a process listing looks like.
It cannot. What it can do is refuse to score anything the graph did not vouch
for, and give the graph one place to vouch.

### `expect`: the graph says what it asked for

A `tool` action gains an optional `expect`, holding a **guard** from the
closed set `automation.md` §3 already defines — `matches`, `contains`,
`exists`, `count`, `entails`, `contradicts` and the three combinators. It is
evaluated once, after the result is parsed and before it is stored, over a
scope in which `context.<into>` is the candidate observation. It passes or the
observation is invalid with `reason: unexpected`. It is the same guard
grammar as `always` and the same evaluator (`guards.evaluate_guard`), so there
is no second guard system, and an NLI `expect` is allowed for the same reason
an NLI `always` is: the author is paying for the call and knows it.

Why this and not an `always` transition to `recover` with a negated guard,
which the schema already allows? Because that is a *transition*, and it needs
a target the author has to invent per state; because it runs *after* the
observation has been stored and every other `always` guard — possibly an NLI
call over garbage — has been evaluated against it; and because the run record
would show an ordinary transition where what happened was an invalid
observation. `expect` is attached to the action whose result it vouches for,
fires first, and produces the same `ERROR` shape as the other two failures.
That is what "one concept" means in code.

The `judge` finding is closed twice over. With `bash` honest about exit codes,
a `ps` that fails is `failed` and reaches `recover`, which was architecturally
unreachable before and is now the first thing the amended triage graph
exercises. With `expect` on the `processes` action, a `ps` that *succeeds* and
prints something that is not a listing is `unexpected` and reaches the same
place. The chooser is never handed a usage message to score at 0.995, because
nothing that has not been vouched for reaches the chooser.

### `bash`: a nonzero exit is a failure

**Choice.** The built-in `bash` returns `is_error: true` when `Exec::ok()` is
false — a nonzero exit, a timeout, a cancellation, a kill by signal — and
`false` otherwise, including for a truncated or backgrounded output. The
content is unchanged: `Exec::render()`'s text, which already leads with the
status line. The mechanism is a sibling primitive, `eidolon::shell_exec`,
returning the `Exec` fields as an object; `eidolon::shell` keeps returning a
`String` so no operator's `.rn` changes meaning (the rule from the D3
near-miss: a shared signature is shared state, widen by adding a sibling).

**Rejected.** Having jev recognise `[exit code` at the head of a `bash`
observation. That is a second implementation of `Exec::ok()`, in Python,
keyed on a render format the Rust side owns and may change. The process that
runs the command is the process that knows whether it failed.

The consequence for graphs is the one `automation.md` §1 always described:
*"`ERROR` is raised when a tool action returns an error."* It was never true
for the commonest error a command can have. The consequences for models are
two, both stated: a failing command's result is flagged as the tool ecosystem
flags it (a `tool_result` with `is_error`), and — with §1's exemption gone —
a *long* failing command's output is bounded, with its tail in the spill file.
`grep` exiting 1 on no match becomes a flagged result; that is what it is, and
a model reads `[exit code 1]` exactly as it reads it today.

And a consequence for the triage graph, stated because it is a visible change:
on a box whose `ps` rejects `-o` — this one — the run no longer *reaches*
`report` by judging an error message. It enters `recover`, and if `CONTINUE`
sends it back through `hist` to `processes` it fails the same way, until a
budget — `visits` on `processes`, or `escalations` — ends it. That is the
true outcome of that command on that box. A graph that wants to survive it
names a second listing command; that is graph authoring, not runner design,
and it is left to D4.

### What D8 needs to know

Dispatches per decision do not change. wiki-hop still makes one snapshot and
one click per hop; the triage graph one command per pick; `expect` dispatches
nothing; the service's `count()` is inside the service. The 61 actions J1
counted were 30 `browser_back` no-ops against `about:blank` — spent, not
needed — and they disappear, so a real run makes *fewer* dispatches in total
while the per-decision number D8's 62 is built on stays where it was.

## 5. What this costs, and what it does not solve

- **The decision row grows with the tree it scores.** `choose.context` for
  wiki-hop embeds `context.obs.text`, stored whole in the row by design. It
  used to be a 2,000-character fragment nobody could learn from; it becomes
  the `main` subtree, whose size is measurement M1. Until it is measured, the
  bound is `MAX_BODY`.
- **Spill files accumulate.** For every origin, as today; more often now.
  Nothing prunes them. Not enlarged by this ruling, only exercised.
- **`browser_read` is not a graph observation yet.** It answers JSON, and a
  graph reading `obs.text` gets a JSON blob. It keeps its shape here, because
  23 tests and a model-facing contract depend on it; giving it the head block
  is a separate, small change and is not in this spec.
- **A `bash` output over 512 KB is cut in-band by its producer** with a tail
  trailer and no stamp. A graph that cares guards on the trailer. jev does not
  validate it, and this document says why (§3): it is the producer's own
  bound.
- **One scope per snapshot.** A menu spanning landmarks uses an unscoped
  snapshot and pays the size.
- **The tail of a long failure needs a `read`.** §1; measured by M4.
- **The triage graph fails visibly on a box with no usable `ps`.** §4.
- **`ask: user`, detached runs, `act`-suspended persistence**: untouched.
- **An `expect` guard is authored, not inferred.** A state that does not
  declare what it expects gets what it always got. The shipped triage graph
  declares one; wiki-hop's arrival guards already are its expectation and it
  gains none.

## 6. Measurements not yet taken

Each names the fallback that holds until it is taken. None is guessed.

- **M1 — the size and cost of a `main`-scoped Cat snapshot.** Characters and
  wall seconds, cold and warm, on this box. Expected to be a fraction of
  814,323 and of 30.4 s, and *not stated as any number*. Until measured:
  `MAX_BODY` stays 256 KB, `SNAPSHOT_TIMEOUT_MS` stays 60,000, and if the
  scoped tree still exceeds `MAX_BODY` the graph adds `depth`, whose value is
  a second measurement. The browser step of the spec takes M1 as part of
  capturing the fixture.
- **M2 — a ref minted by a `Locator` snapshot resolves through `aria-ref=`.**
  Reasoned from `coreBundle.js`; test T13 observes it. Fallback if false: the
  service keeps the `within`/`scope:`/`chars:` contract and implements it by
  taking the full snapshot and cutting the text to the landmark's own
  indented block before answering — still at the source, the 814 KB never
  crosses the process boundary, but the 30 s cost returns.
- **M3 — a `Locator` snapshot renders its root element's own line.** Reasoned
  from the docstring; test T12 observes it. Fallback if false:
  `a11y.parse_refs` gains `default_landmark: str | None`, and `build_obs`
  passes `obs.scope` into it, so every record in a scoped snapshot carries
  the scope as its landmark. One parameter; not built until needed.
- **M4 — how often a failing command's message lies outside the 2,000-char
  head.** Read from transcripts after §1 lands. Fallback: none required; the
  receipt names the file and the total, and the model pages it.
- **M5 — Linux.** The `expect` pattern on the triage graph's listing,
  `^COMMAND\s+PID\s+USER`, is reasoned from procps's documented column
  headers for `comm,pid,user` and has not been observed on Linux; nothing
  else in this document has content that varies by platform, and every code
  path it touches is one already reviewed as branch-free or branch-paired.
  The pattern is the one Linux claim, and it is marked as such in the graph.
- **M6 — the size at which a script-originated result has been proved to
  arrive whole** is the fixture size of T1 and T6, four times
  `MAX_INLINE_CHARS` and one under `MAX_BODY`. Anything larger is expected,
  not proved.

---

## Implementation specification

Five steps, three agents, in this order. Each step leaves the tree compiling
and its own suite green. Nothing here reads a document; every constant, name
and call site is stated.

| step | tree | agent | depends on |
|---|---|---|---|
| 1 | `eidolon/crates/core` | Rust | — |
| 2 | `eidolon/crates/tools` | Rust | — (land after 1 for a coherent binary) |
| 3 | `extensions/browser`, plus one captured fixture written into `jev/tests/fixtures/wiki/` | browser | — |
| 4 | `jev/`, `extensions/jev/graphs/`, `docs/design/automation.md` | jev | 3 (the fixture) |
| 5 | `eidolon/crates/rune`, `eidolon/crates/cli/system/RUNE.md`, `eidolon/docs/extensions.md` | Rust | 1 (the exemption must be gone before `bash` can error long) |

The release binary is stale until steps 1, 2 and 5 are built; step 4's suite
runs against fixtures and does not need it, but the live graph does.

Test names below are the names to use. For every change, the test named
beside it is the one that fails if the change is reverted; a change without
one is documentation.

### Step 1 — `crates/core`

**`eidolon/crates/core/src/dispatch.rs`**

1. `fn spill(&self, call: &ToolCall, output: &ToolOutput) -> ToolOutput` —
   borrow, not move. Guard becomes
   `if output.content.chars().count() <= MAX_INLINE_CHARS { return output.clone(); }`.
   The `output.is_error ||` clause is deleted. The returned `ToolOutput` sets
   `is_error: output.is_error` (today: `false`).
   Test: `a_long_error_output_spills_like_any_other_and_stays_an_error`
   (replaces `an_error_output_is_never_spilled`, which asserts the opposite
   and is deleted).
2. The receipt's wording takes the origin. For `Model`/`User` it is unchanged.
   For `Script`:
   `"{head}\n[…output too long for the record ({total} chars); the script received it whole; the whole {bytes} bytes are in {path}]"`,
   and the could-not-write variant likewise says the script received it whole.
   Test: covered by `a_script_originated_call_is_handed_the_whole_output`'s
   journal assertion (the record contains `received it whole`).
3. `dispatch_with`, after `let output = self.execute(&call, cancel).await;`:
   ```rust
   let to_script = matches!(call.origin, CallOrigin::Script);
   let recorded = self.spill(&call, &output);
   let record = self.journal(&call, &recorded).await;
   self.bus.publish(Event::ToolCallFinished { record, call, output: recorded.clone() });
   if to_script { output } else { recorded }
   ```
   The comment above it ("Spill before journaling…") is rewritten to say:
   the journal and the bus hold the bounded form for every origin; a model
   or the operator is handed that same form so a resume replays what they
   saw; a script is handed the whole, because its output is bounded at its
   own caller's boundary and never enters a context through this record
   (`Session::messages` pairs a result only with a `tool_use` or a
   `UserToolCall`, and `fresh_id` never collides).
   Tests: `a_script_originated_call_is_handed_the_whole_output`,
   `a_user_originated_call_gets_the_receipt`,
   `the_bus_carries_the_bounded_form_for_a_script_call`.
4. Module doc, the "Large outputs spill" section (lines 54–62): replace with
   a paragraph saying what a consumer receives depends on who it is — a model
   or the operator get the head plus a receipt and page the rest with `read`;
   a script gets the whole, because context protection belongs at the
   boundary where a context exists, which is the script's own caller; the
   journal, the bus and the spill file are the same for every origin; error
   outputs are bounded like any other, and stay errors. No test.

**`eidolon/crates/core/tests/spill.rs`**

- Keep `Flood`, `Drip`, `Boom`, `manifest`, `dispatcher`, `call`. `Flood`
  (line 32) returns `"x".repeat(4 * MAX_INLINE_CHARS)` instead of
  `MAX_INLINE_CHARS + 1_000`, so the fixture is M6's size. `Boom`'s comment
  says errors spill like anything else. Add
  `fn call_with(id: &str, name: &str, origin: CallOrigin) -> ToolCall`, the
  body of `call` with the origin as a parameter; `call` becomes
  `call_with(id, name, CallOrigin::Model)`. The file's module doc (lines
  1–9) gains one sentence: what is handed back depends on the origin — a
  script gets the whole, and the journal holds the bounded form for every
  origin.
- `a_script_originated_call_is_handed_the_whole_output`: dispatch
  `call_with("s1", "flood", CallOrigin::Script)`; assert
  `out.content.chars().count() == 4 * MAX_INLINE_CHARS` and `!out.is_error`;
  assert the last `ToolResult` record's content has fewer than
  `MAX_INLINE_CHARS` chars, starts with the `SPILL_HEAD_CHARS` head, and
  contains `received it whole`; assert the spill file `s1-flood.txt` exists
  and holds `4 * MAX_INLINE_CHARS` bytes.
- `a_user_originated_call_gets_the_receipt`:
  `d.dispatch_user(call_with("u1", "flood", CallOrigin::User), "run flood", CancellationToken::new()).await`;
  assert the content has fewer than `MAX_INLINE_CHARS` chars and names
  `u1-flood.txt`. (Fails if the gate is written as "not `Model`" instead of
  "`Script`".)
- `the_bus_carries_the_bounded_form_for_a_script_call`:
  `let mut rx = d.bus().subscribe();` before dispatching the `Script` call;
  drain `rx` to the `Event::ToolCallFinished { output, .. }` for it; that
  `output.content` has fewer than `MAX_INLINE_CHARS` chars while the
  returned value has `4 * MAX_INLINE_CHARS`.
- `a_long_error_output_spills_like_any_other_and_stays_an_error`: dispatch
  `call("spill-boom", "boom")` (Model); assert `out.is_error`, content under
  `MAX_INLINE_CHARS`, receipt names `spill-boom-boom.txt`, the file holds the
  whole, and the journal record has `is_error: true`.
- `a_long_output_spills_to_a_file_beside_the_log`,
  `a_short_output_is_not_spilled`,
  `without_a_spill_dir_the_overflow_lands_beside_the_cwd`: unchanged in
  intent; adjust the byte counts to the new `Flood` size.

### Step 2 — `crates/tools`

**`eidolon/crates/tools/src/service.rs`**

1. New `fn whole(s: &str, method: &str) -> anyhow::Result<String>`: if
   `s.chars().count() > MAX_BODY`, `Err(anyhow!("`{method}` answered {n} characters, over the {MAX_BODY}-character limit for a service result; a result that large is a file — narrow the request, or have the service write it to disk and answer with the path"))`;
   else `Ok(s.to_string())`.
2. `render` (line 264) becomes
   `fn render(value: Option<&serde_json::Value>, method: &str) -> anyhow::Result<String>`
   and routes the `String` arm and the pretty-JSON arm through `whole`; its
   caller at line 172 becomes `Some(true) => render(value.get("result"), method)`.
   `call()`'s two `clip(&text, MAX_BODY)` sites — line 169 (a body that is
   not JSON) and line 181 (an envelope with no `ok`) — become
   `whole(&text, method)`. The two `clip(&text, 1000)` sites — line 166 (a
   non-2xx body) and line 179 (an `ok: false` envelope with no `error`
   string) — stay: they bound a diagnostic inside an `Err`, which is not a
   result.
3. `clip` keeps its name and its two remaining callers; its marker text is now
   unreachable from any `Ok`. `MAX_BODY`'s doc comment: "A service answer at
   or under this many characters arrives whole; over it, the call is refused
   with the count — never shortened, because a shortened answer is a
   different answer and a shortened JSON one is not JSON."
   Tests: `an_answer_over_max_body_is_an_error_naming_the_count_not_a_clipped_ok`
   (a `Fake` answering `{"ok":true,"result":"<MAX_BODY+1 x's>"}`; `call`
   returns `Err` whose text contains `262145` and `262144`);
   `an_answer_at_max_body_arrives_whole` (exactly `MAX_BODY` characters,
   returned byte-identical); `a_result_string_comes_back_as_itself` adjusted
   for the `Result` signature. `a_non_200_says_what_came_back` unchanged,
   proving the diagnostic clip survives.

### Step 3 — `extensions/browser`

**`extensions/browser/service.py`**

1. `_head` (line 200) becomes
   `def _head(url: str, title: str, *, scope: str | None = None, chars: int | None = None) -> str`
   and returns `url: …\ntitle: …\n`, then `scope: {scope}\n` when given, then
   `chars: {chars}\n` when given, then `\n`. Docstring: the head is a block of
   `key: value` lines ending in a blank line; `url` and `title` first, in that
   order, for every reader that predates the block; `chars` is the length of
   the body after the blank line in code points, so a consumer can verify it
   received the whole; `scope` is the landmark role the tree is rooted at.
   The false claim that `_read` shares it is deleted (S26).
2. `_snapshot`:
   ```python
   within = args.get("within")
   if within is not None:
       if not isinstance(within, str) or not within:
           raise ValueError("within must be a landmark role name, e.g. \"main\"")
       target = page.get_by_role(within)
       n = await target.count()
       if n != 1:
           raise ValueError(
               f"within={within!r}: {n} elements with that role on {page.url}; a scope names exactly one"
               + (" -- use an unscoped snapshot, or depth" if n > 1 else "")
           )
       tree = await target.aria_snapshot(mode="ai", timeout=float(timeout_s) * 1000, depth=depth)
   else:
       tree = await page.aria_snapshot(mode="ai", timeout=float(timeout_s) * 1000, depth=depth)
   STATE.known_refs = frozenset(REF_RE.findall(tree))
   return _head(page.url, await page.title(), scope=within, chars=len(tree)) + tree
   ```
   The comment above `depth` (lines 225–233) is extended: `within` is the
   other escape hatch, and the one a graph should reach for first, because
   it changes what crosses the wire rather than how deep the walk goes; the
   service does not know what `main` is — the browser does, through
   `get_by_role`.
   Tests (all in `RealBrowserWalkTests`' style — real headless Chromium,
   pages given as `data:text/html,` URLs the way `PAGE` at line 70 is, no
   network):
   - `a_scoped_snapshot_renders_the_scope_as_its_root_line_and_nothing_outside_it`
     — a page with `<nav><a>Skip</a></nav><main><h1>T</h1><a>In</a></main>`;
     `_snapshot({"within": "main"})`; the head has `scope: main` and
     `chars: N` with `N == len(body)`; the first tree line starts with
     `- main`; `navigation` and `"Skip"` do not appear. (M3.)
   - `a_ref_from_a_scoped_snapshot_is_clickable_and_replaces_the_previous_snapshots_refs`
     — full snapshot, take a ref from `nav`; scoped snapshot, take the `In`
     link's ref; `_click` on the scoped ref succeeds; `_click` on the nav ref
     is refused by name. (M2.)
   - `a_scope_that_matches_nothing_is_refused_by_name` — `about:blank`,
     `within: "main"`; `ValueError` naming `main`, `0` and the url.
   - `a_scope_that_matches_more_than_one_element_is_refused_with_the_count`
     — two `<section role="region" aria-label=…>`, `within: "region"`;
     `ValueError` naming `2`.
   - `an_unscoped_snapshot_stamps_chars_and_carries_no_scope_line`.
   - `SnapshotArgsTests`: `within` forwarded to `get_by_role` when present
     and not otherwise (mocked, as `depth` is today).
3. `_read` unchanged.

**`extensions/browser/tools/snapshot.rn`** — `within` added to
`input_schema.properties` ("Restrict the snapshot to the one element with this
landmark role — `main`, `navigation`, `banner`, `contentinfo`, `complementary`,
`search`, `region`. The tree is rooted at that element and carries its refs; a
page with no such element, or more than one, is refused by name. The first
thing to reach for on a large page, before depth.") and forwarded to
`service_call("snapshot", …)` only when present. The two-literal `match` on
`depth` and the comment justifying it are replaced by one object mutated in
place, the idiom `crates/tui/ui/default.rn` lines 570–572 already use:
```rune
let args = #{ "timeout_s": timeout_s };
if let Some(d) = input.get("depth") { args.insert("depth", d); }
if let Some(w) = input.get("within") { args.insert("within", w); }
eidolon::service_call("snapshot", args).await
```
Verified by `eidolon ext list` compiling the tool, and by two
`SnapshotArgsTests` cases driving `_snapshot` directly: `within` reaches
`get_by_role` when given, and `page.aria_snapshot` is what runs when it is
not (mocked, as the `depth` cases there are).

**The capture** — after the tests pass, against the live service: open
`https://en.wikipedia.org/wiki/Cat`, `_snapshot({"within": "main"})`, and
write the exact result — head block included, byte-for-byte — to
`jev/tests/fixtures/wiki/cat_main_scoped.txt` (a new file; nothing in `jev/`
is edited). Record M1 — `chars`, the `- link` count, cold and warm wall
seconds — in the Build-Log entry, as numbers observed on this box.

### Step 4 — `jev/`

**`jev/automation/a11y.py`**

1. `_HEAD_RE = re.compile(r"^url: (?P<url>[^\n]*)\ntitle: (?P<title>[^\n]*)\n(?P<extra>(?:[a-z][a-z_]*: [^\n]*\n)*)\n")`.
2. `def parse_head(raw: str) -> tuple[dict | None, str]` — `None, raw` when
   the block is absent; otherwise a dict with `url`, `title`, and every extra
   line's key and string value, and the rest. `lift_head` becomes a wrapper
   returning `(head["url"], head["title"], rest)` so `LiftHeadTests` still
   pass unchanged.
3. `def declared_length(raw: str) -> int | None` — the `chars` value as an
   int when present and numeric, else `None`.
4. `is_truncated(raw)` is **redefined**: `declared_length(raw)` is not `None`
   and `len(rest) != declared`. `_TRUNCATION_RE` and its comment block are
   deleted. Docstring: the producer states the body's length in the head; a
   body of any other length was shortened by something between the producer
   and here, whatever that something was, and this function does not need to
   know its name. An unstamped result answers `False` — unverifiable, not
   whole.
5. `build_obs` adds `"scope": head.get("scope")` and
   `"chars": declared_length(raw)` (both `None` when unstamped).
   Tests (`test_a11y.py`): `IsTruncatedTests` is rewritten —
   `a_stamped_body_shorter_than_declared_is_truncated`,
   `a_stamped_body_longer_than_declared_is_truncated`,
   `a_stamped_body_of_the_declared_length_is_whole`,
   `an_unstamped_body_is_not_reported_truncated`; the seven marker tests are
   deleted. `HeadBlockTests`: `scope_and_chars_are_lifted`,
   `unknown_head_keys_are_kept_and_ignored`,
   `the_two_line_head_still_lifts_with_no_extras`. `RealWikipediaCaptureTests`:
   `the_scoped_capture_is_whole_by_its_own_declaration` (loads
   `cat_main_scoped.txt`, `is_truncated` is `False`, `obs.scope == "main"`) and
   `every_link_in_the_scoped_capture_has_main_as_its_landmark`.

**`jev/automation/run.py`**

1. `class _ToolError(Exception)` (line 165) gains `reason: str` and
   `tool: str`: `__init__(self, text: str, *, reason: str, tool: str)`.
2. `_run_tool_action` (line 364), after `result = yield request`:
   ```python
   error = (result or {}).get("error")
   if error:
       raise _ToolError(str(error), reason="failed", tool=name)
   text = (result or {}).get("text") or ""
   declared = a11y.declared_length(text)
   if declared is not None:
       _, body = a11y.parse_head(text)
       if len(body) != declared:
           raise _ToolError(
               f"observation from {name!r} was truncated in transport: {len(body)} of {declared} declared characters arrived",
               reason="truncated", tool=name)
   candidate = a11y.build_obs(text)
   expect = params.get("expect")
   if expect is not None:
       probe = rt.scope()
       probe["context"] = {**rt.context, into: candidate}
       if not guards.evaluate_guard(expect, probe, rt.warnings, entail=rt.entail, default_threshold=rt.default_threshold()):
           raise _ToolError(
               f"observation from {name!r} did not satisfy expect ({expect['type']}): not the kind of result this action asked for",
               reason="unexpected", tool=name)
   rt.context[into] = candidate
   ```
   The old `is_truncated` comment block is replaced by three lines naming the
   three beliefs and `docs/design/observation.md` §4.
3. `_finish_choice`'s stale-ref raise becomes
   `_ToolError(..., reason="stale", tool="pick")`.
4. Both `_fire_event(rt, rt.active, "ERROR", …)` sites pass
   `{"error": e.text, "reason": e.reason, "tool": e.tool}`.
5. `_RunState` (line 202) gains `self.error_reason: str | None = None`
   beside `self.error`; `_fire_event` (line 560), in its
   `if event == "ERROR":` arm, sets `rt.error_reason = event_data.get("reason")`
   beside `rt.error`; `_final_report` (line 1248), in its
   `if rt.outcome == "error":` arm, adds `report["reason"] = rt.error_reason`
   — the same `reason` key the `stopped` arm already emits, which is safe
   because the two outcomes are exclusive.
   Tests (`test_run.py`):
   - `TruncatedSnapshotTests` (line 1243) rewritten as
     `InvalidObservationTests`:
     `a_stamped_result_whose_length_disagrees_is_an_error_with_reason_truncated`
     (a snapshot whose head says `chars: 999999`; outcome `error`, `reason ==
     "truncated"`, the message names both numbers, `context.obs` never set);
     `a_stamped_result_whose_length_agrees_is_stored`;
     `an_unstamped_result_is_stored_without_a_verdict_on_wholeness`.
   - `a_driver_error_is_reason_failed_and_names_the_tool`, in
     `ErrorEventTests` (line 816).
   - `a_failing_expect_is_reason_unexpected_and_the_observation_is_not_stored`
     (`bash` returns `{"text": "ps: unknown option -- o\nTry `ps --help'"}`
     with a `matches ^COMMAND` expect; `recover` is entered; `context.obs` is
     the previous observation, not the usage text).
   - `a_passing_expect_stores_the_observation`.
   - `an_expect_may_be_an_nli_guard` (mocked `entail`; one call; pass and
     fail).
   - `a_stale_ref_is_reason_stale` (extends `StaleRefTests`, line 1174).
   - `TriageLinuxEndToEndTests.test_a_failed_listing_command_reaches_recover`
     — the J1 case: the `ps` entry returns `{"error": "[exit code 1]\nps: unknown option -- o"}`;
     the run enters `recover` with `notes` ending in the error, which is the
     first test in this suite to exercise `recover` from a command rather than
     from a scripted dispatch failure (closing D7's finding).
   - `WikiHopEndToEndTests.test_the_snapshot_is_requested_within_main` —
     `driver.calls` shows every `browser_snapshot` request with
     `input == {"within": "main"}`.
   - `WikiHopRealCaptureTests` gains a class using `cat_main_scoped.txt` as
     the first snapshot: it is accepted as whole, `h1` is `Cat`, and the
     option list is non-empty with every option's landmark `main`.

**`jev/automation/graph.py`** — `_validate_action` (line 596), the `tool`
branch: the `_unknown_keys` call at line 614 becomes
`_unknown_keys(params, {"name", "input", "into", "timeout_s", "expect"}, …)`;
when `"expect" in params`, call the existing `_validate_guard` (line 504) as
`_validate_guard(params["expect"], f"{pwhere}.expect", errors)`, with the
same `where`/`errors` arguments the surrounding branch already passes.
Tests (`test_schema.py`, `NegativeLintTests`):
`an_expect_that_is_not_a_guard_is_rejected_naming_the_action`,
`an_expect_with_an_unknown_guard_type_is_rejected`, and both worked-example
tests stay green against the amended graphs.

**`jev/automation/schema.json`** — under `$defs.action` (line 271), the
`tool` branch's `params.properties` (lines 280–283) gains
`"expect": { "$ref": "#/$defs/guard" }` after `timeout_s`. The copy in
`automation.md` §1 (from line 145, "`jev/automation/schema.json`, verbatim:")
is updated to match. No test compares the two; the executor updates both
and says so in the Build-Log entry.

**`extensions/jev/graphs/wiki-hop.json`** — two edits. `reading.entry[0]`
becomes
`{ "type": "tool", "params": { "name": "browser_snapshot", "input": { "within": "main" }, "into": "obs" } }`,
and `failed.output` gains `"reason": "{{event.reason}}"` beside `error`, so
the root `"on": { "ERROR": "failed" }` carries the reason into the report.
Nothing else changes. `EMPTY → browser_back` stays, because on a page
reached by a click it is the right recovery; the degenerate loop J1 saw —
`browser_back` landing on `about:blank` thirty times — now ends on the first
such landing, with `reason: failed` and an error naming `main`, `0` and
`about:blank`, because `about:blank` has no `main` and the snapshot is
refused.

**`extensions/jev/graphs/triage-linux.json`** — `processes.entry[0]` gains
`"expect": { "type": "matches", "params": { "path": "context.obs.text", "pattern": "^COMMAND\\s+PID\\s+USER" } }`.
`matches` runs `re.search` with `re.MULTILINE` (`guards.py` line 108), so
`^` anchors at any line start; on a zero exit `Exec::render()` prepends no
status line (`shell.rs` line 51), so the header is the first line either
way. JSON has no comments, so the pattern's provenance — procps's column
headers for `comm,pid,user`; reasoned, not observed on Linux, M5 — goes in
`automation.md`'s prose beside the triage worked example.

**`docs/design/automation.md`** — four edits, no others: the schema block
(from line 145) and both graphs (from lines 452 and 569; line 753 says they
are verbatim) updated to match the files; the "**Reserved events.**" bullet
(line 127) says `ERROR` carries `event.error`, `event.reason`
(`failed | truncated | unexpected | stale`) and `event.tool`; the "**A `tool`
action blocks.**" bullet (line 108) gains one sentence on `expect`; the
paragraph at line 832 beginning "The two head lines are a convention" is
replaced by the head block — `url`, `title`, then any `key: value` lines,
then a blank line — naming `chars` and `scope`, and pointing at this
document.

### Step 5 — `crates/rune` (S29)

**`eidolon/crates/rune/src/host.rs`** — beside `shell`, a new primitive
`shell_exec(command: String, timeout_s: Option<i64>)` returning
`#{ content: <Exec::render()>, ok: <Exec::ok()>, code: <Option<i32>>, timed_out, cancelled, truncated, background }`
as a Rune object, built by the same `eidolon_tools::shell::run` call with the
same cwd, timeout and cancellation. `shell` is untouched.

**`eidolon/crates/rune/builtin/bash.rn`** — the non-background path becomes:
```rune
let r = eidolon::shell_exec(input.command, input.get("timeout_s")).await?;
#{ content: r.content, is_error: !r.ok }
```
The manifest description's "Non-zero exit codes and stderr are reported
inline" gains "and a non-zero exit, a timeout or a kill is an error result".
Tests (in `host.rs`'s `mod tests`, same harness as the dispatch tests):
`a_nonzero_exit_is_an_error_output_whose_content_starts_with_the_exit_line`
(`bash` `exit 3` through a real `Dispatcher` with `AllowAll`; `is_error`,
content starts with `[exit code 3]`);
`a_zero_exit_is_not_an_error` (`printf ok`); `a_timeout_is_an_error`
(`sleep 5` with `timeout_s: 1`; `[timed out`); the existing
`sudo id`/`printf ok` laundering tests keep passing. The one that would fail
on revert is the first.

**`eidolon/crates/cli/system/RUNE.md`** — one bullet after `shell` in the
vocabulary list: `shell_exec(command, timeout_s)` — the same run, answered
as an object (`content`, `ok`, `code`, `timed_out`, `cancelled`,
`truncated`, `background`) for a tool that needs to *say* whether the
command failed rather than only show it. And one sentence after "`call`
returns a `String`, or `#{ content, is_error }`; a `Result::Err` returned
through `?` becomes an error output" (lines 56–58): a tool returns its
result whole or as an error; it never returns a shortened `Ok`, because the
host bounds what a model reads at the one place that knows who is reading.

**`eidolon/docs/extensions.md`** — under "## The service" (line 153), after
the envelope table: a result over 256 KiB characters is refused with the count, never
shortened; a service that returns text a graph will observe should lead it
with the head block (`url:`, `title:`, `chars:`), documented in
`docs/design/observation.md`, so the consumer can verify it arrived whole.

### Order, and what each step proves on its own

1 first: it is the layer that made S23 unreachable, and it is internal to one
crate. 2 next: without it a large unscoped snapshot is still silently cut,
now reachable and now *caught* by step 4 — but caught is not the goal; whole
is. 3 in parallel with either: it adds parameters and a head line, breaks no
caller, and produces the fixture step 4 needs. 4 after 3. 5 after 1, and it is
the step that makes `recover` reachable. After all five, J1's wiki-hop run is
expected to snapshot `main`, find 87 links, call the chooser, and either hop
or park — with the number of characters it scored stated in the Build-Log as
M1, not here.

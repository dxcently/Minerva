# Permissions

The permission-mode taxonomy for a session: **plan-only**, **approve-edits-only**,
**unattended** — what each does at the gate, where a mode lives, how it is set
and read over HTTP, how a parked ask carries its options to a chat surface and
an answer back, what plan-only means when nobody is reading, and what inherits.

It **is S79's design** — `docs/Tasklist.md` row S79, line 273 when read:
"Permission modes, and an approval a person can click instead of typing yes. The
ruling is `docs/design/permissions.md`". That row is the acceptance criteria this
document is written against, and the two must not drift: **the four rungs, with
`table` among them** (§1), **an ask that carries its own id and its options to a
surface** (§4 — S71's work, required here and designed there), and **§7's six
questions** as the operator's. Where they disagree, the row is the summary and
this is the source.

It amends no ruling. It **amends two statements in the tree**, both of which stop
being true the moment a chat holds a mode: `docs/design/shim.md:671` ("no
per-chat switch exists") and the `Summoned.yolo` doc comment that repeats it
(`crates/web/src/lib.rs:325-328`). It **extends S70** (the runtime yolo flip and
its semantics, `docs/Tasklist.md` row S70, line 264 when read) and it **hands
S71** (line 265) the requirement that row already owns: an ask must be visible
and answerable from a chat surface. It respects the warrant model of
`docs/design/unattended.md` — the operator's word given once per run, in
writing, and **no mode may create, widen or stand in for a warrant**. It
answers the operator's request for Claude-Code-style control in the Open WebUI
chat surface: pick a mode, and answer a permission prompt by clicking an option
instead of typing `yes`.

Every claim is labelled:

- **observed** — the cited line was opened and says this.
- **reasoned** — it follows from the cited lines; no single line says it.

Paths are relative to `eidolon/` — every `crates/…` line — except two trees:
`harnox/src`, a sibling at the repo root, and `docs/`, which is the bonsai2
**root's** own tree and not `eidolon/`'s (`eidolon/docs` holds one file,
`extensions.md`, and no `design/`). So `docs/design/*.md`, `docs/Tasklist.md` and
`docs/Decisions.md` all resolve at the root. `docs/Tasklist.md` is being edited
while this is written, so its row labels are the citation and its line numbers are
only as of the day this was read — and the same in reverse: row S79 cites *this*
document by line, so those numbers move when this is revised. No code here; the
specification at the end is what the executors land.

## The model in one page

One chokepoint, one lever. `Dispatcher::adjudicate` is the only place a policy
verdict exists (**observed**, `crates/core/src/dispatch.rs:380-434`, and its own
comment at `:436-443`), and dispatch's own three entry points all reach it —
`adjudicate`, `execute`, `dispatch`, which is the ladder *it* means by the word
(**observed**, "all three rungs" at `:439`, the three named at `:489-500`); it
evaluates exactly one hook (**observed**, `:387`, and
`crates/core/src/policy.rs:1-15`). A mode is therefore a **decorator on that
seam** — the same shape as `Yolo` (**observed**,
`crates/core/src/yolo.rs:105-140`) and the same shape as `Warrant`
(**observed**, `crates/cli/src/main.rs:1966-1977`,
`docs/design/unattended.md:422-431`). Nothing else in the harness is told a
mode exists.

The mode ladder, four rungs, strictest first, and what each rung answers:

| rung | on `Deny` | on `Ask` | on `Allow` | what the journal gets | who answers |
|---|---|---|---|---|---|
| **plan** | untouched | held, unless the call is a read | held, unless the call is a read | `Mode`, per held call; the plan is an ordinary assistant message | nobody — the hold is the enforcement; that the model then *states* a plan is unenforced (§5) |
| **edits** | untouched | untouched — the table's own question stands | a read: silent; a change: **asked** | the mode's line on the question; a person's answer (`Approved`/`Declined`) | the person, for every change (§6's judge guard, and only with it) |
| **table** (today's default) | untouched | asked | silent | only what answered (`:444-467`) | the person, for whatever the table flagged |
| **unattended** | untouched | allowed (`Yolo`) | silent | `Yolo`, per call | nobody — the switch |

Every cell is **observed** or **reasoned** below, mode by mode, in §1.

Two things about the ladder are load-bearing:

- **`table` is not one of the three modes the operator named, and it has to stay
  reachable.** The shipped table already allows `write` and `edit` outright
  (**observed**, `crates/rune/policy.rn:111-112`, and the Claude-CLI names at
  `:185-188`), so *auto-approving edits* is a no-op and would be identical to
  `table`; the three named modes are all **stricter** than today's default
  (**reasoned**, from those lines plus the escalate rules in §1). Hiding
  `table` from a picker would silently change behaviour the first time anyone
  touched it.
- **The rungs are mutually exclusive, and one function writes them.** Setting a
  mode sets the `yolo` switch in the same call, or `edits` means nothing while
  the switch is armed (**reasoned**: `Yolo` rewrites every `Ask`, so a stricter
  rung under an armed switch is dead code — `crates/core/src/yolo.rs:127-139`).

## 1. What each mode does at the chokepoint

`crates/core/src/dispatch.rs:380-434` in three lines: ask the one hook (`:387`), match its
`Verdict` (`:391-431`), journal what happened (`:432`, `:444-467`). The arms, in
the order they are written:

```
Allow + ruling.warrant.is_some()  -> run, journal Warranted     (:398)
Allow + ruling.yolo               -> run, journal Yolo          (:403)
Allow                             -> run, journal nothing       (:407)
Deny(reason)                      -> Err "denied by policy: …", Refused (:408-411)
Ask + ruling.judged.is_some()     -> run, journal Judged         (:417)
Ask                               -> publish AskUser (:419), user.approve (:422),
                                     Approved (:423) or Err + Declined (:424-428)
```

**Does a mode rewrite a verdict, add one, or refuse the call earlier?** All
three are decorators, so all three answer *before the gate matches* — they
cannot refuse earlier than that, because there is no earlier: the gate is the
one place a verdict exists (**observed**, `:436-443`). None of them
short-circuits the table either: a decorator sees the `Ruling`, not the call's
merits, so the reason on the record stays the table's own (**observed**, the
`..ruling` spread at `crates/core/src/yolo.rs:134-138` and the reason's own
provenance at `crates/core/src/policy.rs:102-105`; the same argument the
warrant makes for itself, `docs/design/unattended.md:94-97`).

**`Deny` is never softened — by any mode, in any direction.** `Yolo` acts only
on `Ask` (**observed**, `crates/core/src/yolo.rs:131`), and the file argues why
a switch that also switched off the three merits refusals would be doing
something the operator arming it was not thinking about (**observed**,
`crates/core/src/yolo.rs:23-31`). The warrant says the same
(**observed**, `docs/design/unattended.md:190-192`), and neither plan nor edits
below can reach a `Deny` because both stand on `read_only`, which harnox refuses
to set on a refusal (**observed**, `harnox/src/policy/mod.rs:99-102`, and the
constructor that enforces it at `:141`; see §6).

### unattended

**Observed in full**: `Switch` is an `Arc<AtomicBool>` shared between whoever
flips it and the hook that reads it (`crates/core/src/yolo.rs:69-97`); `pre_tool`
returns the inner ruling untouched unless the switch is armed *and* the verdict
is an `Ask` (`:127-133`); it then rewrites to `Allow` **carrying `yolo: true`**
(`:134-138`). `Dispatch` journals that as `PolicyOutcome::Yolo` (`:403`) whose
own doc says the record exists because "the record of what ran while the gate was
blind is the whole of what a yolo session leaves behind" (**observed**,
`:399-402`, and the outcome's doc at `crates/core/src/session/mod.rs:556-566`).

- `Deny` → untouched, refused on the merits (`:408-411`).
- `Ask` → `Allow`, journaled `Yolo`, **`user.approve` is never called**, so a
  yolo'd session never parks (**observed**: the arm at `:403` returns before the
  `Ask` arm at `:418`).
- `Allow` → untouched and silent (`:407`). `Allow` + warrant → untouched,
  `Warranted` (`:398`) — the warrant sits innermost precisely so this survives
  even in a yolo session (**observed**, `docs/design/unattended.md:422-427`).

This is not a fourth mechanism: the mode's `unattended` value **is** this
switch. That is the whole of the agreement with S70 (§2).

### approve-edits-only

New behaviour, one decorator, standing on one field that does not travel today
(§6). Its rule: **the mode is about change, and it only ever adds a question.**
A call the table calls a read passes; a call that changes something goes in front
of a person, whatever the table said; a question the table already raised stays
with the person it was raised for.

- `Deny` → untouched.
- `Ask` → **untouched, whatever the call is.** The existing arm runs —
  `Event::AskUser`, then `user.approve` (**observed**,
  `crates/core/src/dispatch.rs:418-430`) — and the mode may make that question
  *more specific* by appending its own line to it, the warrant's own precedent
  (**observed**, `docs/design/unattended.md:429-431`), and never less.
  An arm here that auto-allowed a *read-only* `Ask` was drafted and is
  **deleted**, because no `Ask` in the tree carries the read-only bit: every
  `Flag` site passes `false` outright (`harnox/src/policy/shell.rs:523`, `:532`,
  `:802`, `:807`) or only inherits it from its stages (`:307`, over
  `read_only &= v.read_only` at `:276`/`:304`), the shipped table's `FLAG`s are
  all `false` too (`crates/rune/policy.rn:265`'s unclassified catch-all among
  them; `grep 'FLAG[^)]*true' crates/rune/policy.rn` returns nothing), a refusal
  never carries it whether the table or the algebra made it
  (`harnox/src/policy/mod.rs:99-102`, forced at `:141`), and the `Ask` a
  structural refusal becomes is therefore not a read either
  (`crates/rune/src/policy.rs:457-459`). A branch no test can exercise is a
  branch that will rot. **Decision taken, overrule in one line:** the arm is
  gone; the day any table site sets `read_only` on a `Flag`, it comes back with
  the test that exercises it.
- `Allow` on a read → untouched, silent.
- `Allow` on a call that changes something → **`Ask`**, with the mode's line.
  This is the one rewrite no layer in the tree performs: `Yolo` and `Warrant`
  both act on `Ask` only (**observed**, `crates/core/src/yolo.rs:131`,
  `docs/design/unattended.md:426-428`), and the table's `Allow` is otherwise
  final. It can only *add* a question; it can never remove a refusal. It is also
  the question §6's judge guard exists for: without that guard, a
  `[policy] judge` box hands this one to a model.

### plan-only

Same decorator, different value. Its rule: **nothing that changes state runs
until a person leaves plan mode.**

- `Deny` → untouched.
- `Ask` on a read → left exactly as the table said. Plan mode's line is "nothing
  changes", not "nothing runs" — a read is how a plan gets made, and the table's
  question about a read's *shape* is still a fair question (**reasoned**, from
  the structural rule at `crates/rune/src/policy.rs:455-459`).
- `Ask` on a change → **held**: rewritten to a `Deny`-shaped verdict carrying
  the mode's own sentence, `ruling.mode` set, journaled `Mode`.
- `Allow` on a change → **held**, as above.

A hold must not fall into the plain `Deny` arm, or the model reads the arm's own
formatting — `denied by policy: write a file` (**observed**,
`crates/core/src/dispatch.rs:408-411`) — which says nothing about what to do
next. So the mode-shaped arms are matched **before** the plain ones, exactly as
the warrant's and yolo's arms are matched before theirs in intent
(**observed**, `:398`, `:403`, and the comment at `:392-397`).

### What each mode journals, and one prohibition

`journal_verdict` writes `RecordKind::PolicyVerdict` with
`outcome` and `note = ruling.judged.or(ruling.warrant)` (**observed**,
`crates/core/src/dispatch.rs:444-467`). This document adds one value to
`PolicyOutcome` — `Mode` — and one field to `Ruling` — `mode: Option<String>`,
the mode's own line, riding beside `judged` and `warrant` — so that:

- a mode's *hold* — the whole of what a posture answers — is never journaled as
  a person's `Approved` (**reasoned**: that writes a lie a ledger groups by, the
  argument the enum makes for `Judged` at
  `crates/core/src/session/mod.rs:548-555` and for `Yolo` at `:556-566`) nor as
  `Refused`, whose doc says "the table said no, and that is the answer"
  (**observed**, `:540-542`), because here the table said yes and a posture said
  wait;
- the mode's line reaches the note the same way a judge's or a warrant's does
  (**observed**, `:453`, `:465`).

`PolicyOutcome`'s own doc says the enum is append-only
(**observed**, `crates/core/src/session/mod.rs:536-537`), so the variant goes
last.

**One prohibition, stated as a prohibition.** The warrant layer confirms a
requested warrant from the session journal, and only from a root record whose
outcome is `Approved` or `Warranted` (**observed**,
`docs/design/unattended.md:442-448`). `Mode` must **never** be added to that
set. If it were, an `edits` auto-allow of a `jev_run` call would mint a warrant
out of a mode — the exact thing the warrant rules out for a judge's yes and for
a yolo yes (**observed**, `docs/design/unattended.md:408-412`).

**Rejected.** A mode that refuses a call before the table answers it: no
reason, no classifier record, and the audit trail loses the question it would
have asked. A mode that reaches a `Deny`: that is a fourth kind of thing
(**observed**, `docs/design/unattended.md:190-192`). A mode expressed as a list
of tool names in `config.toml`: the table is already the operator's own
classification file, and it already answers per call (**observed**,
`crates/rune/policy.rn:33`); a second list would be a second thing to keep true.

## 2. Where the mode lives and how long it lasts

**Per session.** A session is one policy stack, because `build()` mints one per
session (**observed**, `crates/cli/src/main.rs:1989`), and a chat is a session
(**observed**, `crates/web/src/shim/mod.rs:162-214`: one chat id, one `.eid`
log, one `Agent`). `eidolon web`'s single page is one session too, which is why
its own posture is a process-wide `bool` there (**observed**,
`crates/web/src/serve.rs:73`, `crates/web/src/lib.rs:192`).

**In memory, on handles the stack reads per call.** The mode handle is held by
the chat beside the `yolo` boolean it replaces
(**observed**, `crates/web/src/shim/chat.rs:30-53`, `:40`) and by whatever
answers the routes; the stack reads it on every call exactly as it reads the
switch today (**observed**, `crates/core/src/yolo.rs:131`).

**Not journaled.** Agrees with S70 verbatim, and for its reasons: a switch is a
posture the operator is holding, it is not journaled, and a reopened session
comes back gated — a switch that persisted would be one an operator could leave
armed in a log they opened again a week later (**observed**,
`crates/core/src/yolo.rs:50-58`; the same ruling for a warrant at
`docs/design/unattended.md:419-420`). The *effects* are journaled instead,
per call (**observed**, `crates/core/src/dispatch.rs:403`, `:398`, and `Mode`
as added in §1).

**Where this document deliberately differs from S70, in three places:**

1. **The plan is on the branch.** Plan-only's *plan* is journaled — as an
   ordinary `RecordKind::AssistantMessage` (**observed**,
   `crates/core/src/session/mod.rs:57-58`). The posture is a keystroke; the plan
   is the work, and it survives a resume while the mode does not. This is the
   only sense in which plan-only leaves a durable mark.
2. **S70's fix, as written, cannot work.** S70 says the runtime flip should
   reach "the `Switch` in `Ctx`"; `Ctx` holds `pub yolo: bool`, a snapshot
   (**observed**, `crates/web/src/serve.rs:73`). A handle has to be put *into*
   `Ctx` — see §3.
3. **The yolo value is the existing switch, not a parallel one.** S70 owns the
   flip and its semantics; the mode ladder reuses them rather than
   re-implementing them, so a `Mode` value of `unattended` inherits S70's
   journaling (`PolicyOutcome::Yolo`, per call) and its revocation
   (`:yolo off` — `crates/tui/src/app.rs:4571-4574`,
   `crates/cli/src/main.rs:3351-3354`).

**What ends a mode, and what does not.** The process ends it. A chat's idle
eviction ends it, because eviction drops the assembled chat and the next request
re-summons a fresh one over the same log (**observed**,
`crates/web/src/shim/chat.rs:84-92` for the condition,
`crates/web/src/shim/mod.rs:166-167` the cache hit, `:214` the fresh
`ChatSession`). Opening the log in a later process ends it
(**observed**, `crates/web/src/shim/mod.rs:176-181`). Nothing in the journal
ends it, and no clock or timeout does.

**Why, and what it costs.** Three reasons, in order: the audit is per call, not
per setting (**observed**, `crates/core/src/dispatch.rs:403` and its comment at
`:399-402`); a log reopened later must
come back gated (**observed**, `crates/core/src/yolo.rs:56-58`); and the
*absence* is visible rather than silent — every reply names the mode, the way
the shim already stamps its yolo notice into each reply's preamble
(**observed**, `crates/web/src/shim/chat.rs:28`, `:280-283`). A session that
came back disarmed therefore cannot be mistaken for one still armed
(**reasoned**). The honest loss: an operator's pick is lost when their chat goes
idle and is re-summoned, and they re-pick — which is §7's third question, not a
hidden behaviour.

**Rejected.** A mode in `config.toml` (a posture is not a setting, same reason
it is not journaled). A mode per process only (one `eidolon web` serves many
chats, and one operator may want one chat planned and another unattended). A
mode written into the chat log (S70; and the effects are already there). A
three-valued badge on the TUI's status line: it has been ruled out for exactly
this — armed, and only armed, because a three-valued key "would invite a status
line that announced the gate was working" (**observed**,
`crates/tui/src/state.rs:1908-1909`, the key itself at `:1906-1910`). A mode is
a word about a keystroke, shown where the operator is looking (the TUI's own
reasoning for its notice at `crates/tui/src/app.rs:4559-4564`).

## 3. Setting and reading a mode over HTTP, and reaching a running stack

**Today no route sets or reads any posture.** The route table is
**observed** at `crates/web/src/serve.rs:328-499` (`/api/events`, `/api/say`,
`/api/answer`, `/api/cancel`, the operator pages via `page_route` at `:237-263`,
`/v1/operator/*`, `/v1/models`, `/v1/chat/completions`, `/v1/agent/answer`,
`/v1/agent/tool`, `/v1/agent/activity`, then assets) and there is no mode among
them. What exists instead is two snapshots and a stack-local switch:

```
process launch (--yolo / config)
      |
      +-- main.rs:686  Switch::new(yolo)  ── clone ──> ServeHost.yolo   (serve_host.rs:82)
      |                                 └── same ──> ServeOptions.yolo (main.rs:700, lib.rs:71)
      |                                    lib.rs:192  opts.yolo.armed() -> Ctx.yolo: bool  (serve.rs:73)
      |                                    ...read once for the SSE preamble (serve.rs:333)
      |
      +-- ServeHost::summon   let armed = self.yolo.armed();   (serve_host.rs:566)
             |                build(..., armed, ...)            (:573 -> main.rs:1675-1684, `yolo: bool`)
             +-- build()      Switch::new(yolo)                 (main.rs:1989)  <- a FRESH switch
                                 Yolo::new(policy, switch)      (main.rs:1994)
             +-- Shim::summon -> ChatSession { yolo: bool }     (mod.rs:214, chat.rs:40)
```

Every arrow above is **observed**: `crates/cli/src/main.rs:686-701`,
`crates/cli/src/serve_host.rs:73-96`, `:555-585`,
`crates/web/src/lib.rs:60-72`, `:192`, `:320-330`,
`crates/web/src/shim/mod.rs:214`, `crates/web/src/shim/chat.rs:40`.

So the two failures are both in that picture, and neither is about routes:

1. **Nothing calls `.set()`, ever, on the serve process's switch.** The only
   writers of a yolo `Switch` in the tree are the TUI's `:yolo`
   (**observed**, `crates/tui/src/app.rs:4565-4580`) and the headless chat's
   (**observed**, `crates/cli/src/main.rs:3343-3362`). `ServeHost.yolo` is
   shared with `ServeOptions.yolo` and read per summon, and that is all
   (**observed**, `crates/cli/src/serve_host.rs:566`).
2. **Even a route that flipped it would not reach a running chat.** `summon`
   snapshots the switch into a `bool` (`serve_host.rs:566`) and `build()` mints
   a **fresh** `Switch` from that `bool` (`main.rs:1989`). The chat's policy
   stack therefore holds a different atomic from the process's, and every
   running chat keeps the value it was born with (**reasoned**, from those two
   lines).

**The fix is one parameter's type.** `build()`'s `yolo: bool`
(**observed**, `crates/cli/src/main.rs:1675-1684`, parameter at `:1682`) becomes
the mode handle — the handle carries the `yolo::Switch` so there is one object
per session, not two — and `ServeHost::summon` passes the process's clone
through instead of the `armed` snapshot it takes and hands over today
(**observed**: `crates/cli/src/serve_host.rs:566` takes the snapshot, `:573`
hands `build()` that `bool`).
That is three call sites, all of them in `crates/cli`
(**observed**: `main.rs:551`, `main.rs:609`, `serve_host.rs:573`), and after it
the stack reads the handle per call on its own, so a route needs no plumbing to
reach a *running* session (**reasoned**, from
`crates/core/src/yolo.rs:127-139`). Every existing reader keeps working
unchanged — `crates/cli/src/main.rs:1990` (the launch notice),
`crates/cli/src/serve_host.rs:566`, `crates/web/src/lib.rs:192`,
`crates/tui/src/state.rs:1902` — because they all read the handle they already
have (**observed**).

**Routes.** One per surface, both addressed the way that surface already
addresses a session:

- `POST /api/mode` on the one-session server, behind the same host and bearer
  rules as its neighbours (**observed**, `crates/web/src/serve.rs:303` for the
  POST host check, `:343` for the arm it sits beside, `:538-555` for the
  request/response shape of `/api/answer`).
- `POST /v1/agent/mode` on the shim, bearer-checked and addressed by
  `X-Eidolon-Chat-Id`, exactly the shape of `/v1/agent/tool` and
  `/v1/agent/activity` (**observed**, `crates/web/src/serve.rs:470-488`,
  `:776-782`).

Body: `{"mode": "plan" | "edits" | "table" | "yolo"}`.

**Reading it back** happens in two places, both of which already exist as
patterns: `GET /v1/agent/activity`, which today reports the chat's parked
question (**observed**, `crates/web/src/shim/activity.rs:42`) gains the mode, and
**every reply carries a mode line**, the way the yolo notice is pushed into the
preamble today (**observed**, `crates/web/src/shim/chat.rs:280-283`). The reply
is the authority for a person reading a chat; the activity route is for a UI
that polls; the `Ctx`/`ChatSession` value is what the *stack* reads.

**One writer.** The handle's `set` writes the mode and arms or disarms the
switch in the same function (**reasoned**): two fields written in two places is
the one way this design can produce a session that is stricter than its picker
says. The `SSE` preamble's yolo line is a snapshot taken when the connection
opened (**observed**, `crates/web/src/serve.rs:333`), so a mode flipped
mid-connection is not reflected in that connection's preamble — the per-reply
line is what a person should be reading (**reasoned**).

**Who may set one.** Only whoever holds the serve token (**observed**,
`crates/web/src/serve.rs:465`, `:472`). A chat cannot set its own mode: a mode
is a word spoken *at* a session, and a session that could widen its own
permission would be the gate negotiating with itself (**reasoned**, from the
chokepoint's whole arrangement at `docs/design/shim.md:18-26` and the one-hook
seam at `crates/core/src/policy.rs:183-192`).

**Rejected.** A mode in the SSE stream only (a mode must outlive a connection).
A per-process mode (see §2). A mode that a chat's own tool call can set,
including a `mode` tool: the model would hold the lever on its own questions,
which is the one thing every layer in this tree is arranged to prevent
(**reasoned**, `docs/design/unattended.md:11-16`'s interpreter/script/gate
separation).

## 4. A parked ask: how the options reach a surface and how an answer comes back

One gate, two mechanisms, and the difference between them is the whole of this
section.

### The page (`eidolon web`, structured, id-addressed)

**Observed in full.** `Ask { ask_id, kind, prompt, options, call }`
(`crates/web/src/user.rs:48-68`), pushed to every open stream as
`AskEvent::Opened`/`Settled` (`:70-77`, `:153`), answered by
`POST /api/answer {ask_id, answer}` (`crates/web/src/serve.rs:343`,
`:538-555`) through `WebUser::answer` (`crates/web/src/user.rs:118-133`). The
resolve is *by id*: a slot holding a different id, or none, returns `false`, and
the route answers **409 "that ask is no longer pending"** (**observed**,
`:118-126`, `:550-553`). One pending slot, one mutex; a newer ask bumps an older
one and fails it closed (**observed**, `:140-149`); a cancel takes the slot back
so a stale answer cannot land on a question nobody waits on (**observed**,
`:156-166`). `choose` fills `options` from the labels (**observed**, `:178-190`)
and `approve` sends exactly two options, `yes` and `no`, plus the call
(**observed**, `:195-208`).

### The shim (rendered text, no id, no options on the wire)

**Observed in full.** The park renders a **markdown block**
(`crates/web/src/shim/user.rs:216-228` for an approval, `:230-241` for a
choice) whose option labels live *only* in the pending slot's memory
(`Kind::Choose { labels }`, `:52-55`, `:247-248`). `Event::AskUser` renders no
frame at all (**observed**, `crates/web/src/shim/render.rs:226`); the block
reaches a client through the window's **third feed**, the asks channel
(**observed**, `crates/web/src/shim/window.rs:1-14`, `:103`, and
`crates/web/src/shim/user.rs:178`). An answer is either the next chat message
(`classify`: yes-words, no-words, a label or a 1-based index, then fail closed —
**observed**, `:129-163`, `:194-207`) or
`POST /v1/agent/answer {question, approved}` (**observed**,
`crates/web/src/serve.rs:760-773`), which requires the caller to echo the stored
block text back and **requires `Kind::Approve`** (**observed**,
`crates/web/src/shim/user.rs:114-119`). Therefore: **a parked `choices_user`
cannot be answered over HTTP at all today** — it can only be answered by typing
into the chat (**reasoned**, from `:116` and the two routes).

### Stale answers and the two-answer race

- **Page**: a stale id is refused, 409, nothing resolved (**observed**,
  `crates/web/src/user.rs:122-126`, `crates/web/src/serve.rs:547-554`).
- **Shim, HTTP**: the same refusal (**observed**,
  `crates/web/src/serve.rs:772`).
- **Shim, by message**: *not* refused. With nothing parked, the text is
  `Answered::NoQuestion` and becomes an ordinary user message that steers the
  model (**observed**, `crates/web/src/shim/user.rs:134`). Fail-closed in the
  sense that matters — it approves nothing (**observed**,
  `docs/design/shim.md:656-662`) — but a person who typed `yes` one second late
  gets their word read as an instruction. The page does not have this shape
  because an id is not a word.
- **Race**: one pending slot behind one mutex on both paths (**observed**,
  `crates/web/src/user.rs:120-125` and
  `crates/web/src/shim/user.rs:115-117`), so exactly one answer wins and the
  loser fails closed; a cancel clears the slot on both (**observed**,
  `crates/web/src/user.rs:156-166`, `crates/web/src/shim/user.rs:181-189`).

**The requirement this puts on the id.** The operator wants to click an option
rather than type `yes`. A click is safer than a word precisely because it can
name *which* question it answers — the page's 409 exists for that reason
(**reasoned**). So the shim's ask must grow an id and its option list on a wire,
and that is **S71's** work (row S71, "a per-chat notice queue … plus parked asks
from such a turn — read the same feed `/v1/asks`, `/v1/answer`"). This document
requires it and does not design it; the mode layer adds nothing to it and must
not fork it.

### When no window is attached, and when there is no person

**No window attached — a woken turn.** A doorbell ring starts
`continue_turn` on the chat's driver with nobody typing (**observed**,
`crates/web/src/driver.rs:205-225`, `:282-289`), and a window subscribes *before*
a turn is started or joined (**observed**, `crates/web/src/shim/window.rs:1-14`)
— and a broadcast never replays to a late subscriber (**observed**,
`crates/web/src/shim/user.rs:96-101`). So a woken turn's question is published
to a channel with no receivers: it exists in the journal, and in the pending
slot, and no client ever shows it (**reasoned**, from those three). The
consequence for this document is the one S71 was opened for: **a mode that
parks stalls in a woken chat.** There is no timeout that would rescue it, by
design (**observed**, `docs/design/shim.md:723-735`), and a parked chat is exempt
from eviction precisely because it is waiting (**observed**,
`crates/web/src/shim/chat.rs:81-92`).

**No person at all — and why the gate is not where the two are told apart.** `NoUser`
answers every question with `None` (**observed**, `crates/core/src/user.rs:90-99`)
and `approve` defaults through `confirm` to `choose`
(**observed**, `crates/core/src/user.rs:80-87`), so *every* `Ask` on a headless
run becomes `Err` + `PolicyOutcome::Declined` (**observed**,
`crates/core/src/dispatch.rs:424-428`) — the same outcome the enum documents as
"the operator said no — or there was nobody to ask, which `NoUser` answers the
same way **and on purpose**" (**observed**,
`crates/core/src/session/mod.rs:545-547`). Nothing anywhere tells them apart.

**The distinction this document draws, and where it is drawn.** Not at the gate.
The gate knows one thing — a question it raised was declined — and it is honest
about exactly that much; making it know *why* would have it reporting something
it cannot observe, which is the failing this project names as "a protection is
only as reachable as the layer it inspects"
(**observed**, `docs/design/unattended.md:11-13`). Instead:

- the gate keeps journaling `Declined` unchanged;
- the **consumer that owns the question** records the absence, on the branch,
  where every other out-of-band fact about a chat already goes: one
  `RecordKind::Note` (**observed**, the convention and the prefix pattern at
  `crates/web/src/shim/cwd.rs:8-9`, `:92`,
  `crates/web/src/shim/history.rs:8-9`, `:161`,
  `crates/web/src/shim/mod.rs:200`);
- it is written when the park **settles with nobody's word**, which is the two
  settles that resolve `None` — the turn's cancel arm
  (`crates/web/src/shim/user.rs:181-189`) and the second-ask bump (`:169-175`) —
  both inside `park`, where the park's own broadcast has already said how many
  receivers it had (`:178`; a window subscribes before the turn starts,
  `crates/web/src/shim/chat.rs:150`). A person who typed an unrecognised answer
  is a person present: the fail-closed decline is theirs
  (`crates/web/src/shim/user.rs:152-162`) and their words are steered onto the
  branch behind it (**observed**, `crates/web/src/shim/chat.rs:226-232`), so no
  note is owed for that path.

So the operator reading a chat later sees a declined call **and** a line saying
whether anyone was there to have said no. That is the whole of the distinction,
and it is enough for the one thing it is for: telling "nobody is there" apart
from "the operator said no" without teaching the gate to guess.

**Rejected.** Changing `UserIo::approve` from `bool` to an enum so the gate can
journal the absence: it ripples through every implementor (the page, the shim,
`NoUser`, the TUI's, and the tests — **observed**,
`crates/core/src/user.rs:36-46`, `:80-99`, `crates/web/src/user.rs:176-209`,
`crates/web/src/shim/user.rs:243-249`) for a fact the gate cannot observe. A
timeout on a parked ask (ruled out in `docs/design/shim.md:723-735`). A second
pending slot (one question at a time is the dispatcher's own behaviour —
**observed**, `crates/core/src/dispatch.rs:418-430`, and both slot guards say
so). A "nobody present" mode that answers `Ask` with `Allow`: that is not a
permission mode, it is an authorization nobody gave.

## 5. What plan-only means when nobody is reading the plan

"Stop and state the plan before executing" is **one rule plus one sentence**:

1. the rule, at the gate: while `plan` is the session's mode, no call the table
   does not call a read runs, and each held call comes back to the model with
   the mode's own sentence (§1);
2. the sentence, in the tool result the model reads: what plan mode is, that
   nothing has changed, and that the plan is what its next reply should be. The
   dispatcher already owns this wording for a declined call —
   `"the user declined this tool call"` (**observed**,
   `crates/core/src/dispatch.rs:424-428`) — and this is the same job.

The rule is enforced; the sentence is written. What is neither enforced nor
measured is that the model then *states* a plan (**reasoned**: the hold is the
mode, the plan is the model's answer to it, and nothing in the tree tests one);
§1's ladder says so where its "who answers" cell promises nobody.

**Where the plan is written.** On the branch, as an ordinary
`RecordKind::AssistantMessage` (**observed**,
`crates/core/src/session/mod.rs:57-58`). Not a frame, not a new record kind, not
a message over some channel: that record is already what every surface renders —
the shim's window turns each `TextDelta` into a chunk and each finished call
into a tool block (**observed**, `crates/web/src/shim/render.rs:208`,
`:218-224`), and `eidolon log --json` writes it as one JSON line
(**observed**, `crates/core/src/session/mod.rs:30-36`) — and it survives a
resume for free while the posture deliberately does not (§2).
A dedicated `Plan` record kind would be a scarce coordinated thing spent on text
the branch already carries, against `RecordKind`'s own growth rule
(**observed**, `crates/core/src/session/mod.rs:104-106`).

**What ends it.** The operator changing the mode — the picker, or
`:mode edits` in a terminal. Nothing in the journal ends a plan; there is no
per-call dialog, no timeout, and no auto-release. A hold is released by a word
spoken at the session, which is what a mode is (§2). In practice, with a person
present, the plan arrives as a reply, the person reads it, and they pick the
next mode: the release *is* the picker.

**Does the model need to be told, or only held?** Told — at minimum at the
moment of the hold, as above. Holding without saying is measured to be
expensive: 69 bash calls in one corpus were refused after a question and each
one wasted a re-planning call (**observed**, `docs/Tasklist.md` row S74, line
268). Whether the mode's sentence *also* goes into the system prompt — one new
argument to `system_prompt`, which already takes a per-session `note` and
composes it once per turn (**observed**,
`crates/core/src/agent.rs:1286-1319`, `:1529-1559`), at the price of one prefix
miss per mode change (**observed**, `crates/core/src/session/mod.rs:286-292`) —
is the operator's call, not this document's (§7). The hold costs nothing extra
either way; the sentence is one string.

**Posture, graph node, or warrant shape?** A **posture** — a decorator on the
policy seam, armed and disarmed live, per session, unjournaled. Three reasons,
and the middle one is the one this task asked about:

- *Enforcement belongs at the gate.* Permission is decided at one chokepoint
  and no second place may decide it (**observed**,
  `crates/core/src/dispatch.rs:436-443`,
  `docs/design/shim.md:18-26`).
- *Control flow belongs to the graph — and plan mode is not control flow.* The
  ruling is that loops, conditionals and substitutions are control flow, that
  control flow is the graph's job, and that moving it there costs nothing
  because "a `lines` menu **is** the `for`" (**observed**,
  `docs/Decisions.md:5468-5481`, `docs/design/triage.md:13`, `:249`).
  `automation.md` states the same separation as *the service decides; the script
  does; the gate rules* (**observed**, `docs/design/automation.md:53-55`) and
  owns the graph as the thing that shapes the menu
  (**observed**, `:42-51`). That ruling is about **what happens next inside a
  run**. Plan mode does not choose a next step; it refuses to let one be taken
  without a person's word, which is the gate's own question, the gate's own
  seam, and available to the ordinary agent loop — which is not a graph at all
  (**reasoned**). A graph node could not reach the chat surface the operator
  asked for, because there is no graph there.
- *Not a warrant shape.* A warrant names a graph and its hash, the exact tools,
  origins, literal commands, a count and a duration, and it is the operator's
  word given once for one run (**observed**,
  `docs/design/unattended.md:53-62`, `:224-234`). Plan-only authorizes nothing,
  covers no tool, expires never, and cannot be requested by a script — so it is
  not an eighth envelope key, it is an absence of authorization. And a mode may
  never manufacture a warrant (§1's prohibition, §6's interactions).

**What plan mode does not do.** It does not stop a turn, cancel anything, edit
the model's tool list, touch a `Deny`, or hold a read. On a box where the table
calls nothing a read (`[policy] default = "flag"`, **observed**,
`docs/design/unattended.md:196-201`), it holds every call — which is the
correct reading of that box, and the reason the mode's sentence in the tool
result matters more there than anywhere else (**reasoned**).

**Rejected.** A per-call "shall I proceed?" dialog in plan mode: that is
`edits`, and two modes with one behaviour is one mode. A single `choices_user`
question ("may I proceed?") instead of a hold: it has no options to choose from,
and a person's yes is then indistinguishable from `edits`' per-change yes. A
`Plan` record kind (above). A `plan` field on the graph for jev runs: a run's
plan is its graph, already (same ruling as above).

## 6. What inherits: subagents, warrants, the judge, and the read-only bit

### A spawned subagent

**It inherits, by construction, not by a new parameter.** A subagent is summoned
through the same path a chat is — `Shim::summon` (**observed**,
`crates/web/src/shim/mod.rs:162-214`) into the factory's `summon`
(**observed**, `crates/cli/src/serve_host.rs:565-585`) — and S72 already rules
that posture is "inherited at summon …, never passed by hand"
(**observed**, `docs/Tasklist.md` row S72, line 266, citing
`crates/cli/src/serve_host.rs:565-579`). This document adds no inheritance
parameter: the handle travels because the summon path travels (§3's one
parameter type), and the registry row already reports a `posture` field
(**observed**, same row).

**One exception, with one reason: plan does not inherit — a child of a
plan-mode parent is born one rung down, in `edits`.** `send` is not a read
(**observed**, `crates/rune/policy.rn:287-296`, `v(ALLOW, …, false)`), so a
child in plan mode could not `send` its report at all; a subagent's whole
contract is a report to its parent (**observed**, `docs/Tasklist.md` row S78,
line 272), so a child that cannot `send` is a subagent that cannot finish. `edits` is the next rung of the same ladder, so the
child is exactly as careful as the parent's *next* thought, and the operator's
own word still governs by changing it (§7 asks whether they would rather have
`table`). Note the other end of this: a spawn the *model* attempts inside plan
mode is itself a call that changes something, so plan mode holds it (**reasoned**
from §1's rule and `docs/Tasklist.md` row S72's open question about whether a
spawn is gated — this document's answer is that the gate sees it, and plan mode
holds it).

**It can be changed mid-run, and the change is live within one call.** The mode
route addresses a chat id and knows nothing about parentage (§3), so a child's
mode can be widened or narrowed from the webui while it runs (**reasoned**), and
the stack reads the handle per call (**observed**,
`crates/core/src/yolo.rs:127-139`). Changing a child's mode cannot widen it into
authority: still no warrant, still no `Deny` softened (§1).

**The consequence to state out loud.** A child under `edits` parks on its first
change, and its asks go to *its* chat surface, not the parent's
(**observed**, `crates/web/src/shim/mod.rs:210`, one `ChatUser` per chat). If
the operator is not looking at that chat — and a woken child has no window at
all (§4) — the child stalls, fail-closed and without a timeout. That is S71's
requirement again, and it is the reason S71 is this document's first dependency
rather than a nicety (**reasoned**).

### Warrants

**The mode hook sits outside the warrant layer and leaves a warranted ruling
untouched.** The warrant is installed innermost, directly over the table, under
yolo, under the judge (**observed**, `crates/cli/src/main.rs:1956-1977`,
`docs/design/unattended.md:422-431`), so the mode hook is installed between the
warrant block and the yolo block (`main.rs:1977` … `:1988`) giving:

```
table -> Warrant -> Mode -> Yolo -> Judge
```

Read that right to left as the call travelling inward and left to right as the
ruling coming back out: the outermost name is the last to see whatever a mode
wrote. The subsection after this one is about that last hop, because it is the
one that decides whether `edits`' question reaches a person at all.

Two consequences, both wanted:

- the record of what a warrant authorized is not stolen by a mode: a warranted
  call is `Allow` + `warrant`, which the mode's arms skip, exactly as `Yolo`
  skips it (**observed**, `crates/core/src/dispatch.rs:392-398`,
  `crates/core/src/yolo.rs:131`);
- a mode's hold can never be read as warranting anything: `Mode` is not an
  outcome the warrant layer confirms from (§1's prohibition, **observed**,
  `docs/design/unattended.md:442-448`).

And the two interactions worth naming:

- **`plan` holds a `jev_run`.** `jev_run` is honestly `Mutating` and therefore
  not a read (**observed**, `docs/design/unattended.md:33`), so plan mode holds
  it — the warrant is never even requested, which is consistent: a session in
  plan mode has no run to authorize yet.
- **`edits` puts a `jev_run` in front of a person with its envelope in the
  question** (**observed**, `docs/design/unattended.md:362-368`), and that
  person's yes is `Approved` — which *does* confirm the warrant, correctly,
  because a person just gave the word (**reasoned**; and this is exactly the
  path the warrant's provisional-entry rule expects,
  `docs/design/unattended.md:442-448`).

**A mode is not a bound, and never a substitute for a warrant.** It has no
count, no clock, no origin list, no hash, and it cannot be requested by a
script. A session left in `yolo` is exactly as unbounded as it is today
(**reasoned**, from `docs/design/unattended.md:224-234`'s comparison table).

### The judge, and why the guard goes on it

**The chain hands a mode's question outward, and the judge answers `Ask` for a
living.** `Judge::pre_tool` escalates *any* `Ask` — it tests for `judged` and for
nothing else (**observed**, `crates/core/src/escalate.rs:224`, `:229-231`) — and
it is installed last, after the yolo block (**observed**,
`crates/cli/src/main.rs:2034-2056`, `Judge::new` at `:2038`, over the block at
`:1983-2008`). Each layer calls the next one inward
(**observed**, `crates/core/src/escalate.rs:220`, `crates/core/src/yolo.rs:128`)
and a ruling comes back through them in reverse. So an `edits` `Allow → Ask` on a
change is handed to the judge, which may answer it — `judged` set, `:242-248` —
and dispatch then runs the call at `:417` with the operator never asked. On a box
with `[policy] judge` set, **§1's promise that the person answers every change is
false**, and the broken promise is the one on the ladder and at §1's own arm.

**The guard is one test on the judge's `Ask` arm: a ruling that carries a `mode`
is returned untouched.** Its charter is the ground, and it is the judge's own
words: "Exactly one thing: **answer a question the deterministic pass had already
decided to raise**" (**observed**, `crates/core/src/escalate.rs:13-17`). A
posture's question is not that one — the table said *yes* and a mode said *wait*,
there is no question the deterministic pass raised, and a model may not answer
for the person the mode stopped for. Guarding `Mode` instead cannot do this job:
`judged` is set *after* `Mode` returns, so a "skip any ruling that carries
`judged`" test in the mode can never fire.

**The alternative was installing `Mode` outside the judge** —
`table -> Warrant -> Yolo -> Judge -> Mode` — and it is worse, because the mode
would then sit above the switch. `Yolo` rewrites an `Ask` to `Allow` carrying
`yolo: true` (**observed**, `crates/core/src/yolo.rs:131-138`), and the mode's
`Allow → Ask` would be applied to that rewrite: an armed switch would produce a
question instead of a yes, and dispatch's `Allow` + `yolo` arm — the record the
enum calls the whole of what a yolo session leaves behind (**observed**,
`crates/core/src/dispatch.rs:399-403`,
`crates/core/src/session/mod.rs:556-568`) — would never run, while every reply
still said the switch was armed (**observed**,
`crates/web/src/shim/chat.rs:281-283`). The switch is the operator's own word for
the session and a posture under it may not overrule it; that is why `Mode` stays
under `Yolo`, and why the guard had to move to the judge.

### `read_only`: the bit both modes stand on, and where it dies today

Both `edits` and `plan` are decisions about *change*, so both need the one
verdict harnox already makes and the gate has never seen:

```
harnox algebra + table      ->  Verdict { decision, reason, read_only }
                                (harnox/src/policy/mod.rs:87-117)
rune's parsed tier          ->  Tier::tier(decision, reason, read_only)
                                (crates/rune/src/policy.rs:393-401)
Ruling { verdict, reason, structural, judged, yolo, warrant }
                                (crates/rune/src/policy.rs:465-472)   <-- read_only dropped here
```

Every line above is **observed**: the table emits it as `v`'s third argument
(`crates/rune/policy.rn:45-46`), with `write`/`edit` at `false` (`:111-112`,
`:185-188`), Melete's read-only tier at `true` (`:278`), and the
trusted-extension sentinel at `:262-263`; `harnox` composes it per stage and
guarantees a refusal is never read-only (`harnox/src/policy/mod.rs:99-102`, and
the constructor that enforces it at `:141`).

So the single propagation this document needs from `crates/rune` is one field:
`Ruling.read_only` in `crates/core/src/policy.rs:97-140`, set from
`tier.read_only` where the `Ruling` is built
(`crates/rune/src/policy.rs:465-472`). Three things then become true at once
(**reasoned** from the above):

1. the gate can tell a read from a change without re-classifying anything, and
   without a second list of tool names to keep true;
2. **no mode can soften a refusal with it**, because `read_only` can only be
   true of a call that runs — harnox refuses to set it on a `Deny`
   (**observed**, `harnox/src/policy/mod.rs:99-102`, `:141`);
3. two other customers that already want it get it: the `reads` warrant clause
   (**observed**, `docs/design/triage.md:13`, "a verdict harnox already composes
   per stage and already tests"), and any dialog that wants to say *this changes
   nothing* — the page's own `Approval::ReadOnly` vocabulary
   (**observed**, `crates/core/src/policy.rs:39-47`), today read only by the
   extension page's column (**observed**, `crates/cli/src/ext.rs:414-424`) and
   the trusted-extension rewrite (**observed**,
   `crates/rune/src/policy.rs:340-369`).

The only other readers of it today are `eidolon policy`'s explanation line,
which prints it (**observed**, `crates/cli/src/main.rs:1241`) and whose
"what the tier means here" sentence is already mode-aware
(**observed**, `:1245-1254`) — worth keeping in step, since it is the one place
an operator can see the mode's own boundary without running anything.

## 7. What this does not decide — the operator's rulings

1. **`edits`' boundary.** This document reads "approve-edits-only" as *the
   person approves exactly the calls that change something* — reads silent,
   changes asked — because the other reading, auto-approving edits, is a no-op
   on the shipped table (**observed**, `crates/rune/policy.rn:111-112`,
   `:185-188`) and would be identical to the `table` rung. Confirm it, and
   confirm the boundary's reach: this document's "a change" is *every call the
   table does not call a read*, which includes `send` (**observed**,
   `crates/rune/policy.rn:287-296`) and `jev_run`; narrowing it to filesystem
   changes would need a second classification the table does not make.
2. **The names on the picker, and whether `table` is one of them.** Three named
   modes are all stricter than today's default; hiding the default would change
   behaviour the first time anyone touched the picker. This document's wire
   values are `plan | edits | table | yolo`; the labels are the operator's.
3. **How long a pick lasts.** This document rules that a mode dies with the
   chat's eviction and with the process, and that every reply says so (§2). The
   alternative — sticky per chat id for the life of the serve process — is a
   one-line change and a different promise about what an idle chat keeps.
4. **Whether plan mode's sentence goes into the system prompt** (graceful: the
   model plans without bumping into the hold; one prefix miss per mode change,
   `crates/core/src/agent.rs:1286-1319`,
   `crates/core/src/session/mod.rs:286-292`) **or only into the held call's tool
   result** (one wasted model call per plan-mode turn, row S74's measured 69).
5. **What a spawned child of a plan-mode parent gets.** This document rules the
   next rung down (`edits`), so the child can `send` its report; `table` is the
   other defensible answer, and "do not spawn under plan" is a third.
6. **Whether the terminals get the same ladder.** The TUI and the headless chat
   loop have `:yolo` today (**observed**, `crates/tui/src/app.rs:4565-4580`,
   `crates/cli/src/main.rs:3343-3362`). Making it `:mode plan|edits|table|yolo`
   is a change to two operator-facing command sets; keeping `:yolo` alone leaves
   two ways to say unattended and one way to say the rest.

## What this does not solve

- **A mode is not a bound.** No count, no clock, no origin list, no tool list,
  no hash (§6). The thing for an unattended run is a warrant; a session in
  `yolo` is exactly as unbounded as it is today.
- **It does not make a woken chat's asks visible.** S71 does (§4), and until it
  does, a mode that parks stalls in a chat nobody is watching.
- **It does not make a question cheap.** An asked call still costs the turn's
  re-planning (row S74's 69 calls, each a wasted model call).
- **Nothing here judges a call.** No mode reads an argument: `yolo` is still "a
  switch being on" (**observed**, `crates/core/src/session/mod.rs:556-566`) and
  `edits` is still the table's verdict about shape, not a reviewer. A mode moves
  *who is asked*, never *what is understood*.
- **It does not decide who the operator is.** The shim's routes are behind one
  serve token (**observed**, `crates/web/src/serve.rs:465`, `:472`), so anyone
  holding it can change any chat's mode. That is today's token model, not a new
  hole — but a mode is now a thing worth arguing about.
- **It does not touch `Deny`, ever.** Three refusals on the merits refuse in
  every mode. The honest way to have no gate is `[policy] enabled = false`,
  which says so where it can be read (**observed**,
  `crates/core/src/yolo.rs:29-31`).

## What lands, in the order an executor lands it

1. **`Ruling.read_only`** — `crates/core/src/policy.rs:97-140` gains the field
   (documented as the table's own claim about change, never set on a refusal by
   harnox), set where the ruling is built
   (`crates/rune/src/policy.rs:465-472`), and every other construction site the
   compiler names (`crates/rune/src/policy.rs:436-449`, the `..ruling` spreads
   at `crates/core/src/yolo.rs:134-138`). **Proves**: `eidolon policy`'s
   `reads:` line (**observed**, `crates/cli/src/main.rs:1241`) and the value
   reaching a hook are the same value.
2. **`crates/core/src/mode.rs`** (new) — `Mode { Plan, Edits, Table,
   Unattended }`, a `Clone` handle holding one atomic **and** the
   `eidolon_core::yolo::Switch`, `Handle::set` as the only writer of both, and
   `ModeHook: PolicyHook` implementing §1's arms in the order written there (a
   change held under `plan`, a change asked under `edits`, a read and every
   `Ask` untouched under both), skipping any ruling that already carries a
   `warrant` — a `judged` ruling cannot reach it, which is §6's whole point.
3. **The gate's one arm, the judge's test, and one outcome** —
   `crates/core/src/dispatch.rs:391-431` gains the mode-held arm *before* the
   plain ones (`:408`), the `Err` carrying the mode's own sentence rather than
   the `Deny` arm's formatting; `edits`' question needs no arm of its own,
   because it arrives as an ordinary `Ask` and runs the one already at
   `:418-430`; the judge's `Ask` arm gains the mode test beside its `judged` test
   (`crates/core/src/escalate.rs:224-231`, §6 — without it a `[policy] judge` box
   asks a model instead of the person); `PolicyOutcome` gains `Mode`, last, per
   its own append-only rule (`crates/core/src/session/mod.rs:534-537`); the note
   wiring at `:453`/`:465` includes `ruling.mode` beside `judged` and `warrant`.
4. **The stack and the handles** — `crates/cli/src/main.rs`: install the hook
   between the warrant block and the yolo block (`:1977` … `:1988`); `build()`'s
   `yolo: bool` (`:1682`) becomes the handle, and every caller of it hands one
   over (`main.rs:551`, `:609`, `:736`, `:792`, `:822`, `:1103`, and
   `serve_host.rs:573`); `crates/cli/src/serve_host.rs:566`/`:573` then pass the
   process's handle through instead of the `armed` snapshot. The postures that
   handle carries or replaces, so that no reader is left holding a second `bool`:
   `ServeOptions.yolo` is already a `Switch` (`crates/web/src/lib.rs:71`) and
   `ServeHost.yolo` too (`serve_host.rs:82`, read once per summon at `:566`);
   `Summoned.yolo` is a plain `bool` (`crates/web/src/lib.rs:329`) and becomes
   the handle; `Ctx.yolo` is the boot-time snapshot (`crates/web/src/serve.rs:73`,
   taken at `crates/web/src/lib.rs:192`) and becomes it, or sits beside it, so
   the one-session page's new route and its preamble read one object; the SSE
   preamble read (`crates/web/src/serve.rs:333`) becomes the handle's `.armed()`,
   taken once per connection exactly as `ctx.yolo` is today; `ChatSession.yolo`
   is a `bool` (`crates/web/src/shim/chat.rs:40`, set from the constructor's
   `bool` at `:62`/`:69`) and becomes the handle — which is also what lets a
   reply name the mode where it names yolo now (`:281-283`).
5. **The routes and the reads** — `crates/web/src/serve.rs`: `POST /api/mode`
   beside `/api/answer` (`:343`) and `POST /v1/agent/mode` beside
   `/v1/agent/tool` (`:470-488`), both setting the handle the chat's stack
   already reads; `crates/web/src/shim/activity.rs:42` reports the mode;
   `crates/web/src/shim/chat.rs:280-283`'s notice pattern carries it in every
   reply.
6. **The mode's sentence, and the park-without-a-person note** —
   `crates/web/src/shim/user.rs` writes one `RecordKind::Note` where a park
   settles with nobody's word (§4): the two settles that resolve `None`, both
   inside `park` — the turn's cancel arm (`:181-189`) and the second-ask bump
   (`:169-175`). The fact the note records is the park's *own* broadcast —
   `asks.send`'s receiver count, discarded today (`:178`), against a window that
   subscribes before the turn starts (`crates/web/src/shim/chat.rs:150`) — and the
   writer needs the one thing it does not hold today: the agent whose session is
   the journal, read there the way `crates/web/src/shim/chat.rs:286` reads it and
   handed to the `ChatUser` between the summon and the first turn
   (`crates/web/src/shim/mod.rs:210` builds the user, `:212` mints the agent,
   `:214` the chat). It follows the prefix convention of
   `crates/web/src/shim/cwd.rs:92`. **Decision taken, overrule in one line:** the
   writer is `ChatUser` and the fact is the send count, not `ChatSession.window` —
   that slot is private, sits on a struct `ChatUser` does not hold, is created
   after it (`crates/web/src/shim/chat.rs:44`, `:70`) and is superseded per
   connection (`:94-101`), so it answers "is a window attached" at a different
   moment than the one the note is about.

Step 1 is first because both modes are dead without it and it proves itself on
its own. Steps 2 and 3 are one change in behaviour each and are the ones a
reviewer can test at the gate alone: a `Fixed`-style hook
(**observed**, `crates/core/src/yolo.rs:142-154`'s test pattern) that returns
`Ask`, `Allow` and `Deny` under each mode, with the expected `PolicyOutcome` on
the record — and, for step 3's judge test, a `Fixed` ruling that carries a `mode`
and comes back from `Judge` unjudged. Step 4 is what makes a mode reachable while
a session is running and is the half S70 asks for. Steps 5 and 6 are reach and
visibility: the routes make a mode pickable, the note makes a question that
nobody was there for readable afterwards — and the ask's own id and its
visibility are S71's, which step 6 depends on rather than replaces.

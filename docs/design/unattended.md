# Unattended

What authorizes an unattended graph run, what it cannot authorize, what is
recorded, and how an operator revokes or bounds it.

This is the ruling task **A6** was asked for. It amends `automation.md` in
four places — the graph's `meta.jev` block (§1), the `refs` source (§2), the
escalation payload and report (§4), and the sandbox posture (§6) — and it
gives the trust ruling of 2026-09-18 (*An extension's tools are gated until the
operator vouches for the extension*) the job it was landed for and turned out
not to have. It extends three rulings in `Decisions.md` rather than
contradicting any: *the interpreter requests; the script does; the gate
rules*; *a protection is only as reachable as the layer it inspects*; and
*a rule whose exception requires knowing something unknowable is followed
unconditionally* — which is why nothing here touches `classify_tool`'s three
parameters. It writes no code. The specification at the end is what three
executors land, in the order it gives.

The measurement it starts from, taken by D8 against the real compiled
`policy.rn` over the graph's exact reconstructed call sequence — `jev_run` +
`browser_open` + 30 × (`browser_snapshot` + `browser_click`), 62 calls:

| configuration | questions |
|---|---|
| before D1 and D8 | 62 |
| after D1's browser policy | 31 |
| after D1, `browser` trusted | 31 |
| after D1, `browser` **and** `jev` trusted | 31 |

Trust has zero marginal effect on this graph, and every part of that is
correct: the four browser reads are `ALLOW` on `fetch`'s reasoning, the two
page-acting tools are `FLAG` by construction and immune to trust, and
`jev_run` is honestly `Mutating`. The residual 31 is 1 `jev_run` + 30
`browser_click`, and the 30 are flagged for a good reason. This document does
not argue with the table. It rules on what the table cannot see.

## The model in one page

Three different things get authorized in a graph run, and today one mechanism
— a yes/no question per tool call — is asked to authorize all three:

1. **The run.** That this graph, with this goal, may drive this machine's
   browser or shell for a while. Only the operator can say so.
2. **A step.** That a `browser_click` the graph's machinery chose may happen.
   The gate sees `{ref: "e51"}` and nothing else — not the graph, not the
   goal, not the menu, not the confidence — so it asks, every time, at the
   only layer that cannot see why.
3. **A choice.** Which link. The chooser decides when it is sure and parks
   when it is not, and the park already reaches a model or a person with the
   whole menu in front of them. This is the cheaper, better-targeted question,
   and it already exists.

The ruling is that **the operator's word is given once, for the run, in
writing, as an envelope — and each step is authorized by that word to the
extent the step stays inside the envelope.** The envelope is called a
**warrant**. It names the graph and its content hash, the exact tools the run
may dispatch, the origins the browser is confined to, the literal commands a
shell may be given, and a count and a duration. It is authored in the graph,
restated by whoever calls `jev_run` so the gate can see it, verified against
the graph by the service that owns the graph's meaning, shown to the operator
in the one question `jev_run` still asks, and enforced per step by three layers
— each the layer that can actually see the bound it enforces.

A warrant answers only questions the table raised. It never touches a `Deny`,
it never covers a tool it does not name, and it never covers a call from
anywhere but the chain under the one call it was granted to. A click outside
the envelope asks exactly as it does today, and unattended that is a refusal.
The chooser's confidence has no part in any of it: confidence is measured over
the options and can only ever be measured over the options, so it is evidence
about the pick and none at all about the page.

What answers each of the 62 calls:

| call | before | today | under a warrant |
|---|---|---|---|
| `jev_run` | asks | asks | **asks once**, with the envelope in the question |
| `browser_open` | asks | allowed (table) | allowed (table); its `confine` is observed |
| `browser_snapshot` ×30 | asks | allowed (table) | allowed (table) |
| `browser_click` ×30 | asks | asks | **warranted**: allowed by the operator's one answer, each one journaled as such |
| **questions** | **62** | **31** | **1** |

Headless, a standing warrant in the operator's own config answers the one
question too, and the count is 0. Why 1 and not 0 interactively is §4; why
neither number is `--yolo` in disguise is §1.

## 1. What authorizes a click

**Choice.** The operator, once, through the warrant they approve when
`jev_run` asks. A click under a live warrant is allowed by the gate with
outcome `Warranted` and the warrant's id on the record, provided the call's
tool is one the warrant names, the run is still inside the warrant's count and
duration, the browser has been confined to the warrant's origins by a
`browser_open` the gate itself saw, and — for a shell — the command is one of
the literal strings the warrant lists. Everything else about the click stays
exactly as it is: the table still says `FLAG`, the reason on the record is
still the table's own, and a ledger grouping by reason still sees thirty
flagged clicks.

**Rejected.** The chooser's confidence. A pick at 0.99 tells you the model is
sure which of the options it prefers. It cannot tell you the options are
honest, because it never saw anything but the options — the `judge` phase
scored a `ps` usage error at 0.995 today, and a page that writes its link text
to steer a byte-level chooser is the adversary this document is about. A
ruling that let confidence authorize a click would be a ruling that a page can
authorize its own clicks by being persuasive.

Also rejected: extension trust as the authorization. Trust promotes a
self-declaration from ignored to believed; it says nothing about a call's
arguments, and `browser_click`'s danger is entirely in what its argument
resolves to. D8 proved trust cannot reach a named `FLAG`, and that is the
mechanism working. Also rejected: `--yolo` for the run's duration, however
scoped. Yolo answers every question for every tool with nothing looking at
the call; a graph's driver dispatches whatever the service names, and a
service that asked for `bash rm -rf` under a session-wide yes would get it.
The difference is stated in a table at the end of this section. Also
rejected: a judge per click. The judge answers one question by reading the
transcript; thirty clicks are thirty model calls, each reading a page the
adversary wrote, each able to say yes — the judge's safety argument ("a wrong
judge costs a question that was going to be asked anyway") is exactly the
argument that it must not be given a run's worth of yeses at once, and §4
rules that a judge's yes never creates a warrant for the same reason.

Also rejected: the driver vouching for itself. `run.rn` cannot carry a
credential that says "I am under warrant X", because a script can lie about
what it needs; what it cannot lie about is its origin and its position in the
dispatch chain, which the host sets. So the warrant is keyed by the call it was
granted to, and a nested call is covered only because the host says which
outermost call it is running under.

### The one question and the thirty

`jev_run` asks. It should: it is the moment a person authorizes a run, and it
is the only moment at which the whole envelope can be put in front of them —
"this graph, these tools, these origins, this many actions, this long." The
thirty clicks are not thirty decisions; they are the one decision applied
thirty times, and asking it thirty times at a layer that cannot see the
envelope is not thirty times safer. It is the same question with the
information removed. The asymmetry between authorizing a run and authorizing
each of its steps is the whole answer: the run is authorized by a person who
can see the envelope; a step is authorized by the envelope, checked by the
layers that can see each of its bounds.

### The adversary, and exactly what it can reach

A page crafts link text to steer the chooser. Under a warrant, here is
everything that page can cause, and what bounds each:

- **A click on an option the graph admitted.** Every option comes from a
  snapshot the table allowed, filtered by the graph's `roles`, `within` and
  — new in §3 — `url`. The adversary chooses *among* those. Cost: a wasted
  hop, a wrong path, a run that exhausts its budget, and one decision row
  with a steered label. Bounded by the graph's budgets and by the menu bound;
  the row is marked with the warrant id so an exporter can tell it apart.
- **A click that does not go where the snapshot said.** The `/url:` line in
  an accessibility tree is the page's own `href`, and a page's script can
  navigate anywhere on click regardless. Bounded by **confinement** (§3): a
  confined browser aborts every request to an origin the warrant did not name
  — navigations and subresources alike, so the loopback `fetch()` D1r found
  is aborted at the network layer while a warrant is live — and the click
  returns an error naming the origin it tried to reach, which the graph sees
  as `ERROR` and wiki-hop ends on.
- **A click on something that is not a link.** ARIA roles are page content;
  `<button role="link">` snapshots as a link. Bounded by confinement and by
  the context the click runs in: a confined browser is a **fresh context** —
  no cookies, no storage, no downloads — so any same-origin effect a click can
  cause is caused as nobody. A warrant on an origin is therefore an
  authorization to click, as nobody, on that origin. The operator granting one
  on an origin where "as nobody" can still do damage — an intranet without
  login, a local app on `127.0.0.1` — is authorizing that, and the envelope
  says so in plain text.
- **Content reaching the driving model.** An escalation carries 1,500
  characters of the page and every option label to whoever answers it. A
  crafted page can try to instruct that model. What the model can do with an
  instruction: answer the park with a pick (validated against the menu), stop
  the run, or call some other tool — which is a top-level, model-originated
  call the gate classifies exactly as today. The warrant covers none of it,
  and this surface existed before this document; it is not widened here.
- **Content reaching the log.** The decision row stores the chooser's context
  whole, page text included. It always did. The row now says which warrant it
  was made under.

What the page cannot reach, by construction: any tool the warrant does not
name; any origin it does not name; the operator's credentials for any site
(the confined context has none); a shell command that is not one of the
warrant's literal strings; anything after the warrant's count or duration; a
`Deny`.

### What a warrant cannot authorize

- A `Deny`. The three refusals on the merits are untouched, as they are under
  yolo and the judge. A layer that could reach them would be a fourth kind of
  thing.
- A tool it does not name. `tools` is exact, checked by the lint against
  every `tool` action in the graph, and a service that requests anything else
  reaches the ordinary gate.
- A `bash` whose command is not one of its literal `commands`. A command
  rendered from context is not enumerable at authoring time and is therefore
  not covered; it asks, as today. Under the default posture every triage
  command is silent anyway (J1 measured zero shell questions), so `commands`
  matters on a box that runs `[policy] default = "flag"`, which is the box a
  sandbox should be.
- `browser_click` or `browser_type` before the browser has been confined.
  The gate layer watches for a `browser_open` under the warrant whose
  `confine` equals the warrant's `origins`, and until it has seen one those
  two tools ask. Never-optimism: the layer does not assume the service did
  what the graph says; it waits to see the call.
- A call that is not under the warranted call's chain. A top-level call by the
  model during a run — or from a different session — is not the run's.
- A run whose graph changed. The envelope carries the graph's content hash,
  the service refuses a mismatch before the first action, and a standing
  warrant may pin the hash.
- A run of a graph that declares no warrant. An undeclared graph runs
  attended: every click asks, exactly as today.
- Anything when the extensions involved are not trusted. The warrant relies
  on the service that interprets the graph to request what the graph says
  and on the browser service to enforce confinement; both are third-party
  code by the trust ruling's definition, and a warrant naming their tools is
  available only once the operator has vouched for them with
  `[[extensions]] approval = "trust"`. This is what D8's mechanism is *for*
  on this graph: trust is the precondition for a warrant, not a substitute.
- A judge's yes, or a yolo yes. Only a person's answer or the operator's
  standing config creates a warrant (§4).

The table that says this is not yolo with extra steps:

| | `--yolo` | a warrant |
|---|---|---|
| answers | every `Ask`, every tool, whole session | `Ask`s on nested calls under one approved call |
| looks at the call | no | tool name, count, elapsed time, confinement, literal command |
| who said yes | the switch | a person, once, reading the envelope; or the operator's config naming the graph |
| what a hostile service can get | anything the table would have asked about | only the listed tools, inside the listed bounds |
| `Deny` | untouched | untouched |
| on the record | `Yolo`, per call | `Warranted`, per call, with the warrant id; the envelope on the approval record |
| ends | `:yolo off`, session end | the outer call returning, the count, the clock, `:warrant off`, session end |

## 2. The envelope

**Choice.** The warrant is a JSON object with exactly seven keys:

```json
{
  "graph": "wiki-hop@1",
  "sha256": "<64 hex: the graph document, canonical JSON>",
  "tools": ["browser_back", "browser_click", "browser_open", "browser_snapshot"],
  "origins": ["https://en.wikipedia.org"],
  "commands": [],
  "actions": 70,
  "wall_s": 900
}
```

The graph author writes `meta.jev.warrant` with `tools` (required) and
`origins` and `commands` (optional, default empty). The service fills in the
rest when asked: `graph` is `id@version`, `sha256` is the loaded document's
hash, `actions` and `wall_s` are the graph's own `budget`. Lists are sorted
and deduplicated. The **warrant id** is `w_` plus the first twelve hex of the
SHA-256 of the block's canonical JSON — sorted keys, no whitespace, UTF-8, not
ASCII-escaped — computed identically in Python and in Rust, and pinned on both
sides by a test that hard-codes the same literal id for the shipped wiki-hop
graph. The lint admits only printable ASCII in `origins` and `commands` and
tool names already match `^[a-z][a-z0-9_]*$`, so the two serializers cannot
disagree on any block a graph can produce.

**How it reaches the gate.** The caller passes it: `jev_run {graph, input,
warrant}`. `jev_warrant {graph}` — a new read-only tool — returns the exact
block to pass, so a model reads it in one silent call (silent once `jev` is
trusted, which a warrant requires anyway). At `automation.start` the service
canonicalizes both the passed block and the graph's own and refuses on any
difference, echoing the correct block in the error so the consumer is the
specification. A `jev_run` with no `warrant` starts an **attended** run: the
run record says `warrant: null`, and every click asks, as today.

**Rejected.** The Rust side reading `extensions/jev/graphs/<id>.json` to
find the envelope. It would be a second reader of a file whose meaning the
service owns, and this log caught five copies of one fact going stale in a
single day. The block in the call's input is a copy too — but it is verified
against the owner's reading before anything runs, and the gate sees the copy
the operator approved. This is the same shape as `bash` getting the command
string rather than a script name: the gate classifies what it can see. Also
rejected: the driver script declaring the envelope to the host after
`automation.start` answers. The operator must see the envelope *at the
question*, and by then the question has been answered. Also rejected: the
envelope living in `config.toml`. It would duplicate the graph, and the graph
is where a person edits what a run may do; config names *which graphs* may
start unattended (§4), and the graph says what that means. Also rejected: a
warrant on the extension (`[[extensions]] approval = "unattended"`). It would
authorize every graph the extension can run, which is every graph anyone puts
in the directory.

**Why the graph carries `confine` itself.** The graph's `browser_open` action
states `"confine": ["https://en.wikipedia.org"]` explicitly, and the lint
requires it to equal `warrant.origins`. Stated rather than injected: the same
reasoning that keeps `refs.within` in the graph beside the snapshot's
`within`. A graph that omits it lints dirty; an inline graph that omits it
starts, and its clicks ask, because the gate layer never sees the confining
open.

## 3. Where each bound is enforced

A bound is enforced by the one layer that can see it, and this table is the
whole design's spine. Every row names the test that fails if the row is
reverted.

| bound | enforced by | how | test |
|---|---|---|---|
| the tool set | the gate layer (`crates/rune`, `Warrant`) | `call.name ∈ tools`, on `Ask`-tier nested calls under the warranted root | `a_tool_outside_the_envelope_asks_even_under_a_live_warrant` |
| the count | the gate layer | nested dispatches under the root, counted; over `actions`, back to asking | `the_seventy_first_action_under_a_warrant_asks` |
| the duration | the gate layer | from the driving call's approval; over `wall_s`, back to asking | `a_warrant_past_its_wall_clock_asks` |
| literal commands | the gate layer | `input.command` string-equal to one of `commands` | `a_bash_command_not_listed_in_the_warrant_asks` |
| confinement requested | the gate layer | a `browser_open` under the root with `confine` set-equal to `origins`, observed before any click is warranted | `a_click_before_a_confining_open_asks` |
| where a click can go | the browser (`extensions/browser`) | a fresh context with a `route` aborting every request whose origin is not listed; no downloads; popups closed | `a_confined_click_to_an_unlisted_origin_is_refused_naming_it` |
| the menu | the service (`jev/automation/options.py`) | `refs.url`, a regex over the option's snapshot `url` | `a_url_pattern_drops_options_whose_href_does_not_match` |
| `Deny` | the table | untouched by every layer | `a_warrant_never_reaches_a_refusal` |
| the graph's identity | the service | `sha256` in the block against the loaded document | `a_warrant_for_a_changed_graph_is_refused_at_start` |

### Confinement

`browser_open` gains `confine: [origin, …]`. When present, the service
**replaces the shared context**: it closes the current one, opens a fresh
`new_context(accept_downloads=False)`, installs a context-level `route` whose
pattern is a compiled regular expression with a negative lookahead over the
listed origins — so a request to a listed origin never enters Python and
costs nothing, and a request to anything else is aborted — closes any page
the context opens beyond the first, and only then navigates. An open with no
`confine` replaces the context again, unconfined. Confinement is therefore a
property of *the open that set it*, visible in the journal as that call's
input, and it lasts until the next open. The cost, stated: an open with
`confine` discards whatever cookies the shared browser held. A run owns the
one shared browser for its duration already ("this changes what every
`browser_*` call sees next, not just yours"); this makes the ownership
honest.

An origin is scheme, host and port, lowercase host, no path; `localhost` and
`127.0.0.1` are different origins, and a graph that needs both lists both.
The start URL's origin must be literal in the graph — a template before the
third slash is a lint error — and must be in `origins`.

A click whose navigation the route aborted returns an error naming the origin
it tried to reach and the list it was not on. That is `is_error`, which
`run.py` raises as `reason: failed`, which wiki-hop's root `ERROR` handler
turns into outcome `error` with the message in the report. A run that hits
its confinement stops and says where it tried to go.

### The menu bound

`refs` gains `url`, a regular expression; an option is kept only when its
snapshot `url` is present and `re.search` matches. wiki-hop uses
`"^/wiki/[^:#?]+$"`: article-namespace links only, so `Special:`, `Help:`,
`action=edit` under `/w/index.php`, anchors and external references never
reach the chooser. This is the bound the adversary meets first: whatever text
it writes, it can only steer toward what the graph already admitted.
Confinement is the backstop for the case where the `url` line lied.

The menu bound is graph authoring and stays optional in the schema; a
`refs` source with `link` in its roles and no `url` under a warrant is not a
lint error, because a graph may legitimately follow off-site links inside a
listed origin set. It is the recommended shape and both worked examples use
what they need.

## 4. The question, and who answers it headless

**Choice.** `jev_run` asks, once, with the envelope in the question:

> `jev_run` — drives a graph run: every action the graph picks is dispatched
> under this call. It asks for warrant `w_3f9a1c2d7e4b` — `wiki-hop@1`
> (`8b1c…`): tools `browser_back, browser_click, browser_open,
> browser_snapshot`; origins `https://en.wikipedia.org`; no commands; ≤70
> actions, ≤900 s. Run it?

A yes is journaled as `Approved` with the warrant's line in the record's
`note`. `jev_resume` carries the same block (the escalation payload now
includes it, and the `answer` hint says so); a resume whose block hashes to a
warrant already approved in this session is allowed with outcome `Warranted`,
note "under warrant w_… approved at call <id>". So a run that parks thirty
times and is resumed thirty times by the driving model raises one question.
Each new `jev_run` asks again: a warrant is *this envelope, approved in this
session*, and it does not make a second run silent by name — a second run is
a second authorization, and the person who gave the first is still there.

**Headless**, nobody is there, and `NoUser` declines every question by
design. The operator's config names the graphs that may start without one:

```toml
[[warrants]]
graph = "triage-linux@1"
# sha256 = "…"     # optional: pin the exact document
```

A top-level call carrying a valid block whose `graph` (and `sha256`, if
pinned) matches a standing entry is allowed with outcome `Warranted`, note
"standing warrant w_… for triage-linux@1 ([[warrants]])", and its chain is
covered by the block exactly as an approved one is. Without the pin the
operator vouches for the graph by name, future edits included — the same
trust model as vouching for an extension by name — and the enforced envelope
is always the *current* document's declared block, verified by the service,
so an edit that widens the block is a wider envelope the next launch will
carry and the journal will show, never a silent widening of an old approval.
On a box where the graph directory is written by anyone but the operator,
pin the hash.

This replaces `--yolo` in the sandbox posture (`automation.md` §6). A
sandbox's config gains `[[extensions]] name = "jev" approval = "trust"` and
`[[warrants]] graph = "triage-linux@1"`; the Melete seam's command drops
`--yolo`. Under that posture a triage command the table flags is declined by
`NoUser` and reaches `recover`, which is the true outcome of that command on
that box.

**Rejected.** A judge's yes creating a warrant. The judge is allowed to be
wrong because a wrong judge costs one question; a judge whose yes covered
seventy clicks would cost seventy. Also rejected: a yolo yes creating one.
Under yolo everything is waved anyway, and the journal should say `Yolo`, not
dress a blind yes as a warrant. In both cases the layer finds the root's
verdict is neither `Approved` nor `Warranted` and leaves every nested click to
the layers above it — under yolo they are waved, under a judge they are judged
one by one, as today. Also rejected: treating an operator-originated call
(`/do jev_run …`, `CallOrigin::User`) as pre-approved. It would change what
every user-originated `Ask` means, for one caller's convenience, and the
standing warrant covers the headless case without touching that invariant.
Also rejected: a warrant that outlives the session. A reopened session comes
back gated, for the reason the yolo switch does not persist.

**Where the layer sits.** `Warrant` is a decorator on the same seam as
`Yolo` and `Judge`, installed **innermost** — directly over the table, under
yolo, under the judge — so a warranted call is recorded as `Warranted` even
in a yolo session, and so the judge is never called for a call the operator's
own word already covers. Like the judge it only ever answers an `Ask`; like
yolo it rewrites the verdict rather than annotating it, because there is no
model to be wrong about — the operator's answer is the whole content. Unlike
both, it also *observes* `Allow`-tier nested calls (to see the confining
open and to count), rewriting none of them; and it may make an `Ask` more
specific by appending the envelope, never less.

**How the layer knows what it is under.** A nested dispatch's `pre_tool`
runs on the driving script's own thread, inside the `block_on` that
`ScriptTool::call` pins the VM to, which is how `dispatch` already reads the
chain rail. A fourth rail, `ROOT`, carries the outermost script call's id,
planted by `ScriptTool::call` from a `call_id` the dispatcher now puts on
`CallContext`, and replanted onto every nested thread unchanged. A top-level
call reads `None`; a click under `jev_run` reads `jev_run`'s call id. Nothing
a script can do changes it.

**How the layer knows the answer.** It does not see it — the dispatcher asks
after the hook returns. So a warrant requested by an `Ask` is *provisional*
until the layer confirms, from the session journal, that the root call's
`PolicyVerdict` is `Approved` or `Warranted`; a declined root leaves a
provisional entry that no journal record ever confirms, and its chain never
runs. The confirmation is one backward scan of the branch per root, cached;
the judge already reads the branch from the same position.

## 5. What is recorded

An operator reconstructing an unattended run needs to answer: what was
authorized, by whom, on what basis, and what each step was allowed under.
Four records, joined by two keys — the warrant id `w_…` and the graph hash.

**The session journal** (`RecordKind::PolicyVerdict`):

- On the root call: outcome `Approved` (a person) or `Warranted` (standing
  config, or a resume under an approved envelope), and `note` carrying the
  warrant id, the graph ref and hash, and the envelope summary. Today `note`
  is written only for `Judged`; it now carries the warrant line for any
  outcome the layer annotated, including `Declined` — an operator saying no
  to a warrant is worth reading later.
- On every nested call the layer answered: outcome `Warranted`, `reason` the
  table's own (`clicks something the page's own content named`), `note`
  "under warrant w_… (call <root id>); action 7/70". A ledger grouping by
  reason must treat `Warranted` as it treats `Yolo` — not evidence for
  promoting the table entry, since nothing looked at the argument.
- On every nested call the layer declined to answer, whatever the layers
  above did: the ordinary record, with the `Ask` prompt naming why it was
  outside the warrant.

**The run record** (`%LOCALAPPDATA%\eidolon\extensions\jev\runs\<run>.jsonl`):
`warrant` — the whole block, or `null` — on every park snapshot, so a
reconstructed run keeps it, and on the resolved marker's smaller line the id
alone.

**The decision row**: `"warrant": "w_…"` or `null`. `source` and `verified`
are unchanged — a warranted run's chooser picks are still `jevlike`/`null`,
and a model's park answers still `model`. The run-end row
`{run, outcome, ts}` gains `warrant` too, and this document notes a finding
on the way: `decisions.log_run_end` has **no caller** in `run.py` today
(verified by grep), so the `outcome` promotion `automation.md` §5 describes
has never had a row to read. The spec adds the call.

**The escalation payload and the final report**: `warrant` — the block with
its `id`, or `null` — so a model answering a park has the block to pass to
`jev_resume`, and a report says under what it ran.

A reconstruction, then: the report names `warrant.id`; the journal's root
record with that id in its note says who approved it and shows the envelope;
every `Warranted` record under that root is a step it authorized; every
decision row with that id is a pick made under it; the run record's hash
matches the block's `sha256`, and the graph file with that hash is the graph
that was authorized — or, if no file has it any more, the graph has been
edited since, which is itself the finding.

## 6. Revoking and bounding

- **Now, this session:** `:warrant` lists the warrants approved in this
  session with their ids, graphs, actions used and age; `:warrant off`
  revokes them all. Revocation is immediate: the next nested click finds no
  live warrant and asks, which unattended is a refusal the graph sees as
  `ERROR`. `jev_stop {run, reason}` ends the run itself; the table now allows
  it silently, because stopping a run is the operator's `kill`.
- **The run:** cancelling the driving turn cancels the nested dispatch
  (proved by `cancelling_the_outer_call_cancels_a_nested_dispatch`), the run
  is `cancelled` and resumable, and a resume asks again in a new session.
- **The envelope:** edit the graph. Narrow `origins`, drop a tool, shorten
  `budget.actions` or `wall_s`, add `url` to a `refs` source. The next launch
  carries the narrower block, and a standing warrant with a pinned hash
  refuses the old one.
- **Standing:** remove the `[[warrants]]` line; the next session has no
  standing warrant. Trust is a bound too: `approval = "ask"` on `browser` or
  `jev` makes every warrant naming their tools unavailable, and the `jev_run`
  question says so in its prompt.
- **How often it asks a person anyway:** the confidence floor is not
  authorization, but it is the dial for how much of a run a person sees.
  Under a warrant the driving model is the second tier for chooser
  uncertainty, and the operator is the authority for the envelope. Raising
  `meta.floor` makes the run park more; `escalations` in the budget bounds
  how many times.

## 7. What this costs, and what it does not solve

- **One more decorator on the hook seam, and one more rail.** Disarmed it is
  a thread-local read and a match on the verdict per call. The journal scan
  is once per root.
- **A confining open drops the shared browser's cookies.** Stated in §3. A
  person who was logged into something through the shared browser is logged
  out by a warranted run starting. The alternative — a warranted run acting
  with whatever credentials happened to be in the jar — is the thing this
  ruling exists to refuse.
- **`browser_open` carries a security-relevant argument on a tool the table
  allows unconditionally.** Confinement only ever narrows, and an unconfined
  open is what every open is today, so the table's opinion of `browser_open`
  is unchanged. The gate layer verifies the argument on the one path that
  depends on it.
- **The model must fetch and pass the block.** One extra read-only call per
  run, silent under trust; a forgotten block is an attended run, never a
  refused one.
- **The gate layer names two browser tools and one browser argument.** The
  confinement precondition is a coupling between `crates/rune` and
  `extensions/browser`'s `confine` contract, documented in `extensions.md`.
  It is the same kind of coupling `policy.rn` already has with
  `browser_click` by name, and it is the price of a layer that verifies
  rather than assumes.
- **The count and the clock bound one driving call, not the run.** A
  `jev_resume` is a new root with its own ledger, so a run resumed *n*
  times may dispatch up to *n* × `actions` under the gate's own counting.
  The run-level budget — actions, wall, escalations — is the service's, and
  a warrant requires the service trusted; the gate's count is the backstop
  for a single driving call, which is the unit the gate can see. A service
  that lied about its budget would get the count again on each resume, and
  each resume is a call the driving model makes in the transcript, under a
  root the journal records. Keying the ledger by run id instead would need
  the gate to know which of a service's tools opens a run and which
  continues one — service-specific knowledge in the layer, refused for the
  reason `classify_tool`'s signature is fixed.
- **A blocked click ends a wiki-hop run.** The graph's root `ERROR` handler
  is `failed`, so one off-origin navigation stops the run with a report that
  names the origin. A graph that wants to survive one handles `ERROR` with a
  `browser_back` and a re-read; that is graph authoring.
- **Rows from unattended runs are weaker training data**, and say so by
  carrying the warrant id. The exporter (not yet built) must treat an
  `outcome`-promoted row from a warranted run as the weakest tier and say so
  in its output; this document rules it and the exporter's spec inherits it.
- **The `type` tool with `submit: true` is a state change on a listed
  origin**, as nobody. A warrant naming `browser_type` authorizes that on
  those origins, and the envelope shows the tool name; whether that is
  acceptable is the operator's reading of the origin list.
- **Unconfined browsing is unchanged.** The loopback `fetch()` shape D1r
  found is closed while a warrant is live and open otherwise; closing it for
  interactive use is a browser-extension hardening this document names and
  does not rule on.
- **`ask: user`, detached runs, `act`-suspended persistence:** untouched.
- **Melete's connector, the Claude CLI's own tools, MCP-served tools:** a
  warrant covers script-originated nested calls under a root on this host
  and nothing else; a backend running its own tools never dispatches through
  a script here.
- **The chain rail's names are still what the depth error prints.** `ROOT`
  is a separate rail; nothing about `MAX_DISPATCH_DEPTH` changes.

## 8. Measurements not yet taken

Each names the fallback that holds until it is taken. None is guessed.

- **W1 — the cost of the confinement route on a real page.** Warm
  `browser_open` of `https://en.wikipedia.org/wiki/Cat` with and without
  `confine`, on this box, wall seconds. The route pattern is a negative
  lookahead so listed-origin traffic never enters Python; the expectation is
  no measurable difference, and it is *not stated as a number*. Fallback if
  the difference is material: a `"**/*"` glob with a Python handler that
  `continue_()`s listed origins, which is slower and equally correct.
- **W2 — how many of the Cat page's `main` links survive
  `^/wiki/[^:#?]+$`.** Counted by the test against
  `jev/tests/fixtures/wiki/cat_main_scoped.txt` and reported in the Build-Log,
  not assumed. No fallback needed; it is a count.
- **W3 — a fresh context per confining open on Playwright 1.63.** Reasoned
  from the API (`Browser.new_context`, `BrowserContext.route` accepting a
  compiled pattern, `accept_downloads`); observed by the browser step's tests
  against a local HTTP server on two ports. Fallback if `route` will not take
  a pattern object on this version: W1's glob handler.
- **W4 — the journal scan's cost on a long branch.** Expected microseconds;
  the scan is backward from the tail and stops at the root's record, which is
  the most recent `jev_run` verdict. Fallback: none required — it happens
  once per root.
- **W5 — the live re-run.** The number in the last section of this document
  is computed by the same test shape D8 used, against the real compiled
  table with the real layer over it. Whether a live wiki-hop under a warrant
  raises exactly one question on this box, with a real model driving, is the
  measurement the Build-Log entry must carry; until it does, the accounting
  is a test's claim about the gate, not an observation of a session.

## Platform

Nothing in this design is platform-shaped, and the two places it could have
been are named. The graph hash is over canonical JSON of the parsed document,
never file bytes, so a CRLF checkout on Windows and an LF one on Linux hash
the same graph the same. The warrant id is over canonical JSON restricted by
lint to printable ASCII, so Python's and Rust's serializers agree by
construction and the pinned-id test proves it. The rails are thread-locals,
the confinement is Chromium's own request interception through Playwright's
context API, the config is `toml` under the existing `config_dir()`, and
`[[warrants]]` has no path in it. The Linux Chromium sandbox note in
`service.py` stands unchanged and unexercised here.

---

## Implementation specification

Six steps, three agents. Each step leaves its tree compiling and its own
suite green. Nothing here reads a document; every name, key, constant and
call site is stated. Test names are the names to use; for every change the
test beside it is the one that fails if the change is reverted, and a change
without one is documentation.

| step | tree | agent | depends on |
|---|---|---|---|
| 1 | `eidolon/crates/core` | Rust | — |
| 2 | `eidolon/crates/rune` | Rust | 1 |
| 3 | `eidolon/crates/cli`, `eidolon/crates/tui`, `eidolon/crates/web`, `eidolon/docs/extensions.md`, `crates/cli/system/RUNE.md` | Rust | 2 |
| 4 | `extensions/browser` | browser | — (land after S31, which is live in that directory) |
| 5 | `jev/`, `extensions/jev/`, `docs/design/automation.md` | jev | — for its suite; 4 for the live run |
| 6 | the live re-run and its Build-Log entry | whoever runs it | 1–5, and a rebuilt binary |

Steps 1–3 are one Rust agent in sequence. Steps 4 and 5 run in parallel with
them and with each other. The release binary is stale until 1–3 are built;
step 5's suite runs against `FakeDriver` and needs no binary. Do not run
`cargo build --release` while other agents are live in the tree; the rebuild
is step 6's.

### Step 1 — `crates/core`

**`eidolon/crates/core/src/policy.rs`**

1. `Ruling` gains `pub warrant: Option<String>` after `yolo`, with this doc
   comment: set by the warrant layer — on an `Allow` it rewrote from an
   `Ask`, the line that names the warrant and the step; on an `Ask` it left
   standing, the line that names the warrant the call is requesting. Like
   `yolo`, a separate field rather than a rewritten `judged`, because a
   person's yes applied by rule is a fourth way a question becomes a yes,
   and a ledger that counted it as a judge's or a switch's would be the
   boundary quietly moving itself. `From<Verdict>` (line 166) sets
   `warrant: None`. Every literal `Ruling { … }` in the workspace gains
   `warrant: None`; the compiler enumerates them — `yolo.rs:176`,
   `tests/gate.rs:58` and `:68`, `crates/rune/src/policy.rs:421` and `:449`.
   `escalate.rs:243` and `yolo.rs:134` use `..ruling` and need nothing.
2. `verdict_note` (line 211): `Warranted` returns `None`, with one sentence
   added to the function's doc: a warranted call is invisible per call for
   yolo's reason — the warrant was visible once, as the question the
   operator answered with the envelope in it, and thirty lines restating it
   would be the interruption the warrant removes.
   Test: `a_warranted_outcome_has_no_screen_note` in `tests/gate.rs` beside
   the existing `for quiet in [` loop (line 437).

**`eidolon/crates/core/src/session/mod.rs`**

3. `PolicyOutcome` gains `Warranted` **after** `Yolo` — appended, as a
   journaled enum must be — with this doc comment: asked, and the operator's
   own earlier answer covered it — a warrant they approved in this session
   with the envelope in front of them, or one they wrote into `[[warrants]]`
   — so the question became a yes by rule. Its own outcome for the reason
   `Judged` and `Yolo` are: a ledger must be able to tell a person's yes
   given once and applied thirty times from a person's yes given thirty
   times, and from a switch. The `reason` is still the classifier's own; the
   `note` names the warrant.
4. `RecordKind::PolicyVerdict`'s `note` doc (line 244): "The judge's one-line
   reasoning, on `Judged`; the warrant's line — its id, and on the root call
   the envelope — on `Approved`, `Declined` and `Warranted` when a warrant
   was requested or applied. Nothing carries one on `Yolo`."
   Test: `crates/core/tests/pins.rs` — `ordinal` needs no change (the
   variant is inside `PolicyVerdict`), but the pin table gains a row: a
   `PolicyVerdict` with `outcome: PolicyOutcome::Warranted` and
   `note: Some("w_1".into())`, its hex pinned by running it once; every
   existing hex string is unchanged, which is what proves the append.

**`eidolon/crates/core/src/tool.rs`**

5. `CallContext` gains `pub call_id: String` — the id of the call this
   context belongs to, so a script tool can plant it as the root of any
   chain it starts (`crates/rune/src/host.rs`'s `ROOT`). Its doc: "Grows as
   tools need more" already; add that `call_id` is the dispatcher's own
   `ToolCall::id`, never minted here. Literal sites: `dispatch.rs:540`
   (`call_id: call.id.clone()`); the other six — `crates/remote/src/melete.rs:588`,
   `crates/rune/src/ext.rs:715` and `:746`, `crates/rune/src/script.rs:258`,
   `:390` and `:533` — are all inside test modules and take any string.

**`eidolon/crates/core/src/dispatch.rs`**

6. `adjudicate` (line 380): a new arm **before** `Verdict::Allow if
   ruling.yolo`:
   `Verdict::Allow if ruling.warrant.is_some() => (Ok(()), PolicyOutcome::Warranted),`
   with the comment: a question the operator's earlier answer covered. It
   journals, like yolo's, because the record of what ran under a warrant is
   the warrant's whole audit trail. Order matters and is stated: the warrant
   layer sits under yolo, so a ruling cannot carry both; the arm order is
   for a reader, not a tie-break.
7. `journal_verdict` (line 437): `note: ruling.judged.clone().or_else(|| ruling.warrant.clone())`
   in both the record and the `Event::PolicyVerdict`. The judge's line wins
   when both exist; a judged root loses the warrant request line, which is
   acceptable because §4 rules a judged root creates no warrant.
   Tests (`tests/gate.rs`, beside `yolo_answers_the_question_and_says_so_in_the_log`
   at line ~460):
   - `a_warranted_allow_is_journaled_as_warranted_with_its_note` — a `Fixed`
     hook returning `Ruling { verdict: Allow, warrant: Some("w_1 …"), .. }`;
     `verdicts(&s)` shows one record, outcome `Warranted`, note `Some`, the
     reason preserved.
   - `an_approved_call_keeps_the_warrant_request_line_as_its_note` — a
     `Fixed` hook returning an `Ask` with `warrant: Some("requests w_1")`
     and a `ScriptedUser::new(true)`; the record is `Approved` with that
     note. (Fails if `journal_verdict` keeps writing `ruling.judged` alone.)
   - `a_declined_call_keeps_it_too` — same with `ScriptedUser::new(false)`;
     `Declined`, note present.

### Step 2 — `crates/rune`

**`eidolon/crates/rune/src/host.rs`**

1. A fourth thread-local beside `CHAIN` (line 185):
   `static ROOT: RefCell<Option<String>>` — the id of the outermost script
   call the thread is running under, `None` on a thread that is not inside
   any script call. `pub fn with_root<R>(root: Option<String>, f: impl FnOnce() -> R) -> R`
   and `pub(crate) fn current_root() -> Option<String>`, the same shape as
   `with_chain`/`current_chain` and sound for the same reason. Its doc says
   what §4 says: read by the warrant layer to know which call a nested
   dispatch is under; nothing a script does can set it; and — the one
   difference from `CHAIN` — it does not accumulate, it is set once by the
   outermost `ScriptTool::call` and carried unchanged into every nested
   thread.
2. The module doc's "Depth crosses a thread it did not spawn" section gains
   a paragraph naming `ROOT` and what it is for.

**`eidolon/crates/rune/src/script.rs`**

3. `ScriptTool::call` (line 193): before the thread is spawned, on the
   calling thread, `let root = current_root().or_else(|| Some(ctx.call_id.clone()));`
   with the comment: the root is whichever call was outermost — read here,
   where the calling thread's rail is visible, exactly as `chain` is — and a
   call with no root above it is its own. Inside the spawn, `with_root(root,
   || with_chain(chain, || …))` wrapping the existing nest.
   Tests (in `host.rs`'s `mod tests`, the harness of
   `a_nested_dispatch_is_never_laundered_into_the_models_own_origin` at line
   1019): `a_nested_dispatch_sees_the_outermost_call_as_its_root` — a
   recording policy hook that captures `current_root()` at `pre_tool` time
   (read synchronously at the top of `pre_tool`, before any `.await`); a
   driver script dispatching `bash`; the captured root for the inner `bash`
   is the driver's own `ToolCall::id`, and the captured root for the driver
   itself is `None`. `a_two_deep_chain_keeps_the_same_root` — driver → helper
   → `bash`; all nested captures equal the driver's id.

**`eidolon/crates/rune/src/policy.rs`**

4. `PolicySettings` gains two fields, both read by the warrant layer only:
   `pub enabled_extensions: Vec<String>` — every extension this session
   loads, so the layer can tell an extension's tool (which needs its
   extension trusted) from a built-in (which does not), by the same
   `<extension>_<name>` boundary `effective_approval` uses; and
   `pub standing_warrants: Vec<crate::warrant::Standing>` — the operator's
   `[[warrants]]` entries. `Default` is a hand-written impl (line 84), not a
   derive: both get `Vec::new()` there. The struct derives only `Clone` and
   `Debug`, so `Standing` must be both. The doc on `trusted_extensions`
   gains one sentence: a warrant naming an extension's tool is available
   only when that extension is here.
5. The `pre_tool` at line 416 is unchanged except for `warrant: None` in
   its two literals (step 1). The prompt it builds stays
   `"{name} — {reason}. Run it?"`; the layer composes its own sentence
   around the same `reason`.

**`eidolon/crates/rune/policy.rn`** — four rows in `classify_tool`, before
the `_ =>` catch-all, with the comment: jev's own driver tools (D8, A6).
`jev_run` and `jev_resume` are the calls a graph's every action is
dispatched under, and asking about them is asking about the run; the
warrant layer in `crates/rune/src/warrant.rs` may answer the question from
the operator's earlier word, but the table's opinion — that a run is worth
one question — is unconditional. `jev_stop` is the operator's `kill` for a
run and is never worth a question. `jev_runs` reads.

```rune
"jev_run" => v(FLAG, "drives a graph run: every action the graph picks is dispatched under this call", false),
"jev_resume" => v(FLAG, "drives a graph run: every action the graph picks is dispatched under this call", false),
"jev_stop" => v(ALLOW, "stops a graph run", false),
"jev_runs" => v(ALLOW, "lists graph runs", true),
```

Tests (`policy/tests.rs`, beside the D8 block at line 657):
`jev_run_and_resume_flag_with_the_runs_own_reason` (asked, and the ruling's
`reason` is the string above, not `unclassified tool …`);
`jev_stop_is_never_asked_about`; `trust_never_promotes_mutating_or_destructive_tools`
(line 763) keeps passing — `jev_run`/`jev_resume` still ask, `jev_stop` is
removed from its assertions and moved to its own test.

**`eidolon/crates/rune/src/warrant.rs`** — new. `pub mod warrant;` in
`lib.rs` after `policy` (line 99). Everything below is `pub` only where
`crates/cli` needs it.

6. `Envelope` — the seven keys, parsed **strictly** from a call's
   `input.warrant`: `graph: String`, `sha256: String` (64 lowercase hex),
   `tools: BTreeSet<String>`, `origins: BTreeSet<String>`,
   `commands: BTreeSet<String>`, `actions: u64`, `wall_s: u64`. Any missing
   key, extra key, wrong type, or non-printable-ASCII origin or command is
   `Err(String)` naming the key; a call with no `warrant` key at all is
   `Ok(None)`.
   - `fn canonical_json(&self) -> String` — a `serde_json::Map` (sorted by
     construction) with the seven keys, lists as sorted `Vec<String>`,
     `serde_json::to_string`.
   - `fn id(&self) -> String` — `"w_"` + the first twelve hex of
     `sha256(canonical_json)`. Nothing in the workspace hashes today: add
     `sha2 = "0.10"` to `[workspace.dependencies]` in `eidolon/Cargo.toml`
     and `sha2.workspace = true` to `crates/rune/Cargo.toml`'s
     `[dependencies]`, with a comment that it exists for the warrant id and
     nothing else.
   - `fn summary(&self) -> String` — one line:
     `"{graph} ({sha256[..8]}…): tools {tools joined ", "}; origins {…|none}; commands {n|none}; ≤{actions} actions, ≤{wall_s} s"`.
   Tests (`warrant.rs`'s `mod tests`):
   `the_shipped_wiki_hop_envelope_has_the_pinned_id` — the literal seven-key
   block for the shipped wiki-hop graph (the executor takes `sha256` from
   step 5's pinned Python test and pins the same `w_…` here; the two tests
   carry the same two literals and are the cross-language contract);
   `an_envelope_with_an_extra_key_is_refused_naming_it`;
   `a_non_ascii_origin_is_refused`; `key_order_and_list_order_do_not_change_the_id`.
7. `Standing { pub graph: String, pub sha256: Option<String> }`, deriving
   `Debug, Clone, PartialEq, serde::Deserialize` so `crates/cli` can parse
   `[[warrants]]` straight into it (`#[serde(default)]` on `sha256`);
   `fn matches(&self, env: &Envelope) -> bool` — `graph` equal, and
   `sha256` equal when pinned.
8. `Warrants` — the shared handle, `Clone`, over
   `Arc<std::sync::Mutex<State>>`, the shape `yolo::Switch` has and for the
   same reason (thrown from the UI thread, read on the driver): `pub fn
   list(&self) -> Vec<Listed>` (`id`, `graph`, `root_call`, `actions_used`,
   `actions_max`, `age_s`, `confirmed`) and `pub fn revoke_all(&self)`.
   `State`: `roots: VecDeque<Live>` bounded at 8 (evict oldest),
   `Live { id, envelope, root_call: String, root_tool: String, started:
   Option<Instant>, actions: u64, confined: bool, confirmed: bool }`, and
   `revoked: BTreeSet<String>` of ids revoked this session. `started` is
   `None` until the root is confirmed, so the minutes a person takes to
   read the question are not charged to the warrant's clock.
9. `Warrant` — the hook: `inner: Arc<dyn PolicyHook>`, `session:
   Arc<tokio::sync::Mutex<Session>>`, `settings: PolicySettings` (for
   `trusted_extensions`, `enabled_extensions`, `standing_warrants`), `state:
   Warrants`. `pub fn new(inner, session, settings) -> Self`; `pub fn
   handle(&self) -> Warrants`.
10. `impl PolicyHook for Warrant`, `pre_tool`:
    1. `let root = current_root();` — **first statement, before any
       `.await`**, with the comment that the rail is a thread-local and the
       future is polled on this thread, and reading it synchronously makes
       that fact irrelevant.
    2. `let ruling = self.inner.pre_tool(call, manifest, cwd).await;`
    3. If `root.is_none()` (**top-level**):
       - `Envelope::parse(&call.input)`: `Ok(None)` → return `ruling`.
         `Err(why)` → if `ruling.verdict` is `Ask`, return it with the prompt
         re-composed as `"{name} — {reason}. It carries a warrant block that is not valid ({why}); every action it dispatches will ask. Run it?"`
         (`{reason}` is `ruling.reason_or_prompt()` here and below),
         `warrant: None`; else return `ruling`.
       - `eligible(&env, &call.name)`: the root tool's extension (by the
         `<ext>_` boundary against `enabled_extensions`) must be in
         `trusted_extensions`; every name in `env.tools` that has an enabled
         extension's prefix must have that extension trusted; built-ins
         need nothing. `Err(why)` → same re-composed `Ask` with
         `"a warrant is not available: {why}"`; `Allow`/`Deny` returned
         untouched.
       - `Deny` → return `ruling` (a warrant never reaches a refusal).
       - Remember a provisional `Live` for `call.id` (`confirmed: false`,
         `confined: env.origins.is_empty()` — a graph with no origins has
         nothing to confine).
       - If `env.id()` is in `revoked` → fall through to the plain `Ask`
         below (revocation outlasts a re-request in this session).
       - If any `standing.matches(&env)` → mark the `Live` confirmed with
         `started: Some(Instant::now())` and return
         `Ruling { verdict: Allow, warrant: Some(format!("standing warrant {id} for {graph} ([[warrants]]): {summary}")), ..ruling }`
         whatever the inner verdict was, `Deny` excepted (already returned).
       - Else if some other `Live` with the same `id` is `confirmed`, or
         becomes confirmed by `self.confirm(&mut live).await` now → return
         `Ruling { verdict: Allow, warrant: Some(format!("under warrant {id} approved at call {root_call}: {summary}")), ..ruling }`
         when the inner verdict was `Ask`; when it was `Allow`, the same
         with `verdict` unchanged.
       - Else, `Ask` → return it with the prompt re-composed as
         `"{name} — {reason}. It asks for warrant {id}: {summary}. Run it?"`
         and `warrant: Some(format!("requests warrant {id}: {summary}"))`.
         `Allow` → return `ruling` (no question, no warrant; a table that
         allows `jev_run` outright has not shown anyone the envelope).
    4. If `root == Some(root_id)` (**nested**):
       - `live = state.live_for(&root_id)`; none → return `ruling`.
       - If `!live.confirmed`: `live.confirmed = self.confirm(&root_id).await`
         — lock the session, scan `branch()` from the tail for
         `RecordKind::PolicyVerdict { tool_use_id, outcome, .. }` with
         `tool_use_id == root_id`; `true` iff `outcome` is `Approved` or
         `Warranted`; not found or anything else → `false`. On `true`, set
         `live.started = Some(Instant::now())`. Unconfirmed → return
         `ruling`. Locking the session here is safe: the dispatcher releases
         it after `journal_verdict` and never holds it across `tool.call`
         (`dispatch.rs`, `run`, line 510), and the judge already locks it
         from this same position on nested calls.
       - If `live.id ∈ revoked` → return `ruling`.
       - `live.actions += 1`.
       - **Observe**: if `call.name == "browser_open"`, `live.confined =`
         the input's `confine` is an array of strings set-equal to
         `env.origins` (absent or different → `false`).
       - If `ruling.verdict` is not `Ask` → return `ruling`.
       - Check, first failure wins, each with its own sentence:
         `call.name ∈ env.tools` ("tool `{name}` is not in the warrant");
         `live.actions <= env.actions` ("action {n} is past the warrant's
         {max}"); `live.started.unwrap().elapsed() <= wall_s` ("the warrant's
         {wall_s} s have elapsed"); if `call.name` is `browser_click` or `browser_type`,
         `live.confined` ("the browser has not been confined to the
         warrant's origins by a browser_open"); if `call.name == "bash"`,
         `input.command` is a string in `env.commands` ("the command is not
         one the warrant lists").
       - All pass → `Ruling { verdict: Allow, warrant: Some(format!("under warrant {id} (call {root_id}); action {n}/{max}")), ..ruling }`.
       - A failure → return the `Ask` with its prompt re-composed as
         `format!("{} — {} (outside warrant {id}: {why}). Run it?", call.name, ruling.reason_or_prompt())`,
         `warrant: None`. Every re-composed prompt in this layer is built the
         same way, from `call.name` and `ruling.reason_or_prompt()` — the
         classifier's own reason, never the inner prompt's sentence.
    Tests (`warrant.rs`'s `mod tests`; a helper `nested(root_id, f)` that
    runs `f` under `with_root(Some(root_id), || rt.block_on(f()))` on a
    fresh `new_current_thread` runtime, the way `ScriptTool::call` does; a
    `Session::create(&dir.join("s.eid"), "t", dir, None)` in a
    `tempfile::tempdir()`, as `host.rs`'s tests build one (line 990);
    `ScriptPolicy::builtin` with a `PolicySettings` naming `browser` and
    `jev` trusted and enabled; `approve(session, root_id)` appends a
    `PolicyVerdict { outcome: Approved }` for the root, as the dispatcher
    would have):
    - `a_top_level_jev_run_with_a_valid_envelope_asks_with_the_envelope_in_the_prompt`
      — the prompt contains the id and the summary; `ruling.warrant` is
      `Some` starting with `requests warrant`.
    - `a_click_under_an_approved_root_is_warranted` — approve; a nested
      `browser_open {url, confine}` (Allow, observed); a nested
      `browser_click` → `Allow`, `warrant: Some` containing the id and
      `action 2/70`.
    - `a_click_under_a_declined_root_asks` — a `Declined` record instead;
      the click's verdict is `Ask` and the prompt is the table's own.
    - `a_click_before_a_confining_open_asks` — approve; no open; click →
      `Ask`, prompt names confinement.
    - `an_open_with_the_wrong_origins_does_not_confine` — `confine:
      ["https://example.org"]`; click → `Ask`.
    - `a_tool_outside_the_envelope_asks_even_under_a_live_warrant` — approve
      and confine; a nested `browser_type {ref: "e1", text: "x"}` — flagged
      by the table, and not in wiki-hop's `tools` — → `Ask`, the prompt
      naming `browser_type` as outside the warrant, `warrant: None`.
    - `a_warrant_never_reaches_a_refusal` — nested `bash {command: "sudo id"}`
      under an envelope whose `tools` includes `bash` and whose `commands`
      lists `"sudo id"` → still `Deny`.
    - `a_bash_command_not_listed_in_the_warrant_asks` — a triage-shaped
      envelope (`tools: [bash]`, `commands: ["docker ps"]`), nested
      `bash {command: "docker images"}` (flagged by the table) → `Ask`;
      `bash {command: "docker ps"}` → `Allow`, `Warranted`.
    - `the_seventy_first_action_under_a_warrant_asks` — `actions: 70`; the
      open, then 69 snapshots and clicks counted; the click that makes 71
      asks naming `71` and `70`.
    - `a_warrant_past_its_wall_clock_asks` — `wall_s: 0` (the lint refuses
      it in a graph; the layer does not) → the first click asks naming the
      clock.
    - `a_resume_carrying_an_approved_envelope_is_warranted` — after
      approval, a top-level `jev_resume {run, pick, warrant}` with the same
      block → `Allow`, `warrant` note `under warrant … approved at call …`,
      and the dispatcher would journal it `Warranted`.
    - `a_resume_carrying_a_different_envelope_asks`.
    - `a_standing_warrant_answers_the_root_and_covers_its_chain` — settings
      with `Standing { graph: "wiki-hop@1", sha256: None }`; the root →
      `Allow` with the standing note; nested clicks (after a `Warranted`
      record for the root is appended, as the dispatcher would) → `Allow`.
    - `a_standing_warrant_with_a_pinned_hash_refuses_another_document` —
      `sha256: Some(other)` → the root asks.
    - `an_untrusted_browser_makes_the_warrant_unavailable_and_says_so` —
      settings trusting `jev` only; the root's prompt names `browser`; the
      nested click asks.
    - `an_untrusted_driver_extension_makes_the_warrant_unavailable`.
    - `a_judged_root_creates_no_warrant` — a `Judged` record for the root;
      the click asks.
    - `a_yolo_root_creates_no_warrant` — a `Yolo` record; the click asks (and
      the layer returned the table's `Ask`, which a `Yolo` above would wave).
    - `revoke_all_makes_the_next_click_ask_and_the_next_request_ask` —
      approve, one warranted click, `handle.revoke_all()`, the next click
      asks; a new top-level `jev_run` with the same block asks (revocation
      outlasts re-request); `list()` is empty.
    - `an_allow_tier_root_with_an_envelope_creates_no_warrant` — an inner
      hook fixed to `Allow` for the root; the nested click asks.
11. **The accounting**, in `policy/tests.rs` after
    `the_wiki_hop_gate_count_drops_from_62_to_31_after_d1_and_d8` (line 859):
    - `wiki_hop_calls()` (line 811) becomes `wiki_hop_calls(with_warrant: bool)`:
      `jev_run`'s input is `{graph: "wiki-hop", input: {start, goal}, warrant: <the pinned block>}`
      when `with_warrant`, and `browser_open`'s input is
      `{url: "https://en.wikipedia.org/wiki/Cat", confine: ["https://en.wikipedia.org"]}`
      in both cases (the graph carries `confine` regardless). The existing
      test calls it with `false` and its three assertions stay at 31.
    - `gate_questions_under(layer, session, calls)`: the first call at top
      level; when it returned `Ask`, append `Approved` for its id; every later
      call under `nested(root_id, …)`. Counts `Ask`s.
    - `the_wiki_hop_gate_count_drops_from_31_to_1_under_a_warrant` — the
      layer over `ScriptPolicy::builtin` with `browser` and `jev` trusted:
      **exactly 1**, and it is the root; the 30 clicks carry `warrant: Some`.
    - `the_count_stays_31_when_the_open_does_not_confine` — `confine`
      removed from the open: 31.
    - `the_count_stays_31_without_trust` — no `trusted_extensions`: 31, and
      the root's prompt names the untrusted extension.
    - `the_count_is_0_under_a_standing_warrant` — `Standing` for
      `wiki-hop@1`; the root is `Allow`/`Warranted` (append the record as the
      dispatcher would); 0.
    - `the_count_stays_31_with_only_the_old_baseline_test_shape` — the
      original test, unchanged, still at 31 with `with_warrant: false`: the
      table did not move.

### Step 3 — `crates/cli`, `crates/tui`, `crates/web`, docs

**`eidolon/crates/cli/src/config.rs`**

1. `Config` (line 52) gains `#[serde(default)] pub warrants: Vec<eidolon_rune::warrant::Standing>`
   after `policy` (line 131), with the doc: `[[warrants]]` — graphs this
   machine may start unattended, by `id@version`, optionally pinned to a
   document hash; read by the warrant layer to answer `jev_run`'s question
   where nobody is at the keyboard. Per `docs/design/unattended.md` §4. Not
   under `[policy]` for the reason trust is not: it is a decision about a
   specific graph, not about the gate.
2. `policy_settings()` (line 818): `settings.enabled_extensions = self.extensions.iter().filter(|e| e.enabled).map(|e| e.name.clone()).collect();`
   and `settings.standing_warrants = self.warrants.clone();`.
   `Config` derives `Debug, Deserialize` only; the field needs
   `#[serde(default)]` and nothing else.
   Tests in the D8 block (line 1267), beside
   `policy_settings_carries_exactly_the_trusted_enabled_extensions` (line 1341):
   `policy_settings_carries_every_enabled_extension_trusted_or_not`;
   `standing_warrants_parse_with_and_without_a_pinned_hash`;
   `a_warrant_entry_without_a_graph_fails_to_parse`;
   `a_disabled_gate_carries_no_warrants_either` (extends
   `a_disabled_gate_carries_no_trust_either_because_it_carries_nothing`, line
   1357).

**`eidolon/crates/cli/src/main.rs`**

3. The stack (lines 1795–1875): after the `(policy, gated)` match (line
   1800) and **before** the yolo block (line 1828), when `gated`:
   ```rust
   let warrant = eidolon_rune::warrant::Warrant::new(
       policy,
       session.clone(),
       cfg.policy_settings().unwrap_or_default(),
   );
   let warrants = Some(warrant.handle());
   let policy: Arc<dyn PolicyHook> = Arc::new(warrant);
   ```
   with the comment: innermost — directly over the table, under yolo, under
   the judge — so a warranted call is recorded as `Warranted` in a yolo
   session and the judge is never paid for a call the operator's own word
   covers (`docs/design/unattended.md` §4). The settings are read from
   `cfg` again rather than kept from line 1606, where the `Option` was
   moved into the compile thread; `cfg` is still live here (`cfg.policy_judge()`
   reads it a few lines down) and `policy_settings()` is a pure read.
   `session` must already be behind its `Arc<Mutex<_>>`: today the
   `log_path` read and the `Arc::new(Mutex::new(session))` are the two
   statements after `let bus = EventBus::default();` (line 1846) — move
   both to just above the `let (policy, gated)` match; nothing between
   reads `session`. When not gated, `warrants = None`.
4. The runtime struct at line 1328 gains
   `warrants: Option<eidolon_rune::warrant::Warrants>` beside `yolo`, filled
   from the above, and passed to the TUI options beside `yolo` at line 536.
5. The chat loop's command match (line 3049): a `"warrant"` arm mirroring
   `"yolo"`'s: `(None, _)` → `[there is no gate here]`; `(Some(w), "")` →
   print each `list()` row as `  {id}  {graph}  {actions_used}/{actions_max}  {age_s}s  {confirmed|pending}`
   or `[no warrants in this session]`; `(Some(w), "off")` → `w.revoke_all()`,
   `[warrants revoked: every action asks again]`; other → `[warrant takes off, or nothing]`.
   The `help` filter at line 2741 becomes `|| c.name == "yolo" || c.name == "warrant"`.
6. `eidolon policy` (line 1155): no change — it explains a command, not a
   run.

**`eidolon/crates/cli/src/term.rs`** (the `what` match at line 199; `by`
is already bound from `note` just above it):
`Warranted => format!("{asked} ({reason}) — warranted{by}")`, and
`Approved`/`Declined` gain `{by}` so the request line shows:
`Approved => format!("{asked} ({reason}) — approved by you{by}")`, likewise
`Declined`.

**`eidolon/crates/tui/src/command.rs`** (line 597): a row after `yolo`:
`ui_text("warrant", "eidolon", "[off]", Fill::Words(&["off"]), "list the warrants approved this session; `off` revokes them all")`.
**`eidolon/crates/tui/src/app.rs`** (line 4224) and **`state.rs`** (lines
1006, 1203, 1794): `warrants: Option<Warrants>` beside `yolo`, filled from
the options; a `"warrant"` arm beside `"yolo"`'s with the same three
behaviours through `state.info`/`state.alert`; the status JSON at 1794 gains
`"warrants": <count of listed>`. **`usage.rs`** (255, 1640): a `warranted`
counter beside `yolo`. **`crates/web/src/wire.rs`** (144) and
**`shim/render.rs`** (163–167): `Warranted` mapped like `Yolo` (a wire
outcome of its own; no rendered line). The compiler finds every exhaustive
match; these are the ones read for this document.

**`eidolon/docs/extensions.md`** — after "Reaching another tool:
`eidolon::dispatch`", a section **Running unattended: warrants** — what a
warrant is (four sentences from §The model in one page), that a tool which
drives nested dispatches may carry `input.warrant` in the seven-key shape,
that the extension shipping the driver and every extension whose tools the
block names must be `approval = "trust"`, that `[[warrants]]` names graphs
that start unattended, and the confinement contract the gate layer relies on:
*a `browser_open` under a warrant carries `confine` equal to the warrant's
`origins`, and the browser extension enforces it in the browser*. Point at
`docs/design/unattended.md`. **`crates/cli/system/RUNE.md`** — one sentence
after the `dispatch` bullet (line 87): a nested dispatch under a call the
operator warranted may be allowed by that warrant; the journal records it as
`Warranted` with the warrant's id; a script cannot request or widen one.

### Step 4 — `extensions/browser`

**`extensions/browser/service.py`**

S31 is live in this directory, so everything here is anchored by name and
not by line.

1. Module constant `ORIGIN_RE = re.compile(r"^https?://[a-z0-9.\-]+(:\d+)?$")`.
   `file://` has no host and is not a confinable origin; the lint and the
   service both refuse it. The service lowercases a listed origin's host
   before matching, and the lint's pattern admits only lowercase, so a
   graph cannot list one the service reads differently.
2. `Browser` (the class behind `STATE`) gains
   `confined: tuple[str, ...] | None = None` and
   `last_blocked: tuple[str, bool] | None = None`.
3. `def _origin_of(url: str) -> str` — `urllib.parse.urlsplit`, `scheme://host[:port]` with the host lowercased and a default port dropped.
4. `async def _confine(origins: list[str]) -> None`:
   - validate: a non-empty list of strings each matching `ORIGIN_RE` after
     lowercasing, else `ValueError("confine must be a list of origins like https://en.wikipedia.org; got …")`;
   - close `STATE.context` if any (`await STATE.context.close()`);
   - `STATE.context = await STATE.browser.new_context(accept_downloads=False)`;
   - `allowed = re.compile("^(?:" + "|".join(re.escape(o) for o in origins) + ")(?:[/?#]|$)")`
     and `blocked = re.compile(r"^(?!(?:" + "|".join(re.escape(o) for o in origins) + r")(?:[/?#]|$)).*")`;
     `await STATE.context.route(blocked, _abort_route)` where `_abort_route`
     records `(request.url, request.is_navigation_request())` on
     `STATE.last_blocked` and calls `await route.abort("blockedbyclient")`;
   - `STATE.context.on("page", _close_extra_page)` — an `async` handler that
     closes any page that is not `STATE.page`;
   - `STATE.page = await STATE.context.new_page()`; `STATE.confined = tuple(origins)`;
     `_invalidate_refs()`.
   With the docstring: confinement is a fresh context — no cookies, no
   storage, no downloads — with a route that aborts every request whose
   origin is not listed, navigations and subresources alike, so a page under
   a warrant can neither leave the listed origins nor reach one from inside
   itself (`docs/design/unattended.md` §3, and D1r's loopback finding). The
   pattern is a negative lookahead so a listed-origin request never enters
   Python (measurement W1). An open without `confine` replaces the context
   again, unconfined.
5. `async def _unconfine() -> None` — when `STATE.confined` is not `None`:
   close the context, new plain context and page, `STATE.confined = None`,
   `_invalidate_refs()`.
6. `_open`: after validating `url`, `confine = args.get("confine")`;
   `if confine is not None: await _confine(confine)` else `await _unconfine()`;
   then `page = await _ensure_page()` as today. If `STATE.confined` and
   `_origin_of(url)` is not listed, raise
   `ValueError(f"open {url}: origin {origin} is not in confine {list}")`
   **before** navigating. The answer gains `"confined": list(STATE.confined) if STATE.confined else None`.
   `_ensure_page` is unchanged: it creates the plain context only when none
   exists.
7. `_click` and `_type`: after `_settle(page)`, if
   `STATE.confined` and `STATE.last_blocked` recorded a navigation since the
   click began (`STATE.last_blocked = None` set before the click), raise
   `ValueError(f"click {ref}: navigation to {url} was blocked -- origin {origin} is not in confine {list}")`.
   The answer gains `"confined"` as `_open`'s does.
8. `METHODS` unchanged; `_head` unchanged.

**`extensions/browser/tools/open.rn`** — `confine` added to
`input_schema.properties` ("Confine the browser to these origins —
`https://en.wikipedia.org`, `http://127.0.0.1:3000` — until the next
`browser_open`: a fresh context with no cookies, no downloads, and every
request to any other origin aborted. A graph running under a warrant passes
its warrant's origins here; see docs/design/unattended.md.") and forwarded
only when present, by the same object-mutation idiom `snapshot.rn` uses.
The manifest description gains one sentence saying an open with `confine`
discards the shared browser's cookies.

Tests (`test_server.py`, a new class `ConfinementTests(unittest.IsolatedAsyncioTestCase)`
beside `RealBrowserWalkTests`; two `http.server` instances on
two ephemeral loopback ports started in `asyncSetUp` on threads, serving:
`/a.html` on port A with `<main><h1>A</h1><a href="/b.html">same</a><a href="http://127.0.0.1:{B}/x.html">other</a><a href="/d.bin" download>dl</a><script>fetch("http://127.0.0.1:{B}/probe")</script></main>`;
`/b.html` on A; `/x.html` and `/probe` on B, whose handler records hits):
- `a_confined_click_to_an_unlisted_origin_is_refused_naming_it` — open A
  with `confine: ["http://127.0.0.1:{A}"]`, snapshot, click `other`;
  `ValueError` names B's origin and the confine list; `page.url` is still
  A's; B recorded no hit.
- `a_confined_click_within_the_listed_origin_navigates` — click `same`;
  the answer's `url` ends `/b.html`, `confined` lists A.
- `an_in_page_fetch_to_an_unlisted_origin_is_aborted` — after the open,
  B's `/probe` recorded no hit (the D1r shape, closed while confined).
- `a_confining_open_starts_with_no_cookies` — set a cookie on the plain
  context first (`STATE.context.add_cookies`), open with `confine`, read
  `context.cookies()` → empty.
- `a_download_link_under_confinement_saves_nothing` — click `dl`; no
  `download` event completes (assert via `page.expect_download` timing out,
  or the context's `accept_downloads` being `False` on the new context
  object).
- `an_open_without_confine_after_a_confined_one_unconfines` — open B with
  no `confine`; it loads; `confined` is `None`.
- `an_open_to_an_unlisted_origin_is_refused_before_navigating` — open B
  with `confine: [A]`; `ValueError` naming B; no request reached B.
- `SnapshotArgsTests`-style mocked cases: `confine` reaches `_confine` when
  given and `_unconfine` when not; a malformed `confine` (`"main"`, `[]`,
  `["ftp://x"]`, `["https://x/path"]`) is a `ValueError` naming the shape.
- W1, recorded in the Build-Log entry: warm `_open` of the Cat article with
  and without `confine`, wall seconds, on this box.

### Step 5 — `jev/`, `extensions/jev/`, `docs/design/automation.md`

**`jev/automation/warrant.py`** — new:

- `def declared(graph: Graph) -> dict | None` — `graph.jev.get("warrant")`.
- `def canonical(graph: Graph) -> dict | None` — `None` when undeclared;
  else the seven keys: `graph` = `f"{id}@{version}"` or `id`, `sha256` =
  `graph.sha256`, `tools`/`origins`/`commands` sorted and deduplicated
  (`origins` lowercased host — the lint already required it),
  `actions` = `graph.budget("actions", 100)`, `wall_s` = `graph.budget("wall_s", 600)`.
- `def canonical_json(block: dict) -> str` —
  `json.dumps(block, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
- `def warrant_id(block: dict) -> str` — `"w_" + hashlib.sha256(canonical_json(block).encode("utf-8")).hexdigest()[:12]`.
- `def check(passed, graph: Graph) -> tuple[dict, str]` — `passed` must be a
  dict; `canonical(graph)` must not be `None` (else
  `ValueError(f"graph {ref} declares no warrant; run it attended, without a warrant argument")`);
  `canonical_json(_normalize(passed)) == canonical_json(declared)` where
  `_normalize` sorts/dedups the three lists — else
  `ValueError(f"warrant mismatch for {ref}: pass exactly {canonical_json(declared)}")`;
  returns `(block, id)`.
  Tests (`jev/tests/test_warrant.py`, new):
  `the_shipped_wiki_hop_block_is_the_pinned_literal` (the full seven-key
  block for `extensions/jev/graphs/wiki-hop.json` as a literal, including
  the `sha256` the executor computes once and pins — this and step 2's
  Rust test carry the same two literals);
  `the_shipped_wiki_hop_id_is_the_pinned_literal`;
  `a_block_with_reordered_lists_is_equal_and_has_the_same_id`;
  `a_mismatched_block_is_refused_with_the_canonical_block_in_the_message`;
  `an_undeclared_graph_refuses_any_block_naming_attended`;
  `triage_linux_declares_its_nine_commands` (the block's `commands` is the
  nine literal strings from the graph, sorted).

**`jev/automation/graph.py`**

1. `Graph` (line 88) gains `sha256: str`; `load_graph` (line 131) computes
   it from `_load_doc`'s result with the same canonical JSON as
   `warrant.canonical_json` (import it, or inline the same `json.dumps`
   arguments; one definition, so put `canonical_json` in `warrant.py` and
   import it here) and `_build_graph` (line 664) sets it. Docstring: over the
   parsed document, never file bytes, so CRLF and LF checkouts hash alike.
2. `_JEV_KEYS` (line 180) gains `"warrant"`; `_validate_jev` (line 253)
   validates it: an object with `tools` (required, non-empty array of strings
   matching `^[a-z][a-z0-9_]*$`), `origins` (optional array of strings, each
   matching `^https?://[a-z0-9.\-]+(:\d+)?$` — printable ASCII by
   construction), `commands` (optional array of printable-ASCII strings, no
   control characters), no other keys.
3. `_REFS_KEYS` (line 177) gains `"url"`; `_validate_choose_from` (line 393)
   checks it is a string that `re.compile`s, else
   `f"{where}.refs.url: not a valid regular expression: {e}"`.
4. `_validate_action` (line 614): the `tool` branch's `input` is free-form
   today; no change for `confine` (it is input).
5. `_lint` (line 685) gains `_lint_warrant(graph, errors)`, skipped when
   undeclared:
   - the set of `params["name"]` over every `tool` action in every state's
     `entry`/`exit` and every transition's `actions` (root `on` included)
     must equal `set(warrant["tools"])`; error names the missing and the
     extra: `meta.jev.warrant.tools: the graph dispatches {sorted(missing)} which the warrant does not name`
     / `… names {sorted(extra)} which no action dispatches`;
   - for every `tool` action named `browser_open`: `input.confine` must be
     present and set-equal to `origins`
     (`{where}.input.confine: must equal meta.jev.warrant.origins`); the
     `url` must be a string whose text before the third `/` contains no
     `{{` (`{where}.input.url: the origin must be literal under a warrant`)
     and whose origin (`urlsplit`, lowercased host) is in `origins`
     (`{where}.input.url: origin {o} is not in meta.jev.warrant.origins`);
   - if `tools` contains `browser_click` or `browser_type` and `origins` is
     empty: `meta.jev.warrant: a warrant covering browser_click or browser_type must name origins`;
   - every string in `commands` must occur as a literal: a `command` value
     on some `menu`/`also` item, or a literal (no `{{`) `input.command` of
     a `bash` tool action; else `meta.jev.warrant.commands: {cmd!r} is not a command this graph can dispatch`;
   - `budget.wall_s` and `budget.actions` ≥ 1 already by schema.
   Tests (`test_schema.py`, `NegativeLintTests` at line 101):
   `a_warrant_whose_tools_do_not_match_the_actions_is_rejected_naming_both_sets`;
   `a_browser_open_without_confine_under_a_warrant_is_rejected`;
   `a_confine_that_differs_from_origins_is_rejected`;
   `a_templated_origin_is_rejected`;
   `a_start_url_outside_origins_is_rejected`;
   `a_click_warrant_without_origins_is_rejected`;
   `a_warrant_command_the_graph_cannot_dispatch_is_rejected`;
   `a_refs_url_that_is_not_a_regex_is_rejected`;
   `a_non_ascii_command_is_rejected`; and `WorkedExampleTests` (line 45)
   stays green against both amended graphs.

**`jev/automation/schema.json`** — `$defs.jev.properties` gains
`"warrant": { "type": "object", "required": ["tools"], "additionalProperties": false, "properties": { "tools": { "type": "array", "minItems": 1, "items": { "type": "string", "pattern": "^[a-z][a-z0-9_]*$" } }, "origins": { "type": "array", "items": { "type": "string", "pattern": "^https?://[a-z0-9.\\-]+(:[0-9]+)?$" }, "default": [] }, "commands": { "type": "array", "items": { "type": "string" }, "default": [] } } }`
after `budget`; the `refs` block (line 135) gains
`"url": { "type": "string", "description": "A regular expression the option's snapshot url must match; an option with no url is dropped." }`
after `named`. The copy in `automation.md` §1 is updated to match; no test
compares the two, and the executor says so in the Build-Log entry.

**`jev/automation/options.py`** — `_from_refs` (line 104): read
`url_re = refs_cfg.get("url")`, compile once; inside the loop, after the
`ref` check: `if url_re is not None and not (record.get("url") and re.search(url_re, record["url"])): continue`.
Tests (`test_options.py`):
`a_url_pattern_drops_options_whose_href_does_not_match`;
`a_url_pattern_drops_options_with_no_href`; and in
`WikiHopRealCaptureTests` (`test_run.py` line 331):
`the_scoped_capture_yields_only_article_links_under_the_url_bound` — every
option's `url` starts with `/wiki/` and contains no `:`; the counts before
and after the filter are asserted as the numbers observed (W2), stated in the
test's docstring and in the Build-Log.

**`jev/automation/run.py`**

1. `_RunState.__init__` (line 219) gains `warrant: dict | None = None` and
   `warrant_id: str | None = None` as parameters after `graph_ref`, stored.
2. `start` (line 1524) gains `warrant=None` (keyword). After `_check_input`:
   `block = block_id = None; if warrant is not None: block, block_id = warrant_mod.check(warrant, graph)`;
   passed into `Run(…)`. Docstring: a block equal to the graph's declared
   warrant starts a warranted run; none starts an attended one; a mismatch
   refuses before the first action with the correct block in the message
   (`docs/design/unattended.md` §2).
3. `Run.__init__` and `_restore_run_state` thread `warrant`/`warrant_id`.
4. `_RESUME_HINT` (line 156) becomes `def _resume_hint(rt)`: the existing
   string, with `, warrant: <this payload's warrant>` inside the `pick` form
   when `rt.warrant` is not `None`. `_empty_escalation` (line 692) and
   `_floor_escalation` (line 711) gain `"warrant": _warrant_field(rt)` where
   `_warrant_field` returns `{"id": rt.warrant_id, **rt.warrant}` or `None`,
   and use `_resume_hint(rt)`.
5. `_final_report` (line 1303) and `_status_dict` (line 1841) gain
   `"warrant": _warrant_field(rt)`.
6. `_persist_park` (line 796): `"warrant": rt.warrant`; `_persist_resolved`
   (line 843): `"warrant": rt.warrant_id`; `_restore_run_state` (line 1556)
   restores `warrant` and recomputes `warrant_id` with `warrant_mod.warrant_id`.
7. `Run.answer` (line 1405) and the module `answer` (line 1774) gain
   `warrant=None`: when given, `warrant_mod.canonical_json(normalized) == canonical_json(rt.warrant)`
   else `ValueError("warrant mismatch: this run was started under …" / "this run was started attended and cannot be resumed under a warrant")`,
   raised before the generator is touched (the same rule as an out-of-menu
   pick).
8. `_log_pick` (line 996): `warrant=rt.warrant_id` into `decisions.log_decision`.
9. `Run._settle` (line 1450), in the `yielded is None` branch, after
   `_final_report(self.state)`:
   `decisions.log_run_end(graph=_graph_ref(rt.graph), run=rt.id, outcome=rt.outcome, warrant=rt.warrant_id)`
   with `rt = self.state`, inside a `try/except OSError` that appends to
   `rt.warnings`, the same best-effort shape as `_log_pick` — which already
   passes `_graph_ref(rt.graph)` as `graph`, and `_append` strips the
   `@version` for the file name. This is the missing caller noted in §5.
   Tests (`test_run.py`):
   - `WarrantedRunTests(_IsolatedDecisionsDir)`, new:
     `a_run_started_with_the_graphs_own_block_is_warranted_end_to_end` —
     the wiki-hop two-hop walk of `WikiHopEndToEndTests` (line 239) with
     `warrant=<the pinned block>`; the report's `warrant.id` is the pinned
     id; every decision row carries it; the run-end row carries it;
     `driver.calls[0]` (the open) carries `confine: ["https://en.wikipedia.org"]`;
     `a_run_started_without_a_block_is_attended` — report `warrant: None`,
     rows `warrant: None`;
     `a_mismatched_block_is_refused_before_the_first_action` — `start`
     raises `ValueError` with the canonical block in the message and the
     driver saw no call;
     `an_escalation_under_a_warrant_carries_the_block_and_the_hint_names_it`
     — a parking chooser; the payload's `warrant.id` and `answer` text;
     `a_resume_with_the_wrong_block_is_refused_without_disturbing_the_park`;
     `a_resume_of_an_attended_run_with_a_block_is_refused`;
     `a_park_snapshot_carries_the_warrant_and_a_restart_keeps_it` — in
     `PersistenceTests` (line 1656), using its `_simulate_restart` (line
     1776); afterwards `status` shows the same `warrant.id`.
   - `TriageLinuxEndToEndTests` (line 420):
     `a_triage_run_under_its_warrant_dispatches_only_listed_commands` —
     every `bash` request's `command` is in the block's `commands`.
   - `RunEndTests` in `test_decisions.py` (line 146):
     `the_run_end_row_carries_the_warrant`; `RowShapeTests` (line 53):
     `a_row_carries_warrant_null_when_attended` — the row's key set is the
     section-5 set plus `warrant`.

**`jev/automation/decisions.py`** — `log_decision` (line 78) and
`log_run_end` (line 156) gain `warrant: str | None = None`, written as
`"warrant"`. The docstring's row list gains it.

**`jev/server.py`** — `_automation_start` (line 488) passes
`warrant=args.get("warrant")`; `_automation_answer` (line 527) passes
`warrant=args.get("warrant")`; new `_automation_warrant(args)`:
`graph = args.get("graph")` (required), `load_graph_ref` as `start` does,
`block = warrant_mod.canonical(graph)`; `None` →
`ValueError(f"graph {ref} declares no warrant (meta.jev.warrant); it runs attended")`;
else `{"warrant": block, "id": warrant_mod.warrant_id(block)}`. Registered
as `"automation.warrant"` in `METHODS` (line 564).
Tests (`test_server_automation.py`, `AutomationHttpTests` at line 110):
`automation_warrant_answers_the_shipped_wiki_hop_block_over_http`;
`automation_warrant_on_an_undeclared_graph_is_a_clean_ok_false`;
`start_with_a_mismatched_warrant_is_a_clean_ok_false_with_the_block_in_the_message`;
`a_warranted_start_reports_the_warrant_in_status`.

**`extensions/jev/tools/warrant.rn`** — new, registers as `jev_warrant`,
`approval: "read_only"`, input `{graph}` (required), description: "The
warrant block a graph declares — pass it verbatim as `warrant` to `jev_run`
to run the graph unattended under it, and to `jev_resume` when answering its
escalations. Read it before every `jev_run`: the block carries the graph's
content hash and is refused if the graph has changed. A graph with no
`meta.jev.warrant` has no block and runs attended, asking about every action
its policy flags." Body: `eidolon::service_call("automation.warrant", #{ "graph": input.graph }).await`.
**`run.rn`** (line 38): `warrant` forwarded into the `automation.start` args
when present, by the `insert` idiom `resume.rn` already uses; the manifest
description gains: "Pass `warrant` — the block `jev_warrant` returns for this
graph — to run it unattended: the gate asks once, about the run and its
envelope, instead of about every action. Without it every flagged action
asks." **`resume.rn`** (line 44): `warrant` forwarded into
`automation.answer` when present; description gains: "Pass the `warrant`
from the escalation you are answering." **`extension.rn`** (line 60):
`"warrant"` added to `tools`. Verified by `eidolon ext list` compiling the
three tools; the wire shapes are covered by the HTTP tests above.

**`extensions/jev/graphs/wiki-hop.json`** — three edits. `meta.jev` gains
`"warrant": { "tools": ["browser_open", "browser_snapshot", "browser_click", "browser_back"], "origins": ["https://en.wikipedia.org"] }`
after `budget`; `open`'s `browser_open` input gains
`"confine": ["https://en.wikipedia.org"]`; `reading.meta.choose.from.refs`
gains `"url": "^/wiki/[^:#?]+$"`. Nothing else changes.

**`extensions/jev/graphs/triage-linux.json`** — `meta.jev` gains
`"warrant": { "tools": ["bash"], "commands": [ the nine literal command strings: "cat /etc/os-release", "uname -a", "hostname; uptime", "who", "ss -tulpn 2>/dev/null || netstat -tulpn", "ss -tnp state established", "ip -br addr", "nft list ruleset 2>/dev/null || iptables -S", "ps -eo comm,pid,user,pcpu,etime --sort=-pcpu | head -30" ] }`.

**`docs/design/automation.md`** — five edits, no others: the schema block
(from line 154) and both graphs (from lines 464 and 581) updated to match
the files; §4 "The tiers, by who is driving" gains a paragraph after the
`return` bullet: under a warrant the driving model's `jev_resume` is silent
at the gate, so the model is the second tier for the chooser's uncertainty
and the operator is the authority for the envelope, per
`docs/design/unattended.md`; §6 "The gate on a sandbox" is rewritten: a
sandbox runs the graph under a standing warrant — `[[extensions]] name =
"jev" approval = "trust"` and `[[warrants]] graph = "triage-linux@1"` in the
box's config — and `NoUser` declines whatever the warrant does not cover,
which reaches `recover`; the Melete seam's step 2 drops `--yolo` from the
command line and step 1 writes the two stanzas. The escalation payload
example gains `"warrant": { "id": "w_…", … }` and the report example the
same.

### Step 6 — the live re-run

With the binary rebuilt from steps 1–3 and the services restarted: in a
session with `browser` and `jev` trusted, `jev_warrant {graph: "wiki-hop"}`
then `jev_run {graph: "wiki-hop", input: {start: "Cat", goal: "Felis"}, warrant: <block>}`.
The Build-Log entry records: the number of gate questions the session
journal shows (`PolicyVerdict` records with outcome `Approved`, `Declined`
or `Judged` under the run), which must be 1; the number of `Warranted`
records; the report's `warrant.id`; W1 and W2; and, if the run hit its
confinement, the error's text. Isolation as J1 did it: `JEV_DECISIONS_DIR`,
`JEV_RUNS_DIR` and `JEV_DECISIONS_LOG` exported and proven redirected before
the run; the production log's hash verified before and after.

### Order, and what each step proves on its own

1 first: it is the record shape, and nothing downstream can journal a
warrant before it exists. 2 after 1: the layer and the rail, proved against
the real compiled table with a real session journal — the accounting test is
the acceptance test, and it is a claim about the gate. 3 after 2: the layer
is installed and reachable, revocable from a keyboard, and its outcome is
drawn everywhere `Yolo` is. 4 in parallel: confinement is proved against a
real Chromium and two real loopback origins, and D1r's loopback shape is
shown closed while confined. 5 in parallel: the graph declares, the service
verifies and records, the driver forwards; every test runs against
`FakeDriver` and the pinned id ties it to step 2. 6 last, and it is the only
step that observes a session rather than a component.

## Re-run gate accounting

Under this ruling, over the same 62-call reconstruction D8 measured, against
the real compiled `policy.rn` with the warrant layer over it:

| configuration | questions | of which |
|---|---|---|
| before D1 and D8 | 62 | everything |
| after D1 and D8 (today) | 31 | 1 `jev_run` + 30 `browser_click` |
| attended: no `warrant` passed, or `browser`/`jev` untrusted, or the open did not confine | 31 | unchanged — the table did not move |
| **warranted, interactive** | **1** | `jev_run`, with the envelope in the question; 30 clicks `Warranted` |
| warranted, interactive, the run parks *n* times and the model resumes it | 1 | each `jev_resume` `Warranted` under the same envelope |
| warranted, headless, `[[warrants]]` names the graph | **0** | `jev_run` `Warranted` by standing config |
| warranted, but `:warrant off` mid-run | 1 + the clicks after it | each of which `NoUser` declines unattended |

The number is 1, not 0, and 1 is the right number. The one question is the
one a person can answer with the information that decides it — the graph,
its hash, the tools, the origins, the count, the clock — and it is asked at
the one moment the whole envelope can be put in front of them. Every one of
the thirty questions it replaces was the same question with that information
removed. Zero interactively would mean a run authorized by nobody, which is
`--yolo`; zero headless means a run authorized by the operator's own config
line, which is what a config line is for.

What the number does not count, and should not: the parks. A run at the
bootstrap floor of 0.35 will park often, and each park reaches the driving
model or a person with the whole menu. Those are the graph's questions, asked
only when the machine is unsure, and this document leaves them exactly where
`automation.md` §4 put them. The gate's question is about what a run may do;
the park's question is about which way to go. Conflating them is what 62 was.

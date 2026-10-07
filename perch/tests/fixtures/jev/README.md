# tests/fixtures/jev: real wiki-hop journals

Twelve cases of jev's `wiki-hop` graph, written down as the real `eidolon workflow` wrote them: its `log --json` journal, and for each
invocation the exit code, the last stdout line (the door's one JSON line) and stderr. No model and no network took part: the provider is
`mock`, the browser is the fake five-page `browser` plugin of eidolon-plugins' `tests/jev/`, and the jev service is that folder's
`stub_service.py`. Every fixture has `grep -c ModelSpend` of 0.

The folder is read by the runs slice (R1a: `fake_eidolon workflow` replays a case) and by the page (W1 `core/saved.js`, W2a `graph/jevrun.js`
and the tests under `webui/term/tests/`). `webui/term/tests/fixtures/jev/` is a byte-identical copy so the page's tests do not
reach into `perch/`; `capture.sh` writes both and `--check` fails if they differ.

```text
capture.sh                  capture every case into both folders (WSL; it needs the eidolon binary and the eidolon-plugins checkout)
capture.sh --check          re-run every case into a throwaway tree and compare: record kinds and their order (a WorkflowStep by its
                            tool), exit codes, the door's last line without its session path. Ids and timestamps are not compared.
capture.sh --verify         look only at the files kept here: no eidolon, no stub
capture.sh e                only that case (letters after the options)
```

`<case>/case.json` says how the case was made and what each invocation did; `<case>/journal.ndjson` is the journal after the last invocation.

| case | what it is | stub | invocations: exit, what the door said | records |
|---|---|---|---|---|
| a | Ancient_Egypt to Felidae: one chooser pick (Egypt), one rule pick (Felidae); the whole happy path | first | run: 0 completed, `final_state` arrived | 51 |
| b | the same trip, but the chooser's two options are equal: a park on the margin, answered `0` | flat | run: 1 parked; resume `0`: 0 completed, arrived | 30, 57 |
| c | the chooser is unavailable (the stub answers an error to both tools): the park says so, and resume is refused | none | run: 1 parked; resume `0`: 3 refused, naming the `jev_entail` step | 30, 30 |
| d | SIGTERM while `jev_choose` is in flight, the end where eidolon's cancel token wins: the run is `failed`, "the run was cancelled", with no result for the `jev_choose` step | first, held 8 s | run: 2 failed | 25 |
| e | the graph's own `failed` final (the graph from before P1, no `max`, against the long article): the door still says completed, exit 0 | first | run: 0 completed, `final_state` failed, reason truncated | 21 |
| f | Felidae to Egypt: two single-option picks and a rule pick, no chooser call at all | first | run: 0 completed, arrived | 60 |
| g | `jev_entail` errors while `jev_choose` works: the run goes on, parks on a margin, and the park cannot be resumed | flat, entail broken | run: 1 parked; resume `0`: 3 refused, naming the `jev_entail` step | 30, 30 |
| h | a park answered with `banana`: it re-parks (a second escalation), then `0` | flat | run: 1 parked; resume `banana`: 1 parked again; resume `0`: 0 completed, arrived | 30, 36, 63 |
| i | the long article with a goal it does not link to: 64 options, a park under the floor (0.015 < 0.350), answered by the label of an option; the page it clicks to does not exist in the fake site, so the graph ends `failed` | flat | run: 1 parked; resume `Registered cat breed number 002`: 0 completed, `final_state` failed | 30, 44 |
| j | `jev_choose` errors while `jev_entail` works: a "chooser unavailable" park, and the refusal names the `jev_choose` step | flat, choose broken | run: 1 parked; resume `0`: 3 refused, naming the `jev_choose` step | 30, 30 |
| k | SIGKILL while `jev_choose` is in flight: no WorkflowEnd, no stdout line, exit 137; then `resume` with no answer continues it | first, held 3 s | run: 137; resume: 0 completed, arrived | 24, 53 |
| l | SIGTERM again, the other end of the same race: the program wins, `jev_choose` comes back "cancelled while the tool was running", the graph reads that as an unavailable chooser and parks | first, held 8 s | run: 1 parked | 30 |

The fake site: Cat links to Felidae and Ancient Egypt; Felidae to Cat; Egypt to Cat, Felidae and Ancient Egypt; Ancient_Egypt to Egypt and Cat;
List_of_cat_breeds has 130 links, 13.5 KB of snapshot, and "Ancient Egypt" is its 31st. A page that links to the goal is a rule pick (the label
equals the goal), a page with one unvisited link is a single pick, any other page asks the chooser (the stub). Stub modes: `first` puts 0.9 on the
first option; `flat` makes them equal; `none` answers an error to both tools. Cases d, g, j, k and l go through a small wrapper
(`stubwrap.py`, written out by `capture.sh`) that adds `STUB_BREAK=entail|choose` and `STUB_DELAY_CHOOSE=<s>`; the stub has no mode for either.

## The tool sequence of each case

`WorkflowStep.tool` in order (`jq -r 'select(.kind.WorkflowStep) | .kind.WorkflowStep.tool' <case>/journal.ndjson`; jq is not installed in the WSL
here, `capture.sh` prints the same through python).

```text
a  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose browser_click jev_mark browser_snapshot jev_entail browser_click jev_mark browser_snapshot jev_mark
b  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose jev_mark browser_click jev_mark browser_snapshot jev_entail browser_click jev_mark browser_snapshot jev_mark
c  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose jev_mark
d  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose
e  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_mark
f  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail browser_click jev_mark browser_snapshot jev_entail browser_click jev_mark browser_snapshot jev_entail browser_click jev_mark browser_snapshot jev_mark
g  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose jev_mark
h  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose jev_mark jev_mark browser_click jev_mark browser_snapshot jev_entail browser_click jev_mark browser_snapshot jev_mark
i  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose jev_mark browser_click jev_mark browser_snapshot jev_mark
j  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose jev_mark
k  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose browser_click jev_mark browser_snapshot jev_entail browser_click jev_mark browser_snapshot jev_mark
l  jev_graph jev_mark browser_open jev_mark browser_snapshot jev_entail jev_choose jev_mark
```

Read the `jev_mark`s as the graph's own trail: `kind` enter on each reading state, `park` before a park, `final` on a final state (`failed` is a
final state too, as e shows: `marked wiki-hop final failed`, then WorkflowEnd Completed). `jev_entail` is the arrival check; it is skipped when the
heading equals the goal.

## Record shapes (what the later slices read)

Observed, not designed. Every claim is in at least one case here.

- **A journal line** is `{"id", "parent", "ts_ms", "kind": {"<Kind>": {...}}}`: ids dense from 0, `parent` the id before, the kind a one-key
  object. The kinds in these cases are `SessionStart`, `WorkflowStart`, `WorkflowStep`, `PolicyVerdict`, `ToolResult`, `ApiRequest`, `AskUser`,
  `WorkflowEnd`; a reader that ignores unknown kinds survives eidolon adding more.
- **Double JSON.** `WorkflowStep.input` and `WorkflowStart.args` are JSON *strings* (compact, keys sorted: `{"actions":0,"graph":"wiki-hop",...}`),
  and the door's `report` is a JSON string inside the door's JSON line. Parse them once more. `jev_choose`'s input has `context` (the snapshot
  body after its header, behind `Target article:` and `Current article:` lines) and `options` (the labels); its result is `{"probs":{label:p},"best":label}`
  as a string, and `jev_entail`'s is `{"results":[{"label","hypothesis","scores":{"neutral","entailment","contradiction"}}]}`.
- **`ToolResult.tool_use_id` equals `WorkflowStep.call_id`**, which is `w<run>-<index>`; the run is `1` here, equal to the id of the WorkflowStart
  record, and the index is dense from 0 across parks and resumes. A park has its own id, `w1-park<n>`, and takes no index.
- **A step is** WorkflowStep, PolicyVerdict (`tool_use_id` is the call id, `outcome` Approved), for the two jev service tools an ApiRequest
  (`{"consumer":"plugin:jev","service":"jev"}`), then ToolResult. A step cut off by SIGKILL has no ToolResult, and one that is resumed gets a second
  PolicyVerdict and ApiRequest and then its ToolResult (case k, records 22 to 26).
- **The graph rides on the first steps.** `w1-0` is `jev_graph {"from":1,"graph":"wiki-hop"}` and its ToolResult is `jev-page next=0` and then the
  graph's exact text; the text after that first line has the sha256 `graphs.p1` (or `graphs.pre` for e) in `meta.json`, which is the PIN in the
  workflow. The wiki-hop graph is one page (4.9 KB); a longer graph would be several, `next=<line>` pointing at the next. `WorkflowStart.hash` is
  the hash of `workflows/wiki-hop.rn` and `lib/*.rn`, not of the graph.
- **Arrival and picks are readable from the steps.** Each reading state is `browser_snapshot` (input `{"max":100,"roles":["link"],"within":"main"}`
  with P1's graph, no `max` in e), then `jev_entail` unless the heading equals the goal, then `jev_choose` or nothing, then `browser_click {"ref":"eN"}`.
  The ref is the `[ref=eN]` of the option in the snapshot just before (`link "Egypt" [ref=e3]`), and the ToolResult says where it went
  (`clicked e3 -> https://en.wikipedia.org/wiki/Egypt`). No `jev_choose` step before a click means a rule pick or a single pick; telling those two apart
  takes the options, and in f the report's `rule_picks` is 1 for 3 picks.
- **A park** is `jev_mark {"kind":"park",...}`, then `AskUser {"call_id":"w1-park0","prompt":"...","answer":null}`, then `WorkflowEnd {"status":"Parked"}`.
  The answer is recorded by appending a *second* AskUser with the same `call_id` and `"answer":"<text>"`; no new WorkflowStart. An answer that is
  no option is recorded the same way, and then the graph journals another `jev_mark` park, a new `AskUser` (`w1-park1`, whose prompt starts
  `'banana' is not one of the options.` and repeats the menu) and another `Parked` (case h). The prompt is the menu: first line `jev wiki-hop: <why>`,
  with `<why>` one of `top 0.500, margin 0.000 under 0.050` (a margin park), `top 0.015 under floor 0.350` (under the floor), or
  `chooser unavailable: <the tool's error>`; then the graph's `ask`, `state: reading (step N)`, and one line per option, `  <index>  <p>  <label>`.
  Probabilities have 3 decimals and an unavailable chooser prints 0.000 for each. An answer is the index or the exact label (i answers by label).
- **WorkflowEnd.status** is `Completed`, `Failed` or `Parked`. There is no cancelled and no reason: the reason is on the door's stdout only.
  `Completed` covers a graph that reached its `failed` final state (e); the graph's own result is the `report`'s `final_state` and the last
  `jev_mark` (`kind` final, `state` failed).
- **The door's last stdout line** (its keys are alphabetical), and the exit code with it:

  | exit | status | keys | seen in |
  |---|---|---|---|
  | 0 | completed | `report` (a JSON string), `run`, `session`, `status` | a, b, e, f, h, i, k |
  | 1 | parked | `question`, `run`, `session`, `status` | b, c, g, h, i, j, l |
  | 2 | failed | `reason`, `run`, `session`, `status` | d |
  | 3 | refused | `error`, `status` (no run, no session) | c, g, j |
  | 137 | (killed) | nothing on stdout | k |

  The report: `{"actions","chooser_calls","duplicates_collapsed","escalations","exhausted","final_state","graph","outcome","output","path","rule_picks","steps"}`.
  `output` is `{"path": [article titles]}` when it arrived, `{"error","path","reason"}` when it ended in `failed` (`reason` truncated in e). `path` is
  the graph's states. `session` is `<XDG_DATA_HOME>/eidolon/sessions/<ms>.eid`, a temp path of the capture.
- **Resume is refused when the journal holds a failed step.** Exit 3, on stdout, journal untouched (30 records before and after):
  `refusing run #1: step 5 (w1-5 jev_entail) is journaled as a failure — stub: the model is not loaded. The memo replays a journaled result
  verbatim, so resuming cannot change it: start a new run (`eidolon workflow run jev wiki-hop`)`. It names the *first* failed step: c (`none`) and g
  (entail broken) name the `jev_entail` step `w1-5`, only j (choose broken) names `w1-6 jev_choose`. The test is a ToolResult with
  `is_error: true`, so a page can say "cannot be resumed" from the journal alone: c, g, j, and l (`cancelled while the tool was running`; its next step
  is `cancelled before the tool ran`). Only a *parked* run is asked to resume: i holds one error result too (its last snapshot) but ended Completed. A step
  with no ToolResult at all (d, and k up to record 24) is a cut run, not a failed one.
- **A tool that fails does not stop the graph.** `jev_entail` errors in g and c and the run goes on to the chooser; a failing `jev_choose` is a park;
  a failing browser tool is an ERROR event, which wiki-hop turns into its `failed` final state (i, whose last click lands on a page the fake site lacks).
- **SIGTERM is a race.** eidolon prints `[cancelling]` on stderr and cancels one token; the workflow thread races it against the program with
  `tokio::select!`, so in one run the token wins (d: WorkflowEnd Failed, stdout `{"reason":"the run was cancelled",...,"status":"failed"}`, exit 2, the
  in-flight step left without a result) and in another the program wins (l: the in-flight tool's ToolResult is `cancelled while the tool was running`, the
  next `jev_mark` is `cancelled before the tool ran`, then a park). `capture.sh` runs d and l up to 40 times each until it has the end it names.
  The perch's `cancelled` is its own state; no record says it.
- **SIGKILL leaves no WorkflowEnd** (k, first 24 records). `eidolon workflow resume --plugin <dir> <session> <run>` with no `--answer` continues
  such a run; the journal keeps going from record 24. A resume of a parked run takes `--answer=<text>`.
- **Timing** is only in `ts_ms`, the gap from a WorkflowStep to its ToolResult. Over a, b, e, f and h: `jev_entail` and `jev_choose` 2 to 7 ms against the
  stub, `jev_mark` 0 to 7 ms, `jev_graph` 8 to 32 ms, a fake browser tool 19 to 77 ms. These move from one capture to the next, so no test should
  depend on them. The gap between a park's WorkflowEnd and the answer is the time between the two commands.

## What the cases do not cover

- No option label of 32 bytes or more (the longest is 31: `Registered cat breed number 001`), so no two labels share a 32-byte cut: the
  collision flag has no case here.
- No `last` mode of the stub (a best option that is not first), no `stop` answer, no `exhausted` or `stopped` outcome and no `jev_mark` of kind `end`
  (the graph's budgets are 30 steps, 70 actions, 30 escalations; none is reached), no graph of more than one `jev_graph` page.
- SIGTERM inside a browser tool (the fake browser has no delay hook); only inside `jev_choose`.
- Real Wikipedia and a real chooser: those are X1's.

## Provenance

`meta.json` has the machine-readable form.

- eidolon: `eidolon 0.1.0` from `~/.local/bin/eidolon` in WSL (sha256 11583c21...). The binary does not carry its commit; `VENDORED.md` says the
  WSL build is the vendored `eidolon/` at `bab3813`, which is the plan's commit.
- plugins: the P1 branch `jev-wiki-hop-max` of eidolon-plugins, commit `2fa3b85` (jev, and `tests/jev/` with the fake browser, `stub_service.py` and
  `guard.sh`), unpushed; case e takes `jev/graphs/wiki-hop.json` and `jev/workflows/wiki-hop.rn` from `origin/main`, `7a0ff8e`, the graph before P1. Both are
  read with `git archive`; nothing was checked out or written in that repository.
- graph sha256: P1 `5dea1ebe...`, before P1 `2db16e39...`.
- the copy that ran: `tools/choose.rn` and `entail.rn` with the service port 8091 rewritten to 48091 and `token_file` to an absolute path in the
  throwaway HOME, trusted and granted in a temp config with the swarm off. The throwaway tree is `/tmp/minerva-f0-capture` (HOME, all `XDG_*`, the
  token, the config), asserted before every eidolon call, and removed at the end; `SessionStart.cwd` and the `session` paths show it.

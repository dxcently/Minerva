# tests/fixtures/jev: real wiki-hop journals, for the page's tests

A byte-identical copy of `perch/tests/fixtures/jev/` (minus its `capture.sh` and README), so the node tests read their input from this tree and
not from `perch/`. Do not edit a file here by hand: `perch/tests/fixtures/jev/capture.sh` writes both folders, and `capture.sh --check` and
`--verify` fail when they differ. That README has the provenance and every record shape, read it first.

Each case folder holds `case.json` (how it was made, and per invocation the exit code, the door's last stdout line, stderr, and how many journal
records existed after it) and `journal.ndjson` (`eidolon log --json`, one record per line, after the last invocation). In a node test:

```js
const dir = new URL('./fixtures/jev/b/', import.meta.url);
const journal = readFileSync(new URL('journal.ndjson', dir), 'utf8').trim().split('\n').map(JSON.parse);
const meta = JSON.parse(readFileSync(new URL('case.json', dir), 'utf8'));
// journal.slice(0, meta.invocations[0].records) is what the log held when the first invocation had ended
```

The files are LF on every checkout (this folder's `.gitattributes`), so a test need not normalise line ends.

| case | what it holds | meant for (the plan's test lists) |
|---|---|---|
| a | the happy path: a chooser pick, a rule pick, arrived | saved.js (a finished run), jevrun.js (arrived, path, picks) |
| b | a margin park, answered `0`, arrived | saved.js (a park with its answer), jevrun.js (park, `pick.how` answer) |
| c | a chooser-unavailable park (both jev tools erroring), resume refused | jevrun.js (unresumable, naming `w1-5 jev_entail`) |
| d | SIGTERM in `jev_choose`, the cancel token wins: `failed`, "the run was cancelled", a step with no result | jevrun.js (cancelled) |
| e | the graph's `failed` final under a completed run (exit 0), the long article, the graph from before P1 | jevrun.js (failed though the door said completed), saved.js |
| f | single picks and a rule pick, no chooser call | jevrun.js (picks without a menu) |
| g | `jev_entail` erroring, a margin park, resume refused | jevrun.js (an entail error, then an unresumable park) |
| h | an answer that is no option: a re-park and a second escalation, then `0` | jevrun.js (invalid answer, escalations used), saved.js (two parks) |
| i | the long article, 64 options, a park under the floor, answered by label | jevrun.js (floor park, a menu of 64) |
| j | `jev_choose` erroring: a chooser-unavailable park, resume refused naming `w1-6 jev_choose` | jevrun.js (unresumable, naming a `jev_choose` step) |
| k | a run killed in `jev_choose` (24 records, no end) and then resumed | jevrun.js (a run with no WorkflowEnd, a step with two verdicts) |
| l | SIGTERM in `jev_choose`, the program wins: the tool is cancelled and the graph parks | jevrun.js (a park after a cancelled tool) |

# src/runs/ — graph runs (`/hub/runs`)

The runs the page starts for a graph named in `--run-graphs`: a detached `eidolon workflow` child per run, started on the mock provider,
watched through what it prints and through its own journal, and found again by a perch that starts later (D3).

```text
mod.rs     State, pick() (the dispatch), route, and the two hooks: adopt (after the bind), on_shutdown (before the doors)
state.rs   <home>/runs.json and the routes: GET|POST /hub/runs, /<rid>, /<rid>/answer|cancel|abandon      (R1a)
proc.rs    the detached child, its process group, the wall clock, adopt, how a child ended, on_shutdown     (R1a)
health.rs  GET /hub/runs/health and the start gate                                                         (R1a)
log.rs     GET /hub/runs/<rid>/log?after=N                                                                 (R1b)
graphs.rs  GET /hub/runs/graphs, /graphs/<g>                                                               (R1b)
```

Every handler has one signature, `(ctx, req, rest) -> Response<BoxBody>`; `rest` is the path below the handler's own prefix (`""` for
`health`, `""` or `<g>` for `graphs`, the run id for `log`, the whole rest for `state`). `runs::pick` is the whole dispatch.

## Routes

```text
GET  /hub/runs                      [row], newest first, 50 at most
POST /hub/runs {graph, args?}       202 {rid, state: "running"}
       400 body is not {graph: string, args?: object}, or args over 64 KiB     413 a body over the cap
       404 a graph outside --run-graphs, or with no <plugins>/jev/workflows/<g>.rn
       409 {run} a run is running or parked: there is one shared browser
       503 jev or the browser service is down (the sentence says how to start it), or no --home / --plugins-dir
GET  /hub/runs/<rid>                row + question? report? refused? and out, err (the last 4 KiB the child printed)
POST /hub/runs/<rid>/answer {text}  202 {rid, state: "running"}   400 empty, over 1 KiB, a control character   409 not parked   503 a service is down
POST /hub/runs/<rid>/cancel         200 detail (202 if the child is still going after 9 s)   409 not running
POST /hub/runs/<rid>/abandon        200 detail: a parked run becomes abandoned, nothing is started   409 not parked
GET  /hub/runs/health               {jev: S, browser: S, at_ms}   S = {configured, up, start, why?, ...the service's own fields}
```

A refusal's body is `{error, why}` (one sentence under both names) plus `run` or `state` where it names one. Unknown or malformed run
ids, and unknown paths, are the empty 404; a wrong method is the empty 405.

A **row** is `{rid, graph, state, started_ms, ended_ms?, exit?, why?, args?, resumes, stopping?}`. `exit` is the child's exit code when
this perch reaped it (137 is SIGKILL); an adopted run has none.

## States

```text
running    a child is alive (the first `workflow run`, or a `workflow resume`)
parked     the graph asked a person: `question` on the detail; answer or abandon. No process.
done       completed (`report` on the detail: eidolon's JSON string)        failed     the graph or eidolon ended it (`why`)
cancelled  the page asked and the group was stopped                         abandoned  a parked run the page gave up on
refused    eidolon refused to start it (`why`)                              lost       the child is gone and the journal has no end
```

`running` and `parked` hold the one shared browser: a second start is a 409 naming the holder. The check and the insert are one step
under the store lock, so two starts at once make one run. A **refused resume** (eidolon names the step it cannot replay: F0 cases c, g,
j) leaves the run `parked` with eidolon's sentence in `refused`; abandon is how it ends. A resume is `running` from the moment the 202 is
answered, so a second answer is a 409.

## How a child ends (`proc::decide`; the first that applies wins)

```text
the page cancelled it            cancelled (whatever eidolon said on the way out: F0 d says failed, l says parked); done if it had completed
the wall clock stopped it        failed "the run took longer than ..."
the door's last stdout line      completed -> done, parked -> parked, failed -> failed, refused -> refused (parked, if it was a resume)
no line: the journal             `eidolon log --json` through titles::run, only `kind`s read: a WorkflowEnd of Completed / Failed / Parked
                                 with nothing after it is that; anything else is lost
no line and no journal           failed "eidolon exited with ..."
```

Someone else's SIGTERM (not the page's) is not a cancel: eidolon's word stands (F0 d is `failed`, l is `parked`).

## Detached (D3)

The child is its own process group leader (`process_group(0)`), has no `kill_on_drop`, takes its args from `args.json` on stdin and writes
`out.log` / `err.log` in its folder: when the perch stops, the child neither dies nor loses its output, and `on_shutdown` waits only for
a cancel's own grace (2.5 s at most). The pid goes into `runs.json` before the child is trusted. `adopt` reads `runs.json`: a `running`
run whose pid still has `workflow` and its own `config.toml` in `/proc/<pid>/cmdline` is watched until it ends (and keeps its wall clock,
measured from `child_ms`); one that is gone is settled from `out.log` and the journal. A pid is only ever signalled after that cmdline
check, so a reused pid is left alone. While no perch runs, nothing enforces the wall clock.

## On disk

```text
<home>/runs.json            {rid: run}, 0600, written whole through write_atomic_0600; one that does not parse is set aside (.bad-<ts>)
<home>/runs/<rid>/          0700; ids are r<digits> past every id in the file and every folder there, so a lost runs.json never reuses one
    config.toml             the home's config.toml with sessions_dir = this folder, default_model = mock, [swarm] enabled = false; [policy]
                            as the member has it (a run shares the permits store the doors use)
    args.json               the args, compact JSON, the child's stdin
    out.log, err.log        the child's stdout and stderr (appended; the door's line is the last one past out_off)
    <ms>.eid                the journal eidolon made (the folder is its sessions dir, so it is the only one, and never in the chats list)
```

At most 500 runs are kept in `runs.json` (the oldest ended ones go first); a run's folder is never removed by the perch.

## Health

`GET /hub/runs/health` probes `--jev-health` and `--browser-health` (a loopback `http://127.0.0.1|localhost:<port>/<path>`, nothing else is
connected to; 1 s each, side by side, cached 5 s). jev's `status`, `jevlike`, `openjev`, `decisions_log`, `ckpt`, `loaded` and
`openjev_dtype` and the browser's `status` ride along as the service said them; no other key does, and a key an old jev lacks is absent.
**Nothing refuses a run over `decisions_log`** (operator ruling 2026-10-07): the graph tab shows it. A start and an answer do ask the
services again (not the cache) and are a 503 if one that has a flag is down: a resume calls jev and the browser again, and a failed call
is journaled and can never be replayed. A service with no flag is not waited for.

## For R1b

`state::config_of(ctx, rid)` is the run's `config.toml` (what `eidolon --config <it> log --json <journal>` is run with) and
`state::journal_of(ctx, rid)` is its `.eid`; both are `None` for a run the perch does not know. Check a run id with `state::rid_ok`
before using it for anything. The run's journal is read through `eidolon log --json` only; the perch never decodes the file.

## Tests

`tests/runs.rs`. Only `fake_eidolon`'s `workflow` command answers a run (`src/bin/fake_eidolon/workflow.rs`): it replays the F0
journals in `tests/fixtures/jev/` with their exit codes and last lines, and has knobs for the case, the pace, a held SIGTERM, a deaf
child and a lingering one (the header of that file lists them). No model, jev or browser is involved; the health tests serve `/health`
themselves on 48140 and 48141.

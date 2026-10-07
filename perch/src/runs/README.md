# src/runs/ — graph runs (`/hub/runs`)

The runs the page starts for a graph named in `--run-graphs`: a detached `eidolon workflow` child per run, watched through its own
journal.

```text
mod.rs     State, pick() (the dispatch), route, and the two hooks: adopt (after the bind), on_shutdown (before the doors)
state.rs   <home>/runs.json and the routes: GET|POST /hub/runs, /<rid>, /<rid>/answer|cancel|abandon      (R1a)
proc.rs    the detached child, its process group, the wall clock, adopt, on_shutdown                       (R1a)
health.rs  GET /hub/runs/health                                                                            (R1a)
log.rs     GET /hub/runs/<rid>/log?after=N                                                                 (R1b)
graphs.rs  GET /hub/runs/graphs, /graphs/<g>                                                               (R1b)
```

**S0P-a built the seam.** Every handler is a stub with the one signature `(ctx, req, rest) -> Response<BoxBody>` that answers the
empty 404, and `runs::pick` is the finished dispatch, so R1a and R1b fill bodies and touch no `mod` line and no route arm. `rest` is
the path below the handler's own prefix: `""` for `health`, `""` or `<g>` for `graphs`, the run id for `log`, the whole rest for
`state`. `State` is `Default` because `lib.rs` creates it before any hook runs; R1a fills it in `adopt`.

D3: runs are detached and adopted by a restarted perch, so `on_shutdown` must not kill one.

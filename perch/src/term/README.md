# src/term/ — web terminals (`/hub/term`)

The perch hosts the PTYs and keeps an alacritty copy of each screen. The page draws them over one SSE; an agent's `term_*` tools
drive them behind a scoped token.

```text
mod.rs      State (the terminals), pick() (the dispatch), route, shutdown
pty.rs      the PTY, the shell on the spawner thread, reader and writer threads, the ring, close, shutdown   (T1a)
screen.rs   the alacritty copy: text, title, resize                                                        (T1a)
routes.rs   the operator's routes: GET|POST /hub/term, /<t>/input|resize|hold|close|text                   (T1b)
stream.rs   GET /hub/term/events: the one SSE, replay or snap on join                                     (T1c)
snap.rs     the bytes that rebuild a screen from the alacritty copy                                       (T1c)
agent.rs    POST /hub/term/a/<verb>: list, open, read, send, close, behind the agent gate                  (T2)
```

**S0P-a built the seam.** Every route handler is a stub with the one signature `(ctx, req, rest) -> Response<BoxBody>` that answers
the empty 404, and `term::pick` is the finished dispatch, so later slices fill bodies and touch no `mod` line and no route arm. `rest`
is the path below the handler's own prefix: `""` for `stream::events`, the verb for `agent::route`, the whole rest for `routes::route`.
`State` is `Default` because `lib.rs` creates it before any hook runs.

An id is `t<n>`, so `events` and `a` are never ids. `a` and `a/...` are the agent's and nobody else's: `pick` uses the same predicate as
the agent gate (`agent::verb`), and a test holds the two together.

The hook: `term::shutdown`, called from `serve::run`'s cancel arm right after `ctx.stop` is cancelled, before the doors are stopped;
it is `pty::shutdown`'s job to bound itself.

# src/mesh/ — the mesh view (`/hub/mesh`)

What the mesh tab shows: which nodes Aoide knows, whether its daemon answers, the outbox, and a tail of its audit events. Read-only
and network-free: nothing here sends, pairs, spawns or dials a node.

```text
mod.rs    State, route(ctx, req, rest) -> Response<BoxBody>: GET /hub/mesh, GET /hub/mesh/events      (R2b)
```

**S0P-a built the seam.** `route` is a stub that answers the empty 404, as `/hub/mesh` always did, and `serve` hands every
`/hub/mesh[/...]` request to it with `rest` the path below `/hub/mesh` (`""` for the bare path). `State` is `Default` because `lib.rs`
creates it before any hook runs. The flags that tell it where Aoide is (`--aoide`, `--aoide-home`, `--aoide-daemon-socket`,
`--aoide-events`) are already on `Options`.

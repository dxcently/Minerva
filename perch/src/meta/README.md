# src/meta/ — sources for the loaded and diff tabs (`/hub/meta/...`)

What the loaded tab lists (tools, plugins, the policy rows, the guard's status, the home's defaults) and the diff tab's branch line.

```text
mod.rs    State, route(ctx, req, rest) -> Response<BoxBody>: GET /hub/meta/tools|plugins|policy|connectors|home|git   (R3)
```

**S0P-a built the seam.** `route` is a stub that answers the empty 404, and `serve` hands every `/hub/meta/<what>` request to it with
`rest` the path below `/hub/meta/` (never empty: the bare `/hub/meta` is not an arm). `State` is `Default` because `lib.rs` creates it
before any hook runs. The guard's line comes from `guard::status()`.

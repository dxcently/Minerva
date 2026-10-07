# src/install/ — the install notice and update keys (`/hub/install`)

What the page shows about the install and how an update key is added from it. The perch only asks the `minerva` binary beside it,
with fixed argv and the key on stdin.

```text
mod.rs    State, route(ctx, req, rest) -> Response<BoxBody>: GET /hub/install, /keys; POST /keys/inspect|add|remove|arm   (R4)
```

**S0P-a built the seam.** `route` is a stub that answers the empty 404, and `serve` hands every `/hub/install[/...]` request to it with
`rest` the path below `/hub/install` (`""` for the bare path). `State` is `Default` because `lib.rs` creates it before any hook runs.
`--minerva` is already on `Options`.

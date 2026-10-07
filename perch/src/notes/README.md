# src/notes/ — the Notepad (`/hub/notes`)

Notes are `<home>/notes/<name>.md`, `0600`, with a trash beside them. Sending one as mail is `mail`'s.

```text
mod.rs    State, route(ctx, req, rest) -> Response<BoxBody>: GET|POST /hub/notes, /<n>, /<n>/rename|trash|send, /trash   (N1)
```

**S0P-a built the seam.** `route` is a stub that answers the empty 404, and `serve` hands every `/hub/notes[/...]` request to it with
`rest` the path below `/hub/notes` (`""` for the bare path). `State` is `Default` because `lib.rs` creates it before any hook runs.
`POST /hub/notes/<n>/send` is for N1 to route to `mail::send_note(ctx, req, name)`, which is already a stub.

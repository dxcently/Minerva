# src/allow/ — allow for the rest of this chat

Rules the operator makes from a pending ask ("allow this, here, from now on"), and the answerer that applies them without a page.

```text
mod.rs      State (the rules; also the watch::Sink), and the three routes: list, create, revoke      (A1b)
matcher.rs  the pure matcher: rules + ask + door -> the rule that answers it, or none                (A1b)
store.rs    the rules' storage: memory bound to the door pid, or <home>/allow.json                   (A1b)
```

**S0P-a built the seam.** The three routes are `GET|POST /hub/sessions/<id>/allow` and `POST /hub/sessions/<id>/revoke`; `serve`
hands them over before the body is read, with the session id already checked, and each is a stub with the one signature `(ctx, req, id)
-> Response<BoxBody>` that answers the empty 404. `State` implements `watch::Sink` with the trait's no-op defaults, and `serve::door_up`
passes `ctx.allow.clone()` as the sink, so A1b overrides `hello` and `frame` and adds no argument anywhere. `State` is `Default` because
`lib.rs` creates it before any hook runs.

The sink is told a `watch::DoorKey {pid, port}`, not a chat id: the watcher opens before the registry row exists. Bind it to a chat
later through `sessions::Registry::id_of_pid`.

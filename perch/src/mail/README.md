# src/mail/ — Aoide mail (`/hub/mail`)

A note is sent as Aoide mail, and a letter that comes back is a card in the notes tab. Aoide is reached only as a subprocess
(`src/aoide/`), never a transport of the perch's own.

```text
mod.rs    State, pick() (the dispatch), route, send_note, optin, start
send.rs   GET /hub/mail/nodes; the body of POST /hub/notes/<n>/send                                (M2)
recv.rs   GET /hub/mail, POST /hub/mail/<msgid>/place|dismiss|save; the receive task               (M2)
agent.rs  POST /hub/mail/a/<verb> behind the agent gate; POST /hub/sessions/<id>/mailsend           (M3)
```

**S0P-a built the seam.** Every handler is a stub with the one signature `(ctx, req, rest) -> Response<BoxBody>` that answers the
empty 404, and `mail::pick` is the finished dispatch, so M2 and M3 fill bodies and touch no `mod` line and no route arm. `rest` is the
path below the handler's own prefix: `""` for `send::nodes`, the verb for `agent::route`, the whole rest for `recv::route`, the note's
name for `send_note` and the chat's id for `optin`.

Two entry points belong to other routes: `notes` owns `POST /hub/notes/<n>/send` and calls `mail::send_note`; `POST
/hub/sessions/<id>/mailsend` is a session verb that `serve` hands to `mail::optin`. `State` is `Default` because `lib.rs` creates it
before any hook runs. The hook: `mail::start`, after the bind.

A message id is 64 hex digits, so `nodes` and `a` are never ids; `a` and `a/...` are the agent's and nobody else's, by the same
predicate as the agent gate.

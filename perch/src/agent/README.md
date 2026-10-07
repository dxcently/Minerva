# src/agent/ — the agent gate

How a chat's own tools reach the perch, and how the perch knows which chat they came from.

```text
mod.rs     State, Scope, Caller, agent_scope() + verb() (the one predicate), authenticate/start/on_exit
token.rs   the scoped term and mail tokens, their 0600 files, and the gate that compares them   (AG)
attrib.rs  peer socket -> /proc/net/tcp inode -> live door pid -> chat id                      (AG)
```

`/hub/term/a/<verb>` and `/hub/mail/a/<verb>` are the only paths behind a **scoped** token. Gate 1 in `serve.rs` asks
`agent_scope(path)` before the master check; a master bearer or a cookie never authenticates there, and a scoped token on
any other path is the ordinary 401. `term` and `mail` route to their agent handlers with the same `verb()` predicate, so a
path is an agent route if and only if it went through `authenticate`.

**S0P-a built the seam, AG builds the gate.** `authenticate` refuses everything with 401 (fail closed); `start` and `on_exit`
do nothing. `State` is `Default` because `lib.rs` creates it before any hook runs: AG fills it in `start`. `authenticate`'s
`Err` is a `Box<Response>`, as `tree::base`'s is.

**`on_exit` is compare-before-unlink.** It removes a token file only if the file still holds this perch's token (and the url file
only if it still holds this perch's url), the pattern of `sessions::drop_token_file`. A flag's path outlives the perch, and a newer
perch or the operator may have written a file there since; an unconditional `remove_file` would delete someone else's credential.
Remember what `start` wrote in `State`, compare against that, and leave anything else in place.

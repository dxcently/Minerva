# src/agent/ — the agent gate

How a chat's own tools reach the perch, and how the perch knows which chat they came from.

```text
mod.rs     State, Scope, Caller, agent_scope() + verb() (the one predicate), authenticate/start/on_exit
token.rs   the scoped term and mail tokens, their 0600 files, and the gate that compares them
attrib.rs  peer socket -> inode (sock_diag, then /proc/net/tcp) -> live door pid -> chat id
```

`/hub/term/a/<verb>` and `/hub/mail/a/<verb>` are the only paths behind a **scoped** token. Gate 1 in `serve.rs` asks
`agent_scope(path)` before the master check; a master bearer or a cookie never authenticates there, and a scoped token on
any other path is the ordinary 401. `term` and `mail` route to their agent handlers with the same `verb()` predicate, so a
path is an agent route if and only if it went through `authenticate`.

## What the gate answers

```text
  no bearer, or one that is nobody's                      401
  the other scope's token (mail on term, term on mail)    401   a scoped token is good for its own prefix only
  the master bearer, or a login cookie                    404   a real credential, for another door
  this scope's token, from a socket no live door owns     403   "term tools work in chats this Minerva opened"
  this scope's token, from a live door's own socket       in    Caller { chat } in the request's extensions
```

A scope whose file flag was not given has no token and nothing opens it. The check order is the credential first, constant time,
and `/proc` only for a caller that holds the scope's token, so a stranger costs one comparison. A handler behind the gate reads
the chat from `req.extensions().get::<agent::Caller>()` and from nowhere else: a body, a query and a header are all the caller's.

`whoami` is the gate's own verb (`POST /hub/term/a/whoami`, `/hub/mail/a/whoami`): it answers `{"chat": <id>}` from inside
`authenticate`, with the stop, host, `sec-fetch` and method gates repeated, because the answer *is* the attribution. Every other
verb is the handler of T2 (term) or M3 (mail).

## The files

`--term-token-file`, `--mail-token-file` and `--agent-url-file`, all optional, all off by default. With none of them given
nothing is minted and nothing is written. With them, at every start:

- the term and the mail token are `harnox::crypto::random_token` (256 bits), different from each other and from the master;
- each is written atomically, 0600, and **read back** with `O_NOFOLLOW`, owner and mode checked, before the perch serves;
- the url file holds `http://<bound address>/`, the address the agent's tools call;
- a start that cannot do all of that is refused (no boot line), never served half-closed, and takes back what it had written;
- paths must be absolute, distinct from each other, and none of them the perch's own token file (the master one, by any spelling:
  a `..`, a link to the directory), which is looked at before anything is written and again by file identity after.

**The directory.** A directory that is missing is made 0700, every level of it. One that exists is **never chmod'ed**: it must be
ours and not writable by group or others (0755 is fine, 0775 is refused with a sentence), because someone who can write in it
can swap a file for another. Then the chain above it, resolved: every ancestor must be owned by us or root and, if group- or
other-writable at all, sticky (`/tmp` is, a shared directory without the bit is not), because whoever can rename a directory on
the path can put theirs where this one was.

**`on_exit` is compare-before-unlink.** A file is removed only if it is still *the file this perch wrote* (device and inode, from
the `fstat` of the descriptor it was read back through) **and** still holds what this perch put in it (the token, or the url), the
pattern of `sessions::drop_token_file`. Content alone is not enough: the url file of two perches on one port holds the same
bytes, so a perch still draining would take the url file a later perch had written. A flag's path outlives the perch, and a newer
perch or the operator may have written a file there since; an unconditional `remove_file` would delete someone else's credential.
A link, a FIFO (opened `O_NONBLOCK`, so the read refuses it as "not a regular file" and does not wait for a writer for ever),
a file that is not ours or not owner-only is left in place. It is also called when `start` failed part-way. A crash leaves the
files behind; the next start replaces them with new tokens.

A token is in the file and in memory. It is not in argv, in the environment, in a log line, in a response or in a `Debug`.

**Doors and proxies.** The HTTP clients a real door's tools are built on (hyper-util, reqwest) have no loopback bypass of their
own: with `http_proxy` or `ALL_PROXY` exported, a call to this perch would go to the proxy, arrive from the proxy's socket (so
no door: 403) and leave the scoped token in the proxy's history. `spawn::door_command` (which `spawn::fork` runs) appends `127.0.0.1,localhost,::1` to the door's
`NO_PROXY` and `no_proxy` (both spellings set, whatever the operator had kept first).

## The attribution

```text
  1  kernel socket table     sock_diag by the four-tuple (peer -> bound), then /proc/net/tcp{,6}      ->  inode
  2  /proc/<pid>/fd          the live door whose descriptors hold socket:[inode]                       ->  pid
  3  Registry::id_of_pid     that pid, asked again, is still a live door                               ->  chat
```

Door pids only: no walk up through parents. A `bash` or `curl` the agent ran has its own socket, so it is refused, and so are a
terminal's shell, a subagent, a workflow run and the TUI. This is a guard rail, not a boundary: a process of the same uid can
read the master token or take a door's descriptor, and the attribution says only which door's socket the bytes arrived on. It
fails closed: no row, two rows, an inode of 0 (`TIME_WAIT`), a door whose descriptors cannot be read, all give `None`.

`sock_diag` is an exact hash lookup, so its cost does not grow with the box's connections; `/proc/net/tcp` is linear (13 ms for
a thousand sockets) and is the fallback. Each door's `/proc/<pid>/fd` is read with `readlinkat` on the open directory. A call
that arrives on a connection the gate has seen before skips the walk: `State` remembers where an inode was last found (256 at
most), and the hint is **asked, not believed**: one `readlink` of that descriptor must still say `socket:[inode]`, with the inode
the kernel named for this connection just now, and step 3 decides either way.

## The fake door

`src/bin/fake_eidolon/agent.rs` is the door that calls the perch, so a test can see the attribution with a real socket.
`FAKE_AGENT_CALL=<scope>/<verb>:<json>` (several separated by newlines; `@file` to read the script from a file at every say) makes
the door, after each `say`, send the calls itself from its own socket; with `FAKE_AGENT_VIA=child` it has a child `sh -c curl`
send them instead (a non-door, so a 403). `FAKE_AGENT_TERM_TOKEN_FILE`, `FAKE_AGENT_MAIL_TOKEN_FILE` and `FAKE_AGENT_URL_FILE`
say where the token and url files are. Each call is written to the fake's record as `agent pid=.. via=.. <scope>/<verb> <status>
<body>`. A call can be made the way a tool would not: `[method=GET,host=evil.example,sec-fetch-site=cross-site]term/whoami:{}`.
The fake honours `http_proxy` / `ALL_PROXY` and `NO_PROXY` as a hyper-util client does, so a dead proxy in the perch's environment
shows whether the bypass reached the door. The grammar is documented at the top of that file.

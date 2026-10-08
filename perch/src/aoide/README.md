# src/aoide/ — the Aoide runner

The one place that builds an `aoide` argv and runs it: a closed set of shapes, `--flag=value` spelling, `--` before any text, a timeout
that kills the child, a stdout cap, a closed stdin. `mesh` (R2b) and `mail` (M2) call it; it has no route and no `Ctx` field.

```text
mod.rs    Aoide, Shape (To, Sender), Reply, Error, Limits: the runner and every call it can make          (R2a)
```

**Aoide is a program, never a crate.** It is GPL-3.0 and the perch is MIT, so the perch reaches it as a child process by `--json`
and nothing else: no `aoide-*` dependency (`tests/gpl_boundary.rs` fails on one in the lock), no linking, no code copied.

## The shapes

```text
Shape::Schema                   schema                                            (the version, and the schema check; see below)
Shape::MeshJson                 mesh --json
Shape::MailOutboxJson           mail outbox --json
Shape::MailRead {reread}        mail read --for=minerva --json [--reread]        AOIDE_SESSION_ID=minerva-perch, on this call only
Shape::MailSend {to, from,      mail send --json --to=<node>/minerva --from=<minerva|minerva-agent> [--mesh=<m>]
  mesh, subject, hold, text}      --subject=<s> [--hold] -- <text>
```

Nothing that touches a node has a variant: no `node list`, `session`, `send`, `pair`, `spawn`, `allow`, `node status`, `node pull`,
`dispatch`, `mail poll` or anything on the A2A door.

- **`Shape::argv()` is the only code that spells one**, and it checks before anything is started. A refusal is an `Error::Refused`
  with a sentence, costs no process and leaves nothing behind. Refused: a node or mesh outside `^[a-z0-9][a-z0-9-]*$` (64 at most);
  `To::Node("self")` (use `To::SelfNode`); a mesh with `SelfNode` (this machine is in no mesh); `hold` with `SelfNode` (`hold needs another
  node`: aoide wants `<node>/<name>` for `--hold`); a subject that is empty, over 120 characters, or not one line (any control character
  but a tab, U+2028, U+2029); a text that is empty, over 64 KiB, or has a NUL. Nothing is trimmed or cut: what is sent is what was asked.
- **`--flag=value`, always.** Aoide's parser lets a spaced flag swallow the next token (unless it starts with `--`), and refuses a value
  that is also a command word, so `--subject status` is not safe and `--subject=status` is. The text is the last argument, after `--`, so a
  letter that starts with `--` or says `--help` is a letter. `from` is an enum (`Sender::Minerva`, `Sender::MinervaAgent`), never text.
- **The reader.** A plain `mail read` advances the cursor of the reader `minerva-perch`, and so does a `--reread`: the 0.0.26 reread
  reprints the whole history and moves the cursor to the newest entry, so it consumes unread letters exactly like a plain read. The
  identity is set for a read and for nothing else.
- **Version and schema.** `aoide --version` does not exist in 0.0.26: it exits 2 with usage. So the version is the top-level `"aoide"`
  string of `aoide schema`, and [`Aoide::schema_check`] also reads the commands and flags the perch sends (`USED`: `mail send` json, to,
  from, subject, mesh, hold; `mail read` json, for, reread; `mail outbox` json; `mesh` json) and refuses with a sentence that names the
  first one the schema does not list. A refusal turns mail (or mesh) off; the gate is asked at most once a minute, good or bad.

## The child

- **Never a `PATH` lookup.** `Aoide::bin` is `--aoide` (else `~/.local/bin/aoide` when executable; `main.rs` decides), and `run` refuses a
  relative one. The operator has a stale 0.0.25 in `~/.cargo/bin`, ahead of the real one on `PATH`.
- **Its environment** is cleared, then given the allowlist `ENV_KEEP` (`PATH`, `HOME`, `LANG`, `TZ`, and every `LC_*`), then the call's
  own: `AOIDE_ROOT` (`root`, from `--aoide-home`), `AOIDE_AUDIT_LOG` (`audit_log`, `--aoide-events`), `AOIDE_DAEMON_SOCKET`
  (`daemon_socket`) and `XDG_RUNTIME_DIR` (`runtime_dir`), each only when given and each absolute, and `AOIDE_SESSION_ID` for a read.
  The list is one constant, so a variable a call needs is one line there. Why the allowlist: a perch's proxy variables and provider keys
  do not reach aoide (a letter deposit must not go through an operator's proxy), a perch started inside a conducted session would hand its
  `AOIDE_SESSION_ID` to every call, and an ambient `AOIDE_STATE_DIR` moves the mailbase away from `<root>/state/mail/base.jsonl`, the file
  M2's receive loop stats. A test sets `HTTP_PROXY` and `OPENAI_API_KEY` on the perch and checks the fake sees neither. Test doubles'
  `FAKE_*` variables are added through `Aoide::env`, which the runner passes on.
- **Bounded.** stdin is `/dev/null`. stdout is capped at 1 MiB by default (one byte more is `Error::TooBig` and the child is killed); the
  limits of one call are a `Limits` (timeout, stdout cap), and the callers set theirs: a plain `mail read` takes 4 MiB (`PLAIN_CAP`),
  a `--reread` 16 MiB and 30 s (`REREAD_CAP`, `REREAD_TIMEOUT`), `aoide schema` 512 KiB (`SCHEMA_CAP`), and a `mail send` 45 s
  (`SEND_TIMEOUT`, longer than aoide's own 30 s, so a letter aoide spooled is not answered as "did not answer"). The default timeout is
  10 s (`Error::Timeout`; a mail read takes a blocking `flock` on `.stage.lock`, so a held lock ends there). stderr keeps its first 8 KiB
  and the rest is read and dropped, so a chatty child never stalls on a full pipe. On a timeout or a cap the child's whole process group is
  killed and reaped. A process the child left holding a pipe is waited for 250 ms and then killed with the group. The child also gets
  `PR_SET_PDEATHSIG` (SIGKILL) before its exec, so a perch that dies takes a running aoide with it.
- **Logged**: one line per call, the verb and the exit code (`perch: aoide ran`). Never an argument, the text, or what aoide printed.
  `Shape` and `Reply` print themselves the same way (sizes, not words), so a `{:?}` cannot put a letter in a log.

## What comes back

`run` returns `Ok(Reply)` for an exit 0 whose `--json` envelope does not say otherwise, and an `Error` for everything else:
`NoBinary`, `Refused`, `Spawn`, `Timeout`, `TooBig`, `NotJson` (exit 0 but stdout is not one JSON object) and `Failed(Reply)`, an aoide that
ran and said no, with its whole reply so a caller can read `data.reason` (`unknown-node`, `unpaired-node`) or `data.meshes` (the refusal
for a node that is trusted in more than one mesh). `Error`'s `Display` is aoide's own `message` when it sent one, else the first line of its
stderr, else the exit code.

The envelope is `{status, command, message, gated, changed?, data?}` and **its shape is parsed defensively**: `aoide schema` does not
describe it and `schemaVersion` stayed `"0"` through every change. The `mail send --json` answer is not a flat record either: with
`--subject` (which the perch always sends) its `data.recipients[i]` carries the msgid, status, delivery and error of one recipient, and
`mail/send.rs` reads that shape; the fixtures hold the observed one. `Reply::status()`, `message()` and `data()` return `None` for anything
else; only a stdout that is, whole, a JSON object counts as an envelope (a line that starts with `{` after a notice does not, because text
printed on stdout can be somebody's letter).

## The stand-in and its fixtures

`src/bin/fake_aoide.rs` accepts exactly the argvs above, from a parser that shares no code with `argv()`, logs every call (refused ones
too) to `FAKE_AOIDE_LOG` with the argv, the session, the `AOIDE_*` variables it was given and the names of all its environment variables,
and replays `FAKE_AOIDE_DIR` (the perch keeps one at `tests/fixtures/aoide/`). `--version` exits 2 with usage, as the real one does, and
`schema` replays `schema.json`. Its header lists the knobs (`FAKE_AOIDE_SLEEP_MS`, `FLOOD`, `STDERR_FLOOD`, `STRAGGLER_MS`, `STDIN`,
`SPARES_SCHEMA`, `FLOOD_SPARES_REREAD`). A test steers one instance with `Aoide::env("FAKE_AOIDE_LOG", ..)`. The perch hands its children
only the allowlist, so a test that starts a whole perch and wants a knob in the fake runs the fake through a wrapper script that sets it
(`steered` in `tests/mail.rs`, `wrapper` in `tests/mesh.rs`).

Tests: `tests/aoide.rs` (the exact argv of every shape, the limits, the environment, the schema and its check, `--version` exiting 2, aoide's
sentences, the fake's own contract) and the unit tests in `mod.rs` (the pure parts: grammar, refusals, `env_for`, the envelope, `missing`,
the version).

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
Shape::Version                  --version
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
- **The reader.** A plain `mail read` advances the cursor of the reader `minerva-perch`; `--reread` does not move it. The identity is set
  for a read and for nothing else.

## The child

- **Never a `PATH` lookup.** `Aoide::bin` is `--aoide` (else `~/.local/bin/aoide` when executable; `main.rs` decides), and `run` refuses a
  relative one. The operator has a stale 0.0.25 in `~/.cargo/bin`, ahead of the real one on `PATH`.
- **Its environment** is the perch's with every `AOIDE_*` variable and `XDG_RUNTIME_DIR` removed, then `AOIDE_ROOT` (`root`, from
  `--aoide-home`), `AOIDE_AUDIT_LOG` (`audit_log`, from `audit_flag`, which is `--aoide-audit-log` and only when that flag was given),
  `AOIDE_DAEMON_SOCKET` (`daemon_socket`) and `XDG_RUNTIME_DIR` (`runtime_dir`, read by `runtime_dir()`, the one reader the mesh view's
  default socket and feed also use) from the fields. `runtime_dir()` gives only an absolute value: a relative or empty `XDG_RUNTIME_DIR` is
  none, so the runner gives no call one and the view takes no default from it (`absolute_dir`, unit test
  `a_relative_or_empty_runtime_dir_is_none_as_an_unset_one_is`). The other three that are given must be absolute: a relative one is
  `Error::Refused` on every call, so nothing runs with it (and `main.rs` refuses a relative `--aoide-audit-log` and `--aoide-daemon-events`
  at the start, and the two feeds at one file, `mesh::feeds_apart`). The mesh view reads the same flag through the same `audit_flag`, so the file aoide appends to and
  the file the view tails are one file. The daemon's events feed (`--aoide-daemon-events`) is not a field of the runner: no call is ever
  given its path, under any name. Why the removal: a perch started inside a conducted session would hand its
  `AOIDE_SESSION_ID` to every call, and an ambient `AOIDE_STATE_DIR` moves the mailbase away from `<root>/state/mail/base.jsonl`, the file
  M2's receive loop stats. Everything else (`HOME`, `PATH`, `LANG`, the tests' `FAKE_*`) passes through.
- **Bounded.** stdin is `/dev/null`. stdout is capped at 1 MiB (one byte more is `Error::TooBig` and the child is killed). stderr keeps
  its first 8 KiB and the rest is read and dropped, so a chatty child never stalls on a full pipe. The call has 10 s (`Error::Timeout`; a
  mail read takes a blocking `flock` on `.stage.lock`, so a held lock ends there); on a timeout or a cap the child's whole process group is
  killed and reaped. A process the child left holding a pipe is waited for 250 ms and then killed with the group.
- **Logged**: one line per call, the verb and the exit code (`perch: aoide ran`). Never an argument, the text, or what aoide printed.
  `Shape` and `Reply` print themselves the same way (sizes, not words), so a `{:?}` cannot put a letter in a log.

## What comes back

`run` returns `Ok(Reply)` for an exit 0 whose `--json` envelope does not say otherwise, and an `Error` for everything else:
`NoBinary`, `Refused`, `Spawn`, `Timeout`, `TooBig`, `NotJson` (exit 0 but stdout is not one JSON object) and `Failed(Reply)`, an aoide that
ran and said no, with its whole reply so a caller can read `data.reason` (`unknown-node`, `unpaired-node`) or `data.meshes` (the refusal
for a node that is trusted in more than one mesh). `Error`'s `Display` is aoide's own `message` when it sent one, else the first line of its
stderr, else the exit code.

The envelope is `{status, command, message, gated, changed?, data?}` and **its shape is parsed defensively**: `schema --json` does not
describe it and `schemaVersion` stayed `"0"` through every change. `Reply::status()`, `message()` and `data()` return `None` for anything
else; only a stdout that is, whole, a JSON object counts as an envelope (a line that starts with `{` after a notice does not, because text
printed on stdout can be somebody's letter).

## The stand-in and its fixtures

`src/bin/fake_aoide.rs` accepts exactly the argvs above, from a parser that shares no code with `argv()`, logs every call (refused ones
too) to `FAKE_AOIDE_LOG`, and replays `FAKE_AOIDE_DIR` (the perch keeps one at `tests/fixtures/aoide/`). Its header lists the knobs
(`FAKE_AOIDE_SLEEP_MS`, `FLOOD`, `STDERR_FLOOD`, `STRAGGLER_MS`, `STDIN`). A test steers one instance with `Aoide::env("FAKE_AOIDE_LOG", ..)`,
and a test that starts a whole perch puts the same variables in the perch's environment, which the runner passes on.

Tests: `tests/aoide.rs` (the exact argv of every shape, the limits, the environment, aoide's sentences, the fake's own contract) and the
unit tests in `mod.rs` (the pure parts: grammar, refusals, `env_for`, the envelope, the version).

# src/term/ — web terminals (`/hub/term`)

The perch hosts the PTYs and keeps an alacritty copy of each screen. The page draws them over one SSE; an agent's `term_*` tools
drive them behind a scoped token.

```text
mod.rs      Terms (the table), State, Host, OpenSpec/Info/Record/Change, terms.json, pick() (the dispatch), route, shutdown   (T1a + the seam)
pty.rs      the PTY, the shell on the spawner thread, reader/writer/waiter threads, the ring, ending a session   (T1a)
screen.rs   the alacritty copy: replies allow-list, text, title, resize, the guard in front of the parser        (T1a)
routes.rs   the operator's routes: GET|POST /hub/term, /<t>/input|resize|hold|close|text                         (T1b)
stream.rs   GET /hub/term/events: the one SSE, replay or snap on join                                           (T1c)
snap.rs     the bytes that rebuild a screen from the alacritty copy                                             (T1c)
agent.rs    POST /hub/term/a/<verb>: list, open, read, send, close, behind the agent gate                        (T2)
```

**S0P-a built the seam.** Every route handler is a stub with the one signature `(ctx, req, rest) -> Response<BoxBody>` that answers
the empty 404, and `term::pick` is the finished dispatch, so later slices fill bodies and touch no `mod` line and no route arm. `rest`
is the path below the handler's own prefix: `""` for `stream::events`, the verb for `agent::route`, the whole rest for `routes::route`.
An id is `t<n>`, so `events` and `a` are never ids. `a` and `a/...` are the agent's and nobody else's: `pick` uses the same predicate as
the agent gate (`agent::verb`), and a test holds the two together.

**T1a built the host** (this README's rest). It has no route: `cargo test` is its only check.

**T1b built the operator's routes** (the section below). `tests/term.rs` runs them over a real `/bin/sh`.

## What a terminal is

```text
  Terms (ctx.term, made on first use)          one Terminal = one shell on one PTY
  ┌───────────────────────────────┐            ┌────────────────────────────────────────────────────────────┐
  │ Vec<Entry>  Live | Ended      │  open ───▶ │ spawner thread  posix_openpt(O_CLOEXEC) + login shell      │
  │ ids t1.. never reused         │            │                 (setsid: pid = sid)                        │
  │ Hub: tick (watch) + Change    │            │ perch-term-read   poll+read master ─▶ [screen lock]        │
  │      (broadcast, 256)         │            │                       Guard ─▶ Emu.feed ─▶ Ring.push, seq  │
  │ terms.json  {next, terms}     │            │ perch-term-write  WriteQueue (3 lanes) ─▶ master           │
  └───────────────────────────────┘            │ perch-term-wait   waitid(WNOWAIT) ─▶ Exit; reaps last      │
                                               └────────────────────────────────────────────────────────────┘
```

* The shell is a **login shell**: `$SHELL` of the perch's environment, else the passwd entry's, else `/bin/sh`. It starts in `cwd`
  (else `$HOME`) with the perch's environment less its private names (below) plus `TERM=xterm-256color`, `COLORTERM=truecolor`,
  `MINERVA_TERM=<id>`, and `LANG=C.UTF-8` if there was none. The fork runs on the spawner thread (`Spawner::run`), so it never
  interleaves with a door's.
* **The PTY is opened here, not by a library.** `posix_openpt(O_RDWR|O_NOCTTY|O_CLOEXEC)`, `grantpt`, `unlockpt`, and the slave opened
  `O_CLOEXEC` too, so there is no instant in which a fork anywhere in the perch could carry the master into another program. The
  shell is started with `std::process::Command` (the slave as 0, 1 and 2, `setsid` and `TIOCSCTTY` in the child, the signal
  dispositions and mask reset, `close_range(3, ..)`). `portable-pty` is still in `Cargo.toml` but nothing in `src/` uses it.
* The master is used through its raw descriptor, non-blocking, with `poll` and a 100 ms tick. Every thread looks at its terminal's
  state each tick, so none outlives its terminal even when something that escaped the session still holds the slave.
* `Info` is what `GET /hub/term` lists, `Record` is what `terms.json` keeps, `Change` is what a stream follows.
* If one of the three threads cannot be started, `Terminal::start` hangs the session up and reaps the shell before it returns the error.
* A shell that exits is reported **after** the reader has taken the last of what it wrote: the waiter gives the reader up to 250 ms
  to see the master's end (`EIO`), then says the terminal ended. A background job that keeps the slave open means the end never comes,
  so the wait is bounded, and the screen has by then what the reader had.

## Locks, and what blocks

```text
  Terms.file  ──▶  Terms.inner          the file mutex first; never the file mutex while `inner` is held
  Screen.emu  ──▶  Terminal.ring        the reader holds both while it feeds a read; whoever holds both sees one byte
  Core (queue, title)                   its own small locks; taken after the screen lock is dropped, never under it
```

alacritty raises its events (a reply to a query, a new title, the bell) from inside `advance`. The listener only **collects** them
(`screen::Effects`, capped at 64 replies a feed, the last title wins); `Screen::feed` hands them to the sink once the screen's lock is
dropped. So a sink can take any lock, emit on the hub, and queue to the writer without ever being under the screen's lock.

**Every API that takes a screen's lock blocks** until the reader has parsed the read it is in (a few milliseconds; one read is 16 KiB).
They are for threads that can wait; an async handler goes through `tokio::task::spawn_blocking`:
`Terms::text` and `Terms::resize` (for a live terminal), and on `Terminal`: `view`, `text`, `record`, `resize`. `Terminal::record_until`
and `Screen::try_lock` are the ones with a deadline. `Terms::input`, `agent_input`, `set_hold`, `info`, `list`, `open`, `close` and
`Terminal::read` take no screen lock. The copy never holds a synchronized update (DEC 2026) back: it is ended after each advance, so
this copy shows a frame as far as it has been read where the page shows it whole, and no update can keep a lock or the screen frozen.

## Ending a terminal (close, shutdown, drop)

The shell leads its own session, so its pid is the session id. `pty::end_session(sid, foreground, grace)`:

1. List the session once (`/proc/*/stat`: session id equal, state not `Z`/`X`). `SIGHUP` + `SIGCONT` to the shell's group, and to the
   terminal's foreground group **only if a member found in that same listing is in it** (the number `tcgetpgrp` reports can be one whose
   processes are gone and which now belongs to a stranger). Then to every other member, one by one (a job the shell put in a group of
   its own).
2. Wait up to `grace` for the session to empty.
3. `SIGKILL` whatever is left, repeated each tick, wait up to 0.4 s for it to be gone.

**A process is signalled through a pidfd, never by a bare pid.** `signal_member(pid, sid, sig)` is `pidfd_open(pid)`, then re-read
`/proc/<pid>/stat` and confirm the session id (and that it is not a zombie), then `pidfd_send_signal`. A pid that was handed to a
stranger between the listing and the signal is in another session and is left alone; one that died after the check is a pidfd that
signals nothing. Where the kernel has no pidfd (before 5.3, a sandbox that refuses it) it falls back to the check and a plain `kill`,
and the window between them is open. The groups the perch signals with `killpg` are only its own shell's (the shell is the session
leader, a zombie holding the number until the end) and a foreground group a member was just found in.

The shell is **reaped last**, by its waiter thread, after step 3: until then its zombie keeps the pid (and so the session id) ours.
`signalable()` refuses pid `<= 1`, the perch's own pid and its own group, so a bug can never become `kill(-1)` or `killpg(0)`.

| ending | grace | what it is |
|---|---|---|
| `Terms::close(id)` | 2 s | removed from the table at once; the session goes in the background (`Hangup::wait` to wait). The operator's, unchecked |
| `Terms::close_as_agent(chat, id)` | 2 s | the same, for an agent: refused (`NotYours`, `Held`) before anything is removed, under the table's lock |
| `term::shutdown(ctx)` | 1.7 s, 2.5 s bound | every terminal at once, hung up and *then* recorded; opens are refused from the first line |
| `Drop for Terms` / `Terminal` | 0 | a table or terminal dropped without being ended (a failed test) takes its session with it, hard |

**Shutdown under a deadline.** Each live terminal gets a blocking task of its own: hang it up (grace 1.7 s), wait for its reader to
be done and its exit noted, then build its record. The record **tries** the screen's lock until 150 ms before the 2.5 s bound and
gives a record without screen text if it never gets it; a task that has not finished at the bound is recorded as it is, without text
(`Terminal::record_bare`). The file is written once, at the end, from every record. Nothing on this path ever waits for a lock without
a limit. An open that was in flight when shutdown took the table is checked under the same lock when it lands (`Slot::commit`): the
perch is stopping, so the new terminal is hung up and the open answers `Stopping`; no shell is left running behind a finished shutdown.

**Not covered, on purpose** (the residuals):

* A process that calls `setsid` itself leaves the session and is not found (`setsid sleep 1000` survives a close; a test says so).
* Killing the perch with `SIGKILL` (a crash) runs none of the above: the master closes with the dead perch and the kernel hangs the
  shell up, but a job that ignores `SIGHUP` (a `nohup`) outlives that, and no `PR_SET_PDEATHSIG` is set on shells.
* **`terms.json` is in an agent's reach.** An agent's shell runs as the perch's user and `<home>/terms.json` is an ordinary 0600 file of
  that user, so a shell can read the screens in it and rewrite it. The perch reads it back as untrusted (below) and writes it
  atomically, which bounds what a rewrite can do (ids, sizes, text, a high `next`), not whether it can be done.
* **The shell inherits the perch's environment.** `SSH_AUTH_SOCK`, `*_API_KEY`, cloud credentials: whatever the perch was started
  with is the shell's, as the operator's own login would have it. Only `MINERVA_*` names that say `TOKEN`, `SECRET`, `CODE` or `PASS`
  are dropped (below). The perch's own tokens are in memory and in 0600 files, not in its environment. The 0600 keeps them from other
  users, not from the perch's own: the master bearer is `$XDG_RUNTIME_DIR/minerva/perch-<port>.token`, the shell keeps
  `XDG_RUNTIME_DIR`, and a shell of the perch's uid (an agent's included) can read it, with the same caveat as `terms.json` above.

## What the screen writes back (the allow-list)

A program on the PTY can ask the terminal questions; answering writes to the shell's stdin, so what is written back is chosen, not
forwarded. `screen::reply_kind` is the one function that decides, over every alacritty `Event` (a `match` with no wildcard, so a new
event is a compile error until someone judges it):

| event | written back |
|---|---|
| `PtyWrite` (DA1, DA2, DSR `5n`/`6n`, DECRQM ANSI and DEC, `CSI 18 t`) | only if it is `ESC [` + parameter bytes + one final byte, at most 64 bytes |
| `ColorRequest` (OSC 4/10/11/12 `?`) | the default phosphor palette's colour, only as `ESC ]` + digits/`;:/`/lower-case + BEL or ST, at most 64 bytes |
| `ClipboardLoad`, `ClipboardStore` (OSC 52) | nothing (and `osc52` is `Disabled` in the config as well) |
| `TextAreaSizeRequest` (`CSI 14 t`, pixels) | nothing |
| XTVERSION, DECRQSS | nothing, ever (alacritty does not ask; the page swallows them too, see below) |
| kitty keyboard query (`CSI ? u`) | nothing (alacritty answers it only with `kitty_keyboard` on, which this config leaves off) |
| everything else (`Title`, `Bell`, `Wakeup`, `Exit`, ...) | nothing |

Replies queue behind a 4 KiB budget of their own and go to the shell before typed input; a reply that does not fit is dropped.
Colour queries see the **default theme**, not the page's current one (the perch cannot know it).

**For T4a (the page's swallow list).** The perch is the only answerer; the page's xterm must swallow exactly the queries the perch
answers and answer none of them, or the shell reads two answers. That list is: **DA1** (`CSI c`), **DA2** (`CSI > c`), **DSR** `CSI 5 n`
and `CSI 6 n`, **DECRQM** (`CSI Ps $ p` and `CSI ? Ps $ p`), **`CSI 18 t`**, and **OSC 4, 10, 11 and 12** with a `?`. **DECRQSS**
(`DCS $ q`) and **XTVERSION** (`CSI > 0 q`) are never answered by either side. The page swallows both as well, so neither side replies to them. Nothing else is on the list.

## What the guard does to the bytes

The page's xterm sees the raw bytes; the copy an agent reads must show the same text. `screen::Guard` runs in front of the parser and
follows the page's emulator state by state (xterm.js 6.0.0), so vte (alacritty's parser) is made to agree with it. It holds nothing
back but the end of a UTF-8 character (at most 3 bytes; `Terminal::text` reports the seq that far behind the ring).

| input | what the copy does |
|---|---|
| a string: OSC, DCS, PM, APC, SOS | swallowed to its end: ST (`ESC \`), CAN or SUB, and BEL for an OSC. Never ended early, so what the page hides is hidden here and what follows is the same text on both |
| an OSC's body | the first 1024 bytes reach the parser (it collects them without a limit); the rest to the end is dropped, and the end passes |
| the body of a DCS, PM, APC, SOS | dropped whole |
| C1 controls in UTF-8 (U+0080..U+009F) | the escapes they stand for (U+009B is CSI, U+009D OSC, U+009C ST); the rest as CAN |
| a character that is not ASCII inside an escape, a control sequence or an SOS/PM/APC | the page abandons the sequence; the guard writes a CAN so this copy does too |
| bytes that are not UTF-8 | one U+FFFD each, as the page shows them |
| `CSI b` (REP) | the count clamped to the screen's width; a REP of a zero-width character is dropped |
| zero-width characters | 16 in a row at most with no character that takes a cell between them, and at most 16 on one cell however the cursor is moved between them (checked against the cell they would land on) |
| `CSI Pt;Pb r` (DECSTBM) | passed on unchanged when the page takes it, and the region it leaves is kept for the snapshot. A region the page ignores (its top on or past the last line, or a bottom not below its top) is cancelled with a CAN, so the copy ignores it too |
| a bare `CSI r`, `CSI ! p` (DECSTR), `ESC c` (RIS) | pass on; each clears the kept region, as the page's xterm does |
| OSC 8 (hyperlinks) | dropped (the grid would hold an allocation for every cell written under one) |

None of this changes the ring; the page reads what the shell wrote. The tests feed the page's probe (`ESC ] 9999 ;` + 1100 filler +
`\r AGENT-ONLY: ... \n` + BEL, which vte would end early and print the rest of), unbounded continuation bytes, strings of every kind,
REP with marks, one unique hyperlink per cell over a full grid, and every code point's width against the emulator's.

## The hold (what it stops, for T2)

The hold is the operator's stop for the agent. `Terms::agent_input(id, chat, bytes)` is **the one way an agent's bytes get in**; the
operator's own typing is `Terms::input` and the hold never touches it. Turning the hold on (`Terms::set_hold`):

| | stopped by the hold? |
|---|---|
| new agent input | yes: `InputError::Held`, decided under the lock that queues the bytes, so nothing gets in behind a hold set a moment before |
| agent input queued and not yet written | yes: dropped when the hold goes on |
| a write of an agent's bytes in progress | waited for (the queue's lock is held across it), so none is half-way when the hold returns |
| bytes already written to the PTY and not yet read by the program | yes: `tcflush(TCIFLUSH)` on the slave, opened by name for the moment. **That also takes the operator's unread bytes**: the PTY does not know whose they were |
| bytes the program has already read | **no** (a line the shell has read runs) |
| the program's output | no |
| the operator's input from then on | no |
| an agent's `close` of the terminal | yes: refused with `CloseError::Held` (423), decided under the table's lock with the owner rule, so the terminal stays and its shell is not hung up (U21a) |

The writer has three lanes (replies 4 KiB, the operator's 64 KiB, an agent's 64 KiB), taken in that order, so an agent that fills its own
cannot keep out a reply or the operator's `^C`. The writer also lets at most 1 KiB of an agent's bytes sit unread in the PTY (`TIOCINQ`
on the slave): a program in raw mode that reads nothing keeps its input queue, and a `^C` written after a full one would wait behind all
of it. What does not fit waits in the agent's lane, where the operator's input goes ahead of it. The agent's bytes go in 512 at a time,
and after each the writer waits (a few milliseconds at most) for them to show in the input queue before it counts again: a write lands
in the kernel's buffer first and reaches the input queue a moment later, and a writer that counted only the queue would see it empty
and fill the buffer behind a full queue, with the `^C` at the back of both.

`agent_input` also stamps the terminal (`agent_last_ms`) and emits `Change::Input{id, chat, text}`, with what was typed made printable
(control characters as their pictures, `^M` as `␍`) and cut to 80 characters. Whether this chat may drive this terminal at all is the
route's decision (T2), made before it calls.

## What an agent's terminal tools expose

What a chat's agent can reach through `/hub/term/a/*`, and what the operator should know about each. The first three are the risks the ⌨
tab states; the rest were found in review and are written here so that no reader has to find them.

- **Reads are not limited to the agent's own terminals.** `read` of any web terminal, the operator's included, sends that terminal's
  screen to the model provider, secrets included (ruling 2). Whatever is on the screen can reach the provider. Under auto the policy
  vouches `read` (D6), so it runs without asking.
- **That text is untrusted.** It can carry prompt injection. The perch strips control and invisible characters from it, and judges
  nothing else about what it says.
- **In yolo the door's policy answers yes to `term_send`, and the perch never asks.** The text guard (D9) only judges one send's text.
- **A read leaves no record (U21c).** It sends no stream event and no log line, so the operator's ⌨ tab shows the sends and not the
  reads, and nothing shows which terminal an agent read. Whether a read should show (chat, terminal id, lines, no text) is the
  operator's call and a later slice.
- **`list` is a second route for untrusted text.** Every chat sees every web terminal's id, title, foreground command (`fg`, the
  program in the foreground), who opened it (`by`) and the owning chat's id (`chat`). A title is what the program wrote, cut at 80 characters with controls dropped (an OSC from a remote host
  or a catted file is text in every agent's `list`). A normal shell title, `user@host:path`, tells the provider the operator's user,
  host and folder on every `list` call.
- **The master bearer file is readable from an agent's shell.** It is `$XDG_RUNTIME_DIR/minerva/perch-<port>.token` (`lib.rs`), mode
  0600 and the perch's own uid, and the shell keeps `XDG_RUNTIME_DIR` (`pty::child_env`), so a shell of that uid can read it (read from
  the source; not run). This is the same-uid caveat that `terms.json` has above, and `install/mod.rs` already says it for the bearer.
  With the bearer a shell can call the master gate's routes from `curl`, and the text guard does not judge that path.
- **Every credential exported to the perch reaches the shells.** A shell starts from the perch's environment minus only the `MINERVA_*`
  names that say TOKEN, SECRET, CODE or PASS (`pty::child_env`). So `env`, then `read`, sends `SSH_AUTH_SOCK`, a `*_API_KEY` or a cloud
  credential the operator exported to the perch to the model provider.
- **Keys are not judged (U21b).** The text guard reads one send's `text`, and a keys-only send is not judged at all. `up`, `down`,
  `ctrl-r` and `tab` bring back or complete a line the guard never read: a command the operator typed before the call, or one in the
  shell's history. The keys stay on purpose, since the guard is a hint and not a gate (`install/mod.rs`, what the arm does not defend).
  The guard sees only this call's text, not the shell's line.
- **The operator's hold stops an agent's `send` and `close` (423), not its reads.** No agent route lifts a hold: the operator's
  `hold` route is refused to a term token (`tests/agent.rs`).

## Chats and the cap (D5)

An agent's terminal is for a chat: `OpenSpec` with `owner.by == Agent` and no chat is `OpenError::NoChat`. A chat holds at most
`chat_cap` terminals (`AGENT_CHAT_CAP` = 4 when the caller names none), counted under the table's lock together with the table's
cap, so two opens at once cannot both pass. An agent terminal whose shell has ended but which was not closed **counts**, since it
keeps its place for 30 minutes; when the chat (or the full table) wants another, its own oldest ended terminal is closed to make room
(`Change::Closed`), and only when it has none is the open refused (`ChatCap`, or `Cap` for the table). `Owner::is_agents_of(chat)` is
the one test of "this is that chat's agent's". An operator's terminal that sits beside a chat is not an agent's.

**The operator's share (U21d).** Every agent's terminals together may hold `AGENT_SLOTS` (12) of the table, and the other 4 places are
the operator's alone: an agent's open that would take a thirteenth is `Cap` (409, "no room for another agent terminal"), even when its
chat has room, and the operator can still open up to the 16 in all. An agent's ended terminal counts against that share until it is
closed or let go, and only its own chat can let it go, so a chat cannot free another's place.

## Bounds

| thing | bound | where |
|---|---|---|
| terminals | 16, ended ones included | `MAX_TERMS`; `OpenError::Cap` |
| agent terminals, every chat together | 12 (`AGENT_SLOTS`): the other 4 are the operator's alone (U21d), so four chats at four each cannot lock the operator out | `AGENT_SLOTS`; `OpenError::Cap` for an agent |
| agent terminals of one chat | 4 (or the caller's `chat_cap`), ended ones included, checked with the cap under one lock | `AGENT_CHAT_CAP`; `OpenError::ChatCap` |
| screen | 20-400 columns, 5-200 rows, default 120x32; clamped, never refused | `screen::clamp_size` |
| scrollback | 2000 lines | `screen::SCROLLBACK` |
| memory of one screen | a cell is 24 bytes: a 400-column row about 10 KB, 400x200 and 2000 lines of history about 21 MB, the default 120x32 about 6.5 MB. A cell can own one allocation (zero-width marks, an underline colour): about 64 bytes and 4 per mark, so a program that gives every cell its 16 marks makes about 170 bytes a cell, 150 MB for a largest terminal, for about 30 MB of output (four seconds at the pace limit below). The same for each of the 16 terminals | `screen::SCROLLBACK`, `MAX_ZEROWIDTH` |
| ring of raw output | 512 KiB (`--knob term_ring=N`, 1 KiB..8 MiB) | `pty::RING_BYTES` |
| one read of the master | 16 KiB; output above about 8 MiB/s is slowed (10 ms after each read) | `pty::Throttle` |
| input | 64 KiB per call (`TooBig`); 64 KiB queued for the operator and 64 KiB for an agent (`Full`); all of a call or none | `MAX_INPUT`, `WriteQueue` |
| an agent's bytes unread in the PTY | 1 KiB | `pty::AGENT_AHEAD` |
| an OSC | 1024 bytes reach the parser; other strings none | `screen::STRING_RUN` |
| title | 80 printable characters, one line; announced when it differs, at most every 100 ms, the newest winning | `screen::MAX_TITLE`, `pty::TITLE_EVERY` |
| text read (`Terminal::text`) | 400 lines, 7000 characters, the end kept | `screen::MAX_TEXT_*` |
| an ended terminal | kept 30 minutes, then let go; screen kept 64 KiB | `KEEP_ENDED_MS`, `SAVED_TEXT_BYTES` |
| `terms.json` read | 2 MiB, a plain file | `MAX_FILE` |

## The environment and secrets

`pty::child_env` drops every `MINERVA_*` name that says `TOKEN`, `SECRET`, `CODE` or `PASS` (the perch's own tokens are in memory and in
0600 files, not in its environment; this keeps a later change from handing one to every shell). `MINERVA_TERM` is set last. No secret
is ever put in a shell's environment by the perch, its argv, the ring, `terms.json`, a log line or a `Change`. What a shell prints is
its own and can hold anything the user typed into it: that is why `terms.json` is `0600`. See the residuals above for what the shell
has anyway.

## terms.json

`<home>/terms.json`: `{"next": <n>, "terms": [<record>, ...]}`, `0600`, written atomically (`write_atomic_0600`: temp, fsync, rename;
creates the folder). No `--home`, no file.

* `next` is the **high-water mark of ids**, kept apart from the records. It is raised when an id is taken (so not even a crash hands one
  out twice), never lowered by a close or an expiry, and a restart continues from it (or past the highest id in the records, if that is
  more). A closed terminal's id is not handed out again.
* **The file is never deleted**, only rewritten; with nothing to remember it says `"terms": []` and keeps `next`.
* Written when the table changes in a way a restart should know about: an id taken, an ended terminal closed or expired, and at
  shutdown (every terminal with its last screen, `by_perch`, and how it ended). **One mutex is held from collecting the records to the
  rename**, so two writers cannot hand the disk each other's older view.
* Read back by `Terms::new` as if someone else had written it. The file must be a plain file (`symlink_metadata`: not a link, a pipe, a
  folder) of at most 2 MiB, opened `O_NOFOLLOW`; otherwise, or if it does not parse, it is set aside whole as `terms.json.bad-<secs>`
  (`-1`, `-2` on a clash) and the table starts empty. Set-aside copies older than 30 minutes are deleted when the table is made.
* Records are vetted: ids canonical `t<n>` (no leading zero, six digits), no repeats, at most 16, nothing older than 30 minutes, an end
  more than a minute in the future moved back to now, sizes clamped, and the title, cwd, chat id and screen text keep no control
  character, no format character (tags, U+061C, U+2060..206F, variation selectors, U+00AD, U+180E, U+FFF9..FFFB, ...) and no line or
  paragraph separator (the newline and the tab stay in the text). A chat id with any of them is dropped, not repaired.
* An ended terminal read from the file can be listed, read (`text`) and closed; `input`, `resize` and `hold` answer `Ended`.

## The operator's routes and refusals (T1b, `routes.rs`)

The operator is the master bearer or a same-origin login cookie; gate 1 has passed by the time a request is here. The route bodies
arrive unread, and each route reads its own under its own cap (`serve::body_of`). Nothing an answer carries is the environment, a
token or the perch's argv.

| route | answers | refused with |
|---|---|---|
| `GET /hub/term` | 200, a list of `Info` (oldest first, ended ones included; `started_ms` is in it as well) | none |
| `POST /hub/term {chat?, cwd?, cols?, rows?, title?}` (no body is the default) | 201 `{id}` | 400 not that shape; 404 no such chat; 422 `cwd` not under a `--root`, or not a folder (with the reason); 409 the table holds 16; 503 the perch is stopping; 500 the shell did not start |
| `POST /hub/term/<t>/input` (`application/octet-stream`, the bytes as sent) | 204 | 415 any other `Content-Type`, before the body is read; 413 over 64 KiB, by the declared length before a byte is read, or by the first frame past it; 409 the shell has ended; 503 the queue is full; 404 no such terminal |
| `POST /hub/term/<t>/resize {cols, rows}` | 200 `{cols, rows}`, the size now (T1a's clamp: 20-400 columns, 5-200 rows) | 400 not an object of unsigned integers (unknown keys too); 404; 409 an ended terminal read back from `terms.json` |
| `POST /hub/term/<t>/hold {hold}` | 200 `{hold}`, the state now | 400 not `{hold: bool}`; 404; 409 as resize |
| `POST /hub/term/<t>/close` | 202, no body; the hang-up runs in the background and the request does not wait for it | 404 |
| `GET /hub/term/<t>/text?lines=N` | 200 `{text, cursor, alt_screen}`, `cursor` `null` for an ended terminal; `lines` is digits given once, 100 when absent, and a number over 400 reads 400 (`screen::MAX_TEXT_LINES`); at most 7000 characters (`screen::MAX_TEXT_CHARS`), the end kept | 400 `lines` not digits or given twice; 404 |

What the table does not show:

* **Ids and paths.** `valid_id` (`^t[0-9]{1,6}$`) is checked first. A bad id, a path that is no route and a terminal that is not there
  are the empty 404. A route with the wrong method is a 405 that names the method it takes (`Allow`). `DELETE` is a 405 at the gate,
  before any route. `a` and `events` are never ids. `/hub/term/a/<verb>` belongs to the agent gate, which answers a master credential
  (the bearer, or a same-origin login cookie) with the empty 404, and no credential or a wrong token with 401, before a route is asked.
* **Existence before the body.** A terminal is looked up before its `Content-Type` or its body is judged, so a request to a terminal
  that is not there is a 404 whatever body or `Content-Type` it sends. The method is checked before the terminal is looked up, so a
  wrong method on a real route is a 405 first.
* **Where a shell starts.** An explicit `cwd` goes through `Registry::cwd_under_a_root` (canonical, under a `--root`, not the Minerva
  home's own). Without one, a `chat` gives that chat's remembered folder (`known_cwd`), and a chat with none starts in `$HOME`; an
  unknown chat is a 404. A chat's remembered folder and `$HOME` are **not** root-checked: the check bounds only where a shell is
  started, and the operator's own shell can `cd` anywhere.
* **What a chat is.** A chat is known when the perch remembers a folder for it, or when the session list has a row for it. The list
  is read only for a chat with no remembered folder, since reading it reads the logs.
* **Whose terminal.** Every terminal a route makes is the operator's (`by: operator`); `chat` is recorded when one was named. An agent's
  terminal is T2's.
* **The lane.** The route calls `Terms::input`, never `agent_input`, so the operator's hold never refuses it.
* **Content-Type.** Exactly one header, whose media type is `application/octet-stream`: parameters after `;` are allowed, the case of
  the media type is not judged. A repeated header is refused.
* **Blocking.** `text` and `resize` take a live screen's lock, so they run on a blocking thread on an `Arc<Terminal>` from `Terms::get`;
  nothing on the runtime waits for a lock. A terminal whose shell has ended but which is still listed (kept 30 minutes) answers
  resize and hold as a live one does, because `Terms` does; only input answers 409.

## The stream (T1c, `stream.rs` and `snap.rs`)

`GET /hub/term/events?since=t1:4096,t3:0` is the one SSE for every terminal, behind the same gate as the other routes. A non-GET is a
405 with `Allow: GET`. A `since` that is not a comma list of `<id>:<seq>` (a valid id, a decimal seq), or that is given twice, is a 400
before the stream starts. Every entry is checked, including one past the `MAX_TERMS` (16) that are kept, so a malformed entry anywhere is
a 400. `since` is the page's seq per terminal (the first seq for a terminal named twice); a terminal it does not name joins with a reset.

```text
  GET /hub/term/events?since=..
  |
  +- term-list                          the roster, always first
  +- per listed terminal, in roster order
  |    resume  named seq the ring still holds, not ahead of the terminal   --> term-out from that seq
  |    reset   everything else                                             --> term-reset {snap}
  |                                       (then term-out chunks when it is over 32 KiB)
  +- live, from the subscription taken before the list
  |    term-out                 on a tick: a flush at most every 16 ms, at most 32 KiB raw a frame
  |    term-open / term-size / term-title / term-hold / term-exit / term-closed / term-input   (each flushes the output before it, at once)
  +- ": keep-alive" after 15 s of silence (knob term_keepalive=N, in ms, clamped to 10..=60000)
  +- hub-stream-end {reason: "stopping"}  once, when the perch stops
```

Each frame is one `data: {json}` line. Every frame has a string `type`; the terminal frames have a string `id`; output is base64.

| type | fields | when |
|---|---|---|
| `term-list` | `terms` (the `Info` rows) | first, and again on a relist after the subscription lagged |
| `term-reset` | `id`, `seq`, `data`, `snap` | a join that is not a resume. `snap` false: the ring still starts at byte 0, so `data` is all the output and `seq` is its length. `snap` true: the screen is rebuilt (`snap::encode`), and `seq` is the ring's end less the UTF-8 character the guard still holds (`Guard::lag`), which follows as live output. An escape the parser is in the middle of is not counted (see the residuals) |
| `term-out` | `id`, `seq`, `data` | output. `seq` is where the page is after the frame; a reset's chunks carry the reset's `seq` |
| `term-open` | `id`, `term` | a terminal the stream does not track yet |
| `term-size` | `id`, `cols`, `rows` | a resize |
| `term-title` | `id`, `title`, `fg` | a title change |
| `term-hold` | `id`, `hold` | a hold change |
| `term-exit` | `id`, `code`, `signal` | the shell ended |
| `term-closed` | `id` | a close, or a relist that no longer lists the terminal |
| `term-input` | `id`, `by` (`"agent"`), `chat`, `text` | an agent's input, as the frame shows it (`stream.rs`, `glyphs`). The change carries what was typed as pictures (`typed_text`: C0 controls at U+2400 plus their code, DEL at U+2421, what the screen would not show left out, cut at 80 characters). The frame turns those into what a person reads: CR and LF are `⏎`, tab is `⇥`, DEL is `^?`, and another control is `^` and its letter (ESC is `^[`, ^C is `^C`); the frame is cut at 200 characters with an ellipsis (U+2026) |
| `hub-stream-end` | `reason` (`"stopping"`) | the perch stops; exactly once |

**Agent-supplied strings are text, never HTML.** The open's title, the `term-input` text and `fg` (and every terminal's title) come from a
program or an agent, so the operator's page shows them as text and never as HTML: no `innerHTML`, no markup built from them. T4a's review
checks the page keeps to this.

**Snapshots.** `snap::picture` copies the visible cells, the cursor, the modes and the scroll region under the screen lock; `snap::encode`
writes the bytes after the lock is dropped (inside `spawn_blocking`). The bytes are RIS, the alternate screen when it is on, the DECSET
and ANSI modes a TUI uses (application cursor, autowrap, the mouse modes, focus, alternate scroll, bracketed paste, cursor visibility,
LNM and IRM), the scroll region when there is one, then each row by CUP with SGR runs (an underline's style as `4:n` and its colour as
`58`, both as alacritty 0.26 keeps them on the cell), then origin mode when it is on, and the cursor last. Origin mode comes last
because, with it on, a row is addressed from the region's top; setting it also homes the cursor. alacritty keeps the mouse modes
mutually exclusive, so the one that is set is the one written.

**Bounds.** A flush reads the ring from each watch's cursor in pieces of at most 32 KiB raw (`FRAME_RAW`). The per-connection queue is
32 frames (`IN_FLIGHT`); a page that stops reading holds its own cursor back, not the ring, and a cursor the ring has passed gets a
`term-reset` at the next flush. A reset over 32 KiB goes out as its `term-reset` and then `term-out` chunks at the reset's `seq`. The output tick is coalesced to 16 ms
(`COALESCE`, through `flush_due`): a burst of reads is one flush. A change event is not held back: it flushes the output before it at once,
so the 16 ms bound is for output ticks only.

**Residuals** (what a join does not rebuild):

* The scroll region is the guard's, not alacritty's (alacritty 0.26 keeps it private). The guard keeps the region each `CSI Pt;Pb r`
  leaves by the page's rule (xterm.js `setScrollRegion`: a bottom past the screen, or 0, is the last line; a region applies only when
  its bottom is below its top, so one that starts on or past the last line is ignored; a region as big as the screen is none), and a
  resize clears it, as alacritty's and the page's xterm do. Two places still differ from the page. alacritty has no DECSTR (vte 0.15
  does not dispatch `CSI ! p`), so the copy keeps its own region and every mode a DECSTR resets on the page (DECCKM, the keypad,
  bracketed paste, focus, origin, insert, cursor visibility), where the page clears them all; the snapshot follows the page for the
  region and the copy for the modes. `reset` sends RIS first, which clears both alike, so only a bare `CSI ! p` meets this. And the page keeps a region for each screen, while alacritty keeps one
  for both, so a snapshot of the alternate screen can carry a region the program set on the primary.
* An escape split across reads: the guard and the parser keep that escape's state, not its bytes. A snapshot taken then is the screen
  without the escape's remaining bytes, and the page resumes after them, so those bytes show as text until the next repaint.
  `Guard::lag` counts only the UTF-8 tail.
* A relist (the change channel lagged): a terminal that has left the table loses the output it wrote after the last flush, because its
  `term-closed` goes out without one. A close that is not lagged is flushed first.
* The primary screen behind the alternate one: a snapshot taken on the alternate screen is the alternate screen alone.
* History: a snapshot is the visible screen. An exact replay (`snap` false) carries the history, since it is the whole ring.
* `note_input` (T1c) only stamps and announces an agent's input that the perch does not write itself, pictured the way
  `agent_input` pictures it and cut at 80 characters. `agent_input` announces its own input, and no route calls `note_input`: the agent
  route (T2) calls `agent_input`, so one input is announced once.

## For the slices that build on this

```text
ctx.term.terms(ctx) -> &Terms                       made on first use (Host::of(ctx)); ctx.term.get() -> Option<&Terms> never makes one
Terms::open(OpenSpec{owner,cwd,cols,rows,title,chat_cap}).await -> Result<String, OpenError{Cap,ChatCap,NoChat,Cwd,Stopping,Spawn}>
Terms::list() -> Vec<Info>  info(id)  get(id) -> Option<Arc<Terminal>>  (None for an ended-from-file one)
Terms::input(id, &[u8])                -> Result<(), InputError{NotFound,TooBig,Ended,Full}>   the operator's; T1b: 409 Ended, 413 TooBig, 503 Full
Terms::agent_input(id, chat, &[u8])    -> Result<(), InputError{..., Held}>                    an agent's; stamps, emits Change::Input
Terms::resize(id, cols, rows) -> Result<(u16,u16), Missing{NotFound,Ended}>   set_hold(id, bool) -> Result<bool, Missing>
Terms::text(id, lines, max_chars) -> Option<Text{text,cursor,alt_screen,seq,truncated}>        (blocking for a live terminal)
Terms::close(id) -> Option<Hangup>                  the operator's, unchecked; Hangup::wait().await
Terms::close_as_agent(chat, id) -> Result<Hangup, CloseError{NotFound,NotYours,Held}>   an agent's: the owner and the hold, under the lock
Terms::ticks() -> watch::Receiver<u64>              any output or change; coalesces
Terms::subscribe() -> broadcast::Receiver<Change>   Open Size Title Hold Exit Closed Input{id,chat,text}; lag means list again
Terms::emit(Change)                                 T1c's note_input
Terminal::view(|emu, ring| ..)                      screen and ring at the same byte (T1c's snapshot; blocking; do not call back into the terminal)
Terminal::read(from_seq, max) -> Read{Data, Behind{start}}   Ring::start()/end()/contents()      seq = bytes ever written
Terminal::{id, owner, is_alive, exited, seq, hold, fg, title, info, record, record_until, note_agent}
Owner::is_agents_of(chat)                           AGENT_CHAT_CAP
screen::{text_of, saved_text, text_of_saved, clean_title, shown, clamp_size, tail_chars, tail_bytes}   pty::{live_members, end_session}
```

`Host` is what the terminals need of the perch (`spawner`, `stop`, `ring`, `state_file`, `env`); a test builds one by hand and calls
`Terms::new(host)` without a `Ctx`. A test of a route that wants a shell uses `/bin/sh` through `Host.env` (`SHELL`).

## Tests

`cargo test --features test-bins --test term` (`perch/tests/term.rs`, T1b) runs the operator's routes through the perch binary, over
a real `/bin/sh`, and stops the perch at the end of each test so no shell outlives it.

`cargo test --lib term::`: unit tests for the ring, the queue and its lanes and hold, the pace, `/proc` parsing, the signal guards (a
process of another session is left alone; a foreground group no member is in is left alone), the thread-start failure, the title pace
and the environment (`pty`); for every reply, the guard (strings of every kind, unbounded bodies, REP, marks, hyperlinks, C1) and
the width table against the emulator (`screen`); for `terms.json` (size, links, pipes, set-aside cleanup, every invisible class) and
`term::e2e`, which opens real `/bin/sh` shells on real PTYs and checks what a person would: typing, resize, close sweeping a background
job and a subshell's orphan, the DSR answer, what is *not* answered, the 17th terminal, the chat cap and its evictions, the
environment, the shell's descriptors, backpressure, the hold's flush, titles, the ring, events, ids never reused, `terms.json`, a
shutdown inside 2.5 s with a shell that ignores `SIGHUP` and with a screen that is held, an open racing the shutdown, an open dropped
mid-fork, a dropped table, and the last output of a shell being there when it is reported ended. Each e2e rig kills anything still
working in its temporary home when it is dropped, so no test leaves a shell behind.

`cargo test --features test-bins --test term_stream` (`perch/tests/term_stream.rs`, T1c) runs the stream through the perch binary over a
real `/bin/sh`: the roster first, a join that resumes from a seq and one that rebuilds a screen whose ring has moved on (DECCKM kept, and the
scroll region the shell set),
a since the ring has passed is a snapshot and one it holds resumes from that seq, a live client the ring passes is reset with a
snapshot at the shell's end (a 16 MiB flood the page does not read), a lagged change channel is listed again and a terminal that left
is closed (16 MiB unread, then 300 changes), a burst of 2000 one-byte writes is flushed in a few frames, a pager (`less`, skipped when it
is not installed) is rebuilt as the screen it drew, a live flush and a replay cut at 32 KiB (the replay's chunks at its seq, the same
bytes as a resume from byte 0), opens, closes, size, title, hold and exit announced, the keep-alive knob, a shape check on every frame,
the stop that ends the stream within 3 s with `hub-stream-end`, and the refusals (401, 405 with `Allow`, 400 for a bad `since`). The
unit tests `term::stream` and `term::snap` cover the `since` grammar (a bad entry past the 16th included), the base64, the pictures,
the coalescing's due time, the snapshot's modes and colours, the scroll region (a round trip, RIS, DECSTR and a bare `CSI r` clearing
it, origin mode inside one, a region the page ignores), the underline's style and colour, and the `term-input` frame that `note_input`
announces (an agent's input only; the operator's typing is not an event). `term::screen` covers how the guard keeps the region as the
page takes each sequence, and that a resize starts it over. The frame is pinned here; T2's `term_agent.rs` pins the route that calls
`agent_input`, and the `term-input` frame it puts on the stream.

Limits worth knowing: an exited terminal's session is swept when it is closed or expires, not the moment its shell exits (the zombie
shell holds the session id until then); `fg` is the foreground group's `comm` and is empty for an ended terminal.

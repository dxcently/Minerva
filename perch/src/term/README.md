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
| `Terms::close(id)` | 2 s | removed from the table at once; the session goes in the background (`Hangup::wait` to wait) |
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
  are dropped (below). The perch's own tokens are in memory and in 0600 files, not in its environment.

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
| XTVERSION, DECRQSS | nothing, ever (alacritty does not ask; neither is in the list for the page to swallow either) |
| kitty keyboard query (`CSI ? u`) | nothing (alacritty answers it only with `kitty_keyboard` on, which this config leaves off) |
| everything else (`Title`, `Bell`, `Wakeup`, `Exit`, ...) | nothing |

Replies queue behind a 4 KiB budget of their own and go to the shell before typed input; a reply that does not fit is dropped.
Colour queries see the **default theme**, not the page's current one (the perch cannot know it).

**For T4a (the page's swallow list).** The perch is the only answerer; the page's xterm must swallow exactly the queries the perch
answers and answer none of them, or the shell reads two answers. That list is: **DA1** (`CSI c`), **DA2** (`CSI > c`), **DSR** `CSI 5 n`
and `CSI 6 n`, **DECRQM** (`CSI Ps $ p` and `CSI ? Ps $ p`), **`CSI 18 t`**, and **OSC 4, 10, 11 and 12** with a `?`. **DECRQSS**
(`DCS $ q`) and **XTVERSION** (`CSI > 0 q`) are never answered by either side. Nothing else is on the list.

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

## Chats and the cap (D5)

An agent's terminal is for a chat: `OpenSpec` with `owner.by == Agent` and no chat is `OpenError::NoChat`. A chat holds at most
`chat_cap` terminals (`AGENT_CHAT_CAP` = 4 when the caller names none), counted under the table's lock together with the table's
cap, so two opens at once cannot both pass. An agent terminal whose shell has ended but which was not closed **counts**, since it
keeps its place for 30 minutes; when the chat (or the full table) wants another, its own oldest ended terminal is closed to make room
(`Change::Closed`), and only when it has none is the open refused (`ChatCap`, or `Cap` for the table). `Owner::is_agents_of(chat)` is
the one test of "this is that chat's agent's". An operator's terminal that sits beside a chat is not an agent's.

## Bounds

| thing | bound | where |
|---|---|---|
| terminals | 16, ended ones included | `MAX_TERMS`; `OpenError::Cap` |
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

## For the slices that build on this

```text
ctx.term.terms(ctx) -> &Terms                       made on first use (Host::of(ctx)); ctx.term.get() -> Option<&Terms> never makes one
Terms::open(OpenSpec{owner,cwd,cols,rows,title,chat_cap}).await -> Result<String, OpenError{Cap,ChatCap,NoChat,Cwd,Stopping,Spawn}>
Terms::list() -> Vec<Info>  info(id)  get(id) -> Option<Arc<Terminal>>  (None for an ended-from-file one)
Terms::input(id, &[u8])                -> Result<(), InputError{NotFound,TooBig,Ended,Full}>   the operator's; T1b: 409 Ended, 413 TooBig, 503 Full
Terms::agent_input(id, chat, &[u8])    -> Result<(), InputError{..., Held}>                    an agent's; stamps, emits Change::Input
Terms::resize(id, cols, rows) -> Result<(u16,u16), Missing{NotFound,Ended}>   set_hold(id, bool) -> Result<bool, Missing>
Terms::text(id, lines, max_chars) -> Option<Text{text,cursor,alt_screen,seq,truncated}>        (blocking for a live terminal)
Terms::close(id) -> Option<Hangup>                  Hangup::wait().await
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

Limits worth knowing: an exited terminal's session is swept when it is closed or expires, not the moment its shell exits (the zombie
shell holds the session id until then); `fg` is the foreground group's `comm` and is empty for an ended terminal.

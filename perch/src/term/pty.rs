//! The PTY host (slice T1a): the PTY and the shell on the spawner thread, the reader, writer and waiter threads, the ring, close, and
//! the shutdown of every terminal. No route here.
//!
//! ```text
//!   spawner thread   posix_openpt (close-on-exec from the first instant) + the login shell, a job on spawn::Spawner so it never
//!                    interleaves with a door's fork
//!   reader           poll + read the master -> screen (alacritty) and ring, under the screen's lock -> tick
//!   writer           the queue (replies, the operator's input, the agent's input) -> the master
//!   waiter           waitid(WNOWAIT) -> the exit, once the reader has the last of the output; reaps only when the terminal is ended
//! ```
//!
//! **One shell, one session.** The shell is made a session and process-group leader (`setsid` in the child, the slave as its
//! controlling terminal), so its pid is the terminal's session id. Ending a terminal is `SIGHUP` to that group, to the foreground
//! group (only if a process of the session is in it) and to every other process of the session (a job the shell put in a group of its
//! own), a wait, then a sweep of `/proc/*/stat` for every process whose session is that id (jobs, background children, a `nohup`,
//! whatever ignored the hang-up) with `SIGKILL`. Each process is signalled through a pidfd opened *before* its session is read again,
//! so a pid that was handed to a stranger since the listing is not signalled. What calls `setsid` itself leaves the session and is not
//! found: that is the one way out, and it is the documented one. The shell is reaped last, never earlier, so its pid cannot be handed
//! to a stranger while the session id is still being signalled.
//!
//! **Threads and descriptors.** The master is used through its raw descriptor, non-blocking, with `poll` and a 100 ms tick, so every
//! thread notices the end of its terminal and none outlives it, even when something that escaped the session still holds the slave.
//! The PTY is opened here, not by a library: `posix_openpt` with `O_CLOEXEC`, the slave opened `O_CLOEXEC` too, so there is no moment
//! in which a fork elsewhere in the perch could carry the master into another program (the shell gets its slave as 0, 1 and 2 and
//! every other descriptor closed).
//!
//! **Locks.** Where two are held: the screen's, then the ring's. The reader holds both while it feeds a read to the screen and appends
//! it to the ring, so whoever holds both sees the two at the same byte. **Every method that takes the screen's lock blocks** until
//! the reader has parsed the read it is in (a few milliseconds at most): [`Terminal::view`], [`Terminal::text`],
//! [`Terminal::record`] and [`Terminal::resize`] are for threads that can wait, not for an async worker (go through `spawn_blocking`),
//! and [`Terminal::record_until`] is the one with a deadline.

use std::collections::VecDeque;
use std::ffi::{CStr, OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use super::screen::{self, Emu, Screen, Sink, Text};
use super::{Change, Exit, Hub, Info, InputError, Owner, Record, lock, now_ms};
use crate::serve::Ctx;

/// The default size of a terminal's ring of raw output (`--knob term_ring=N` changes it, within [`RING_MIN`]..=[`RING_MAX`]).
pub const RING_BYTES: usize = 512 * 1024;
pub const RING_MIN: usize = 1024;
pub const RING_MAX: usize = 8 * 1024 * 1024;
/// What one read of the master takes.
const READ_BUF: usize = 16 * 1024;
/// How long the reader and the writer wait on the master before looking at whether their terminal has ended.
const POLL_MS: i32 = 100;
/// Input queued for the shell, for each of the two who type (the operator, an agent), and replies queued for it. A full lane refuses
/// input and drops replies; nothing waits. The lanes are apart so that an agent that fills its own cannot keep the operator's `^C` out.
pub const INPUT_QUEUE: usize = 64 * 1024;
pub const REPLY_QUEUE: usize = 4 * 1024;
/// One write to the master.
const WRITE_CHUNK: usize = 4096;
/// The most of an agent's bytes the writer lets sit unread in the PTY's input queue; the rest waits in the agent's lane, where the
/// operator's input goes ahead of it. Small against the queue's 4 KiB, so that a `^C` always has room.
const AGENT_AHEAD: usize = 1024;
/// The most of an agent's bytes one write puts in, and how long the writer then waits for them to show in the input queue before it
/// looks at the queue again. A write to the master lands in the kernel's buffer first and reaches the input queue a moment later (on
/// another thread, which a busy machine can delay); counting only the queue, the writer would see it empty and write the next one
/// into the buffer, and so on until the buffer was full and the input queue behind it, with a `^C` stuck at the back of both.
const AGENT_STEP: usize = 512;
const SETTLE_TRIES: u32 = 16;
const SETTLE_STEP: Duration = Duration::from_micros(250);
/// Output faster than this is slowed (see [`Throttle`]): a program that prints without end must not be what the perch spends itself on.
const THROTTLE_WINDOW: Duration = Duration::from_millis(100);
const THROTTLE_BYTES: usize = 8 * 1024 * 1024 / 10;
const THROTTLE_PAUSE: Duration = Duration::from_millis(10);
/// How often an ending terminal looks for what is left of its session, and how long it waits for the last of it after `SIGKILL`.
const SWEEP_TICK: Duration = Duration::from_millis(25);
const KILL_CONFIRM: Duration = Duration::from_millis(400);
/// How long the waiter gives the reader, after the shell ended, to take the last of what it wrote (the reader ends on the master's
/// `EIO` or end of file, which comes only once nothing holds the slave any more; a background job that does keeps it waiting).
const READER_WAIT: Duration = Duration::from_millis(250);
/// A terminal's title change is announced at most this often (the newest title wins).
const TITLE_EVERY: Duration = Duration::from_millis(100);
/// The most reads the reader takes after the terminal is ended, to have the last of the session's output.
const FINAL_READS: usize = 64;

// ---- the ring ----------------------------------------------------------------------------------------------------------------

/// The last bytes a terminal wrote, each with its place: `seq` is the byte's offset from the first byte ever written, so a page that
/// has seen up to `seq` asks for exactly what follows, and the ring says whether that is still in it.
pub struct Ring {
    buf: VecDeque<u8>,
    cap: usize,
    end: u64,
}

/// What [`Ring::read`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Read {
    /// The bytes from the asked seq (up to the limit); empty when the asker is at the end.
    Data(Vec<u8>),
    /// The asked seq is older than the ring: the bytes are gone, and the ring starts here.
    Behind { start: u64 },
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        Self { buf: VecDeque::new(), cap: cap.max(1), end: 0 }
    }

    pub fn push(&mut self, bytes: &[u8]) {
        self.end += bytes.len() as u64;
        let bytes = &bytes[bytes.len().saturating_sub(self.cap)..];
        self.buf.extend(bytes);
        let over = self.buf.len().saturating_sub(self.cap);
        if over > 0 {
            self.buf.drain(..over);
        }
    }

    /// The seq of the first byte still held; `0` for as long as nothing has been dropped (an exact replay is possible).
    pub fn start(&self) -> u64 {
        self.end - self.buf.len() as u64
    }

    /// The seq the next byte will have, which is also how many bytes have ever been written.
    pub fn end(&self) -> u64 {
        self.end
    }

    pub fn read(&self, from: u64, max: usize) -> Read {
        let start = self.start();
        if from < start {
            return Read::Behind { start };
        }
        let skip = usize::try_from(from - start).unwrap_or(usize::MAX);
        Read::Data(self.buf.iter().skip(skip).take(max).copied().collect())
    }

    /// Everything held.
    pub fn contents(&self) -> Vec<u8> {
        self.buf.iter().copied().collect()
    }
}

// ---- the writer's queue ------------------------------------------------------------------------------------------------------

/// What a terminal is to be told, in three lanes so that none starves another: the screen's replies (up to [`REPLY_QUEUE`] bytes),
/// the operator's input and an agent's input (each up to [`INPUT_QUEUE`]). The writer takes them in that order, so a reply and the
/// operator's `^C` go ahead of an agent's backlog.
///
/// **The hold** is the operator's stop for the agent, and it lives here so that it is decided where the bytes are. While it is on,
/// [`WriteQueue::agent_input`] refuses with [`InputError::Held`]; turning it on drops what the agent has queued
/// ([`WriteQueue::drop_input`]); and the writer looks at it under the queue's lock for every write of an agent's bytes, so none is
/// written once it is on. What the hold does *not* reach here is in [`Terminal::set_hold`] and the README: bytes already written to
/// the PTY and not yet read by the program.
pub struct WriteQueue {
    state: Mutex<Queued>,
    ready: Condvar,
}

#[derive(Default)]
struct Queued {
    replies: VecDeque<u8>,
    operator: VecDeque<u8>,
    agent: VecDeque<u8>,
    held: bool,
    closed: bool,
}

/// Which of the queue's three lanes a piece came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Replies,
    Operator,
    Agent,
}

/// A piece of the queue taken for the writer, and the lane it came from (the agent's is the one the hold can still stop).
#[derive(Debug, PartialEq, Eq)]
pub struct Chunk {
    pub bytes: Vec<u8>,
    pub lane: Lane,
}

impl Default for WriteQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl WriteQueue {
    pub fn new() -> Self {
        Self { state: Mutex::new(Queued::default()), ready: Condvar::new() }
    }

    /// Queue the operator's input, all of it or none of it. The hold does not apply: the operator can always type.
    pub fn input(&self, bytes: &[u8]) -> Result<(), InputError> {
        let mut queued = lock(&self.state);
        if queued.closed {
            return Err(InputError::Ended);
        }
        if queued.operator.len() + bytes.len() > INPUT_QUEUE {
            return Err(InputError::Full);
        }
        queued.operator.extend(bytes);
        self.ready.notify_one();
        Ok(())
    }

    /// Queue an agent's input, all of it or none of it; refused while the hold is on. The hold is read under the lock that queues the
    /// bytes, so an agent's input cannot get in behind a hold that was set a moment before.
    pub fn agent_input(&self, bytes: &[u8]) -> Result<(), InputError> {
        let mut queued = lock(&self.state);
        if queued.closed {
            return Err(InputError::Ended);
        }
        if queued.held {
            return Err(InputError::Held);
        }
        if queued.agent.len() + bytes.len() > INPUT_QUEUE {
            return Err(InputError::Full);
        }
        queued.agent.extend(bytes);
        self.ready.notify_one();
        Ok(())
    }

    /// Queue a reply; `false` when it was dropped (the queue is full or closed).
    pub fn reply(&self, bytes: &[u8]) -> bool {
        let mut queued = lock(&self.state);
        if queued.closed || queued.replies.len() + bytes.len() > REPLY_QUEUE {
            return false;
        }
        queued.replies.extend(bytes);
        self.ready.notify_one();
        true
    }

    /// Turn the hold on or off; whether it changed. Turning it on drops the agent's queued input.
    pub fn set_hold(&self, hold: bool) -> bool {
        let mut queued = lock(&self.state);
        let changed = queued.held != hold;
        queued.held = hold;
        if hold {
            queued.agent.clear();
        }
        changed
    }

    pub fn held(&self) -> bool {
        lock(&self.state).held
    }

    /// Drop what an agent has queued and not yet had written; how many bytes that was.
    pub fn drop_input(&self) -> usize {
        let mut queued = lock(&self.state);
        let dropped = queued.agent.len();
        queued.agent.clear();
        dropped
    }

    /// Nothing more goes in, and what is queued is never written. Wakes the writer.
    pub fn close(&self) {
        lock(&self.state).closed = true;
        self.ready.notify_all();
    }

    /// The next bytes to write (at most `max`), waiting for them; `None` once closed.
    fn next(&self, max: usize) -> Option<Chunk> {
        let mut queued = lock(&self.state);
        loop {
            if queued.closed {
                return None;
            }
            // Replies first: they are a few bytes, and a program is waiting for each. Then the operator, then the agent.
            let (from, lane) = if !queued.replies.is_empty() {
                (&mut queued.replies, Lane::Replies)
            } else if !queued.operator.is_empty() {
                (&mut queued.operator, Lane::Operator)
            } else {
                (&mut queued.agent, Lane::Agent)
            };
            if !from.is_empty() {
                let n = from.len().min(max);
                return Some(Chunk { bytes: from.drain(..n).collect(), lane });
            }
            queued = self.ready.wait(queued).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    /// One `write` of the first `allow` bytes of `chunk`. An agent's bytes are written with the queue's lock held, the hold looked at
    /// in the same breath: `None` is "the hold is on, drop the rest". A hold set after this returns finds the bytes written, and the
    /// flush that goes with it ([`Terminal::set_hold`]) takes them back out of the PTY if the program has not read them.
    fn write(&self, fd: RawFd, chunk: &Chunk, allow: usize) -> Option<io::Result<usize>> {
        let bytes = &chunk.bytes[..allow.min(chunk.bytes.len())];
        if chunk.lane != Lane::Agent {
            return Some(write_fd(fd, bytes));
        }
        let queued = lock(&self.state);
        if queued.held {
            return None;
        }
        let wrote = write_fd(fd, bytes);
        drop(queued);
        Some(wrote)
    }

    /// Put back what a write did not take (`written` bytes of `chunk` went), at the front of its lane, so that the next [`next`] picks
    /// again by priority: an operator's `^C` queued meanwhile goes ahead of the rest of an agent's chunk that a full PTY would not
    /// take. An agent's rest is dropped instead if the hold is on, and everything is if the queue is closed.
    fn give_back(&self, chunk: Chunk, written: usize) {
        let rest = &chunk.bytes[written.min(chunk.bytes.len())..];
        if rest.is_empty() {
            return;
        }
        let mut queued = lock(&self.state);
        if queued.closed {
            return;
        }
        let lane = match chunk.lane {
            Lane::Replies => &mut queued.replies,
            Lane::Operator => &mut queued.operator,
            Lane::Agent if queued.held => return,
            Lane::Agent => &mut queued.agent,
        };
        for byte in rest.iter().rev() {
            lane.push_front(*byte);
        }
    }
}

// ---- the reader's pace -------------------------------------------------------------------------------------------------------

/// Slows output above about 8 MiB/s: once a 100 ms window has taken more than its share, every read after it is followed by a 10 ms
/// sleep until the window turns over. The program on the other end blocks on a full PTY, which is the point.
#[derive(Default)]
pub struct Throttle {
    window: Option<Instant>,
    bytes: usize,
}

impl Throttle {
    /// How long to sleep after a read of `n` bytes at `now`.
    pub fn after(&mut self, now: Instant, n: usize) -> Duration {
        match self.window {
            Some(start) if now.saturating_duration_since(start) < THROTTLE_WINDOW => self.bytes += n,
            _ => {
                self.window = Some(now);
                self.bytes = n;
            }
        }
        if self.bytes > THROTTLE_BYTES { THROTTLE_PAUSE } else { Duration::ZERO }
    }
}

// ---- the session ------------------------------------------------------------------------------------------------------------

/// One row of `/proc/<pid>/stat` the sweep cares about: the state letter, the process group and the session id.
fn parse_stat(text: &str) -> Option<(char, i32, i32)> {
    // `pid (comm) state ppid pgrp session ...`, and comm may hold spaces and parentheses, so the last `)` is the end of it.
    let rest = &text[text.rfind(')')? + 1..];
    let mut fields = rest.split_ascii_whitespace();
    let state = fields.next()?.chars().next()?;
    let _ppid = fields.next()?;
    let pgrp = fields.next()?.parse().ok()?;
    Some((state, pgrp, fields.next()?.parse().ok()?))
}

fn read_stat(pid: i32) -> Option<(char, i32, i32)> {
    parse_stat(&std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?)
}

/// Whether `pid` is a process of session `sid` that is not already dead (a zombie waits only to be reaped).
fn member_of(pid: i32, sid: i32) -> bool {
    read_stat(pid).is_some_and(|(state, _, session)| session == sid && !matches!(state, 'Z' | 'X'))
}

/// The processes of session `sid` that are not already dead, each with its process group.
fn session_members(sid: i32) -> Vec<(i32, i32)> {
    // Session 0 is the kernel's threads and session 1 is init's: never a terminal's, and never something to find and signal.
    if !signalable(sid) {
        return Vec::new();
    }
    let Ok(dir) = std::fs::read_dir("/proc") else { return Vec::new() };
    let mut found = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|n| n.parse::<i32>().ok()) else { continue };
        if let Some((state, pgrp, session)) = read_stat(pid)
            && session == sid
            && !matches!(state, 'Z' | 'X')
        {
            found.push((pid, pgrp));
        }
    }
    found
}

/// The pids of [`session_members`].
pub fn live_members(sid: i32) -> Vec<i32> {
    session_members(sid).into_iter().map(|(pid, _)| pid).collect()
}

/// A pid the perch may signal: a real process that is neither init, nor this process, nor this process's group. `kill(0)`, `kill(-1)` and
/// `killpg(0)` would reach everything or the perch itself, so nothing that is not a plain positive pid gets through here.
fn signalable(pid: i32) -> bool {
    pid > 1 && pid != std::process::id() as i32 && pid != unsafe { libc::getpgrp() }
}

/// Signal a process group. Only for a group that is known to be the terminal's: its own (the shell leads it, and a leader is not
/// reaped until the terminal is gone, so its number is not handed to anyone else) or one a member of the session was found in.
fn signal_group(pgid: i32, signal: i32) {
    if signalable(pgid) {
        unsafe { libc::killpg(pgid, signal) };
    }
}

/// Signal one process, if it is in session `sid`. The pidfd is opened first and the session read after: a pid that was handed to
/// someone else since it was listed is that someone's process then, in another session, and is left alone; one that died after
/// the read is a pidfd that signals nothing (`ESRCH`), whatever the number has become. Where the kernel has no pidfd (before 5.3, a
/// sandbox that refuses the call) the check and the `kill` are all there is, and the window between them is open.
fn signal_member(pid: i32, sid: i32, signal: i32) {
    if !signalable(pid) {
        return;
    }
    let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::c_long, 0 as libc::c_long) };
    if pidfd < 0 {
        if member_of(pid, sid) {
            unsafe { libc::kill(pid, signal) };
        }
        return;
    }
    let pidfd = pidfd as libc::c_int;
    if member_of(pid, sid) {
        unsafe {
            libc::syscall(libc::SYS_pidfd_send_signal, pidfd as libc::c_long, signal as libc::c_long, std::ptr::null::<libc::siginfo_t>(), 0 as libc::c_long)
        };
    }
    unsafe { libc::close(pidfd) };
}

/// `SIGKILL` for the whole session, without waiting: its own group at once (atomic against forks), then each process found.
fn kill_session(sid: i32) {
    signal_group(sid, libc::SIGKILL);
    for pid in live_members(sid) {
        signal_member(pid, sid, libc::SIGKILL);
    }
}

/// The foreground process group `group` of the terminal of session `sid` as the comm of its leader can be read: a leader that is a
/// process of this session, or failing that any process of it that is in the group. The kernel's idea of the foreground group can be
/// a number whose processes are gone and which now belongs to a stranger, so it is a group only if a process of the session says so.
fn fg_leader(sid: i32, group: i32) -> Option<i32> {
    if group > 1 && member_of(group, sid) && read_stat(group).is_some_and(|(_, pgrp, _)| pgrp == group) {
        return Some(group);
    }
    session_members(sid).into_iter().find(|(_, pgrp)| *pgrp == group).map(|(pid, _)| pid)
}

/// End session `sid`: hang up its leader's group, the foreground group (if a process of the session is in it) and every other process
/// of the session, give what is in it `grace` to go, then `SIGKILL` whatever is left and wait a moment for it to be gone. Returns
/// early the moment nothing is left. Blocking.
pub fn end_session(sid: i32, foreground: Option<i32>, grace: Duration) {
    // The foreground group is what the terminal says; it is signalled as a group only when the same listing that finds the session's
    // processes has one of them in it (see `fg_leader`).
    let members = session_members(sid);
    let foreground = foreground.filter(|group| *group != sid && members.iter().any(|(_, in_group)| in_group == group));
    let groups: Vec<i32> = [Some(sid), foreground].into_iter().flatten().collect();
    // SIGCONT after SIGHUP: a stopped job gets its hang-up only when it runs again.
    for group in &groups {
        signal_group(*group, libc::SIGHUP);
        signal_group(*group, libc::SIGCONT);
    }
    // What a shell with job control started with `&` is in a process group of its own, in the session but in neither group above: it is
    // hung up one by one, as a terminal that closes does it, so that it goes now and not at the `SIGKILL` after the grace.
    for (pid, group) in members {
        if !groups.contains(&group) {
            signal_member(pid, sid, libc::SIGHUP);
            signal_member(pid, sid, libc::SIGCONT);
        }
    }
    let until = Instant::now() + grace;
    while !live_members(sid).is_empty() {
        if Instant::now() >= until {
            let confirm = Instant::now() + KILL_CONFIRM;
            loop {
                kill_session(sid);
                if live_members(sid).is_empty() || Instant::now() >= confirm {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        std::thread::sleep(SWEEP_TICK);
    }
}

// ---- the environment ---------------------------------------------------------------------------------------------------------

/// The environment of a shell: the perch's own, less the perch's private names, plus what a terminal says about itself.
///
/// Nothing here adds a credential, and the perch's own tokens are not in its environment to begin with (they are in memory and in
/// 0600 files). The `MINERVA_*` names that look like one are dropped anyway, so that a future change which puts one in the perch's
/// environment does not hand it to every shell: a name is private when it starts `MINERVA_` and says `TOKEN`, `SECRET`, `CODE` or
/// `PASS`. `MINERVA_TERM` is set last, to the terminal's id.
///
/// What is *not* dropped is everything else the perch was started with: `SSH_AUTH_SOCK`, a `*_API_KEY`, cloud credentials. The shell
/// is the operator's, in the operator's session, and has them as the operator's own login would; the README lists this as a residual.
pub fn child_env(base: &[(OsString, OsString)], id: &str) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = base.iter().filter(|(name, _)| !private_name(name)).cloned().collect();
    let mut set = |name: &str, value: &str| {
        env.retain(|(k, _)| k != name);
        env.push((name.into(), value.into()));
    };
    set("TERM", "xterm-256color");
    set("COLORTERM", "truecolor");
    set("MINERVA_TERM", id);
    if !env.iter().any(|(k, _)| k == "LANG") {
        env.push(("LANG".into(), "C.UTF-8".into()));
    }
    env
}

fn private_name(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    name == "MINERVA_TERM" || (name.starts_with("MINERVA_") && ["TOKEN", "SECRET", "CODE", "PASS"].iter().any(|word| name.contains(word)))
}

// ---- the PTY and its shell ---------------------------------------------------------------------------------------------------

/// A shell on a PTY, between the spawner thread and a [`Terminal`] that takes it over. Dropped before that happens (the request that
/// asked for it went away), it kills the session: a shell nobody owns must not be left running.
pub struct Spawned {
    master: Option<OwnedFd>,
    child: Option<Child>,
    pid: i32,
    tty: PathBuf,
}

impl Drop for Spawned {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // The shell itself first (its pid is ours and unreaped, so this cannot reach a stranger), then what it started.
            let _ = child.kill();
            kill_session(self.pid);
            let _ = child.wait();
        }
    }
}

fn last_os_error<T>() -> io::Result<T> {
    Err(io::Error::last_os_error())
}

fn set_winsize(fd: RawFd, cols: u16, rows: u16) -> io::Result<()> {
    let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ as _, &size) } < 0 { last_os_error() } else { Ok(()) }
}

/// A new PTY at `cols` x `rows`: the master, the slave, and the slave's name (to flush its input by name, see [`Master`]). Both are
/// close-on-exec from the call that makes them, so the shell's `exec` closes them, and so does any other program a fork elsewhere in
/// the perch starts at the same moment.
fn open_pty(cols: u16, rows: u16) -> io::Result<(OwnedFd, OwnedFd, PathBuf)> {
    let raw = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if raw < 0 {
        return last_os_error();
    }
    let master = unsafe { OwnedFd::from_raw_fd(raw) };
    if unsafe { libc::grantpt(raw) } != 0 || unsafe { libc::unlockpt(raw) } != 0 {
        return last_os_error();
    }
    let mut name = [0 as libc::c_char; 128];
    let failed = unsafe { libc::ptsname_r(raw, name.as_mut_ptr(), name.len()) };
    if failed != 0 {
        return Err(io::Error::from_raw_os_error(failed));
    }
    set_winsize(raw, cols, rows)?;
    let slave = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) };
    if slave < 0 {
        return last_os_error();
    }
    let slave = unsafe { OwnedFd::from_raw_fd(slave) };
    let tty = PathBuf::from(OsString::from_vec(unsafe { CStr::from_ptr(name.as_ptr()) }.to_bytes().to_vec()));
    Ok((master, slave, tty))
}

/// The shell: `$SHELL` of `env` if it names an executable file, else the passwd entry's, else `/bin/sh`.
fn shell_of(env: &[(OsString, OsString)]) -> PathBuf {
    let usable = |path: &Path| path.is_absolute() && std::fs::metadata(path).is_ok_and(|m| m.is_file());
    if let Some((_, shell)) = env.iter().find(|(name, _)| name == "SHELL") {
        let shell = PathBuf::from(shell);
        if usable(&shell) {
            return shell;
        }
    }
    let mut buf = vec![0u8; 4096];
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe { libc::getpwuid_r(libc::getuid(), &mut entry, buf.as_mut_ptr().cast(), buf.len(), &mut found) };
    if rc == 0 && !found.is_null() && !entry.pw_shell.is_null() {
        let shell = PathBuf::from(OsString::from_vec(unsafe { CStr::from_ptr(entry.pw_shell) }.to_bytes().to_vec()));
        if usable(&shell) {
            return shell;
        }
    }
    PathBuf::from("/bin/sh")
}

/// A PTY and a login shell on it, to be run on the spawner thread (`Spawner::run`): the shell is `$SHELL` of `env` (see
/// [`shell_of`]), started as a login shell (its name with a `-` in front) in `cwd` as a session leader with the slave as its
/// controlling terminal. Nothing but `env` reaches it, and nothing but descriptors 0, 1 and 2.
pub fn spawn(cols: u16, rows: u16, cwd: &Path, env: &[(OsString, OsString)]) -> io::Result<Spawned> {
    let (master, slave, tty) = open_pty(cols, rows)?;
    let shell = shell_of(env);
    let name = shell.file_name().map_or_else(|| "sh".to_string(), |n| n.to_string_lossy().into_owned());
    let mut command = Command::new(&shell);
    command.arg0(format!("-{name}")).env_clear().envs(env.iter().map(|(name, value)| (name, value))).current_dir(cwd);
    command.stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave));
    // SAFETY: the closure runs in the forked child before `exec` and calls only functions that are safe there (`setsid`, `ioctl`,
    // `signal`, `pthread_sigmask`, `syscall`), allocating nothing.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return last_os_error();
            }
            // The slave, which `Stdio` has put on 0, is the session's controlling terminal.
            if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return last_os_error();
            }
            // What the perch ignores or blocks is not the shell's to inherit.
            for signal in [
                libc::SIGHUP,
                libc::SIGINT,
                libc::SIGQUIT,
                libc::SIGTERM,
                libc::SIGALRM,
                libc::SIGCHLD,
                libc::SIGPIPE,
                libc::SIGTSTP,
                libc::SIGTTIN,
                libc::SIGTTOU,
            ] {
                libc::signal(signal, libc::SIG_DFL);
            }
            let mut none: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut none);
            libc::pthread_sigmask(libc::SIG_SETMASK, &none, std::ptr::null_mut());
            // Everything above stderr was close-on-exec already; this is for a descriptor some other code opened without it. The
            // flag (`CLOSE_RANGE_CLOEXEC`, Linux 5.11) marks them to close at the `exec` instead of closing them now, which would
            // close the pipe `Command` tells the parent of a failed `exec` by; a kernel without it ignores the call (`EINVAL`).
            libc::syscall(libc::SYS_close_range, 3 as libc::c_long, libc::c_uint::MAX as libc::c_long, 4 as libc::c_long);
            Ok(())
        });
    }
    let child = command.spawn()?;
    // The slave side is the shell's now; ours went with the command, so that the master sees the end when the last process holding it goes.
    drop(command);
    let pid = child.id() as i32;
    let spawned = Spawned { master: Some(master), child: Some(child), pid, tty };
    // The whole of ending a terminal rests on the shell leading its own session; a shell that does not is killed, not kept.
    if unsafe { libc::getsid(pid) } != pid || !signalable(pid) {
        return Err(io::Error::other("the shell did not become a session leader"));
    }
    let fd = spawned.master.as_ref().map(AsRawFd::as_raw_fd).ok_or_else(|| io::Error::other("the pty has no descriptor"))?;
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return last_os_error();
    }
    Ok(spawned)
}

fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}

fn write_fd(fd: RawFd, buf: &[u8]) -> io::Result<usize> {
    let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
    if n < 0 { Err(io::Error::last_os_error()) } else { Ok(n as usize) }
}

/// `poll` on one descriptor: the events that happened, `0` for the timeout.
fn wait_fd(fd: RawFd, events: i16, ms: i32) -> io::Result<i16> {
    let mut poll = libc::pollfd { fd, events, revents: 0 };
    loop {
        let found = unsafe { libc::poll(&mut poll, 1, ms) };
        if found >= 0 {
            return Ok(if found == 0 { 0 } else { poll.revents });
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Wait for the shell to end without reaping it (`WNOWAIT`): the zombie keeps the pid, and so the session id, ours.
fn wait_exit(pid: i32) -> Exit {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, libc::WEXITED | libc::WNOWAIT) } == 0 {
            break;
        }
        if io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return Exit { code: None, signal: None };
        }
    }
    let status = unsafe { info.si_status() };
    if info.si_code == libc::CLD_EXITED {
        Exit { code: Some(status), signal: None }
    } else {
        Exit { code: None, signal: Some(status) }
    }
}

// ---- the terminal ------------------------------------------------------------------------------------------------------------

/// The title as the screen last set it, whether an announcement of a newer one is waiting its turn, and when the last went out.
struct TitleState {
    text: String,
    announced: Option<Instant>,
    pending: bool,
}

/// What the screen's callbacks and the threads share: the writer's queue, the title, and the hub. It is the screen's [`Sink`], and
/// the sink is called after the screen's lock is dropped, so nothing here may assume that lock is held or free.
struct Core {
    id: String,
    base_title: String,
    title: Mutex<TitleState>,
    queue: WriteQueue,
    hub: Arc<Hub>,
}

impl Core {
    /// The title is now `title` (`None` is "back to the terminal's own"). `Change::Title` goes out only when the text differs from
    /// what it was, and at most once in [`TITLE_EVERY`]; a title that arrives sooner waits for [`Core::flush_title`], so the newest
    /// is what is announced and a program that sets one in a loop costs one event in a tenth of a second.
    fn set_title(&self, title: Option<String>) {
        let text = title.unwrap_or_else(|| self.base_title.clone());
        let announce = {
            let mut state = lock(&self.title);
            if state.text == text {
                return;
            }
            state.text = text;
            match state.announced {
                Some(at) if at.elapsed() < TITLE_EVERY => {
                    state.pending = true;
                    false
                }
                _ => {
                    state.announced = Some(Instant::now());
                    state.pending = false;
                    true
                }
            }
        };
        if announce {
            self.hub.emit(Change::Title(self.id.clone()));
        }
    }

    /// Announce a title that was held back, once its turn has come (or at once, `force`, as the reader ends).
    fn flush_title(&self, force: bool) {
        let announce = {
            let mut state = lock(&self.title);
            if !state.pending || (!force && state.announced.is_some_and(|at| at.elapsed() < TITLE_EVERY)) {
                return;
            }
            state.pending = false;
            state.announced = Some(Instant::now());
            true
        };
        if announce {
            self.hub.emit(Change::Title(self.id.clone()));
        }
    }
}

impl Sink for Core {
    fn reply(&self, bytes: &[u8]) {
        self.queue.reply(bytes);
    }

    fn title(&self, title: Option<String>) {
        self.set_title(title);
    }

    fn bell(&self) {}
}

/// The master side of the PTY, and the name of its slave.
struct Master {
    fd: OwnedFd,
    tty: PathBuf,
}

impl Master {
    fn raw(&self) -> RawFd {
        self.fd.as_raw_fd()
    }

    fn resize(&self, cols: u16, rows: u16) -> io::Result<()> {
        set_winsize(self.raw(), cols, rows)
    }

    /// The foreground process group of the terminal, when there is one.
    fn foreground(&self) -> Option<i32> {
        let group = unsafe { libc::tcgetpgrp(self.raw()) };
        (group > 0).then_some(group)
    }

    /// The slave, opened by its name for the moment (`O_NOCTTY`, so that it cannot become this process's controlling terminal). It is
    /// not kept open: a descriptor of ours on the slave would keep the master from ever seeing the end of the session.
    fn open_slave(&self) -> io::Result<OwnedFd> {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(self.tty.as_os_str().as_bytes()).map_err(io::Error::other)?;
        let slave = unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC | libc::O_NONBLOCK) };
        if slave < 0 { last_os_error() } else { Ok(unsafe { OwnedFd::from_raw_fd(slave) }) }
    }

    /// Throw away what has been written to the PTY and not yet read by the program (`tcflush(TCIFLUSH)` on the slave). The master has
    /// no such call: the input queue is the slave's.
    fn flush_input(&self) -> io::Result<()> {
        let slave = self.open_slave()?;
        if unsafe { libc::tcflush(slave.as_raw_fd(), libc::TCIFLUSH) } < 0 { last_os_error() } else { Ok(()) }
    }

    /// How many bytes sit in the PTY's input queue, written and not yet read by the program (`TIOCINQ` on the slave). In canonical
    /// mode that counts the finished lines only, which is enough: a canonical queue that is full still lets `^C` and the like through.
    /// A raw one does not, and it is for raw mode that this is asked. `None` when it cannot be told.
    fn pending_input(&self) -> Option<usize> {
        let slave = self.open_slave().ok()?;
        let mut pending: libc::c_int = 0;
        (unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCINQ as _, &mut pending) } == 0).then(|| pending.max(0) as usize)
    }

    /// Wait, for a few milliseconds at most, until the `written` bytes just put into the master show in the input queue on top of the
    /// `before` that were there, or until the program has taken them (which looks the same as their not having arrived, and is why
    /// the wait has an end). See [`AGENT_STEP`].
    fn settle_input(&self, before: usize, written: usize) {
        for _ in 0..SETTLE_TRIES {
            match self.pending_input() {
                Some(now) if now < before + written => std::thread::sleep(SETTLE_STEP),
                _ => return,
            }
        }
    }
}

/// What a [`Terminal`] is made of, before it has a shell.
pub struct StartArgs {
    pub id: String,
    pub owner: Owner,
    pub cwd: PathBuf,
    pub title: String,
    pub cols: u16,
    pub rows: u16,
    pub ring: usize,
}

/// One terminal: a shell on a PTY, the screen copy of what it wrote, and the ring of the raw bytes. Shared as an `Arc` between the
/// terminal table, the three threads that serve it, and whoever streams it; it ends with [`Terminal::hangup`].
///
/// Lock order, where two are held: the screen's, then the ring's. The reader holds both while it feeds a read to the screen and appends
/// it to the ring, so whoever holds both (see [`Terminal::view`]) sees the two at the same byte. Every method that takes the screen's
/// lock blocks (see the module's notes); the ones that do not say so.
pub struct Terminal {
    id: String,
    owner: Owner,
    cwd: PathBuf,
    started_ms: u64,
    screen: Screen,
    core: Arc<Core>,
    master: Master,
    sid: i32,
    ring: Mutex<Ring>,
    seq: AtomicU64,
    last_ms: AtomicU64,
    /// When an agent last typed into this terminal (`0` for never).
    agent_last_ms: AtomicU64,
    /// `0` while the shell runs, else when it ended.
    exited_ms: AtomicU64,
    exit: Mutex<Option<Exit>>,
    closing: AtomicBool,
    /// The reader has ended: it has taken the last of what it was going to take.
    read_done: AtomicBool,
    hung: AtomicBool,
    /// Handed to the waiter when the terminal ends: it reaps the shell then and not before. Dropping it does the same.
    reap: Mutex<Option<mpsc::Sender<()>>>,
    hub: Arc<Hub>,
}

impl Terminal {
    /// Take over a shell from the spawner and start the threads that serve it. If a thread cannot be started the shell is ended and
    /// reaped before the error is returned: nothing is left running, and no zombie.
    pub fn start(args: StartArgs, mut spawned: Spawned, hub: Arc<Hub>) -> io::Result<Arc<Terminal>> {
        let (Some(master), Some(child)) = (spawned.master.take(), spawned.child.take()) else {
            return Err(io::Error::other("the shell was already taken"));
        };
        let sid = spawned.pid;
        let tty = std::mem::take(&mut spawned.tty);
        let child = Arc::new(Mutex::new(Some(child)));
        let core = Arc::new(Core {
            id: args.id.clone(),
            base_title: args.title.clone(),
            title: Mutex::new(TitleState { text: args.title, announced: None, pending: false }),
            queue: WriteQueue::new(),
            hub: hub.clone(),
        });
        let (reap_tx, reap_rx) = mpsc::channel();
        let now = now_ms();
        let term = Arc::new(Terminal {
            id: args.id,
            owner: args.owner,
            cwd: args.cwd,
            started_ms: now,
            screen: Screen::new(args.cols, args.rows, core.clone()),
            core,
            master: Master { fd: master, tty },
            sid,
            ring: Mutex::new(Ring::new(args.ring)),
            seq: AtomicU64::new(0),
            last_ms: AtomicU64::new(now),
            agent_last_ms: AtomicU64::new(0),
            exited_ms: AtomicU64::new(0),
            exit: Mutex::new(None),
            closing: AtomicBool::new(false),
            read_done: AtomicBool::new(false),
            hung: AtomicBool::new(false),
            reap: Mutex::new(Some(reap_tx)),
            hub,
        });
        let reader = term.clone();
        let mut failed = thread(&term.id, "read", move || read_loop(&reader)).err();
        if failed.is_none() {
            let writer = term.clone();
            failed = thread(&term.id, "write", move || write_loop(&writer)).err();
        }
        let mut waiting = false;
        if failed.is_none() {
            let weak = Arc::downgrade(&term);
            let shell = child.clone();
            failed = thread(&term.id, "wait", move || wait_loop(weak, shell, sid, reap_rx)).err();
            waiting = failed.is_none();
        }
        if let Some(e) = failed {
            term.hangup(Duration::from_millis(500));
            // With the waiter running it reaps the shell as soon as the hang-up has dropped the sender; without it, it is done here.
            if !waiting {
                reap_shell(&child);
            }
            return Err(e);
        }
        Ok(term)
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn owner(&self) -> &Owner {
        &self.owner
    }

    pub fn is_alive(&self) -> bool {
        self.exited_ms.load(Ordering::Acquire) == 0
    }

    /// When the shell ended on its own or was ended, and how.
    pub fn exited(&self) -> Option<(u64, Exit)> {
        let at = self.exited_ms.load(Ordering::Acquire);
        let exit = *lock(&self.exit);
        (at != 0).then(|| (at, exit.unwrap_or(Exit { code: None, signal: None })))
    }

    /// The byte the ring is at: how much this terminal has ever written.
    pub fn seq(&self) -> u64 {
        self.seq.load(Ordering::Acquire)
    }

    /// Whether the hold is on (the operator's stop for the agent).
    pub fn hold(&self) -> bool {
        self.core.queue.held()
    }

    /// Set the hold; whether it changed. Turning it on stops, in this order, what an agent can still get to the shell through this
    /// terminal: new input (refused, [`InputError::Held`]), input queued and not yet written (dropped), a write in progress (waited
    /// for, so none is half-way), and what was written to the PTY and not yet read by the program (flushed from the slave's input
    /// queue, which takes the operator's unread bytes with it: the PTY does not know whose they were). What the program has already
    /// read, and the operator's input from here on, it does not touch.
    pub fn set_hold(&self, hold: bool) -> bool {
        let changed = self.core.queue.set_hold(hold);
        if changed && hold && self.is_alive() && let Err(e) = self.master.flush_input() {
            tracing::debug!(error = %e, id = %self.id, "perch: the pty's unread input was not flushed");
        }
        if changed {
            self.hub.emit(Change::Hold(self.id.clone()));
        }
        changed
    }

    /// An agent typed into this terminal just now.
    pub fn note_agent(&self) {
        let now = now_ms();
        self.agent_last_ms.store(now, Ordering::Release);
        self.last_ms.store(now, Ordering::Release);
    }

    /// The comm of the foreground process group, which is what is running in it now (`bash`, `vim`, `ssh`). The group is believed only
    /// if a process of this terminal's session is in it: the number the terminal reports can be one whose processes are gone.
    pub fn fg(&self) -> String {
        if !self.is_alive() {
            return String::new();
        }
        let leader = self.master.foreground().and_then(|group| fg_leader(self.sid, group));
        let comm = leader.and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/comm")).ok());
        comm.map(|c| screen::clean_title(c.trim())).unwrap_or_default()
    }

    pub fn title(&self) -> String {
        lock(&self.core.title).text.clone()
    }

    /// The terminal's facts that need no screen lock (the size is kept apart for this).
    pub fn info(&self) -> Info {
        let (cols, rows) = self.screen.size();
        let agent = self.agent_last_ms.load(Ordering::Acquire);
        Info {
            id: self.id.clone(),
            title: self.title(),
            fg: self.fg(),
            by: self.owner.by,
            chat: self.owner.chat.clone(),
            cwd: self.cwd.display().to_string(),
            cols,
            rows,
            alive: self.is_alive(),
            exit: self.exited().map(|(_, exit)| exit),
            seq: self.seq(),
            started_ms: self.started_ms,
            last_ms: self.last_ms.load(Ordering::Acquire),
            agent_last_ms: (agent != 0).then_some(agent),
            hold: self.hold(),
            ended: false,
        }
    }

    /// Queue the operator's bytes for the shell. All or nothing; nothing waits. The hold does not apply: the operator can always type.
    pub fn input(&self, bytes: &[u8]) -> Result<(), InputError> {
        if !self.is_alive() {
            return Err(InputError::Ended);
        }
        self.core.queue.input(bytes)?;
        self.last_ms.store(now_ms(), Ordering::Release);
        Ok(())
    }

    /// Queue an agent's bytes for the shell: all or nothing, nothing waits, refused while the hold is on (the hold is read under the
    /// lock that queues the bytes). [`Terms::agent_input`](super::Terms::agent_input) is the way in; it adds the event.
    pub fn agent_input(&self, bytes: &[u8]) -> Result<(), InputError> {
        if !self.is_alive() {
            return Err(InputError::Ended);
        }
        self.core.queue.agent_input(bytes)?;
        self.note_agent();
        Ok(())
    }

    /// Resize the PTY (the shell gets `SIGWINCH`) and the screen to `cols` x `rows`, clamped; the size they have now. **Blocking:** it
    /// takes the screen's lock.
    pub fn resize(&self, cols: u32, rows: u32) -> (u16, u16) {
        let (cols, rows) = screen::clamp_size(Some(cols), Some(rows));
        if self.screen.size() != (cols, rows) {
            if let Err(e) = self.screen.resize(cols, rows, || self.master.resize(cols, rows)) {
                tracing::debug!(error = %e, id = %self.id, "perch: the pty did not take its new size");
            }
            self.hub.emit(Change::Size(self.id.clone()));
        }
        self.screen.size()
    }

    /// The screen as text (see [`screen::text_of`]), at the byte the screen has taken: the ring's end less the few bytes of a character
    /// still waiting for its end. **Blocking:** it takes the screen's lock.
    pub fn text(&self, lines: usize, max_chars: usize) -> Text {
        self.view(|emu, ring| screen::text_of(emu, lines, max_chars, ring.end().saturating_sub(emu.lag() as u64)))
    }

    /// Look at the screen and the ring at the same byte: the screen's lock, then the ring's, as the reader takes them. `f` must be
    /// quick, and must not call back into this terminal. **Blocking:** it takes the screen's lock, so it is for a thread that can wait
    /// (an async worker goes through `spawn_blocking`).
    pub fn view<R>(&self, f: impl FnOnce(&Emu, &Ring) -> R) -> R {
        let emu = self.screen.lock();
        let ring = lock(&self.ring);
        f(&emu, &ring)
    }

    /// The raw output from `from` on, if the ring still has it (see [`Ring::read`]). Takes only the ring's lock.
    pub fn read(&self, from: u64, max: usize) -> Read {
        lock(&self.ring).read(from, max)
    }

    /// What `terms.json` keeps of this terminal, screen text included. **Blocking:** it takes the screen's lock. A path with a deadline
    /// (the shutdown) uses [`Terminal::record_until`].
    pub fn record(&self, by_perch: bool) -> Record {
        let text = screen::saved_text(&self.screen.lock());
        self.record_with(by_perch, text)
    }

    /// The same record under a deadline: the screen's lock is tried, not waited for, until `until`, and a screen that could not be had
    /// by then is a record without text. If the reader is still on its way out (after a hang-up it takes the last of the output
    /// first), or the waiter has not yet noted that the shell is gone (so that the record can say how it ended), that is waited for
    /// too, within the same limit. Never blocks past `until` by more than a couple of milliseconds.
    pub fn record_until(&self, by_perch: bool, until: Instant) -> Record {
        while !(self.read_done.load(Ordering::Acquire) && !self.is_alive()) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        let text = loop {
            if let Some(emu) = self.screen.try_lock() {
                break screen::saved_text(&emu);
            }
            if Instant::now() >= until {
                break String::new();
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        self.record_with(by_perch, text)
    }

    /// The record without the screen text: what can be said of a terminal whose screen could not be had in time. Never blocks.
    pub fn record_bare(&self, by_perch: bool) -> Record {
        self.record_with(by_perch, String::new())
    }

    fn record_with(&self, by_perch: bool, text: String) -> Record {
        let (cols, rows) = self.screen.size();
        let exited = self.exited();
        Record {
            id: self.id.clone(),
            title: self.title(),
            cwd: self.cwd.display().to_string(),
            cols,
            rows,
            by: self.owner.by,
            chat: self.owner.chat.clone(),
            started_ms: self.started_ms,
            ended_ms: exited.map_or_else(now_ms, |(at, _)| at),
            exit: exited.map(|(_, exit)| exit),
            by_perch,
            text,
        }
    }

    /// End the terminal's whole session (see [`end_session`]) and hand the shell to the waiter to be reaped. Once; blocking, for up to
    /// `grace` and a moment more.
    pub fn hangup(&self, grace: Duration) {
        if self.hung.swap(true, Ordering::AcqRel) {
            return;
        }
        self.core.queue.close();
        end_session(self.sid, self.master.foreground(), grace);
        // The reader stays until now, so the last of what the session wrote is in the screen; it ends on this.
        self.closing.store(true, Ordering::Release);
        drop(lock(&self.reap).take());
    }

    /// End the session hard and without waiting, for a terminal nobody is going to hang up properly (its table was dropped, a test
    /// failed): `SIGKILL` to the session, the threads told to go, the shell reaped by the waiter. Once, like [`Terminal::hangup`].
    pub fn abandon(&self) {
        if self.hung.swap(true, Ordering::AcqRel) {
            return;
        }
        self.core.queue.close();
        kill_session(self.sid);
        self.closing.store(true, Ordering::Release);
        drop(lock(&self.reap).take());
    }

    /// Feed one read of output to the screen and the ring, at once.
    fn absorb(&self, bytes: &[u8]) {
        self.screen.feed(bytes, || {
            let mut ring = lock(&self.ring);
            ring.push(bytes);
            self.seq.store(ring.end(), Ordering::Release);
        });
        self.last_ms.store(now_ms(), Ordering::Release);
        self.hub.touch();
    }

    /// Wait, for up to `max`, for the reader to have ended (its terminal's output is all in the screen then).
    fn await_reader(&self, max: Duration) {
        let until = Instant::now() + max;
        while !self.read_done.load(Ordering::Acquire) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The waiter saw the shell end (and gave the reader its moment).
    fn ended(&self, exit: Exit) {
        *lock(&self.exit) = Some(exit);
        self.exited_ms.store(now_ms().max(1), Ordering::Release);
        self.core.queue.close();
        self.hub.emit(Change::Exit(self.id.clone()));
    }

    /// The screen's lock, held by the test, so that a reader or a record has to wait for it.
    #[cfg(test)]
    pub fn hold_screen(&self) -> std::sync::MutexGuard<'_, Emu> {
        self.screen.lock()
    }

    #[cfg(test)]
    pub fn sid(&self) -> i32 {
        self.sid
    }
}

impl Drop for Terminal {
    /// A terminal dropped without being ended (a panic, a test that failed) takes its session with it: hard, and without waiting.
    fn drop(&mut self) {
        self.abandon();
    }
}

#[cfg(test)]
thread_local! {
    /// Fail the start of the thread after this many more have started, on this thread only (`None`: never).
    static FAIL_THREAD: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}

fn thread(id: &str, what: &str, work: impl FnOnce() + Send + 'static) -> io::Result<()> {
    #[cfg(test)]
    if FAIL_THREAD.with(|left| match left.get() {
        Some(0) => {
            left.set(None);
            true
        }
        Some(n) => {
            left.set(Some(n - 1));
            false
        }
        None => false,
    }) {
        return Err(io::Error::other("test: this thread does not start"));
    }
    std::thread::Builder::new().name(format!("perch-term-{what}-{id}")).spawn(work).map(drop)
}

/// The reader: master -> screen and ring. Ends when the terminal is ended or the master has nothing more to give (every process that
/// held the slave is gone: EIO, or EOF); either way it takes what is left in the master first, and then says it is done.
fn read_loop(term: &Terminal) {
    let fd = term.master.raw();
    let mut buf = vec![0u8; READ_BUF];
    let mut pace = Throttle::default();
    while !term.closing.load(Ordering::Acquire) {
        term.core.flush_title(false);
        match wait_fd(fd, libc::POLLIN, POLL_MS) {
            Ok(0) => continue,
            Ok(events) if events & libc::POLLNVAL != 0 => break,
            Ok(_) => {}
            Err(_) => break,
        }
        match read_fd(fd, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                term.absorb(&buf[..n]);
                let pause = pace.after(Instant::now(), n);
                if !pause.is_zero() {
                    std::thread::sleep(pause);
                }
            }
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {}
            Err(_) => break,
        }
    }
    // The terminal was ended while its session still wrote: what is in the master now is the last of it.
    if term.closing.load(Ordering::Acquire) {
        for _ in 0..FINAL_READS {
            match read_fd(fd, &mut buf) {
                Ok(n) if n > 0 => term.absorb(&buf[..n]),
                _ => break,
            }
        }
    }
    term.core.flush_title(true);
    term.read_done.store(true, Ordering::Release);
    term.hub.touch();
}

/// The writer: the queue -> master. Ends when the queue is closed (the shell ended, or the terminal did).
fn write_loop(term: &Terminal) {
    let fd = term.master.raw();
    while let Some(chunk) = term.core.queue.next(WRITE_CHUNK) {
        if term.closing.load(Ordering::Acquire) {
            return;
        }
        // An agent's bytes go in only while little of them sits unread in the PTY: a program in raw mode that reads nothing keeps its
        // queue (4 KiB, then the kernel's buffers behind it), and a `^C` written after a full one would wait behind all of it.
        let pending = if chunk.lane == Lane::Agent { term.master.pending_input() } else { None };
        let allow = pending.map_or(chunk.bytes.len(), |pending| AGENT_AHEAD.saturating_sub(pending).min(AGENT_STEP));
        if allow == 0 {
            term.core.queue.give_back(chunk, 0);
            std::thread::sleep(Duration::from_millis(10));
            continue;
        }
        match term.core.queue.write(fd, &chunk, allow) {
            // The hold is on: an agent's bytes are not written, and the rest of them is gone.
            None => {}
            Some(Ok(n)) => {
                term.core.queue.give_back(chunk, n);
                if let Some(before) = pending {
                    term.master.settle_input(before, n);
                }
            }
            Some(Err(e)) if e.kind() == io::ErrorKind::WouldBlock => {
                // The shell is not reading and the PTY is full: what did not go waits at the front of its lane, so that whatever
                // is more urgent by the time there is room (the operator's `^C`) is what is written then.
                term.core.queue.give_back(chunk, 0);
                let _ = wait_fd(fd, libc::POLLOUT, POLL_MS);
            }
            Some(Err(e)) if e.kind() == io::ErrorKind::Interrupted => term.core.queue.give_back(chunk, 0),
            Some(Err(_)) => {
                term.core.queue.close();
                return;
            }
        }
    }
}

/// The waiter: notice the shell end, give the reader its moment to take the last of the output, say so, and reap the shell only once
/// the terminal itself is ended (or gone), because until then its pid is the session id that the sweep signals.
fn wait_loop(term: Weak<Terminal>, child: Arc<Mutex<Option<Child>>>, pid: i32, reap: mpsc::Receiver<()>) {
    let exit = wait_exit(pid);
    if let Some(term) = term.upgrade() {
        // The reader ends on the master's `EIO`, which is the end of everything the session wrote; a job that kept the slave open
        // (`sleep 1000 &`) never gives it, so the wait has a limit and the output the reader has by then is what the record shows.
        term.await_reader(READER_WAIT);
        term.ended(exit);
    }
    let _ = reap.recv();
    reap_shell(&child);
}

/// Reap the shell (the session it led is gone, or being killed, by now), waiting a couple of seconds at most.
fn reap_shell(child: &Mutex<Option<Child>>) {
    let Some(mut shell) = lock(child).take() else { return };
    let until = Instant::now() + Duration::from_secs(2);
    while Instant::now() < until {
        if !matches!(shell.try_wait(), Ok(None)) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// At shutdown (serve.rs): end every terminal, concurrently, within 2.5 s.
pub async fn shutdown(ctx: &Ctx) {
    if let Some(terms) = ctx.term.get() {
        terms.shutdown().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::By;
    use std::os::unix::process::ExitStatusExt as _;

    /// A child process that is killed and reaped when the test is over, whatever it did.
    struct Reaped(std::process::Child);

    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A `sleep` in a process group of its own, in this test's session and not in any terminal's.
    fn sleeper() -> Reaped {
        Reaped(Command::new("sleep").arg("60").process_group(0).spawn().unwrap())
    }

    /// Whether `pid` is a process that is not already dead (a zombie is not).
    fn alive(pid: i32) -> bool {
        read_stat(pid).is_some_and(|(state, _, _)| !matches!(state, 'Z' | 'X'))
    }

    fn test_env() -> Vec<(OsString, OsString)> {
        let path = std::env::var_os("PATH").unwrap_or_else(|| "/usr/bin:/bin".into());
        child_env(&[("SHELL".into(), "/bin/sh".into()), ("PATH".into(), path)], "t9")
    }

    fn until(what: &str, mut done: impl FnMut() -> bool) {
        let end = Instant::now() + Duration::from_secs(8);
        while !done() {
            assert!(Instant::now() < end, "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn the_ring_numbers_every_byte_and_says_when_a_reader_fell_behind() {
        let mut ring = Ring::new(8);
        assert_eq!((ring.start(), ring.end()), (0, 0));
        ring.push(b"abcde");
        assert_eq!((ring.start(), ring.end()), (0, 5), "while nothing was dropped the ring starts at byte 0: an exact replay");
        assert_eq!(ring.read(2, 100), Read::Data(b"cde".to_vec()));
        assert_eq!(ring.read(5, 100), Read::Data(Vec::new()), "caught up: nothing, not an error");

        ring.push(b"fghijkl");
        assert_eq!((ring.start(), ring.end()), (4, 12), "twelve bytes written, the last eight held");
        assert_eq!(ring.contents(), b"efghijkl");
        assert_eq!(ring.read(4, 3), Read::Data(b"efg".to_vec()), "limited");
        assert_eq!(ring.read(9, 100), Read::Data(b"jkl".to_vec()));
        assert_eq!(ring.read(3, 100), Read::Behind { start: 4 }, "one byte too old");
        assert_eq!(ring.read(0, 100), Read::Behind { start: 4 });

        // One push bigger than the ring keeps its tail and still counts every byte.
        ring.push(b"0123456789ABCDEF");
        assert_eq!((ring.start(), ring.end()), (20, 28));
        assert_eq!(ring.contents(), b"89ABCDEF");
    }

    #[test]
    fn the_write_queue_is_bounded_in_every_lane_and_replies_go_first() {
        let queue = WriteQueue::new();
        assert!(queue.input(&vec![b'x'; INPUT_QUEUE]).is_ok(), "exactly the cap fits");
        assert_eq!(queue.input(b"y"), Err(InputError::Full), "one more does not, and nothing of it is queued");
        assert!(queue.agent_input(&vec![b'a'; INPUT_QUEUE]).is_ok(), "the agent's lane is its own");
        assert_eq!(queue.agent_input(b"y"), Err(InputError::Full));
        assert!(queue.reply(&vec![b'r'; REPLY_QUEUE]));
        assert!(!queue.reply(b"r"), "replies have a budget of their own and are dropped when it is spent");

        let first = queue.next(10).unwrap();
        assert_eq!(first, Chunk { bytes: b"rrrrrrrrrr".to_vec(), lane: Lane::Replies }, "replies first, in chunks of the size asked");
        let (mut taken, mut operator, mut agent) = (first.bytes.len(), 0, 0);
        while taken < 2 * INPUT_QUEUE + REPLY_QUEUE {
            let chunk = queue.next(4096).unwrap();
            operator += chunk.bytes.iter().filter(|b| **b == b'x').count();
            agent += chunk.bytes.iter().filter(|b| **b == b'a').count();
            assert_eq!(chunk.lane == Lane::Agent, chunk.bytes.contains(&b'a'), "a chunk says whose it is");
            taken += chunk.bytes.len();
        }
        assert_eq!((operator, agent), (INPUT_QUEUE, INPUT_QUEUE), "every input byte came out, none twice");
        assert!(queue.input(b"again").is_ok(), "room again once drained");

        queue.close();
        assert_eq!(queue.next(10), None, "closed: the writer's loop ends, and what was queued is not written");
        assert_eq!(queue.input(b"z"), Err(InputError::Ended));
        assert_eq!(queue.agent_input(b"z"), Err(InputError::Ended));
        assert!(!queue.reply(b"z"));
    }

    #[test]
    fn a_closed_queue_wakes_a_writer_that_waits() {
        let queue = Arc::new(WriteQueue::new());
        let waiter = {
            let queue = queue.clone();
            std::thread::spawn(move || queue.next(10))
        };
        std::thread::sleep(Duration::from_millis(50));
        queue.close();
        assert_eq!(waiter.join().unwrap(), None);
    }

    #[test]
    fn the_hold_drops_what_an_agent_queued_refuses_more_and_leaves_the_operator_alone() {
        let queue = WriteQueue::new();
        queue.agent_input(b"rm -rf ~\r").unwrap();
        queue.input(b"\x03").unwrap();
        assert!(queue.set_hold(true));
        assert!(!queue.set_hold(true), "no change, no event");
        assert!(queue.held());
        assert_eq!(queue.drop_input(), 0, "what the agent had queued went with the hold");
        assert_eq!(queue.agent_input(b"x"), Err(InputError::Held), "refused under the queue's lock, so none can get in behind the hold");
        assert!(queue.input(b"\x03").is_ok(), "the operator can always type");
        let chunk = queue.next(100).unwrap();
        assert_eq!(chunk, Chunk { bytes: b"\x03\x03".to_vec(), lane: Lane::Operator });

        assert!(queue.set_hold(false));
        queue.agent_input(b"ls\r").unwrap();
        assert_eq!(queue.drop_input(), 3, "dropping the agent's input is available on its own too");
    }

    #[test]
    fn an_agent_that_fills_its_lane_does_not_keep_the_operators_interrupt_out() {
        let queue = WriteQueue::new();
        queue.agent_input(&vec![b'a'; INPUT_QUEUE]).unwrap();
        assert_eq!(queue.agent_input(b"a"), Err(InputError::Full));
        queue.input(b"\x03").unwrap();
        assert_eq!(queue.next(4096), Some(Chunk { bytes: b"\x03".to_vec(), lane: Lane::Operator }), "the operator's byte goes first");
        let chunk = queue.next(4096).unwrap();
        assert!(chunk.lane == Lane::Agent && chunk.bytes.iter().all(|b| *b == b'a'));
    }

    #[test]
    fn what_a_full_pty_did_not_take_goes_back_to_the_front_of_its_lane_and_the_operator_goes_ahead_of_it() {
        let queue = WriteQueue::new();
        queue.agent_input(b"0123456789").unwrap();
        let chunk = queue.next(100).unwrap();
        assert_eq!(chunk.lane, Lane::Agent);
        queue.input(b"\x03").unwrap();
        // Four bytes went and six did not: the six wait, and the interrupt that came meanwhile is written before them.
        queue.give_back(chunk, 4);
        assert_eq!(queue.next(100), Some(Chunk { bytes: b"\x03".to_vec(), lane: Lane::Operator }));
        assert_eq!(queue.next(100), Some(Chunk { bytes: b"456789".to_vec(), lane: Lane::Agent }));

        // More queued behind it stays behind it.
        queue.agent_input(b"abc").unwrap();
        queue.give_back(Chunk { bytes: b"xyz".to_vec(), lane: Lane::Agent }, 0);
        assert_eq!(queue.next(100), Some(Chunk { bytes: b"xyzabc".to_vec(), lane: Lane::Agent }), "order is kept");

        // Under a hold the rest of an agent's chunk is dropped, not put back; and after the close nothing is.
        queue.give_back(Chunk { bytes: b"abc".to_vec(), lane: Lane::Agent }, 1);
        queue.set_hold(true);
        queue.give_back(Chunk { bytes: b"abc".to_vec(), lane: Lane::Agent }, 1);
        assert_eq!(queue.drop_input(), 0);
        queue.give_back(Chunk { bytes: b"op".to_vec(), lane: Lane::Operator }, 0);
        assert_eq!(queue.next(100), Some(Chunk { bytes: b"op".to_vec(), lane: Lane::Operator }), "the operator's is put back whatever the hold");
        queue.close();
        queue.give_back(Chunk { bytes: b"op".to_vec(), lane: Lane::Operator }, 0);
        assert_eq!(queue.next(100), None);
    }

    #[test]
    fn the_writer_looks_at_the_hold_for_each_write_of_an_agents_bytes_and_not_the_operators() {
        let mut ends = [0 as libc::c_int; 2];
        assert_eq!(unsafe { libc::pipe(ends.as_mut_ptr()) }, 0);
        let (read_end, write_end) = (unsafe { OwnedFd::from_raw_fd(ends[0]) }, unsafe { OwnedFd::from_raw_fd(ends[1]) });
        let queue = WriteQueue::new();
        let agent = Chunk { bytes: b"agent".to_vec(), lane: Lane::Agent };
        let operator = Chunk { bytes: b"operator".to_vec(), lane: Lane::Operator };
        assert_eq!(queue.write(write_end.as_raw_fd(), &agent, 100).unwrap().unwrap(), 5);
        queue.set_hold(true);
        assert!(queue.write(write_end.as_raw_fd(), &agent, 100).is_none(), "held: not written");
        assert_eq!(queue.write(write_end.as_raw_fd(), &operator, 100).unwrap().unwrap(), 8, "the operator's bytes are not held");
        let mut buf = [0u8; 64];
        let n = read_fd(read_end.as_raw_fd(), &mut buf).unwrap();
        assert_eq!(&buf[..n], b"agentoperator");
    }

    #[test]
    fn output_above_eight_mebibytes_a_second_is_slowed_and_below_it_is_not() {
        let mut pace = Throttle::default();
        let start = Instant::now();
        // 16 KiB every millisecond is 16 MiB/s: the window's share is spent after 52 reads, and then every read waits.
        let mut first_pause = None;
        for ms in 0..90u64 {
            let pause = pace.after(start + Duration::from_millis(ms), 16 * 1024);
            if !pause.is_zero() && first_pause.is_none() {
                first_pause = Some(ms);
                assert_eq!(pause, THROTTLE_PAUSE);
            }
            if first_pause.is_some() {
                assert_eq!(pause, THROTTLE_PAUSE, "every read of the rest of the window waits");
            }
        }
        assert_eq!(first_pause, Some(51));
        // The window turns over and the count starts again.
        assert_eq!(pace.after(start + Duration::from_millis(250), 16 * 1024), Duration::ZERO);
        // 16 KiB every 10 ms is 1.6 MiB/s and never waits.
        let mut pace = Throttle::default();
        for n in 0..500u64 {
            assert_eq!(pace.after(start + Duration::from_millis(10 * n), 16 * 1024), Duration::ZERO);
        }
    }

    #[test]
    fn a_stat_line_is_read_from_the_last_parenthesis() {
        assert_eq!(parse_stat("1234 (bash) S 1 1234 1234 34816 1234 4194560 100 0"), Some(('S', 1234, 1234)));
        assert_eq!(parse_stat("55 (we (ird) name) Z 1 56 77 0 -1"), Some(('Z', 56, 77)), "a name with spaces and parentheses");
        assert_eq!(parse_stat("55 (x) R 1 55"), None, "a line cut short");
        assert_eq!(parse_stat("garbage"), None);
    }

    #[test]
    fn nothing_that_could_reach_the_perch_or_everything_is_ever_signalled() {
        let own = std::process::id() as i32;
        for pid in [-1, 0, 1, own, unsafe { libc::getpgrp() }] {
            assert!(!signalable(pid), "{pid}");
            // And so these are no-ops that cannot take the test process down.
            signal_group(pid, 0);
            signal_member(pid, own, 0);
        }
        assert!(signalable(own + 1_000_000));
        // Session 0 is the kernel's threads and 1 is init's: a sweep of either finds nothing to kill.
        assert!(live_members(0).is_empty());
        assert!(live_members(1).is_empty());
        assert!(live_members(-5).is_empty());
    }

    #[test]
    fn a_process_is_signalled_only_when_it_is_in_the_session_that_was_named() {
        let child = sleeper();
        let pid = child.0.id() as i32;
        let mine = unsafe { libc::getsid(0) };
        assert!(member_of(pid, mine));
        assert!(!member_of(pid, mine + 1_000_000));

        // The number was handed to someone else since it was listed: that is a process in another session, and it is left alone.
        signal_member(pid, mine + 1_000_000, libc::SIGKILL);
        std::thread::sleep(Duration::from_millis(100));
        assert!(alive(pid), "a process of another session survives the signal");

        signal_member(pid, mine, libc::SIGKILL);
        until("the member did not die", || !alive(pid));
        let mut child = child;
        assert_eq!(child.0.wait().unwrap().signal(), Some(libc::SIGKILL));
    }

    #[test]
    fn a_foreground_group_no_process_of_the_session_is_in_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let shell = spawn(80, 24, dir.path(), &test_env()).unwrap();
        let sid = shell.pid;
        let stranger = sleeper();
        let group = stranger.0.id() as i32;

        assert_eq!(fg_leader(sid, sid), Some(sid), "the shell's own group has a member");
        assert_eq!(fg_leader(sid, group), None, "a group that no process of the session is in is not the terminal's");
        assert_eq!(fg_leader(sid, 1), None);

        // The terminal says this is its foreground group; nothing of the session is in it, so it is not signalled.
        end_session(sid, Some(group), Duration::from_millis(100));
        assert!(live_members(sid).is_empty(), "the session itself is gone");
        assert!(alive(group), "the stranger's group was not signalled");
        drop(shell);
        assert!(alive(group));
    }

    #[test]
    fn the_pty_is_close_on_exec_from_the_moment_it_exists() {
        let (master, slave, tty) = open_pty(80, 24).unwrap();
        for fd in [&master, &slave] {
            let flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            assert!(flags >= 0 && flags & libc::FD_CLOEXEC != 0, "{flags}");
        }
        assert!(tty.starts_with("/dev/pts/"), "{tty:?}");
        let mut size: libc::winsize = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::ioctl(master.as_raw_fd(), libc::TIOCGWINSZ as _, &mut size) }, 0);
        assert_eq!((size.ws_col, size.ws_row), (80, 24));
    }

    #[test]
    fn a_shell_nobody_took_is_killed_and_reaped_when_it_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let shell = spawn(80, 24, dir.path(), &test_env()).unwrap();
        let pid = shell.pid;
        assert_eq!(unsafe { libc::getsid(pid) }, pid, "a session of its own");
        assert!(alive(pid));
        drop(shell);
        assert!(!Path::new(&format!("/proc/{pid}")).exists(), "gone, and not a zombie");
        assert!(live_members(pid).is_empty());
    }

    fn start_args(id: &str, dir: &Path) -> StartArgs {
        StartArgs { id: id.into(), owner: Owner { by: By::Operator, chat: None }, cwd: dir.into(), title: "shell".into(), cols: 80, rows: 24, ring: RING_BYTES }
    }

    #[test]
    fn a_thread_that_will_not_start_leaves_no_shell_and_no_zombie() {
        let dir = tempfile::tempdir().unwrap();
        // The first thread, the second, the third: whichever fails, the shell is ended and reaped before the error comes back.
        for (n, failing) in ["the reader", "the writer", "the waiter"].into_iter().enumerate() {
            let shell = spawn(80, 24, dir.path(), &test_env()).unwrap();
            let pid = shell.pid;
            FAIL_THREAD.with(|left| left.set(Some(n)));
            let started = Terminal::start(start_args("t1", dir.path()), shell, Hub::new());
            FAIL_THREAD.with(|left| left.set(None));
            assert!(started.is_err(), "{failing} failed to start");
            until(&format!("the shell is still there after {failing} failed"), || !Path::new(&format!("/proc/{pid}")).exists());
            assert!(live_members(pid).is_empty());
        }
    }

    #[test]
    fn a_title_is_announced_when_it_differs_and_at_most_ten_times_a_second_with_the_newest_winning() {
        let hub = Hub::new();
        let mut changes = hub.changes.subscribe();
        let core = Core {
            id: "t1".into(),
            base_title: "shell".into(),
            title: Mutex::new(TitleState { text: "shell".into(), announced: None, pending: false }),
            queue: WriteQueue::new(),
            hub,
        };
        let seen = |changes: &mut tokio::sync::broadcast::Receiver<Change>| {
            let mut n = 0;
            while changes.try_recv().is_ok() {
                n += 1;
            }
            n
        };
        core.set_title(Some("shell".into()));
        assert_eq!(seen(&mut changes), 0, "the title the terminal has is not news");
        core.set_title(Some("one".into()));
        assert_eq!(seen(&mut changes), 1, "the first change goes out at once");
        core.set_title(Some("one".into()));
        assert_eq!(seen(&mut changes), 0, "the same title again is not news");
        for n in 0..50 {
            core.set_title(Some(format!("storm {n}")));
        }
        assert_eq!(seen(&mut changes), 0, "a title set in a loop is held back");
        core.flush_title(false);
        assert_eq!(seen(&mut changes), 0, "and not announced before its turn");
        std::thread::sleep(TITLE_EVERY + Duration::from_millis(30));
        core.flush_title(false);
        assert_eq!(seen(&mut changes), 1, "one event for the whole storm");
        assert_eq!(lock(&core.title).text, "storm 49", "the newest is the title");
        core.flush_title(false);
        assert_eq!(seen(&mut changes), 0, "once");

        core.set_title(None);
        core.flush_title(true);
        assert_eq!(lock(&core.title).text, "shell", "back to the terminal's own");
        assert_eq!(seen(&mut changes), 1, "forced at the end of the reader, whatever the time");
    }

    #[test]
    fn the_shell_gets_a_terminal_environment_and_none_of_the_perchs_private_names() {
        let os = |s: &str| OsString::from(s);
        let base = vec![
            (os("PATH"), os("/usr/bin")),
            (os("HOME"), os("/home/me")),
            (os("TERM"), os("dumb")),
            (os("MINERVA_TERM"), os("t999")),
            (os("MINERVA_HOME"), os("/home/me/Minerva")),
            (os("MINERVA_TERM_TOKEN"), os("secret-1")),
            (os("minerva_mail_token"), os("secret-2")),
            (os("MINERVA_LOGIN_CODE"), os("secret-3")),
            (os("MINERVA_ADMIN_PASSWORD"), os("secret-4")),
        ];
        let env = child_env(&base, "t7");
        let get = |name: &str| env.iter().find(|(k, _)| k == name).map(|(_, v)| v.to_string_lossy().into_owned());
        assert_eq!(get("MINERVA_TERM").as_deref(), Some("t7"), "the terminal's own id, not an inherited one");
        assert_eq!(get("TERM").as_deref(), Some("xterm-256color"));
        assert_eq!(get("COLORTERM").as_deref(), Some("truecolor"));
        assert_eq!(get("LANG").as_deref(), Some("C.UTF-8"), "set when it was not");
        assert_eq!(get("PATH").as_deref(), Some("/usr/bin"));
        assert_eq!(get("MINERVA_HOME").as_deref(), Some("/home/me/Minerva"), "a path is not a secret and a shell may want it");
        for name in ["MINERVA_TERM_TOKEN", "minerva_mail_token", "MINERVA_LOGIN_CODE", "MINERVA_ADMIN_PASSWORD"] {
            assert_eq!(get(name), None, "{name}");
        }
        assert_eq!(env.iter().filter(|(k, _)| k == "MINERVA_TERM" || k == "TERM").count(), 2, "each once");
        let with_lang = child_env(&[(os("LANG"), os("de_DE.UTF-8"))], "t1");
        assert_eq!(with_lang.iter().filter(|(k, _)| k == "LANG").count(), 1);
        assert!(with_lang.iter().any(|(k, v)| k == "LANG" && v == "de_DE.UTF-8"), "the member's language is kept");
    }
}

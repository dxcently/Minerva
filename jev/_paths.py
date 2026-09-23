"""Shared, dependency-free helpers `server.py` and `automation/decisions.py`
both need: where this machine's cache directory is, and how to append a
line to a shared file safely across processes.

Split out of `server.py` (which originally defined `_cache_dir`,
`_lock_file`/`_unlock_file` and `_O_BINARY` inline for its own ad-hoc
decision log) so that `automation/decisions.py`'s per-graph log -- a
second, independent set of files under the same cache root -- reuses the
exact locking `server.py`'s `_log_decision` already learned the hard way,
rather than a second implementation of the same lesson. See
`append_locked`'s docstring for what that lesson was.

This module has no dependency on `EIDOLON_SERVICE_PORT`/`_TOKEN` or on
FastAPI, unlike `server.py` -- importing it does not require a service
environment, which is what lets `automation/decisions.py` (and its tests)
use it without importing `server` and without `server` importing
`automation` at module scope either. Neither module imports the other.
"""
from __future__ import annotations

import os
import sys
from pathlib import Path, PurePosixPath


def cache_dir() -> Path:
    """Mirrors `dirs::cache_dir()` (Rust, `crates/rune/src/ext.rs`'s
    `record_dir()`) platform by platform, so a file placed under
    `cache_dir() / "eidolon" / "extensions" / "jev" / ...` lands beside
    jev's own machine-scoped ext record everywhere this runs, not only on
    Windows where both happen to be `%LOCALAPPDATA%`.

    Uses `PurePosixPath`, not the ambient `Path`, to test `XDG_CACHE_HOME`
    for absoluteness: a leading `/` is what makes a path absolute on the
    POSIX systems the Linux branch actually runs on, and that must hold
    even when the interpreter evaluating it is `WindowsPath` (as it is
    under a test that drives this branch on Windows by mocking
    `sys.platform`) -- `WindowsPath("/x").is_absolute()` is `False`
    (Windows absoluteness needs a drive letter), which would silently
    invert this check. Observed on Windows; reasoned, not observed, for the
    two POSIX branches themselves.
    """
    if sys.platform == "win32":
        return Path(os.environ.get("LOCALAPPDATA") or Path.home())
    if sys.platform == "darwin":
        return Path.home() / "Library" / "Caches"
    xdg = os.environ.get("XDG_CACHE_HOME")
    if xdg and PurePosixPath(xdg).is_absolute():
        return Path(xdg)
    return Path.home() / ".cache"


# `os.O_BINARY` exists only on Windows, where the C runtime's text mode
# rewrites every "\n" a write() call hands it into "\r\n" on the way to
# disk; `0` on POSIX, a no-op flag on `os.open`, not a missing feature
# standing in for one.
O_BINARY = getattr(os, "O_BINARY", 0)

# The cross-process half of `append_locked` below -- picked once, at
# import, since `msvcrt` does not exist on POSIX and `fcntl` does not exist
# on Windows. `msvcrt.locking` locks a caller-chosen byte range and is not
# released by closing the fd with defined behaviour (Microsoft's own docs
# call it "unpredictable"), so it needs an explicit matching unlock before
# that close; POSIX `flock` locks the whole open file description and is
# released automatically when the fd closes, so its own unlock call is
# symmetry, not load-bearing. Both give `append_locked` the one guarantee
# it needs: one process's lock/unlock pair excludes every other process's,
# for as long as it is held. See `jev/server.py`'s `_log_decision` for the
# measurement that makes this load-bearing on Windows specifically: six
# processes appending 200 rows each with this lock stubbed out lost 37% of
# rows to `_O_APPEND`'s reposition-then-write race.
if sys.platform == "win32":
    import msvcrt

    def lock_file(fd: int) -> None:
        os.lseek(fd, 0, os.SEEK_SET)
        msvcrt.locking(fd, msvcrt.LK_LOCK, 1)

    def unlock_file(fd: int) -> None:
        os.lseek(fd, 0, os.SEEK_SET)
        msvcrt.locking(fd, msvcrt.LK_UNLCK, 1)

else:
    import fcntl

    def lock_file(fd: int) -> None:
        fcntl.flock(fd, fcntl.LOCK_EX)  # blocks until held

    def unlock_file(fd: int) -> None:
        fcntl.flock(fd, fcntl.LOCK_UN)


def append_locked(path: Path, line: bytes) -> None:
    """Append `line` (already newline-terminated, if that is wanted) to
    `path`, creating parent directories as needed, holding a real OS lock
    across the write so two processes' appends cannot interleave or
    clobber each other on Windows (see `lock_file` above). Raises `OSError`
    on failure -- a full disk, a lock that timed out -- rather than
    swallowing it: whether "best effort, do not fail the caller's real
    answer over a logging problem" is the right call is a decision each
    call site makes for itself (`jev/server.py`'s `_log_decision` does;
    `automation/decisions.py` does too, for the same reason), not one this
    shared helper should make for both.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    fd = os.open(path, os.O_APPEND | os.O_CREAT | os.O_WRONLY | O_BINARY, 0o600)
    try:
        lock_file(fd)
        try:
            os.write(fd, line)
        finally:
            unlock_file(fd)
    finally:
        os.close(fd)

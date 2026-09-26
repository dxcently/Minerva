//! Where the milliseconds went between `main` and the first frame.
//!
//! Startup is a budget — the harness means to be at a live prompt in under
//! a hundred milliseconds — and a budget nobody can read is not kept. So
//! the consumers drop a [`mark`] at each phase boundary (config read,
//! catalog built, tools registered, terminal taken, first frame), and with
//! `EIDOLON_BOOT_LOG=/path` set every launch appends one block to that
//! file: each mark's time since start and the gap from the one before,
//! which is the column to read. Unset, a mark is two atomic loads and
//! nothing is kept.
//!
//! The clock starts at the first call — put one at the top of `main` — so
//! what happens before it (the loader, `#[tokio::main]` building its
//! runtime, about two milliseconds together) is not in the table. Same
//! shape as `EIDOLON_WIRE_LOG`: a file named by an environment variable,
//! off unless set, because the thing it measures owns the terminal.

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static START: OnceLock<Instant> = OnceLock::new();
static LOG: OnceLock<Option<PathBuf>> = OnceLock::new();
static MARKS: Mutex<Vec<(&'static str, Duration)>> = Mutex::new(Vec::new());

fn log_path() -> Option<&'static PathBuf> {
    LOG.get_or_init(|| std::env::var_os("EIDOLON_BOOT_LOG").map(PathBuf::from))
        .as_ref()
}

/// Start the clock, if nothing has yet. [`mark`] does this too; calling it
/// first thing in `main` only makes the first mark's time honest.
pub fn start() {
    START.get_or_init(Instant::now);
}

/// Record that `label` has just been reached. Free when the log is off.
pub fn mark(label: &'static str) {
    let at = START.get_or_init(Instant::now).elapsed();
    if log_path().is_some()
        && let Ok(mut m) = MARKS.lock()
    {
        m.push((label, at));
    }
}

/// The marks so far as a table: `label`, time since start, gap since the
/// previous mark. Empty when the log is off.
pub fn report() -> String {
    let Ok(marks) = MARKS.lock() else {
        return String::new();
    };
    let mut out = String::new();
    let mut prev = Duration::ZERO;
    for (label, at) in marks.iter() {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        out.push_str(&format!(
            "{:>8.1} ms  {:>+7.1}  {label}\n",
            ms(*at),
            ms(at.saturating_sub(prev))
        ));
        prev = *at;
    }
    out
}

/// Append the report to the log file, when there is one. Call once the
/// screen is up; anything marked later is simply not in this launch's block.
pub fn flush() {
    let Some(path) = log_path() else { return };
    let block = format!(
        "--- {} pid {}\n{}\n",
        std::process::id(),
        std::env::args().collect::<Vec<_>>().join(" "),
        report()
    );
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = f.write_all(block.as_bytes());
    }
}

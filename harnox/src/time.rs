//! Wall-clock seconds since the Unix epoch, as every expiry in this crate is
//! stored.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch, `0` if the clock is before it.
pub fn now_secs() -> u64 {
    secs_since_epoch(SystemTime::now())
}

/// `t` as seconds since the Unix epoch, `0` if it is before it.
pub fn secs_since_epoch(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

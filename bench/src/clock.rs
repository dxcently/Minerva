//! Time, behind a trait so the loop's time box and poll sleep are testable
//! without wall-clock waits.

use std::time::{Duration, Instant};

pub trait Clock {
    /// Seconds since the clock was created.
    fn elapsed_s(&self) -> u64;
    /// Wait, or in tests advance the clock without waiting.
    fn sleep_s(&self, secs: u64);
}

pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        Self { start: Instant::now() }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn elapsed_s(&self) -> u64 {
        self.start.elapsed().as_secs()
    }
    fn sleep_s(&self, secs: u64) {
        std::thread::sleep(Duration::from_secs(secs));
    }
}

#[cfg(test)]
pub mod test_support {
    use super::Clock;
    use std::cell::Cell;

    /// A clock that only moves when the loop sleeps, so a whole run resolves in
    /// microseconds and the time box is exact.
    pub struct FakeClock {
        now: Cell<u64>,
    }

    impl FakeClock {
        pub fn new() -> Self {
            Self { now: Cell::new(0) }
        }
    }

    impl Clock for FakeClock {
        fn elapsed_s(&self) -> u64 {
            self.now.get()
        }
        fn sleep_s(&self, secs: u64) {
            self.now.set(self.now.get() + secs);
        }
    }
}

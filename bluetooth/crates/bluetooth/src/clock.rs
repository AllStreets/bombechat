//! The wall clock contact handshakes are stamped and judged by.
//!
//! Every timer in the transport runs on the node's `now_ms`, a monotonic
//! clock. On macOS that clock stops while the Mac sleeps, and it never
//! follows a step of the system clock, so the Unix time can't be worked
//! out as "the time at launch plus `now_ms`": after a sleep longer than
//! the contact window every handshake would be stale. The transport
//! instead reads a [`WallClock`] each time it builds a contact handshake
//! or judges a request's hello.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Where the transport reads the Unix time.
pub trait WallClock: Send {
    /// Milliseconds since the Unix epoch, as the clock reads now.
    /// `now_ms` is the transport's monotonic clock at the same moment:
    /// [`SystemClock`] ignores it, and [`TestClock`] runs from it.
    fn unix_ms(&self, now_ms: u64) -> u64;
}

/// The system's wall clock, read afresh every time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl WallClock for SystemClock {
    fn unix_ms(&self, _now_ms: u64) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as u64)
    }
}

/// A wall clock for tests: `base` plus the transport's `now_ms`, plus a
/// shift the test may change at any moment, as a Mac's wall clock moves
/// apart from its monotonic one when it sleeps or the system clock is
/// stepped. Clones share the shift.
#[derive(Debug, Clone)]
pub struct TestClock {
    base: u64,
    shift: Arc<AtomicI64>,
}

impl TestClock {
    /// A clock that reads `base` when `now_ms` is zero.
    pub fn new(base: u64) -> TestClock {
        TestClock {
            base,
            shift: Arc::new(AtomicI64::new(0)),
        }
    }

    /// Move the wall clock by `by_ms` without moving `now_ms`: a Mac that
    /// slept that long, or a clock stepped by that much.
    pub fn shift(&self, by_ms: i64) {
        self.shift.fetch_add(by_ms, Ordering::SeqCst);
    }
}

impl WallClock for TestClock {
    fn unix_ms(&self, now_ms: u64) -> u64 {
        let at = self.base.saturating_add(now_ms);
        at.saturating_add_signed(self.shift.load(Ordering::SeqCst))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shift_moves_the_wall_clock_and_not_now() {
        let c = TestClock::new(1_000);
        assert_eq!(c.unix_ms(5), 1_005);
        let other = c.clone();
        other.shift(600_000);
        assert_eq!(c.unix_ms(5), 601_005, "clones share the shift");
        c.shift(-601_005);
        assert_eq!(c.unix_ms(5), 0);
    }
}

//! Time, injected rather than read from the ambient world.
//!
//! Nothing in this crate calls `Timestamp::now()` directly. A bitemporal store
//! is defined by its timestamps, so tests that cannot pin time cannot assert
//! anything stable about it.

use jiff::Timestamp;
use std::sync::atomic::{AtomicI64, Ordering};

pub trait Clock: Send + Sync {
    fn now(&self) -> Timestamp;
}

/// The real clock. Used everywhere outside tests.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// A clock frozen at one instant.
pub struct FixedClock(Timestamp);

impl FixedClock {
    pub fn new(at: Timestamp) -> Self {
        Self(at)
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// A clock that advances a fixed number of milliseconds on every read.
///
/// This is the one to reach for when a test needs distinct, ordered timestamps
/// (successive facts, a supersession chain) without depending on wall-clock
/// timing or sleeping.
pub struct StepClock {
    start: Timestamp,
    step_ms: i64,
    reads: AtomicI64,
}

impl StepClock {
    pub fn new(start: Timestamp, step_ms: i64) -> Self {
        Self {
            start,
            step_ms,
            reads: AtomicI64::new(0),
        }
    }
}

impl Clock for StepClock {
    fn now(&self) -> Timestamp {
        let n = self.reads.fetch_add(1, Ordering::SeqCst);
        self.start + jiff::SignedDuration::from_millis(self.step_ms * n)
    }
}

//! Identifier generation, injected for the same reason as [`crate::clock`].
//!
//! UUIDv7 embeds a timestamp plus randomness, so it is doubly non-deterministic.
//! It is still the right choice in production -- the time prefix gives an
//! append-only fact log good B-tree locality -- but tests need a seeded variant.

use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

pub trait IdGen: Send + Sync {
    fn next_id(&self) -> Uuid;
}

/// The real generator: time-ordered UUIDv7.
pub struct UuidV7Gen;

impl IdGen for UuidV7Gen {
    fn next_id(&self) -> Uuid {
        Uuid::now_v7()
    }
}

/// A counter-backed generator producing the same sequence for the same seed.
///
/// The UUIDs are well-formed v7 values with a synthetic timestamp derived from
/// the seed, so they stay sortable and keep the shape production code expects.
pub struct SeededIdGen {
    seed: u64,
    counter: AtomicU64,
}

impl SeededIdGen {
    pub fn new(seed: u64) -> Self {
        Self {
            seed,
            counter: AtomicU64::new(0),
        }
    }
}

impl IdGen for SeededIdGen {
    fn next_id(&self) -> Uuid {
        let n = self.counter.fetch_add(1, Ordering::SeqCst);

        // Lay the counter into the 48-bit big-endian timestamp field so the
        // generated IDs sort in generation order, exactly as real v7 does.
        let mut bytes = [0u8; 16];
        bytes[0..6].copy_from_slice(&n.to_be_bytes()[2..8]);
        // Remaining bytes carry the seed, keeping distinct seeds distinct.
        bytes[6..14].copy_from_slice(&self.seed.to_be_bytes());
        bytes[14..16].copy_from_slice(&(n as u16).to_be_bytes());

        // Stamp version 7 and the RFC 4122 variant.
        bytes[6] = (bytes[6] & 0x0f) | 0x70;
        bytes[8] = (bytes[8] & 0x3f) | 0x80;

        Uuid::from_bytes(bytes)
    }
}

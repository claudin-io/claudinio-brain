//! Passo 1, part 1: the injectable non-determinism sources.
//!
//! Every later test in this project depends on time and IDs being controllable.
//! If these are wrong, the whole suite becomes intermittent.

use brain::clock::{Clock, FixedClock, StepClock, SystemClock};
use brain::ids::{IdGen, SeededIdGen, UuidV7Gen};
use jiff::Timestamp;

fn ts(s: &str) -> Timestamp {
    s.parse().expect("valid timestamp")
}

#[test]
fn fixed_clock_never_moves() {
    let c = FixedClock::new(ts("2026-07-28T12:00:00Z"));
    let a = c.now();
    let b = c.now();
    assert_eq!(a, b);
    assert_eq!(a, ts("2026-07-28T12:00:00Z"));
}

#[test]
fn step_clock_advances_by_a_fixed_amount_per_call() {
    let c = StepClock::new(ts("2026-07-28T00:00:00Z"), 1000); // 1s steps
    assert_eq!(c.now(), ts("2026-07-28T00:00:00Z"));
    assert_eq!(c.now(), ts("2026-07-28T00:00:01Z"));
    assert_eq!(c.now(), ts("2026-07-28T00:00:02Z"));
}

#[test]
fn step_clock_is_reproducible_across_instances() {
    let a = StepClock::new(ts("2026-01-01T00:00:00Z"), 500);
    let b = StepClock::new(ts("2026-01-01T00:00:00Z"), 500);
    let seq_a: Vec<_> = (0..5).map(|_| a.now()).collect();
    let seq_b: Vec<_> = (0..5).map(|_| b.now()).collect();
    assert_eq!(seq_a, seq_b);
}

#[test]
fn system_clock_moves_forward_monotonically() {
    let c = SystemClock;
    let a = c.now();
    let b = c.now();
    assert!(b >= a, "system clock went backwards: {a} then {b}");
}

#[test]
fn seeded_idgen_is_reproducible_across_instances() {
    let a = SeededIdGen::new(42);
    let b = SeededIdGen::new(42);
    let seq_a: Vec<_> = (0..5).map(|_| a.next_id()).collect();
    let seq_b: Vec<_> = (0..5).map(|_| b.next_id()).collect();
    assert_eq!(seq_a, seq_b);
}

#[test]
fn seeded_idgen_differs_by_seed_and_never_repeats_within_a_run() {
    let a = SeededIdGen::new(1);
    let b = SeededIdGen::new(2);
    assert_ne!(a.next_id(), b.next_id());

    let g = SeededIdGen::new(7);
    let ids: Vec<_> = (0..100).map(|_| g.next_id()).collect();
    let mut uniq = ids.clone();
    uniq.sort_unstable();
    uniq.dedup();
    assert_eq!(
        uniq.len(),
        ids.len(),
        "seeded id generator emitted a duplicate"
    );
}

#[test]
fn uuid_v7_gen_is_unique_and_time_ordered() {
    let g = UuidV7Gen;
    let ids: Vec<_> = (0..50).map(|_| g.next_id()).collect();
    let mut sorted = ids.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(sorted.len(), ids.len(), "uuid v7 collision");
    // v7 embeds a big-endian timestamp prefix, so generation order is sort order.
    // This is what gives us good B-tree locality on an append-only fact log.
    let mut by_value = ids.clone();
    by_value.sort_unstable();
    assert_eq!(by_value, ids, "uuid v7 was not monotonically ordered");
}

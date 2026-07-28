//! Passo 3, part 2: bitemporal invariants under arbitrary write orders.
//!
//! The example-based tests pin down the cases we thought of. These pin down the
//! ones we did not: facts arriving out of order, corrections landing on the same
//! instant, retractions interleaved with supersessions.
//!
//! These are cheap to write now and nearly impossible to retrofit once a brain
//! holds real data.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use jiff::Timestamp;
use proptest::prelude::*;

const EPOCH: &str = "2026-01-01T00:00:00Z";

fn day(n: u16) -> Timestamp {
    EPOCH.parse::<Timestamp>().unwrap() + jiff::SignedDuration::from_hours(24 * i64::from(n))
}

/// One write against a single (entity, predicate) pair.
#[derive(Debug, Clone)]
enum Op {
    /// Assert `value` valid from day `at`.
    Assert { value: u8, at: u16 },
    /// Retract whichever fact is the nth in history, if there is one.
    Retract { nth: usize },
}

fn op_strategy() -> impl Strategy<Value = Op> {
    prop_oneof![
        // Weighted towards asserts: a history of only retractions proves little.
        8 => (0u8..5, 0u16..40).prop_map(|(value, at)| Op::Assert { value, at }),
        1 => (0usize..8).prop_map(|nth| Op::Retract { nth }),
    ]
}

/// Applies a sequence of writes to a fresh brain and hands back the history.
fn run(ops: &[Op]) -> (Brain, Vec<brain::brain::Fact>) {
    let dir = tempfile::TempDir::new().unwrap();
    let b = Brain::init(
        &dir.path().join("t.db"),
        "prop",
        Box::new(StepClock::new(EPOCH.parse().unwrap(), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();

    for op in ops {
        match op {
            Op::Assert { value, at } => {
                b.remember(&Assertion::new("e", "p", Object::num(f64::from(*value))).at(day(*at)))
                    .unwrap();
            }
            Op::Retract { nth } => {
                let h = b.history("e", "p").unwrap();
                if let Some(f) = h.get(*nth) {
                    b.retract(f.id, None).unwrap();
                }
            }
        }
    }

    let history = b.history("e", "p").unwrap();
    // The TempDir must outlive the connection, so leak it deliberately: these are
    // short-lived test processes and the OS reclaims the files.
    std::mem::forget(dir);
    (b, history)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(120))]

    /// The invariant the schema's partial unique index also enforces. Asserted
    /// here too, because the index only covers what reaches SQLite -- this covers
    /// what the supersession logic decided to write.
    #[test]
    fn at_most_one_fact_is_open_at_a_time(ops in prop::collection::vec(op_strategy(), 1..12)) {
        let (_b, history) = run(&ops);
        let open = history
            .iter()
            .filter(|f| f.valid_to.is_none() && f.retracted_at.is_none())
            .count();
        prop_assert!(open <= 1, "{open} facts were open at once: {history:#?}");
    }

    #[test]
    fn every_closed_interval_is_non_empty(ops in prop::collection::vec(op_strategy(), 1..12)) {
        let (_b, history) = run(&ops);
        for f in &history {
            if let Some(to) = f.valid_to {
                prop_assert!(
                    f.valid_from < to,
                    "empty or inverted interval [{}, {})",
                    f.valid_from,
                    to
                );
            }
        }
    }

    /// Live intervals never overlap: two facts covering one instant would make
    /// "what is the value?" ambiguous, which is the failure this whole design
    /// exists to prevent.
    ///
    /// Note this asserts no-overlap rather than gapless tiling. Retracting a fact
    /// from the middle of a timeline leaves a genuine hole -- "that was never
    /// true" says nothing about what *was* -- and inventing coverage for it would
    /// be worse than admitting the gap. Contiguity under pure supersession is
    /// covered by `three_changes_produce_a_contiguous_chain` instead.
    #[test]
    fn live_intervals_never_overlap(ops in prop::collection::vec(op_strategy(), 1..12)) {
        let (_b, history) = run(&ops);
        let live: Vec<_> = history.iter().filter(|f| f.retracted_at.is_none()).collect();

        for w in live.windows(2) {
            prop_assert!(
                w[0].valid_from <= w[1].valid_from,
                "history is not ordered by valid_from"
            );
            prop_assert!(
                w[0].valid_to.is_some_and(|to| to <= w[1].valid_from),
                "interval {:?} overlaps or runs past {:?}",
                w[0],
                w[1]
            );
        }
    }

    /// `recorded_at` is transaction time: it must follow write order, never be
    /// rewritten, and never be reordered by a backdated assertion.
    #[test]
    fn recorded_at_never_decreases_with_fact_id(
        ops in prop::collection::vec(op_strategy(), 1..12)
    ) {
        let (_b, history) = run(&ops);
        let mut by_id: Vec<_> = history.iter().collect();
        by_id.sort_by_key(|f| f.id);
        for w in by_id.windows(2) {
            prop_assert!(
                w[0].recorded_at <= w[1].recorded_at,
                "recorded_at went backwards between fact {} and {}",
                w[0].id,
                w[1].id
            );
        }
    }

    /// Following `superseded_by` must always terminate: a cycle would hang any
    /// provenance walk, and a dangling id would break `why`.
    #[test]
    fn supersession_chains_terminate(ops in prop::collection::vec(op_strategy(), 1..12)) {
        let (_b, history) = run(&ops);
        let ids: std::collections::HashSet<i64> = history.iter().map(|f| f.id).collect();

        for start in &history {
            let mut seen = std::collections::HashSet::new();
            let mut cur = start.superseded_by;
            while let Some(id) = cur {
                prop_assert!(ids.contains(&id), "superseded_by {id} points at nothing");
                prop_assert!(seen.insert(id), "cycle in supersession chain at {id}");
                cur = history.iter().find(|f| f.id == id).and_then(|f| f.superseded_by);
            }
        }
    }

    /// The defining property of an as-of query, checked against the history
    /// directly rather than against the SQL that produced it.
    #[test]
    fn as_of_returns_exactly_the_fact_whose_interval_contains_the_instant(
        ops in prop::collection::vec(op_strategy(), 1..12),
        probe in 0u16..45,
    ) {
        let (b, history) = run(&ops);
        let t = day(probe);

        let expected: Vec<i64> = history
            .iter()
            .filter(|f| {
                f.retracted_at.is_none()
                    && f.valid_from <= t
                    && f.valid_to.is_none_or(|to| t < to)
            })
            .map(|f| f.id)
            .collect();

        let got = b.as_of("e", "p", t).unwrap().map(|f| f.id);
        prop_assert!(expected.len() <= 1, "more than one fact valid at {t}");
        prop_assert_eq!(got, expected.first().copied());
    }

    /// A retracted fact is gone from answers but never gone from the record.
    #[test]
    fn retraction_hides_without_deleting(ops in prop::collection::vec(op_strategy(), 1..12)) {
        let (b, history) = run(&ops);
        for f in history.iter().filter(|f| f.retracted_at.is_some()) {
            let at_start = b.as_of("e", "p", f.valid_from).unwrap();
            prop_assert_ne!(
                at_start.map(|x| x.id),
                Some(f.id),
                "a retracted fact was still answering queries"
            );
        }
    }
}

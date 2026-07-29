//! Passo 15: facts that carry their own end.
//!
//! Until now a `valid_to` could only be written by a *second* fact arriving to
//! close the first, which quietly decided what the brain was for: anything whose
//! end was known in advance -- a freeze that lifts on Friday, a token good for an
//! hour, a state nobody will come back to correct -- either had to be revisited by
//! hand or should not be recorded at all. That is the real reason short-lived
//! state was off-limits, and it was a missing verb rather than a principle.
//!
//! Time passing is simulated by reopening the same file with a later clock, which
//! is the only way to test expiry without sleeping. It also demonstrates the
//! property being claimed: nothing is written when a fact expires, so the passage
//! of time alone has to change the answer.

use brain::brain::{Assertion, Brain, BrainError, Object, WhichQuery};
use brain::clock::{FixedClock, StepClock};
use brain::ids::SeededIdGen;
use brain::recall::{Channel, RecallQuery};
use jiff::Timestamp;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    let s = if s.len() == 10 {
        format!("{s}T00:00:00Z")
    } else {
        s.to_string()
    };
    s.parse().unwrap()
}

/// A new brain whose clock stands at `now`.
fn brain(tmp: &TempDir, now: &str) -> Brain {
    Brain::init(
        &tmp.path().join("t.db"),
        "expiry",
        Box::new(StepClock::new(ts(now), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

/// The same brain, later. Nothing is written in between.
fn later(tmp: &TempDir, now: &str) -> Brain {
    Brain::open(
        &tmp.path().join("t.db"),
        Box::new(FixedClock::new(ts(now))),
        Box::new(SeededIdGen::new(2)),
    )
    .unwrap()
}

fn freeze(b: &Brain, from: &str, until: &str) {
    b.remember(
        &Assertion::new("release_1_4", "freeze", Object::text("on"))
            .at(ts(from))
            .until(ts(until)),
    )
    .unwrap();
}

fn value(b: &Brain) -> Option<String> {
    b.current("release_1_4", "freeze")
        .unwrap()
        .and_then(|f| f.object_text)
}

// --- a fact that ends on its own ----------------------------------------------

#[test]
fn a_fact_holds_until_its_end_and_then_stops_without_a_second_write() {
    let tmp = TempDir::new().unwrap();
    {
        let b = brain(&tmp, "2026-08-01");
        freeze(&b, "2026-08-01", "2026-08-15");
        assert_eq!(value(&b), Some("on".to_string()), "true the day it starts");
    }

    assert_eq!(
        value(&later(&tmp, "2026-08-14")),
        Some("on".to_string()),
        "still true the day before it lifts"
    );
    assert_eq!(
        value(&later(&tmp, "2026-08-16")),
        None,
        "nobody wrote anything, and it stopped being true anyway"
    );
}

#[test]
fn an_expired_fact_is_not_a_deleted_one() {
    // The distinction the whole design rests on: it stopped being true, which is
    // not the same as never having been true, and not the same as being gone.
    let tmp = TempDir::new().unwrap();
    {
        let b = brain(&tmp, "2026-08-01");
        freeze(&b, "2026-08-01", "2026-08-15");
    }
    let b = later(&tmp, "2026-09-01");

    let h = b.history("release_1_4", "freeze").unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].valid_to, Some(ts("2026-08-15")));
    assert!(h[0].retracted_at.is_none(), "expiring is not retracting");
    assert!(
        h[0].superseded_by.is_none(),
        "nothing replaced it -- that is what makes it self-closing"
    );

    assert_eq!(
        b.as_of("release_1_4", "freeze", ts("2026-08-10"))
            .unwrap()
            .and_then(|f| f.object_text),
        Some("on".to_string()),
        "and it is still the answer for the period it covered"
    );
}

#[test]
fn a_fact_written_already_over_is_never_current() {
    // Learned late: the freeze happened and ended before anyone recorded it.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-09-01");
    freeze(&b, "2026-08-01", "2026-08-15");

    assert_eq!(value(&b), None);
    assert_eq!(
        b.as_of("release_1_4", "freeze", ts("2026-08-10"))
            .unwrap()
            .and_then(|f| f.object_text),
        Some("on".to_string())
    );
}

#[test]
fn an_end_before_the_start_is_refused() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-08-01");
    let err = b
        .remember(
            &Assertion::new("release_1_4", "freeze", Object::text("on"))
                .at(ts("2026-08-15"))
                .until(ts("2026-08-01")),
        )
        .unwrap_err();
    assert!(
        matches!(err, BrainError::EmptyInterval { .. }),
        "expected EmptyInterval, got {err:?}"
    );

    // And the rejected write left nothing behind.
    assert!(b.history("release_1_4", "freeze").unwrap().is_empty());
}

#[test]
fn an_end_equal_to_the_start_is_refused_too() {
    // A zero-width interval is not a short fact, it is a fact that was never true
    // at any instant -- and the schema would reject it with a constraint name.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-08-01");
    assert!(matches!(
        b.remember(
            &Assertion::new("x", "p", Object::text("v"))
                .at(ts("2026-08-01"))
                .until(ts("2026-08-01")),
        )
        .unwrap_err(),
        BrainError::EmptyInterval { .. }
    ));
}

// --- narrowing, never widening ------------------------------------------------

#[test]
fn an_end_never_outlives_the_fact_that_follows_it() {
    // The author of a claim does not get to decide that it outlives the next one.
    // Two facts covering one instant is the single thing this timeline exists to
    // prevent, and an over-long `until` must lose to it.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-12-01");
    b.remember(&Assertion::new("api", "timeout", Object::num(30.0)).at(ts("2026-09-01")))
        .unwrap();
    // Backdated, and claiming to run a year past where the 30 already starts.
    b.remember(
        &Assertion::new("api", "timeout", Object::num(10.0))
            .at(ts("2026-08-01"))
            .until(ts("2027-08-01")),
    )
    .unwrap();

    let h = b.history("api", "timeout").unwrap();
    let ten = h.iter().find(|f| f.number() == Some(10.0)).unwrap();
    assert_eq!(
        ten.valid_to,
        Some(ts("2026-09-01")),
        "the end was not clipped to where the next fact starts"
    );
    assert_eq!(
        b.current("api", "timeout").unwrap().unwrap().number(),
        Some(30.0)
    );
}

#[test]
fn reasserting_with_a_later_end_pushes_it_back() {
    // The heartbeat. "Still failing, and good for another hour" is the same claim
    // reinforced, not a new one, so it extends rather than duplicating.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-08-01");
    freeze(&b, "2026-08-01", "2026-08-15");
    freeze(&b, "2026-08-01", "2026-08-30");

    let h = b.history("release_1_4", "freeze").unwrap();
    assert_eq!(h.len(), 1, "a refresh must not add a fact: {h:#?}");
    assert_eq!(h[0].valid_to, Some(ts("2026-08-30")));
    assert_eq!(h[0].reassert_count, 1);
    assert_eq!(
        h[0].valid_from,
        ts("2026-08-01"),
        "a refresh must not move the start"
    );

    assert_eq!(
        value(&later(&tmp, "2026-08-20")),
        Some("on".to_string()),
        "the extension did not take"
    );
}

#[test]
fn reasserting_with_an_earlier_end_does_not_pull_it_in() {
    // Shortening on reassert would let a heartbeat kill the thing it was sent to
    // keep alive. Ending something early is a change, and a change is a write.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-08-01");
    freeze(&b, "2026-08-01", "2026-08-30");
    freeze(&b, "2026-08-01", "2026-08-05");

    let h = b.history("release_1_4", "freeze").unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].valid_to, Some(ts("2026-08-30")));
}

#[test]
fn reasserting_an_open_ended_fact_with_an_end_leaves_it_open() {
    // Same rule seen from the other side: unbounded is the longest end there is,
    // so nothing a reassertion says can shorten it.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-08-01");
    b.remember(&Assertion::new("api", "timeout", Object::num(30.0)).at(ts("2026-08-01")))
        .unwrap();
    b.remember(
        &Assertion::new("api", "timeout", Object::num(30.0))
            .at(ts("2026-08-01"))
            .until(ts("2026-08-05")),
    )
    .unwrap();

    let h = b.history("api", "timeout").unwrap();
    assert_eq!(h.len(), 1);
    assert_eq!(h[0].valid_to, None);
}

// --- the rest of the brain has to agree ---------------------------------------

#[test]
fn a_set_query_drops_a_fact_that_expired() {
    let tmp = TempDir::new().unwrap();
    {
        let b = brain(&tmp, "2026-08-01");
        b.remember(
            &Assertion::new("ci", "state", Object::text("failing"))
                .at(ts("2026-08-01"))
                .until(ts("2026-08-02")),
        )
        .unwrap();
        b.remember(
            &Assertion::new("deploy", "state", Object::text("failing")).at(ts("2026-08-01")),
        )
        .unwrap();
    }

    let q = WhichQuery::new("state").value(Object::text("failing"));
    let subjects = |b: &Brain| -> Vec<String> {
        b.which(&q)
            .unwrap()
            .facts
            .into_iter()
            .map(|f| f.entity_key)
            .collect()
    };

    assert_eq!(
        subjects(&later(&tmp, "2026-08-01T12:00:00Z")),
        ["ci", "deploy"]
    );
    assert_eq!(
        subjects(&later(&tmp, "2026-08-03")),
        ["deploy"],
        "the expired one is still on the list"
    );
}

#[test]
fn an_expiring_fact_is_findable_by_meaning_while_it_holds() {
    // The failure this guards against is silent and one-channel-wide: the vector
    // index stores whether a fact is open, and reading that as "has no valid_to"
    // would hide every self-expiring fact from the only channel that can match a
    // paraphrase -- while words, names and the graph all still found it.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-08-01");
    freeze(&b, "2026-08-01", "2026-08-15");

    let hits = b
        .recall(
            &RecallQuery::new("release_1_4 freeze on")
                .channels(&[Channel::Semantic])
                .limit(5),
        )
        .unwrap();
    assert!(
        !hits.is_empty(),
        "the semantic channel cannot see a fact that has not expired yet"
    );
}

#[test]
fn a_reindex_keeps_an_unexpired_fact_reachable() {
    // `reindex` recomputes the open flag from scratch, so it is a second place
    // that decides what "open" means. If it disagreed with the write path, the
    // repair tool would be the thing that broke the index.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "2026-08-01");
    freeze(&b, "2026-08-01", "2026-08-15");
    b.reindex().unwrap();

    let hits = b
        .recall(
            &RecallQuery::new("release_1_4 freeze on")
                .channels(&[Channel::Semantic])
                .limit(5),
        )
        .unwrap();
    assert!(!hits.is_empty(), "a reindex made it unreachable");
}

#[test]
fn a_multi_valued_predicate_can_expire_one_value_at_a_time() {
    let tmp = TempDir::new().unwrap();
    {
        let b = brain(&tmp, "2026-08-01");
        b.remember(
            &Assertion::new("oncall", "member", Object::text("maria"))
                .at(ts("2026-08-01"))
                .until(ts("2026-08-08"))
                .cardinality(brain::brain::Cardinality::Multi),
        )
        .unwrap();
        b.remember(&Assertion::new("oncall", "member", Object::text("joao")).at(ts("2026-08-01")))
            .unwrap();
        assert_eq!(b.current_all("oncall", "member").unwrap().len(), 2);
    }

    let b = later(&tmp, "2026-08-10");
    let held: Vec<_> = b
        .current_all("oncall", "member")
        .unwrap()
        .into_iter()
        .filter_map(|f| f.object_text)
        .collect();
    assert_eq!(held, ["joao"], "one rotation ended, the other did not");
}

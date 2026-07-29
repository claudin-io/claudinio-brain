//! Passo 11: reaching what nothing connects.
//!
//! The graph answers "what is this connected to". Kinship answers "what else is
//! like this", and the difference is the whole point: almost nothing in a real
//! brain has an edge to its siblings. Twenty vouchers each recording `is_a
//! voucher_sazonal` are a cohort nobody ever drew.
//!
//! The cases that matter most are the ones where the shared value is a plain
//! string, because that is the brain this was built for -- one where 59 of 69
//! `is_a` facts were written as text and no walk could follow any of them.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
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

fn brain(tmp: &TempDir) -> Brain {
    Brain::init(
        &tmp.path().join("t.db"),
        "kin",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

fn text(b: &Brain, subject: &str, predicate: &str, value: &str) {
    b.remember(&Assertion::new(subject, predicate, Object::text(value)))
        .unwrap();
}

fn num(b: &Brain, subject: &str, predicate: &str, value: f64) {
    b.remember(&Assertion::new(subject, predicate, Object::num(value)))
        .unwrap();
}

/// The statements the kin channel returns on its own, best first.
fn kin_only(b: &Brain, query: &str) -> Vec<String> {
    let q = RecallQuery::new(query).limit(20).channels(&[Channel::Kin]);
    b.recall(&q)
        .unwrap()
        .into_iter()
        .map(|h| h.fact.statement)
        .collect()
}

// --- the motivating case -------------------------------------------------------

#[test]
fn a_class_stored_as_a_string_still_makes_a_cohort() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // No edges anywhere. This is exactly the brain that could not be walked.
    text(&b, "voucher_a", "is_a", "voucher_sazonal");
    text(&b, "voucher_b", "is_a", "voucher_sazonal");
    num(&b, "voucher_b", "percent_off", 50.0);
    num(&b, "plano_pro", "monthly_price", 59.0);

    let hits = kin_only(&b, "voucher_a");
    assert!(
        hits.iter().any(|s| s.starts_with("voucher_b")),
        "the sibling should be reachable: {hits:?}"
    );
    assert!(
        !hits.iter().any(|s| s.starts_with("plano_pro")),
        "an entity sharing nothing must not be reached: {hits:?}"
    );
}

#[test]
fn the_cohort_survives_the_class_becoming_a_real_edge() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // An entity object keeps its label in `object_text`, which is what lets a
    // cohort keep working across the moment somebody links it properly. Nobody
    // should have to re-learn their brain to repair it.
    b.link("voucher_a", "is_a", "voucher_sazonal", None)
        .unwrap();
    b.link("voucher_b", "is_a", "voucher_sazonal", None)
        .unwrap();
    num(&b, "voucher_b", "percent_off", 50.0);

    let hits = kin_only(&b, "voucher_a");
    assert!(
        hits.iter().any(|s| s.contains("voucher_b percent_off")),
        "{hits:?}"
    );
}

// --- rarity is the mechanism ---------------------------------------------------

#[test]
fn the_rare_pair_outranks_the_common_one() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // The anchor shares `valido true` with five vouchers and `percent_off 50`
    // with one. Cohorts and noise are the same size in a real brain, so no
    // threshold can separate them -- only the ordering can.
    text(&b, "voucher_a", "valido", "true");
    num(&b, "voucher_a", "percent_off", 50.0);
    num(&b, "voucher_c", "percent_off", 50.0);
    for other in [
        "voucher_d",
        "voucher_e",
        "voucher_f",
        "voucher_g",
        "voucher_h",
    ] {
        text(&b, other, "valido", "true");
    }

    let hits = kin_only(&b, "voucher_a");
    let first = hits.first().expect("some kin");
    assert!(
        first.starts_with("voucher_c"),
        "the voucher sharing the rare pair should come first, got {first:?} from {hits:?}"
    );
}

#[test]
fn a_pair_every_entity_holds_reaches_nothing() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // `ativo true` on everything carries no information about which entity is
    // relevant. Weighted at zero, these still appear -- kinship is real -- but
    // they must never be the reason a genuine answer is pushed out.
    for s in ["api_gateway", "checkout", "payments", "billing"] {
        text(&b, s, "ativo", "true");
    }
    num(&b, "api_gateway", "timeout", 30.0);

    let q = RecallQuery::new("qual e o timeout do api_gateway").limit(5);
    let hits = b.recall(&q).unwrap();
    assert_eq!(
        hits.first().map(|h| h.fact.statement.as_str()),
        Some("api_gateway timeout 30"),
        "{:?}",
        hits.iter().map(|h| &h.fact.statement).collect::<Vec<_>>()
    );
}

// --- what kinship must not do --------------------------------------------------

#[test]
fn an_entity_is_never_its_own_kin() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    num(&b, "voucher_a", "percent_off", 50.0);
    num(&b, "voucher_c", "percent_off", 50.0);

    let hits = kin_only(&b, "voucher_a");
    assert!(
        !hits.iter().any(|s| s.starts_with("voucher_a")),
        "the anchor's own facts belong to the content channels: {hits:?}"
    );
}

#[test]
fn a_value_that_closed_stops_making_kin() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // The anchor was 50% off and is now 10%. `voucher_b` is still 50%, and is
    // kin to what the anchor *was*, which is not what "now" means.
    num(&b, "voucher_b", "percent_off", 50.0);
    b.remember(&{
        let mut a = Assertion::new("voucher_a", "percent_off", Object::num(50.0));
        a.valid_from = Some(ts("2026-01-01"));
        a
    })
    .unwrap();
    b.remember(&{
        let mut a = Assertion::new("voucher_a", "percent_off", Object::num(10.0));
        a.valid_from = Some(ts("2026-06-01"));
        a
    })
    .unwrap();
    num(&b, "voucher_c", "percent_off", 10.0);

    let hits = kin_only(&b, "voucher_a");
    assert!(
        hits.iter().any(|s| s.starts_with("voucher_c")),
        "the voucher sharing the current value is kin: {hits:?}"
    );
    assert!(
        !hits.iter().any(|s| s.starts_with("voucher_b")),
        "a voucher sharing only a closed value is not kin now: {hits:?}"
    );
}

#[test]
fn a_value_only_one_entity_holds_makes_no_kin() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "auth", "strategy", "server-side sessions");
    text(&b, "cache", "strategy", "write-through");

    assert!(kin_only(&b, "auth").is_empty());
}

#[test]
fn a_question_naming_nothing_known_returns_nothing_rather_than_erroring() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    num(&b, "voucher_a", "percent_off", 50.0);

    assert!(kin_only(&b, "").is_empty());
    assert!(kin_only(&b, "!!! ???").is_empty());
}

// --- the channel in the fusion -------------------------------------------------

#[test]
fn kinship_reaches_across_two_cohorts_a_shared_label_joins() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // A plan and a seat plan, joined by nothing but the word on their label.
    // In the brain this came from, that pair was invisible to every channel.
    text(&b, "plano_base", "label", "Base");
    text(&b, "lugar_base", "label", "Base");
    num(&b, "lugar_base", "lugares_minimos", 3.0);
    text(&b, "v2_pro", "label", "Pro");

    let hits = kin_only(&b, "plano_base");
    assert!(hits.iter().any(|s| s.contains("lugar_base")), "{hits:?}");
    assert!(!hits.iter().any(|s| s.contains("v2_pro")), "{hits:?}");
}

#[test]
fn results_are_ordered_deterministically_across_repeated_runs() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    for s in ["voucher_a", "voucher_b", "voucher_c", "voucher_d"] {
        text(&b, s, "is_a", "voucher_sazonal");
        num(&b, s, "percent_off", 25.0);
    }

    let first = kin_only(&b, "voucher_a");
    for _ in 0..5 {
        assert_eq!(kin_only(&b, "voucher_a"), first);
    }
}

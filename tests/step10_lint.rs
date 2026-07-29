//! Passo 10: the brain reporting what is wrong with itself.
//!
//! Every case here reproduces a defect that succeeds. Nothing errors, nothing is
//! lost, and retrieval still cannot use what was written -- which is precisely
//! why a command has to say so. The shapes come from a real brain where 59 of 69
//! `is_a` facts carried a string where an entity belonged, and where the only
//! symptom was a 3D scene full of loose dots.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::lint;
use jiff::Timestamp;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn brain(tmp: &TempDir) -> Brain {
    Brain::init(
        &tmp.path().join("t.db"),
        "lint",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

fn text(b: &Brain, subject: &str, predicate: &str, value: &str) {
    b.remember(&Assertion::new(subject, predicate, Object::text(value)))
        .unwrap();
}

fn check(b: &Brain) -> lint::Report {
    lint::check(b.store().conn()).unwrap()
}

// --- relations written as strings ---------------------------------------------

#[test]
fn a_predicate_used_both_ways_is_reported_with_both_counts() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // The shape that started this: some writers passed an entity, most passed a
    // string, and every string one is invisible to the graph.
    b.link("v2_pro", "is_a", "plano", None).unwrap();
    text(&b, "cupao_a", "is_a", "cupao_stripe");
    text(&b, "cupao_b", "is_a", "cupao_stripe");

    let report = check(&b);
    let mixed = report
        .mixed_predicates
        .iter()
        .find(|m| m.predicate == "is_a")
        .expect("is_a reported as mixed");
    assert_eq!(mixed.as_entity, 1);
    assert_eq!(mixed.as_text, 2);
    assert!(mixed.examples.contains(&"cupao_stripe".to_string()));
}

#[test]
fn a_predicate_used_one_way_only_is_not_a_finding() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    b.link("a", "depends_on", "b", None).unwrap();
    b.link("c", "depends_on", "d", None).unwrap();
    text(&b, "a", "owner", "platform-team");

    assert!(check(&b).mixed_predicates.is_empty());
}

#[test]
fn an_entity_valued_object_is_not_mistaken_for_text() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // An edge stores the object's label in `object_text` as well, so a check
    // written against that column would call every relation a string and report
    // a brain that is entirely broken. This is the guard on that mistake.
    b.link("a", "is_a", "thing", None).unwrap();
    b.link("b", "is_a", "thing", None).unwrap();

    assert!(check(&b).mixed_predicates.is_empty());
}

// --- classes that were never made into nodes ----------------------------------

#[test]
fn a_string_several_entities_share_is_a_candidate_class() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "cupao_a", "is_a", "cupao_stripe");
    text(&b, "cupao_b", "is_a", "cupao_stripe");
    text(&b, "cupao_c", "is_a", "cupao_stripe");

    let found = check(&b);
    let class = found.candidate_classes.first().expect("a candidate class");
    assert_eq!(class.value, "cupao_stripe");
    assert_eq!(class.entities, 3);
}

#[test]
fn a_string_only_one_entity_holds_is_not_a_class() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "auth", "strategy", "server-side sessions");
    text(&b, "cache", "strategy", "write-through");

    assert!(check(&b).candidate_classes.is_empty());
}

#[test]
fn booleans_and_prose_are_not_reported_as_classes() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // Flags are the most-shared strings in any brain. Left in, they would bury
    // the real candidates.
    text(&b, "cupao_a", "ativo", "true");
    text(&b, "cupao_b", "ativo", "true");

    let essay = "a".repeat(200);
    text(&b, "doc_a", "resumo", &essay);
    text(&b, "doc_b", "resumo", &essay);

    assert!(check(&b).candidate_classes.is_empty());
}

#[test]
fn a_string_that_already_names_an_entity_is_not_a_candidate_class() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // `plano` exists as a node, so this is somebody forgetting to link, which is
    // the mixed-predicate finding rather than a missing class.
    b.link("v2_pro", "is_a", "plano", None).unwrap();
    text(&b, "v2_lite", "is_a", "plano");
    text(&b, "v2_ultra", "is_a", "plano");

    assert!(check(&b).candidate_classes.is_empty());
}

// --- entities nothing can reach ------------------------------------------------

#[test]
fn an_entity_with_no_relation_is_unreachable() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    b.link("a", "depends_on", "b", None).unwrap();
    text(&b, "lonely", "nota", "knows things, connects to nothing");

    let keys: Vec<String> = check(&b).orphans.into_iter().map(|o| o.key).collect();
    assert_eq!(keys, vec!["lonely".to_string()]);
}

#[test]
fn a_relation_that_closed_stops_counting_as_connectivity() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // `fornecido_por` is single-valued, so the second link closes the first and
    // `acme` is left with nothing open pointing at it. An entity reachable only
    // through history is not reachable now, and that is what retrieval sees.
    b.link("produto", "fornecido_por", "acme", Some(ts("2026-01-01T00:00:00Z")))
        .unwrap();
    b.link("produto", "fornecido_por", "globex", Some(ts("2026-06-01T00:00:00Z")))
        .unwrap();

    let keys: Vec<String> = check(&b).orphans.into_iter().map(|o| o.key).collect();
    assert_eq!(keys, vec!["acme".to_string()]);
}

#[test]
fn being_pointed_at_counts_as_being_connected() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // A relation is a road, not a one-way street -- the same rule the walk obeys.
    b.link("produto", "fornecido_por", "acme", None).unwrap();

    assert!(check(&b).orphans.is_empty());
}

// --- one thing under two names -------------------------------------------------

#[test]
fn a_spelling_that_drifted_is_reported() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "plano_de_lugar", "nota", "singular");
    text(&b, "planos_de_lugar", "nota", "plural");

    let twins = check(&b).twins;
    assert_eq!(twins.len(), 1);
    assert_eq!(twins[0].distance, 1);
}

#[test]
fn parallel_family_names_are_not_twins() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // Two families naming their members the same way is good naming. A rule that
    // matched on the shared suffix would report every one of these.
    text(&b, "claudinio_senior", "nota", "a");
    text(&b, "claudius_senior", "nota", "b");
    text(&b, "claudinio_associate", "nota", "c");
    text(&b, "claudius_associate", "nota", "d");

    assert!(check(&b).twins.is_empty());
}

#[test]
fn numbered_members_of_a_series_are_not_twins() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // Nobody mistypes 25 as 10; numbering is how a series gets named.
    text(&b, "codigo_upgrade25_liu", "percent_off", "25");
    text(&b, "codigo_upgrade10_liu", "percent_off", "10");

    assert!(check(&b).twins.is_empty());
}

#[test]
fn two_entities_a_relation_already_joins_are_not_twins() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // A code and the coupon it redeems look like one thing spelled twice right up
    // until you notice the edge, at which point they are two things modelled
    // correctly.
    b.link("codigo_promo", "resgata", "codigo_prom", None).unwrap();

    assert!(check(&b).twins.is_empty());
}

// --- the whole report ----------------------------------------------------------

#[test]
fn a_well_formed_brain_is_clean() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    b.link("v2_pro", "is_a", "plano_claudinio", None).unwrap();
    b.link("v2_lite", "is_a", "plano_claudinio", None).unwrap();
    b.remember(&Assertion::new(
        "v2_pro",
        "monthly_price",
        Object::num(59.0),
    ))
    .unwrap();

    let report = check(&b);
    assert!(report.is_clean(), "unexpected findings: {report:?}");
    assert_eq!(report.entities, 3);
    assert_eq!(report.edges, 2);
}

#[test]
fn an_empty_brain_is_clean() {
    let tmp = TempDir::new().unwrap();
    let report = check(&brain(&tmp));

    assert!(report.is_clean());
    assert_eq!(report.entities, 0);
    assert_eq!(report.facts, 0);
}

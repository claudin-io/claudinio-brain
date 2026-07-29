//! Passo 16: keeping a namespace out of the answer.
//!
//! `scope` could only ever narrow inward. That is enough while every fact in a
//! brain is the same kind of thing, and it stops being enough the moment one holds
//! something high-churn beside the durable knowledge: a task list turning over
//! three times per task, a run of build states. Nothing here is ever deleted, and
//! every fact lands in the one full-text index and the one vector index that every
//! question is searched through -- so the churn does not fade, it accumulates, and
//! it competes forever with the facts somebody actually wanted to keep.
//!
//! The asymmetry was the bug: you could ask *only* about the noise and never ask
//! about everything except it.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::{Channel, RecallQuery};
use jiff::Timestamp;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    format!("{s}T00:00:00Z").parse().unwrap()
}

fn brain(tmp: &TempDir) -> Brain {
    Brain::init(
        &tmp.path().join("t.db"),
        "scope",
        Box::new(StepClock::new(ts("2026-08-01"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

/// A brain holding one durable fact about the deploy and a pile of task churn
/// whose words collide with it.
fn mixed(tmp: &TempDir) -> Brain {
    let b = brain(tmp);
    b.remember(
        &Assertion::new("deploy", "strategy", Object::text("blue green rollout"))
            .at(ts("2026-02-01")),
    )
    .unwrap();
    for i in 0..20 {
        b.remember(
            &Assertion::new(
                format!("task_{i:02}"),
                "about",
                Object::text("deploy strategy"),
            )
            .at(ts("2026-02-01"))
            .scope("todo"),
        )
        .unwrap();
    }
    b
}

fn statements(b: &Brain, q: &RecallQuery) -> Vec<String> {
    b.recall(q)
        .unwrap()
        .into_iter()
        .map(|h| h.fact.statement)
        .collect()
}

#[test]
fn the_churn_crowds_out_the_answer_until_it_is_excluded() {
    let tmp = TempDir::new().unwrap();
    let b = mixed(&tmp);

    let asked = RecallQuery::new("deploy strategy").limit(5);
    let crowded = statements(&b, &asked);
    assert!(
        crowded.iter().any(|s| s.starts_with("task_")),
        "the premise of this test is gone: {crowded:?}"
    );

    let quiet = statements(&b, &asked.clone().not_scope("todo"));
    assert!(
        !quiet.iter().any(|s| s.starts_with("task_")),
        "an excluded namespace still answered: {quiet:?}"
    );
    assert_eq!(
        quiet,
        ["deploy strategy blue green rollout"],
        "and what is left is the fact that was being buried"
    );
}

#[test]
fn an_unscoped_fact_survives_an_exclusion() {
    // The bug this guards against is a SQL one and it is total: `scope <> 'todo'`
    // is NULL for a fact with no scope, so a plain inequality would drop every
    // unscoped fact in the brain -- which is most of it.
    let tmp = TempDir::new().unwrap();
    let b = mixed(&tmp);

    let hits = statements(
        &b,
        &RecallQuery::new("deploy strategy")
            .limit(10)
            .not_scope("todo"),
    );
    assert_eq!(hits.len(), 1, "unscoped facts were swept up: {hits:?}");
}

#[test]
fn excluding_a_namespace_nothing_is_in_changes_nothing() {
    let tmp = TempDir::new().unwrap();
    let b = mixed(&tmp);

    let asked = RecallQuery::new("deploy strategy").limit(5);
    assert_eq!(
        statements(&b, &asked.clone().not_scope("no_such_scope")),
        statements(&b, &asked)
    );
}

#[test]
fn every_channel_honours_the_exclusion() {
    // The temporal filter is shared by words, names, the walk and kinship, so
    // those come along for free. The semantic channel does not: `scope` is a vec0
    // partition key, which indexes equality and cannot express an exclusion, so
    // that one filter had to be applied after the search. A channel that ignored
    // it would leak the noise back in on exactly the questions where meaning
    // matters most.
    let tmp = TempDir::new().unwrap();
    let b = mixed(&tmp);

    for channel in [Channel::Bm25, Channel::Alias, Channel::Semantic] {
        let hits = statements(
            &b,
            &RecallQuery::new("deploy strategy")
                .limit(10)
                .channels(&[channel])
                .not_scope("todo"),
        );
        assert!(
            !hits.iter().any(|s| s.starts_with("task_")),
            "{channel:?} leaked an excluded fact: {hits:?}"
        );
    }
}

#[test]
fn narrowing_and_excluding_are_still_independent() {
    let tmp = TempDir::new().unwrap();
    let b = mixed(&tmp);

    let only_todo = statements(
        &b,
        &RecallQuery::new("deploy strategy").limit(30).scope("todo"),
    );
    assert!(
        only_todo.iter().all(|s| s.starts_with("task_")),
        "narrowing inward broke: {only_todo:?}"
    );
    assert_eq!(only_todo.len(), 20);
}

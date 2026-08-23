//! Passo 19: showing the arithmetic.
//!
//! `recall` fuses five channels and then multiplies the result by up to three
//! re-ranking rules. Every one of those steps is a judgement about intent, and
//! until now the only way to interrogate a surprising ranking was `--channels`:
//! ask the question again with a retriever switched off and compare. That
//! answers *what found this*. It cannot answer *why this outranks that*, because
//! the contest is settled between the channels and after them -- and a bisection
//! is not an explanation, it is a second experiment.
//!
//! Two claims under test:
//!
//! - **The explanation is the arithmetic, not a story about it.** `fused` is the
//!   sum of the votes, and `score` is `fused` times every factor reported. If a
//!   fourth rule is ever added and forgotten here, that identity breaks.
//! - **Asking does not change the answer.** An explained recall returns exactly
//!   the ranking an unexplained one does.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::RecallQuery;
use jiff::Timestamp;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    format!("{s}T00:00:00Z").parse().unwrap()
}

/// The brain from the README's graph example: the answer to "which region" sits
/// on an entity the question never names, one hop past the one it does.
fn brain(tmp: &TempDir) -> Brain {
    let b = Brain::init(
        &tmp.path().join("t.db"),
        "explain",
        Box::new(StepClock::new(ts("2026-07-01"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    b.link("checkout_service", "depends_on", "payments_db", None)
        .unwrap();
    b.remember(&Assertion::new(
        "checkout_service",
        "owner",
        Object::entity("platform-team"),
    ))
    .unwrap();
    b.remember(&Assertion::new(
        "payments_db",
        "region",
        Object::text("eu-west-1"),
    ))
    .unwrap();
    b
}

const QUESTION: &str = "which region does checkout_service data live in";

#[test]
fn the_parts_add_up_to_the_score() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    let hits = b
        .recall(&RecallQuery::new(QUESTION).limit(5).explaining())
        .unwrap();
    assert!(!hits.is_empty());

    for h in &hits {
        let x = h.explain.as_ref().expect("asked for, so present");

        // Every channel on the hit voted, and every vote is a channel on the hit.
        // The two lists are built in different places and would drift apart
        // silently.
        let mut voted: Vec<_> = x.votes.iter().map(|v| v.channel).collect();
        voted.sort_unstable();
        voted.dedup();
        assert_eq!(voted, h.channels, "{}", h.fact.statement);

        let summed: f64 = x.votes.iter().map(|v| v.points).sum();
        assert!((summed - x.fused).abs() < 1e-12, "{}", h.fact.statement);

        let expected = x.demotions.iter().fold(x.fused, |acc, d| acc * d.factor);
        assert!(
            (expected - h.score).abs() < 1e-12,
            "{}: {} votes and {} rules do not make {}",
            h.fact.statement,
            x.fused,
            x.demotions.len(),
            h.score,
        );
    }
}

/// The bridge demotion is the one a reader is most likely to be surprised by,
/// because the demoted fact is the one that literally contains the words of the
/// question. It should be named in the answer rather than left to be guessed.
#[test]
fn a_crossed_edge_says_it_was_a_road() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    let hits = b
        .recall(&RecallQuery::new(QUESTION).limit(5).explaining())
        .unwrap();

    let bridge = hits
        .iter()
        .find(|h| h.fact.statement.contains("depends_on"))
        .expect("the edge that was crossed is in the answer");
    let rules: Vec<_> = bridge
        .explain
        .as_ref()
        .unwrap()
        .demotions
        .iter()
        .map(|d| d.rule.as_str())
        .collect();
    assert!(rules.contains(&"bridge"), "{rules:?}");

    // And the destination -- the fact the walk was crossing that edge to reach --
    // is not demoted for anything.
    let answer = hits
        .iter()
        .find(|h| h.fact.statement.contains("eu-west-1"))
        .expect("the answer is in the answer");
    assert!(answer.explain.as_ref().unwrap().demotions.is_empty());
    assert!(answer.score > bridge.score);
}

/// Asking for the arithmetic must not be a different question.
#[test]
fn explaining_does_not_change_the_ranking() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    let plain = b.recall(&RecallQuery::new(QUESTION).limit(5)).unwrap();
    let explained = b
        .recall(&RecallQuery::new(QUESTION).limit(5).explaining())
        .unwrap();

    let ids = |hs: &[brain::recall::Hit]| hs.iter().map(|h| h.fact.id).collect::<Vec<_>>();
    let scores = |hs: &[brain::recall::Hit]| hs.iter().map(|h| h.score).collect::<Vec<_>>();
    assert_eq!(ids(&plain), ids(&explained));
    assert_eq!(scores(&plain), scores(&explained));

    // And a caller that did not ask gets nothing rather than something empty:
    // "no explanation" must not read as "explained, and nothing happened".
    assert!(plain.iter().all(|h| h.explain.is_none()));
}

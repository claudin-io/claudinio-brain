//! Passo 8: a question the brain cannot answer is not a question it may fail on.
//!
//! `recall` takes free text from an agent, and an agent will eventually hand it
//! something that is not a question: a stack trace, a pasted file, a whole
//! transcript. The honest answer to those is "nothing relevant", and the one
//! answer that helps nobody is an error, because a `Result::Err` out of recall
//! looks like a broken brain rather than a bad query.
//!
//! Before the cap in `MAX_QUERY_WORDS`, a twenty-thousand-word query failed with
//! *"too many SQL variables"*: every word yields up to two identity terms, the
//! alias channel binds each twice, and SQLite takes 32766 parameters.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::RecallQuery;
use tempfile::TempDir;

fn brain_with_facts() -> (TempDir, Brain) {
    let tmp = TempDir::new().unwrap();
    let brain = Brain::init(
        &tmp.path().join("t.db"),
        "teste",
        Box::new(StepClock::new(
            "2026-01-01T00:00:00Z".parse().unwrap(),
            1000,
        )),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    for i in 0..20 {
        brain
            .remember(&Assertion::new(
                format!("produto_{i}"),
                "preco",
                Object::num(f64::from(i)),
            ))
            .unwrap();
    }
    (tmp, brain)
}

#[test]
fn a_question_far_longer_than_a_question_still_answers() {
    let (_tmp, brain) = brain_with_facts();
    for words in [1_000usize, 20_000] {
        let q: String = std::iter::repeat_n("palavra", words)
            .enumerate()
            .map(|(i, w)| format!("{w}{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        let hits = brain
            .recall(&RecallQuery::new(&q))
            .unwrap_or_else(|e| panic!("{words} words: {e}"));
        assert!(
            hits.is_empty(),
            "{words} words of nonsense should match nothing, got {hits:?}"
        );
    }
}

#[test]
fn the_answer_survives_being_buried_in_a_pasted_document() {
    let (_tmp, brain) = brain_with_facts();
    // The name comes first and the document follows, which is the shape of a
    // question with context stapled to it. Everything past the cut is dropped in
    // order, so the part that names something is the part that is read.
    let mut q = String::from("preco do produto_7 ");
    for i in 0..5_000 {
        q.push_str(&format!("ruido{i} "));
    }
    let hits = brain.recall(&RecallQuery::new(&q)).unwrap();
    assert_eq!(
        hits.first().map(|h| h.fact.statement.as_str()),
        Some("produto_7 preco 7")
    );
}

#[test]
fn the_same_long_text_always_asks_the_same_question() {
    let (_tmp, brain) = brain_with_facts();
    let q: String = (0..3_000)
        .map(|i| format!("termo{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let once = brain.recall(&RecallQuery::new(&q)).unwrap();
    let twice = brain.recall(&RecallQuery::new(&q)).unwrap();
    let ids = |h: &[brain::recall::Hit]| h.iter().map(|x| x.fact.id).collect::<Vec<_>>();
    assert_eq!(ids(&once), ids(&twice));
}

#[test]
fn a_query_of_punctuation_and_control_characters_is_not_an_error() {
    let (_tmp, brain) = brain_with_facts();
    for q in [
        "\"\"\" NEAR( AND OR NOT *",
        "\0\u{1}\u{2}",
        "((((((((((",
        "   \t\n  ",
        "🧠🧠🧠",
        "-",
    ] {
        let hits = brain
            .recall(&RecallQuery::new(q))
            .unwrap_or_else(|e| panic!("{q:?}: {e}"));
        assert!(hits.is_empty(), "{q:?} matched something: {hits:?}");
    }
}

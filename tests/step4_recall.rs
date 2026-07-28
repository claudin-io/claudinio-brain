//! Passo 4: lexical recall.
//!
//! Two channels, fused with reciprocal rank fusion:
//!
//! - **BM25** over the statement and search text, via FTS5
//! - **alias match**, an exact hit on a normalized entity key -- cheap and very
//!   high precision, and the layer that does the work embeddings fumble
//!
//! Recall is temporal by default: it answers with what is true now, not with
//! everything ever recorded.

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
    s.parse().expect("valid timestamp")
}

struct Fixture {
    _tmp: TempDir,
    brain: Brain,
}

impl Fixture {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let brain = Brain::init(
            &tmp.path().join("t.db"),
            "teste",
            Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
            Box::new(SeededIdGen::new(1)),
        )
        .unwrap();
        Self { _tmp: tmp, brain }
    }

    fn say(&self, subject: &str, predicate: &str, value: &str, at: &str) {
        let obj = match value.parse::<f64>() {
            Ok(n) => Object::num(n),
            Err(_) => Object::text(value),
        };
        self.brain
            .remember(&Assertion::new(subject, predicate, obj).at(ts(at)))
            .unwrap();
    }

    fn ask(&self, q: &str) -> Vec<String> {
        self.hits(&RecallQuery::new(q))
    }

    fn hits(&self, q: &RecallQuery) -> Vec<String> {
        self.brain
            .recall(q)
            .unwrap()
            .into_iter()
            .map(|h| h.fact.statement)
            .collect()
    }
}

// --- the basics ---------------------------------------------------------------

#[test]
fn a_term_from_the_statement_finds_the_fact() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    f.say("servidor_web", "porta", "8080", "2026-07-01");
    // Several unrelated rows so FTS5's IDF term is not degenerate: with a
    // two-document corpus it collapses to exactly zero and ranks nothing.
    for i in 0..5 {
        f.say(&format!("ruido_{i}"), "campo", "valor", "2026-07-01");
    }

    let hits = f.ask("preco");
    assert!(!hits.is_empty(), "no hit for a literal term");
    assert!(hits[0].contains("preco"), "top hit was {:?}", hits[0]);
}

#[test]
fn searching_is_accent_insensitive_even_though_identity_is_not() {
    // `preço` and `preco` are different *keys* -- see norm::key -- but a question
    // typed without the cedilla must still find the fact. That split is
    // deliberate: identity is exact, search is forgiving.
    let f = Fixture::new();
    f.say("produto_a", "preço", "20", "2026-07-01");
    for i in 0..5 {
        f.say(&format!("ruido_{i}"), "campo", "valor", "2026-07-01");
    }

    assert!(
        !f.ask("preco").is_empty(),
        "accent-stripped query found nothing"
    );
    assert!(!f.ask("preço").is_empty(), "accented query found nothing");
}

#[test]
fn an_entity_name_matches_through_the_alias_channel() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    for i in 0..8 {
        f.say(&format!("outro_{i}"), "preco", "5", "2026-07-01");
    }

    // "Produto A" normalizes to the same key as the stored entity, so the alias
    // channel pins it exactly even though "preco" matches nine facts.
    let hits = f
        .brain
        .recall(&RecallQuery::new("preco do Produto A"))
        .unwrap();
    assert!(
        hits[0].fact.entity_key == "produto_a",
        "top hit was {:?}",
        hits[0].fact.statement
    );
    assert!(
        hits[0].channels.contains(&Channel::Alias),
        "the alias channel did not fire: {:?}",
        hits[0].channels
    );
}

#[test]
fn a_query_matching_nothing_is_an_empty_answer_not_an_error() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    assert!(f.ask("xyzzy nada disso").is_empty());
}

#[test]
fn an_empty_or_punctuation_only_query_returns_nothing_without_erroring() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    for q in ["", "   ", "???", "--"] {
        assert!(f.ask(q).is_empty(), "query {q:?} should return nothing");
    }
}

#[test]
fn fts_syntax_in_a_query_is_treated_as_text_not_as_operators() {
    // An agent will eventually pass a raw user question through. FTS5 would choke
    // on unbalanced quotes or a bare `NEAR(`, and a crash here would look like a
    // brain failure rather than a query-parsing one.
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    for q in [
        "preco \" unbalanced",
        "preco AND OR NOT",
        "NEAR(preco",
        "preco*",
        "produto_a: preco?",
        "^preco",
    ] {
        let r = f.brain.recall(&RecallQuery::new(q));
        assert!(r.is_ok(), "query {q:?} errored: {:?}", r.err());
    }
}

// --- recall is temporal -------------------------------------------------------

#[test]
fn recall_answers_with_what_is_true_now_not_with_everything_ever_said() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "10", "2026-07-01");
    f.say("produto_a", "preco", "20", "2026-07-28");
    for i in 0..5 {
        f.say(&format!("ruido_{i}"), "campo", "valor", "2026-07-01");
    }

    let hits = f.ask("preco do produto_a");
    assert_eq!(
        hits.len(),
        1,
        "closed facts leaked into a plain recall: {hits:?}"
    );
    assert!(hits[0].contains("20"), "got {:?}", hits[0]);
}

#[test]
fn as_of_recall_answers_with_what_was_true_then() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "10", "2026-07-01");
    f.say("produto_a", "preco", "20", "2026-07-28");

    let hits = f.hits(&RecallQuery::new("preco do produto_a").as_of(ts("2026-07-10")));
    assert_eq!(hits.len(), 1);
    assert!(hits[0].contains("10"), "got {:?}", hits[0]);
}

#[test]
fn history_mode_returns_the_whole_trajectory() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "10", "2026-07-01");
    f.say("produto_a", "preco", "20", "2026-07-28");

    let hits = f.hits(&RecallQuery::new("preco do produto_a").history());
    assert_eq!(hits.len(), 2, "got {hits:?}");
}

#[test]
fn a_retracted_fact_never_surfaces_even_in_history_mode() {
    // History is about what was true, not about every row. A retraction says the
    // claim was never true, so replaying it as an answer would be a lie.
    let f = Fixture::new();
    f.say("produto_a", "preco", "10", "2026-07-01");
    let id = f.brain.current("produto_a", "preco").unwrap().unwrap().id;
    f.brain.retract(id, Some("wrong")).unwrap();

    assert!(f.ask("preco do produto_a").is_empty());
    assert!(
        f.hits(&RecallQuery::new("preco do produto_a").history())
            .is_empty(),
        "a retracted fact surfaced in history mode"
    );
}

// --- ranking ------------------------------------------------------------------

#[test]
fn results_are_ordered_deterministically_across_repeated_runs() {
    // Ties broken by fact id. Without this, two facts with identical scores come
    // back in whatever order SQLite happened to produce, and every snapshot test
    // downstream becomes intermittent.
    let f = Fixture::new();
    for i in 0..10 {
        f.say(&format!("item_{i}"), "estado", "ativo", "2026-07-01");
    }

    let first = f.ask("estado ativo");
    assert!(first.len() > 1, "need several hits to test tie-breaking");
    for _ in 0..5 {
        assert_eq!(f.ask("estado ativo"), first, "ordering was not stable");
    }
}

#[test]
fn the_limit_is_respected() {
    let f = Fixture::new();
    for i in 0..20 {
        f.say(&format!("item_{i}"), "estado", "ativo", "2026-07-01");
    }
    let hits = f.hits(&RecallQuery::new("estado ativo").limit(3));
    assert_eq!(hits.len(), 3);
}

#[test]
fn each_hit_reports_which_channels_found_it() {
    // The eval harness measures per-channel contribution, so a hit has to carry
    // its provenance. It is also what makes a surprising ranking debuggable.
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    for i in 0..5 {
        f.say(&format!("ruido_{i}"), "campo", "valor", "2026-07-01");
    }

    let hits = f
        .brain
        .recall(&RecallQuery::new("preco do produto_a"))
        .unwrap();
    let top = &hits[0];
    assert!(!top.channels.is_empty());
    assert!(top.score > 0.0);
}

#[test]
fn channels_can_be_disabled_for_ablation() {
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    for i in 0..8 {
        f.say(&format!("outro_{i}"), "preco", "5", "2026-07-01");
    }

    let alias_only = f
        .brain
        .recall(&RecallQuery::new("Produto A").channels(&[Channel::Alias]))
        .unwrap();
    assert!(
        alias_only
            .iter()
            .all(|h| h.channels == vec![Channel::Alias]),
        "a disabled channel still contributed"
    );
    assert_eq!(
        alias_only.len(),
        1,
        "alias match should pin exactly one entity"
    );

    let bm25_only = f
        .brain
        .recall(&RecallQuery::new("preco").channels(&[Channel::Bm25]))
        .unwrap();
    assert!(bm25_only.len() > 1, "bm25 should match every priced item");
}

#[test]
fn a_fact_found_by_both_channels_outranks_one_found_by_a_single_channel() {
    // This is the entire point of fusing: agreement between independent signals
    // is evidence, and reciprocal rank fusion is how that evidence compounds.
    let f = Fixture::new();
    f.say("produto_a", "preco", "20", "2026-07-01");
    for i in 0..8 {
        f.say(&format!("outro_{i}"), "preco", "5", "2026-07-01");
    }

    let hits = f
        .brain
        .recall(&RecallQuery::new("preco do produto_a"))
        .unwrap();
    assert!(
        hits[0].channels.len() > 1,
        "top hit came from one channel only"
    );
    assert!(hits[0].fact.entity_key == "produto_a");
}

//! Passo 3: the bitemporal core.
//!
//! The whole point of this project lives here. A vector store overwrites, so it
//! loses history; a naive append-only log keeps history but cannot say what is
//! true *now*. Facts here carry two time axes:
//!
//! - **valid time** (`valid_from`/`valid_to`) -- when the fact was true in the world
//! - **transaction time** (`recorded_at`) -- when the brain learned it
//!
//! Writing a new value **closes** the previous one instead of deleting it.

use brain::brain::{Assertion, Brain, Object, Outcome};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use jiff::Timestamp;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    // Bare dates are a convenience: `2026-07-01` means midnight UTC.
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
        let path = tmp.path().join("t.db");
        let brain = Brain::init(
            &path,
            "teste",
            // 1s per read: successive writes get distinct, ordered recorded_at
            // without sleeping or depending on wall-clock resolution.
            //
            // Set *after* every instant these tests write at, and that is
            // load-bearing rather than cosmetic: "now" means the instant the
            // question is asked, so a clock sitting before the data would make
            // every fact in this file a future one and `current` would answer
            // nothing. The single deliberate exception is the 2027 price in
            // `a_future_dated_fact_does_not_become_current_until_its_time`.
            Box::new(StepClock::new(ts("2026-08-01T00:00:00Z"), 1000)),
            Box::new(SeededIdGen::new(1)),
        )
        .unwrap();
        Self { _tmp: tmp, brain }
    }

    /// `produto_a preco = <n>` valid from `at`.
    fn price(&self, n: f64, at: &str) -> Outcome {
        self.brain
            .remember(&Assertion::new("produto_a", "preco", Object::num(n)).at(ts(at)))
            .unwrap()
    }

    fn current_price(&self) -> Option<f64> {
        self.brain
            .current("produto_a", "preco")
            .unwrap()
            .and_then(|f| f.number())
    }

    fn price_at(&self, when: &str) -> Option<f64> {
        self.brain
            .as_of("produto_a", "preco", ts(when))
            .unwrap()
            .and_then(|f| f.number())
    }

    fn price_history(&self) -> Vec<(f64, String, Option<String>)> {
        self.brain
            .history("produto_a", "preco")
            .unwrap()
            .into_iter()
            .map(|f| {
                (
                    f.number().unwrap(),
                    f.valid_from.to_string(),
                    f.valid_to.map(|t| t.to_string()),
                )
            })
            .collect()
    }
}

// --- the case that motivated the whole project -------------------------------

#[test]
fn a_new_value_supersedes_the_old_one_without_destroying_it() {
    let f = Fixture::new();
    f.price(10.0, "2026-07-01");
    f.price(20.0, "2026-07-28");

    // Asking plainly gets today's answer, and only that.
    assert_eq!(f.current_price(), Some(20.0));

    // Asking for the history gets the whole trajectory, with closed intervals.
    assert_eq!(
        f.price_history(),
        vec![
            (
                10.0,
                ts("2026-07-01").to_string(),
                Some(ts("2026-07-28").to_string())
            ),
            (20.0, ts("2026-07-28").to_string(), None),
        ]
    );

    // And asking about a past moment gets the answer that was true then.
    assert_eq!(f.price_at("2026-07-10"), Some(10.0));
    assert_eq!(f.price_at("2026-06-01"), None, "before the first fact");
}

#[test]
fn supersession_links_the_old_fact_to_the_new_one() {
    let f = Fixture::new();
    let first = match f.price(10.0, "2026-07-01") {
        Outcome::Created(fact) => fact,
        other => panic!("expected Created, got {other:?}"),
    };
    let (closed, created) = match f.price(20.0, "2026-07-28") {
        Outcome::Superseded { closed, created } => (closed, created),
        other => panic!("expected Superseded, got {other:?}"),
    };

    assert_eq!(closed.id, first.id);
    assert_eq!(closed.superseded_by, Some(created.id));
    assert_eq!(closed.valid_to, Some(created.valid_from));
    assert_eq!(created.superseded_by, None);
    assert_eq!(created.valid_to, None);

    // The closed fact keeps its original value and its original recorded_at:
    // closing edits the interval, never the content.
    assert_eq!(closed.number(), Some(10.0));
    assert_eq!(closed.recorded_at, first.recorded_at);
}

#[test]
fn three_changes_produce_a_contiguous_chain_with_no_gaps_or_overlaps() {
    let f = Fixture::new();
    f.price(10.0, "2026-01-10");
    f.price(20.0, "2026-02-10");
    f.price(30.0, "2026-03-10");

    let h = f.price_history();
    assert_eq!(h.len(), 3);
    for w in h.windows(2) {
        assert_eq!(
            w[0].2.as_ref(),
            Some(&w[1].1),
            "interval {:?} does not meet {:?}",
            w[0],
            w[1]
        );
    }
    assert_eq!(f.current_price(), Some(30.0));
    assert_eq!(f.price_at("2026-02-15"), Some(20.0));
}

// --- reasserting is reinforcement, not clutter -------------------------------

#[test]
fn reasserting_the_same_value_strengthens_it_instead_of_adding_a_fact() {
    let f = Fixture::new();
    f.price(10.0, "2026-07-01");

    let again = f.price(10.0, "2026-07-15");
    let fact = match again {
        Outcome::Reasserted(fact) => fact,
        other => panic!("expected Reasserted, got {other:?}"),
    };

    assert_eq!(fact.reassert_count, 1);
    assert_eq!(
        fact.valid_from,
        ts("2026-07-01"),
        "reasserting must not move the start of validity"
    );
    assert_eq!(f.price_history().len(), 1, "history gained a duplicate");
}

#[test]
fn a_value_that_returns_after_a_change_is_a_new_fact_not_a_reassertion() {
    // 10 -> 20 -> 10. The final 10 is genuinely a new period of validity, so the
    // history must show three intervals, not two.
    let f = Fixture::new();
    f.price(10.0, "2026-01-10");
    f.price(20.0, "2026-02-10");
    f.price(10.0, "2026-03-10");

    let h = f.price_history();
    assert_eq!(h.len(), 3, "got {h:?}");
    assert_eq!(f.current_price(), Some(10.0));
    assert_eq!(f.price_at("2026-02-15"), Some(20.0));
}

// --- writing about the past --------------------------------------------------

#[test]
fn a_backdated_fact_slots_into_the_timeline_instead_of_closing_the_open_one() {
    // Learning late about an old price must not make it look like the current one
    // ended. This is the edge case that quietly corrupts naive implementations.
    let f = Fixture::new();
    f.price(20.0, "2026-07-28");
    f.price(10.0, "2026-07-01"); // learned afterwards, but true earlier

    assert_eq!(f.current_price(), Some(20.0), "the open fact was disturbed");
    assert_eq!(
        f.price_history(),
        vec![
            (
                10.0,
                ts("2026-07-01").to_string(),
                Some(ts("2026-07-28").to_string())
            ),
            (20.0, ts("2026-07-28").to_string(), None),
        ]
    );
    assert_eq!(f.price_at("2026-07-10"), Some(10.0));
}

#[test]
fn a_fact_inserted_between_two_others_closes_against_its_successor() {
    let f = Fixture::new();
    f.price(10.0, "2026-01-10");
    f.price(30.0, "2026-03-10");
    f.price(20.0, "2026-02-10"); // squeezed into the middle

    assert_eq!(
        f.price_history(),
        vec![
            (
                10.0,
                ts("2026-01-10").to_string(),
                Some(ts("2026-02-10").to_string())
            ),
            (
                20.0,
                ts("2026-02-10").to_string(),
                Some(ts("2026-03-10").to_string())
            ),
            (30.0, ts("2026-03-10").to_string(), None),
        ]
    );
    assert_eq!(f.price_at("2026-02-15"), Some(20.0));
}

#[test]
fn a_future_dated_fact_does_not_become_current_until_its_time() {
    let f = Fixture::new();
    f.price(10.0, "2026-07-01");
    f.price(99.0, "2027-01-01"); // a price rise announced in advance

    assert_eq!(f.price_at("2026-08-01"), Some(10.0));
    assert_eq!(f.price_at("2027-06-01"), Some(99.0));

    // The assertion this test was named for and did not make. `current` used to
    // mean "the latest fact nobody has closed", which is the 2027 one -- so the
    // brain answered "what does it cost" with a price that does not apply yet,
    // while `as_of(today)` answered correctly. Now they are the same question.
    assert_eq!(
        f.current_price(),
        Some(10.0),
        "an announced price is not today's price"
    );
}

#[test]
fn asserting_the_same_valid_from_twice_corrects_instead_of_creating_a_zero_width_interval() {
    // Two values claiming the same instant is a correction, not a change over
    // time. Closing one against the other would leave valid_from == valid_to.
    let f = Fixture::new();
    f.price(10.0, "2026-07-01");
    let outcome = f.price(20.0, "2026-07-01");

    assert!(
        matches!(outcome, Outcome::Corrected { .. }),
        "expected Corrected, got {outcome:?}"
    );
    assert_eq!(f.current_price(), Some(20.0));

    // The mistaken claim stays in the record, marked as never having been true.
    // Hiding it would make the audit trail lie about what the brain believed.
    let h = f.brain.history("produto_a", "preco").unwrap();
    assert_eq!(h.len(), 2);
    let old = h.iter().find(|x| x.number() == Some(10.0)).unwrap();
    assert!(
        old.retracted_at.is_some(),
        "the corrected fact was not retracted"
    );
    assert!(!old.is_open());

    // Exactly one answer at that instant, and no empty interval anywhere.
    assert_eq!(f.price_at("2026-07-01"), Some(20.0));
    assert!(
        h.iter()
            .all(|x| x.valid_to.is_none_or(|to| x.valid_from < to))
    );
}

// --- retraction is not supersession ------------------------------------------

#[test]
fn retracting_a_fact_removes_it_from_answers_without_reopening_its_predecessor() {
    // "This was never true" is a different claim from "this stopped being true".
    let f = Fixture::new();
    f.price(10.0, "2026-01-10");
    let bad = match f.price(20.0, "2026-02-10") {
        Outcome::Superseded { created, .. } => created,
        other => panic!("expected Superseded, got {other:?}"),
    };

    f.brain.retract(bad.id, Some("typo")).unwrap();

    assert_eq!(
        f.current_price(),
        None,
        "retracting must not silently resurrect the 10"
    );
    let h = f.brain.history("produto_a", "preco").unwrap();
    assert_eq!(h.len(), 2, "the retracted fact stays in the record");
    assert!(h.iter().any(|x| x.retracted_at.is_some()));
}

#[test]
fn a_retracted_fact_is_invisible_to_as_of_queries() {
    let f = Fixture::new();
    let only = match f.price(10.0, "2026-01-10") {
        Outcome::Created(fact) => fact,
        other => panic!("expected Created, got {other:?}"),
    };
    f.brain.retract(only.id, None).unwrap();

    assert_eq!(f.price_at("2026-06-01"), None);
    assert_eq!(f.current_price(), None);
}

// --- multi-valued predicates --------------------------------------------------

#[test]
fn a_multi_valued_predicate_lets_values_coexist() {
    let f = Fixture::new();
    f.brain
        .set_cardinality("tag", brain::brain::Cardinality::Multi)
        .unwrap();

    for tag in ["promocao", "importado", "fragil"] {
        f.brain
            .remember(&Assertion::new("produto_a", "tag", Object::text(tag)).at(ts("2026-01-10")))
            .unwrap();
    }

    let open = f.brain.current_all("produto_a", "tag").unwrap();
    assert_eq!(open.len(), 3, "multi-valued facts superseded each other");
}

#[test]
fn a_relation_is_a_fact_and_can_therefore_end() {
    // Relations are stored as facts whose object is an entity, so the graph gets
    // bitemporality for free: a dependency that ended is visible as such.
    let f = Fixture::new();
    f.brain
        .link("produto_a", "fornecido_por", "acme", Some(ts("2026-01-10")))
        .unwrap();
    f.brain
        .link(
            "produto_a",
            "fornecido_por",
            "globex",
            Some(ts("2026-06-10")),
        )
        .unwrap();

    let current = f
        .brain
        .current("produto_a", "fornecido_por")
        .unwrap()
        .unwrap();
    assert_eq!(current.object_entity_label().as_deref(), Some("globex"));

    let past = f
        .brain
        .as_of("produto_a", "fornecido_por", ts("2026-03-01"))
        .unwrap()
        .unwrap();
    assert_eq!(past.object_entity_label().as_deref(), Some("acme"));
}

// --- identity and normalization ----------------------------------------------

#[test]
fn entities_are_matched_after_normalization() {
    let f = Fixture::new();
    f.brain
        .remember(&Assertion::new("Produto A", "preco", Object::num(10.0)).at(ts("2026-01-10")))
        .unwrap();
    f.brain
        .remember(&Assertion::new("produto-a", "preco", Object::num(20.0)).at(ts("2026-02-10")))
        .unwrap();

    // "Produto A", "produto-a" and "produto_a" are one entity.
    assert_eq!(f.current_price(), Some(20.0));
    assert_eq!(f.price_history().len(), 2);
}

#[test]
fn accents_survive_but_composition_form_does_not_split_an_entity() {
    // "preço" typed directly (NFC) and pasted from a macOS filename (NFD) must be
    // the same predicate, or a brain silently grows two parallel histories.
    let f = Fixture::new();
    let nfc = "pre\u{e7}o"; // ç
    let nfd = "prec\u{327}o"; // c + combining cedilla

    f.brain
        .remember(&Assertion::new("produto_a", nfc, Object::num(10.0)).at(ts("2026-01-10")))
        .unwrap();
    f.brain
        .remember(&Assertion::new("produto_a", nfd, Object::num(20.0)).at(ts("2026-02-10")))
        .unwrap();

    let h = f.brain.history("produto_a", nfc).unwrap();
    assert_eq!(
        h.len(),
        2,
        "the two spellings split into separate histories"
    );
}

// --- provenance ---------------------------------------------------------------

#[test]
fn why_reports_where_a_fact_came_from_and_what_replaced_it() {
    let f = Fixture::new();
    f.brain
        .remember(
            &Assertion::new("produto_a", "preco", Object::num(10.0))
                .at(ts("2026-01-10"))
                .source("cotacao.pdf")
                .locator(serde_json::json!({"file": "cotacao.pdf", "page": 3})),
        )
        .unwrap();
    f.price(20.0, "2026-02-10");

    let first = &f.brain.history("produto_a", "preco").unwrap()[0];
    let why = f.brain.why(first.id).unwrap();

    assert_eq!(why.fact.source.as_deref(), Some("cotacao.pdf"));
    assert_eq!(why.fact.locator.as_ref().unwrap()["page"], 3);
    assert_eq!(why.superseded_by.as_ref().unwrap().number(), Some(20.0));
}

#[test]
fn a_fact_can_be_a_pointer_instead_of_a_value() {
    // The graph is an index, not a warehouse: a fact may locate the answer rather
    // than contain it, so the corpus is never duplicated and never goes stale.
    let f = Fixture::new();
    f.brain
        .remember(
            &Assertion::new(
                "regra_de_preco",
                "definida_em",
                Object::text("src/pricing.rs"),
            )
            .at(ts("2026-01-10"))
            .locator(serde_json::json!({"file": "src/pricing.rs", "lines": "40-52"})),
        )
        .unwrap();

    let fact = f
        .brain
        .current("regra_de_preco", "definida_em")
        .unwrap()
        .unwrap();
    assert_eq!(fact.locator.as_ref().unwrap()["lines"], "40-52");
}

// --- transaction time ---------------------------------------------------------

#[test]
fn recorded_at_reflects_learning_order_independently_of_validity_order() {
    // The two axes really are independent: this fact is valid earlier but was
    // recorded later.
    let f = Fixture::new();
    f.price(20.0, "2026-07-28");
    f.price(10.0, "2026-07-01");

    let h = f.brain.history("produto_a", "preco").unwrap();
    let early = h.iter().find(|x| x.number() == Some(10.0)).unwrap();
    let late = h.iter().find(|x| x.number() == Some(20.0)).unwrap();

    assert!(early.valid_from < late.valid_from);
    assert!(
        early.recorded_at > late.recorded_at,
        "recorded_at should follow write order, not validity order"
    );
}

#[test]
fn writes_are_atomic_so_a_rejected_assertion_leaves_nothing_behind() {
    let f = Fixture::new();
    f.price(10.0, "2026-01-10");

    // A confidence outside [0,1] is nonsense and must be refused outright.
    let bad = f.brain.remember(
        &Assertion::new("produto_a", "preco", Object::num(20.0))
            .at(ts("2026-02-10"))
            .confidence(7.5),
    );
    assert!(bad.is_err(), "an invalid confidence was accepted");

    assert_eq!(f.current_price(), Some(10.0));
    assert_eq!(f.price_history().len(), 1, "a rejected write left a trace");
}

//! Passo 17: searching the content, and asking what the brain records.
//!
//! Every read before this one needs a name. `get` and `history` resolve a
//! subject, `which` starts from a predicate key, and `recall` answers a question
//! by ranking. None of them answers *"which records mention this string"* -- an
//! identifier, a file path, a phrase somebody used -- and that is the question
//! anyone gathering evidence actually asks.
//!
//! What it cost to be missing is on record. On a 566-fact brain, `recall
//! "_normalize_email"` put the one record containing the identifier at rank 12,
//! past the default limit and therefore invisible, so the agent opened the SQLite
//! file and grepped it by hand. `find` is the tool it wanted; `predicates` is the
//! other half, because `which` cannot be used by anyone who does not already know
//! a predicate key, and until now there was no way to learn one from inside the
//! tool.
//!
//! Two claims under test. `find` is **literal and complete**: it takes the needle
//! as a string rather than as a bag of words, it reaches inside a token where the
//! index cannot, and it reports how many matched so a cut answer is visible.
//! `predicates` is the brain's **learned ontology**, counted against what holds
//! now.

use brain::brain::{Assertion, Brain, Cardinality, FindQuery, Object, Via};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::When;
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

/// The clock starts after the instants these tests write at, so "now" really is
/// after the facts rather than before them.
fn brain(tmp: &TempDir) -> Brain {
    Brain::init(
        &tmp.path().join("t.db"),
        "find",
        Box::new(StepClock::new(ts("2026-07-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

fn say(b: &Brain, subject: &str, predicate: &str, value: &str, at: &str) {
    b.remember(&Assertion::new(subject, predicate, Object::text(value)).at(ts(at)))
        .unwrap();
}

/// The statements a search returned, which is what the answer is read for.
fn found(b: &Brain, needle: &str) -> Vec<String> {
    b.find(&FindQuery::new(needle))
        .unwrap()
        .facts
        .into_iter()
        .map(|h| h.fact.statement)
        .collect()
}

/// A brain holding the shapes a literal search exists for: an identifier buried
/// in prose, a coupon code with a fragment in the middle of it, and an accent.
fn corpus(tmp: &TempDir) -> Brain {
    let b = brain(tmp);
    say(
        &b,
        "trial",
        "regra_de_migracao",
        "QUALQUER mudanca em _normalize_email TEM de vir acompanhada de scripts/migrate_trial_locks.py",
        "2026-02-01",
    );
    say(
        &b,
        "trial",
        "locks",
        "locks no Redis SEM TTL, um por mailbox",
        "2026-02-01",
    );
    say(
        &b,
        "cupom_x",
        "codigo",
        "FEEDBACK25-PRIDAYFARELYA",
        "2026-02-01",
    );
    say(&b, "produto_a", "preco", "preço de tabela", "2026-02-01");
    b
}

// --- literal, in the ways an index is not --------------------------------------

#[test]
fn a_fragment_from_the_middle_of_a_token_is_found() {
    // The case the FTS5 index structurally cannot serve: it indexes tokens, and
    // nothing in it can see `PRIDAY` inside one. This is the entire reason the
    // scan exists beside the index.
    let tmp = TempDir::new().unwrap();
    let b = corpus(&tmp);

    assert_eq!(
        found(&b, "PRIDAY"),
        ["cupom_x codigo FEEDBACK25-PRIDAYFARELYA"]
    );
}

#[test]
fn a_token_prefix_is_found_by_the_index() {
    let tmp = TempDir::new().unwrap();
    let b = corpus(&tmp);

    assert_eq!(
        found(&b, "FEEDBACK"),
        ["cupom_x codigo FEEDBACK25-PRIDAYFARELYA"]
    );
}

#[test]
fn each_stage_says_it_found_the_record() {
    // Reported for the same reason a `recall` hit reports its channels: a result
    // nobody can explain is a result nobody can trust. `FEEDBACK` is a token
    // prefix *and* a substring, so both stages must claim it, once.
    let tmp = TempDir::new().unwrap();
    let b = corpus(&tmp);

    let both = b.find(&FindQuery::new("FEEDBACK")).unwrap();
    assert_eq!(both.facts.len(), 1, "one record, found twice, listed once");
    assert_eq!(both.facts[0].via, [Via::Term, Via::Fragment]);

    // Inside a token, so only the scan can have found it.
    let scan = b.find(&FindQuery::new("PRIDAY")).unwrap();
    assert_eq!(scan.facts[0].via, [Via::Fragment]);
}

#[test]
fn accents_and_case_are_ignored_in_both_directions() {
    // The fold has to agree with the index, which is tokenized
    // `remove_diacritics 2`. If the scan were stricter than the index, one
    // command would answer the same question two ways depending on which stage
    // reached the row first.
    let tmp = TempDir::new().unwrap();
    let b = corpus(&tmp);

    for needle in ["preco", "PREÇO", "preço", "Preco"] {
        assert_eq!(
            found(&b, needle),
            ["produto_a preco preço de tabela"],
            "{needle:?} did not reach the accented record"
        );
    }
}

#[test]
fn an_underscore_is_a_character_and_not_a_wildcard() {
    // `_` is `LIKE`'s single-character wildcard, and identifiers are made of
    // underscores. Unescaped, the one search whose job is to be literal would
    // match every near-miss.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "a", "nota", "chama _normalize_email", "2026-02-01");
    say(&b, "b", "nota", "chama xnormalizexemail", "2026-02-01");

    assert_eq!(
        found(&b, "_normalize_email"),
        ["a nota chama _normalize_email"]
    );
}

#[test]
fn a_percent_is_a_character_and_not_a_wildcard() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "desconto", "regra", "10% no primeiro mes", "2026-02-01");
    say(&b, "outro", "regra", "sem desconto nenhum", "2026-02-01");

    assert_eq!(found(&b, "10%"), ["desconto regra 10% no primeiro mes"]);
}

#[test]
fn a_phrase_stays_a_phrase() {
    // The difference between a literal search and a question. Returning every
    // record that mentions either word is a different, much less useful answer.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(
        &b,
        "sessao",
        "gatilho",
        "um context reset perde tudo",
        "2026-02-01",
    );
    say(
        &b,
        "outra",
        "gatilho",
        "reset manual do context",
        "2026-02-01",
    );

    assert_eq!(
        found(&b, "context reset"),
        ["sessao gatilho um context reset perde tudo"]
    );
}

#[test]
fn an_empty_needle_matches_nothing_rather_than_everything() {
    // `LIKE '%%'` is true for every row, so the harmless-looking degenerate case
    // is the one that hands back the whole brain.
    let tmp = TempDir::new().unwrap();
    let b = corpus(&tmp);

    for needle in ["", "   "] {
        let all = b.find(&FindQuery::new(needle)).unwrap();
        assert!(all.facts.is_empty(), "{needle:?} returned the brain");
        assert_eq!(all.matched, 0);
    }
}

#[test]
fn a_needle_the_index_cannot_tokenize_is_answered_and_not_an_error() {
    // `___` and `!!` have no tokens in them, so the term stage drops out and the
    // scan is the only branch left. It has to still be a query: an empty FTS5
    // MATCH expression is a syntax error, and a branch with no column names is a
    // SQL error the caller can do nothing about.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "sep", "estilo", "usa ___ como divisor", "2026-02-01");

    assert_eq!(found(&b, "___"), ["sep estilo usa ___ como divisor"]);
    assert!(b.find(&FindQuery::new("!!")).unwrap().facts.is_empty());
}

#[test]
fn the_subject_and_the_predicate_are_searchable_too() {
    // `search_text` carries the entity's label and the predicate, so a needle
    // that names the thing rather than quoting its value still lands.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "payments_db", "regiao", "eu-west-1", "2026-02-01");

    assert_eq!(found(&b, "payments_db"), ["payments_db regiao eu-west-1"]);
    assert_eq!(found(&b, "regiao"), ["payments_db regiao eu-west-1"]);
}

// --- completeness --------------------------------------------------------------

#[test]
fn every_match_is_returned_and_not_the_best_ten() {
    // The anti-`recall` property, and the reason this is not a ranking with a
    // different name. A caller gathering evidence acts on the set as a set.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    for i in 0..60 {
        say(
            &b,
            &format!("nota_{i:02}"),
            "texto",
            "cita _normalize_email",
            "2026-02-01",
        );
    }

    let all = b.find(&FindQuery::new("_normalize_email")).unwrap();
    assert_eq!(all.matched, 60);
    assert_eq!(all.facts.len(), 60, "the answer was truncated");
    assert!(!all.truncated);
}

#[test]
fn a_cut_answer_says_how_much_it_cut() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    for i in 0..60 {
        say(
            &b,
            &format!("nota_{i:02}"),
            "texto",
            "cita _normalize_email",
            "2026-02-01",
        );
    }

    let page = b
        .find(&FindQuery::new("_normalize_email").limit(5))
        .unwrap();
    assert_eq!(page.facts.len(), 5);
    assert_eq!(page.matched, 60, "the total was reported as the page size");
    assert!(page.truncated);
}

#[test]
fn a_needle_nothing_mentions_is_an_empty_answer_and_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let b = corpus(&tmp);

    let none = b.find(&FindQuery::new("kubernetes")).unwrap();
    assert!(none.facts.is_empty());
    assert_eq!(none.matched, 0);
    assert!(!none.truncated);
}

#[test]
fn records_arrive_grouped_by_subject_and_in_the_same_order_every_run() {
    // A page of evidence is read subject by subject. The instant and the id break
    // the ties, so two runs of the same search never disagree.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "zeta", "nota", "cita o token", "2026-02-01");
    say(&b, "alfa", "nota", "cita o token", "2026-03-01");
    say(&b, "alfa", "outra", "cita o token", "2026-01-01");

    assert_eq!(
        found(&b, "cita o token"),
        [
            "alfa outra cita o token",
            "alfa nota cita o token",
            "zeta nota cita o token",
        ]
    );
}

// --- the timeline the search inherits ------------------------------------------

#[test]
fn a_superseded_record_is_not_what_holds_now() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(
        &b,
        "deploy",
        "estrategia",
        "canary com _normalize_email",
        "2026-02-01",
    );
    say(
        &b,
        "deploy",
        "estrategia",
        "blue green sem nada disso",
        "2026-03-01",
    );

    assert!(
        found(&b, "_normalize_email").is_empty(),
        "a closed record answered as current"
    );
    assert_eq!(
        b.find(&FindQuery::new("_normalize_email").when(When::AsOf(ts("2026-02-15"))))
            .unwrap()
            .matched,
        1,
        "and February cannot see what February said"
    );
    assert_eq!(
        b.find(&FindQuery::new("_normalize_email").when(When::History))
            .unwrap()
            .matched,
        1
    );
}

#[test]
fn a_retracted_record_is_found_by_nothing() {
    // It was never true. Surfacing it as evidence would be the same lie `recall`
    // and `which` both refuse to tell.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(
        &b,
        "fantasma",
        "nota",
        "cita _normalize_email",
        "2026-02-01",
    );
    say(&b, "real", "nota", "cita _normalize_email", "2026-02-01");
    let ghost = b.current("fantasma", "nota").unwrap().unwrap();
    b.retract(ghost.id, Some("escrito no projeto errado"))
        .unwrap();

    assert_eq!(
        found(&b, "_normalize_email"),
        ["real nota cita _normalize_email"]
    );
    assert_eq!(
        b.find(&FindQuery::new("_normalize_email").when(When::History))
            .unwrap()
            .matched,
        1,
        "a retraction is hidden even from history"
    );
}

#[test]
fn a_scope_narrows_the_search_and_not_scope_keeps_one_out() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    b.remember(
        &Assertion::new("auth", "decisao", Object::text("sessoes no servidor"))
            .at(ts("2026-02-01")),
    )
    .unwrap();
    b.remember(
        &Assertion::new("fix_login", "nota", Object::text("mexer nas sessoes"))
            .at(ts("2026-02-01"))
            .scope("todo"),
    )
    .unwrap();

    assert_eq!(b.find(&FindQuery::new("sessoes")).unwrap().matched, 2);
    assert_eq!(
        b.find(&FindQuery::new("sessoes").scope("todo"))
            .unwrap()
            .matched,
        1
    );
    assert_eq!(
        b.find(&FindQuery::new("sessoes").not_scope("todo"))
            .unwrap()
            .facts
            .into_iter()
            .map(|h| h.fact.statement)
            .collect::<Vec<_>>(),
        ["auth decisao sessoes no servidor"]
    );
}

// --- what this brain records ---------------------------------------------------

#[test]
fn predicates_are_listed_heaviest_first_and_ties_break_by_key() {
    // The first question anyone has of an unfamiliar brain is what it is mostly
    // made of, and an alphabetical list answers that only by accident.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    for i in 0..3 {
        say(&b, &format!("s_{i}"), "status", "open", "2026-02-01");
    }
    say(&b, "s_0", "zelo", "alto", "2026-02-01");
    say(&b, "s_1", "alfa", "x", "2026-02-01");

    let listed = b.predicates().unwrap();
    assert_eq!(
        listed.iter().map(|p| p.key.as_str()).collect::<Vec<_>>(),
        ["status", "alfa", "zelo"]
    );
    assert_eq!(listed[0].facts, 3);
    assert_eq!(listed[0].subjects, 3);
}

#[test]
fn a_predicate_counts_what_holds_now_and_still_appears_when_nothing_does() {
    // Counted against the present for the same reason `which` defaults to it: a
    // predicate whose every fact closed is part of the history, not part of what
    // this brain is currently about. It stays on the list at zero, because the
    // brain does record it -- silently dropping it would make the ontology
    // flicker with the clock.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    say(&b, "task_a", "status", "open", "2026-02-01");
    say(&b, "task_a", "status", "done", "2026-03-01");
    say(&b, "task_b", "prazo", "2026-08-15", "2026-02-01");

    let listed = b.predicates().unwrap();
    let status = listed.iter().find(|p| p.key == "status").unwrap();
    assert_eq!(status.facts, 1, "the superseded interval was counted");
    assert_eq!(status.subjects, 1);

    let ghost = b.current("task_b", "prazo").unwrap().unwrap();
    b.retract(ghost.id, None).unwrap();
    let listed = b.predicates().unwrap();
    let prazo = listed
        .iter()
        .find(|p| p.key == "prazo")
        .expect("still recorded");
    assert_eq!(prazo.facts, 0, "a retracted fact was counted");
}

#[test]
fn a_predicate_reports_its_shape() {
    // The two properties that decide how a predicate behaves: whether a second
    // value supersedes the first, and whether its object names something the
    // graph can walk to. The `relational` flag is what `brain lint` reports on,
    // and a caller has no other way to see it.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);
    b.link(
        "voucher_x",
        "is_a",
        "voucher_sazonal",
        Some(ts("2026-02-01")),
    )
    .unwrap();
    say(&b, "voucher_x", "tags", "verao", "2026-02-01");
    b.set_cardinality("tags", Cardinality::Multi).unwrap();

    let listed = b.predicates().unwrap();
    let is_a = listed.iter().find(|p| p.key == "is_a").unwrap();
    assert!(is_a.relational);
    assert_eq!(is_a.cardinality, Cardinality::Single);

    let tags = listed.iter().find(|p| p.key == "tags").unwrap();
    assert!(!tags.relational);
    assert_eq!(tags.cardinality, Cardinality::Multi);
}

#[test]
fn a_brain_that_records_nothing_says_so_rather_than_failing() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    assert!(b.predicates().unwrap().is_empty());
}

//! Passo 12: predicates that know their object names a thing, and repairing the
//! ones that did not.
//!
//! Two mechanisms with a deliberate gap between them. Inference and declaration
//! change how *later* facts are stored; `repair` is the only thing that touches
//! facts already written, and it is a separate command because rewriting a stored
//! row is a different kind of act from deciding how to store the next one.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::{lint, repair};
use jiff::Timestamp;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    s.parse().unwrap()
}

fn brain_at(path: &std::path::Path) -> Brain {
    Brain::init(
        path,
        "relational",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

fn brain(tmp: &TempDir) -> Brain {
    brain_at(&tmp.path().join("t.db"))
}

fn text(b: &Brain, subject: &str, predicate: &str, value: &str) {
    b.remember(&Assertion::new(subject, predicate, Object::text(value)))
        .unwrap();
}

/// Whether the fact is an edge, read back through the public view.
fn is_edge(b: &Brain, subject: &str, predicate: &str) -> bool {
    b.current(subject, predicate)
        .unwrap()
        .expect("a fact")
        .object_entity
        .is_some()
}

// --- learning what a predicate is ---------------------------------------------

#[test]
fn one_entity_valued_write_settles_what_the_predicate_is() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // Before any evidence, a string is a string. Guessing here would invent an
    // entity out of every value in the brain.
    text(&b, "voucher_a", "is_a", "voucher_sazonal");
    assert!(!is_edge(&b, "voucher_a", "is_a"));

    // One write says what `is_a` is, and it is not a majority rule: the brain
    // this was built for stood at 10 entity-valued against 59 strings, and any
    // threshold would have called it an attribute and kept it broken.
    b.link("v2_pro", "is_a", "plano", None).unwrap();

    text(&b, "voucher_b", "is_a", "voucher_sazonal");
    assert!(
        is_edge(&b, "voucher_b", "is_a"),
        "later strings should be promoted"
    );
}

#[test]
fn an_ordinary_attribute_is_never_promoted() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "auth", "strategy", "server-side sessions");
    text(&b, "cache", "strategy", "write-through");

    assert!(!is_edge(&b, "auth", "strategy"));
}

#[test]
fn a_number_is_never_promoted_under_a_relational_predicate() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // A predicate can be a relation and still be handed something that is
    // plainly a literal. An entity called `50` would be worse than the mistake.
    b.link("voucher_a", "is_a", "voucher_sazonal", None)
        .unwrap();
    b.remember(&Assertion::new("voucher_b", "is_a", Object::num(50.0)))
        .unwrap();

    assert!(!is_edge(&b, "voucher_b", "is_a"));
}

#[test]
fn declaring_it_off_survives_evidence_to_the_contrary() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // `declared = 1` is never revisited. Somebody who has said `codigo` holds a
    // literal has to be able to make that stick, or the override is decoration.
    b.set_relational("codigo", false).unwrap();
    b.link("voucher_a", "codigo", "ABC123", None).unwrap();

    text(&b, "voucher_b", "codigo", "XYZ789");
    assert!(!is_edge(&b, "voucher_b", "codigo"));
}

#[test]
fn declaring_it_on_promotes_without_any_evidence() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    b.set_relational("is_a", true).unwrap();
    text(&b, "voucher_a", "is_a", "voucher_sazonal");

    assert!(is_edge(&b, "voucher_a", "is_a"));
}

// --- repairing what is already stored ------------------------------------------

#[test]
fn repair_is_dry_until_told_otherwise() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "voucher_a", "is_a", "voucher_sazonal");
    text(&b, "voucher_b", "is_a", "voucher_sazonal");
    b.set_relational("is_a", true).unwrap();

    let dry = repair::relations(b.store().conn(), false).unwrap();
    assert_eq!(dry.promotions.len(), 2);
    assert_eq!(dry.entities_created, 1);
    assert!(!dry.applied);
    assert!(
        !is_edge(&b, "voucher_a", "is_a"),
        "a dry run must change nothing"
    );

    let wet = repair::relations(b.store().conn(), true).unwrap();
    assert!(wet.applied);
    assert!(is_edge(&b, "voucher_a", "is_a"));
    assert!(is_edge(&b, "voucher_b", "is_a"));
}

#[test]
fn repair_leaves_the_statement_byte_identical() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // The test of whether a change belongs in `repair` at all. A repair that had
    // to alter what a fact says would be a correction, and corrections go through
    // `retract` where they are visible. It is also what keeps the FTS triggers
    // quiet and the stored embeddings valid.
    text(&b, "voucher_a", "is_a", "voucher_sazonal");
    let before = b.current("voucher_a", "is_a").unwrap().unwrap();

    b.set_relational("is_a", true).unwrap();
    repair::relations(b.store().conn(), true).unwrap();

    let after = b.current("voucher_a", "is_a").unwrap().unwrap();
    assert_eq!(before.id, after.id, "the same row, not a new one");
    assert_eq!(before.statement, after.statement);
    assert_eq!(before.object_text, after.object_text);
    assert_eq!(before.valid_from, after.valid_from);
    assert_eq!(before.recorded_at, after.recorded_at);
    assert!(after.object_entity.is_some(), "and now it is an edge");
}

#[test]
fn repair_never_touches_a_predicate_that_is_not_relational() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "auth", "strategy", "server-side sessions");
    text(&b, "cache", "strategy", "write-through");

    let report = repair::relations(b.store().conn(), true).unwrap();
    assert!(report.promotions.is_empty());
    assert!(!is_edge(&b, "auth", "strategy"));
}

#[test]
fn repair_lands_on_an_entity_that_already_answers_to_the_name() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    // Growing a second entity beside one that already answers to this name is
    // the exact failure the whole project treats as unrecoverable, so the repair
    // resolves through declared aliases just as the write path does.
    b.link("v2_pro", "is_a", "plano_base", None).unwrap();
    b.declare_alias("plano_base", "a escada de planos").unwrap();
    text(&b, "v2_lite", "pertence_a", "a escada de planos");

    let entities = |b: &Brain| -> i64 {
        b.store()
            .conn()
            .query_row("SELECT count(*) FROM entity", [], |r| r.get(0))
            .unwrap()
    };
    let before = entities(&b);

    b.set_relational("pertence_a", true).unwrap();
    let report = repair::relations(b.store().conn(), true).unwrap();

    assert_eq!(report.promotions.len(), 1);
    assert_eq!(report.entities_created, 0, "the alias already named one");
    assert_eq!(entities(&b), before, "no second entity was grown");
    assert_eq!(
        b.current("v2_lite", "pertence_a")
            .unwrap()
            .unwrap()
            .object_entity,
        Some("plano_base".to_string()),
        "it landed on the entity the alias names"
    );
}

#[test]
fn repair_closes_the_lint_finding_it_was_reported_for() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp);

    text(&b, "voucher_a", "is_a", "voucher_sazonal");
    text(&b, "voucher_b", "is_a", "voucher_sazonal");
    text(&b, "voucher_c", "is_a", "voucher_sazonal");

    let before = lint::check(b.store().conn()).unwrap();
    assert_eq!(before.candidate_classes.len(), 1);
    assert_eq!(before.orphans.len(), 3);

    b.set_relational("is_a", true).unwrap();
    repair::relations(b.store().conn(), true).unwrap();

    let after = lint::check(b.store().conn()).unwrap();
    assert!(after.candidate_classes.is_empty(), "{after:?}");
    assert!(after.orphans.is_empty(), "{after:?}");
    assert_eq!(after.edges, 3);
}

// --- the migration --------------------------------------------------------------

#[test]
fn a_brain_written_before_v5_opens_and_gains_the_column() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("old.db");
    {
        let b = brain_at(&path);
        text(&b, "voucher_a", "is_a", "voucher_sazonal");
    }

    // Wind it back to what a v4 binary would have left on disk. `open` accepted
    // any version at or below the current one before this step, which was fine
    // only for as long as no version ever added a column -- it does not survive
    // one, and the failure would land at runtime in whatever command got there
    // first rather than at open.
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute(
            "UPDATE meta SET value = '4' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
        conn.execute("ALTER TABLE predicate DROP COLUMN relational", [])
            .unwrap();
    }

    let b = Brain::open(
        &path,
        Box::new(StepClock::new(ts("2026-02-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(2)),
    )
    .unwrap();

    let version: String = b
        .store()
        .conn()
        .query_row(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    // Against the constant rather than a literal: the ladder is cumulative, so an
    // old brain must arrive at whatever the current version is, and a test that
    // named one version would have to be edited on every bump -- which is exactly
    // when it is most useful for it to be checking something.
    assert_eq!(version, brain::store::SCHEMA_VERSION.to_string());

    // Migrated brains start conservative: every predicate keeps storing objects
    // exactly as it did until somebody says otherwise.
    let relational: i64 = b
        .store()
        .conn()
        .query_row(
            "SELECT relational FROM predicate WHERE key = 'is_a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(relational, 0);

    // And the facts it already held are untouched and still readable.
    assert_eq!(
        b.current("voucher_a", "is_a").unwrap().unwrap().object_text,
        Some("voucher_sazonal".to_string())
    );
}

//! Passo 7: the names a thing goes by.
//!
//! Two mechanisms share one table and are trusted differently, and almost every
//! test here exists to hold that line. A declared alias is a statement about the
//! world and decides where a fact is written. A learned one is a guess made from
//! watching a question, and may only widen retrieval.
//!
//! The guard worth stating: if a learned alias could decide identity, one
//! well-phrased question would silently graft an entity's entire future history
//! onto the wrong node, with nothing in any output to show it happened.

use brain::alias::AliasSource;
use brain::brain::{Assertion, Brain, BrainError, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::{Channel, RecallQuery, When};
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

fn brain(tmp: &TempDir, label: &str) -> Brain {
    Brain::init(
        &tmp.path().join("t.db"),
        label,
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap()
}

fn num(b: &Brain, subject: &str, predicate: &str, v: f64) {
    b.remember(&Assertion::new(subject, predicate, Object::num(v)))
        .unwrap();
}

/// Asks, then hands the answer back for learning -- the sequence `recall --learn`
/// performs, kept in one place so every test exercises the real ordering.
fn ask_and_learn(b: &Brain, q: &str) -> Option<brain::alias::Learned> {
    let hits = b.recall(&RecallQuery::new(q)).unwrap();
    b.learn_alias(q, &hits).unwrap()
}

/// What the alias channel alone finds -- the channel a learned name has to move.
fn by_name_only(b: &Brain, q: &str) -> Vec<String> {
    b.recall(&RecallQuery::new(q).channels(&[Channel::Alias]))
        .unwrap()
        .into_iter()
        .map(|h| h.fact.entity_key)
        .collect()
}

// --- declared -----------------------------------------------------------------

#[test]
fn a_declared_name_decides_where_a_fact_is_written() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "identity");
    num(&b, "acme", "funcionarios", 40.0);
    b.declare_alias("acme", "ACME Corp").unwrap();

    // Writing under the declared name must land on the existing entity, not
    // create a second one. This is the whole point of declaring it.
    num(&b, "ACME Corp", "pais", 55.0);

    let view = b.entity("acme", When::Now, 0).unwrap().expect("acme");
    let predicates: Vec<_> = view.facts.iter().map(|f| f.predicate.clone()).collect();
    assert_eq!(predicates, ["funcionarios", "pais"]);

    // Asking by the alias reaches the same entity and says so: the view reports
    // the key of what was found, not the key that was asked for.
    let by_alias = b
        .entity("ACME Corp", When::Now, 0)
        .unwrap()
        .expect("reachable by its declared name");
    assert_eq!(by_alias.key, "acme");
    assert_eq!(by_alias.facts.len(), 2);
}

#[test]
fn a_name_already_in_use_is_refused() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "collision");
    num(&b, "acme", "funcionarios", 40.0);
    num(&b, "globex", "funcionarios", 12.0);

    // Accepting this would send every later fact about one of the two to the
    // other, and no output would ever show that it happened.
    let err = b.declare_alias("acme", "globex").unwrap_err();
    assert!(
        matches!(&err, BrainError::AliasTaken { alias, .. } if alias == "globex"),
        "expected AliasTaken, got {err:?}"
    );

    // Its own key is equally refused: a thing does not need an alias to itself,
    // and the row would be a permanent no-op nobody could interpret.
    assert!(matches!(
        b.declare_alias("acme", "ACME").unwrap_err(),
        BrainError::AliasTaken { .. }
    ));
}

#[test]
fn naming_something_that_does_not_exist_is_an_error() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "missing");
    assert!(matches!(
        b.declare_alias("fantasma", "apelido").unwrap_err(),
        BrainError::NoSuchEntity { .. }
    ));
}

#[test]
fn a_forgotten_name_stops_resolving() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "forget");
    num(&b, "acme", "funcionarios", 40.0);
    b.declare_alias("acme", "ACME Corp").unwrap();

    assert!(b.forget_alias("acme", "ACME Corp").unwrap());
    assert!(
        !b.forget_alias("acme", "ACME Corp").unwrap(),
        "forgetting twice must report that there was nothing to forget"
    );
    assert!(b.aliases("acme").unwrap().is_empty());

    // With the name gone, the same write now creates its own entity -- which is
    // exactly what makes the alias load-bearing rather than decorative.
    num(&b, "ACME Corp", "pais", 55.0);
    assert!(b.entity("acme_corp", When::Now, 0).unwrap().is_some());
}

// --- learned ------------------------------------------------------------------

#[test]
fn a_question_that_leaves_no_doubt_teaches_a_name() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "learn");
    num(&b, "Produto Brasília", "preco", 20.0);
    num(&b, "servidor", "porta", 8080.0);
    num(&b, "cache", "ttl", 300.0);
    num(&b, "fila", "tamanho", 10.0);

    // The accent is dropped, so the question names nothing: identity is exact,
    // and `produto_brasilia` is not `produto_brasília`. Only BM25 can answer,
    // because search is the forgiving layer.
    assert_eq!(
        by_name_only(&b, "quanto custa o produto brasilia"),
        [] as [String; 0]
    );

    let learned = ask_and_learn(&b, "quanto custa o produto brasilia").expect("a name");
    assert_eq!(learned.alias, "produto_brasilia");
    assert_eq!(learned.entity_key, "produto_brasília");

    // Now the same words name the thing, which is the point: a lexical hit has
    // been converted into an entity the next question can point at directly.
    assert_eq!(
        by_name_only(&b, "quanto custa o produto brasilia"),
        ["produto_brasília"]
    );
}

#[test]
fn a_learned_name_never_decides_where_a_fact_is_written() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "guess");
    num(&b, "Produto Brasília", "preco", 20.0);
    num(&b, "servidor", "porta", 8080.0);
    num(&b, "cache", "ttl", 300.0);

    let learned = ask_and_learn(&b, "quanto custa o produto brasilia").expect("a name");
    assert_eq!(learned.alias, "produto_brasilia");

    // The guess widens retrieval and stops there. A write under the guessed name
    // makes its own entity, because a guess that could merge two histories would
    // be unrecoverable and invisible.
    num(&b, "produto brasilia", "estoque", 7.0);

    let guessed = b.entity("produto_brasilia", When::Now, 0).unwrap();
    assert!(
        guessed.is_some(),
        "a write under a learned name must create its own entity"
    );
    let real = b
        .entity("Produto Brasília", When::Now, 0)
        .unwrap()
        .expect("the original");
    let predicates: Vec<_> = real.facts.iter().map(|f| f.predicate.clone()).collect();
    assert_eq!(
        predicates,
        ["preco"],
        "the guessed name must not have grafted a fact onto the real entity"
    );
}

#[test]
fn question_filler_never_becomes_a_name() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "filler");
    // One entity, so every question about it is unanimous and the only thing
    // standing between "qual" and permanent aliashood is the cosine floor.
    num(&b, "pgbouncer", "porta", 6432.0);

    ask_and_learn(&b, "qual e a porta do pgbouncer mesmo");

    let names: Vec<String> = b
        .aliases("pgbouncer")
        .unwrap()
        .into_iter()
        .map(|a| a.key)
        .collect();
    for junk in ["qual", "mesmo", "porta", "a_porta", "do_pgbouncer"] {
        assert!(
            !names.contains(&junk.to_string()),
            "{junk:?} is not a name for pgbouncer; learned {names:?}"
        );
    }
}

#[test]
fn an_ambiguous_question_teaches_nothing() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "ambiguous");
    num(&b, "Produto Brasília", "preco", 20.0);
    num(&b, "Produto Bahia", "preco", 30.0);
    num(&b, "Produto Ceará", "preco", 40.0);

    // "preco" is about all three. Learning from it would pin every later price
    // question to whichever one happened to rank first today.
    assert!(ask_and_learn(&b, "preco").is_none());
    for e in ["Produto Brasília", "Produto Bahia", "Produto Ceará"] {
        assert!(b.aliases(e).unwrap().is_empty());
    }
}

#[test]
fn a_term_that_already_names_something_is_left_alone() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "taken");
    b.link("produto_a", "fornecido_por", "acme", Some(ts("2026-01-01")))
        .unwrap();
    num(&b, "acme", "pais", 55.0);

    // The question names produto_a and the answer is reached by walking to acme.
    // Concluding that `produto_a` is a name for `acme` would be the worst
    // possible reading of a graph answer.
    ask_and_learn(&b, "de que pais vem o produto_a");

    let names: Vec<String> = b
        .aliases("acme")
        .unwrap()
        .into_iter()
        .map(|a| a.key)
        .collect();
    assert!(
        !names.contains(&"produto_a".to_string()),
        "an existing entity key must never be learned as another entity's name; got {names:?}"
    );
}

#[test]
fn a_predicate_is_never_learned_as_a_name() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "predicate");
    num(&b, "servidor_web", "porta", 8080.0);
    // A single entity makes every question unanimous, so only the predicate
    // exclusion can save "porta" here. Learning it would send every later port
    // question to this one server.
    ask_and_learn(&b, "a porta");

    let names: Vec<String> = b
        .aliases("servidor_web")
        .unwrap()
        .into_iter()
        .map(|a| a.key)
        .collect();
    assert!(!names.contains(&"porta".to_string()), "learned {names:?}");
}

#[test]
fn a_name_that_points_at_two_things_keeps_only_the_stronger_reading() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "one-name-one-thing");
    num(&b, "pgbouncer", "porta", 6432.0);
    num(&b, "Produto Brasília", "preco", 20.0);

    ask_and_learn(&b, "pgbounce");
    ask_and_learn(&b, "pgbounce brasilia");

    // Whatever the outcome, the term resolves to exactly one entity: the alias
    // channel's entire value is precision, and a term it cannot resolve is worse
    // than no term at all.
    let holders: Vec<&str> = ["pgbouncer", "Produto Brasília"]
        .into_iter()
        .filter(|e| {
            b.aliases(e)
                .unwrap()
                .iter()
                .any(|a| a.key == "pgbounce" || a.key == "pgbounce_brasilia")
        })
        .collect();
    assert!(
        holders.len() <= 1,
        "one name ended up pointing at {holders:?}"
    );
}

#[test]
fn a_declared_name_outranks_a_learned_one() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "trust");
    num(&b, "Produto Brasília", "preco", 20.0);
    num(&b, "servidor", "porta", 8080.0);
    num(&b, "cache", "ttl", 300.0);

    ask_and_learn(&b, "quanto custa o produto brasilia");
    b.declare_alias("Produto Brasília", "pb").unwrap();

    let names = b.aliases("Produto Brasília").unwrap();
    let declared = names.iter().find(|a| a.key == "pb").expect("declared");
    let learned = names
        .iter()
        .find(|a| a.key == "produto_brasilia")
        .expect("learned");

    assert_eq!(declared.source, AliasSource::Declared);
    assert_eq!(declared.weight, 1.0);
    assert_eq!(learned.source, AliasSource::Learned);
    assert!(
        learned.weight < declared.weight,
        "a guess must never rank level with a statement: {learned:?} vs {declared:?}"
    );
}

#[test]
fn declaring_a_learned_name_promotes_it_in_place() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "promote");
    num(&b, "Produto Brasília", "preco", 20.0);
    num(&b, "servidor", "porta", 8080.0);
    num(&b, "cache", "ttl", 300.0);
    ask_and_learn(&b, "quanto custa o produto brasilia");

    b.declare_alias("Produto Brasília", "produto brasilia")
        .unwrap();

    let names = b.aliases("Produto Brasília").unwrap();
    assert_eq!(names.len(), 1, "confirming a guess must not duplicate it");
    assert_eq!(names[0].source, AliasSource::Declared);

    // And now, being declared, it decides identity.
    num(&b, "produto brasilia", "estoque", 7.0);
    let view = b
        .entity("Produto Brasília", When::Now, 0)
        .unwrap()
        .expect("entity");
    assert_eq!(view.facts.len(), 2);
}

// --- the read path stays a read path ------------------------------------------

#[test]
fn recall_alone_never_writes() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "pure");
    num(&b, "Produto Brasília", "preco", 20.0);
    num(&b, "servidor", "porta", 8080.0);
    num(&b, "cache", "ttl", 300.0);

    let q = "quanto custa o produto brasilia";
    let first: Vec<i64> = b
        .recall(&RecallQuery::new(q))
        .unwrap()
        .iter()
        .map(|h| h.fact.id)
        .collect();
    for _ in 0..3 {
        let again: Vec<i64> = b
            .recall(&RecallQuery::new(q))
            .unwrap()
            .iter()
            .map(|h| h.fact.id)
            .collect();
        assert_eq!(first, again, "asking twice changed the answer");
    }
    assert!(
        b.aliases("Produto Brasília").unwrap().is_empty(),
        "recall learned a name nobody asked it to learn"
    );
}

#[test]
fn a_learned_name_anchors_the_walk() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "anchor");
    // The answer -- acme's country -- shares no word with the question and is one
    // hop past the entity the question names by a spelling the brain never saw.
    b.link(
        "Produto Brasília",
        "fornecido_por",
        "acme",
        Some(ts("2026-01-01")),
    )
    .unwrap();
    b.remember(&Assertion::new("acme", "pais", Object::text("Chile")))
        .unwrap();
    num(&b, "servidor", "porta", 8080.0);
    num(&b, "cache", "ttl", 300.0);

    b.declare_alias("Produto Brasília", "produto brasilia")
        .unwrap();

    let hits = b
        .recall(&RecallQuery::new("de que pais vem o produto brasilia"))
        .unwrap();
    assert!(
        hits.iter().any(|h| h.channels.contains(&Channel::Graph)),
        "the walk found nothing to start from: {:?}",
        hits.iter().map(|h| &h.fact.statement).collect::<Vec<_>>()
    );
}

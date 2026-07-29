//! Passo 8: what the question did not ask for.
//!
//! Two re-ranking rules, both applied to the fused score and both inert unless
//! the question said something specific enough to justify them:
//!
//! - a fact about an entity the question neither **named** nor reached is here
//!   because it shares a word, which is the weakest reason a fact can be here
//! - a fact whose **predicate** the question did not name, when the question
//!   named one the brain holds, is not what was asked for
//!
//! The evals measure what these buy. Two tests here are load-bearing -- set
//! either constant to 1.0 and exactly one of them fails -- and the rest are
//! guards on what the rules must never start doing: firing on a question that
//! pointed nowhere, deleting a candidate, or learning to always answer one hop
//! out. A guard cannot fail with the rule switched off, and is not supposed to.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::recall::RecallQuery;
use tempfile::TempDir;

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
            Box::new(StepClock::new(
                "2026-01-01T00:00:00Z".parse().unwrap(),
                1000,
            )),
            Box::new(SeededIdGen::new(1)),
        )
        .unwrap();
        Self { _tmp: tmp, brain }
    }

    fn say(&self, subject: &str, predicate: &str, value: &str) {
        let obj = match value.parse::<f64>() {
            Ok(n) => Object::num(n),
            Err(_) => Object::text(value),
        };
        self.brain
            .remember(&Assertion::new(subject, predicate, obj))
            .unwrap();
    }

    fn link(&self, subject: &str, predicate: &str, object: &str) {
        self.brain
            .remember(&Assertion::new(subject, predicate, Object::entity(object)))
            .unwrap();
    }

    fn ask(&self, q: &str) -> Vec<String> {
        self.brain
            .recall(&RecallQuery::new(q))
            .unwrap()
            .into_iter()
            .map(|h| h.fact.statement)
            .collect()
    }
}

// --- off topic ----------------------------------------------------------------

#[test]
fn a_namesake_the_question_never_named_loses_to_the_entity_it_did() {
    let f = Fixture::new();
    // `preco_produto_a` and `produto_c` share two tokens once FTS5 splits on the
    // underscore, which is the whole of the second fact's claim to be here.
    f.link("preco_produto_a", "calculado_por", "regra_desconto");
    f.say("regra_desconto", "definida_em", "src/pricing/discount.rs");
    f.say("produto_c", "preco", "7");
    f.say("produto_d", "preco", "9");
    f.say("cache", "ttl", "300");

    let hits = f.ask("onde esta a logica do preco_produto_a");
    assert_eq!(
        hits.first().map(String::as_str),
        Some("regra_desconto definida_em src/pricing/discount.rs")
    );
}

#[test]
fn a_fact_pointing_at_the_named_entity_is_not_off_topic() {
    let f = Fixture::new();
    // The named entity is the *object* here. A rule that only looked at the
    // subject would call this edge off topic and bury the one answer there is.
    f.link("produto_a", "fornecido_por", "globex");
    f.link("produto_b", "fornecido_por", "acme");
    f.link("produto_c", "fornecido_por", "initech");
    f.say("cache", "ttl", "300");

    let hits = f.ask("quem e fornecido por globex");
    assert_eq!(
        hits.first().map(String::as_str),
        Some("produto_a fornecido_por globex")
    );
}

#[test]
fn a_question_that_names_nothing_calls_nothing_off_topic() {
    let f = Fixture::new();
    f.say("Produto Brasília", "preco", "20");
    f.say("servidor", "porta", "8080");
    f.say("cache", "ttl", "300");
    f.say("fila", "tamanho", "10");

    // Names no entity the brain knows, so the anchors are a guess from the fused
    // head. A guess is not an address, and nothing may be demoted for
    // disagreeing with one.
    let hits = f.ask("quanto custa aquilo");
    assert!(
        hits.iter().any(|h| h.contains("Produto Brasília preco")),
        "a paraphrase that points nowhere must not lose its answer: {hits:?}"
    );
}

#[test]
fn demotion_ranks_and_never_removes() {
    let f = Fixture::new();
    f.say("servidor_web", "porta", "8080");
    f.say("servidor_db", "porta", "5432");
    f.say("cache", "ttl", "300");
    f.say("fila", "tamanho", "10");

    // `servidor_db porta 5432` is about an entity the question never named and
    // whose predicate it did name. It must lose, and it must still be there:
    // every signal here is a guess about intent, and a guess that can delete the
    // answer has far too much authority.
    let hits = f.ask("porta do servidor_web");
    assert_eq!(
        hits.first().map(String::as_str),
        Some("servidor_web porta 8080")
    );
    assert!(
        hits.iter().any(|h| h == "servidor_db porta 5432"),
        "the demoted fact was dropped instead of ranked: {hits:?}"
    );
}

// --- the predicate the question named -----------------------------------------

#[test]
fn naming_a_predicate_reaches_past_the_anchors_own_facts() {
    let f = Fixture::new();
    f.say("plano_prata", "assentos", "10");
    f.say("plano_ouro", "assentos", "10");
    f.say("plano_ouro", "suporte", "24x7");
    f.say("plano_bronze", "suporte", "horario comercial");
    f.say("plano_diamante", "suporte", "dedicado");
    f.say("fila", "tamanho", "10");

    // Nothing connects the two plans; they merely both seat ten. The anchor has
    // no `suporte` of its own, and three channels agree on the fact whose words
    // the question's subject matches -- so without this rule the answer is
    // "plano_prata assentos 10", which is true and is not what was asked.
    let hits = f.ask("qual o suporte do plano_prata");
    assert_eq!(
        hits.first().map(String::as_str),
        Some("plano_ouro suporte 24x7")
    );
}

#[test]
fn naming_a_predicate_the_anchor_holds_keeps_the_answer_at_home() {
    let f = Fixture::new();
    f.say("servidor_web", "porta", "8080");
    f.say("servidor_web", "versao", "2.1.0");
    f.link("servidor_web", "roda_em", "cluster_azul");
    f.say("cluster_azul", "regiao", "us-east-1");
    f.say("fila", "tamanho", "10");

    // The mirror of the test above, and the one that catches a rule that has
    // learned to always answer one hop out.
    let hits = f.ask("qual a porta do servidor_web");
    assert_eq!(
        hits.first().map(String::as_str),
        Some("servidor_web porta 8080")
    );
}

#[test]
fn a_word_that_is_not_a_predicate_demotes_nothing() {
    let f = Fixture::new();
    f.say("servidor_web", "porta", "8080");
    f.say("servidor_web", "versao", "2.1.0");
    f.say("cache", "ttl", "300");
    f.say("fila", "tamanho", "10");

    // "custa" is not a predicate this brain holds. The rule has to stay quiet
    // rather than demote every fact for failing to match a word that names
    // nothing -- which would be every fact, and a no-op only by accident.
    let hits = f.ask("quanto custa o servidor_web");
    assert!(
        hits.iter().take(2).any(|h| h.starts_with("servidor_web")),
        "a question describing rather than naming a predicate lost its entity: {hits:?}"
    );
}

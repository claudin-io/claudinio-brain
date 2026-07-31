//! Passo 8: what the question did not ask for.
//!
//! Three re-ranking rules, all applied to the fused score and all inert unless
//! the question said something specific enough to justify them:
//!
//! - a fact about an entity the question neither **named** nor reached is here
//!   because it shares a word, which is the weakest reason a fact can be here
//! - a fact whose **predicate** the question did not name, when the question
//!   named one the brain holds, is not what was asked for
//! - a fact missing every **rare term** the question used, when the question
//!   named no entity at all, is here on resemblance alone
//!
//! The first and the third are the same idea reading the address off whichever
//! the question supplied, and never fire together: one applies when the question
//! named an entity and the other when it did not.
//!
//! The evals measure what these buy. Four tests here are load-bearing -- set one
//! constant to 1.0 and only that rule's tests fail, one each for the first two and
//! two for the third -- and the rest are guards on what the rules must never start
//! doing: firing on a question that pointed nowhere, deleting a candidate,
//! promoting the road over the destination, or learning to always answer one hop
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

// --- the rare term the question used ------------------------------------------

/// The shape the failure was found in: a ladder of near-identical numeric facts,
/// every rung linked to the same class and the same owner, beside three records
/// of prose about something else entirely.
///
/// The ladder is what makes this hard. Six subjects differing only in their
/// numbers give the semantic channel a dense, mutually-similar neighbourhood, and
/// the shared `is_a` and `servido_por` edges let traversal and kinship expand
/// through it -- so three channels can agree on a plan's price for a question that
/// has nothing to do with plans, while the one record containing the identifier
/// has a single vote from BM25.
/// Kept identical to the `retrieval` suite's cases of this shape, down to the
/// wording. The margins here are narrow enough that a shortened sentence or an
/// evened-out owner changes which side wins, and a fixture that no longer
/// reproduces the failure is a test that no longer defends against it.
fn ladder() -> Fixture {
    let f = Fixture::new();
    for (plan, discount, price, budget, tier) in [
        ("v2_starter", "18", "5", "0.5", "associate"),
        ("v2_lite", "18", "9", "1.0", "associate"),
        ("v2_essential", "17", "19", "2.0", "associate"),
        ("v2_pro", "17", "39", "4.0", "senior"),
        ("v2_power", "17", "59", "6.0", "senior"),
        ("v2_ultra", "17", "99", "10.0", "senior"),
    ] {
        f.say(plan, "yearly_discount", discount);
        f.say(plan, "monthly_price", price);
        f.say(plan, "hourly_budget", budget);
        f.link(plan, "is_a", "escada_de_planos");
        f.link(plan, "servido_por", tier);
    }
    f.say(
        "trial",
        "regra_de_migracao",
        "QUALQUER mudanca em _normalize_email TEM de vir acompanhada de uma corrida do \
         scripts/migrate_trial_locks.py, senao os locks existentes ficam orfaos e a dedup reabre",
    );
    f.say(
        "trial",
        "locks",
        "locks no Redis SEM TTL, um por mailbox e um por fingerprint do cartao",
    );
    f.say(
        "trial",
        "pontos_de_imposicao",
        "pre-check no checkout e verificacao no webhook; se o lock pertence a outro uid, \
         cancela a subscricao",
    );
    f.link("trial", "is_a", "escada_de_planos");
    f
}

#[test]
fn a_file_path_as_the_whole_question_finds_the_record_that_names_it() {
    // The failure this rule was written for, at a twentieth of the size. A path
    // names nothing the brain has a name for, so there is no neighbourhood and
    // nothing can be off its topic -- meanwhile the semantic channel has answered
    // anyway, because a vector index always answers, and traversal and kinship
    // then expand from *those* hits. Three channels end up agreeing about a plan's
    // discount while the one record naming the file has a single vote from BM25.
    //
    // A term the brain uses once is an address. Nothing arrives at it by
    // resembling something; it arrives by being about it.
    let f = ladder();

    let hits = f.ask("migrate_trial_locks.py");
    assert!(
        hits.first()
            .is_some_and(|h| h.contains("migrate_trial_locks.py")),
        "the record naming the file lost to a plan's discount: {hits:?}"
    );
}

#[test]
fn a_rare_term_inside_a_sentence_still_addresses_the_record() {
    // The same question with the words a person actually types around it. The
    // filler is common and carries no address, so it must neither add one nor
    // dilute the one term that does.
    let f = ladder();

    let hits = f.ask("onde e que se usa o _normalize_email");
    assert!(
        hits.first().is_some_and(|h| h.contains("_normalize_email")),
        "the filler words outvoted the identifier: {hits:?}"
    );
}

#[test]
fn a_bare_identifier_was_already_findable_and_stays_so() {
    // Not load-bearing, and worth having anyway: this is the question the failure
    // was *reported* as, and on a brain this size BM25 alone already wins it. It
    // guards the rule against breaking the easy case while fixing the hard one --
    // the two above are the ones that need it.
    let f = ladder();

    let hits = f.ask("_normalize_email");
    assert!(
        hits.first().is_some_and(|h| h.contains("_normalize_email")),
        "the record containing the identifier lost to a plan's price: {hits:?}"
    );
}

#[test]
fn a_rare_term_ranks_and_never_removes() {
    // Same as every other rule here: this is a guess about intent, and a guess
    // that can delete a candidate has far too much authority. The demoted facts
    // are still an answer, just not the first one.
    let f = ladder();

    let hits = f.ask("_normalize_email");
    assert!(
        hits.iter().any(|h| h.starts_with("v2_")),
        "demotion removed the plans instead of ranking them: {hits:?}"
    );
}

#[test]
fn a_question_that_named_an_entity_is_left_to_the_off_topic_rule() {
    // The gate, and the reason it is not tidiness. `produto_a` is rare here and
    // the answer does not contain it -- the bridge that was crossed to reach the
    // answer does. Ungated, this rule would promote the road over the
    // destination, which is the precise failure the graph suite exists to catch.
    let f = Fixture::new();
    f.link("produto_a", "fornecido_por", "acme");
    f.say("acme", "pais", "brasil");
    f.link("produto_b", "fornecido_por", "globex");
    f.say("globex", "pais", "mexico");
    f.say("produto_c", "preco", "7");

    let hits = f.ask("de que pais vem o produto_a");
    assert_eq!(hits.first().map(String::as_str), Some("acme pais brasil"));
}

#[test]
fn an_ordinary_question_about_the_ladder_is_still_answered_from_the_ladder() {
    // The other half of the gate. The prose records are the ones carrying rare
    // terms in this brain, so a rule that fired here would have every reason to
    // promote them -- and a question about a plan's price would come back with a
    // paragraph about Redis locks.
    //
    // Deliberately not asserting *which* rung comes first. It is `v2_power` today
    // and the question asks for `v2_pro`, which is a real weakness of a ladder of
    // near-identical facts and is recorded as such: it is the one case the
    // retrieval suite reports as a miss. Asserting the rung here would be
    // asserting that miss, and it belongs in the evals where it can be measured
    // rather than in a test that would have to be edited when it is fixed.
    let f = ladder();

    let hits = f.ask("qual o monthly_price do v2_pro");
    assert!(
        hits.first()
            .is_some_and(|h| h.contains("monthly_price") && h.starts_with("v2_")),
        "a question about a plan was answered with the prose: {hits:?}"
    );
}

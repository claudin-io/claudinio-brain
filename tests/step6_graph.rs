//! Passo 6: expansion over the graph.
//!
//! The claim under test is the one that justifies a graph existing at all: the
//! answer to a question is often not stored anywhere near the words of the
//! question, and the relations are the map to it. Everything here is written
//! against the public surface -- `Brain::entity` for the walk itself, `recall`
//! for what the walk does to an answer -- because that is what any caller can
//! actually observe.

use brain::brain::{Assertion, Brain, Object};
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

/// The keys of everything the walk reached, for compact assertions.
fn neighbours(b: &Brain, name: &str, depth: u32) -> Vec<String> {
    let view = b.entity(name, When::Now, depth).unwrap().expect("entity");
    view.neighbours.into_iter().map(|n| n.key).collect()
}

// --- the walk -----------------------------------------------------------------

#[test]
fn the_walk_stops_at_the_depth_it_was_given() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "depth");
    // a -> b -> c -> d. Depth two must see b and c, and must not see d: the
    // neighbourhood grows faster than its usefulness, and an unbounded walk
    // eventually returns the whole brain for every question.
    b.link("a", "aponta_para", "b", Some(ts("2026-01-01")))
        .unwrap();
    b.link("b", "aponta_para", "c", Some(ts("2026-01-01")))
        .unwrap();
    b.link("c", "aponta_para", "d", Some(ts("2026-01-01")))
        .unwrap();

    assert_eq!(neighbours(&b, "a", 1), ["b"]);
    assert_eq!(neighbours(&b, "a", 2), ["b", "c"]);
    assert_eq!(neighbours(&b, "a", 0), Vec::<String>::new());
}

#[test]
fn the_walk_follows_relations_in_both_directions() {
    // "How much does what acme supplies cost" has to walk an edge backwards.
    // Treating relations as one-way would make half the useful questions
    // unanswerable while looking like a modelling choice rather than a bug.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "reverse");
    b.link(
        "produto_x",
        "fornecido_por",
        "acme_ltda",
        Some(ts("2026-01-01")),
    )
    .unwrap();

    assert_eq!(neighbours(&b, "acme_ltda", 1), ["produto_x"]);
}

#[test]
fn a_cycle_terminates_instead_of_looping_forever() {
    // The graph will have cycles -- `a depende_de b`, `b usado_por a` -- and the
    // recursive CTE has no natural stopping point on one. This test is the only
    // thing standing between that and a hang in production.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "cycle");
    b.link("x", "chama", "y", Some(ts("2026-01-01"))).unwrap();
    b.link("y", "chama", "z", Some(ts("2026-01-01"))).unwrap();
    b.link("z", "chama", "x", Some(ts("2026-01-01"))).unwrap();

    let reached = neighbours(&b, "x", 2);
    assert!(reached.contains(&"y".to_string()));
    assert!(reached.contains(&"z".to_string()));
    // The anchor is never reported as its own neighbour, even though the cycle
    // walks straight back into it.
    assert!(!reached.contains(&"x".to_string()), "got {reached:?}");
}

#[test]
fn an_edge_that_has_closed_is_not_walked_in_the_present_but_is_in_the_past() {
    // Relations are facts, so a supplier that changed is a closed interval rather
    // than a deleted row. A walk that ignored that would answer today's question
    // with last year's structure -- silently, and with full confidence.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "temporal");
    b.link("motor", "fornecido_por", "velha_sa", Some(ts("2026-01-01")))
        .unwrap();
    b.link("motor", "fornecido_por", "nova_sa", Some(ts("2026-06-01")))
        .unwrap();

    assert_eq!(neighbours(&b, "motor", 1), ["nova_sa"]);

    let past = b
        .entity("motor", When::AsOf(ts("2026-03-01")), 1)
        .unwrap()
        .unwrap();
    let keys: Vec<String> = past.neighbours.into_iter().map(|n| n.key).collect();
    assert_eq!(keys, ["velha_sa"], "the walk ignored the temporal filter");
}

#[test]
fn a_hub_is_reported_but_never_expanded_through() {
    // One entity everything points at -- a company, a repo, a `production` tag --
    // would otherwise make every fact reachable from every other, which is the
    // same as having no graph at all.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "hub");
    b.link("origem", "usa", "hub", Some(ts("2026-01-01")))
        .unwrap();
    // Past the degree cut. The 60 satellites are what makes `hub` a hub.
    for i in 0..60 {
        b.link(&format!("sat_{i}"), "usa", "hub", Some(ts("2026-01-01")))
            .unwrap();
    }

    let reached = neighbours(&b, "origem", 2);
    assert!(
        reached.contains(&"hub".to_string()),
        "the hub itself must still be an answer: {reached:?}"
    );
    assert!(
        !reached.iter().any(|k| k.starts_with("sat_")),
        "expansion went through the hub: {reached:?}"
    );
}

#[test]
fn a_neighbour_carries_the_relation_that_reached_it() {
    // Provenance for a structural answer. Without it, a fact appearing in a result
    // for no visible lexical reason is indistinguishable from a ranking bug.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "via");
    b.link("pedido_9", "pago_com", "cartao_1", Some(ts("2026-01-01")))
        .unwrap();

    let view = b.entity("pedido_9", When::Now, 1).unwrap().unwrap();
    assert_eq!(view.neighbours[0].hops, 1);
    assert!(
        view.neighbours[0].via.contains("pago_com"),
        "got {:?}",
        view.neighbours[0].via
    );
}

#[test]
fn asking_about_an_unknown_entity_is_empty_rather_than_an_error() {
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "unknown");
    assert!(b.entity("nao_existe", When::Now, 2).unwrap().is_none());
}

// --- what the walk does to an answer ------------------------------------------

/// A brain where the answer is one hop from the words of the question.
fn one_hop_brain(tmp: &TempDir) -> Brain {
    let b = brain(tmp, "hop");
    b.link(
        "laptop_x",
        "montado_por",
        "nova_ind",
        Some(ts("2026-01-01")),
    )
    .unwrap();
    b.remember(&Assertion::new("nova_ind", "sede", Object::text("curitiba")).at(ts("2026-01-01")))
        .unwrap();
    // Noise with the same shape, so the right answer has to be found rather than
    // be the only candidate.
    for (prod, maker, city) in [
        ("teclado_y", "sul_tech", "porto_alegre"),
        ("monitor_z", "leste_sa", "recife"),
        ("mouse_w", "norte_ltda", "belem"),
    ] {
        b.link(prod, "montado_por", maker, Some(ts("2026-01-01")))
            .unwrap();
        b.remember(&Assertion::new(maker, "sede", Object::text(city)).at(ts("2026-01-01")))
            .unwrap();
    }
    b
}

#[test]
fn recall_answers_with_the_fact_one_hop_away_not_with_the_road_to_it() {
    let tmp = TempDir::new().unwrap();
    let b = one_hop_brain(&tmp);

    let hits = b
        .recall(&RecallQuery::new("qual a sede de quem monta o laptop_x"))
        .unwrap();
    let top = &hits[0];
    assert_eq!(
        top.fact.object_text.as_deref(),
        Some("curitiba"),
        "top hit was {:?}",
        top.fact.statement
    );
    assert!(
        top.channels.contains(&Channel::Graph),
        "the answer should be attributed to traversal: {:?}",
        top.channels
    );
}

#[test]
fn without_the_graph_channel_the_same_question_cannot_be_answered() {
    // The control for the test above. If this ever starts passing on content
    // channels alone, the graph channel is no longer earning its keep and the
    // eval table should say so before anyone argues about it.
    let tmp = TempDir::new().unwrap();
    let b = one_hop_brain(&tmp);

    let hits = b
        .recall(
            &RecallQuery::new("qual a sede de quem monta o laptop_x").channels(&[
                Channel::Bm25,
                Channel::Alias,
                Channel::Semantic,
            ]),
        )
        .unwrap();
    assert!(
        hits[0].fact.object_text.as_deref() != Some("curitiba"),
        "content channels alone already answered it"
    );
    assert!(
        !hits.iter().any(|h| h.channels.contains(&Channel::Graph)),
        "a disabled channel still voted"
    );
}

#[test]
fn a_question_about_the_relation_itself_keeps_the_relation_on_top() {
    // The exception that keeps bridge demotion honest: when the question names the
    // predicate of the edge, the edge *is* the answer, and pushing it down would
    // trade one wrong ranking for another.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "relation");
    b.link("laptop_x", "fabricante", "nova_ind", Some(ts("2026-01-01")))
        .unwrap();
    b.remember(&Assertion::new("nova_ind", "sede", Object::text("curitiba")).at(ts("2026-01-01")))
        .unwrap();

    let hits = b
        .recall(&RecallQuery::new("qual o fabricante do laptop_x"))
        .unwrap();
    assert_eq!(
        hits[0].fact.predicate, "fabricante",
        "top hit was {:?}",
        hits[0].fact.statement
    );
}

#[test]
fn expansion_never_returns_a_fact_the_temporal_filter_hides() {
    // The invariant that makes traversal safe to add: it can widen *which* facts
    // are considered, never *when* they are true.
    let tmp = TempDir::new().unwrap();
    let b = brain(&tmp, "hidden");
    b.link("pedido_9", "atendido_por", "loja_1", Some(ts("2026-01-01")))
        .unwrap();
    b.remember(&Assertion::new("loja_1", "gerente", Object::text("ana")).at(ts("2026-01-01")))
        .unwrap();
    b.remember(&Assertion::new("loja_1", "gerente", Object::text("bruno")).at(ts("2026-06-01")))
        .unwrap();

    let hits = b
        .recall(&RecallQuery::new("quem gerencia o pedido_9"))
        .unwrap();
    let managers: Vec<&str> = hits
        .iter()
        .filter(|h| h.fact.predicate == "gerente")
        .filter_map(|h| h.fact.object_text.as_deref())
        .collect();
    assert_eq!(
        managers,
        ["bruno"],
        "a closed fact came back through the graph"
    );
}

#[test]
fn recall_stays_deterministic_with_traversal_on() {
    let tmp = TempDir::new().unwrap();
    let b = one_hop_brain(&tmp);
    let q = RecallQuery::new("qual a sede de quem monta o laptop_x");

    let first: Vec<i64> = b
        .recall(&q)
        .unwrap()
        .into_iter()
        .map(|h| h.fact.id)
        .collect();
    for _ in 0..5 {
        let again: Vec<i64> = b
            .recall(&q)
            .unwrap()
            .into_iter()
            .map(|h| h.fact.id)
            .collect();
        assert_eq!(again, first, "traversal made the ranking unstable");
    }
}

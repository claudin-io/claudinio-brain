//! Passo 5: the semantic channel.
//!
//! Static embeddings -- a token to vector lookup table, no transformer
//! inference. That buys determinism for free (no sampling, no dropout), roughly
//! 8000 embeddings/sec on one core, and about 7.7 MB of weights inside the
//! binary. It costs some quality against a real transformer, which is precisely
//! what the eval suites are there to price.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::embed::{Embedder, StaticEmbedder};
use brain::ids::SeededIdGen;
use brain::index::{BruteForceIndex, Vec0Index, VecFilter, VectorIndex};
use brain::recall::{Channel, RecallQuery};
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

fn embedder() -> StaticEmbedder {
    StaticEmbedder::bundled().expect("the bundled model must load")
}

// --- the embedder -------------------------------------------------------------

#[test]
fn the_bundled_model_loads_and_reports_its_shape() {
    let e = embedder();
    assert_eq!(e.dim(), 256);
    assert!(e.model_id().contains("potion"), "got {:?}", e.model_id());
    assert_eq!(e.embed_one("qual o preco do produto A").unwrap().len(), 256);
}

#[test]
fn embedding_is_deterministic_byte_for_byte() {
    // Determinism by construction: a lookup table has no sampling and no
    // dropout. Every snapshot and every eval baseline downstream depends on it.
    let e = embedder();
    let a = e.embed_one("qual o preco do produto A").unwrap();
    let b = e.embed_one("qual o preco do produto A").unwrap();
    assert_eq!(a, b);

    // And across independent instances, since the weights are compiled in.
    let c = embedder().embed_one("qual o preco do produto A").unwrap();
    assert_eq!(a, c);
}

#[test]
fn embeddings_are_l2_normalized_so_cosine_is_a_dot_product() {
    let e = embedder();
    for text in ["preco", "qual o preco do produto A", "unrelated banana"] {
        let v = e.embed_one(text).unwrap();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "{text:?} had norm {norm}");
    }
}

#[test]
fn paraphrases_sit_closer_together_than_unrelated_text() {
    // The whole justification for the channel. If this fails, the 7.7 MB buys
    // nothing and the channel should be cut.
    let e = embedder();
    let cos = |a: &str, b: &str| -> f32 {
        let (x, y) = (e.embed_one(a).unwrap(), e.embed_one(b).unwrap());
        x.iter().zip(&y).map(|(p, q)| p * q).sum()
    };

    let near = cos(
        "what is the price of the product",
        "how much does the product cost",
    );
    let far = cos(
        "what is the price of the product",
        "the server restarted at midnight",
    );
    assert!(
        near > far,
        "paraphrase {near:.3} should beat unrelated {far:.3}"
    );
}

#[test]
fn an_empty_string_embeds_without_panicking() {
    let e = embedder();
    assert_eq!(e.embed_one("").unwrap().len(), 256);
    assert_eq!(e.embed_one("   \n\t ").unwrap().len(), 256);
}

#[test]
fn batching_gives_the_same_vectors_as_embedding_one_at_a_time() {
    let e = embedder();
    let texts = ["preco do produto", "porta do servidor", "banana"];
    let batch = e.embed(&texts).unwrap();
    for (i, t) in texts.iter().enumerate() {
        assert_eq!(batch[i], e.embed_one(t).unwrap(), "mismatch for {t:?}");
    }
}

// --- the index ----------------------------------------------------------------

/// Both index implementations must agree, because one is the fallback for the
/// other. A divergence would mean recall silently changes depending on whether
/// `sqlite-vec` happened to build on the target.
#[test]
fn the_vec0_index_and_the_brute_force_fallback_return_the_same_top_k() {
    let tmp = TempDir::new().unwrap();
    let b = Brain::init(
        &tmp.path().join("t.db"),
        "conformance",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    let e = embedder();

    let corpus = [
        "preco do produto a",
        "porta do servidor web",
        "custo do item a",
        "host do banco de dados",
        "quantidade em estoque",
        "prazo de entrega",
        "nome do fornecedor",
        "cor do produto",
    ];
    for (i, text) in corpus.iter().enumerate() {
        b.remember(
            &Assertion::new(format!("e{i}"), "campo", Object::text(*text)).at(ts("2026-01-01")),
        )
        .unwrap();
    }

    let vec0 = Vec0Index::new(b.store().conn(), e.dim());
    let brute = BruteForceIndex::new(b.store().conn(), e.dim());

    for query in ["quanto custa o produto", "qual a porta", "estoque"] {
        let q = e.embed_one(query).unwrap();
        let a = vec0.search(&q, 5, &VecFilter::default()).unwrap();
        let c = brute.search(&q, 5, &VecFilter::default()).unwrap();

        let ids = |r: &[(i64, f32)]| r.iter().map(|(id, _)| *id).collect::<Vec<_>>();
        assert_eq!(ids(&a), ids(&c), "implementations disagreed on {query:?}");

        // Distances must agree too, not just the ordering.
        for ((_, da), (_, db)) in a.iter().zip(&c) {
            assert!((da - db).abs() < 1e-3, "distance drift: {da} vs {db}");
        }
    }
}

#[test]
fn the_index_filter_hides_closed_facts_like_every_other_channel() {
    let tmp = TempDir::new().unwrap();
    let b = Brain::init(
        &tmp.path().join("t.db"),
        "filter",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    let e = embedder();

    b.remember(&Assertion::new("produto_a", "preco", Object::num(10.0)).at(ts("2026-07-01")))
        .unwrap();
    b.remember(&Assertion::new("produto_a", "preco", Object::num(20.0)).at(ts("2026-07-28")))
        .unwrap();

    let idx = Vec0Index::new(b.store().conn(), e.dim());
    let q = e.embed_one("preco do produto_a").unwrap();

    let open_only = idx.search(&q, 10, &VecFilter::open()).unwrap();
    assert_eq!(open_only.len(), 1, "a closed fact leaked past the filter");

    let all = idx.search(&q, 10, &VecFilter::default()).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn reindex_rebuilds_the_vector_index_from_the_stored_embeddings() {
    // The vec0 table is a derived index, not the source of truth. If its on-disk
    // format ever changes we drop and rebuild rather than migrate -- so this path
    // has to actually work.
    let tmp = TempDir::new().unwrap();
    let b = Brain::init(
        &tmp.path().join("t.db"),
        "reindex",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    for i in 0..5 {
        b.remember(
            &Assertion::new(format!("e{i}"), "campo", Object::text("valor")).at(ts("2026-01-01")),
        )
        .unwrap();
    }

    let before = b.recall(&RecallQuery::new("valor")).unwrap().len();
    b.store()
        .conn()
        .execute("DELETE FROM fact_vec", [])
        .expect("vec0 rows are droppable");

    let rebuilt = b.reindex().unwrap();
    assert_eq!(rebuilt, 5, "reindex did not restore every embedding");
    assert_eq!(b.recall(&RecallQuery::new("valor")).unwrap().len(), before);
}

// --- the channel in recall ----------------------------------------------------

#[test]
fn the_semantic_channel_finds_a_paraphrase_that_shares_no_words() {
    // What the lexical channels structurally cannot do: no token in the question
    // appears in the fact.
    let tmp = TempDir::new().unwrap();
    let b = Brain::init(
        &tmp.path().join("t.db"),
        "semantic",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();

    b.remember(&Assertion::new("laptop", "cost", Object::num(1200.0)).at(ts("2026-01-01")))
        .unwrap();
    for (s, p) in [
        ("server", "hostname"),
        ("queue", "depth"),
        ("cache", "expiry"),
        ("disk", "capacity"),
    ] {
        b.remember(&Assertion::new(s, p, Object::text("x")).at(ts("2026-01-01")))
            .unwrap();
    }

    let hits = b
        .recall(&RecallQuery::new("how expensive is the notebook").channels(&[Channel::Semantic]))
        .unwrap();
    assert!(!hits.is_empty(), "the semantic channel found nothing");
    assert!(
        hits[0].channels.contains(&Channel::Semantic),
        "wrong channel attribution"
    );
}

#[test]
fn semantic_hits_are_deterministic_across_runs() {
    let tmp = TempDir::new().unwrap();
    let b = Brain::init(
        &tmp.path().join("t.db"),
        "det",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    for i in 0..12 {
        b.remember(
            &Assertion::new(format!("item_{i}"), "estado", Object::text("ativo"))
                .at(ts("2026-01-01")),
        )
        .unwrap();
    }

    let q = RecallQuery::new("qual o estado").channels(&[Channel::Semantic]);
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
        assert_eq!(again, first, "semantic ordering was not stable");
    }
}

//! Finding facts.
//!
//! Independent channels retrieve candidates, and reciprocal rank fusion combines
//! them. The point of fusing rather than picking one is that agreement between
//! independent signals is itself evidence: a fact several channels surface is far
//! more likely to be the answer than one any of them finds alone.
//!
//! Three channels retrieve by content -- words, entity names, meaning. The fourth
//! retrieves by *structure*: it walks the graph out from whatever the question
//! names and reports what the walk found. That one is the reason this is a brain
//! and not a search index, because the answer to "which country does produto_a
//! come from" is not stored anywhere near produto_a. It is one hop away, and the
//! relation is the map.
//!
//! Everything is filtered temporally *before* ranking, so recall answers with
//! what is true, not with everything ever recorded.

use crate::brain::{Brain, BrainError, Fact};
use crate::graph;
use crate::index::{Vec0Index, VecFilter, VectorIndex};
use crate::norm;
use jiff::Timestamp;
use rusqlite::{Connection, params_from_iter};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// Which retriever found a hit. Carried on every result so per-channel
/// contribution is measurable, and so a surprising ranking is debuggable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    /// BM25 over the statement and search text.
    Bm25,
    /// An exact hit on a normalized entity key or alias.
    Alias,
    /// Nearest neighbours in static-embedding space. The only channel that can
    /// match a paraphrase sharing no words with the fact.
    Semantic,
    /// Facts found by walking relations out from what the question names. The
    /// only channel that can answer with a fact stored nowhere near the words of
    /// the question.
    Graph,
}

impl Channel {
    pub const ALL: &'static [Channel] = &[
        Channel::Bm25,
        Channel::Alias,
        Channel::Semantic,
        Channel::Graph,
    ];

    /// The channels that retrieve from a fact's own content. The graph channel
    /// is not one of them: it expands what these find.
    const CONTENT: &'static [Channel] = &[Channel::Bm25, Channel::Alias, Channel::Semantic];
}

/// How the timeline is filtered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum When {
    /// Only what currently holds. The default.
    #[default]
    Now,
    /// What held at a given instant.
    AsOf(Timestamp),
    /// The whole trajectory, closed intervals included. Retractions stay hidden:
    /// a retracted claim was never true, so replaying it would be a lie.
    History,
}

#[derive(Debug, Clone)]
pub struct RecallQuery {
    pub text: String,
    pub when: When,
    pub limit: usize,
    pub scope: Option<String>,
    pub channels: Vec<Channel>,
}

impl RecallQuery {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            when: When::Now,
            limit: 10,
            scope: None,
            channels: Channel::ALL.to_vec(),
        }
    }

    pub fn as_of(mut self, t: Timestamp) -> Self {
        self.when = When::AsOf(t);
        self
    }

    pub fn history(mut self) -> Self {
        self.when = When::History;
        self
    }

    pub fn limit(mut self, n: usize) -> Self {
        self.limit = n;
        self
    }

    pub fn scope(mut self, s: impl Into<String>) -> Self {
        self.scope = Some(s.into());
        self
    }

    pub fn channels(mut self, c: &[Channel]) -> Self {
        self.channels = c.to_vec();
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Hit {
    pub fact: Fact,
    pub score: f64,
    /// Every channel that surfaced this fact, sorted for stable output.
    pub channels: Vec<Channel>,
}

/// Reciprocal rank fusion constant. 60 is the value from the original TREC work
/// and the de facto default; it damps the head enough that one channel's
/// confident top hit cannot bulldoze agreement between channels further down.
const RRF_K: f64 = 60.0;

/// How many candidates each channel contributes before fusion.
const CHANNEL_DEPTH: usize = 50;

/// How many fused hits seed graph expansion when the question names no entity
/// the brain recognizes.
const SEED_DEPTH: usize = 8;

/// What a fused score is multiplied by when the fact is an edge the walk crossed
/// to reach something better.
///
/// An edge that was traversed is a *road*, and roads are rarely the destination:
/// "produto_a fornecido_por acme" is how the brain got to acme's country, not an
/// answer to which country that is. Without this, the bridge wins on agreement
/// alone -- it is the fact that literally contains the words of the question --
/// and the answer sits at rank two forever, which is exactly what the graph suite
/// measured before this step.
///
/// A bridge is spared whenever the question names its predicate, because then the
/// relation *is* what was asked for ("quem fornece o produto_a"). Demotion is a
/// scale rather than an exile so that a bridge still outranks the noise below it.
///
/// Calibrated, not guessed. Sweeping 1.0/0.75/0.5/0.25 moves graph top-1 through
/// 0.000/0.625/0.750/0.750 while retrieval and temporal never budge. 1.0 is the
/// control: with expansion on but demotion off, Recall@10 is already 1.000 and
/// top-1 is still 0.000 -- the walk had found the answer all along and the bridge
/// was sitting on it. 0.5 is the mildest value that reaches the ceiling.
const BRIDGE_DEMOTION: f64 = 0.5;

/// Minimum cosine similarity for a semantic hit to count.
///
/// Nearest-neighbour search always returns *something* -- there is no such thing
/// as "no match" in a vector index. Without a floor, a nonsense question returns
/// the k least-unrelated facts, `recall` stops ever being empty, and the noise
/// gets full RRF credit that can outvote a genuine lexical hit.
///
/// 0.20 was calibrated, not guessed. Sweeping 0.20/0.25/0.30/0.35 against the
/// suites: every value keeps a nonsense query empty, but anything above 0.20
/// costs graph Recall@10 (1.000 at 0.20, 0.875 at 0.35) by cutting off the
/// one-hop answers that sit furthest out. Any change here must be re-measured.
const MIN_SEMANTIC_COSINE: f32 = 0.20;

impl Brain {
    /// Retrieves facts relevant to a question.
    pub fn recall(&self, q: &RecallQuery) -> std::result::Result<Vec<Hit>, BrainError> {
        let conn = self.store().conn();
        let filter = TemporalFilter::new(q);

        // Rank per channel, then fuse. `fused` maps fact id -> (rrf score,
        // channels), in a BTreeMap so iteration order never varies by run.
        let mut fused: BTreeMap<i64, (f64, Vec<Channel>)> = BTreeMap::new();
        for channel in Channel::CONTENT.iter().filter(|c| q.channels.contains(c)) {
            let ids = match channel {
                Channel::Bm25 => bm25_channel(conn, &q.text, &filter)?,
                Channel::Alias => alias_channel(conn, &q.text, &filter)?,
                Channel::Semantic => self.semantic_channel(&q.text, q)?,
                Channel::Graph => unreachable!("the graph channel expands, it does not retrieve"),
            };
            fuse(&mut fused, ids, *channel);
        }

        // The graph runs last because it expands what the others found. Its own
        // anchors come first from the question, and only fall back to the fused
        // head when the question names nothing the brain knows by name.
        if q.channels.contains(&Channel::Graph) {
            let walk = self.walk(conn, q, &filter, &fused)?;
            fuse(&mut fused, walk.ranked.clone(), Channel::Graph);
            demote_bridges(&mut fused, &walk);
        }

        let mut order: Vec<(i64, f64, Vec<Channel>)> = fused
            .into_iter()
            .map(|(id, (score, mut ch))| {
                ch.sort_unstable();
                ch.dedup();
                (id, score, ch)
            })
            .collect();

        // Descending score, then ascending id. The id tie-break is what makes
        // output reproducible: equal scores would otherwise come back in whatever
        // order SQLite happened to produce.
        order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        order.truncate(q.limit);

        let mut hits = Vec::with_capacity(order.len());
        for (id, score, channels) in order {
            hits.push(Hit {
                fact: self.fact(id)?,
                score,
                channels,
            });
        }
        Ok(hits)
    }
}

/// Adds one channel's ranking to the fusion.
fn fuse(fused: &mut BTreeMap<i64, (f64, Vec<Channel>)>, ids: Vec<i64>, channel: Channel) {
    for (rank, id) in ids.into_iter().enumerate() {
        let e = fused.entry(id).or_insert((0.0, Vec::new()));
        e.0 += 1.0 / (RRF_K + (rank + 1) as f64);
        e.1.push(channel);
    }
}

/// What one graph expansion produced, and everything the re-rank needs to know
/// about how it got there.
struct Walk {
    /// Facts of reached entities, best first.
    ranked: Vec<i64>,
    /// Edge facts lying on the route to something the walk is reporting.
    roads: BTreeSet<i64>,
    /// The predicate of each crossed edge, so a question that asks for the
    /// relation itself can spare it.
    bridge_predicates: BTreeMap<i64, String>,
    /// Normalized terms the question uses.
    asked: BTreeSet<String>,
}

/// Pushes traversed edges below what they led to.
///
/// See [`BRIDGE_DEMOTION`]. Only edges on the route to a reported fact are
/// demoted, and the route may be longer than one edge: reaching a contact through
/// a supplier makes *both* hops roads, even though the middle entity contributed
/// no answer of its own.
fn demote_bridges(fused: &mut BTreeMap<i64, (f64, Vec<Channel>)>, walk: &Walk) {
    for edge_id in &walk.roads {
        let asked_for = walk
            .bridge_predicates
            .get(edge_id)
            .is_some_and(|p| walk.asked.contains(p));
        if asked_for {
            continue;
        }
        if let Some(entry) = fused.get_mut(edge_id) {
            entry.0 *= BRIDGE_DEMOTION;
        }
    }
}

impl Brain {
    /// Expands outward from what the question names, and ranks what is out there.
    fn walk(
        &self,
        conn: &Connection,
        q: &RecallQuery,
        filter: &TemporalFilter,
        fused: &BTreeMap<i64, (f64, Vec<Channel>)>,
    ) -> std::result::Result<Walk, BrainError> {
        let terms = normalized_terms(&q.text);
        let asked: BTreeSet<String> = terms.iter().cloned().collect();

        // Anchoring on entities the question actually names is what keeps this
        // channel precise: the walk starts where the user pointed, not wherever
        // the ranking happened to land. Seeding from the fused head is the
        // fallback for a pure paraphrase, which names nothing the brain knows.
        let mut anchors = graph::named_entities(conn, &terms)?;
        if anchors.is_empty() {
            anchors = seed_entities(conn, fused)?;
        }

        let n = graph::expand(conn, &anchors, filter, graph::MAX_DEPTH)?;
        let reached = n.reached();
        let hop: BTreeMap<i64, u32> = reached.iter().copied().collect();
        let ids: Vec<i64> = reached.iter().map(|&(id, _)| id).collect();

        let mut candidates = graph::facts_of(conn, &ids, filter)?;
        // A crossed edge is not reported by the channel that crossed it. It is
        // already in the results via the content channels if it deserves to be.
        candidates.retain(|c| !n.bridges.contains(&c.fact_id));
        // Nearest first; within a hop, the fact whose predicate the question
        // named -- "de que pais" is asking for `pais`, and a brain that knows its
        // own ontology should use that.
        candidates.sort_by_key(|c| {
            (
                hop.get(&c.entity_id).copied().unwrap_or(u32::MAX),
                !asked.contains(&c.predicate),
                c.fact_id,
            )
        });
        candidates.truncate(CHANNEL_DEPTH);

        Ok(Walk {
            roads: n.roads_to(candidates.iter().map(|c| c.entity_id)),
            bridge_predicates: graph::predicates(conn, n.bridges.iter().copied())?,
            ranked: candidates.iter().map(|c| c.fact_id).collect(),
            asked,
        })
    }
}

/// The entities behind the best hits so far, both ends of an edge included.
fn seed_entities(
    conn: &Connection,
    fused: &BTreeMap<i64, (f64, Vec<Channel>)>,
) -> std::result::Result<Vec<i64>, BrainError> {
    let mut best: Vec<(i64, f64)> = fused.iter().map(|(&id, (s, _))| (id, *s)).collect();
    best.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    best.truncate(SEED_DEPTH);
    if best.is_empty() {
        return Ok(Vec::new());
    }

    let ph = vec!["?"; best.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT entity_id, object_entity_id FROM fact WHERE id IN ({ph})"
    ))?;
    let rows = stmt.query_map(params_from_iter(best.iter().map(|&(id, _)| id)), |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?))
    })?;

    let mut out = BTreeSet::new();
    for row in rows {
        let (entity, object) = row?;
        out.insert(entity);
        out.extend(object);
    }
    Ok(out.into_iter().collect())
}

impl Brain {
    /// Nearest neighbours by embedding.
    ///
    /// Uses the `vec0` index, whose metadata filter can express "currently
    /// holds" but not the full as-of predicate. Anything richer than that is
    /// re-checked against the facts afterwards rather than pushed into the
    /// index, because a vector index that tries to be a temporal query engine is
    /// a vector index that drifts out of step with one.
    fn semantic_channel(
        &self,
        text: &str,
        q: &RecallQuery,
    ) -> std::result::Result<Vec<i64>, BrainError> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let vector = self.embedder().embed_one(text)?;
        let filter = VecFilter {
            open_only: matches!(q.when, When::Now),
            scope: q.scope.clone(),
        };

        // Over-fetch when a post-filter will discard rows, so the temporal modes
        // do not silently return fewer candidates than the other channels.
        let depth = match q.when {
            When::Now => CHANNEL_DEPTH,
            _ => CHANNEL_DEPTH * 4,
        };
        let index = Vec0Index::new(self.store().conn(), self.embedder().dim());
        let raw = index.search(&vector, depth, &filter)?;

        let mut out = Vec::with_capacity(raw.len());
        for (id, distance) in raw {
            if cosine_from_l2(distance) < MIN_SEMANTIC_COSINE {
                // Distances come back ascending, so the first rejection ends it.
                break;
            }
            let f = self.fact(id)?;
            let keep = match q.when {
                When::Now => f.is_open(),
                When::AsOf(t) => f.covers(t),
                When::History => f.retracted_at.is_none(),
            };
            if keep {
                out.push(id);
            }
            if out.len() >= CHANNEL_DEPTH {
                break;
            }
        }
        Ok(out)
    }
}

/// Recovers cosine similarity from the L2 distance `vec0` reports.
///
/// Vectors are L2-normalized before int8 packing, so each component is scaled by
/// 127 and `d^2 = 2 * 127^2 * (1 - cos)`. Inverting that is exact up to
/// quantization error, and avoids a second pass over the vectors just to score
/// them a different way.
fn cosine_from_l2(distance: f32) -> f32 {
    const SCALE: f32 = 127.0;
    1.0 - (distance * distance) / (2.0 * SCALE * SCALE)
}

/// The SQL fragment restricting which facts may answer.
///
/// Shared with [`crate::graph`] on purpose. Traversal has to obey exactly the
/// same notion of "which facts count" as retrieval does, and the only way to
/// guarantee two subsystems agree about that is to give them one definition
/// instead of two that look alike today.
pub(crate) struct TemporalFilter {
    sql: String,
    at: Option<i64>,
    scope: Option<String>,
}

impl TemporalFilter {
    pub(crate) fn new(q: &RecallQuery) -> Self {
        Self::for_when(q.when, q.scope.clone())
    }

    pub(crate) fn for_when(when: When, scope: Option<String>) -> Self {
        // A retracted fact is excluded in every mode, including History.
        let mut sql = String::from(" AND f.retracted_at IS NULL");
        let mut at = None;
        match when {
            When::Now => sql.push_str(" AND f.valid_to IS NULL"),
            When::AsOf(t) => {
                sql.push_str(" AND f.valid_from <= ?a AND (f.valid_to IS NULL OR ?a < f.valid_to)");
                at = Some(t.as_microsecond());
            }
            When::History => {}
        }
        if scope.is_some() {
            sql.push_str(" AND f.scope = ?s");
        }
        Self { sql, at, scope }
    }

    /// Rewrites the named markers (`?a`, `?s`) into plain positional `?`, pushing
    /// each value onto `into` in the order SQLite will read them.
    ///
    /// Named markers exist only so the fragment above stays readable: `?a`
    /// appears twice in the as-of clause, and tracking that by hand against a
    /// positional list is exactly the kind of off-by-one that silently returns
    /// the wrong facts.
    pub(crate) fn bind(&self, into: &mut Vec<rusqlite::types::Value>) -> String {
        let mut sql = String::with_capacity(self.sql.len());
        let mut rest = self.sql.as_str();
        while let Some(i) = rest.find('?') {
            sql.push_str(&rest[..i]);
            sql.push('?');
            match &rest[i..i + 2] {
                "?a" => into.push(self.at.unwrap_or_default().into()),
                "?s" => into.push(self.scope.clone().unwrap_or_default().into()),
                other => unreachable!("unknown filter marker {other}"),
            }
            rest = &rest[i + 2..];
        }
        sql.push_str(rest);
        sql
    }
}

/// BM25 over FTS5, best match first.
fn bm25_channel(
    conn: &Connection,
    text: &str,
    filter: &TemporalFilter,
) -> std::result::Result<Vec<i64>, BrainError> {
    let Some(match_expr) = fts_query(text) else {
        return Ok(Vec::new());
    };

    let mut binds: Vec<rusqlite::types::Value> = vec![match_expr.into()];
    let filter_sql = filter.bind(&mut binds);
    binds.push((CHANNEL_DEPTH as i64).into());

    // `search_text` is weighted above `statement` because it carries the entity
    // key and predicate, which is what a question actually aims at.
    //
    // bm25() returns a *negative* score, more negative being a better match, so
    // ascending order is correct here. Fusion consumes ranks, not scores, which
    // sidesteps the sign entirely.
    let sql = format!(
        "SELECT f.id FROM fact_fts
         JOIN fact f ON f.id = fact_fts.rowid
         WHERE fact_fts MATCH ?{filter_sql}
         ORDER BY bm25(fact_fts, 1.0, 2.0), f.id
         LIMIT ?"
    );

    let mut stmt = conn.prepare(&sql)?;
    let ids = stmt
        .query_map(params_from_iter(binds), |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

/// Facts about entities the query names outright.
///
/// Cheap and very precise: it needs no index beyond the one already on
/// `entity.key`, and it is the layer that handles the cases a purely lexical
/// score fumbles -- a rare identifier buried in a common-word question.
///
/// An entity's own key and a declared alias both count as certain; a learned
/// alias carries the cosine that earned it, and orders below them. That is the
/// only place the declared/learned distinction shows up in ranking: when one
/// question names two entities, the one it named outright is reported first.
fn alias_channel(
    conn: &Connection,
    text: &str,
    filter: &TemporalFilter,
) -> std::result::Result<Vec<i64>, BrainError> {
    let candidates = normalized_terms(text);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = vec!["?"; candidates.len()].join(",");

    // The candidate list appears twice in the SQL -- once for entity keys, once
    // for aliases -- so it is bound twice, in that order.
    let mut binds: Vec<rusqlite::types::Value> = Vec::new();
    binds.extend(candidates.iter().cloned().map(Into::into));
    binds.extend(candidates.iter().cloned().map(Into::into));
    let filter_sql = filter.bind(&mut binds);
    binds.push((CHANNEL_DEPTH as i64).into());

    // `MAX(w)` over a `UNION ALL`, not a bare `UNION`: an entity matched both by
    // its key and by an alias would otherwise appear twice and duplicate all of
    // its facts in the ranking.
    let sql = format!(
        "SELECT f.id FROM fact f
         JOIN (
           SELECT entity_id, MAX(w) AS w FROM (
             SELECT id AS entity_id, 1.0 AS w FROM entity WHERE key IN ({placeholders})
             UNION ALL
             SELECT entity_id, weight AS w FROM entity_alias
               WHERE alias_key IN ({placeholders})
           ) GROUP BY entity_id
         ) named ON named.entity_id = f.entity_id
         WHERE 1 = 1{filter_sql}
         ORDER BY named.w DESC, f.id
         LIMIT ?"
    );

    let mut stmt = conn.prepare(&sql)?;
    let ids = stmt
        .query_map(params_from_iter(binds), |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

/// The identity keys a question could be naming -- of an entity or of a predicate.
///
/// Both single words and adjacent pairs, because "Produto A" is two tokens but
/// one entity. Two is enough in practice and keeps this linear.
///
/// Normalization is [`norm::key`], the same function that assigns identity on
/// write, so "Produto A", "produto-a" and "produto_a" all name the same thing.
/// Accents are preserved here, which means an unaccented question still finds the
/// fact through BM25 but does not *name* it -- identity stays exact even where
/// search is forgiving.
pub(crate) fn normalized_terms(text: &str) -> Vec<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut out = Vec::new();
    for (i, w) in words.iter().enumerate() {
        let single = norm::key(w);
        if single.len() > 1 {
            out.push(single);
        }
        if let Some(next) = words.get(i + 1) {
            let pair = norm::key(&format!("{w} {next}"));
            if pair.len() > 1 {
                out.push(pair);
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Builds an FTS5 MATCH expression from free text.
///
/// Every token is quoted, which makes FTS5 treat it as a literal string rather
/// than as syntax. Without this, a question containing an unbalanced quote, a
/// bare `NEAR(`, or even the word "AND" would be a query-syntax error surfacing
/// to the user as a brain failure.
fn fts_query(text: &str) -> Option<String> {
    let terms: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        // Single characters carry almost no signal in Latin scripts and blow up
        // the candidate set. Non-ASCII single characters are kept: one CJK
        // character is a word.
        .filter(|t| t.chars().count() > 1 || !t.is_ascii())
        .map(|t| format!("\"{}\"", t.replace('"', "")))
        .collect();

    if terms.is_empty() {
        return None;
    }
    Some(terms.join(" OR "))
}

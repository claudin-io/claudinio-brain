//! Finding facts.
//!
//! Two independent channels retrieve candidates, and reciprocal rank fusion
//! combines them. The point of fusing rather than picking one is that agreement
//! between independent signals is itself evidence: a fact both channels surface
//! is far more likely to be the answer than one either finds alone.
//!
//! Everything is filtered temporally *before* ranking, so recall answers with
//! what is true, not with everything ever recorded.

use crate::brain::{Brain, BrainError, Fact};
use crate::index::{Vec0Index, VecFilter, VectorIndex};
use crate::norm;
use jiff::Timestamp;
use rusqlite::{Connection, params_from_iter};
use serde::Serialize;
use std::collections::BTreeMap;

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
}

impl Channel {
    pub const ALL: &'static [Channel] = &[Channel::Bm25, Channel::Alias, Channel::Semantic];
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

        // Rank per channel, then fuse. `ranked` maps fact id -> (rrf score,
        // channels), in a BTreeMap so iteration order never varies by run.
        let mut fused: BTreeMap<i64, (f64, Vec<Channel>)> = BTreeMap::new();
        for channel in Channel::ALL.iter().filter(|c| q.channels.contains(c)) {
            let ids = match channel {
                Channel::Bm25 => bm25_channel(conn, &q.text, &filter)?,
                Channel::Alias => alias_channel(conn, &q.text, &filter)?,
                Channel::Semantic => self.semantic_channel(&q.text, q)?,
            };
            for (rank, id) in ids.into_iter().enumerate() {
                let e = fused.entry(id).or_insert((0.0, Vec::new()));
                e.0 += 1.0 / (RRF_K + (rank + 1) as f64);
                e.1.push(*channel);
            }
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
struct TemporalFilter {
    sql: String,
    at: Option<i64>,
    scope: Option<String>,
}

impl TemporalFilter {
    fn new(q: &RecallQuery) -> Self {
        // A retracted fact is excluded in every mode, including History.
        let mut sql = String::from(" AND f.retracted_at IS NULL");
        let mut at = None;
        match q.when {
            When::Now => sql.push_str(" AND f.valid_to IS NULL"),
            When::AsOf(t) => {
                sql.push_str(" AND f.valid_from <= ?a AND (f.valid_to IS NULL OR ?a < f.valid_to)");
                at = Some(t.as_microsecond());
            }
            When::History => {}
        }
        if q.scope.is_some() {
            sql.push_str(" AND f.scope = ?s");
        }
        Self {
            sql,
            at,
            scope: q.scope.clone(),
        }
    }

    /// Rewrites the named markers (`?a`, `?s`) into plain positional `?`, pushing
    /// each value onto `into` in the order SQLite will read them.
    ///
    /// Named markers exist only so the fragment above stays readable: `?a`
    /// appears twice in the as-of clause, and tracking that by hand against a
    /// positional list is exactly the kind of off-by-one that silently returns
    /// the wrong facts.
    fn bind(&self, into: &mut Vec<rusqlite::types::Value>) -> String {
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
fn alias_channel(
    conn: &Connection,
    text: &str,
    filter: &TemporalFilter,
) -> std::result::Result<Vec<i64>, BrainError> {
    let candidates = entity_candidates(text);
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

    let sql = format!(
        "SELECT f.id FROM fact f
         WHERE f.entity_id IN (
           SELECT id FROM entity WHERE key IN ({placeholders})
           UNION
           SELECT entity_id FROM entity_alias WHERE alias_key IN ({placeholders})
         ){filter_sql}
         ORDER BY f.id
         LIMIT ?"
    );

    let mut stmt = conn.prepare(&sql)?;
    let ids = stmt
        .query_map(params_from_iter(binds), |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

/// Normalized keys the query might be naming.
///
/// Both single words and adjacent pairs, because "Produto A" is two tokens but
/// one entity. Two is enough in practice and keeps this linear.
fn entity_candidates(text: &str) -> Vec<String> {
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

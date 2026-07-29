//! Entities that have something in common.
//!
//! The graph answers "what is this connected to". This answers a different
//! question -- "what else is like this" -- and the difference matters because
//! almost nothing in a real brain is explicitly connected to its siblings. Twenty
//! vouchers each recording `is_a voucher_sazonal` are, between them, a cohort that
//! nobody ever drew an edge for. Asking about one of them should be able to
//! surface the others.
//!
//! Two entities are **kin** when they hold the same `(predicate, object)` pair.
//! That is all. No edge is required, and the pair may be a plain string, which is
//! what lets this work on a brain whose relations were never linked at all -- the
//! case that motivated it.
//!
//! ## Why rarity is the whole mechanism
//!
//! Kinship without weighting is useless, because the pairs entities share most
//! are the ones that say least. From the brain this was built against:
//!
//! ```text
//! pair                   entities   verdict
//! redencoes 0.0                54   noise
//! is_a <a class>               33   the cohort
//! is_a <another class>         23   the cohort
//! valido true                  22   noise
//! ativo true                   19   noise
//! percent_off 25.0              7   worth surfacing
//! percent_off 50.0              5   worth surfacing
//! label <a tier name>           2   worth surfacing -- and it is the only thing
//!                                   joining a plan to a seat plan
//! ```
//!
//! Note that the second class (23) and `valido true` (22) are the same size.
//! No threshold can separate them, and looking for one is a trap. What separates
//! them is *ordering*: inverse document frequency, `ln(N / n)`, the same measure
//! BM25 already uses on the lexical channel, ranks the rare pair above the common
//! one and lets truncation do the rest. Nothing here is tuned against a case.
//!
//! The cap on how many kin are considered is a bound on work, not a judgement --
//! kin are taken rarest-first, so what falls off the end is always the least
//! informative.

use crate::brain::BrainError;
use crate::recall::TemporalFilter;
use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};
use std::collections::BTreeMap;

/// How many related entities may contribute facts.
///
/// Mirrors [`crate::graph::MAX_REACHED`] and exists for the same reason: to keep
/// the follow-up query a bounded `IN` list. Entities are taken rarest-first.
pub const MAX_KIN: usize = 64;

/// Fewest entities a pair must join for it to mean anything.
///
/// One entity holding a value is not a cohort, it is a fact.
const MIN_SHARED: i64 = 2;

/// An entity that shares something with an anchor, and what it shares.
#[derive(Debug, Clone)]
pub struct Kin {
    pub entity_id: i64,
    /// Inverse document frequency of the rarest pair joining it to an anchor.
    /// Higher is more distinctive.
    pub weight: f64,
    /// The pair that earned the highest weight, in words. This is the provenance
    /// of a lateral answer -- the reason the brain looked over here at all.
    pub via: String,
}

/// Entities sharing an open `(predicate, object)` pair with any anchor.
///
/// Anchors are excluded from their own result for the same reason the walk
/// excludes them: their facts are what the lexical and semantic channels already
/// retrieve, and re-reporting them here would hand a fact a second vote for
/// sitting where the question pointed.
pub(crate) fn related(
    conn: &Connection,
    anchors: &[i64],
    filter: &TemporalFilter,
) -> Result<Vec<Kin>, BrainError> {
    if anchors.is_empty() {
        return Ok(Vec::new());
    }
    let total: i64 = conn.query_row("SELECT count(*) FROM entity", [], |r| r.get(0))?;
    if total <= 0 {
        return Ok(Vec::new());
    }

    let anchor_ph = vec!["?"; anchors.len()].join(",");
    let mut binds: Vec<Value> = anchors.iter().map(|&id| Value::Integer(id)).collect();
    let anchor_filter = filter.bind(&mut binds);
    let shared_filter = filter.bind(&mut binds);
    binds.extend(anchors.iter().map(|&id| Value::Integer(id)));
    binds.push(Value::Integer(MIN_SHARED));

    // `IS` rather than `=` throughout the join: an object lives in exactly one of
    // `object_text` and `object_num`, so the other column is NULL on both sides,
    // and `NULL = NULL` would quietly match nothing at all.
    //
    // An entity-valued object carries its label in `object_text` too, which is
    // deliberate here: it means a cohort keeps working across the moment somebody
    // converts `is_a voucher_sazonal` from a string into a real edge. The kinship
    // does not blink.
    let sql = format!(
        "WITH anchor_pair(predicate, otext, onum) AS (
           SELECT DISTINCT f.predicate, f.object_text, f.object_num
             FROM fact f WHERE f.entity_id IN ({anchor_ph}){anchor_filter}
         ),
         shared(predicate, otext, onum, entity_id) AS (
           SELECT f.predicate, f.object_text, f.object_num, f.entity_id
             FROM fact f
             JOIN anchor_pair p
               ON f.predicate = p.predicate
              AND f.object_text IS p.otext
              AND f.object_num  IS p.onum
            WHERE 1 = 1{shared_filter}
         ),
         degree(predicate, otext, onum, n) AS (
           SELECT predicate, otext, onum, count(DISTINCT entity_id)
             FROM shared GROUP BY predicate, otext, onum
         )
         SELECT s.entity_id, d.n, s.predicate,
                COALESCE(s.otext, CAST(s.onum AS TEXT))
           FROM shared s
           JOIN degree d
             ON d.predicate = s.predicate
            AND d.otext IS s.otext
            AND d.onum  IS s.onum
          WHERE s.entity_id NOT IN ({anchor_ph})
            AND d.n >= ?
          ORDER BY s.entity_id"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
        ))
    })?;

    // Best pair per entity: an entity sharing both `ativo true` and
    // `percent_off 50` is worth reaching for the second, and averaging the two
    // would let the common pair dilute the distinctive one.
    let mut best: BTreeMap<i64, Kin> = BTreeMap::new();
    for row in rows {
        let (entity_id, n, predicate, object) = row?;
        let weight = idf(total, n);
        let entry = best.entry(entity_id).or_insert_with(|| Kin {
            entity_id,
            weight: f64::NEG_INFINITY,
            via: String::new(),
        });
        if weight > entry.weight {
            entry.weight = weight;
            entry.via = match object {
                Some(o) => format!("{predicate} {o}"),
                None => predicate,
            };
        }
    }

    let mut out: Vec<Kin> = best.into_values().collect();
    // Rarest first, then by id so equal weights never reorder between runs.
    out.sort_by(|a, b| {
        b.weight
            .total_cmp(&a.weight)
            .then(a.entity_id.cmp(&b.entity_id))
    });
    out.truncate(MAX_KIN);
    Ok(out)
}

/// Inverse document frequency of a pair `n` of `total` entities hold.
///
/// `ln(total / n)`, floored at zero: a pair every entity holds distinguishes
/// nothing, and a negative weight would rank it *below* having no pair at all,
/// which is not a thing the ordering should be able to express.
fn idf(total: i64, n: i64) -> f64 {
    if n <= 0 {
        return 0.0;
    }
    ((total as f64) / (n as f64)).ln().max(0.0)
}

#[cfg(test)]
mod tests {
    use super::idf;

    #[test]
    fn rarer_pairs_weigh_more() {
        // The ordering this channel lives or dies by, on the real numbers from
        // the brain in the module comment.
        assert!(idf(134, 5) > idf(134, 23));
        assert!(idf(134, 23) > idf(134, 54));
    }

    #[test]
    fn a_pair_everything_shares_is_worthless_but_never_negative() {
        assert_eq!(idf(134, 134), 0.0);
        assert_eq!(idf(10, 20), 0.0);
    }
}

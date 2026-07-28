//! Walking the graph.
//!
//! There is no separate graph structure to keep in step with the facts: a
//! relation *is* a fact whose object is an entity, so the edges are rows in
//! `fact` and inherit every guarantee the fact model already provides. Traversal
//! therefore reuses the very same [`TemporalFilter`] that retrieval uses, rather
//! than defining a parallel notion of "which edges count". A traversal able to
//! reach through an edge that retrieval would have hidden is a traversal that
//! answers with dead history.
//!
//! Two things keep a walk from turning into a scan of the whole brain:
//!
//! - **Depth.** Two hops answers "which country does this product's supplier sit
//!   in"; past that the neighbourhood grows far faster than its usefulness.
//! - **Hub blocking.** One entity everything points at -- a company, a repo, a
//!   `production` tag -- would otherwise connect every fact to every other. A hub
//!   still shows up in the result, it just is not expanded *through*.

use crate::brain::BrainError;
use crate::recall::TemporalFilter;
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params_from_iter};
use std::collections::{BTreeMap, BTreeSet};

/// How many hops a walk may take.
pub const MAX_DEPTH: u32 = 2;

/// Degree at which an entity is treated as a hub even in a small brain.
///
/// The cut is `max(HUB_FLOOR, p99(degree))`: the percentile adapts to a brain
/// whose ordinary entities are richly connected, and the floor stops a young
/// brain -- where p99 is 2 -- from blocking traversal it should be doing.
const HUB_FLOOR: i64 = 50;

/// Ceiling on how many reached entities contribute candidate facts.
///
/// Hub blocking already bounds the fan-out, but it bounds it at roughly the
/// square of the cut. This keeps the follow-up query a bounded `IN` list instead
/// of an unbounded one; entities are taken nearest-first, so what gets dropped is
/// always the furthest and least relevant.
const MAX_REACHED: usize = 200;

/// What a walk found.
#[derive(Debug, Default)]
pub struct Neighbourhood {
    /// Every entity reached, at the *fewest* hops it was reached in. Anchors are
    /// hop 0.
    pub hops: BTreeMap<i64, u32>,
    /// How each reached entity was first arrived at. This is the provenance of a
    /// graph answer -- the reason the brain looked over here -- and following it
    /// backwards reconstructs the whole path.
    pub arrival: BTreeMap<i64, Arrival>,
    /// Every edge fact the walk crossed. An edge in here was used as a road,
    /// which is what makes it demotable as an answer.
    pub bridges: BTreeSet<i64>,
    /// The degree at which expansion stopped, kept for diagnostics.
    pub hub_cut: i64,
}

/// The step that first reached an entity.
#[derive(Debug, Clone, Copy)]
pub struct Arrival {
    /// The edge fact crossed.
    pub edge: i64,
    /// The entity it was crossed from.
    pub from: i64,
}

impl Neighbourhood {
    /// The edges lying on shortest paths from an anchor to any of `targets`.
    ///
    /// This is what "was this edge actually used" means, precisely: not "does it
    /// touch something interesting", but "is it a step on the route to a fact we
    /// are about to report". A road that led nowhere we are answering with is not
    /// a road, and must not be demoted for the shape of its neighbourhood.
    pub fn roads_to(&self, targets: impl IntoIterator<Item = i64>) -> BTreeSet<i64> {
        let mut roads = BTreeSet::new();
        for target in targets {
            let mut at = target;
            // Terminates on its own: `arrival` is a shortest-path tree, so
            // following it strictly decreases the hop count. The visited guard is
            // for sharing between targets, not for cycles.
            while let Some(step) = self.arrival.get(&at) {
                if !roads.insert(step.edge) {
                    break;
                }
                at = step.from;
            }
        }
        roads
    }

    /// Entities at least one hop out, nearest first, capped at [`MAX_REACHED`].
    ///
    /// Anchors are excluded on purpose: their own facts are what the lexical and
    /// semantic channels already retrieve, and letting the graph channel re-report
    /// them would hand a fact a second vote for being where the walk started.
    pub fn reached(&self) -> Vec<(i64, u32)> {
        let mut out: Vec<(i64, u32)> = self
            .hops
            .iter()
            .filter(|&(_, &hop)| hop > 0)
            .map(|(&id, &hop)| (id, hop))
            .collect();
        out.sort_by_key(|&(id, hop)| (hop, id));
        out.truncate(MAX_REACHED);
        out
    }
}

/// Walks outward from `anchors`, up to `depth` hops.
pub(crate) fn expand(
    conn: &Connection,
    anchors: &[i64],
    filter: &TemporalFilter,
    depth: u32,
) -> Result<Neighbourhood, BrainError> {
    if anchors.is_empty() {
        return Ok(Neighbourhood::default());
    }
    let hub_cut = hub_cut(conn, filter)?;

    let mut binds: Vec<Value> = Vec::new();
    // The edge predicate appears twice -- once per direction -- so the filter is
    // bound twice, in the order SQLite will read the two arms.
    let forward = filter.bind(&mut binds);
    let backward = filter.bind(&mut binds);
    binds.extend(anchors.iter().map(|&id| Value::Integer(id)));
    binds.push(Value::Integer(depth as i64));
    binds.push(Value::Integer(hub_cut));
    let anchor_ph = vec!["?"; anchors.len()].join(",");

    // Edges are walked in both directions. "Which country does produto_a come
    // from" follows produto_a -> acme; "how much does what acme supplies cost"
    // follows the same edge backwards. A relation is a road, not a one-way street.
    //
    // `UNION`, never `UNION ALL`: the graph has cycles, and `UNION` deduplicates
    // the frontier so a cycle terminates instead of looping. The depth bound is a
    // second, independent stop -- one guard for a property this important is not
    // enough.
    //
    // The hub test is a `LEFT JOIN` rather than a correlated subquery because
    // SQLite forbids a subquery that references the recursive table.
    let sql = format!(
        "WITH RECURSIVE
           adj(edge_id, a, b) AS (
             SELECT f.id, f.entity_id, f.object_entity_id
               FROM fact f WHERE f.object_entity_id IS NOT NULL{forward}
             UNION ALL
             SELECT f.id, f.object_entity_id, f.entity_id
               FROM fact f WHERE f.object_entity_id IS NOT NULL{backward}
           ),
           deg(a, d) AS (SELECT a, count(*) FROM adj GROUP BY a),
           walk(entity_id, hop, edge_id, from_id) AS (
             SELECT e.id, 0, NULL, NULL FROM entity e WHERE e.id IN ({anchor_ph})
             UNION
             SELECT adj.b, walk.hop + 1, adj.edge_id, walk.entity_id
               FROM walk
               JOIN adj ON adj.a = walk.entity_id
               LEFT JOIN deg ON deg.a = walk.entity_id
              WHERE walk.hop < ? AND COALESCE(deg.d, 0) < ?
           )
         SELECT entity_id, hop, edge_id, from_id FROM walk
          ORDER BY hop, entity_id, edge_id"
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds), |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)? as u32,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
        ))
    })?;

    let mut n = Neighbourhood {
        hub_cut,
        ..Default::default()
    };
    for row in rows {
        let (entity_id, hop, edge_id, from_id) = row?;
        let known = n.hops.entry(entity_id).or_insert(hop);
        if hop < *known {
            *known = hop;
        }
        if let (Some(edge), Some(from)) = (edge_id, from_id) {
            n.bridges.insert(edge);
            // Rows arrive ordered by hop, so the first arrival recorded for an
            // entity is on a shortest path; longer routes never overwrite it.
            n.arrival.entry(entity_id).or_insert(Arrival { edge, from });
        }
    }
    Ok(n)
}

/// The degree above which an entity stops being expanded through.
///
/// `count(*) - 1` in the offset makes this the value at the 99th percentile
/// rather than the maximum: with 100 entities, offset 98 is the 99th of them,
/// while `count(*) * 99 / 100` would be the 100th and no entity would ever be a
/// hub.
fn hub_cut(conn: &Connection, filter: &TemporalFilter) -> Result<i64, BrainError> {
    let mut binds: Vec<Value> = Vec::new();
    let forward = filter.bind(&mut binds);
    let backward = filter.bind(&mut binds);

    let sql = format!(
        "WITH adj(a) AS (
           SELECT f.entity_id FROM fact f WHERE f.object_entity_id IS NOT NULL{forward}
           UNION ALL
           SELECT f.object_entity_id FROM fact f WHERE f.object_entity_id IS NOT NULL{backward}
         ),
         deg(d) AS (SELECT count(*) FROM adj GROUP BY a)
         SELECT d FROM deg ORDER BY d
         LIMIT 1 OFFSET (SELECT (count(*) - 1) * 99 / 100 FROM deg)"
    );

    let p99: Option<i64> = conn
        .query_row(&sql, params_from_iter(binds), |r| r.get(0))
        .optional()?;
    Ok(p99.unwrap_or(0).max(HUB_FLOOR))
}

/// A fact belonging to a reached entity.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub fact_id: i64,
    pub entity_id: i64,
    pub predicate: String,
}

/// Every fact of the given entities that the temporal filter admits.
pub(crate) fn facts_of(
    conn: &Connection,
    entities: &[i64],
    filter: &TemporalFilter,
) -> Result<Vec<Candidate>, BrainError> {
    if entities.is_empty() {
        return Ok(Vec::new());
    }
    let mut binds: Vec<Value> = entities.iter().map(|&id| Value::Integer(id)).collect();
    let where_sql = filter.bind(&mut binds);
    let ph = vec!["?"; entities.len()].join(",");

    let sql = format!(
        "SELECT f.id, f.entity_id, f.predicate FROM fact f
          WHERE f.entity_id IN ({ph}){where_sql}
          ORDER BY f.id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params_from_iter(binds), |r| {
        Ok(Candidate {
            fact_id: r.get(0)?,
            entity_id: r.get(1)?,
            predicate: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The predicate of each of the given facts.
pub fn predicates(
    conn: &Connection,
    fact_ids: impl IntoIterator<Item = i64>,
) -> Result<BTreeMap<i64, String>, BrainError> {
    let ids: Vec<Value> = fact_ids.into_iter().map(Value::Integer).collect();
    if ids.is_empty() {
        return Ok(BTreeMap::new());
    }
    let ph = vec!["?"; ids.len()].join(",");
    let mut stmt = conn.prepare(&format!(
        "SELECT id, predicate FROM fact WHERE id IN ({ph})"
    ))?;
    let rows = stmt.query_map(params_from_iter(ids), |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<BTreeMap<i64, String>>>()?)
}

/// Entity ids the text names outright, by key or by any alias.
///
/// Learned aliases count here, unlike in [`crate::brain::find_entity`]. Anchoring
/// is a retrieval decision -- a wrong anchor costs one extra branch of a walk
/// that the ranking then discards -- so it can use a guess where identity cannot.
/// It is also where a learned name pays for itself: an entity the question names
/// only by a nickname is an entity the walk can start from.
pub fn named_entities(conn: &Connection, keys: &[String]) -> Result<Vec<i64>, BrainError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let ph = vec!["?"; keys.len()].join(",");
    let mut binds: Vec<Value> = Vec::new();
    binds.extend(keys.iter().cloned().map(Value::Text));
    binds.extend(keys.iter().cloned().map(Value::Text));

    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM entity WHERE key IN ({ph})
         UNION
         SELECT entity_id FROM entity_alias WHERE alias_key IN ({ph})"
    ))?;
    let rows = stmt.query_map(params_from_iter(binds), |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<i64>>>()?)
}

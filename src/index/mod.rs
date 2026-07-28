//! Nearest-neighbour search over stored embeddings.
//!
//! Two implementations sit behind one trait, and a conformance test asserts they
//! agree. `sqlite-vec` is the fast path; the brute-force scan is the fallback,
//! and it exists because a vector index that only works on some targets would
//! make recall silently target-dependent.
//!
//! Neither is the source of truth. Embeddings live in the ordinary
//! `fact_embedding` table, and `vec0` is a derived index rebuilt by
//! `brain reindex`. If `sqlite-vec`'s on-disk format ever changes -- it is a
//! 0.1.x crate with no stability guarantee -- the answer is to drop and rebuild,
//! not to write a migration.

use crate::brain::BrainError;
use crate::embed;
use rusqlite::{Connection, params, params_from_iter};

type Result<T> = std::result::Result<T, BrainError>;

/// Restricts which facts may be returned. Mirrors the temporal filter the other
/// channels apply, expressed in the terms `vec0` can actually index on.
#[derive(Debug, Clone, Default)]
pub struct VecFilter {
    /// Only facts that currently hold.
    pub open_only: bool,
    pub scope: Option<String>,
}

impl VecFilter {
    pub fn open() -> Self {
        Self {
            open_only: true,
            ..Default::default()
        }
    }
}

pub trait VectorIndex {
    /// Returns `(fact_id, distance)`, nearest first.
    fn search(&self, query: &[f32], k: usize, filter: &VecFilter) -> Result<Vec<(i64, f32)>>;
}

/// The fast path: `sqlite-vec`'s `vec0` virtual table.
pub struct Vec0Index<'a> {
    conn: &'a Connection,
    dim: usize,
}

impl<'a> Vec0Index<'a> {
    pub fn new(conn: &'a Connection, dim: usize) -> Self {
        Self { conn, dim }
    }
}

impl VectorIndex for Vec0Index<'_> {
    fn search(&self, query: &[f32], k: usize, filter: &VecFilter) -> Result<Vec<(i64, f32)>> {
        debug_assert_eq!(query.len(), self.dim);

        // `vec0` metadata filters support only equality and ordering -- no
        // `IS NULL` -- which is why "currently holds" is stored as the boolean
        // `is_open` rather than as a nullable valid_to.
        let mut conds = String::new();
        let mut binds: Vec<rusqlite::types::Value> = vec![embed::to_bytes(query).into()];
        if filter.open_only {
            conds.push_str(" AND is_open = 1");
        }
        if filter.scope.is_some() {
            conds.push_str(" AND scope = ?");
        }
        binds.push((k as i64).into());
        if let Some(s) = &filter.scope {
            binds.push(s.clone().into());
        }

        // A bare blob is ambiguous to vec0 -- four bytes read as float32[1] --
        // so the element type must be declared on both sides of the MATCH.
        //
        // vec0 rejects anything but a bare `ORDER BY distance`, so the fact_id
        // tie-break happens in Rust below rather than in SQL.
        let sql = format!(
            "SELECT fact_id, distance FROM fact_vec
             WHERE embedding MATCH vec_int8(?) AND k = ?{conds}
             ORDER BY distance"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut rows = stmt
            .query_map(params_from_iter(binds), |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<(i64, f32)>>>()?;
        rows.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        Ok(rows)
    }
}

/// The fallback: read every embedding and score it in Rust.
///
/// Honest about its limits -- it is linear in the corpus -- but at 256
/// dimensions and int8 storage a 100k-fact brain is about 26 MB and a few
/// milliseconds per query, which is well past where this tool is aimed.
pub struct BruteForceIndex<'a> {
    conn: &'a Connection,
    dim: usize,
}

impl<'a> BruteForceIndex<'a> {
    pub fn new(conn: &'a Connection, dim: usize) -> Self {
        Self { conn, dim }
    }
}

impl VectorIndex for BruteForceIndex<'_> {
    fn search(&self, query: &[f32], k: usize, filter: &VecFilter) -> Result<Vec<(i64, f32)>> {
        debug_assert_eq!(query.len(), self.dim);

        let mut sql = String::from(
            "SELECT e.fact_id, e.embedding FROM fact_embedding e
             JOIN fact f ON f.id = e.fact_id
             WHERE 1=1",
        );
        let mut binds: Vec<rusqlite::types::Value> = Vec::new();
        if filter.open_only {
            sql.push_str(" AND f.valid_to IS NULL AND f.retracted_at IS NULL");
        }
        if let Some(s) = &filter.scope {
            sql.push_str(" AND f.scope = ?");
            binds.push(s.clone().into());
        }

        let q = embed::to_int8(query);
        let mut stmt = self.conn.prepare(&sql)?;
        let mut scored: Vec<(i64, f32)> = stmt
            .query_map(params_from_iter(binds), |r| {
                let id: i64 = r.get(0)?;
                let raw: Vec<u8> = r.get(1)?;
                Ok((id, raw))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(id, raw)| (id, l2_distance(&q, bytemuck::cast_slice::<u8, i8>(&raw))))
            .collect();

        // Ascending distance, then ascending id: the same tie-break every other
        // ranked path in this crate uses, so results never depend on row order.
        scored.sort_by(|a, b| a.1.total_cmp(&b.1).then(a.0.cmp(&b.0)));
        scored.truncate(k);
        Ok(scored)
    }
}

/// Euclidean distance over the int8 representation, matching what `vec0`
/// computes for an `int8[N]` column.
///
/// Both sides quantize first so the two implementations see identical inputs;
/// scoring the f32 query against int8 rows would make them disagree by exactly
/// the quantization error, and the conformance test would be measuring rounding
/// rather than agreement.
fn l2_distance(a: &[i8], b: &[i8]) -> f32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| {
            let d = f32::from(*x) - f32::from(*y);
            d * d
        })
        .sum::<f32>()
        .sqrt()
}

/// Writes an embedding to both the source-of-truth table and the derived index.
pub fn store(
    conn: &Connection,
    fact_id: i64,
    vector: &[f32],
    is_open: bool,
    scope: Option<&str>,
    model_id: &str,
) -> Result<()> {
    let bytes = embed::to_bytes(vector);
    conn.execute(
        "INSERT INTO fact_embedding(fact_id, model_id, embedding) VALUES (?,?,?)
         ON CONFLICT(fact_id) DO UPDATE SET model_id = excluded.model_id,
                                            embedding = excluded.embedding",
        params![fact_id, model_id, bytes],
    )?;
    index_row(conn, fact_id, &bytes, is_open, scope)
}

fn index_row(
    conn: &Connection,
    fact_id: i64,
    bytes: &[u8],
    is_open: bool,
    scope: Option<&str>,
) -> Result<()> {
    conn.execute("DELETE FROM fact_vec WHERE fact_id = ?", params![fact_id])?;
    conn.execute(
        "INSERT INTO fact_vec(fact_id, embedding, is_open, scope)
         VALUES (?, vec_int8(?), ?, ?)",
        params![fact_id, bytes, is_open as i64, scope.unwrap_or("")],
    )?;
    Ok(())
}

/// Updates only the index's view of whether a fact still holds.
///
/// Called when a fact is closed or retracted. The embedding itself never
/// changes -- the text did not -- so re-embedding would be waste.
pub fn mark_closed(conn: &Connection, fact_id: i64) -> Result<()> {
    conn.execute(
        "UPDATE fact_vec SET is_open = 0 WHERE fact_id = ?",
        params![fact_id],
    )?;
    Ok(())
}

/// Rebuilds the whole `vec0` index from the stored embeddings. Returns how many
/// rows were written.
pub fn rebuild(conn: &Connection) -> Result<usize> {
    conn.execute("DELETE FROM fact_vec", [])?;
    let rows: Vec<(i64, Vec<u8>, bool, Option<String>)> = conn
        .prepare(
            "SELECT e.fact_id, e.embedding,
                    (f.valid_to IS NULL AND f.retracted_at IS NULL), f.scope
             FROM fact_embedding e JOIN fact f ON f.id = e.fact_id
             ORDER BY e.fact_id",
        )?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
        .collect::<rusqlite::Result<_>>()?;

    let n = rows.len();
    for (id, bytes, is_open, scope) in rows {
        index_row(conn, id, &bytes, is_open, scope.as_deref())?;
    }
    Ok(n)
}

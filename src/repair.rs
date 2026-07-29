//! Fixing how a fact is stored, without touching what it says.
//!
//! This module is the one place in the brain that rewrites a row that is already
//! true, so it is worth being precise about why that is not a contradiction of
//! everything else here.
//!
//! Nothing is deleted and no claim changes. `voucher_a is_a voucher_sazonal`
//! means exactly what it meant before; the repair gives the object an id
//! alongside the label it already carried, so that a walk can follow it. The
//! `statement` and `search_text` columns come out byte-identical, which is not a
//! happy accident -- it is the test of whether a change belongs here. It also
//! means the FTS triggers do not fire and the stored embeddings stay valid, so
//! there is no reindex to forget.
//!
//! A repair that had to alter a statement would be a correction, and corrections
//! go through `retract` where they are visible. That distinction is the whole
//! reason this is a separate command rather than something `lint --fix` does on
//! its own.
//!
//! Dry by default. `--apply` is the only way to write, and the report it prints
//! first is the same list it would act on.

use crate::brain::BrainError;
use crate::norm;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

/// One string object that a relational predicate should have pointed through.
#[derive(Debug, Clone, Serialize)]
pub struct Promotion {
    pub fact_id: i64,
    pub predicate: String,
    pub subject: String,
    /// The string as stored, which is also the label the entity will take.
    pub object: String,
    /// The identity key it resolves to.
    pub key: String,
    /// Whether that entity has to be created, or already exists under this key.
    pub creates_entity: bool,
}

/// What a repair did, or would do.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub applied: bool,
    pub promotions: Vec<Promotion>,
    /// Distinct entities the repair creates. The interesting number: these are
    /// the classes that existed only as text.
    pub entities_created: usize,
}

impl Report {
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.promotions.is_empty() {
            out.push("nothing to repair.".into());
            return out;
        }

        // `creates_entity` is true on every promotion naming a class that does
        // not exist yet, so twenty vouchers naming one class all carry it. Marking
        // each line would read as twenty new entities in the dry run of a command
        // that writes -- exactly the wrong place to be loose with a number.
        let mut announced = std::collections::BTreeSet::new();
        for p in self.promotions.iter().take(20) {
            let first = p.creates_entity && announced.insert(p.key.as_str());
            out.push(format!(
                "  {} {} {}{}",
                p.subject,
                p.predicate,
                p.object,
                if first {
                    "   (creates this entity)"
                } else {
                    ""
                }
            ));
        }
        if self.promotions.len() > 20 {
            out.push(format!("  ... and {} more", self.promotions.len() - 20));
        }

        out.push(if self.applied {
            format!(
                "repaired {} fact(s), creating {} entit{}.",
                self.promotions.len(),
                self.entities_created,
                if self.entities_created == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        } else {
            format!(
                "would repair {} fact(s), creating {} entit{}. Re-run with --apply.",
                self.promotions.len(),
                self.entities_created,
                if self.entities_created == 1 {
                    "y"
                } else {
                    "ies"
                }
            )
        });
        out
    }
}

/// Finds string objects under relational predicates, and optionally links them.
///
/// Only predicates already marked relational are touched. That is deliberate:
/// deciding *which* predicates are relations is `lint`'s job to surface and a
/// person's to confirm, and a repair that made that judgement for itself would be
/// inventing entities out of prose.
pub fn relations(conn: &Connection, apply: bool) -> Result<Report, BrainError> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.predicate, e.label, f.object_text
           FROM fact f
           JOIN entity e ON e.id = f.entity_id
           JOIN predicate p ON p.key = f.predicate
          WHERE p.relational = 1
            AND f.object_entity_id IS NULL
            AND f.object_text IS NOT NULL
            AND f.retracted_at IS NULL
          ORDER BY f.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;

    let mut promotions = Vec::new();
    for row in rows {
        let (fact_id, predicate, subject, object) = row?;
        let key = norm::key(&object);
        // A string that normalizes to nothing names nothing. Promoting it would
        // create an entity with an empty key, which the write path rejects for
        // good reason.
        if key.is_empty() {
            continue;
        }
        let exists = find_entity_id(conn, &key)?.is_some();
        promotions.push(Promotion {
            fact_id,
            predicate,
            subject,
            object,
            key,
            creates_entity: !exists,
        });
    }

    // Counted over distinct keys: fifty vouchers naming one class create one
    // entity, and reporting fifty would badly misdescribe the change.
    let mut new_keys: Vec<&str> = promotions
        .iter()
        .filter(|p| p.creates_entity)
        .map(|p| p.key.as_str())
        .collect();
    new_keys.sort_unstable();
    new_keys.dedup();
    let entities_created = new_keys.len();

    if apply && !promotions.is_empty() {
        let tx = conn.unchecked_transaction()?;
        let now = jiff::Timestamp::now().as_microsecond();
        for p in &promotions {
            let id = match find_entity_id(&tx, &p.key)? {
                Some(id) => id,
                None => {
                    tx.execute(
                        "INSERT INTO entity(key, label, created_at) VALUES (?, ?, ?)",
                        params![p.key, p.object, now],
                    )?;
                    tx.last_insert_rowid()
                }
            };
            // `object_text` is left exactly as it was, which is what keeps
            // `statement` and `search_text` true and the FTS triggers quiet.
            tx.execute(
                "UPDATE fact SET object_entity_id = ? WHERE id = ?",
                params![id, p.fact_id],
            )?;
        }
        tx.commit()?;
    }

    Ok(Report {
        applied: apply && !promotions.is_empty(),
        promotions,
        entities_created,
    })
}

/// The entity a key belongs to, declared aliases included.
///
/// Mirrors the write path's lookup rather than querying `entity` alone: if a
/// class already answers to this name by declaration, the repair must land on
/// that entity instead of growing a second one beside it.
fn find_entity_id(conn: &Connection, key: &str) -> Result<Option<i64>, BrainError> {
    Ok(conn
        .query_row(
            "SELECT id FROM entity WHERE key = ?
             UNION ALL
             SELECT entity_id FROM entity_alias
               WHERE alias_key = ? AND source = 'declared'
             LIMIT 1",
            params![key, key],
            |r| r.get(0),
        )
        .optional()?)
}

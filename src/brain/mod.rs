//! The bitemporal core.
//!
//! Writing a new value never overwrites the old one. It **closes** it: the
//! previous fact keeps its content and its `recorded_at`, and only gains a
//! `valid_to` plus a pointer to its replacement. That is what lets one brain
//! answer both "what is the price?" and "what was the price in March?".
//!
//! Three write outcomes are deliberately distinguished, because they mean
//! different things to an agent:
//!
//! - **superseded** -- it changed. The old value was true, and then stopped being.
//! - **corrected** -- we were wrong. The old value was never true, so it is retracted.
//! - **reasserted** -- we were told the same thing again. Reinforce, do not duplicate.

mod types;

use crate::clock::Clock;
use crate::embed::{Embedder, StaticEmbedder};
use crate::ids::IdGen;
use crate::index;
use crate::norm;
use crate::recall::{TemporalFilter, When};
use crate::store::{Store, StoreError};
use jiff::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

pub use types::{Assertion, Cardinality, Fact, Object, Outcome, Provenance};

/// Everything the brain holds about one entity.
#[derive(Debug, Clone, Serialize)]
pub struct EntityView {
    pub key: String,
    pub label: String,
    /// The other names it answers to. Visible because a learned one is a guess,
    /// and a guess nobody can see is a guess nobody can correct.
    pub aliases: Vec<crate::alias::Alias>,
    pub facts: Vec<Fact>,
    /// Nearest first. Empty unless a depth was asked for.
    pub neighbours: Vec<Neighbour>,
}

/// An entity reached by walking relations, and the relation that got there.
#[derive(Debug, Clone, Serialize)]
pub struct Neighbour {
    pub key: String,
    pub entity: String,
    pub hops: u32,
    /// The edge crossed on the shortest route here, in words.
    pub via: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BrainError {
    #[error("confidence must be between 0.0 and 1.0, got {0}")]
    InvalidConfidence(f64),

    #[error("{what} cannot be empty or punctuation-only (got {given:?})")]
    EmptyKey { what: &'static str, given: String },

    #[error("no fact with id {0}")]
    NoSuchFact(i64),

    #[error("no entity named {name:?}")]
    NoSuchEntity { name: String },

    #[error("{alias:?} already names {owner:?}")]
    AliasTaken { alias: String, owner: String },

    #[error("cannot make predicate {key:?} single-valued: {open} facts are open at once")]
    CardinalityConflict { key: String, open: i64 },

    #[error(transparent)]
    Store(#[from] StoreError),

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error("embedding failed: {0}")]
    Embedding(String),

    #[error("malformed locator JSON stored for fact {id}: {source}")]
    BadLocator {
        id: i64,
        #[source]
        source: serde_json::Error,
    },
}

type Result<T> = std::result::Result<T, BrainError>;

/// Everything one write needs, resolved once and passed around as a unit.
///
/// Bundled rather than passed as eight parameters so the placement logic below
/// reads as a sequence of decisions instead of parameter plumbing.
struct Write<'a> {
    a: &'a Assertion,
    entity_id: i64,
    predicate_key: &'a str,
    object: &'a ResolvedObject,
    /// When the claim becomes true in the world.
    valid_from: Timestamp,
    /// When the brain is learning it.
    now: Timestamp,
}

pub struct Brain {
    store: Store,
    clock: Box<dyn Clock>,
    ids: Box<dyn IdGen>,
    /// Built once per process. Loading the table is the expensive part; encoding
    /// afterwards is a lookup, so writes never need to defer embedding.
    embedder: Box<dyn Embedder>,
}

impl Brain {
    pub fn init(
        path: &std::path::Path,
        label: &str,
        clock: Box<dyn Clock>,
        ids: Box<dyn IdGen>,
    ) -> Result<Self> {
        let store = Store::init(path, label, clock.as_ref(), ids.as_ref())?;
        Ok(Self {
            store,
            clock,
            ids,
            embedder: Box::new(StaticEmbedder::bundled()?),
        })
    }

    pub fn open(
        path: &std::path::Path,
        clock: Box<dyn Clock>,
        ids: Box<dyn IdGen>,
    ) -> Result<Self> {
        Ok(Self {
            store: Store::open(path)?,
            clock,
            ids,
            embedder: Box::new(StaticEmbedder::bundled()?),
        })
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn embedder(&self) -> &dyn Embedder {
        self.embedder.as_ref()
    }

    fn conn(&self) -> &Connection {
        self.store.conn()
    }

    // --- writing -------------------------------------------------------------

    /// Records a claim, deciding for itself whether that supersedes, corrects or
    /// merely reinforces what is already known.
    pub fn remember(&self, a: &Assertion) -> Result<Outcome> {
        if let Some(c) = a.confidence
            && !(0.0..=1.0).contains(&c)
        {
            return Err(BrainError::InvalidConfidence(c));
        }
        let subject_key = require_key("subject", &a.subject)?;
        let predicate_key = require_key("predicate", &a.predicate)?;

        // One transaction for the whole decision: a rejected or failed write must
        // leave no trace, including no half-closed predecessor.
        let tx = self.conn().unchecked_transaction()?;
        let now = self.clock.now();
        let valid_from = a.valid_from.unwrap_or(now);

        let entity_id = upsert_entity(&tx, &subject_key, &a.subject, now)?;
        let shape = upsert_predicate(&tx, &predicate_key, a.cardinality)?;
        let object = resolve_object(&tx, &a.object, shape.relational, now)?;
        let cardinality = shape.cardinality;

        let w = Write {
            a,
            entity_id,
            predicate_key: &predicate_key,
            object: &object,
            valid_from,
            now,
        };

        let outcome = match cardinality {
            Cardinality::Multi => {
                let id = self.insert_fact(&tx, &w, None, false)?;
                Outcome::Created(load_fact(&tx, id)?)
            }
            Cardinality::Single => self.place_single(&tx, &w)?,
        };

        tx.commit()?;
        Ok(outcome)
    }

    /// Slots a single-valued fact into an existing timeline.
    ///
    /// This is the delicate part. The new fact may land after everything (the
    /// ordinary case), on top of an existing claim (a correction), or in the
    /// middle or before everything (a backdated write, learned late). Each is
    /// handled by looking at the neighbours rather than by assuming the new fact
    /// is the latest.
    fn place_single(&self, tx: &Connection, w: &Write<'_>) -> Result<Outcome> {
        let (valid_from, now) = (w.valid_from, w.now);
        let live = live_facts(tx, w.entity_id, w.predicate_key)?;

        // Being told the same thing again reinforces it. Creating a second fact
        // would clutter the history and make `reassert_count` meaningless.
        if let Some(covering) = live.iter().find(|f| f.covers(valid_from))
            && w.object.matches(covering)
        {
            tx.execute(
                "UPDATE fact
                 SET reassert_count = reassert_count + 1,
                     confidence = MIN(1.0, confidence + (1.0 - confidence) * 0.5)
                 WHERE id = ?",
                params![covering.id],
            )?;
            return Ok(Outcome::Reasserted(load_fact(tx, covering.id)?));
        }

        // A different value claiming the exact same instant is a correction, not
        // a change over time -- closing one against the other would leave an
        // empty [t, t) interval.
        let same_instant = live
            .iter()
            .find(|f| f.valid_from == valid_from)
            .map(|f| f.id);
        if let Some(id) = same_instant {
            retract_fact(tx, id, now, Some("corrected by a later assertion"))?;
        }

        let predecessor = live
            .iter()
            .filter(|f| f.valid_from < valid_from && Some(f.id) != same_instant)
            .max_by_key(|f| f.valid_from)
            .cloned();
        let successor = live
            .iter()
            .filter(|f| f.valid_from > valid_from)
            .min_by_key(|f| f.valid_from)
            .cloned();

        // Close the predecessor *before* inserting: the partial unique index
        // allows only one open fact, so two would collide mid-transaction.
        if let Some(p) = &predecessor
            && p.valid_to.is_none_or(|to| to > valid_from)
        {
            tx.execute(
                "UPDATE fact SET valid_to = ? WHERE id = ?",
                params![micros(valid_from), p.id],
            )?;
            // The text did not change, so the embedding stands; only the index's
            // view of "still holds" has to follow.
            index::mark_closed(tx, p.id)?;
        }

        let new_id = self.insert_fact(tx, w, successor.as_ref().map(|s| s.valid_from), true)?;

        // Only link the predecessor if it actually meets the new fact. A
        // predecessor that closed earlier sits before a genuine gap -- extending
        // it across that gap would invent history.
        if let Some(p) = &predecessor
            && p.valid_to.is_none_or(|to| to >= valid_from)
        {
            tx.execute(
                "UPDATE fact SET superseded_by = ? WHERE id = ?",
                params![new_id, p.id],
            )?;
        }

        let created = load_fact(tx, new_id)?;
        Ok(match (same_instant, predecessor) {
            (Some(id), _) => Outcome::Corrected {
                retracted: load_fact(tx, id)?,
                created,
            },
            (None, Some(p)) if p.valid_to.is_none_or(|to| to >= valid_from) => {
                Outcome::Superseded {
                    closed: load_fact(tx, p.id)?,
                    created,
                }
            }
            _ => Outcome::Created(created),
        })
    }

    fn insert_fact(
        &self,
        tx: &Connection,
        w: &Write<'_>,
        valid_to: Option<Timestamp>,
        is_single: bool,
    ) -> Result<i64> {
        let (a, object) = (w.a, w.object);
        let entity_label = entity_label(tx, w.entity_id)?;
        let display = object.display();
        let statement = format!("{entity_label} {} {display}", a.predicate);
        let search_text = format!(
            "{entity_label} {} {} {display}",
            norm::key(&a.subject),
            a.predicate
        );

        tx.execute(
            "INSERT INTO fact(
               uuid, entity_id, predicate, is_single,
               object_text, object_num, object_entity_id, unit,
               statement, search_text,
               valid_from, valid_to, recorded_at, confidence, scope, source, locator)
             VALUES (?,?,?,?, ?,?,?,?, ?,?, ?,?,?, ?,?,?,?)",
            params![
                self.ids.next_id().to_string(),
                w.entity_id,
                w.predicate_key,
                is_single as i64,
                object.text,
                object.num,
                object.entity_id,
                object.unit,
                statement,
                search_text,
                micros(w.valid_from),
                valid_to.map(micros),
                micros(w.now),
                a.confidence.unwrap_or(1.0),
                a.scope,
                a.source,
                a.locator.as_ref().map(|v| v.to_string()),
            ],
        )?;
        let id = tx.last_insert_rowid();

        // Embed inside the same transaction as the fact. A vector written
        // separately could be lost on a crash, leaving a fact the semantic
        // channel can never find -- invisible, and undetectable without a scan.
        let vector = self.embedder.embed_one(&statement)?;
        index::store(
            tx,
            id,
            &vector,
            valid_to.is_none(),
            w.a.scope.as_deref(),
            self.embedder.model_id(),
        )?;
        Ok(id)
    }

    /// Rebuilds the derived vector index from the stored embeddings.
    pub fn reindex(&self) -> Result<usize> {
        let tx = self.conn().unchecked_transaction()?;
        let n = index::rebuild(&tx)?;
        tx.commit()?;
        Ok(n)
    }

    /// Records a relation. Relations are facts, so this is `remember` with an
    /// entity-valued object -- and therefore gets bitemporality for free.
    pub fn link(&self, from: &str, rel: &str, to: &str, at: Option<Timestamp>) -> Result<Outcome> {
        let mut a = Assertion::new(from, rel, Object::entity(to));
        a.valid_from = at;
        self.remember(&a)
    }

    /// Marks a fact as never having been true.
    ///
    /// Deliberately not the inverse of supersession: it does not reopen whatever
    /// this fact closed. "This was wrong" leaves the earlier period genuinely
    /// unknown, and inventing an answer for it would be worse than admitting the
    /// gap.
    pub fn retract(&self, id: i64, reason: Option<&str>) -> Result<Fact> {
        let tx = self.conn().unchecked_transaction()?;
        if load_fact(&tx, id).is_err() {
            return Err(BrainError::NoSuchFact(id));
        }
        retract_fact(&tx, id, self.clock.now(), reason)?;
        let f = load_fact(&tx, id)?;
        tx.commit()?;
        Ok(f)
    }

    /// Fixes a predicate's cardinality, overriding whatever was inferred.
    pub fn set_cardinality(&self, predicate: &str, c: Cardinality) -> Result<()> {
        let key = require_key("predicate", predicate)?;
        let tx = self.conn().unchecked_transaction()?;

        // Tightening to single-valued is only legal if the existing facts already
        // satisfy it; the partial unique index would reject the change anyway,
        // but with a message about an index rather than about the data.
        if c == Cardinality::Single {
            let open: i64 = tx.query_row(
                "SELECT count(*) FROM fact
                 WHERE predicate = ? AND valid_to IS NULL AND retracted_at IS NULL",
                params![key],
                |r| r.get(0),
            )?;
            if open > 1 {
                return Err(BrainError::CardinalityConflict { key, open });
            }
        }

        tx.execute(
            "INSERT INTO predicate(key, cardinality, declared) VALUES (?, ?, 1)
             ON CONFLICT(key) DO UPDATE SET cardinality = excluded.cardinality, declared = 1",
            params![key, c.as_str()],
        )?;
        tx.execute(
            "UPDATE fact SET is_single = ? WHERE predicate = ?",
            params![(c == Cardinality::Single) as i64, key],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Fixes whether a predicate's object names a thing, overriding what was
    /// inferred.
    ///
    /// Takes effect on later writes only. Facts already stored keep the shape
    /// they were written with -- rewriting them is [`crate::repair`]'s job, and
    /// it is a separate decision because it touches rows that are already true.
    pub fn set_relational(&self, predicate: &str, relational: bool) -> Result<()> {
        let key = require_key("predicate", predicate)?;
        let tx = self.conn().unchecked_transaction()?;
        tx.execute(
            "INSERT INTO predicate(key, cardinality, declared, relational)
             VALUES (?, 'single', 1, ?)
             ON CONFLICT(key) DO UPDATE SET relational = excluded.relational, declared = 1",
            params![key, relational as i64],
        )?;
        tx.commit()?;
        Ok(())
    }

    // --- reading -------------------------------------------------------------

    /// The value that currently holds: the open fact.
    ///
    /// "Current" means the latest state the brain knows, not "valid at this
    /// instant on the wall clock". Use [`Brain::as_of`] to travel in time. The
    /// distinction matters for a fact dated in the future: it is the newest thing
    /// known, but it is not yet true.
    pub fn current(&self, subject: &str, predicate: &str) -> Result<Option<Fact>> {
        Ok(self.current_all(subject, predicate)?.into_iter().next())
    }

    /// Every open fact, for multi-valued predicates.
    pub fn current_all(&self, subject: &str, predicate: &str) -> Result<Vec<Fact>> {
        let Some(entity_id) = find_entity(self.conn(), &norm::key(subject))? else {
            return Ok(Vec::new());
        };
        query_facts(
            self.conn(),
            &format!(
                "{SELECT_FACT} WHERE f.entity_id = ? AND f.predicate = ?
                   AND f.valid_to IS NULL AND f.retracted_at IS NULL
                 ORDER BY f.valid_from, f.id"
            ),
            params![entity_id, norm::key(predicate)],
        )
    }

    /// What was true at `t`.
    pub fn as_of(&self, subject: &str, predicate: &str, t: Timestamp) -> Result<Option<Fact>> {
        let Some(entity_id) = find_entity(self.conn(), &norm::key(subject))? else {
            return Ok(None);
        };
        let facts = query_facts(
            self.conn(),
            &format!(
                "{SELECT_FACT} WHERE f.entity_id = ? AND f.predicate = ?
                   AND f.retracted_at IS NULL
                   AND f.valid_from <= ?
                   AND (f.valid_to IS NULL OR ? < f.valid_to)
                 ORDER BY f.valid_from DESC, f.id DESC LIMIT 1"
            ),
            params![entity_id, norm::key(predicate), micros(t), micros(t)],
        )?;
        Ok(facts.into_iter().next())
    }

    /// The full record, including retracted facts.
    ///
    /// Retractions stay visible on purpose: "we believed 10, then corrected it"
    /// is part of the trajectory, and hiding it would make the audit trail lie.
    pub fn history(&self, subject: &str, predicate: &str) -> Result<Vec<Fact>> {
        let Some(entity_id) = find_entity(self.conn(), &norm::key(subject))? else {
            return Ok(Vec::new());
        };
        query_facts(
            self.conn(),
            &format!(
                "{SELECT_FACT} WHERE f.entity_id = ? AND f.predicate = ?
                 ORDER BY f.valid_from, f.id"
            ),
            params![entity_id, norm::key(predicate)],
        )
    }

    /// Loads one fact by id.
    pub fn fact(&self, id: i64) -> Result<Fact> {
        load_fact(self.conn(), id)
    }

    /// What is known about one entity, and what it connects to.
    ///
    /// The neighbourhood is the part worth having: it is the brain's own answer
    /// to "where would the answer be if it is not here", which is the question
    /// `recall` asks internally on every query. Being able to read it directly is
    /// what makes a surprising ranking explainable instead of mysterious.
    pub fn entity(&self, name: &str, when: When, depth: u32) -> Result<Option<EntityView>> {
        let conn = self.conn();
        let Some(id) = find_entity(conn, &norm::key(name))? else {
            return Ok(None);
        };
        // The entity's own key, not the one the caller asked by: looking something
        // up through an alias must report the thing that was found, or the answer
        // describes the question instead of the brain.
        let key = entity_key(conn, id)?;
        let filter = TemporalFilter::for_when(when, None);

        let mut binds: Vec<rusqlite::types::Value> = vec![id.into()];
        let where_sql = filter.bind(&mut binds);
        let mut stmt = conn.prepare(&format!(
            "{SELECT_FACT} WHERE f.entity_id = ?{where_sql} ORDER BY f.valid_from, f.id"
        ))?;
        let mut facts = Vec::new();
        for row in stmt.query_map(rusqlite::params_from_iter(binds), row_to_fact)? {
            facts.push(row??);
        }

        let mut neighbours = Vec::new();
        if depth > 0 {
            let walk = crate::graph::expand(conn, &[id], &filter, depth)?;
            for (entity_id, hops) in walk.reached() {
                let via = match walk.arrival.get(&entity_id) {
                    Some(step) => load_fact(conn, step.edge)?.statement,
                    None => continue,
                };
                neighbours.push(Neighbour {
                    key: entity_key(conn, entity_id)?,
                    entity: entity_label(conn, entity_id)?,
                    hops,
                    via,
                });
            }
        }

        Ok(Some(EntityView {
            key,
            label: entity_label(conn, id)?,
            aliases: crate::alias::aliases_of(conn, id)?,
            facts,
            neighbours,
        }))
    }

    /// Where a fact came from and what became of it.
    pub fn why(&self, id: i64) -> Result<Provenance> {
        let fact = load_fact(self.conn(), id).map_err(|_| BrainError::NoSuchFact(id))?;
        let superseded_by = match fact.superseded_by {
            Some(next) => Some(load_fact(self.conn(), next)?),
            None => None,
        };
        let supersedes = query_facts(
            self.conn(),
            &format!("{SELECT_FACT} WHERE f.superseded_by = ? LIMIT 1"),
            params![id],
        )?
        .into_iter()
        .next();

        Ok(Provenance {
            fact,
            superseded_by,
            supersedes,
        })
    }
}

// --- helpers -----------------------------------------------------------------

/// Timestamps are stored as microseconds; see the header of `schema.sql`.
fn micros(t: Timestamp) -> i64 {
    t.as_microsecond()
}

fn from_micros(v: i64) -> Timestamp {
    Timestamp::from_microsecond(v).unwrap_or(Timestamp::UNIX_EPOCH)
}

fn require_key(what: &'static str, given: &str) -> Result<String> {
    let k = norm::key(given);
    if k.is_empty() {
        return Err(BrainError::EmptyKey {
            what,
            given: given.to_string(),
        });
    }
    Ok(k)
}

const SELECT_FACT: &str = "
SELECT f.id, f.uuid, e.label, e.key, f.predicate,
       f.object_text, f.object_num, oe.label, f.unit, f.statement,
       f.valid_from, f.valid_to, f.recorded_at, f.retracted_at, f.superseded_by,
       f.confidence, f.reassert_count, f.scope, f.source, f.locator
FROM fact f
JOIN entity e ON e.id = f.entity_id
LEFT JOIN entity oe ON oe.id = f.object_entity_id";

fn query_facts(conn: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> Result<Vec<Fact>> {
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(p, row_to_fact)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r??);
    }
    Ok(out)
}

/// The inner `Result` carries a `BrainError` so a malformed stored locator is
/// reported as such rather than as an opaque SQLite type error.
#[allow(clippy::type_complexity)]
fn row_to_fact(r: &rusqlite::Row<'_>) -> rusqlite::Result<Result<Fact>> {
    let id: i64 = r.get(0)?;
    let locator_raw: Option<String> = r.get(19)?;
    let locator = match locator_raw {
        Some(s) => match serde_json::from_str(&s) {
            Ok(v) => Some(v),
            Err(source) => return Ok(Err(BrainError::BadLocator { id, source })),
        },
        None => None,
    };
    let uuid_raw: String = r.get(1)?;

    Ok(Ok(Fact {
        id,
        uuid: uuid_raw.parse().unwrap_or_default(),
        entity: r.get(2)?,
        entity_key: r.get(3)?,
        predicate: r.get(4)?,
        object_text: r.get(5)?,
        object_num: r.get(6)?,
        object_entity: r.get(7)?,
        unit: r.get(8)?,
        statement: r.get(9)?,
        valid_from: from_micros(r.get(10)?),
        valid_to: r.get::<_, Option<i64>>(11)?.map(from_micros),
        recorded_at: from_micros(r.get(12)?),
        retracted_at: r.get::<_, Option<i64>>(13)?.map(from_micros),
        superseded_by: r.get(14)?,
        confidence: r.get(15)?,
        reassert_count: r.get(16)?,
        scope: r.get(17)?,
        source: r.get(18)?,
        locator,
    }))
}

fn load_fact(conn: &Connection, id: i64) -> Result<Fact> {
    query_facts(conn, &format!("{SELECT_FACT} WHERE f.id = ?"), params![id])?
        .into_iter()
        .next()
        .ok_or(BrainError::NoSuchFact(id))
}

/// Facts still believed, oldest first. Retracted ones are excluded because they
/// must not influence where a new fact lands.
fn live_facts(conn: &Connection, entity_id: i64, predicate: &str) -> Result<Vec<Fact>> {
    query_facts(
        conn,
        &format!(
            "{SELECT_FACT} WHERE f.entity_id = ? AND f.predicate = ? AND f.retracted_at IS NULL
             ORDER BY f.valid_from, f.id"
        ),
        params![entity_id, predicate],
    )
}

fn retract_fact(conn: &Connection, id: i64, now: Timestamp, reason: Option<&str>) -> Result<()> {
    conn.execute(
        "UPDATE fact
         SET retracted_at = ?,
             source = COALESCE(source, '') || CASE WHEN ?2 IS NULL THEN '' ELSE ' [retracted: ' || ?2 || ']' END
         WHERE id = ?3",
        params![micros(now), reason, id],
    )?;
    index::mark_closed(conn, id)?;
    Ok(())
}

/// Which entity a key belongs to -- the lookup that decides where a write lands.
///
/// Declared aliases resolve here, so "ACME Corp" and `acme` accumulate one
/// history. Learned ones deliberately do **not**: they are guesses made from
/// watching questions, and a guess must never decide where a fact is stored. See
/// the module comment on [`crate::alias`]; the split is enforced by this `WHERE`
/// clause and by nothing else, so it is load-bearing.
pub(crate) fn find_entity(conn: &Connection, key: &str) -> Result<Option<i64>> {
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

fn upsert_entity(conn: &Connection, key: &str, label: &str, now: Timestamp) -> Result<i64> {
    if let Some(id) = find_entity(conn, key)? {
        return Ok(id);
    }
    conn.execute(
        "INSERT INTO entity(key, label, created_at) VALUES (?, ?, ?)",
        params![key, label, micros(now)],
    )?;
    Ok(conn.last_insert_rowid())
}

fn entity_label(conn: &Connection, id: i64) -> Result<String> {
    Ok(
        conn.query_row("SELECT label FROM entity WHERE id = ?", params![id], |r| {
            r.get(0)
        })?,
    )
}

fn entity_key(conn: &Connection, id: i64) -> Result<String> {
    Ok(
        conn.query_row("SELECT key FROM entity WHERE id = ?", params![id], |r| {
            r.get(0)
        })?,
    )
}

/// What a predicate is: how many values it holds at once, and whether its object
/// names a thing.
#[derive(Debug, Clone, Copy)]
struct PredicateShape {
    cardinality: Cardinality,
    relational: bool,
}

/// Returns the predicate's cardinality, creating it if unknown.
///
/// New predicates default to single-valued. That is the conservative choice: if
/// a genuinely multi-valued predicate is treated as single, values supersede each
/// other and the mistake is immediately visible in `history` and fixable with
/// `set_cardinality`. The opposite mistake -- treating price as multi-valued --
/// leaves several facts open at once, which makes "what is the price?"
/// ambiguous. That is precisely the failure this project exists to prevent.
fn upsert_predicate(
    conn: &Connection,
    key: &str,
    declared: Option<Cardinality>,
) -> Result<PredicateShape> {
    let existing: Option<(String, i64, i64)> = conn
        .query_row(
            "SELECT cardinality, declared, relational FROM predicate WHERE key = ?",
            params![key],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;

    // Read out before the match below consumes the row.
    let was_declared = matches!(existing, Some((_, 1, _)));
    let stored_relational = existing.as_ref().map(|&(_, _, r)| r != 0);

    let relational = match (was_declared, stored_relational) {
        (true, Some(r)) => r,
        _ => is_relational(conn, key)?,
    };

    let cardinality = match existing {
        // A cardinality the user fixed is never revised by an incoming assertion.
        Some((c, 1, _)) => Cardinality::parse(&c).unwrap_or(Cardinality::Single),
        Some((c, _, _)) => {
            if let Some(d) = declared {
                conn.execute(
                    "UPDATE predicate SET cardinality = ?, declared = 1 WHERE key = ?",
                    params![d.as_str(), key],
                )?;
                d
            } else {
                conn.execute(
                    "UPDATE predicate SET observed_n = observed_n + 1 WHERE key = ?",
                    params![key],
                )?;
                Cardinality::parse(&c).unwrap_or(Cardinality::Single)
            }
        }
        None => {
            let c = declared.unwrap_or(Cardinality::Single);
            conn.execute(
                "INSERT INTO predicate(key, cardinality, declared, observed_n) VALUES (?,?,?,1)",
                params![key, c.as_str(), declared.is_some() as i64],
            )?;
            c
        }
    };

    // Record what was inferred, so the flag on disk always matches the behaviour.
    // A brain whose stored ontology disagrees with how it actually writes is a
    // brain nobody can reason about, including `lint` and the studio.
    if !was_declared && stored_relational != Some(relational) {
        conn.execute(
            "UPDATE predicate SET relational = ? WHERE key = ?",
            params![relational as i64, key],
        )?;
    }

    Ok(PredicateShape {
        cardinality,
        relational,
    })
}

/// Whether this predicate's objects name things.
///
/// The evidence is one entity-valued fact. Not a majority, deliberately: a
/// predicate is one relation or one attribute, never both, so the first time
/// somebody records `is_a` pointing at an entity they have said what `is_a` is.
/// A majority rule would have been useless for the case this was built for --
/// there `is_a` stood at 10 entity-valued against 59 strings, and any threshold
/// worth the name would have called it an attribute and kept it broken.
///
/// This is the same rule [`crate::lint::missed_relation`] warns from, on purpose.
/// One notion of "this reads as a relation" is checkable; two that merely agree
/// today are a divergence waiting to happen.
///
/// It can be wrong, so it is overridable and the override sticks: `declared = 1`
/// is never revisited, here or anywhere.
fn is_relational(conn: &Connection, key: &str) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM fact
          WHERE predicate = ? AND object_entity_id IS NOT NULL AND retracted_at IS NULL",
        params![key],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// An object flattened into the columns it occupies.
struct ResolvedObject {
    text: Option<String>,
    num: Option<f64>,
    entity_id: Option<i64>,
    unit: Option<String>,
}

impl ResolvedObject {
    fn display(&self) -> String {
        match (&self.text, self.num, &self.unit) {
            (Some(t), _, _) => t.clone(),
            (None, Some(n), Some(u)) => format!("{n} {u}"),
            (None, Some(n), None) => n.to_string(),
            _ => String::new(),
        }
    }

    /// Whether an existing fact already says exactly this.
    fn matches(&self, f: &Fact) -> bool {
        // An entity object is identified by its id, not its label: two spellings
        // of the same entity are the same claim.
        if self.entity_id.is_some() || f.object_entity.is_some() {
            return self.text == f.object_text;
        }
        self.text == f.object_text && self.num == f.object_num && self.unit == f.unit
    }
}

/// Flattens an object into columns, promoting a string to an entity when the
/// predicate is a relation.
///
/// Promotion is what makes `relational` more than a label. `is_a cupao_stripe`
/// written as a string is a fact nothing can walk through, and the writer gets no
/// signal because nothing fails; once the predicate is known to be a relation,
/// the string is resolved through the very same path `Object::Entity` takes, so
/// identity stays exactly as exact as it was.
///
/// A number is never promoted. A predicate can be a relation and still receive
/// something that is plainly a literal, and inventing an entity called `50` would
/// be a worse outcome than the mistake being caught.
fn resolve_object(
    conn: &Connection,
    o: &Object,
    relational: bool,
    now: Timestamp,
) -> Result<ResolvedObject> {
    Ok(match o {
        Object::Text(s) if relational && !norm::key(s).is_empty() => {
            let key = norm::key(s);
            let id = upsert_entity(conn, &key, s, now)?;
            ResolvedObject {
                text: Some(entity_label(conn, id)?),
                num: None,
                entity_id: Some(id),
                unit: None,
            }
        }
        Object::Text(s) => ResolvedObject {
            text: Some(s.clone()),
            num: None,
            entity_id: None,
            unit: None,
        },
        Object::Num { value, unit } => ResolvedObject {
            text: None,
            num: Some(*value),
            entity_id: None,
            unit: unit.clone(),
        },
        Object::Entity(name) => {
            let key = require_key("object entity", name)?;
            let id = upsert_entity(conn, &key, name, now)?;
            ResolvedObject {
                // The label is stored in `object_text` too so that search and the
                // NOT NULL check work without a join.
                text: Some(entity_label(conn, id)?),
                num: None,
                entity_id: Some(id),
                unit: None,
            }
        }
    })
}

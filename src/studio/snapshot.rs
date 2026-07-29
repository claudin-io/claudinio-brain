//! Everything a brain holds, in one JSON document.
//!
//! The studio needs the *whole* timeline, not the filtered views the other read
//! paths return. `get` and `recall` answer with what is true; a debugger has to
//! show what was believed, what closed it, and what turned out never to have been
//! true -- so this deliberately selects closed and retracted facts too, and lets
//! the browser do the temporal filtering as the time cursor moves. Re-querying
//! SQLite on every frame of a scrub is not a thing that can be made to feel
//! instant.
//!
//! Entities carry their numeric `id` here, which the [`crate::brain::Fact`] view
//! does not: a graph needs stable node handles, and matching on labels would
//! collapse two entities that happen to print the same.

use crate::brain::{Brain, BrainError};
use jiff::Timestamp;
use rusqlite::Connection;
use serde::Serialize;

type Result<T> = std::result::Result<T, BrainError>;

/// The JSON escape for `<`, spelled out so no editor or codemod can quietly
/// turn it back into a literal `<` and make `to_inline_json` a no-op.
const LT_ESCAPE: &str = "\\u003c";

/// The whole brain, as the browser sees it.
#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub brain_id: String,
    pub brain_label: String,
    pub brain_path: String,
    /// When this snapshot was taken. A static export is a photograph, and a
    /// photograph with no date invites being read as live.
    pub generated_at: Timestamp,
    /// Whether a write API is reachable. The page hides its editor when not.
    pub live: bool,
    pub entities: Vec<SnapEntity>,
    pub predicates: Vec<SnapPredicate>,
    pub facts: Vec<SnapFact>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapEntity {
    pub id: i64,
    pub key: String,
    pub label: String,
    pub kind: Option<String>,
    pub created_at: Timestamp,
    pub aliases: Vec<SnapAlias>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapAlias {
    pub key: String,
    /// `declared` or `learned`. The studio colours them differently because the
    /// difference decides whether a name can move a fact to another entity.
    pub source: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapPredicate {
    pub key: String,
    pub cardinality: String,
    pub declared: bool,
    pub observed_n: i64,
}

/// One fact, both time axes intact.
///
/// `object_entity_id` being set is what makes this row an edge; the browser
/// builds the graph from that single field rather than from a separate edge
/// list, for the same reason the schema does.
#[derive(Debug, Clone, Serialize)]
pub struct SnapFact {
    pub id: i64,
    pub uuid: String,
    pub entity_id: i64,
    pub entity: String,
    pub entity_key: String,
    pub predicate: String,

    pub object_text: Option<String>,
    pub object_num: Option<f64>,
    pub object_entity_id: Option<i64>,
    pub object_entity: Option<String>,
    pub unit: Option<String>,
    pub statement: String,

    pub valid_from: Timestamp,
    pub valid_to: Option<Timestamp>,
    pub recorded_at: Timestamp,
    pub retracted_at: Option<Timestamp>,
    pub superseded_by: Option<i64>,

    pub confidence: f64,
    pub reassert_count: i64,
    pub scope: Option<String>,
    pub source: Option<String>,
    pub locator: Option<serde_json::Value>,
}

impl Snapshot {
    /// Reads the entire brain.
    ///
    /// Four queries, none of them per-row: a brain with a thousand entities would
    /// otherwise cost a thousand alias lookups, and the studio reloads this on
    /// every edit.
    pub fn capture(brain: &Brain, live: bool) -> Result<Self> {
        let conn = brain.store().conn();
        Ok(Self {
            brain_id: brain.store().id().to_string(),
            brain_label: brain.store().label().to_string(),
            brain_path: brain.store().path().display().to_string(),
            generated_at: jiff::Timestamp::now(),
            live,
            entities: entities(conn)?,
            predicates: predicates(conn)?,
            facts: facts(conn)?,
        })
    }

    /// The snapshot as JSON safe to inline inside a `<script>` element.
    ///
    /// A fact whose text contains `</script>` would otherwise close the element
    /// early and turn stored data into markup, and a stray `<!--` inside a script
    /// element swallows what follows it. In serialized JSON a `<` can only ever
    /// occur inside a string -- every structural character is one of `{}[]:,"` or
    /// part of a number or literal -- so escaping every one of them to the JSON
    /// unicode escape below is both valid JSON and a rule with no exceptions to
    /// remember.
    pub fn to_inline_json(&self) -> serde_json::Result<String> {
        Ok(serde_json::to_string(self)?.replace('<', LT_ESCAPE))
    }
}

fn entities(conn: &Connection) -> Result<Vec<SnapEntity>> {
    let mut stmt =
        conn.prepare("SELECT id, key, label, kind, created_at FROM entity ORDER BY id")?;
    let mut out: Vec<SnapEntity> = Vec::new();
    let rows = stmt.query_map([], |r| {
        Ok(SnapEntity {
            id: r.get(0)?,
            key: r.get(1)?,
            label: r.get(2)?,
            kind: r.get(3)?,
            created_at: from_micros(r.get(4)?),
            aliases: Vec::new(),
        })
    })?;
    for r in rows {
        out.push(r?);
    }

    // One sweep, merged by position: `out` is ordered by id and so is this, so
    // the join is a binary search rather than a query per entity.
    let mut stmt = conn.prepare(
        "SELECT entity_id, alias_key, source, weight FROM entity_alias
         ORDER BY entity_id, weight DESC, alias_key",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            SnapAlias {
                key: r.get(1)?,
                source: r.get(2)?,
                weight: r.get(3)?,
            },
        ))
    })?;
    for r in rows {
        let (entity_id, alias) = r?;
        if let Ok(i) = out.binary_search_by_key(&entity_id, |e| e.id) {
            out[i].aliases.push(alias);
        }
    }
    Ok(out)
}

fn predicates(conn: &Connection) -> Result<Vec<SnapPredicate>> {
    let mut stmt =
        conn.prepare("SELECT key, cardinality, declared, observed_n FROM predicate ORDER BY key")?;
    let rows = stmt.query_map([], |r| {
        Ok(SnapPredicate {
            key: r.get(0)?,
            cardinality: r.get(1)?,
            declared: r.get::<_, i64>(2)? != 0,
            observed_n: r.get(3)?,
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn facts(conn: &Connection) -> Result<Vec<SnapFact>> {
    let mut stmt = conn.prepare(
        "SELECT f.id, f.uuid, f.entity_id, e.label, e.key, f.predicate,
                f.object_text, f.object_num, f.object_entity_id, oe.label,
                f.unit, f.statement,
                f.valid_from, f.valid_to, f.recorded_at, f.retracted_at,
                f.superseded_by, f.confidence, f.reassert_count,
                f.scope, f.source, f.locator
         FROM fact f
         JOIN entity e ON e.id = f.entity_id
         LEFT JOIN entity oe ON oe.id = f.object_entity_id
         ORDER BY f.id",
    )?;
    let rows = stmt.query_map([], |r| {
        let locator_raw: Option<String> = r.get(21)?;
        Ok(SnapFact {
            id: r.get(0)?,
            uuid: r.get(1)?,
            entity_id: r.get(2)?,
            entity: r.get(3)?,
            entity_key: r.get(4)?,
            predicate: r.get(5)?,
            object_text: r.get(6)?,
            object_num: r.get(7)?,
            object_entity_id: r.get(8)?,
            object_entity: r.get(9)?,
            unit: r.get(10)?,
            statement: r.get(11)?,
            valid_from: from_micros(r.get(12)?),
            valid_to: r.get::<_, Option<i64>>(13)?.map(from_micros),
            recorded_at: from_micros(r.get(14)?),
            retracted_at: r.get::<_, Option<i64>>(15)?.map(from_micros),
            superseded_by: r.get(16)?,
            confidence: r.get(17)?,
            reassert_count: r.get(18)?,
            scope: r.get(19)?,
            source: r.get(20)?,
            // A locator that will not parse is shown as the raw string rather
            // than failing the whole snapshot: the studio exists to look at
            // brains that have something wrong with them.
            locator: locator_raw
                .map(|s| serde_json::from_str(&s).unwrap_or(serde_json::Value::String(s))),
        })
    })?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn from_micros(v: i64) -> Timestamp {
    Timestamp::from_microsecond(v).unwrap_or(Timestamp::UNIX_EPOCH)
}

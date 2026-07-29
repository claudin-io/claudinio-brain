//! What the brain can see wrong with itself.
//!
//! Every finding here is a *structural* defect: the fact is stored, it is true,
//! and retrieval still cannot use it. That combination is the dangerous one,
//! because nothing fails. A relation written as a string succeeds, reads back
//! correctly, prints correctly, and is invisible to every walk of the graph --
//! the brain is quietly less connected than its contents deserve, and the only
//! symptom is an answer that never arrives.
//!
//! The motivating case: a brain where 59 of 69 `is_a` facts had a string where an
//! entity belonged. Every coupon knew its class and no coupon was reachable from
//! it. Discovering that required opening a 3D scene and noticing loose dots. A
//! memory system that can detect this and does not report it is failing at its
//! job, so this module exists to make that a command instead of an observation.
//!
//! Nothing here writes. Lint reports and suggests; repairing is a separate,
//! explicit decision.

use crate::brain::BrainError;
use crate::norm;
use rusqlite::Connection;
use serde::Serialize;
use std::collections::BTreeSet;

/// Object strings that several entities share without ever being a class.
///
/// A boolean is the value of a flag, not the name of a category, and flags are
/// the most-shared strings in any brain -- left in, they would bury the real
/// candidates under `true` and `false`. Kept deliberately short: the risk of a
/// longer list is suppressing a genuine class whose name happens to sit on it.
const NOT_CLASSES: &[&str] = &["true", "false", "sim", "nao", "yes", "no", "n_a", "none"];

/// Longest object string still plausible as a class name.
///
/// A class is named, not described. Past this length a shared string is prose --
/// two entities carrying the same paragraph is a copy-paste, which is worth
/// knowing but is not the finding this section is about.
const MAX_CLASS_LEN: usize = 64;

/// How many single-character edits still count as one name misspelled twice.
///
/// See [`twins`]. Two is the usual near-duplicate cut: it covers a plural, a
/// dropped accent and a transposition, and stops short of the three edits that
/// separate genuinely parallel names like `claudinio_senior` and
/// `claudius_senior`.
const MAX_TWIN_DISTANCE: usize = 2;

/// Keys shorter than this are not compared.
///
/// Two edits inside a five-character key is most of the key, and short keys are
/// where unrelated words collide.
const MIN_TWIN_LEN: usize = 6;

/// Everything lint found, plus the totals that give the findings scale.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub entities: i64,
    pub facts: i64,
    /// Facts whose object is an entity: the edges of the graph.
    pub edges: i64,
    pub orphans: Vec<Orphan>,
    pub mixed_predicates: Vec<MixedPredicate>,
    pub candidate_classes: Vec<CandidateClass>,
    pub twins: Vec<Twin>,
}

/// An entity nothing currently connects to.
///
/// "Currently" is the point: an entity whose only relation was closed is
/// genuinely unreachable now, and that is what retrieval and the studio's default
/// view both see. Counting a closed edge as connectivity would report a graph
/// that no longer exists.
#[derive(Debug, Clone, Serialize)]
pub struct Orphan {
    pub key: String,
    pub label: String,
    /// How much is known about it. A rich orphan is a worse defect than a bare
    /// one: the facts are there, and nothing can reach them sideways.
    pub facts: i64,
}

/// A predicate used both ways: sometimes an entity, sometimes a string.
///
/// This is the strongest signal in the report. A predicate is one relation or one
/// attribute; it is not both. When both shapes appear, the string ones are almost
/// always the mistake -- somebody passed `value` where `entity` belonged -- and
/// the count says how many times.
#[derive(Debug, Clone, Serialize)]
pub struct MixedPredicate {
    pub predicate: String,
    pub as_entity: i64,
    pub as_text: i64,
    /// The most common string objects, as evidence for the reading above.
    pub examples: Vec<String>,
}

/// A string that several entities share and that is not an entity.
///
/// If three things are `cupao_stripe`, then `cupao_stripe` is a thing. Promoting
/// it to a node is what lets the three reach each other.
#[derive(Debug, Clone, Serialize)]
pub struct CandidateClass {
    pub predicate: String,
    pub value: String,
    /// Distinct entities holding it.
    pub entities: i64,
}

/// Two entity keys close enough to be one name written twice.
///
/// The failure this catches is a spelling that drifted between two writes --
/// `plano_de_lugar` and `planos_de_lugar` -- which the brain stores as two
/// entities growing two parallel histories. The SKILL warns that this is not
/// recoverable; nothing ever checked for it.
#[derive(Debug, Clone, Serialize)]
pub struct Twin {
    pub a: String,
    pub b: String,
    /// Single-character edits between the two keys.
    pub distance: usize,
}

impl Report {
    /// Whether anything was found. Drives `--strict`.
    pub fn is_clean(&self) -> bool {
        self.orphans.is_empty()
            && self.mixed_predicates.is_empty()
            && self.candidate_classes.is_empty()
            && self.twins.is_empty()
    }

    /// The report as lines for a person, findings first and totals last.
    pub fn lines(&self) -> Vec<String> {
        let mut out = Vec::new();

        if let Some(worst) = self.mixed_predicates.first() {
            out.push(format!(
                "{} predicate(s) store an entity sometimes and a string other times:",
                self.mixed_predicates.len()
            ));
            for m in &self.mixed_predicates {
                out.push(format!(
                    "  {:<20} {} as entity, {} as text{}",
                    m.predicate,
                    m.as_entity,
                    m.as_text,
                    if m.examples.is_empty() {
                        String::new()
                    } else {
                        format!("   e.g. {}", m.examples.join(", "))
                    }
                ));
            }
            out.push(format!(
                "  -> the text ones are unreachable by any walk. Record a relation with \
                 `--entity {}`, not `--value`.",
                worst.examples.first().map_or("<thing>", String::as_str)
            ));
            out.push(String::new());
        }

        if !self.candidate_classes.is_empty() {
            out.push(format!(
                "{} string(s) shared by several entities but stored as no node:",
                self.candidate_classes.len()
            ));
            for c in &self.candidate_classes {
                out.push(format!(
                    "  {:<20} {:<28} {} entities",
                    c.predicate, c.value, c.entities
                ));
            }
            out.push(
                "  -> each of these is a thing several entities have in common, and \
                 nothing can be reached through it."
                    .into(),
            );
            out.push(String::new());
        }

        if !self.orphans.is_empty() {
            out.push(format!(
                "{} of {} entities have no open relation in either direction:",
                self.orphans.len(),
                self.entities
            ));
            for o in self.orphans.iter().take(10) {
                out.push(format!("  {:<32} {} facts", o.label, o.facts));
            }
            if self.orphans.len() > 10 {
                out.push(format!("  ... and {} more", self.orphans.len() - 10));
            }
            out.push(String::new());
        }

        if !self.twins.is_empty() {
            out.push(format!(
                "{} pair(s) of entities look like one thing under two names:",
                self.twins.len()
            ));
            for t in &self.twins {
                out.push(format!("  {}  <->  {}   ({} edits)", t.a, t.b, t.distance));
            }
            out.push(
                "  -> if they are the same thing, `brain alias <keep> <drop>` before the \
                 two histories diverge further."
                    .into(),
            );
            out.push(String::new());
        }

        out.push(format!(
            "{} entities, {} facts, {} of them relations.",
            self.entities, self.facts, self.edges
        ));
        if self.is_clean() {
            out.push("Nothing to report.".into());
        }
        out
    }
}

/// Whether a text-valued write under this predicate looks like a missed relation.
///
/// The evidence is the brain's own history with the predicate: if somebody has
/// already recorded `is_a` pointing at an entity, then `is_a` is a relation here,
/// and the string that just landed is almost certainly the mistake. Deliberately
/// evidence-based rather than a list of known relational words -- this core has
/// no opinion about vocabulary in any language, and acquiring one to catch this
/// would be a worse trade than missing the first occurrence.
///
/// Returns the sentence to hand back to the writer, or `None` when there is
/// nothing to say. Called on the write path, so it is one indexed count.
pub fn missed_relation(conn: &Connection, predicate: &str) -> Result<Option<String>, BrainError> {
    let as_entity: i64 = conn.query_row(
        "SELECT count(*) FROM fact
          WHERE predicate = ? AND object_entity_id IS NOT NULL AND retracted_at IS NULL",
        [predicate],
        |r| r.get(0),
    )?;
    if as_entity == 0 {
        return Ok(None);
    }
    Ok(Some(format!(
        "`{predicate}` already points at an entity in {as_entity} other fact(s), so it \
         reads as a relation here. This one stored a string, which no walk of the graph \
         can follow -- if the value names a thing, write it with `entity` instead of \
         `value`. Run `brain lint` to see the rest."
    )))
}

/// Reads the brain and reports what is structurally wrong with it.
pub fn check(conn: &Connection) -> Result<Report, BrainError> {
    Ok(Report {
        entities: count(conn, "SELECT count(*) FROM entity")?,
        facts: count(conn, "SELECT count(*) FROM fact")?,
        edges: count(
            conn,
            "SELECT count(*) FROM fact WHERE object_entity_id IS NOT NULL",
        )?,
        orphans: orphans(conn)?,
        mixed_predicates: mixed_predicates(conn)?,
        candidate_classes: candidate_classes(conn)?,
        twins: twins(conn)?,
    })
}

fn count(conn: &Connection, sql: &str) -> Result<i64, BrainError> {
    Ok(conn.query_row(sql, [], |r| r.get(0))?)
}

/// Entities with no open edge on either end.
///
/// Both ends, because a relation is a road and not a one-way street -- the same
/// reason [`crate::graph::expand`] walks edges in both directions.
fn orphans(conn: &Connection) -> Result<Vec<Orphan>, BrainError> {
    let mut stmt = conn.prepare(
        "WITH linked(id) AS (
           SELECT entity_id        FROM fact WHERE object_entity_id IS NOT NULL
             AND valid_to IS NULL AND retracted_at IS NULL
           UNION
           SELECT object_entity_id FROM fact WHERE object_entity_id IS NOT NULL
             AND valid_to IS NULL AND retracted_at IS NULL
         )
         SELECT e.key, e.label,
                (SELECT count(*) FROM fact f
                  WHERE f.entity_id = e.id AND f.valid_to IS NULL
                    AND f.retracted_at IS NULL)
         FROM entity e
         WHERE e.id NOT IN (SELECT id FROM linked)
         ORDER BY 3 DESC, e.key",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Orphan {
            key: r.get(0)?,
            label: r.get(1)?,
            facts: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Predicates whose objects are entities in some facts and strings in others.
///
/// The entity test is `object_entity_id IS NULL`, never `object_text IS NULL`:
/// an entity-valued object carries its label in `object_text` as well, so testing
/// the text column would classify every edge as a string and report nothing.
fn mixed_predicates(conn: &Connection) -> Result<Vec<MixedPredicate>, BrainError> {
    let mut stmt = conn.prepare(
        "SELECT predicate,
                sum(object_entity_id IS NOT NULL),
                sum(object_entity_id IS NULL)
         FROM fact
         WHERE retracted_at IS NULL
         GROUP BY predicate
         HAVING sum(object_entity_id IS NOT NULL) > 0
            AND sum(object_entity_id IS NULL) > 0
         ORDER BY 3 DESC, predicate",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(MixedPredicate {
            predicate: r.get(0)?,
            as_entity: r.get(1)?,
            as_text: r.get(2)?,
            examples: Vec::new(),
        })
    })?;
    let mut out = rows.collect::<rusqlite::Result<Vec<MixedPredicate>>>()?;

    let mut stmt = conn.prepare(
        "SELECT object_text FROM fact
          WHERE predicate = ? AND object_entity_id IS NULL AND object_text IS NOT NULL
            AND retracted_at IS NULL
          GROUP BY object_text
          ORDER BY count(*) DESC, object_text
          LIMIT 3",
    )?;
    for m in &mut out {
        let rows = stmt.query_map([&m.predicate], |r| r.get::<_, String>(0))?;
        m.examples = rows
            .collect::<rusqlite::Result<Vec<String>>>()?
            .into_iter()
            .filter(|v| v.chars().count() <= MAX_CLASS_LEN)
            .collect();
    }
    Ok(out)
}

/// Strings that several entities share under one predicate and that name nothing.
fn candidate_classes(conn: &Connection) -> Result<Vec<CandidateClass>, BrainError> {
    let existing: BTreeSet<String> = {
        let mut stmt = conn.prepare("SELECT key FROM entity")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<BTreeSet<String>>>()?
    };

    let mut stmt = conn.prepare(
        "SELECT predicate, object_text, count(DISTINCT entity_id) AS n
         FROM fact
         WHERE object_entity_id IS NULL AND object_text IS NOT NULL
           AND valid_to IS NULL AND retracted_at IS NULL
         GROUP BY predicate, object_text
         HAVING n >= 2
         ORDER BY n DESC, predicate, object_text",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(CandidateClass {
            predicate: r.get(0)?,
            value: r.get(1)?,
            entities: r.get(2)?,
        })
    })?;

    Ok(rows
        .collect::<rusqlite::Result<Vec<CandidateClass>>>()?
        .into_iter()
        .filter(|c| {
            let k = norm::key(&c.value);
            c.value.chars().count() <= MAX_CLASS_LEN
                && !k.is_empty()
                && !NOT_CLASSES.contains(&k.as_str())
                // Already a node: whoever wrote it just did not link it, which is
                // the mixed-predicate finding rather than a missing class.
                && !existing.contains(&k)
        })
        .collect())
}

/// Entity keys within [`MAX_TWIN_DISTANCE`] edits of each other.
///
/// Edit distance rather than a shared-suffix rule, and the difference is not
/// cosmetic. A suffix rule reports every family that names its members the same
/// way -- `claudinio_senior` against `claudius_senior` -- which is good naming,
/// not a defect. Measuring the whole key instead asks the question that matters:
/// could one of these be the other, typed slightly differently?
///
/// Pairs already joined by a relation or an alias are dropped. `codigo_5tulsxzo`
/// and `cupao_5tulsxzo` look like one thing under two names right up until you
/// notice the `resgata` edge between them, at which point they are plainly two
/// things somebody modelled correctly.
fn twins(conn: &Connection) -> Result<Vec<Twin>, BrainError> {
    let mut stmt = conn.prepare("SELECT id, key FROM entity ORDER BY id")?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
    let keys: Vec<(i64, Vec<char>, String)> = rows
        .collect::<rusqlite::Result<Vec<(i64, String)>>>()?
        .into_iter()
        .filter(|(_, k)| k.chars().count() >= MIN_TWIN_LEN)
        .map(|(id, k)| (id, k.chars().collect(), k))
        .collect();

    let connected = connected_pairs(conn)?;
    let mut out = Vec::new();
    for (i, (id_a, chars_a, key_a)) in keys.iter().enumerate() {
        for (id_b, chars_b, key_b) in &keys[i + 1..] {
            // Lengths that differ by more than the budget cannot be closed by it,
            // and this is what keeps the quadratic loop cheap on a large brain.
            if chars_a.len().abs_diff(chars_b.len()) > MAX_TWIN_DISTANCE {
                continue;
            }
            if connected.contains(&(*id_a.min(id_b), *id_a.max(id_b))) {
                continue;
            }
            if numbered_variants(key_a, key_b) {
                continue;
            }
            let Some(distance) = edit_distance(chars_a, chars_b, MAX_TWIN_DISTANCE) else {
                continue;
            };
            out.push(Twin {
                a: key_a.clone(),
                b: key_b.clone(),
                distance,
            });
        }
    }
    out.sort_by(|x, y| x.distance.cmp(&y.distance).then(x.a.cmp(&y.a)));
    Ok(out)
}

/// Whether two keys are the same name carrying different numbers.
///
/// `codigo_upgrade25_liu67871` and `codigo_upgrade10_liu67871` are two edits
/// apart and are obviously not each other misspelled: numbering is how a series
/// gets named, and nobody mistypes 25 as 10. Stripping the digits and asking
/// whether what remains is identical separates a series from a typo without
/// knowing anything about the domain.
fn numbered_variants(a: &str, b: &str) -> bool {
    let strip = |s: &str| s.chars().filter(|c| !c.is_numeric()).collect::<String>();
    a != b && strip(a) == strip(b)
}

/// Levenshtein distance, or `None` once it is known to exceed `budget`.
///
/// Two rolling rows rather than a full matrix, and an early exit when no cell in
/// a row is still within budget -- the answer this caller wants is "close or
/// not", so computing an exact large distance would be work spent on a result
/// that gets thrown away.
fn edit_distance(a: &[char], b: &[char], budget: usize) -> Option<usize> {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];

    for (i, ca) in a.iter().enumerate() {
        curr[0] = i + 1;
        let mut row_best = curr[0];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(curr[j] + 1);
            row_best = row_best.min(curr[j + 1]);
        }
        if row_best > budget {
            return None;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    let d = prev[b.len()];
    (d <= budget).then_some(d)
}

/// Entity pairs already joined by an open edge or by a declared alias, as
/// `(lower id, higher id)` so lookup does not care which way round the pair came.
fn connected_pairs(conn: &Connection) -> Result<BTreeSet<(i64, i64)>, BrainError> {
    let mut stmt = conn.prepare(
        "SELECT entity_id, object_entity_id FROM fact
          WHERE object_entity_id IS NOT NULL
            AND valid_to IS NULL AND retracted_at IS NULL
         UNION
         SELECT a.entity_id, b.entity_id FROM entity_alias a
           JOIN entity_alias b ON a.alias_key = b.alias_key
          WHERE a.entity_id <> b.entity_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    let mut out = BTreeSet::new();
    for row in rows {
        let (a, b) = row?;
        out.insert((a.min(b), a.max(b)));
    }

    // An alias key that *is* another entity's key joins them just as firmly, and
    // that is exactly how one thing ends up under two spellings.
    let mut stmt = conn.prepare(
        "SELECT al.entity_id, e.id FROM entity_alias al
           JOIN entity e ON e.key = al.alias_key
          WHERE e.id <> al.entity_id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)))?;
    for row in rows {
        let (a, b) = row?;
        out.insert((a.min(b), a.max(b)));
    }
    Ok(out)
}

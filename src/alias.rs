//! The names a thing goes by.
//!
//! An entity is stored under one key -- [`crate::norm::key`] of whatever was
//! first written -- but people ask about it in other words. An alias is a second
//! key pointing at the same entity, and there are exactly two ways one appears:
//!
//! - **declared**, because someone said so: `brain alias acme "ACME Corp"`.
//! - **learned**, because the brain watched a question resolve unambiguously to
//!   one entity and kept the name that question used.
//!
//! The two are stored side by side but trusted very differently, and the
//! difference is the whole design:
//!
//! **A declared alias decides identity. A learned one only helps retrieval.**
//!
//! [`crate::brain::find_entity`] -- the lookup that decides which entity a write
//! attaches to -- resolves declared aliases and *ignores* learned ones. A user
//! saying "ACME Corp is acme" is a statement about the world, and later facts
//! about "ACME Corp" belong on that entity. A guess made from watching one
//! question is not: if it decided identity, a single well-phrased question could
//! silently graft an entity's whole future history onto the wrong node, and
//! nothing in the output would show it happened. Retrieval can afford a wrong
//! guess -- it surfaces one extra candidate and RRF buries it. Identity cannot.

use crate::brain::{Brain, BrainError, Fact};
use crate::norm;
use crate::recall::{Hit, normalized_terms};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;

type Result<T> = std::result::Result<T, BrainError>;

/// Where a name came from, which is what says how far to trust it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasSource {
    Declared,
    Learned,
}

impl AliasSource {
    pub fn as_str(self) -> &'static str {
        match self {
            AliasSource::Declared => "declared",
            AliasSource::Learned => "learned",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        match s {
            "declared" => Some(AliasSource::Declared),
            "learned" => Some(AliasSource::Learned),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Alias {
    pub key: String,
    pub source: AliasSource,
    /// How much this name is trusted when several entities match one question.
    pub weight: f64,
}

/// A name the brain taught itself, and the entity it now points at.
#[derive(Debug, Clone, Serialize)]
pub struct Learned {
    pub alias: String,
    pub entity: String,
    pub entity_key: String,
    pub weight: f64,
}

/// Minimum cosine between a candidate name and the label of the entity it would
/// name.
///
/// This is what stops the brain from deciding that "quanto", "do" and "qual" are
/// names for whatever the question happened to be about. No stopword list is
/// involved, and that is deliberate: a list is a language decision, and this
/// brain answers in Portuguese and English in the same breath. Asking the
/// embedder "is this term another way of saying this entity's name?" is the same
/// question in every language.
///
/// Calibrated against the labels the eval suites use. Measured cosines fall into
/// three bands with a wide empty gap between them:
///
/// | band | example | cosine |
/// |---|---|---|
/// | genuine variant | `produto_brasilia` -> "Produto Brasília" | 1.000 |
/// | | `pgbounce` -> "pgbouncer" | 0.961 |
/// | | `pg_bouncer` -> "pgbouncer" | 0.621 |
/// | name fragment | `gateway` -> "api_gateway" | 0.715 |
/// | question filler | `quanto_custa` -> "Produto Brasília" | 0.251 |
/// | | `qual` -> "pgbouncer" | 0.052 |
/// | a different entity | `produto_a` -> "acme" | -0.042 |
///
/// 0.50 sits in the gap, above every filler measured and below every genuine
/// variant.
///
/// Two honest caveats about how that number was arrived at, because the usual
/// method did not work here:
///
/// - **The eval suites do not discriminate it.** Sweeping 0.40/0.50/0.60/0.70
///   moves no metric on any suite, because genuine variants score near 1.0 and
///   the fillers in those questions are already excluded by the "a name that
///   contains a name is not a name" rule in [`Brain::eligible_terms`]. The
///   suites therefore cannot be cited as evidence for this value, and are not.
/// - **The floor is still load-bearing**, which the tests show where the evals
///   cannot: remove it entirely and `mesmo` (0.097) becomes a name for
///   `pgbouncer`. What it guards is the word the brain has never seen in any
///   role -- not an entity, not a predicate, so nothing else excludes it. Any
///   value from 0.20 up holds that line; 0.50 is the middle of the measured gap
///   rather than its edge.
const MIN_ALIAS_COSINE: f32 = 0.50;

/// A learned name never outranks one a person declared, whatever its cosine.
const LEARNED_MAX_WEIGHT: f64 = 0.9;

/// Whether a question picked out one entity clearly enough to be worth learning
/// from.
///
/// The test is agreement between channels, not a score threshold: the best hit
/// must have been found by *strictly more* channels than anything belonging to a
/// different entity. That is [`crate::recall`]'s own founding principle -- a fact
/// several independent retrievers surface is far more likely to be the answer
/// than one any of them finds alone -- read backwards to ask how sure the
/// ranking is of itself.
///
/// Deliberately not a margin on the fused score. RRF scores are comparable
/// within one query and meaningless across queries, so any numeric cut would
/// need calibrating against a corpus and would quietly stop meaning "decisive"
/// as the brain grew. Counting channels needs no constant at all.
///
/// Measured on a four-fact brain: "quanto custa o produto brasilia" gives the
/// right fact 0.0328 on `[bm25, semantic]` and two unrelated facts 0.0161 each
/// on `[semantic]` alone -- the vector index always returns *something*, so the
/// runners-up are floor noise rather than rival readings. Asking "preco" of a
/// brain holding three products instead puts several entities on `[bm25,
/// semantic]` together, and nothing is learned. That is the distinction the rule
/// has to draw, and it draws it without being told a number.
fn is_decisive(hits: &[Hit]) -> Option<&Fact> {
    let best = hits.first()?;
    let rivals = hits
        .iter()
        .filter(|h| h.fact.entity_key != best.fact.entity_key)
        .map(|h| h.channels.len())
        .max()
        .unwrap_or(0);
    (best.channels.len() > rivals).then_some(&best.fact)
}

impl Brain {
    /// Gives an entity another name, on purpose.
    pub fn declare_alias(&self, entity: &str, alias: &str) -> Result<Alias> {
        let entity_key = norm::key(entity);
        let alias_key = norm::key(alias);
        if alias_key.is_empty() {
            return Err(BrainError::EmptyKey {
                what: "alias",
                given: alias.to_string(),
            });
        }

        let conn = self.store().conn();
        let id = require_entity(conn, &entity_key, entity)?;

        // A name already in use is refused rather than merged. Because declared
        // aliases decide identity, quietly accepting this would send every future
        // fact about one of the two entities to the other one.
        if let Some(owner) = owner_of(conn, &alias_key)?
            && owner != id
        {
            return Err(BrainError::AliasTaken {
                alias: alias_key,
                owner: entity_label(conn, owner)?,
            });
        }
        if alias_key == entity_key {
            return Err(BrainError::AliasTaken {
                alias: alias_key,
                owner: entity_label(conn, id)?,
            });
        }

        // Promotion, not a second row: a name someone declares stops being a
        // guess, and the (alias_key, entity_id) primary key holds one verdict per
        // pair rather than a history of how it was arrived at.
        conn.execute(
            "INSERT INTO entity_alias(alias_key, entity_id, source, weight)
             VALUES (?, ?, 'declared', 1.0)
             ON CONFLICT(alias_key, entity_id)
             DO UPDATE SET source = 'declared', weight = 1.0",
            params![alias_key, id],
        )?;

        Ok(Alias {
            key: alias_key,
            source: AliasSource::Declared,
            weight: 1.0,
        })
    }

    /// Takes a name away. Returns whether there was one to take.
    pub fn forget_alias(&self, entity: &str, alias: &str) -> Result<bool> {
        let conn = self.store().conn();
        let entity_key = norm::key(entity);
        let id = require_entity(conn, &entity_key, entity)?;
        let n = conn.execute(
            "DELETE FROM entity_alias WHERE alias_key = ? AND entity_id = ?",
            params![norm::key(alias), id],
        )?;
        Ok(n > 0)
    }

    /// Every name an entity answers to, declared first.
    pub fn aliases(&self, entity: &str) -> Result<Vec<Alias>> {
        let conn = self.store().conn();
        let entity_key = norm::key(entity);
        let id = require_entity(conn, &entity_key, entity)?;
        aliases_of(conn, id)
    }

    /// Keeps the name a question used, if that question left no doubt what it was
    /// about.
    ///
    /// Called only when the caller opts in, because a read that writes is a read
    /// that cannot be replayed: it would make two identical `recall`s return
    /// different rankings depending on what was asked in between, and take the
    /// eval baselines with it.
    ///
    /// At most one name is learned per question. The candidate terms of a
    /// sentence overlap heavily -- "quanto custa o produto brasilia" offers
    /// `produto_brasilia`, `do_produto`, `produto` and `brasilia`, all of which
    /// clear the cosine floor against the right entity -- and keeping only the
    /// best one is what stops a single question from stamping four half-names
    /// onto an entity. The runners-up are not lost so much as deferred: if
    /// `brasilia` is really how people ask, a question that offers it *without*
    /// the full name will teach it then.
    pub fn learn_alias(&self, question: &str, hits: &[Hit]) -> Result<Option<Learned>> {
        let conn = self.store().conn();

        let Some(first) = is_decisive(hits) else {
            return Ok(None);
        };
        let entity_id = require_entity(conn, &first.entity_key, &first.entity_key)?;
        let label = entity_label(conn, entity_id)?;

        let candidates = self.eligible_terms(conn, question)?;
        if candidates.is_empty() {
            return Ok(None);
        }

        // One embedding of the label, compared against each candidate. The
        // embedder normalizes, so cosine is a plain dot product.
        let label_vec = self.embedder().embed_one(&label)?;
        let mut best: Option<(f32, String)> = None;
        for term in candidates {
            // The key is a storage form; `_` stands for whatever separator the
            // question actually used, so the surface form is what gets embedded.
            let surface = term.replace('_', " ");
            let cos = dot(&label_vec, &self.embedder().embed_one(&surface)?);
            let better = match &best {
                // Ties break on the term itself, so learning is reproducible
                // rather than dependent on candidate order.
                Some((c, t)) => cos > *c || (cos == *c && &term < t),
                None => true,
            };
            if better {
                best = Some((cos, term));
            }
        }

        let Some((cos, alias_key)) = best.filter(|(c, _)| *c >= MIN_ALIAS_COSINE) else {
            return Ok(None);
        };
        let weight = (f64::from(cos)).min(LEARNED_MAX_WEIGHT);

        // A name that points at two things is not a name. When the same term has
        // already been learned for a different entity, the stronger reading wins
        // outright instead of both surviving -- keeping both would hand the alias
        // channel, whose entire value is precision, a term it cannot resolve.
        // Comparing stored weights (not recency) is what keeps this from
        // oscillating: the same question always reaches the same verdict.
        if let Some((other_id, other_weight)) = learned_holder(conn, &alias_key)?
            && other_id != entity_id
        {
            if other_weight >= weight {
                return Ok(None);
            }
            conn.execute(
                "DELETE FROM entity_alias WHERE alias_key = ? AND entity_id = ?",
                params![alias_key, other_id],
            )?;
        }

        conn.execute(
            "INSERT INTO entity_alias(alias_key, entity_id, source, weight)
             VALUES (?, ?, 'learned', ?)
             ON CONFLICT(alias_key, entity_id) DO UPDATE SET weight = excluded.weight
             WHERE entity_alias.source = 'learned'",
            params![alias_key, entity_id, weight],
        )?;

        Ok(Some(Learned {
            alias: alias_key,
            entity: label,
            entity_key: first.entity_key.clone(),
            weight,
        }))
    }

    /// The terms of a question that are free to become a name.
    ///
    /// Anything the brain can already resolve is excluded, for one reason each:
    /// an existing entity key or declared alias already names something, and
    /// pointing it elsewhere would make one word mean two things; a predicate key
    /// is what is being *asked about* rather than what is being asked *of*, and
    /// letting "preco" become a name for whichever product was asked about first
    /// would pin every later price question to that product.
    ///
    /// A term is disqualified when any of its *words* is taken, not only when the
    /// whole term is -- because a name that contains a name is not a new name.
    /// Without that, excluding `pgbouncer` merely promoted `do_pgbouncer`: the
    /// same name with a preposition welded on, scoring almost as well against the
    /// label and meaning nothing. The rule keeps `produto_brasilia`, whose words
    /// name nothing on their own, and drops `a_porta` and `do_pgbouncer`.
    ///
    /// Learned aliases are *not* excluded, so a later question re-scores one
    /// rather than being blocked by the brain's own earlier guess.
    fn eligible_terms(&self, conn: &Connection, question: &str) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for term in normalized_terms(question) {
            let mut free = true;
            for word in term.split('_') {
                let taken: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM entity WHERE key = ?1)
                         OR EXISTS(SELECT 1 FROM entity_alias WHERE alias_key = ?1
                                     AND source = 'declared')
                         OR EXISTS(SELECT 1 FROM predicate WHERE key = ?1)",
                    params![word],
                    |r| r.get(0),
                )?;
                if taken {
                    free = false;
                    break;
                }
            }
            if free {
                out.push(term);
            }
        }
        Ok(out)
    }
}

/// Cosine of two vectors the embedder already normalized.
fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

pub(crate) fn aliases_of(conn: &Connection, entity_id: i64) -> Result<Vec<Alias>> {
    let mut stmt = conn.prepare(
        "SELECT alias_key, source, weight FROM entity_alias
         WHERE entity_id = ? ORDER BY weight DESC, alias_key",
    )?;
    let rows = stmt.query_map(params![entity_id], |r| {
        let source: String = r.get(1)?;
        Ok(Alias {
            key: r.get(0)?,
            source: AliasSource::parse(&source).unwrap_or(AliasSource::Learned),
            weight: r.get(2)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The entity a key names, by its own key or by any alias.
fn owner_of(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(conn
        .query_row(
            "SELECT id FROM entity WHERE key = ?1
             UNION ALL
             SELECT entity_id FROM entity_alias WHERE alias_key = ?1
             LIMIT 1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
}

/// The entity currently holding a key as a *learned* alias, if any.
fn learned_holder(conn: &Connection, alias_key: &str) -> Result<Option<(i64, f64)>> {
    Ok(conn
        .query_row(
            "SELECT entity_id, weight FROM entity_alias
             WHERE alias_key = ? AND source = 'learned' LIMIT 1",
            params![alias_key],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

fn require_entity(conn: &Connection, key: &str, given: &str) -> Result<i64> {
    crate::brain::find_entity(conn, key)?.ok_or_else(|| BrainError::NoSuchEntity {
        name: given.to_string(),
    })
}

fn entity_label(conn: &Connection, id: i64) -> Result<String> {
    Ok(
        conn.query_row("SELECT label FROM entity WHERE id = ?", params![id], |r| {
            r.get(0)
        })?,
    )
}

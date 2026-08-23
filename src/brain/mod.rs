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

/// A question about a *set*: which subjects hold this predicate, optionally with
/// this value, at some instant.
///
/// The counterpart to [`crate::recall::RecallQuery`], and deliberately not a
/// variant of it. `recall` answers with what is *relevant* -- it ranks, it guesses
/// at intent, and it truncates -- which is right for a question and wrong for a
/// list. This answers with what *matches*, and reports how much matched so the
/// caller can tell a complete list from the top of one.
#[derive(Debug, Clone)]
pub struct WhichQuery {
    pub predicate: String,
    /// The value to match, or `None` for every subject holding the predicate at
    /// all. A literal is matched exactly, the way it was written.
    pub value: Option<Object>,
    pub when: When,
    pub order: Order,
    pub desc: bool,
    pub limit: usize,
    pub scope: Option<String>,
}

impl WhichQuery {
    pub fn new(predicate: impl Into<String>) -> Self {
        Self {
            predicate: predicate.into(),
            value: None,
            when: When::Now,
            order: Order::Subject,
            desc: false,
            limit: 200,
            scope: None,
        }
    }

    pub fn value(mut self, o: Object) -> Self {
        self.value = Some(o);
        self
    }

    pub fn when(mut self, w: When) -> Self {
        self.when = w;
        self
    }

    pub fn order(mut self, o: Order) -> Self {
        self.order = o;
        self
    }

    pub fn desc(mut self, yes: bool) -> Self {
        self.desc = yes;
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
}

/// What a set answer is sorted by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Order {
    /// The subject's identity key. The default, because a list of things reads
    /// best in the order the things are named.
    Subject,
    /// The object. Numbers sort numerically and text lexicographically, which is
    /// what makes an ISO-8601 date usable as a deadline without a date type.
    Value,
    /// When each fact became true, oldest first.
    Since,
}

impl Order {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Subject => "subject",
            Self::Value => "value",
            Self::Since => "since",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "subject" => Some(Self::Subject),
            "value" => Some(Self::Value),
            "since" => Some(Self::Since),
            _ => None,
        }
    }

    /// The sort keys, in order. `f.id` is appended by the caller and is always
    /// ascending, so equal keys never reorder between runs.
    fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Subject => &["e.key"],
            Self::Value => &["f.object_num", "f.object_text"],
            Self::Since => &["f.valid_from"],
        }
    }
}

/// The facts a set question matched, and how many there were.
///
/// `matched` is the whole point of this type. A vector store can always return
/// ten things and can never tell you whether ten was all of them; an agent
/// deciding what to do next needs to know the difference between "these are the
/// open tasks" and "these are ten of the open tasks".
#[derive(Debug, Clone, Serialize)]
pub struct WhichAnswer {
    pub facts: Vec<Fact>,
    /// How many facts satisfied the query, ignoring `limit`.
    pub matched: i64,
    /// Whether `limit` cut the answer short.
    pub truncated: bool,
}

impl WhichAnswer {
    fn empty() -> Self {
        Self {
            facts: Vec::new(),
            matched: 0,
            truncated: false,
        }
    }
}

/// A search for a *string*, rather than for a question or for a set.
///
/// The third shape, and the one the other two cannot cover. [`RecallQuery`]
/// answers a question: it ranks, it guesses at intent, and it cannot say what it
/// left out. [`WhichQuery`] answers about a set, but only starting from a
/// predicate key the caller already knows. Neither can answer *"which records
/// mention this"* -- which is what anyone gathering evidence actually asks, and
/// what sends an agent to open the SQLite file by hand when it is missing.
///
/// [`RecallQuery`]: crate::recall::RecallQuery
#[derive(Debug, Clone)]
pub struct FindQuery {
    /// The needle, taken literally. Not a bag of words: "context reset" finds
    /// that phrase, not every fact mentioning either word.
    pub needle: String,
    pub when: When,
    pub limit: usize,
    pub scope: Option<String>,
    pub not_scope: Option<String>,
}

impl FindQuery {
    pub fn new(needle: impl Into<String>) -> Self {
        Self {
            needle: needle.into(),
            when: When::Now,
            // `which`'s default, for `which`'s reason: a list that silently stops
            // at ten is worse than no list.
            limit: 200,
            scope: None,
            not_scope: None,
        }
    }

    pub fn when(mut self, w: When) -> Self {
        self.when = w;
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

    pub fn not_scope(mut self, s: impl Into<String>) -> Self {
        self.not_scope = Some(s.into());
        self
    }
}

/// Which stage of [`Brain::find`] surfaced a record.
///
/// Reported for the same reason [`crate::recall::Hit`] reports its channels: a
/// result nobody can explain is a result nobody can trust. It is also the fastest
/// way to see *why* a needle matched more than expected -- a `term` hit on a
/// stemmed word looks like a false positive until you know it was the index and
/// not the scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// The FTS5 index matched a whole token or a token prefix. Accent- and
    /// case-insensitive, and stemmed: `normalize` finds `normalized`.
    Term,
    /// A folded substring scan matched. The only stage that finds a fragment from
    /// the middle of a token, which is what identifiers are made of.
    Fragment,
}

/// One record a search matched.
///
/// Serializes as a fact with one extra field, so anything already reading
/// `which`'s facts reads these unchanged.
#[derive(Debug, Clone, Serialize)]
pub struct FindHit {
    #[serde(flatten)]
    pub fact: Fact,
    /// Every stage that found it, sorted so output never varies by run.
    pub via: Vec<Via>,
}

/// One property this brain records, and how much it holds under it.
///
/// The brain's ontology is learned rather than declared, so the only way to know
/// what a brain can be asked about is to ask it. Without this read, `which`
/// requires a predicate key the caller has to already know, and an agent that
/// does not know one has no move left inside the tool.
#[derive(Debug, Clone, Serialize)]
pub struct PredicateInfo {
    pub key: String,
    pub cardinality: Cardinality,
    /// Whether its object names another entity. A `false` here on something that
    /// obviously names a thing is the defect `brain lint` reports: it stores and
    /// reads back perfectly and no walk of the graph can follow it.
    pub relational: bool,
    /// Facts currently holding under it.
    pub facts: i64,
    /// Distinct subjects those facts are about. A predicate with many facts and
    /// one subject is a timeline; with many of both, it is a set worth `which`.
    pub subjects: i64,
}

/// What a search matched, and how much of it came back.
#[derive(Debug, Clone, Serialize)]
pub struct FindAnswer {
    pub facts: Vec<FindHit>,
    /// How many records matched, ignoring `limit`. The number `recall`
    /// structurally cannot give.
    pub matched: i64,
    pub truncated: bool,
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

    #[error("a fact cannot stop being true before it starts: from {from}, until {until}")]
    EmptyInterval { from: Timestamp, until: Timestamp },

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
    /// When it stops being true, if the claim says so itself. Carried here
    /// rather than read off the assertion because it has been rounded to what
    /// the store keeps -- see [`stored`].
    valid_to: Option<Timestamp>,
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

    /// The instant this brain calls now.
    ///
    /// The clock itself stays private: a caller that could swap it mid-life could
    /// make one brain answer two questions about time.
    pub(crate) fn now(&self) -> Timestamp {
        self.clock.now()
    }

    fn conn(&self) -> &Connection {
        self.store.conn()
    }

    // --- writing -------------------------------------------------------------

    /// Records a claim, deciding for itself whether that supersedes, corrects or
    /// merely reinforces what is already known.
    pub fn remember(&self, a: &Assertion) -> Result<Outcome> {
        // One transaction for the whole decision: a rejected or failed write must
        // leave no trace, including no half-closed predecessor.
        let tx = self.conn().unchecked_transaction()?;
        let outcome = self.write(&tx, a, self.clock.now())?;
        tx.commit()?;
        Ok(outcome)
    }

    /// Records many claims as one write.
    ///
    /// Two properties, and both are the reason this exists rather than a loop
    /// around [`Brain::remember`] in the caller.
    ///
    /// **One transaction.** A batch that fails writes nothing at all. The caller
    /// of a batch is nearly always a machine -- a hook flushing what a session
    /// learned, an importer replaying a file -- and a machine that half-wrote
    /// cannot tell which half. Retrying an all-or-nothing batch is safe; retrying
    /// a partial one is a guess.
    ///
    /// **One instant.** Every claim in the batch is recorded at the same `now`,
    /// so two claims about the same subject and predicate with no `--at` between
    /// them land on the same instant and the later one *corrects* the earlier.
    /// Reading the clock per claim would instead close the first a microsecond
    /// after opening it, and the history would be honest about nothing except how
    /// fast the loop ran.
    pub fn remember_all(&self, assertions: &[Assertion]) -> Result<Vec<Outcome>> {
        let tx = self.conn().unchecked_transaction()?;
        let now = self.clock.now();
        let mut outcomes = Vec::with_capacity(assertions.len());
        for a in assertions {
            outcomes.push(self.write(&tx, a, now)?);
        }
        tx.commit()?;
        Ok(outcomes)
    }

    /// One claim, inside a transaction somebody else owns and against an instant
    /// somebody else read.
    fn write(&self, tx: &Connection, a: &Assertion, now: Timestamp) -> Result<Outcome> {
        if let Some(c) = a.confidence
            && !(0.0..=1.0).contains(&c)
        {
            return Err(BrainError::InvalidConfidence(c));
        }
        let subject_key = require_key("subject", &a.subject)?;
        let predicate_key = require_key("predicate", &a.predicate)?;

        // Rounded before anything is compared, and before anything is written.
        // See [`stored`]: the clock reads finer than the store keeps, and an
        // instant that survives the round trip differently from the one held in
        // memory makes "the same moment" a question with two answers.
        let now = stored(now);
        let valid_from = stored(a.valid_from.unwrap_or(now));
        let valid_to = a.valid_to.map(stored);

        // Rejected here rather than left to the schema's CHECK, so the caller is
        // told what is wrong with the claim instead of which constraint tripped.
        // An interval shorter than a microsecond lands here too, now that both
        // ends have been rounded: it is empty in the only timeline that exists.
        if let Some(until) = valid_to
            && until <= valid_from
        {
            return Err(BrainError::EmptyInterval {
                from: valid_from,
                until,
            });
        }

        let entity_id = upsert_entity(tx, &subject_key, &a.subject, now)?;
        let shape = upsert_predicate(
            tx,
            &predicate_key,
            a.cardinality,
            matches!(&a.object, Object::Entity(_)),
        )?;
        let object = resolve_object(tx, &a.object, shape.relational, now)?;
        let cardinality = shape.cardinality;

        let w = Write {
            a,
            entity_id,
            predicate_key: &predicate_key,
            object: &object,
            valid_from,
            valid_to,
            now,
        };

        let outcome = match cardinality {
            Cardinality::Multi => {
                // Nothing supersedes anything here, so the only end this fact can
                // have is the one it was given.
                let id = self.insert_fact(tx, &w, valid_to, false)?;
                Outcome::Created(load_fact(tx, id)?)
            }
            Cardinality::Single => self.place_single(tx, &w)?,
        };

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

            // "Still true, and now true for another hour" is a reinforcement of
            // the same claim, so a reassertion may push the end back -- which is
            // what makes a self-expiring fact refreshable instead of having to be
            // rewritten. It never pulls the end in, and it never moves the start:
            // shortening on reassert would let a heartbeat quietly kill the thing
            // it was keeping alive.
            if let Some(until) = w.valid_to {
                // Capped at whatever starts next, for the same reason a new fact
                // is: an extension running past the following claim would make two
                // facts true at once, which is the one thing this timeline exists
                // to prevent. A fact already closed by its successor therefore
                // cannot be extended at all, which is correct -- it did end.
                let end = live
                    .iter()
                    .filter(|f| f.valid_from > covering.valid_from)
                    .map(|f| f.valid_from)
                    .min()
                    .map_or(until, |next| until.min(next));
                if covering.valid_to.is_some_and(|to| to < end) {
                    tx.execute(
                        "UPDATE fact SET valid_to = ? WHERE id = ?",
                        params![micros(end), covering.id],
                    )?;
                    // It may have been written already over, in which case the
                    // index was told so and now has to be told otherwise.
                    index::mark_open(tx, covering.id)?;
                }
            }
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

        // The earlier of what the claim says about itself and where the next claim
        // begins. `--until` narrows and never widens: a fact cannot outlive the one
        // that follows it just because its author thought it would.
        let end = match (w.valid_to, successor.as_ref().map(|s| s.valid_from)) {
            (Some(until), Some(next)) => Some(until.min(next)),
            (until, None) => until,
            (None, next) => next,
        };
        let new_id = self.insert_fact(tx, w, end, true)?;

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
            // A fact that carries its own end is still a candidate until it
            // reaches it. Reading this as `valid_to.is_none()` would hide every
            // self-expiring fact from the semantic channel for the whole of its
            // life -- findable by its words, unfindable by its meaning.
            valid_to.is_none_or(|to| to > w.now),
            w.a.scope.as_deref(),
            self.embedder.model_id(),
        )?;
        Ok(id)
    }

    /// Rebuilds the derived vector index from the stored embeddings.
    pub fn reindex(&self) -> Result<usize> {
        let tx = self.conn().unchecked_transaction()?;
        let n = index::rebuild(&tx, self.clock.now())?;
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

    /// The value that holds now.
    ///
    /// "Now" is the instant the question is asked, so this is exactly
    /// [`Brain::as_of`] against the clock. It used to mean something subtly
    /// different -- the latest fact nobody had closed -- and the two readings
    /// disagree in both directions once a fact can be dated in the future or carry
    /// its own end: a price announced for next year is the newest thing known and
    /// is not today's price, and a freeze that lifted on Friday is still the newest
    /// thing known on Saturday. One meaning of now, or `get` and `as_of(now)`
    /// answer the same question differently.
    pub fn current(&self, subject: &str, predicate: &str) -> Result<Option<Fact>> {
        Ok(self.current_all(subject, predicate)?.into_iter().next())
    }

    /// Every fact holding now, which is more than one only for a multi-valued
    /// predicate.
    pub fn current_all(&self, subject: &str, predicate: &str) -> Result<Vec<Fact>> {
        let Some(entity_id) = find_entity(self.conn(), &norm::key(subject))? else {
            return Ok(Vec::new());
        };
        let now = micros(self.clock.now());
        query_facts(
            self.conn(),
            &format!(
                "{SELECT_FACT} WHERE f.entity_id = ? AND f.predicate = ?
                   AND f.retracted_at IS NULL
                   AND f.valid_from <= ?
                   AND (f.valid_to IS NULL OR ? < f.valid_to)
                 ORDER BY f.valid_from, f.id"
            ),
            params![entity_id, norm::key(predicate), now, now],
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

    /// Which subjects hold a predicate, and optionally hold it with a given value.
    ///
    /// The read every other one here cannot do: it starts from a predicate instead
    /// of from a name. Everything above resolves a subject through [`find_entity`]
    /// first, which answers "what is this thing's status" and can never answer
    /// "which things are open".
    ///
    /// Two details are load-bearing:
    ///
    /// - **A literal matches on `object_text`, edge or not.** An entity-valued
    ///   object carries its label in that column too, so a selection keeps working
    ///   across the moment somebody converts a predicate from strings into real
    ///   relations. [`crate::kin`] relies on the same property for the same reason.
    /// - **"Now" is the instant the question is asked**, not "the row nobody has
    ///   closed yet". A fact dated next week is the newest thing known and is not
    ///   true today; a list of what holds now must not contain it.
    pub fn which(&self, q: &WhichQuery) -> Result<WhichAnswer> {
        let conn = self.conn();
        let predicate = require_key("predicate", &q.predicate)?;

        // Resolved before the query runs, so a name the brain does not know is an
        // empty answer rather than a filter that matches everything.
        let (value_sql, value_bind): (&str, Option<rusqlite::types::Value>) = match &q.value {
            None => ("", None),
            Some(Object::Text(s)) => (" AND f.object_text = ?", Some(s.clone().into())),
            Some(Object::Num { value, .. }) => (" AND f.object_num = ?", Some((*value).into())),
            Some(Object::Entity(name)) => match find_entity(conn, &norm::key(name))? {
                Some(id) => (" AND f.object_entity_id = ?", Some(id.into())),
                None => return Ok(WhichAnswer::empty()),
            },
        };

        let filter = TemporalFilter::for_when(q.when, self.clock.now(), q.scope.clone());

        let mut binds: Vec<rusqlite::types::Value> = vec![predicate.into()];
        binds.extend(value_bind);
        let temporal_sql = filter.bind(&mut binds);
        let where_sql = format!(" WHERE f.predicate = ?{value_sql}{temporal_sql}");

        // Counted separately rather than inferred from the rows, because the
        // whole difference between this and `recall` is being able to say how
        // much was left out.
        let matched: i64 = conn.query_row(
            &format!("SELECT count(*) FROM fact f{where_sql}"),
            rusqlite::params_from_iter(binds.iter()),
            |r| r.get(0),
        )?;

        let dir = if q.desc { " DESC" } else { "" };
        let order_sql = q
            .order
            .keys()
            .iter()
            .map(|k| format!("{k}{dir}"))
            .collect::<Vec<_>>()
            .join(", ");

        binds.push((q.limit as i64).into());
        let mut stmt = conn.prepare(&format!(
            "{SELECT_FACT}{where_sql} ORDER BY {order_sql}, f.id LIMIT ?"
        ))?;
        let mut facts = Vec::new();
        for row in stmt.query_map(rusqlite::params_from_iter(binds), row_to_fact)? {
            facts.push(row??);
        }

        Ok(WhichAnswer {
            truncated: matched > facts.len() as i64,
            matched,
            facts,
        })
    }

    /// The properties this brain records, most-used first.
    ///
    /// Counted against what holds *now*, for the same reason `which` defaults to
    /// now: a predicate whose every fact was superseded years ago is part of the
    /// history, not part of what this brain is currently about.
    ///
    /// Ordered by weight rather than alphabetically. The first question anyone
    /// has of an unfamiliar brain is what it is mostly made of, and an
    /// alphabetical list answers that only by accident. The key breaks ties, so
    /// two predicates of equal size never swap between runs.
    pub fn predicates(&self) -> Result<Vec<PredicateInfo>> {
        let now = micros(self.clock.now());
        let mut stmt = self.conn().prepare(
            "SELECT p.key, p.cardinality, p.relational,
                    count(f.id), count(DISTINCT f.entity_id)
             FROM predicate p
             LEFT JOIN fact f ON f.predicate = p.key
               AND f.retracted_at IS NULL
               AND f.valid_from <= ?1 AND (f.valid_to IS NULL OR ?1 < f.valid_to)
             GROUP BY p.key
             ORDER BY count(f.id) DESC, p.key",
        )?;
        let rows = stmt.query_map([now], |r| {
            Ok(PredicateInfo {
                key: r.get(0)?,
                cardinality: Cardinality::parse(&r.get::<_, String>(1)?)
                    .unwrap_or(Cardinality::Single),
                relational: r.get(2)?,
                facts: r.get(3)?,
                subjects: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every record whose text contains a string.
    ///
    /// Two stages, unioned, because they fail in opposite directions and neither
    /// alone is "search the content":
    ///
    /// - **Term**, through `fact_fts`. Indexed, and forgiving in the ways the
    ///   index already is -- `unicode61 remove_diacritics 2` folds accents and
    ///   case, `porter` folds morphology, and a trailing `*` makes it a prefix,
    ///   so `FEEDBACK` finds `FEEDBACK25-PRIDAYFARELYA`. What it cannot do is see
    ///   inside a token: FTS5 indexes tokens, and nothing in it can find `PRIDAY`
    ///   in the middle of one.
    /// - **Fragment**, a folded `LIKE` scan. Exactly the case the index cannot
    ///   serve, at the cost of reading every fact. That bound is real and stated
    ///   rather than hidden: it is linear in the size of the brain, negligible at
    ///   the hundreds-to-tens-of-thousands of facts one of these holds, and the
    ///   upgrade path if it ever stops being negligible is a trigram index rather
    ///   than a cleverer scan.
    ///
    /// Both scanned columns are read on purpose, though `search_text` is a
    /// superset of `statement` today by construction. Making a search's
    /// completeness depend on an invariant maintained in a different function is
    /// how a promise like "every record that mentions this" quietly stops being
    /// true, and the cost of not depending on it is one extra fold per row.
    ///
    /// Aliases are deliberately not searched. An alias is a name for a *thing*,
    /// not text in a record, and `entity` and `alias` already answer that
    /// question with the entity rather than with a list of its facts.
    pub fn find(&self, q: &FindQuery) -> Result<FindAnswer> {
        let conn = self.conn();
        let needle = q.needle.trim();
        // An empty needle matches nothing rather than everything. `LIKE '%%'` is
        // true for every row, so the harmless-looking degenerate case is the one
        // that returns the whole brain.
        if needle.is_empty() {
            return Ok(FindAnswer {
                facts: Vec::new(),
                matched: 0,
                truncated: false,
            });
        }

        // Each stage contributes a branch, and a stage with nothing to ask for
        // contributes none. The term stage drops out for a needle FTS5 would
        // tokenize to nothing -- `___`, `!!` -- where an empty MATCH expression
        // is a query-syntax error rather than an empty result.
        let mut stages: Vec<String> = Vec::new();
        let mut binds: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(expr) = fts_prefix_query(needle) {
            stages.push(
                "SELECT fact_fts.rowid AS id, 1 AS term, 0 AS fragment
                 FROM fact_fts WHERE fact_fts MATCH ?"
                    .into(),
            );
            binds.push(expr.into());
        }
        // Aliased even though a `UNION ALL` takes its names from the first branch:
        // when the term stage drops out this *is* the first branch, and the
        // grouping above selects `term` and `fragment` by name.
        stages.push(
            "SELECT f.id AS id, 0 AS term, 1 AS fragment FROM fact f
             WHERE fold(f.search_text) LIKE ? ESCAPE '\\'
                OR fold(f.statement) LIKE ? ESCAPE '\\'"
                .into(),
        );
        let pattern = like_pattern(needle);
        binds.push(pattern.clone().into());
        binds.push(pattern.into());

        // `MAX` over a `UNION ALL`, not a bare `UNION`: a record both stages found
        // is one record that two stages agree on, and it must appear once with
        // both markers rather than twice.
        let hits = format!(
            "WITH hit(id, term, fragment) AS (
               SELECT id, MAX(term), MAX(fragment) FROM ({}) GROUP BY id
             )",
            stages.join(" UNION ALL ")
        );

        // Applied outside the union so both stages are filtered by one definition
        // of which facts count, and by the same one retrieval and traversal use.
        let filter = TemporalFilter::for_when(q.when, self.clock.now(), q.scope.clone())
            .excluding(q.not_scope.clone());

        let mut count_binds = binds.clone();
        let temporal_sql = filter.bind(&mut count_binds);
        let matched: i64 = conn.query_row(
            &format!(
                "{hits} SELECT count(*) FROM fact f
                 JOIN hit ON hit.id = f.id WHERE 1 = 1{temporal_sql}"
            ),
            rusqlite::params_from_iter(count_binds.iter()),
            |r| r.get(0),
        )?;

        filter.bind(&mut binds);
        binds.push((q.limit as i64).into());

        // Subject, then time, then id. Grouping one thing's records together is
        // what makes a page of evidence readable, and the id keeps runs identical.
        //
        // `SELECT_FACT` is wrapped rather than extended because it is a whole
        // `SELECT ... FROM`, and the two markers have to land *after* its columns
        // for `row_to_fact`'s indices to keep meaning what they say. The
        // alternative -- writing the twenty-column list out a second time -- is
        // the version that goes wrong silently when a column is added.
        let mut stmt = conn.prepare(&format!(
            "{hits}
             SELECT s.*, hit.term, hit.fragment
             FROM ({SELECT_FACT} WHERE 1 = 1{temporal_sql}) s
             JOIN hit ON hit.id = s.id
             ORDER BY s.key, s.valid_from, s.id
             LIMIT ?"
        ))?;
        let mut facts = Vec::new();
        for row in stmt.query_map(rusqlite::params_from_iter(binds), |r| {
            let fact = row_to_fact(r)?;
            // The two markers sit immediately after the fact's own columns.
            let term: bool = r.get(20)?;
            let fragment: bool = r.get(21)?;
            Ok(fact.map(|fact| FindHit {
                fact,
                via: [(term, Via::Term), (fragment, Via::Fragment)]
                    .into_iter()
                    .filter_map(|(hit, v)| hit.then_some(v))
                    .collect(),
            }))
        })? {
            facts.push(row??);
        }

        Ok(FindAnswer {
            truncated: matched > facts.len() as i64,
            matched,
            facts,
        })
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
        let filter = TemporalFilter::for_when(when, self.clock.now(), None);

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

/// An instant as the store will keep it.
///
/// `fact.valid_from` is an integer count of microseconds and `Timestamp::now()`
/// is not: on Linux it carries nanoseconds, on macOS it does not. So an instant
/// held in memory and the same instant read back from the store were not equal,
/// and every comparison the write path makes between the two -- *is this the
/// same moment as the fact already there* -- answered differently depending on
/// the platform.
///
/// What that cost: two claims about one subject and predicate at one instant are
/// a correction, and the correction was only recognised where the clock happened
/// to be coarse. Elsewhere the second claim was treated as a *change*, which
/// closed the first one at its own start instant and produced an empty interval
/// -- caught by the schema's `valid_from < valid_to`, so the write failed with a
/// constraint violation rather than doing the wrong thing quietly. It surfaced
/// through `remember --batch`, where every claim shares one instant by design and
/// the collision is therefore certain rather than a coincidence of timing, but
/// the bug was never about batches: two ordinary `remember` calls inside the same
/// microsecond hit it too.
///
/// Rounding at the boundary rather than comparing loosely, because a tolerance
/// would have to be agreed on by every comparison separately and one of them
/// would eventually disagree. There is one timeline here, its resolution is a
/// microsecond, and an instant that cannot be stored is not an instant this brain
/// has an opinion about.
fn stored(t: Timestamp) -> Timestamp {
    from_micros(micros(t))
}

/// The needle as an FTS5 phrase-prefix expression, or `None` when FTS5 would see
/// no tokens in it at all.
///
/// One quoted phrase rather than `OR`-ed terms, because a needle is a literal
/// string: `find "context reset"` is asking for that phrase, and returning every
/// fact mentioning either word would be a different, much less useful question.
/// The trailing `*` makes the last token a prefix, which is what lets `FEEDBACK`
/// reach `FEEDBACK25-PRIDAYFARELYA`.
///
/// Quoting is also what makes the needle safe. Unquoted, a stray `NEAR(`, `AND`
/// or unbalanced quote is FTS5 syntax, and the user's search comes back as a
/// query-syntax error they cannot act on.
fn fts_prefix_query(needle: &str) -> Option<String> {
    let tokens: Vec<&str> = needle
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return None;
    }
    Some(format!("\"{}\"*", tokens.join(" ")))
}

/// The needle as a `LIKE` pattern matching it anywhere, folded the way
/// [`norm::fold`] folds the column.
///
/// Escaping is not a nicety here. `_` is a single-character wildcard in `LIKE`,
/// and identifiers are made of underscores -- unescaped, `_normalize_email`
/// would match `xnormalizexemail` and every other near-miss, which is precisely
/// wrong for the one search whose job is to be literal.
fn like_pattern(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 2);
    out.push('%');
    for c in norm::fold(needle).chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
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
    object_names_entity: bool,
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

    // The write in hand counts as evidence, not only the ones before it.
    // [`is_relational`] looks at stored facts, so on the very first edge under a
    // predicate it answers "no" and the flag lands wrong -- exactly contradicting
    // the rule it documents, that one entity-valued fact settles what a predicate
    // is. It self-corrected on the second write, which meant a predicate with a
    // single relation read as an attribute in `lint` and in the studio forever.
    let relational = match (was_declared, stored_relational) {
        (true, Some(r)) => r,
        _ => object_names_entity || is_relational(conn, key)?,
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

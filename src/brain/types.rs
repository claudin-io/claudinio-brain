//! The vocabulary of the fact model.

use jiff::Timestamp;
use serde::Serialize;
use uuid::Uuid;

/// How many values a predicate holds at once. This *is* the supersession rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Cardinality {
    /// One value at a time. A new value closes the previous one.
    Single,
    /// Values coexist. Only an explicit retraction removes one.
    Multi,
}

impl Cardinality {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Multi => "multi",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "single" => Some(Self::Single),
            "multi" => Some(Self::Multi),
            _ => None,
        }
    }
}

/// What a fact says about its subject.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    Text(String),
    Num {
        value: f64,
        unit: Option<String>,
    },
    /// Another entity. A fact with an entity object *is* a graph edge.
    Entity(String),
}

impl Object {
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    pub fn num(value: f64) -> Self {
        Self::Num { value, unit: None }
    }

    pub fn entity(s: impl Into<String>) -> Self {
        Self::Entity(s.into())
    }

    pub fn with_unit(self, unit: impl Into<String>) -> Self {
        match self {
            Self::Num { value, .. } => Self::Num {
                value,
                unit: Some(unit.into()),
            },
            other => other,
        }
    }

    /// How the object reads inside a statement.
    pub fn display(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Entity(s) => s.clone(),
            Self::Num { value, unit } => match unit {
                Some(u) => format!("{value} {u}"),
                None => value.to_string(),
            },
        }
    }
}

/// A claim being written into the brain.
#[derive(Debug, Clone)]
pub struct Assertion {
    pub subject: String,
    pub predicate: String,
    pub object: Object,
    /// When this became true. Defaults to the clock's now.
    pub valid_from: Option<Timestamp>,
    pub source: Option<String>,
    pub locator: Option<serde_json::Value>,
    pub scope: Option<String>,
    pub confidence: Option<f64>,
    /// Declares the predicate's cardinality if it is not yet known.
    pub cardinality: Option<Cardinality>,
}

impl Assertion {
    pub fn new(subject: impl Into<String>, predicate: impl Into<String>, object: Object) -> Self {
        Self {
            subject: subject.into(),
            predicate: predicate.into(),
            object,
            valid_from: None,
            source: None,
            locator: None,
            scope: None,
            confidence: None,
            cardinality: None,
        }
    }

    pub fn at(mut self, t: Timestamp) -> Self {
        self.valid_from = Some(t);
        self
    }

    pub fn source(mut self, s: impl Into<String>) -> Self {
        self.source = Some(s.into());
        self
    }

    pub fn locator(mut self, v: serde_json::Value) -> Self {
        self.locator = Some(v);
        self
    }

    pub fn scope(mut self, s: impl Into<String>) -> Self {
        self.scope = Some(s.into());
        self
    }

    pub fn confidence(mut self, c: f64) -> Self {
        self.confidence = Some(c);
        self
    }

    pub fn cardinality(mut self, c: Cardinality) -> Self {
        self.cardinality = Some(c);
        self
    }
}

/// A stored fact, with both time axes.
#[derive(Debug, Clone, Serialize)]
pub struct Fact {
    pub id: i64,
    pub uuid: Uuid,
    pub entity: String,
    pub entity_key: String,
    pub predicate: String,

    pub object_text: Option<String>,
    pub object_num: Option<f64>,
    pub object_entity: Option<String>,
    pub unit: Option<String>,
    pub statement: String,

    /// When it became true in the world.
    pub valid_from: Timestamp,
    /// When it stopped being true. `None` means it still holds.
    pub valid_to: Option<Timestamp>,
    /// When the brain learned it.
    pub recorded_at: Timestamp,
    /// Set when we learn it was *never* true -- distinct from having ended.
    pub retracted_at: Option<Timestamp>,
    pub superseded_by: Option<i64>,

    pub confidence: f64,
    pub reassert_count: i64,
    pub scope: Option<String>,
    pub source: Option<String>,
    pub locator: Option<serde_json::Value>,
}

impl Fact {
    pub fn number(&self) -> Option<f64> {
        self.object_num
    }

    pub fn object_entity_label(&self) -> Option<String> {
        self.object_entity.clone()
    }

    /// True while this fact is still believed and has not been closed.
    pub fn is_open(&self) -> bool {
        self.valid_to.is_none() && self.retracted_at.is_none()
    }

    /// Whether this fact answers a question asked about instant `t`.
    pub fn covers(&self, t: Timestamp) -> bool {
        self.retracted_at.is_none() && self.valid_from <= t && self.valid_to.is_none_or(|to| t < to)
    }
}

/// What a write actually did. Callers -- and agents -- need to tell these apart.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// A first value, or one that fills a gap in the timeline.
    Created(Fact),
    /// The same value asserted again: reinforced rather than duplicated.
    Reasserted(Fact),
    /// A different value: the previous one was closed, not deleted.
    Superseded { closed: Fact, created: Fact },
    /// A correction at the same instant: the previous claim was never true.
    Corrected { retracted: Fact, created: Fact },
}

impl Outcome {
    /// The fact now carrying the value, whichever path was taken.
    pub fn fact(&self) -> &Fact {
        match self {
            Self::Created(f) | Self::Reasserted(f) => f,
            Self::Superseded { created, .. } | Self::Corrected { created, .. } => created,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Created(_) => "created",
            Self::Reasserted(_) => "reasserted",
            Self::Superseded { .. } => "superseded",
            Self::Corrected { .. } => "corrected",
        }
    }
}

/// Where a fact came from and what became of it.
#[derive(Debug, Clone, Serialize)]
pub struct Provenance {
    pub fact: Fact,
    /// The fact that replaced this one, if any.
    pub superseded_by: Option<Fact>,
    /// The fact this one replaced, if any.
    pub supersedes: Option<Fact>,
}

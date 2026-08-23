//! Reading many facts at once.
//!
//! A batch is what a machine writes: a hook flushing what a session learned, an
//! importer replaying a file someone else produced. That changes what the format
//! has to be careful about. A person mistyping `--sujeito` gets an error from
//! clap and fixes it; a generator emitting `"sujeito"` into a JSON object gets
//! whatever the parser decides to do with a key it did not expect, and if that
//! is *nothing* the fact is silently born without a subject -- or, worse,
//! without the `source` that made it reviewable.
//!
//! So: every key is known or the line is rejected, and the rejection names the
//! line. Nothing is written until every line has parsed, which is the same rule
//! [`crate::brain::Brain::remember_all`] enforces one level down for the write
//! itself.

use crate::brain::{Assertion, Cardinality, Object};
use crate::cli::parse_when;
use serde::Deserialize;

/// One line of a batch: the flags of `remember`, spelled the same way.
///
/// `deny_unknown_fields` is the whole point -- see the module note.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub subject: String,
    pub predicate: String,

    /// A literal. A JSON number stays a number; a JSON string is read the way
    /// `--value` is, so `"30"` is still the number 30 and the two entry points
    /// cannot disagree about what a value is.
    #[serde(default)]
    pub value: Option<serde_json::Value>,

    /// Another entity, making this fact an edge.
    #[serde(default)]
    pub entity: Option<String>,

    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub source: Option<String>,

    /// Where the answer actually lives. An object here rather than the string
    /// `--locator` has to take, because in JSON there is no reason to escape it.
    #[serde(default)]
    pub locator: Option<serde_json::Value>,

    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub cardinality: Option<String>,
}

/// Turns the text of a batch into assertions, or into an error naming the line.
///
/// Both shapes are accepted: one JSON object per line, and a single top-level
/// array. Not politeness -- an array is what anything generating this file
/// produces on the first try, and rejecting it would teach nothing except that
/// the format is fussy. The two are distinguished by the first non-space
/// character, which is the only thing they can be distinguished by without
/// parsing the whole input twice.
///
/// Blank lines are skipped. A line that is only whitespace is not a claim about
/// anything, and a generator that ends its file with a newline is not making an
/// error.
pub fn parse(text: &str) -> anyhow::Result<Vec<Assertion>> {
    let records: Vec<(usize, Record)> = match text.trim_start().starts_with('[') {
        true => serde_json::from_str::<Vec<Record>>(text)
            .map_err(|e| anyhow::anyhow!("batch is not a JSON array of facts: {e}"))?
            .into_iter()
            .enumerate()
            // The array's index, not a line: a pretty-printed array has no
            // meaningful line to point at, and the nth element is what the
            // caller can find.
            .map(|(i, r)| (i + 1, r))
            .collect(),
        false => {
            let mut out = Vec::new();
            for (i, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let r: Record = serde_json::from_str(line)
                    .map_err(|e| anyhow::anyhow!("line {}: {}", i + 1, without_position(&e)))?;
                out.push((i + 1, r));
            }
            out
        }
    };

    records
        .into_iter()
        .map(|(n, r)| assertion(r).map_err(|e| anyhow::anyhow!("line {n}: {e}")))
        .collect()
}

/// Drops serde's own position from a message that already carries one.
///
/// Each line is parsed as its own document, so serde's "at line 1 column 25" is
/// always line 1 -- and printed beside our line 7, it reads as a contradiction.
/// The column would be worth keeping if it survived the line being wrong, and it
/// does not.
fn without_position(e: &serde_json::Error) -> String {
    let msg = e.to_string();
    match msg.find(" at line ") {
        Some(i) => msg[..i].to_string(),
        None => msg,
    }
}

/// One record, validated the way `cmd_remember` validates its flags -- before
/// anything is opened, let alone written.
fn assertion(r: Record) -> anyhow::Result<Assertion> {
    let object = match (r.value, r.entity) {
        (Some(_), Some(_)) => anyhow::bail!("pass `value` or `entity`, not both"),
        (None, None) => anyhow::bail!("pass `value` or `entity`"),
        (_, Some(e)) => Object::entity(e),
        (Some(v), None) => {
            let literal = match v {
                serde_json::Value::Number(n) => match n.as_f64() {
                    Some(f) => Object::num(f),
                    None => anyhow::bail!("`value` is a number this brain cannot hold: {n}"),
                },
                serde_json::Value::String(s) => Object::parse_literal(&s),
                serde_json::Value::Bool(b) => Object::text(b.to_string()),
                other => anyhow::bail!("`value` must be text or a number, got {other}"),
            };
            match &r.unit {
                Some(u) => literal.with_unit(u),
                None => literal,
            }
        }
    };

    let mut a = Assertion::new(r.subject, r.predicate, object);
    a.valid_from = r.at.as_deref().map(parse_when).transpose()?;
    a.valid_to = r.until.as_deref().map(parse_when).transpose()?;
    a.source = r.source;
    a.locator = r.locator;
    a.confidence = r.confidence;
    a.scope = r.scope;
    a.cardinality = match &r.cardinality {
        Some(c) => Some(
            Cardinality::parse(c)
                .ok_or_else(|| anyhow::anyhow!("expected `single` or `multi`, got {c:?}"))?,
        ),
        None => None,
    };
    Ok(a)
}

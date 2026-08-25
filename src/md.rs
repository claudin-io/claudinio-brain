//! A brain as text.
//!
//! `brain export` already wrote a page you can look at. This writes one you can
//! *review*: plain Markdown, greppable, and above all diffable, so a brain can go
//! through the same reading a change to the code goes through. The thing a single
//! binary file cannot do is show up in a pull request as five changed lines.
//!
//! Two decisions carry the format, and both come from that.
//!
//! **It is deterministic.** No export timestamp, no ids that renumber, and a
//! fixed order everywhere: entities by key, then predicates by key, then by when
//! the claim opened. Exporting an unchanged brain twice produces byte-identical
//! files, which is the only thing that makes a diff mean "something changed"
//! rather than "somebody ran the command".
//!
//! **It keeps the timeline.** A rendering that shows only what is true now throws
//! away the half of this project that is not a key-value store. Every claim about
//! a subject and predicate is listed in the order it happened, each one marked
//! with what became of it -- still true, ended, or withdrawn as never true. Those
//! are three different events, and a reviewer is exactly the reader who has to
//! tell them apart.
//!
//! What this is not is a second source of truth. Nothing reads Markdown back in:
//! the store stays the brain, and this is a view of it.

use jiff::Timestamp;
use rusqlite::Connection;

/// One claim, flattened for printing.
struct Row {
    entity: String,
    entity_key: String,
    predicate: String,
    statement: String,
    valid_from: i64,
    valid_to: Option<i64>,
    retracted_at: Option<i64>,
    confidence: f64,
    scope: Option<String>,
    source: Option<String>,
    relational: bool,
}

/// Renders the whole brain.
///
/// `label` is the brain's own name rather than the file's, so a copied database
/// still says what it is.
pub fn render(conn: &Connection, label: &str) -> rusqlite::Result<String> {
    let rows = load(conn)?;
    let mut out = String::with_capacity(rows.len() * 96);
    out.push_str(&format!("# {}\n", escape(label)));
    out.push_str(&format!(
        "\n{} facts about {} entities.\n",
        rows.len(),
        rows.iter()
            .map(|r| r.entity_key.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    ));

    let mut entity = String::new();
    let mut predicate = String::new();
    for r in &rows {
        if r.entity_key != entity {
            entity = r.entity_key.clone();
            predicate.clear();
            out.push_str(&format!("\n## {}\n", escape(&r.entity)));
        }
        if r.predicate != predicate {
            predicate = r.predicate.clone();
            out.push_str(&format!("\n### {}\n\n", escape(&r.predicate)));
        }
        out.push_str(&line(r));
    }
    Ok(out)
}

/// One claim as a list item.
///
/// The bullet says what kind of claim it is before it says anything else, because
/// that is the distinction a reader loses first: `now` still holds, `was` held and
/// stopped, `never` was withdrawn as untrue. A list where all three look alike is
/// a list that reads as a pile of contradictions.
fn line(r: &Row) -> String {
    let marker = match (r.retracted_at, r.valid_to) {
        (Some(_), _) => "never",
        (None, Some(_)) => "was",
        (None, None) => "now",
    };
    // Flat, deliberately. Nesting the closed intervals under the live one was the
    // first shape this took, and it is wrong for the order the list is in: history
    // reads forward, so the superseded claim comes first and an indented item that
    // precedes its parent is a sub-list of nothing. The markers carry the
    // distinction without needing the geometry to agree.
    let arrow = match r.relational {
        // An edge, so the object names something the brain also knows -- worth
        // seeing at a glance, since only these are walkable.
        true => "→ ",
        false => "",
    };
    let mut s = format!(
        "- `{marker}` {arrow}{}",
        escape(strip(&r.statement, &r.entity, &r.predicate))
    );
    s.push_str(&format!(" — since {}", date(r.valid_from)));
    if let Some(to) = r.valid_to {
        s.push_str(&format!(" until {}", date(to)));
    }
    // Only what was actually recorded is printed. A field rendered at its default
    // is a field that shows up in every diff and tells a reviewer nothing.
    if r.confidence < 1.0 {
        s.push_str(&format!(" · confidence {:.2}", r.confidence));
    }
    if let Some(sc) = &r.scope {
        s.push_str(&format!(" · scope {}", escape(sc)));
    }
    if let Some(src) = &r.source {
        s.push_str(&format!(" · source {}", escape(src)));
    }
    s.push('\n');
    s
}

/// Drops the subject and predicate from a statement, since both are the headings
/// this line sits under. "auth strategy JWT" under `## auth` / `### strategy` is
/// two thirds noise.
fn strip<'a>(statement: &'a str, entity: &str, predicate: &str) -> &'a str {
    let prefix = format!("{entity} {predicate} ");
    statement.strip_prefix(&prefix).unwrap_or(statement)
}

/// Days rather than instants. A brain's timeline is read by people here, and the
/// microsecond a fact was written is noise in a document meant to be diffed.
fn date(micros: i64) -> String {
    Timestamp::from_microsecond(micros)
        .map(|t| t.strftime("%Y-%m-%d").to_string())
        .unwrap_or_else(|_| micros.to_string())
}

/// Neutralises the handful of characters that would turn recorded text into
/// Markdown structure.
///
/// The same argument as the hook's sanitiser, for a different output: a value is
/// text somebody wrote, and it must not be able to become a heading, a bullet or
/// a code fence in a document somebody else is reading to decide whether the
/// brain is right. Backticks and newlines are the two that matter; the rest is
/// left alone, because escaping every Markdown character makes the common case
/// unreadable to protect against nothing.
fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '`' => out.push('\''),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

/// Every fact, in the order the document prints them.
///
/// Ordered in SQL rather than in Rust so the ordering is one thing rather than
/// two that have to agree. `valid_from` last is what puts a superseded claim
/// directly under the one that replaced it.
fn load(conn: &Connection) -> rusqlite::Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT e.label, e.key, f.predicate, f.statement,
                f.valid_from, f.valid_to, f.retracted_at,
                f.confidence, f.scope, f.source,
                f.object_entity_id IS NOT NULL
         FROM fact f
         JOIN entity e ON e.id = f.entity_id
         ORDER BY e.key, f.predicate, f.valid_from, f.id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Row {
            entity: r.get(0)?,
            entity_key: r.get(1)?,
            predicate: r.get(2)?,
            statement: r.get(3)?,
            valid_from: r.get(4)?,
            valid_to: r.get(5)?,
            retracted_at: r.get(6)?,
            confidence: r.get(7)?,
            scope: r.get(8)?,
            source: r.get(9)?,
            relational: r.get::<_, i64>(10)? != 0,
        })
    })?;
    rows.collect()
}

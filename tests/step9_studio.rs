//! Passo 9: the studio.
//!
//! Two things are being pinned here, and they are different in kind.
//!
//! The snapshot is a *contract*: the browser does its own temporal filtering, so
//! anything the snapshot drops is a question the studio can never answer. Closed
//! intervals and retracted facts have to survive the trip even though no other
//! read path returns them.
//!
//! The server is a *boundary*. It is the only part of `brain` that accepts input
//! from something other than the user's own shell, and the tests below are the
//! ones that would notice if the token check, the `Host` check or the loopback
//! bind ever quietly stopped applying.

use brain::brain::{Assertion, Brain, Object};
use brain::clock::StepClock;
use brain::ids::SeededIdGen;
use brain::studio::Snapshot;
use jiff::Timestamp;
use std::io::{Read, Write};
use std::net::TcpStream;
use tempfile::TempDir;

fn ts(s: &str) -> Timestamp {
    let s = if s.len() == 10 {
        format!("{s}T00:00:00Z")
    } else {
        s.to_string()
    };
    s.parse().expect("valid timestamp")
}

fn fixture() -> (TempDir, Brain) {
    let tmp = TempDir::new().unwrap();
    let brain = Brain::init(
        &tmp.path().join("t.db"),
        "studio-test",
        Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
        Box::new(SeededIdGen::new(1)),
    )
    .unwrap();
    (tmp, brain)
}

fn remember(b: &Brain, subject: &str, predicate: &str, value: &str, at: &str) -> i64 {
    let a = Assertion::new(subject, predicate, Object::text(value)).at(ts(at));
    b.remember(&a).unwrap().fact().id
}

// --- the snapshot ------------------------------------------------------------

/// The whole reason the studio reads its own snapshot rather than calling `get`:
/// a debugger that only sees what is currently true cannot show what changed.
#[test]
fn snapshot_keeps_closed_and_retracted_facts() {
    let (_tmp, b) = fixture();
    remember(&b, "produto", "preco", "20", "2026-01-01");
    remember(&b, "produto", "preco", "25", "2026-06-01");
    let wrong = remember(&b, "produto", "cor", "azul", "2026-01-01");
    b.retract(wrong, Some("never was")).unwrap();

    let snap = Snapshot::capture(&b, false).unwrap();
    assert_eq!(
        snap.facts.len(),
        3,
        "closed and retracted facts must survive"
    );

    let closed = snap
        .facts
        .iter()
        .find(|f| f.object_text.as_deref() == Some("20"))
        .unwrap();
    assert!(
        closed.valid_to.is_some(),
        "the superseded fact keeps its close"
    );
    assert!(
        closed.superseded_by.is_some(),
        "and the pointer to what closed it -- the studio reconstructs \
         transaction-time from the closer's recorded_at"
    );

    let retracted = snap.facts.iter().find(|f| f.id == wrong).unwrap();
    assert!(retracted.retracted_at.is_some());
}

/// Edges are facts, and the browser builds the graph from `object_entity_id`
/// alone. Labels would collapse two entities that happen to print the same.
#[test]
fn snapshot_gives_edges_numeric_endpoints() {
    let (_tmp, b) = fixture();
    b.link("produto", "fornecido_por", "acme", Some(ts("2026-01-01")))
        .unwrap();

    let snap = Snapshot::capture(&b, false).unwrap();
    let edge = snap
        .facts
        .iter()
        .find(|f| f.object_entity_id.is_some())
        .unwrap();

    let src = snap
        .entities
        .iter()
        .find(|e| e.id == edge.entity_id)
        .unwrap();
    let dst = snap
        .entities
        .iter()
        .find(|e| e.id == edge.object_entity_id.unwrap())
        .unwrap();
    assert_eq!(src.key, "produto");
    assert_eq!(dst.key, "acme");
}

#[test]
fn snapshot_carries_both_alias_kinds() {
    let (_tmp, b) = fixture();
    remember(&b, "acme", "pais", "Chile", "2026-01-01");
    b.declare_alias("acme", "ACME Corp").unwrap();

    let snap = Snapshot::capture(&b, false).unwrap();
    let acme = snap.entities.iter().find(|e| e.key == "acme").unwrap();
    let alias = acme.aliases.iter().find(|a| a.key == "acme_corp").unwrap();
    assert_eq!(
        alias.source, "declared",
        "the studio colours declared and learned differently because only one \
         of them can move a fact"
    );
}

#[test]
fn snapshot_says_which_predicates_are_relational() {
    // The half of a predicate's shape the snapshot used to leave out. Cardinality
    // decides whether a value supersedes; this decides whether the graph can be
    // walked through it at all -- and a relation written as a string is the exact
    // defect the studio was built to make visible, so the page has to be told.
    let (_tmp, b) = fixture();
    remember(&b, "acme", "pais", "Chile", "2026-01-01");
    b.link("produto_a", "fornecido_por", "acme", None).unwrap();

    let snap = Snapshot::capture(&b, false).unwrap();
    let by = |k: &str| {
        snap.predicates
            .iter()
            .find(|p| p.key == k)
            .unwrap_or_else(|| panic!("no predicate {k}"))
            .relational
    };
    assert!(
        by("fornecido_por"),
        "an edge's predicate reads as a relation"
    );
    assert!(!by("pais"), "a literal's predicate does not");
}

/// A fact is user data, and user data ends up inside a `<script>` element. A
/// value containing `</script>` must not be able to close it.
#[test]
fn inline_json_cannot_break_out_of_a_script_element() {
    let (_tmp, b) = fixture();
    remember(
        &b,
        "hostile",
        "payload",
        "</script><script>alert(1)</script>",
        "2026-01-01",
    );

    let json = Snapshot::capture(&b, false)
        .unwrap()
        .to_inline_json()
        .unwrap();
    assert!(!json.contains('<'), "no raw `<` may survive: {json}");
    assert!(
        json.contains("\\u003cscript"),
        "escaped rather than dropped: {json}"
    );

    // Still the same data once parsed.
    let back: serde_json::Value = serde_json::from_str(&json).unwrap();
    let facts = back["facts"].as_array().unwrap();
    assert_eq!(
        facts[0]["object_text"].as_str().unwrap(),
        "</script><script>alert(1)</script>"
    );
}

#[test]
fn rendered_page_is_self_contained() {
    let (_tmp, b) = fixture();
    remember(&b, "acme", "pais", "Chile", "2026-01-01");

    let snap = Snapshot::capture(&b, false).unwrap();
    let html = brain::studio::render_page(&snap).unwrap();

    assert!(
        html.contains("var THREE="),
        "three.js is compiled in, not fetched"
    );
    assert!(html.contains("brain-snapshot"), "the data is inlined");
    assert!(
        !html.contains("__BRAIN_"),
        "no placeholder survives rendering"
    );

    // Nothing may be loadable from anywhere. Checking for the absence of every
    // `http` string would be checking the wrong thing -- three.js cites a paper
    // in a comment, and an SVG data URI names the XML namespace -- so the
    // assertion is about the constructs that actually fetch.
    for fetching in [
        "<script src",
        "<link rel=\"stylesheet\"",
        "@import",
        "url(http",
    ] {
        assert!(!html.contains(fetching), "found {fetching:?} in the page");
    }
    assert!(
        html.contains("default-src 'none'"),
        "and the browser is told to enforce it, so the claim is checkable \
         rather than merely made"
    );
}

// --- the server --------------------------------------------------------------

/// Starts a studio on a free port, in its own thread.
///
/// The `Brain` is built inside that thread rather than moved into it: it owns a
/// SQLite connection, and keeping its whole lifetime on one thread is simpler
/// than proving to the compiler that it never crosses.
fn spawn_studio(dir: &std::path::Path) -> (u16, String) {
    let path = dir.join("served.db");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let brain = Brain::init(
            &path,
            "served",
            Box::new(StepClock::new(ts("2026-01-01T00:00:00Z"), 1000)),
            Box::new(SeededIdGen::new(7)),
        )
        .unwrap();
        brain
            .remember(&Assertion::new("acme", "pais", Object::text("Chile")).at(ts("2026-01-01")))
            .unwrap();

        let studio = brain::studio::server::Studio::bind(brain, 0).unwrap();
        let url = studio.url();
        let token = url.split("?t=").nth(1).unwrap().to_string();
        tx.send((studio.port(), token)).unwrap();
        studio.run().unwrap();
    });
    rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap()
}

fn raw(port: u16, request: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.write_all(request.as_bytes()).unwrap();
    let mut buf = Vec::new();
    s.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
    (status, body)
}

fn get(port: u16, path: &str) -> (u16, String) {
    raw(
        port,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"),
    )
}

fn post(port: u16, path: &str, token: &str, body: &str) -> (u16, String) {
    raw(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Brain-Token: {token}\r\n\
             Content-Type: application/json\r\nContent-Length: {len}\r\n\
             Connection: close\r\n\r\n{body}",
            len = body.len()
        ),
    )
}

#[test]
fn page_requires_the_token() {
    let tmp = TempDir::new().unwrap();
    let (port, token) = spawn_studio(tmp.path());

    let (status, _) = get(port, "/");
    assert_eq!(status, 403, "no token, no page");

    let (status, _) = get(port, "/?t=not-the-token");
    assert_eq!(status, 403);

    let (status, body) = get(port, &format!("/?t={token}"));
    assert_eq!(status, 200);
    assert!(body.contains("brain-snapshot"));
}

/// A name that resolves to 127.0.0.1 is same-origin to a browser, so the token
/// would travel with a rebound request. The `Host` check is what stops that, and
/// it is the check most likely to be "simplified" away by someone who reads the
/// loopback bind as sufficient on its own.
#[test]
fn a_non_loopback_host_header_is_refused() {
    let tmp = TempDir::new().unwrap();
    let (port, token) = spawn_studio(tmp.path());

    let (status, _) = raw(
        port,
        &format!(
            "GET /?t={token} HTTP/1.1\r\nHost: brain.attacker.example\r\nConnection: close\r\n\r\n"
        ),
    );
    assert_eq!(status, 403, "the token alone must not be enough");
}

#[test]
fn api_writes_need_the_token_in_a_header() {
    let tmp = TempDir::new().unwrap();
    let (port, token) = spawn_studio(tmp.path());
    let body = r#"{"subject":"acme","predicate":"pais","value":"Peru"}"#;

    // No header: a cross-origin form post is exactly this request.
    let (status, _) = raw(
        port,
        &format!(
            "POST /api/remember HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
             Content-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            len = body.len()
        ),
    );
    assert_eq!(status, 403);

    let (status, out) = post(port, "/api/remember", &token, body);
    assert_eq!(status, 200, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        v["outcome"]["kind"], "superseded",
        "a new value for a single-valued predicate closes the old one, and the \
         studio reports which of the four things happened"
    );
    assert!(
        v["snapshot"]["facts"].as_array().unwrap().len() >= 2,
        "the write answers with the fresh snapshot so the page never guesses"
    );
}

#[test]
fn recall_reports_the_channel_that_found_each_hit() {
    let tmp = TempDir::new().unwrap();
    let (port, token) = spawn_studio(tmp.path());

    let (status, out) = post(port, "/api/recall", &token, r#"{"query":"acme"}"#);
    assert_eq!(status, 200, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let hits = v["hits"].as_array().unwrap();
    assert!(!hits.is_empty());
    assert!(
        hits[0]["channels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "alias"),
        "a question naming an entity outright is what the alias channel is for: {out}"
    );
}

#[test]
fn a_bad_field_is_the_callers_problem_not_a_crash() {
    let tmp = TempDir::new().unwrap();
    let (port, token) = spawn_studio(tmp.path());

    let (status, out) = post(port, "/api/remember", &token, r#"{"predicate":"pais"}"#);
    assert_eq!(status, 400);
    assert!(out.contains("subject"), "{out}");

    let (status, _) = post(port, "/api/nope", &token, "{}");
    assert_eq!(status, 404);
}

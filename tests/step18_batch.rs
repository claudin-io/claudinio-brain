//! Passo 18: writing many facts as one write.
//!
//! Every write before this one was typed by a person: one claim, one command,
//! one chance to notice that the date was wrong. The caller this passo is for is
//! a machine -- a lifecycle hook flushing what a session learned, an importer
//! replaying somebody else's file -- and a machine fails differently. It does not
//! notice a typo, it does not read a warning, and if a write lands halfway it has
//! no way to find out which half.
//!
//! So a batch makes two promises, and this suite is those two promises:
//!
//! - **Nothing lands unless everything parses.** A bad line is an error naming
//!   the line, and the brain is untouched.
//! - **One batch is one instant.** Two claims about the same subject and
//!   predicate, written together with no `at` between them, are a correction --
//!   not a value that was true for the microseconds it took the loop to run.

use assert_cmd::Command;
use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    root: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("xdg/config")).unwrap();
        std::fs::create_dir_all(root.join("xdg/data")).unwrap();
        let s = Self { _tmp: tmp, root };
        s.cmd().args(["init", "--label", "lote"]).assert().success();
        s
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("brain").unwrap();
        c.current_dir(&self.root)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BRAIN_CONFIG_DIR", self.root.join("xdg/config"))
            .env("BRAIN_DATA_DIR", self.root.join("xdg/data"));
        c
    }

    /// Feeds a batch on stdin, the way a hook does.
    fn batch(&self, input: &str) -> std::process::Output {
        self.cmd()
            .args(["remember", "--batch", "-", "--json"])
            .write_stdin(input.to_string())
            .output()
            .unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.cmd().args(args).arg("--json").assert().success();
        serde_json::from_slice(&out.get_output().stdout).unwrap()
    }
}

/// The ordinary case: several claims, one write, one outcome each.
#[test]
fn a_batch_records_every_line() {
    let s = Sandbox::new();
    let out = s.batch(
        r#"{"subject":"auth","predicate":"strategy","value":"JWT","at":"2026-01-01","source":"adr-004"}
{"subject":"api_gateway","predicate":"timeout","value":30,"unit":"s"}
{"subject":"checkout_service","predicate":"owner","entity":"platform-team"}
"#,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["wrote"], 3);
    assert_eq!(body["counts"]["created"], 3);

    // A JSON number stays a number and keeps its unit; `--value 30` and `30` are
    // the same claim written two ways.
    let fact = &s.json(&["get", "api_gateway", "timeout"])["fact"];
    assert_eq!(fact["object_num"], 30.0);
    assert_eq!(fact["unit"], "s");

    // `entity` is an edge, not a string that happens to name one.
    let owner = &s.json(&["get", "checkout_service", "owner"])["fact"];
    assert_eq!(owner["object_entity"], "platform-team");
}

/// A top-level array is what anything generating this file produces first.
#[test]
fn an_array_is_a_batch_too() {
    let s = Sandbox::new();
    let out = s.batch(
        r#"[{"subject":"a","predicate":"p","value":"1"},{"subject":"b","predicate":"p","value":"2"}]"#,
    );
    assert!(out.status.success());
    assert_eq!(s.json(&["which", "p"])["matched"], 2);
}

/// The promise the caller cannot verify for itself: a batch that fails wrote
/// nothing, so retrying it is safe.
#[test]
fn a_bad_line_writes_nothing() {
    let s = Sandbox::new();
    let out = s.batch(
        r#"{"subject":"x","predicate":"y","value":1}
{"subject":"z","predicat":"typo","value":2}
"#,
    );
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("line 2"),
        "the error must name the line: {err}"
    );
    assert!(err.contains("predicat"), "and the key it choked on: {err}");
    // serde's own "at line 1 column N" would contradict the line we just named.
    assert!(
        !err.contains("at line 1"),
        "two line numbers, one error: {err}"
    );

    // The good line above it is not in the brain.
    assert_eq!(s.json(&["which", "y"])["matched"], 0);
}

/// A key nobody defined is a fact quietly missing a field. It is rejected
/// instead, which is the whole reason the format is strict.
#[test]
fn an_unknown_key_is_never_ignored() {
    let s = Sandbox::new();
    let out = s.batch(r#"{"subject":"a","predicate":"p","value":"1","fonte":"adr-004"}"#);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("fonte"));
}

/// One batch is one instant.
///
/// Two claims about the same subject and predicate, neither carrying `at`, are
/// two readings of the same moment: the second corrects the first. Reading the
/// clock per line would instead close the first one a microsecond after opening
/// it, leaving a history that records how fast the loop ran.
#[test]
fn a_batch_is_one_instant() {
    let s = Sandbox::new();
    let out = s.batch(
        r#"{"subject":"port","predicate":"value","value":8080}
{"subject":"port","predicate":"value","value":9090}
"#,
    );
    assert!(out.status.success());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["counts"]["corrected"], 1);

    // Both claims start at the same instant, which is what makes the first a
    // correction rather than a value that held for a microsecond. `history`
    // keeps the retracted one visible -- that is its job -- but it never held.
    let history = s.json(&["history", "port", "value"]);
    let facts = history["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0]["valid_from"], facts[1]["valid_from"]);
    assert!(facts[0]["retracted_at"].is_string());
    assert!(facts[1]["retracted_at"].is_null());
    assert_eq!(
        s.json(&["get", "port", "value"])["fact"]["object_num"],
        9090.0
    );
}

/// A hook that learned nothing still runs. A command that fails when there was
/// nothing to do is a command every caller has to special-case.
#[test]
fn an_empty_batch_is_not_an_error() {
    let s = Sandbox::new();
    let out = s.batch("\n\n");
    assert!(out.status.success());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["wrote"], 0);
}

/// The warning that made `--entity` findable in the first place still fires --
/// once per predicate, not once per fact. A batch of forty owners stored as
/// strings has one problem.
#[test]
fn a_relation_stored_as_a_string_is_still_reported() {
    let s = Sandbox::new();
    s.cmd()
        .args(["link", "checkout_service", "owner", "platform-team"])
        .assert()
        .success();
    let out = s.batch(
        r#"{"subject":"billing","predicate":"owner","value":"platform-team"}
{"subject":"search","predicate":"owner","value":"platform-team"}
"#,
    );
    assert!(out.status.success());
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hints = body["hints"].as_array().unwrap();
    assert_eq!(hints.len(), 1, "one predicate, one hint: {hints:?}");
}

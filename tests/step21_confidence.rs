//! Passo 21: how sure somebody was, on the read path.
//!
//! `confidence` was write-only for the whole life of this project. It was
//! accepted on `remember`, validated, stored, and pushed halfway to 1.0 every
//! time a fact was reasserted -- and then `recall` ranked as if it had never been
//! written. A brain could hold a measured value and a hedge about the same thing
//! and hand back whichever the channels happened to like, which makes the flag
//! decoration rather than a field.
//!
//! What is under test is the rule and its two edges: it orders by recorded
//! certainty, it never removes anything, and it does not exist at all for a brain
//! that does not use the flag.

use assert_cmd::Command;
use serde_json::Value;
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
        s.cmd().args(["init", "--label", "t"]).assert().success();
        s
    }

    fn cmd(&self) -> Command {
        let mut c = Command::cargo_bin("brain").unwrap();
        c.current_dir(&self.root)
            .env("XDG_CONFIG_HOME", self.root.join("xdg/config"))
            .env("XDG_DATA_HOME", self.root.join("xdg/data"))
            .env("HOME", &self.root);
        c
    }

    fn remember(&self, subject: &str, predicate: &str, value: &str, confidence: Option<&str>) {
        let mut c = self.cmd();
        c.args([
            "remember",
            "--subject",
            subject,
            "--predicate",
            predicate,
            "--value",
            value,
        ]);
        if let Some(k) = confidence {
            c.args(["--confidence", k]);
        }
        c.assert().success();
    }

    fn recall(&self, q: &str) -> Value {
        let out = self
            .cmd()
            .args(["--json", "recall", q, "--explain"])
            .output()
            .unwrap();
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

/// The statements a question returned, best first.
fn ranked(v: &Value) -> Vec<String> {
    v.get("hits")
        .or_else(|| v.as_array().map(|_| v))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("hits in {v}"))
        .iter()
        .map(|h| {
            h.pointer("/fact/statement")
                .or_else(|| h.get("statement"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Two claims about the same thing, one of them hedged. The hedge is still an
/// answer -- it is not filtered, and a question with nothing better must still
/// reach it -- but it does not get to go first.
#[test]
fn a_hedge_does_not_outrank_a_measurement() {
    let s = Sandbox::new();
    s.remember("gateway", "timeout_medido", "30", None);
    s.remember("gateway", "timeout_estimado", "45", Some("0.3"));

    let order = ranked(&s.recall("qual o timeout do gateway"));
    let measured = order.iter().position(|l| l.contains("30"));
    let hedged = order.iter().position(|l| l.contains("45"));
    assert!(
        hedged.is_some(),
        "the hedge is still returned, not filtered: {order:?}"
    );
    assert!(measured < hedged, "certainty orders them: {order:?}");
}

/// The factor is the number the writer wrote. Nothing translates it, so `--why`
/// can be read against the record: a fact at 0.3 is multiplied by 0.3.
#[test]
fn the_factor_is_the_recorded_number() {
    let s = Sandbox::new();
    s.remember("cache", "ttl", "300", Some("0.25"));

    let v = s.recall("qual o ttl do cache");
    let body = v.to_string();
    assert!(body.contains("uncertain"), "the rule is named: {body}");
    let factor = v
        .pointer("/hits/0/explain/demotions")
        .and_then(Value::as_array)
        .and_then(|d| {
            d.iter()
                .find(|x| x.get("rule").and_then(Value::as_str) == Some("uncertain"))
        })
        .and_then(|d| d.get("factor").and_then(Value::as_f64))
        .unwrap_or_else(|| panic!("an uncertain demotion in {v:#}"));
    assert!((factor - 0.25).abs() < 1e-9, "the number itself: {factor}");
}

/// The guarantee that let this ship without a swept constant: a brain that never
/// passes `--confidence` cannot tell the rule was added. Every fact is born at
/// 1.0, the factor is 1.0, and nothing is even recorded in the explanation --
/// which is what keeps `fused == score` visibly true for the ordinary case.
#[test]
fn a_brain_that_never_hedges_sees_no_rule() {
    let s = Sandbox::new();
    s.remember("auth", "strategy", "server-side sessions", None);
    s.remember("gateway", "timeout", "30", None);

    let v = s.recall("qual a estrategia de auth");
    let body = v.to_string();
    assert!(!body.contains("uncertain"), "no rule fires: {body}");
}

/// Being told the same thing again moves confidence halfway to certain, so a
/// hedge that keeps getting confirmed climbs on its own. This is the half of the
/// signal that earns its way *up*, and it is why the rule is not simply a penalty
/// somebody has to remember to undo by editing the record.
#[test]
fn reassertion_lifts_a_hedge() {
    let s = Sandbox::new();
    s.remember("fila", "tamanho", "10", Some("0.2"));
    let before = s
        .recall("qual o tamanho da fila")
        .pointer("/hits/0/explain/demotions/0/factor")
        .and_then(Value::as_f64)
        .unwrap();

    s.remember("fila", "tamanho", "10", None);
    let after = s
        .recall("qual o tamanho da fila")
        .pointer("/hits/0/explain/demotions/0/factor")
        .and_then(Value::as_f64)
        .unwrap();

    assert!(
        after > before,
        "confirmed, so ranked higher: {before} -> {after}"
    );
}

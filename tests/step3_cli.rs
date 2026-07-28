//! Passo 3, part 3: the bitemporal core through the real binary.
//!
//! The anchor test here is the scenario that motivated the project: a price that
//! changes, where the brain must answer "what is it?" and "what was it?" from the
//! same record.

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
        s.run(&["init", "--label", "teste"]);
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

    fn run(&self, args: &[&str]) -> String {
        let out = self.cmd().args(args).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    }

    fn json(&self, args: &[&str]) -> serde_json::Value {
        let out = self.cmd().args(args).arg("--json").assert().success();
        serde_json::from_slice(&out.get_output().stdout).expect("valid JSON on stdout")
    }

    fn fails(&self, args: &[&str]) -> String {
        let out = self.cmd().args(args).assert().failure();
        String::from_utf8(out.get_output().stderr.clone()).unwrap()
    }
}

/// The scenario from the original brief, end to end through the binary.
#[test]
fn a_price_that_changes_answers_both_now_and_then() {
    let s = Sandbox::new();

    s.run(&[
        "remember",
        "--subject",
        "produto_a",
        "--predicate",
        "preco",
        "--value",
        "10",
        "--unit",
        "BRL",
        "--at",
        "2026-07-01",
    ]);
    s.run(&[
        "remember",
        "--subject",
        "produto_a",
        "--predicate",
        "preco",
        "--value",
        "20",
        "--unit",
        "BRL",
        "--at",
        "2026-07-28",
    ]);

    // "What is the price?" -- one answer, the current one.
    let now = s.json(&["get", "produto_a", "preco"]);
    assert_eq!(now["fact"]["object_num"], 20.0);
    assert_eq!(now["fact"]["unit"], "BRL");

    // "What was the price in July?" -- the answer that was true then.
    let then = s.json(&["get", "produto_a", "preco", "--as-of", "2026-07-10"]);
    assert_eq!(then["fact"]["object_num"], 10.0);

    // "What is the history?" -- the whole trajectory, with closed intervals.
    let hist = s.json(&["history", "produto_a", "preco"]);
    let facts = hist["facts"].as_array().unwrap();
    assert_eq!(facts.len(), 2);
    assert_eq!(facts[0]["object_num"], 10.0);
    assert!(
        facts[0]["valid_to"].is_string(),
        "the old price was not closed"
    );
    assert_eq!(facts[1]["object_num"], 20.0);
    assert!(
        facts[1]["valid_to"].is_null(),
        "the new price should be open"
    );

    // Nothing was destroyed: the old fact still carries its own value.
    assert_eq!(facts[0]["object_num"], 10.0);
}

#[test]
fn every_answer_carries_the_brain_identity() {
    let s = Sandbox::new();
    s.run(&[
        "remember",
        "--subject",
        "x",
        "--predicate",
        "p",
        "--value",
        "1",
    ]);

    for args in [
        vec!["get", "x", "p"],
        vec!["history", "x", "p"],
        vec![
            "remember",
            "--subject",
            "y",
            "--predicate",
            "p",
            "--value",
            "2",
        ],
    ] {
        let v = s.json(&args);
        assert!(v["brain_id"].is_string(), "{args:?} lost brain_id");
        assert_eq!(v["brain_label"], "teste", "{args:?}");
    }
}

#[test]
fn remember_reports_what_it_actually_did() {
    let s = Sandbox::new();
    let base = ["remember", "--subject", "p", "--predicate", "preco"];

    let created = s.json(&[&base[..], &["--value", "10", "--at", "2026-01-10"]].concat());
    assert_eq!(created["outcome"], "created");

    let same = s.json(&[&base[..], &["--value", "10", "--at", "2026-01-20"]].concat());
    assert_eq!(same["outcome"], "reasserted");
    assert_eq!(same["fact"]["reassert_count"], 1);

    let changed = s.json(&[&base[..], &["--value", "20", "--at", "2026-02-10"]].concat());
    assert_eq!(changed["outcome"], "superseded");

    let fixed = s.json(&[&base[..], &["--value", "25", "--at", "2026-02-10"]].concat());
    assert_eq!(fixed["outcome"], "corrected");
}

#[test]
fn a_fact_can_point_at_where_the_answer_lives() {
    let s = Sandbox::new();
    s.run(&[
        "remember",
        "--subject",
        "regra_de_preco",
        "--predicate",
        "definida_em",
        "--value",
        "src/pricing.rs",
        "--locator",
        r#"{"file":"src/pricing.rs","lines":"40-52"}"#,
        "--source",
        "codebase",
    ]);

    let v = s.json(&["get", "regra_de_preco", "definida_em"]);
    assert_eq!(v["fact"]["locator"]["lines"], "40-52");
    assert_eq!(v["fact"]["source"], "codebase");
}

#[test]
fn why_explains_a_facts_provenance_and_fate() {
    let s = Sandbox::new();
    s.run(&[
        "remember",
        "--subject",
        "p",
        "--predicate",
        "preco",
        "--value",
        "10",
        "--at",
        "2026-01-10",
        "--source",
        "cotacao.pdf",
    ]);
    s.run(&[
        "remember",
        "--subject",
        "p",
        "--predicate",
        "preco",
        "--value",
        "20",
        "--at",
        "2026-02-10",
    ]);

    let hist = s.json(&["history", "p", "preco"]);
    let first_id = hist["facts"][0]["id"].as_i64().unwrap();

    let why = s.json(&["why", &first_id.to_string()]);
    assert_eq!(why["fact"]["source"], "cotacao.pdf");
    assert_eq!(why["superseded_by"]["object_num"], 20.0);
}

#[test]
fn retract_removes_a_fact_from_answers_but_not_from_the_record() {
    let s = Sandbox::new();
    s.run(&[
        "remember",
        "--subject",
        "p",
        "--predicate",
        "preco",
        "--value",
        "10",
    ]);
    let id = s.json(&["get", "p", "preco"])["fact"]["id"]
        .as_i64()
        .unwrap();

    s.run(&["retract", &id.to_string(), "--reason", "typo"]);

    let got = s.json(&["get", "p", "preco"]);
    assert!(got["fact"].is_null(), "a retracted fact still answers");

    let hist = s.json(&["history", "p", "preco"]);
    assert_eq!(hist["facts"].as_array().unwrap().len(), 1);
    assert!(hist["facts"][0]["retracted_at"].is_string());
}

#[test]
fn a_relation_can_change_over_time_like_any_other_fact() {
    let s = Sandbox::new();
    s.run(&[
        "link",
        "produto_a",
        "fornecido_por",
        "acme",
        "--at",
        "2026-01-10",
    ]);
    s.run(&[
        "link",
        "produto_a",
        "fornecido_por",
        "globex",
        "--at",
        "2026-06-10",
    ]);

    let now = s.json(&["get", "produto_a", "fornecido_por"]);
    assert_eq!(now["fact"]["object_entity"], "globex");

    let then = s.json(&["get", "produto_a", "fornecido_por", "--as-of", "2026-03-01"]);
    assert_eq!(then["fact"]["object_entity"], "acme");
}

#[test]
fn a_multi_valued_predicate_can_be_declared_and_then_accumulates() {
    let s = Sandbox::new();
    s.run(&["predicate", "tag", "--cardinality", "multi"]);
    for t in ["promocao", "importado"] {
        s.run(&[
            "remember",
            "--subject",
            "p",
            "--predicate",
            "tag",
            "--value",
            t,
        ]);
    }

    let v = s.json(&["get", "p", "tag"]);
    assert_eq!(
        v["facts"].as_array().unwrap().len(),
        2,
        "multi-valued facts superseded each other"
    );
}

#[test]
fn an_unparseable_date_is_rejected_before_anything_is_written() {
    let s = Sandbox::new();
    let err = s.fails(&[
        "remember",
        "--subject",
        "p",
        "--predicate",
        "preco",
        "--value",
        "10",
        "--at",
        "ontem",
    ]);
    assert!(err.contains("ontem"), "got: {err}");

    let hist = s.json(&["history", "p", "preco"]);
    assert!(hist["facts"].as_array().unwrap().is_empty());
}

#[test]
fn asking_about_something_unknown_is_an_empty_answer_not_an_error() {
    let s = Sandbox::new();
    let v = s.json(&["get", "nao_existe", "nada"]);
    assert!(v["fact"].is_null());

    let h = s.json(&["history", "nao_existe", "nada"]);
    assert!(h["facts"].as_array().unwrap().is_empty());
}

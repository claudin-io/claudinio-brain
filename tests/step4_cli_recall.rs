//! Passo 4, part 2: `brain recall` through the binary.
//!
//! This is the acceptance test the plan called for: the motivating scenario
//! asked in natural language rather than by explicit subject/predicate.

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
        s.run(&["init", "--label", "loja"]);
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

    fn price(&self, n: &str, at: &str) {
        self.run(&[
            "remember",
            "--subject",
            "produto_a",
            "--predicate",
            "preco",
            "--value",
            n,
            "--at",
            at,
        ]);
    }
}

#[test]
fn the_motivating_question_asked_in_plain_portuguese() {
    let s = Sandbox::new();
    s.price("10", "2026-07-01");
    s.price("20", "2026-07-28");
    // Other products, so the answer has to be found rather than be the only row.
    for (p, v) in [("produto_b", "5"), ("produto_c", "7"), ("produto_d", "9")] {
        s.run(&[
            "remember",
            "--subject",
            p,
            "--predicate",
            "preco",
            "--value",
            v,
        ]);
    }

    // "What is the price of product A?"
    let now = s.json(&["recall", "qual o preco do produto_a"]);
    let hits = now["hits"].as_array().unwrap();
    assert_eq!(hits[0]["fact"]["object_num"], 20.0, "got {hits:#?}");

    // "And what was it on the 10th of July?"
    let then = s.json(&[
        "recall",
        "qual o preco do produto_a",
        "--as-of",
        "2026-07-10",
    ]);
    assert_eq!(then["hits"][0]["fact"]["object_num"], 10.0);

    // "And the history?"
    let hist = s.json(&["recall", "preco do produto_a", "--history"]);
    let values: Vec<f64> = hist["hits"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|h| h["fact"]["object_num"].as_f64())
        .collect();
    assert!(
        values.contains(&10.0) && values.contains(&20.0),
        "got {values:?}"
    );
}

#[test]
fn recall_output_reports_the_score_and_the_channels_that_found_each_hit() {
    let s = Sandbox::new();
    s.price("20", "2026-07-01");
    for i in 0..5 {
        s.run(&[
            "remember",
            "--subject",
            &format!("ruido_{i}"),
            "--predicate",
            "campo",
            "--value",
            "x",
        ]);
    }

    let v = s.json(&["recall", "preco do produto_a"]);
    let top = &v["hits"][0];
    assert!(top["score"].as_f64().unwrap() > 0.0);
    assert!(!top["channels"].as_array().unwrap().is_empty());
    assert!(v["brain_label"] == "loja", "answer lost its brain identity");
}

#[test]
fn recall_respects_the_limit_and_is_empty_rather_than_erroring() {
    let s = Sandbox::new();
    for i in 0..12 {
        s.run(&[
            "remember",
            "--subject",
            &format!("item_{i}"),
            "--predicate",
            "estado",
            "--value",
            "ativo",
        ]);
    }

    let v = s.json(&["recall", "estado ativo", "--limit", "3"]);
    assert_eq!(v["hits"].as_array().unwrap().len(), 3);

    let empty = s.json(&["recall", "xyzzy quux"]);
    assert!(empty["hits"].as_array().unwrap().is_empty());
}

#[test]
fn as_of_and_history_are_mutually_exclusive() {
    // They ask contradictory questions -- one instant versus every instant -- and
    // silently preferring one would give a confidently wrong answer.
    let s = Sandbox::new();
    s.price("10", "2026-07-01");

    s.cmd()
        .args(["recall", "preco", "--as-of", "2026-07-05", "--history"])
        .assert()
        .failure();
}

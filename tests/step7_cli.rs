//! Passo 7, part 2: names through the real binary.
//!
//! A learned name is a guess the brain made on its own, so the shell has to be
//! able to show it and take it back. A guess nobody can see is a guess nobody
//! can correct.

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

    fn num(&self, subject: &str, predicate: &str, value: &str) {
        self.run(&[
            "remember",
            "--subject",
            subject,
            "--predicate",
            predicate,
            "--value",
            value,
        ]);
    }
}

#[test]
fn a_name_can_be_given_listed_and_taken_back() {
    let s = Sandbox::new();
    s.num("acme", "funcionarios", "40");

    s.run(&["alias", "acme", "ACME Corp"]);
    let listed = s.run(&["alias", "acme"]);
    assert!(listed.contains("acme_corp"), "listing was: {listed}");
    assert!(listed.contains("declared"), "listing was: {listed}");

    // The name resolves: a fact written under it lands on the same entity.
    s.num("ACME Corp", "pais", "55");
    let v = s.json(&["entity", "acme"]);
    assert_eq!(v["entity"]["facts"].as_array().unwrap().len(), 2);

    s.run(&["alias", "acme", "ACME Corp", "--forget"]);
    assert!(s.run(&["alias", "acme"]).contains("no other names"));
}

#[test]
fn a_name_already_in_use_is_refused_with_a_reason() {
    let s = Sandbox::new();
    s.num("acme", "funcionarios", "40");
    s.num("globex", "funcionarios", "12");

    let out = s.cmd().args(["alias", "acme", "globex"]).assert().failure();
    let err = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(err.contains("already names"), "stderr was: {err}");
}

#[test]
fn recall_learns_only_when_asked_to() {
    let s = Sandbox::new();
    s.num("Produto Brasília", "preco", "20");
    s.num("servidor", "porta", "8080");
    s.num("cache", "ttl", "300");

    let plain = s.json(&["recall", "quanto custa o produto brasilia"]);
    assert!(plain["learned"].is_null(), "a plain recall wrote something");
    assert!(
        s.json(&["entity", "Produto Brasília"])["entity"]["aliases"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let learning = s.json(&["recall", "quanto custa o produto brasilia", "--learn"]);
    assert_eq!(learning["learned"]["alias"], "produto_brasilia");

    // And it is visible where anyone would look for it, marked as a guess.
    let view = s.json(&["entity", "Produto Brasília"]);
    let names = view["entity"]["aliases"].as_array().unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0]["source"], "learned");
    assert!(s.run(&["entity", "Produto Brasília"]).contains("also:"));
}

#[test]
fn reindex_rebuilds_the_vector_index() {
    let s = Sandbox::new();
    s.num("produto_a", "preco", "20");
    s.num("produto_b", "preco", "5");

    // The recovery path the schema comments promise has to exist in the binary,
    // not only in the library.
    let out = s.json(&["reindex"]);
    assert_eq!(out["reindexed"], 2);
    assert!(!s.run(&["recall", "preco do produto_a"]).is_empty());
}

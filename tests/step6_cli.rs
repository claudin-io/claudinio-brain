//! Passo 6, part 3: the graph through the real binary.
//!
//! `brain entity --neighbors` is the brain's own answer to "where would this be
//! if it is not here", which is the question `recall` asks itself on every query.
//! Being able to read it from a shell is what makes a structural answer
//! explainable instead of magic.

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

    /// produto_a -> acme -> joana, plus a second chain as noise.
    fn supply_chain(&self) -> &Self {
        self.run(&["link", "produto_a", "fornecido_por", "acme"]);
        self.run(&["link", "acme", "contato", "joana"]);
        self.run(&[
            "remember",
            "--subject",
            "joana",
            "--predicate",
            "email",
            "--value",
            "joana@acme.com",
        ]);
        self.run(&["link", "produto_b", "fornecido_por", "globex"]);
        self.run(&["link", "globex", "contato", "pedro"]);
        self
    }
}

#[test]
fn entity_reports_what_is_known_without_walking_unless_asked() {
    let s = Sandbox::new();
    s.supply_chain();

    let plain = s.run(&["entity", "produto_a"]);
    assert!(plain.contains("fornecido_por"), "got {plain}");
    assert!(
        !plain.contains("neighbours"),
        "walking is opt-in, got {plain}"
    );

    let v = s.json(&["entity", "produto_a"]);
    assert!(v["entity"]["neighbours"].as_array().unwrap().is_empty());
    assert_eq!(v["entity"]["key"], "produto_a");
    assert_eq!(v["brain_label"], "loja", "answer lost its brain identity");
}

#[test]
fn neighbors_walks_two_hops_and_says_how_it_got_there() {
    let s = Sandbox::new();
    s.supply_chain();

    let v = s.json(&["entity", "produto_a", "--neighbors"]);
    let hops: Vec<(String, u64)> = v["entity"]["neighbours"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| {
            (
                n["key"].as_str().unwrap().to_string(),
                n["hops"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        hops,
        vec![("acme".into(), 1), ("joana".into(), 2)],
        "expected the chain, nearest first"
    );

    // The reason the brain looked at joana at all.
    let via = v["entity"]["neighbours"][1]["via"].as_str().unwrap();
    assert!(via.contains("contato"), "got {via}");

    // The other chain is a different component and must not leak in.
    let text = s.run(&["entity", "produto_a", "--neighbors"]);
    assert!(!text.contains("globex"), "got {text}");
}

#[test]
fn depth_is_a_knob_the_caller_controls() {
    let s = Sandbox::new();
    s.supply_chain();

    let one = s.json(&["entity", "produto_a", "--neighbors", "--depth", "1"]);
    let keys: Vec<&str> = one["entity"]["neighbours"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| n["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, ["acme"]);
}

#[test]
fn recall_answers_across_the_chain_from_the_command_line() {
    // The end-to-end claim of this step: the question names produto_a, and the
    // answer is stored two hops away with none of its words.
    let s = Sandbox::new();
    s.supply_chain();

    let v = s.json(&["recall", "com quem falo sobre o produto_a"]);
    let top = &v["hits"][0];
    assert_eq!(
        top["fact"]["object_text"], "joana@acme.com",
        "got {:#?}",
        v["hits"]
    );
    assert!(
        top["channels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "graph"),
        "the answer should be attributed to traversal: {top:#?}"
    );
}

#[test]
fn an_unknown_entity_says_so_instead_of_failing() {
    let s = Sandbox::new();
    assert!(s.run(&["entity", "nao_existe"]).contains("no entity"));
    assert!(s.json(&["entity", "nao_existe"])["entity"].is_null());
}

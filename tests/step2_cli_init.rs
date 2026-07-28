//! Passo 2, part 2: `brain init` at the process level.
//!
//! `init` has its own path rules, deliberately simpler than the lookup ladder:
//! an explicit path, or `--global`, or the local `.brain/` -- and nothing else.
//! It must never guess.

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
        Self { _tmp: tmp, root }
    }

    fn dir(&self, rel: &str) -> std::path::PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn cmd(&self, cwd: &std::path::Path) -> Command {
        let mut c = Command::cargo_bin("brain").unwrap();
        c.current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BRAIN_CONFIG_DIR", self.root.join("xdg/config"))
            .env("BRAIN_DATA_DIR", self.root.join("xdg/data"));
        c
    }

    fn json(&self, cwd: &std::path::Path, args: &[&str]) -> serde_json::Value {
        let out = self.cmd(cwd).args(args).arg("--json").assert().success();
        serde_json::from_slice(&out.get_output().stdout).expect("valid JSON on stdout")
    }
}

#[test]
fn init_with_no_arguments_creates_the_local_brain() {
    let s = Sandbox::new();
    let proj = s.dir("proj");

    let v = s.json(&proj, &["init", "--label", "meu-projeto"]);

    assert_eq!(
        v["brain_path"],
        proj.join(".brain/brain.db").to_str().unwrap()
    );
    assert_eq!(v["brain_label"], "meu-projeto");
    assert!(
        v["brain_id"].as_str().unwrap().len() == 36,
        "expected a UUID"
    );
    assert!(proj.join(".brain/brain.db").is_file());
}

#[test]
fn a_brain_created_by_init_is_then_found_by_where() {
    let s = Sandbox::new();
    let proj = s.dir("proj");
    s.cmd(&proj)
        .args(["init", "--label", "x"])
        .assert()
        .success();

    let deep = s.dir("proj/a/b/c");
    let v = s.json(&deep, &["where"]);

    assert_eq!(
        v["brain_path"],
        proj.join(".brain/brain.db").to_str().unwrap()
    );
    assert_eq!(v["exists"], true);
}

#[test]
fn init_twice_in_the_same_place_is_an_error_that_does_not_clobber() {
    let s = Sandbox::new();
    let proj = s.dir("proj");
    let first = s.json(&proj, &["init", "--label", "original"]);

    let out = s
        .cmd(&proj)
        .args(["init", "--label", "substituto"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("already exists"), "got: {stderr}");

    // The original survived, identity and all.
    let after = s.json(&proj, &["where"]);
    assert_eq!(after["brain_path"], first["brain_path"]);
    let reopened = s.json(&proj, &["stats"]);
    assert_eq!(reopened["brain_label"], "original");
    assert_eq!(reopened["brain_id"], first["brain_id"]);
}

#[test]
fn init_at_an_explicit_path_puts_the_brain_exactly_there() {
    let s = Sandbox::new();
    let target = s.root.join("cofre/cliente-y.db");

    let v = s.json(
        &s.dir("anywhere"),
        &["init", target.to_str().unwrap(), "--label", "cliente-y"],
    );

    assert_eq!(v["brain_path"], target.to_str().unwrap());
    assert!(target.is_file());
}

#[test]
fn init_global_uses_the_data_dir() {
    let s = Sandbox::new();
    let v = s.json(&s.dir("proj"), &["init", "--global", "--label", "pessoal"]);

    assert_eq!(
        v["brain_path"],
        s.root.join("xdg/data/brain.db").to_str().unwrap()
    );
    // And it did NOT create a local .brain/.
    assert!(!s.root.join("proj/.brain").exists());
}

#[test]
fn init_with_a_name_registers_the_brain_in_the_catalogue() {
    let s = Sandbox::new();
    let target = s.root.join("empresas/acme.db");
    s.cmd(&s.dir("anywhere"))
        .args([
            "init",
            target.to_str().unwrap(),
            "--label",
            "acme",
            "--name",
            "trabalho",
        ])
        .assert()
        .success();

    // The catalogue entry is usable from an unrelated directory.
    let elsewhere = s.dir("totally/unrelated");
    let v = s.json(&elsewhere, &["--use", "trabalho", "where"]);
    assert_eq!(v["brain_path"], target.to_str().unwrap());

    // And it landed in the global config as a readable TOML file.
    let cfg = std::fs::read_to_string(s.root.join("xdg/config/config.toml")).unwrap();
    assert!(cfg.contains("trabalho"), "config was: {cfg}");
}

#[test]
fn registering_a_name_preserves_entries_that_were_already_there() {
    let s = Sandbox::new();
    std::fs::write(
        s.root.join("xdg/config/config.toml"),
        "default_brain = \"antigo\"\n[brains]\nantigo = \"/ja/existia.db\"\n",
    )
    .unwrap();

    let target = s.root.join("novo.db");
    s.cmd(&s.dir("anywhere"))
        .args([
            "init",
            target.to_str().unwrap(),
            "--label",
            "n",
            "--name",
            "novo",
        ])
        .assert()
        .success();

    let cfg = std::fs::read_to_string(s.root.join("xdg/config/config.toml")).unwrap();
    assert!(cfg.contains("antigo"), "clobbered an existing entry: {cfg}");
    assert!(
        cfg.contains("novo"),
        "did not register the new entry: {cfg}"
    );
    assert!(
        cfg.contains("default_brain"),
        "dropped default_brain: {cfg}"
    );
}

#[test]
fn init_refuses_to_reuse_a_catalogue_name_that_points_somewhere_else() {
    let s = Sandbox::new();
    std::fs::write(
        s.root.join("xdg/config/config.toml"),
        "[brains]\ntrabalho = \"/outro/lugar.db\"\n",
    )
    .unwrap();

    let out = s
        .cmd(&s.dir("anywhere"))
        .args([
            "init",
            s.root.join("novo.db").to_str().unwrap(),
            "--label",
            "x",
            "--name",
            "trabalho",
        ])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();
    assert!(stderr.contains("trabalho"), "got: {stderr}");
    assert!(
        !s.root.join("novo.db").exists(),
        "a rejected init must not leave a brain behind"
    );
}

#[test]
fn stats_reports_identity_so_an_agent_can_tell_two_brains_apart() {
    let s = Sandbox::new();
    let a = s.dir("empresa-a");
    let b = s.dir("empresa-b");
    s.cmd(&a)
        .args(["init", "--label", "acme"])
        .assert()
        .success();
    s.cmd(&b)
        .args(["init", "--label", "globex"])
        .assert()
        .success();

    let sa = s.json(&a, &["stats"]);
    let sb = s.json(&b, &["stats"]);

    assert_eq!(sa["brain_label"], "acme");
    assert_eq!(sb["brain_label"], "globex");
    assert_ne!(sa["brain_id"], sb["brain_id"]);
    assert_ne!(sa["brain_path"], sb["brain_path"]);
}

//! Passo 1, part 3: `brain where` at the process level.
//!
//! These run the real binary. They are hermetic: `BRAIN_CONFIG_DIR` and
//! `BRAIN_DATA_DIR` are always overridden so the developer's real config is
//! never read or written.

use assert_cmd::Command;
use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    root: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        // Canonicalized on purpose: on macOS /var is a symlink to /private/var and
        // `std::env::current_dir()` already returns the resolved form, so an
        // uncanonicalized expectation would never match what the process sees.
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

    fn file(&self, rel: &str, body: &str) -> std::path::PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    /// A `brain` invocation with every ambient input pinned.
    fn cmd(&self, cwd: &std::path::Path) -> Command {
        let mut c = Command::cargo_bin("brain").unwrap();
        c.current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("BRAIN_CONFIG_DIR", self.root.join("xdg/config"))
            .env("BRAIN_DATA_DIR", self.root.join("xdg/data"));
        c
    }
}

#[test]
fn where_reports_the_local_brain_and_explains_why() {
    let s = Sandbox::new();
    let db = s.file("proj/.brain/brain.db", "");
    let deep = s.dir("proj/a/b");

    let out = s.cmd(&deep).arg("where").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();

    assert!(stdout.contains(db.to_str().unwrap()), "got: {stdout}");
    assert!(
        stdout.contains(".brain/"),
        "should explain the reason; got: {stdout}"
    );
}

#[test]
fn where_json_carries_the_path_and_the_reason() {
    let s = Sandbox::new();
    let db = s.file("proj/.brain/brain.db", "");

    let out = s
        .cmd(&s.dir("proj"))
        .args(["where", "--json"])
        .assert()
        .success();
    let v: serde_json::Value =
        serde_json::from_slice(&out.get_output().stdout).expect("valid JSON on stdout");

    assert_eq!(v["brain_path"], db.to_str().unwrap());
    assert_eq!(v["exists"], true);
    assert!(v["reason"].as_str().unwrap().contains(".brain/"));
}

#[test]
fn where_fails_loudly_when_no_brain_is_configured() {
    let s = Sandbox::new();
    // A global brain file exists but nothing points at it.
    s.file("xdg/data/brain.db", "");

    let out = s.cmd(&s.dir("orphan")).arg("where").assert().failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();

    assert!(stderr.contains("brain init"), "got: {stderr}");
    assert!(
        !stderr.contains("xdg/data/brain.db"),
        "must not silently point at the global brain; got: {stderr}"
    );
}

#[test]
fn where_reports_a_brain_that_does_not_exist_yet_without_creating_it() {
    let s = Sandbox::new();
    let cwd = s.dir("proj");
    let target = s.root.join("proj/planned.db");

    let out = s
        .cmd(&cwd)
        .args(["where", "--brain"])
        .arg(&target)
        .args(["--json"])
        .assert()
        .success();
    let v: serde_json::Value = serde_json::from_slice(&out.get_output().stdout).unwrap();

    assert_eq!(v["exists"], false);
    assert!(!target.exists(), "`where` must never create anything");
}

#[test]
fn conflicting_selectors_fail_at_the_process_level() {
    let s = Sandbox::new();
    let out = s
        .cmd(&s.dir("proj"))
        .args(["where", "--brain", "/a.db", "--global"])
        .assert()
        .failure();
    let stderr = String::from_utf8(out.get_output().stderr.clone()).unwrap();

    assert!(stderr.contains("--brain"), "got: {stderr}");
    assert!(stderr.contains("--global"), "got: {stderr}");
}

#[test]
fn nothing_is_written_to_stdout_when_resolution_fails() {
    // Callers parse stdout as JSON. An error must leave it completely empty.
    let s = Sandbox::new();
    let out = s
        .cmd(&s.dir("orphan"))
        .args(["where", "--json"])
        .assert()
        .failure();

    assert!(
        out.get_output().stdout.is_empty(),
        "stdout was not empty on failure: {:?}",
        String::from_utf8_lossy(&out.get_output().stdout)
    );
}

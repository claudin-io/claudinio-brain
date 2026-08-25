//! Passo 23: a brain somebody can review.
//!
//! Everything else here renders a brain to be *looked at*. This renders one to be
//! read in a diff, which is a different requirement and a stricter one: the value
//! of the output depends on it not changing when the brain did not.
//!
//! So the claim under test is byte-identity across runs, plus the two things a
//! reviewer would be misled by if they were missing -- the closed intervals, and
//! recorded text that can turn itself into document structure.

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
        s.cmd().args(["init", "--label", "loja"]).assert().success();
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

    fn md(&self) -> String {
        let out = self
            .cmd()
            .args(["export", "--markdown", "--stdout"])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn remember(&self, args: &[&str]) {
        self.cmd().arg("remember").args(args).assert().success();
    }
}

/// The property the whole format exists for. An export carrying a timestamp, or
/// ordering by anything that renumbers, produces a diff on every run -- and a
/// diff that always fires is one nobody reads.
#[test]
fn an_unchanged_brain_exports_identically() {
    let s = Sandbox::new();
    s.remember(&[
        "--subject",
        "auth",
        "--predicate",
        "strategy",
        "--value",
        "JWT",
    ]);
    s.remember(&[
        "--subject",
        "gateway",
        "--predicate",
        "timeout",
        "--value",
        "30",
    ]);

    let first = s.md();
    let second = s.md();
    assert_eq!(first, second, "byte-identical across runs");
    assert!(!first.is_empty());
}

/// ...and a brain that *did* change produces a diff that says what changed,
/// keeping the claim that was ended rather than quietly replacing it. Losing the
/// old line would make a supersession look like an edit, which is the exact
/// confusion this project exists to remove.
#[test]
fn superseding_shows_up_as_both_claims() {
    let s = Sandbox::new();
    s.remember(&[
        "--subject",
        "auth",
        "--predicate",
        "strategy",
        "--value",
        "JWT",
        "--at",
        "2026-01-01",
    ]);
    let before = s.md();
    s.remember(&[
        "--subject",
        "auth",
        "--predicate",
        "strategy",
        "--value",
        "sessions",
        "--at",
        "2026-06-01",
    ]);
    let after = s.md();

    assert_ne!(before, after);
    assert!(
        after.contains("`was` JWT"),
        "the old claim is kept: {after}"
    );
    assert!(after.contains("until 2026-06-01"), "and closed: {after}");
    assert!(
        after.contains("`now` sessions"),
        "the new one is live: {after}"
    );
}

/// A retraction is not a supersession and must not read like one. "This was never
/// true" leaves a genuine gap in the timeline; "this stopped being true" does not.
#[test]
fn a_retraction_reads_as_never_true() {
    let s = Sandbox::new();
    s.remember(&[
        "--subject",
        "fila",
        "--predicate",
        "tamanho",
        "--value",
        "10",
    ]);
    let out = s
        .cmd()
        .args(["--json", "find", "tamanho"])
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let id = v
        .pointer("/facts/0/id")
        .or_else(|| v.pointer("/hits/0/fact/id"))
        .and_then(serde_json::Value::as_i64)
        .unwrap_or_else(|| panic!("a fact id in {v:#}"));

    s.cmd()
        .args(["retract", &id.to_string(), "--reason", "misread"])
        .assert()
        .success();

    let md = s.md();
    assert!(md.contains("`never`"), "marked as never true: {md}");
    assert!(!md.contains("`now` 10"), "and not as live: {md}");
}

/// A recorded value is text nobody reviewed, and this document is read to decide
/// whether the brain is right. A value that can open a code fence or become a
/// heading is a value that can hide the lines under it.
#[test]
fn a_value_cannot_become_document_structure() {
    let s = Sandbox::new();
    s.remember(&[
        "--subject",
        "runbook",
        "--predicate",
        "passo",
        "--value",
        "```\n## Tudo certo\n- `now` nada quebrado",
    ]);

    let md = s.md();
    assert!(md.contains("Tudo certo"), "the value is kept: {md}");
    let headings = md.lines().filter(|l| l.starts_with("## ")).count();
    assert_eq!(headings, 1, "one entity, one heading: {md}");
    assert!(!md.contains("```"), "no fence escapes: {md}");
}

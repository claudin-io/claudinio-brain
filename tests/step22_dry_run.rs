//! Passo 22: what a write would do.
//!
//! Every write here is one of four events, and only the timeline knows which.
//! `created` and `superseded` leave the same exit code and mean entirely
//! different things: one added a claim, the other ended one somebody may still be
//! acting on. A caller recording what a session learned cannot know in advance
//! which it is about to cause -- that depends on what is already there.
//!
//! `--dry-run` answers that. The claim under test is not that it prints something
//! plausible; it is that the rehearsal and the performance agree, because a dry
//! run built from a shortcut would diverge precisely where somebody trusted it.

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

    /// The outcome kinds a batch produced, in order.
    fn batch(&self, jsonl: &str, dry: bool) -> Vec<String> {
        let mut c = self.cmd();
        c.args(["--json", "remember", "--batch", "-"]);
        if dry {
            c.arg("--dry-run");
        }
        let out = c.write_stdin(jsonl.to_string()).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v["facts"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["outcome"].as_str().unwrap().to_string())
            .collect()
    }

    fn facts(&self) -> usize {
        let out = self.cmd().args(["--json", "stats"]).output().unwrap();
        let v: Value = serde_json::from_slice(&out.stdout).unwrap();
        v.pointer("/facts")
            .or_else(|| v.pointer("/stats/facts"))
            .and_then(Value::as_u64)
            .unwrap_or_else(|| panic!("a fact count in {v:#}")) as usize
    }
}

/// The whole point. A batch that mixes all four outcomes is rehearsed, then run
/// for real, and the two reports have to be the same list -- not the same length,
/// the same list. Anything less and the flag is a guess with a confident name.
#[test]
fn the_rehearsal_agrees_with_the_performance() {
    let s = Sandbox::new();
    s.cmd()
        .args([
            "remember",
            "--subject",
            "auth",
            "--predicate",
            "strategy",
            "--value",
            "JWT",
        ])
        .assert()
        .success();
    s.cmd()
        .args([
            "remember",
            "--subject",
            "cache",
            "--predicate",
            "ttl",
            "--value",
            "300",
        ])
        .assert()
        .success();

    let jsonl = concat!(
        // supersedes a live value
        r#"{"subject":"auth","predicate":"strategy","value":"server-side sessions"}"#,
        "\n",
        // the same value again: reinforced, not duplicated
        r#"{"subject":"cache","predicate":"ttl","value":300}"#,
        "\n",
        // nothing to collide with
        r#"{"subject":"fila","predicate":"tamanho","value":10}"#,
        "\n",
    );

    let rehearsed = s.batch(jsonl, true);
    let performed = s.batch(jsonl, false);
    assert_eq!(
        rehearsed, performed,
        "the rehearsal is the write, rolled back"
    );
    assert_eq!(rehearsed, vec!["superseded", "reasserted", "created"]);
}

/// Rolled back means rolled back. Not "the fact is closed again afterwards" --
/// nothing was ever open, and the count is the count.
#[test]
fn a_rehearsal_leaves_nothing_behind() {
    let s = Sandbox::new();
    s.cmd()
        .args([
            "remember",
            "--subject",
            "auth",
            "--predicate",
            "strategy",
            "--value",
            "JWT",
        ])
        .assert()
        .success();
    let before = s.facts();

    s.batch(
        concat!(
            r#"{"subject":"auth","predicate":"strategy","value":"sessions"}"#,
            "\n",
            r#"{"subject":"novo","predicate":"x","value":1}"#,
            "\n"
        ),
        true,
    );

    assert_eq!(s.facts(), before, "no fact was added or closed");
    let out = s.cmd().args(["get", "auth", "strategy"]).output().unwrap();
    let got = String::from_utf8_lossy(&out.stdout);
    assert!(got.contains("JWT"), "the live value is untouched: {got}");
}

/// A rehearsal that quietly wrote to the derived index would be worse than one
/// that wrote a fact: the fact is visible and the index is not. `recall` is what
/// reads that index, so asking it is the honest check.
#[test]
fn a_rehearsal_does_not_leak_into_recall() {
    let s = Sandbox::new();
    s.cmd()
        .args([
            "remember",
            "--subject",
            "auth",
            "--predicate",
            "strategy",
            "--value",
            "JWT",
        ])
        .assert()
        .success();

    s.cmd()
        .args([
            "remember",
            "--subject",
            "pagamento",
            "--predicate",
            "provedor",
            "--value",
            "stripe",
            "--dry-run",
        ])
        .assert()
        .success();

    let out = s
        .cmd()
        .args(["--json", "recall", "quem processa os pagamentos"])
        .output()
        .unwrap();
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(
        !body.contains("stripe"),
        "a rehearsed fact is not retrievable: {body}"
    );
}

/// The rejection rules do not soften for a rehearsal. A batch with a bad line is
/// an error either way, and finding that out is most of why somebody would ask.
#[test]
fn a_bad_batch_is_still_bad_in_rehearsal() {
    let s = Sandbox::new();
    let out = s
        .cmd()
        .args(["remember", "--batch", "-", "--dry-run"])
        .write_stdin(r#"{"sujeito":"auth","predicate":"strategy","value":"JWT"}"#.to_string())
        .output()
        .unwrap();
    assert!(!out.status.success(), "an unknown key is rejected");
}

/// A single `remember` is a batch of one, and the flag has to mean the same thing
/// there -- including the tense. "would superseded" is how a reader learns the
/// tool is pasting strings together.
#[test]
fn one_claim_reads_as_something_that_has_not_happened() {
    let s = Sandbox::new();
    s.cmd()
        .args([
            "remember",
            "--subject",
            "auth",
            "--predicate",
            "strategy",
            "--value",
            "JWT",
        ])
        .assert()
        .success();

    let out = s
        .cmd()
        .args([
            "remember",
            "--subject",
            "auth",
            "--predicate",
            "strategy",
            "--value",
            "sessions",
            "--dry-run",
        ])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("would supersede"), "{said}");
    assert!(said.contains("nothing written"), "{said}");
}

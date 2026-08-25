//! Passo 24: wiring a harness up, without anybody editing JSON.
//!
//! The configuration this writes is not complicated, and that is exactly why it
//! is worth being code: every way of doing it by hand fails *quietly*. A
//! placeholder path left unreplaced, a file overwritten instead of merged, a
//! second copy of the same hook on the second attempt -- none of those produce an
//! error. A hook never fails and never explains, so all three end as a harness
//! that runs `brain` on every prompt and shows nothing.
//!
//! So the three things under test are the three silent failures: the path is
//! real, somebody else's configuration survives, and installing twice leaves one
//! hook rather than two.

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    home: std::path::PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let home = tmp.path().canonicalize().unwrap();
        Self { _tmp: tmp, home }
    }

    fn install(&self, harness: &str, extra: &[&str]) -> String {
        let out = Command::cargo_bin("brain")
            .unwrap()
            .env("HOME", &self.home)
            .args(["hook", "install", harness])
            .args(extra)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn json(&self, rel: &str) -> Value {
        let text = std::fs::read_to_string(self.home.join(rel)).expect(rel);
        serde_json::from_str(&text).expect("valid JSON")
    }
}

/// Every command written is an absolute path to a binary that is actually there.
/// The placeholder somebody forgets to replace is the whole reason this exists.
#[test]
fn the_command_written_is_a_real_executable() {
    let s = Sandbox::new();
    s.install("codex", &[]);

    let v = s.json(".codex/hooks.json");
    let command = v
        .pointer("/hooks/SessionStart/0/hooks/0/command")
        .and_then(Value::as_str)
        .expect("a command");

    let (exe, rest) = command.split_once(" hook ").expect("`… hook <what>`");
    assert!(
        std::path::Path::new(exe).is_absolute(),
        "absolute: {command}"
    );
    assert!(std::path::Path::new(exe).exists(), "it exists: {command}");
    assert_eq!(rest, "context");
    assert!(!command.contains("PATH/TO"), "no placeholder: {command}");
}

/// A config file belongs to whoever already had one. Their hooks, their other
/// events, and their unrelated top-level keys all have to come back out.
#[test]
fn somebody_elses_configuration_survives() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.home.join(".codex")).unwrap();
    std::fs::write(
        s.home.join(".codex/hooks.json"),
        r#"{
          "description": "mine",
          "hooks": {
            "SessionStart": [{"hooks":[{"type":"command","command":"/bin/theirs"}]}],
            "PreToolUse": [{"matcher":"Bash","hooks":[{"type":"command","command":"/bin/guard"}]}]
          }
        }"#,
    )
    .unwrap();

    s.install("codex", &[]);
    let v = s.json(".codex/hooks.json");

    assert_eq!(v.get("description").and_then(Value::as_str), Some("mine"));
    assert!(
        v.to_string().contains("/bin/guard"),
        "an unrelated event is untouched: {v}"
    );
    let starts = v
        .pointer("/hooks/SessionStart")
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(starts.len(), 2, "theirs and ours: {v}");
    assert!(v.to_string().contains("/bin/theirs"), "theirs kept: {v}");
}

/// Installing twice is something people do -- after an upgrade, after moving the
/// binary. A hook added twice answers the same prompt twice and costs the context
/// twice, and nothing about that is visible.
#[test]
fn installing_twice_leaves_one_hook() {
    let s = Sandbox::new();
    s.install("codex", &[]);
    s.install("codex", &[]);

    let v = s.json(".codex/hooks.json");
    let ours = v
        .pointer("/hooks/SessionStart")
        .and_then(Value::as_array)
        .unwrap()
        .iter()
        .filter(|g| g.to_string().contains("hook context"))
        .count();
    assert_eq!(ours, 1, "{v}");
}

/// `--dry-run` says what it would do and touches nothing, for the same reason
/// `remember --dry-run` does: this rewrites a file somebody else may own.
#[test]
fn a_dry_run_writes_nothing() {
    let s = Sandbox::new();
    let said = s.install("codex", &["--dry-run"]);

    assert!(said.contains("would create"), "{said}");
    assert!(said.contains("nothing written"), "{said}");
    assert!(!s.home.join(".codex/hooks.json").exists(), "no file");
}

/// Gemini counts hook timeouts in milliseconds where the others count seconds --
/// the same field name, three orders of magnitude apart. A 15 written there is
/// fifteen milliseconds, which is a hook that always times out.
#[test]
fn gemini_timeouts_are_milliseconds() {
    let s = Sandbox::new();
    s.install("gemini", &[]);
    let v = s.json(".gemini/settings.json");

    assert_eq!(
        v.pointer("/hooks/SessionStart/0/hooks/0/timeout")
            .and_then(Value::as_u64),
        Some(15_000)
    );
    // And the event is the one Gemini actually has. It has no `UserPromptSubmit`;
    // `BeforeAgent` is where it takes context for a turn.
    assert!(v.pointer("/hooks/BeforeAgent").is_some(), "{v}");
    assert!(v.pointer("/hooks/UserPromptSubmit").is_none(), "{v}");
}

/// Cline locates hooks by filename and runs them directly, so the names and the
/// executable bit are both load-bearing.
#[test]
fn cline_gets_three_named_executables() {
    let s = Sandbox::new();
    s.install("cline", &[]);

    for event in ["TaskStart", "UserPromptSubmit", "PreCompact"] {
        let p = s.home.join("Documents/Cline/Hooks").join(event);
        let body = std::fs::read_to_string(&p).unwrap_or_else(|_| panic!("{event} exists"));
        assert!(body.contains("--format cline"), "{event}: {body}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&p).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "{event} is executable: {mode:o}");
        }
    }
}

/// A config file that is not JSON is somebody's, and the one thing worse than
/// failing to install is installing on top of it.
#[test]
fn an_unparseable_config_is_refused_not_overwritten() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.home.join(".codex")).unwrap();
    let path = s.home.join(".codex/hooks.json");
    std::fs::write(&path, "this is not json {{{").unwrap();

    let out = Command::cargo_bin("brain")
        .unwrap()
        .env("HOME", &s.home)
        .args(["hook", "install", "codex"])
        .output()
        .unwrap();

    assert!(!out.status.success(), "it refuses");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "this is not json {{{",
        "and leaves the file alone"
    );
}

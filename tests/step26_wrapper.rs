//! Passo 26: the shell wrapper keeps the CLI's contract.
//!
//! `hooks/brain-hook.sh` sits between a harness and the binary, and it is the
//! one piece of this project that runs as `sh` on somebody else's machine with
//! nobody watching. Its failure mode is the worst one this project knows: it
//! swallows stderr by design, so an argument it mishandles does not error -- it
//! prints `{}` and the brain goes silent with no sign anything is wrong.
//!
//! That is not hypothetical. The wrapper used to read its second argument as
//! the output format, so `brain-hook.sh context --global` -- a perfectly
//! reasonable thing to write into a settings file -- became
//! `brain hook context --format --global`, which the CLI rejects, which the
//! wrapper swallowed, which left a user's global brain silently uninjected in
//! every session. These tests pin the argument contract so that cannot come
//! back.
//!
//! The binary is a stub that records its argv, because what is under test is
//! not what the brain answers -- step 20 owns that -- but exactly what the
//! wrapper asks it.

use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;

fn wrapper() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("hooks/brain-hook.sh")
}

struct Stub {
    tmp: TempDir,
}

impl Stub {
    /// A fake `brain` on PATH that writes its argv, one per line, and answers
    /// like a hook that had something to say.
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let bin = tmp.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(
            bin.join("brain"),
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\"; done > \"$BRAIN_ARGS\"\nprintf '{\"stub\":true}\\n'\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(bin.join("brain"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        Self { tmp }
    }

    fn run(&self, args: &[&str]) -> Output {
        let bin = self.tmp.path().join("bin");
        // HOME is pointed into the sandbox so a real `~/.local/bin/brain` on the
        // machine running the tests cannot shadow the stub.
        Command::new("sh")
            .arg(wrapper())
            .args(args)
            .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
            .env("HOME", self.tmp.path())
            .env("BRAIN_ARGS", self.tmp.path().join("args"))
            .env_remove("BRAIN_HOOK")
            .output()
            .unwrap()
    }

    /// What the binary was actually asked, one argument per line.
    fn argv(&self) -> Vec<String> {
        std::fs::read_to_string(self.tmp.path().join("args"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The shape every config file already on somebody's machine uses. It has to
/// keep parsing exactly the way it always did.
#[test]
fn the_old_contract_still_holds() {
    let s = Stub::new();
    let out = s.run(&["context"]);
    assert!(out.status.success());
    assert_eq!(s.argv(), ["hook", "context", "--format", "claude"]);

    let out = s.run(&["context", "text"]);
    assert!(out.status.success());
    assert_eq!(s.argv(), ["hook", "context", "--format", "text"]);

    let out = s.run(&["recall", "cline"]);
    assert!(out.status.success());
    assert_eq!(s.argv(), ["hook", "recall", "--format", "cline"]);
}

/// The bug this file exists for: `--global` after the positionals must select
/// the global brain, not be eaten as the output format.
#[test]
fn global_is_a_selector_not_a_format() {
    let s = Stub::new();
    let out = s.run(&["context", "--global"]);
    assert!(out.status.success());
    assert_eq!(
        s.argv(),
        ["hook", "context", "--format", "claude", "--global"]
    );
    assert_eq!(stdout(&out), "{\"stub\":true}\n");

    // With the format stated too, in the order a person writing a settings file
    // by hand actually produced.
    let out = s.run(&["context", "claude", "--global"]);
    assert!(out.status.success());
    assert_eq!(
        s.argv(),
        ["hook", "context", "--format", "claude", "--global"]
    );
}

/// Flags may come before, between, or after the positionals: a settings file is
/// written by hand, and the wrapper's job is to not care.
#[test]
fn flag_position_does_not_matter() {
    let s = Stub::new();
    let out = s.run(&["--global", "context", "text"]);
    assert!(out.status.success());
    assert_eq!(
        s.argv(),
        ["hook", "context", "--format", "text", "--global"]
    );
}

/// `--use` and `--brain` carry a value, and the value must arrive as one
/// argument even when it holds a space -- it is a path.
#[test]
fn value_flags_travel_intact() {
    let s = Stub::new();
    let out = s.run(&["context", "--use", "work"]);
    assert!(out.status.success());
    assert_eq!(
        s.argv(),
        ["hook", "context", "--format", "claude", "--use", "work"]
    );

    let out = s.run(&["recall", "text", "--brain", "/tmp/a dir/b.db"]);
    assert!(out.status.success());
    assert_eq!(
        s.argv(),
        [
            "hook",
            "recall",
            "--format",
            "text",
            "--brain",
            "/tmp/a dir/b.db"
        ]
    );
}

/// `capture` takes no format -- it answers to the brain, not the harness -- but
/// it writes to a brain, so the selector has to reach it too.
#[test]
fn capture_forwards_selectors() {
    let s = Stub::new();
    let out = s.run(&["capture", "--global"]);
    assert!(out.status.success());
    assert_eq!(s.argv(), ["hook", "capture", "--global"]);
    // And the harness is told the same quiet nothing as always.
    assert_eq!(stdout(&out), "{}\n");
}

/// A wrapper invoked with no arguments is a misconfiguration, and the rule for
/// a hook is absolute anyway: it never fails into somebody's session.
#[test]
fn no_arguments_is_quiet_not_an_error() {
    let s = Stub::new();
    let out = s.run(&[]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(stdout(&out), "{}\n");
}

/// No binary anywhere is the ordinary state on a machine that has the plugin
/// and not the 14 MB -- silent, with the selector making no difference.
#[test]
fn missing_binary_stays_silent_even_with_selectors() {
    let tmp = TempDir::new().unwrap();
    let out = Command::new("sh")
        .arg(wrapper())
        .args(["context", "--global"])
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", tmp.path())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "{}\n");
}

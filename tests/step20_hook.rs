//! Passo 20: being there without being called.
//!
//! Everything before this passo has to be *invoked*. That is the honest failure
//! mode of a memory tool: it answers the questions somebody already suspected it
//! could answer, and stays silent about the value it would have corrected --
//! silence being indistinguishable from having nothing to say.
//!
//! `brain hook` is read by the harness rather than by a person, on every prompt,
//! which changes what the code has to guarantee. It runs in directories with no
//! brain, on prompts that name nothing, against input it did not write. All of
//! those are ordinary, none of them is an error, and the only acceptable answer
//! to any of them is `{}`.
//!
//! Two claims under test: **it never fails and never writes**, and **it answers
//! for whichever event actually fired** rather than for the one its subcommand is
//! named after.

use assert_cmd::Command;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Sandbox {
    _tmp: TempDir,
    root: std::path::PathBuf,
}

impl Sandbox {
    /// `init: false` is the case most of a hook's life is spent in: a plugin
    /// installed once, in a project that never ran `brain init`.
    fn new(init: bool) -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("xdg/config")).unwrap();
        std::fs::create_dir_all(root.join("xdg/data")).unwrap();
        let s = Self { _tmp: tmp, root };
        if init {
            s.cmd().args(["init", "--label", "loja"]).assert().success();
            s.cmd()
                .args([
                    "remember",
                    "--subject",
                    "auth",
                    "--predicate",
                    "strategy",
                    "--value",
                    "server-side sessions",
                ])
                .assert()
                .success();
        }
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

    /// Runs a hook the way a harness does: JSON in, JSON out, and the exit code
    /// is checked here because every caller of this helper depends on it.
    fn hook(&self, what: &str, input: Value) -> Value {
        let out = self
            .cmd()
            .args(["hook", what])
            .write_stdin(input.to_string())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "a hook must never fail: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("a hook always prints JSON")
    }

    fn facts(&self) -> usize {
        let out = self
            .cmd()
            .args(["stats", "--json"])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice::<Value>(&out).unwrap()["facts"]
            .as_u64()
            .unwrap() as usize
    }
}

fn context_of(v: &Value) -> Option<String> {
    Some(
        v.get("hookSpecificOutput")?
            .get("additionalContext")?
            .as_str()?
            .to_string(),
    )
}

#[test]
fn a_prompt_is_answered_from_what_the_brain_holds() {
    let s = Sandbox::new(true);
    let out = s.hook(
        "recall",
        json!({"hook_event_name": "UserPromptSubmit", "prompt": "qual a estrategia de auth que usamos"}),
    );
    let ctx = context_of(&out).expect("the brain knows something about this");
    assert!(ctx.contains("server-side sessions"), "{ctx}");
    // The date is what tells an agent how old "current" is. A decision from
    // January and one from last week are not equally safe to repeat back.
    assert!(ctx.contains("[since "), "{ctx}");
}

/// The event is read from the input, not assumed from the subcommand, so one
/// subcommand can be attached to two events without lying about either.
#[test]
fn the_answer_names_the_event_that_fired() {
    let s = Sandbox::new(true);
    let out = s.hook("flush", json!({"hook_event_name": "SessionEnd"}));
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "SessionEnd");

    let out = s.hook("flush", json!({}));
    assert_eq!(out["hookSpecificOutput"]["hookEventName"], "PreCompact");
}

/// The ordinary case in every repository that never ran `brain init`. Not an
/// error, not a warning -- nothing at all.
#[test]
fn no_brain_here_is_not_a_problem() {
    let s = Sandbox::new(false);
    for what in ["context", "recall", "flush"] {
        let out = s.hook(what, json!({"prompt": "anything at all, at length"}));
        assert_eq!(out, json!({}), "{what}");
    }
}

/// "ok" names nothing, and `recall` answers anyway -- the ranking channels always
/// return their best guess. Injecting that is worse than injecting nothing: it is
/// a confident irrelevance attached to the user's own words.
#[test]
fn a_prompt_too_short_to_mean_anything_is_left_alone() {
    let s = Sandbox::new(true);
    assert_eq!(s.hook("recall", json!({"prompt": "ok"})), json!({}));
    // And input that carries no prompt at all is not an error either.
    assert_eq!(s.hook("recall", json!({})), json!({}));
}

/// Reading is safe to do on every prompt. Writing is not, and a brain that grew a
/// fact every time somebody typed would be a log.
#[test]
fn a_hook_never_writes() {
    let s = Sandbox::new(true);
    let before = s.facts();
    for what in ["context", "recall", "flush"] {
        s.hook(
            what,
            json!({"prompt": "andre is on the platform team and owns billing"}),
        );
    }
    assert_eq!(s.facts(), before);
}

/// Turning a hook off should not require editing the settings file that
/// installed it -- which is usually somewhere the person debugging is not
/// looking.
#[test]
fn the_off_switch_is_in_the_environment() {
    let s = Sandbox::new(true);
    let out = s
        .cmd()
        .env("BRAIN_HOOK", "off")
        .args(["hook", "recall"])
        .write_stdin(json!({"prompt": "qual a estrategia de auth que usamos"}).to_string())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&out.stdout).unwrap(),
        json!({})
    );
}

/// A brain that holds a task list holds facts that are true, current, and beside
/// the point on every prompt that is not about them. The hook takes no flags, so
/// the existing `--not-scope` has to be reachable from the environment.
#[test]
fn a_high_churn_scope_can_be_kept_out() {
    let s = Sandbox::new(true);
    s.cmd()
        .args([
            "remember",
            "--subject",
            "auth",
            "--predicate",
            "status",
            "--value",
            "open",
            "--scope",
            "todo",
        ])
        .assert()
        .success();

    let prompt = json!({"prompt": "qual a estrategia de auth que usamos"});
    let with = context_of(&s.hook("recall", prompt.clone())).unwrap();
    assert!(with.contains("status open"), "{with}");

    let out = s
        .cmd()
        .env("BRAIN_HOOK_NOT_SCOPE", "todo")
        .args(["hook", "recall"])
        .write_stdin(prompt.to_string())
        .output()
        .unwrap();
    let without = context_of(&serde_json::from_slice(&out.stdout).unwrap()).unwrap();
    assert!(!without.contains("status open"), "{without}");
    assert!(without.contains("server-side sessions"), "{without}");
}

/// A session that starts with no idea the brain exists will not consult it. The
/// introduction is counts and vocabulary rather than an assurance: "you have
/// memory" is not something an agent can act on.
#[test]
fn the_session_is_told_what_this_brain_is() {
    let s = Sandbox::new(true);
    let ctx = context_of(&s.hook("context", json!({"hook_event_name": "SessionStart"})))
        .expect("a brain exists here");
    assert!(ctx.contains("loja"), "the label: {ctx}");
    assert!(ctx.contains("strategy"), "the vocabulary it learned: {ctx}");
    assert!(ctx.contains("brain recall"), "how to ask: {ctx}");
}

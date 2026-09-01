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

    /// Hermes keeps its hooks in the config file it keeps everything else in, so
    /// what comes back here is somebody's whole setup rather than a file this
    /// owns.
    fn yaml(&self, rel: &str) -> Value {
        let text = std::fs::read_to_string(self.home.join(rel)).expect(rel);
        serde_yaml_ng::from_str(&text).expect("valid YAML")
    }
}

/// The commands this wrote, for one event, in the order they were written.
fn commands(v: &Value, event: &str) -> Vec<String> {
    v.pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|e| e.get("command").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
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

// --- passo 25: wiring the hook that writes --------------------------------

/// `SessionEnd` is where a session's record gets its last and most complete
/// look at the transcript, and it is also the event with the shortest leash:
/// hooks there share a second and a half unless one of them asks for longer, and
/// a hook killed on its timeout has its work thrown away. The timeout is not
/// decoration here -- it is the difference between capturing and not.
#[test]
fn claude_code_gets_a_session_end_capture_with_its_own_timeout() {
    let s = Sandbox::new();
    s.install("claude-code", &[]);
    let v = s.json(".claude/settings.json");

    let entry = v
        .pointer("/hooks/SessionEnd/0/hooks/0")
        .unwrap_or_else(|| panic!("a SessionEnd hook: {v}"));
    assert!(
        entry["command"].as_str().unwrap().contains("hook capture"),
        "{entry}"
    );
    assert!(
        entry["timeout"].as_u64().unwrap() >= 60,
        "long enough to survive the shared budget: {entry}"
    );
}

/// A session that is killed, crashes, or is closed by the window going away
/// never reaches `SessionEnd`. Capturing on `Stop` as well is what makes the
/// record survive that -- and it has to be `async`, because a hook that runs
/// after every turn and blocks the next one is a hook that gets uninstalled.
#[test]
fn a_stop_capture_runs_async() {
    let s = Sandbox::new();
    s.install("claude-code", &[]);
    let v = s.json(".claude/settings.json");

    let entry = v
        .pointer("/hooks/Stop/0/hooks/0")
        .unwrap_or_else(|| panic!("a Stop hook: {v}"));
    assert!(
        entry["command"].as_str().unwrap().contains("hook capture"),
        "{entry}"
    );
    assert_eq!(
        entry["async"].as_bool(),
        Some(true),
        "it must not block the next turn: {entry}"
    );
}

/// Claude Code guarantees that a `SessionStart` hook's plain stdout reaches the
/// context; whether it also reads `additionalContext` there is no longer
/// documented. A JSON envelope that stops being unwrapped does not fail -- it
/// arrives as its own source code, which is worse than not arriving.
#[test]
fn session_start_speaks_plain_text_for_claude() {
    let s = Sandbox::new();
    s.install("claude-code", &[]);
    let v = s.json(".claude/settings.json");

    let command = v
        .pointer("/hooks/SessionStart/0/hooks/0/command")
        .and_then(Value::as_str)
        .unwrap();
    assert!(command.contains("hook context --format text"), "{command}");
    // The prompt event is unchanged: `additionalContext` is documented there,
    // and it is the only shape that can carry an answer without becoming one.
    let recall = v
        .pointer("/hooks/UserPromptSubmit/0/hooks/0/command")
        .and_then(Value::as_str)
        .unwrap();
    assert!(!recall.contains("--format"), "{recall}");
}

/// Somebody who installed before this existed has the old three hooks in their
/// file. Installing again has to replace them, not sit beside them -- the
/// installer recognises its own entries by the command they run, and a
/// subcommand it does not know about is a subcommand it would leave behind.
#[test]
fn installing_over_the_old_three_hooks_upgrades_them() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.home.join(".claude")).unwrap();
    std::fs::write(
        s.home.join(".claude/settings.json"),
        r#"{
          "hooks": {
            "SessionStart": [{"hooks":[{"type":"command","command":"/old/brain hook context","timeout":15}]}],
            "Stop": [{"hooks":[{"type":"command","command":"/old/brain hook capture"}]}]
          }
        }"#,
    )
    .unwrap();

    s.install("claude-code", &[]);
    let v = s.json(".claude/settings.json");

    assert!(!v.to_string().contains("/old/brain"), "replaced: {v}");
    for event in ["SessionStart", "Stop"] {
        assert_eq!(
            v.pointer(&format!("/hooks/{event}"))
                .and_then(Value::as_array)
                .unwrap()
                .len(),
            1,
            "{event} has one of ours: {v}"
        );
    }
}

/// Codex switched hooks on by default and renamed the flag that used to gate
/// them. A "still to do" that is no longer to do costs more than saying nothing:
/// it is a step somebody performs, finds no effect from, and then distrusts the
/// rest of the message for.
#[test]
fn the_codex_caveat_no_longer_asks_for_a_flag() {
    let s = Sandbox::new();
    let said = s.install("codex", &[]);
    assert!(!said.contains("codex_hooks"), "{said}");
}

/// Codex has `PreCompact` now, and it reads the same envelope the others do.
/// The flush prompt is the only chance to write down what a session learned
/// before the transcript it learned it from is summarised away.
#[test]
fn codex_gets_the_flush_it_used_to_be_denied() {
    let s = Sandbox::new();
    s.install("codex", &[]);
    let v = s.json(".codex/hooks.json");
    assert!(
        v.pointer("/hooks/PreCompact/0/hooks/0/command")
            .and_then(Value::as_str)
            .is_some_and(|c| c.contains("hook flush")),
        "{v}"
    );
}

/// The plugin and the installer write the same hooks into two different files,
/// and there is nothing in either that would notice them drifting apart. Somebody
/// on the plugin and somebody who ran `hook install` should get the same brain.
#[test]
fn the_plugin_and_the_installer_wire_the_same_events() {
    let s = Sandbox::new();
    s.install("claude-code", &[]);
    let installed = s.json(".claude/settings.json");

    let plugin: Value = serde_json::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hooks.json"),
        )
        .expect("the plugin's hooks.json"),
    )
    .expect("valid JSON");

    let events = |v: &Value| -> Vec<String> {
        let mut e: Vec<String> = v["hooks"].as_object().unwrap().keys().cloned().collect();
        e.sort();
        e
    };
    assert_eq!(events(&installed), events(&plugin));

    // And the one hook whose behaviour is not visible from its event: `Stop`
    // fires between the user's turns, so both have to say it does not block.
    assert_eq!(
        plugin
            .pointer("/hooks/Stop/0/hooks/0/async")
            .and_then(Value::as_bool),
        Some(true),
        "{plugin}"
    );
}

/// Hermes keeps hooks in `~/.hermes/config.yaml`, which is the file its whole
/// setup lives in -- models, gateway, everything. So the rule that already holds
/// for JSON has more riding on it here: what this does not understand, it leaves
/// exactly where it found it.
#[test]
fn hermes_merges_into_yaml_it_did_not_write() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.home.join(".hermes")).unwrap();
    std::fs::write(
        s.home.join(".hermes/config.yaml"),
        "model: hermes-4\nhooks:\n  post_tool_call:\n  - command: /usr/local/bin/format.sh\n    matcher: write_file\n",
    )
    .unwrap();

    s.install("hermes", &[]);
    let v = s.yaml(".hermes/config.yaml");

    assert_eq!(v.get("model").and_then(Value::as_str), Some("hermes-4"));
    assert_eq!(
        commands(&v, "post_tool_call"),
        vec!["/usr/local/bin/format.sh"],
        "somebody else's hook, on an event this never touches: {v:?}"
    );
    assert_eq!(
        v.pointer("/hooks/post_tool_call/0/matcher")
            .and_then(Value::as_str),
        Some("write_file"),
        "including the fields this does not write itself"
    );
    let ours = commands(&v, "pre_llm_call");
    assert_eq!(ours.len(), 2, "introduce and recall: {ours:?}");
    assert!(ours[0].contains("hook context"), "{ours:?}");
    assert!(ours[1].contains("hook recall"), "{ours:?}");
}

/// The same idempotence every other harness gets, through a different parser. A
/// duplicated hook answers the same turn twice and is paid for twice.
#[test]
fn installing_hermes_twice_leaves_one_hook() {
    let s = Sandbox::new();
    s.install("hermes", &[]);
    let once = commands(&s.yaml(".hermes/config.yaml"), "pre_llm_call");
    s.install("hermes", &[]);
    let twice = commands(&s.yaml(".hermes/config.yaml"), "pre_llm_call");
    assert_eq!(once, twice);
}

/// A config file this cannot parse is somebody's config file. Refusing is the
/// only safe answer: the alternative is a valid YAML file where their setup used
/// to be.
#[test]
fn an_unparseable_hermes_config_is_refused_not_overwritten() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.home.join(".hermes")).unwrap();
    let path = s.home.join(".hermes/config.yaml");
    let before = "model: [unclosed\n  - :::\n";
    std::fs::write(&path, before).unwrap();

    let out = Command::cargo_bin("brain")
        .unwrap()
        .env("HOME", &s.home)
        .args(["hook", "install", "hermes"])
        .output()
        .unwrap();

    assert!(!out.status.success(), "it refuses");
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        before,
        "and it changed nothing"
    );
}

/// Hermes counts seconds and clamps anything over five minutes with a warning.
/// A timeout copied across from a harness that counts milliseconds is how that
/// warning gets earned.
#[test]
fn hermes_timeouts_are_seconds_under_its_cap() {
    let s = Sandbox::new();
    s.install("hermes", &[]);
    let v = s.yaml(".hermes/config.yaml");
    let timeouts: Vec<u64> = v["hooks"]["pre_llm_call"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e.get("timeout").and_then(Value::as_u64))
        .collect();
    assert_eq!(timeouts.len(), 2, "{v:?}");
    for t in timeouts {
        assert!((1..=300).contains(&t), "seconds, under the cap: {t}");
    }
}

/// Hermes asks the person before it runs a hook it has not seen before, and a
/// hook awaiting consent is a hook that does nothing. That is not a failure this
/// can detect afterwards -- it looks exactly like a brain with nothing to say --
/// so it is said in advance.
#[test]
fn the_hermes_caveat_names_the_consent_gate() {
    let s = Sandbox::new();
    let out = s.install("hermes", &[]);
    assert!(out.contains("still to do"), "{out}");
    assert!(
        out.contains("hooks_auto_accept") || out.contains("HERMES_ACCEPT_HOOKS"),
        "it names the way through the gate: {out}"
    );
}

/// The file somebody copies by hand and the file this writes have to wire the
/// same events, for the same reason the plugin and the installer do: two ways of
/// installing that disagree means one of them is wrong and neither says so.
#[test]
fn the_reference_config_and_the_installer_wire_the_same_events() {
    let s = Sandbox::new();
    s.install("hermes", &[]);
    let installed = s.yaml(".hermes/config.yaml");

    let reference: Value = serde_yaml_ng::from_str(
        &std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/hermes/config.yaml"),
        )
        .expect("the reference config"),
    )
    .expect("valid YAML");

    let events = |v: &Value| -> Vec<String> {
        let mut e: Vec<String> = v["hooks"]
            .as_object()
            .expect("a hooks mapping")
            .keys()
            .cloned()
            .collect();
        e.sort();
        e
    };
    assert_eq!(events(&installed), events(&reference));

    // And the same subcommands, in the same order: the introduction is only
    // worth anything on the turn it is first.
    let subcommand = |c: &str| -> String {
        c.split_once(" hook ")
            .map(|(_, rest)| rest.to_string())
            .unwrap_or_default()
    };
    assert_eq!(
        commands(&installed, "pre_llm_call")
            .iter()
            .map(|c| subcommand(c))
            .collect::<Vec<_>>(),
        commands(&reference, "pre_llm_call")
            .iter()
            .map(|c| subcommand(c))
            .collect::<Vec<_>>(),
    );
}

/// `--project` scopes a config to one directory, and Hermes has no directory to
/// scope it to: one user-level file holds its models, its gateway and its hooks.
/// Writing `<project>/.hermes/config.yaml` would succeed, be read by nobody, and
/// look exactly like a brain with nothing to say -- so this refuses instead, and
/// says which flag to drop.
#[test]
fn hermes_has_no_project_scope_and_says_so() {
    let s = Sandbox::new();
    let out = Command::cargo_bin("brain")
        .unwrap()
        .env("HOME", &s.home)
        .current_dir(&s.home)
        .args(["hook", "install", "hermes", "--project"])
        .output()
        .unwrap();

    assert!(!out.status.success(), "it refuses");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("--project"),
        "it names the flag to drop: {err}"
    );
    assert!(
        !s.home.join(".hermes/config.yaml").exists(),
        "and wrote nothing"
    );
}

/// OpenClaw loads plugins as directories with a manifest, not as an entry in
/// somebody's config file, so there is nothing to merge here -- three files,
/// named exactly, or the runtime does not see a plugin at all.
#[test]
fn openclaw_gets_a_plugin_directory_not_a_config() {
    let s = Sandbox::new();
    s.install("openclaw", &[]);

    let dir = s.home.join(".openclaw/plugins/claudinio-brain");
    for name in ["package.json", "openclaw.plugin.json", "index.ts"] {
        let f = dir.join(name);
        assert!(f.exists(), "{name} is there");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&f).unwrap().permissions().mode();
            assert_eq!(mode & 0o111, 0, "{name} is read, not run");
        }
    }

    // The manifest is what points the runtime at the entry point; a plugin whose
    // package.json does not name it loads as nothing.
    let pkg: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).unwrap()).unwrap();
    assert_eq!(
        pkg.pointer("/openclaw/extensions/0")
            .and_then(Value::as_str),
        Some("./index.ts"),
        "{pkg}"
    );
}

/// The same idempotence the config-file harnesses get. A plugin directory cannot
/// grow a second copy of itself, but it can go stale, and the second install has
/// to be the one that wins.
#[test]
fn installing_openclaw_twice_rewrites_the_same_files() {
    let s = Sandbox::new();
    s.install("openclaw", &[]);
    let dir = s.home.join(".openclaw/plugins/claudinio-brain");
    std::fs::write(dir.join("index.ts"), "// stale").unwrap();

    s.install("openclaw", &[]);
    let entry = std::fs::read_to_string(dir.join("index.ts")).unwrap();
    assert!(entry.contains("before_prompt_build"), "it was rewritten");
    assert_eq!(
        std::fs::read_dir(&dir).unwrap().count(),
        3,
        "and no fourth file appeared"
    );
}

/// Writing the files is not installing the plugin here: OpenClaw registers a
/// plugin through its own command and gates conversation hooks behind a config
/// flag. Both are steps this deliberately does not take for somebody, so both
/// have to be said -- a plugin sitting unregistered on disk is indistinguishable
/// from a brain with nothing to say.
#[test]
fn the_openclaw_caveat_names_what_is_left_to_do() {
    let s = Sandbox::new();
    let out = s.install("openclaw", &[]);
    assert!(out.contains("still to do"), "{out}");
    assert!(
        out.contains("openclaw plugins install"),
        "it names the command: {out}"
    );
    assert!(
        out.contains("allowConversationAccess"),
        "and the flag: {out}"
    );
}

/// The rule the shell hooks are held to, checked on the one entry here that is
/// code rather than configuration: no failure reaches the prompt, and the prompt
/// crosses as an argument rather than through a shell.
#[test]
fn the_openclaw_plugin_never_lets_a_failure_reach_the_prompt() {
    let entry = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/openclaw/index.ts"),
    )
    .expect("the plugin entry");

    assert!(entry.contains("catch"), "every failure is caught: {entry}");
    assert!(
        entry.contains("--format") && entry.contains("text"),
        "it asks for the answer without an envelope"
    );
    assert!(
        entry.contains("--prompt"),
        "the prompt is an argument, not a shell string"
    );
    assert!(
        entry.contains("prependContext"),
        "and it answers in the field OpenClaw reads"
    );
}

/// A selector on `hook install` selects which brain the hooks will answer from.
/// It used to parse and be silently ignored -- an install that says "global"
/// and wires "local" is the placeholder-path failure wearing a flag.
#[test]
fn a_global_install_wires_the_global_brain() {
    let s = Sandbox::new();
    s.install("augment", &["--global"]);

    let v = s.json(".augment/settings.json");
    let command = v
        .pointer("/hooks/SessionStart/0/hooks/0/command")
        .and_then(Value::as_str)
        .expect("a command");
    assert!(
        command.ends_with("hook context --global"),
        "the selector reaches the command: {command}"
    );
}

/// A global wiring and a local one answer different questions, so they are two
/// entries, not one replacing the other -- and each reinstall replaces only its
/// own.
#[test]
fn global_and_local_wirings_coexist() {
    let s = Sandbox::new();
    s.install("augment", &[]);
    s.install("augment", &["--global"]);

    let v = s.json(".augment/settings.json");
    let both = commands_grouped(&v, "SessionStart");
    assert_eq!(both.len(), 2, "one local, one global: {both:?}");
    assert!(both.iter().any(|c| c.ends_with("--global")), "{both:?}");
    assert!(both.iter().any(|c| !c.contains("--global")), "{both:?}");

    // Installing the global one again replaces it rather than stacking.
    s.install("augment", &["--global"]);
    let v = s.json(".augment/settings.json");
    assert_eq!(commands_grouped(&v, "SessionStart").len(), 2);

    // And so does the local one.
    s.install("augment", &[]);
    let v = s.json(".augment/settings.json");
    assert_eq!(commands_grouped(&v, "SessionStart").len(), 2);
}

/// Same rule for the wiring somebody wrote by hand: a local install must not
/// eat a `--global` entry it did not write. That deletion is exactly how a
/// user's patched-in global hook died on reinstall.
#[test]
fn a_hand_written_global_entry_survives_a_local_install() {
    let s = Sandbox::new();
    std::fs::create_dir_all(s.home.join(".augment")).unwrap();
    std::fs::write(
        s.home.join(".augment/settings.json"),
        r#"{"hooks":{"SessionStart":[
            {"hooks":[{"type":"command","command":"/somewhere/brain hook context --global","timeout":15}]}
        ]}}"#,
    )
    .unwrap();

    s.install("augment", &[]);
    let v = s.json(".augment/settings.json");
    let both = commands_grouped(&v, "SessionStart");
    assert!(
        both.iter().any(|c| c.ends_with("--global")),
        "the global entry is still there: {both:?}"
    );
    assert_eq!(both.len(), 2, "{both:?}");
}

/// `--use` and `--brain` ride along the same way, and a path is quoted so the
/// shell that eventually runs the command reads it as one argument.
#[test]
fn use_and_brain_selectors_are_written_and_quoted() {
    let s = Sandbox::new();
    s.install("codex", &["--use", "work"]);
    let v = s.json(".codex/hooks.json");
    for c in commands_grouped(&v, "UserPromptSubmit") {
        assert!(c.ends_with("hook recall --use work"), "{c}");
    }

    let s = Sandbox::new();
    s.install("codex", &["--brain", "/tmp/a dir/b.db"]);
    let v = s.json(".codex/hooks.json");
    for c in commands_grouped(&v, "UserPromptSubmit") {
        assert!(c.ends_with("hook recall --brain '/tmp/a dir/b.db'"), "{c}");
    }
}

/// Cline's hooks are scripts rather than config entries, and the selector has
/// to reach those too.
#[test]
fn cline_scripts_carry_the_selector() {
    let s = Sandbox::new();
    s.install("cline", &["--global"]);
    let script = std::fs::read_to_string(s.home.join("Documents/Cline/Hooks/TaskStart")).unwrap();
    assert!(
        script.contains("hook context --format cline --global"),
        "{script}"
    );
}

/// The plugin-file harnesses run `brain` bare from PATH and cannot carry a
/// selector. Accepting one and writing a file that ignores it would be the
/// silent no-op this module exists to prevent, so it is an error instead.
#[test]
fn a_selector_on_a_static_plugin_harness_is_refused() {
    let s = Sandbox::new();
    for harness in ["opencode", "kilo", "openclaw"] {
        let out = Command::cargo_bin("brain")
            .unwrap()
            .env("HOME", &s.home)
            .args(["hook", "install", harness, "--global"])
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "{harness} must refuse a selector it cannot write"
        );
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("--global"), "{harness}: {err}");
    }
}

/// The commands written for one event, flattened across matcher groups.
fn commands_grouped(v: &Value, event: &str) -> Vec<String> {
    v.pointer(&format!("/hooks/{event}"))
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .flat_map(|g| {
                    g.pointer("/hooks")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default()
                })
                .filter_map(|e| e.get("command").and_then(Value::as_str).map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

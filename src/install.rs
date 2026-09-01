//! Wiring `brain` into a harness, without anybody editing JSON by hand.
//!
//! The configuration this writes is not complicated. What makes it worth being
//! code is that every step of doing it by hand fails *quietly*: a placeholder
//! path left unreplaced, a config overwritten instead of merged, a second copy of
//! the same hook added on the second attempt. None of those produce an error. A
//! hook never fails and never explains, so all three end as a harness that runs
//! `brain` on every prompt and shows nothing -- which is indistinguishable from a
//! brain with nothing to say.
//!
//! Three rules follow, and they are the whole design:
//!
//! - **It knows its own path.** [`std::env::current_exe`] is the one answer that
//!   cannot be typed wrong, and it is why nothing installed here goes through a
//!   wrapper script that has to travel alongside it.
//! - **It merges.** Somebody else's hooks in the same file are read, kept, and
//!   written back. A config file is not ours to own.
//! - **It is idempotent.** Installing twice replaces our entry rather than adding
//!   a second one, because a duplicated hook is a hook that answers the same
//!   prompt twice and costs the context twice.
//!
//! What it deliberately does not do is decide anything a person would want to see
//! first. Every plan can be printed instead of applied -- see `--dry-run`, which
//! exists here for the same reason it exists on `remember`.

use crate::locate::Selection;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// Everything this understands how to wire.
///
/// A harness is here only if its contract was read from a published schema or its
/// own source; see `docs/harnesses.md` for what each one can actually do, which is
/// not the same for any two of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Harness {
    ClaudeCode,
    Cline,
    Codex,
    Gemini,
    Cursor,
    Augment,
    Opencode,
    Kilo,
    Hermes,
    Openclaw,
}

impl Harness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Cline => "cline",
            Self::Codex => "codex",
            Self::Gemini => "gemini",
            Self::Cursor => "cursor",
            Self::Augment => "augment",
            Self::Opencode => "opencode",
            Self::Kilo => "kilo",
            Self::Hermes => "hermes",
            Self::Openclaw => "openclaw",
        }
    }

    /// Anything the harness needs from the person that this cannot do for them.
    ///
    /// Reported rather than attempted. Turning on somebody's feature flags is a
    /// larger claim on their setup than writing the config file they asked for,
    /// and a step they perform themselves is a step they know happened.
    pub fn caveat(self) -> Option<&'static str> {
        match self {
            Self::Cline => Some(
                "Cline runs hooks only when they are enabled: Settings -> Feature \
                 Settings -> Enable Hooks. macOS and Linux only.",
            ),
            Self::ClaudeCode => Some(
                "The plugin is the maintained path here (`/plugin install \
                 claudinio-brain@claudin-io`); this writes the same hooks into \
                 settings.json by hand instead.",
            ),
            Self::Hermes => Some(
                "Hermes asks before it runs a hook it has not seen before, so the \
                 first session after this shows a prompt and the hooks do nothing \
                 until it is answered. Approve them once, or set \
                 `hooks_auto_accept: true` in the same file (or \
                 `HERMES_ACCEPT_HOOKS=1` in the environment) to skip the gate. \
                 Note also that this file was rewritten through a YAML parser: \
                 every value survived, and any comments in it did not.",
            ),
            Self::Openclaw => Some(
                "OpenClaw registers a plugin through its own command, and gates \
                 conversation hooks behind a flag. Both are yours to run:\n  \
                 openclaw plugins install --link <the directory above>\n  \
                 and, in openclaw.json: {\"plugins\": {\"entries\": \
                 {\"claudinio-brain\": {\"enabled\": true, \"hooks\": \
                 {\"allowConversationAccess\": true}}}}}",
            ),
            _ => None,
        }
    }
}

/// One hook entry to write.
///
/// A tuple until a hook needed to say more about itself than when it fires and
/// how long it may take. `capture` runs after every turn and must not hold the
/// next one up, and "run this without blocking" is a field rather than a
/// convention -- so it is named here, once, instead of being remembered at each
/// call site.
struct Wire {
    event: &'static str,
    command: String,
    /// Always in seconds. [`Unit`] converts at write time, and passing
    /// milliseconds to the thing whose job is to produce them is how this first
    /// shipped a ten-thousand-second timeout.
    timeout: u64,
    /// Whether the harness should let this one finish in the background.
    ///
    /// Only Claude Code documents the field, and it is only written when it is
    /// set: an extra key is a small thing to send a parser that rejects the ones
    /// it does not know.
    detached: bool,
}

fn on(event: &'static str, command: String, timeout: u64) -> Wire {
    Wire {
        event,
        command,
        timeout,
        detached: false,
    }
}

impl Wire {
    fn detached(mut self) -> Self {
        self.detached = true;
        self
    }
}

/// One file this would create or change.
#[derive(Debug, Clone)]
pub struct Change {
    pub path: PathBuf,
    pub contents: String,
    /// Set for the files a harness locates by name and runs directly.
    pub executable: bool,
    /// What was there before, for a diff nobody has to go and read.
    pub existed: bool,
}

/// What installing would do.
#[derive(Debug, Clone)]
pub struct Plan {
    pub harness: Harness,
    pub changes: Vec<Change>,
}

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("could not find this executable: {0}")]
    NoExe(std::io::Error),
    #[error("no home directory to install into")]
    NoHome,
    #[error("{0} is not something this can merge into: {1}")]
    NotMergeable(PathBuf, String),
    #[error(
        "{0} keeps its configuration in one user-level file, so there is nothing \
         for --project to scope; install it without --project"
    )]
    NoProjectScope(&'static str),
    #[error("{0}: {1}")]
    Io(PathBuf, std::io::Error),
    #[error(
        "{0} runs `brain` bare from PATH inside a bundled plugin file, so a brain \
         selector cannot be written into it; install it without --global, --use \
         or --brain"
    )]
    NoSelector(&'static str),
    #[error(transparent)]
    Selector(#[from] crate::locate::LocateError),
}

type Result<T> = std::result::Result<T, InstallError>;

/// Where the config goes when it is not being scoped to one project.
fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or(InstallError::NoHome)
}

/// Works out what installing would write, and reads nothing else.
///
/// Split from [`apply`] so the answer can be printed. The plan is the whole of
/// what would happen: there is no step in `apply` that decides anything.
pub fn plan(harness: Harness, project: Option<&Path>, sel: &Selection) -> Result<Plan> {
    crate::locate::check_conflicts(sel)?;
    // The plugin-file harnesses (OpenCode, Kilo, OpenClaw) run `brain` bare from
    // PATH inside a file this ships verbatim: there is nowhere to write a
    // selector. Accepting one and installing a file that ignores it would be the
    // silent no-op this module exists to prevent.
    let wants_selector = sel.global || sel.use_name.is_some() || sel.brain.is_some();
    if wants_selector
        && matches!(
            harness,
            Harness::Opencode | Harness::Kilo | Harness::Openclaw
        )
    {
        return Err(InstallError::NoSelector(harness.as_str()));
    }
    // Which brain the hooks will answer from, appended to every command written.
    // Empty for the default: the brain of whatever directory the session is in.
    let sfx = selector_args(sel);
    let want = Fingerprint::of_selection(sel);
    let exe = std::env::current_exe().map_err(InstallError::NoExe)?;
    let exe = exe.display().to_string();
    let root = match project {
        Some(p) => p.to_path_buf(),
        None => home()?,
    };
    let scoped = project.is_some();

    let changes = match harness {
        Harness::ClaudeCode => vec![merge_claude_style(
            root.join(".claude/settings.json"),
            &[
                // Plain text, not the JSON envelope. Claude Code guarantees that
                // a session-start hook's stdout reaches the context; whether it
                // also unwraps `additionalContext` there is no longer documented,
                // and an envelope that stops being unwrapped does not fail -- it
                // arrives as its own source code.
                on(
                    "SessionStart",
                    format!("{exe} hook context --format text{sfx}"),
                    15,
                ),
                on("UserPromptSubmit", format!("{exe} hook recall{sfx}"), 10),
                on("PreCompact", format!("{exe} hook flush{sfx}"), 10),
                // The two ends of capturing a session, and both are needed.
                //
                // `Stop` fires after every turn, so the record survives a session
                // that is killed or crashes and never reaches `SessionEnd`. It is
                // detached because it runs on the path between the user's turns,
                // and a hook that makes somebody wait is a hook they uninstall.
                //
                // `SessionEnd` is the last and most complete look at the
                // transcript. Its timeout is not decoration: hooks there share a
                // second and a half unless one asks for longer, and a hook killed
                // on its timeout has its work discarded.
                on("Stop", format!("{exe} hook capture{sfx}"), 60).detached(),
                on("SessionEnd", format!("{exe} hook capture{sfx}"), 60),
            ],
            Seconds,
            &want,
        )?],
        Harness::Codex => vec![merge_claude_style(
            match scoped {
                // Codex reads project hooks from the same file name in a
                // `.codex` folder; the user-level one is the whole config dir.
                true => root.join(".codex/hooks.json"),
                false => root.join(".codex/hooks.json"),
            },
            &[
                on("SessionStart", format!("{exe} hook context{sfx}"), 15),
                on("UserPromptSubmit", format!("{exe} hook recall{sfx}"), 10),
                // `PreCompact` used to be impossible here: its output schema
                // carried only the universal fields and denied unknown ones, so
                // context sent to it was rejected rather than ignored. Codex
                // documents the shared `hookSpecificOutput` contract on it now.
                on("PreCompact", format!("{exe} hook flush{sfx}"), 10),
            ],
            Seconds,
            &want,
        )?],
        Harness::Gemini => vec![merge_claude_style(
            root.join(".gemini/settings.json"),
            &[
                // Seconds here, always. `Unit` below is what converts, and passing
                // milliseconds to something whose job is to produce them is how
                // this first shipped a ten-thousand-second timeout.
                on("SessionStart", format!("{exe} hook context{sfx}"), 15),
                // Not `UserPromptSubmit`, which Gemini does not have. `BeforeAgent`
                // is its "after the prompt, before the agent plans" event, and the
                // one place it accepts additionalContext per turn.
                on("BeforeAgent", format!("{exe} hook recall{sfx}"), 10),
            ],
            // Gemini counts this one in milliseconds. Same field, same shape,
            // three orders of magnitude apart.
            Milliseconds,
            &want,
        )?],
        Harness::Augment => vec![merge_claude_style(
            root.join(".augment/settings.json"),
            // SessionStart only: Augment has no prompt hook at all.
            &[on("SessionStart", format!("{exe} hook context{sfx}"), 15)],
            Seconds,
            &want,
        )?],
        Harness::Cursor => vec![merge_cursor(
            root.join(".cursor/hooks.json"),
            format!("{exe} hook context --format cursor{sfx}"),
            &want,
        )?],
        Harness::Cline => cline_scripts(&root, &exe, &sfx, scoped)?,
        Harness::Opencode => vec![file(
            match scoped {
                true => root.join(".opencode/plugins/claudinio-brain.js"),
                false => root.join(".config/opencode/plugins/claudinio-brain.js"),
            },
            PLUGIN_JS.to_string(),
            false,
        )],
        Harness::Hermes => {
            // Hermes reads one config file, in the home directory, and documents
            // no project-level equivalent. A `<project>/.hermes/config.yaml`
            // would be written successfully, read by nobody, and look exactly
            // like a brain with nothing to say -- which is the failure this
            // whole module exists to prevent.
            if scoped {
                return Err(InstallError::NoProjectScope(harness.as_str()));
            }
            vec![merge_hermes_yaml(
                root.join(".hermes/config.yaml"),
                &want,
                &[
                    // Both on the same event, because Hermes injects on exactly
                    // one. `on_session_start` exists and ignores whatever a hook
                    // returns, so the introduction rides on the per-turn event
                    // and decides for itself which turn is the first -- see
                    // `hook::introduce` and `extra.is_first_turn`.
                    //
                    // Order is load-bearing: the introduction is only worth
                    // anything on the turn it is first, so it goes first.
                    on(
                        "pre_llm_call",
                        format!("{exe} hook context --format hermes{sfx}"),
                        15,
                    ),
                    on(
                        "pre_llm_call",
                        format!("{exe} hook recall --format hermes{sfx}"),
                        10,
                    ),
                ],
            )?]
        }
        Harness::Openclaw => {
            // A directory rather than a file, and a staging directory rather
            // than a drop-in one: OpenClaw discovers plugins through
            // `openclaw plugins install --link <path>`, so where these three
            // files sit is this installer's choice and the command that adopts
            // them is the person's. Both halves are in the caveat.
            let dir = root.join(".openclaw/plugins/claudinio-brain");
            vec![
                file(dir.join("package.json"), OPENCLAW_PKG.to_string(), false),
                file(
                    dir.join("openclaw.plugin.json"),
                    OPENCLAW_MANIFEST.to_string(),
                    false,
                ),
                file(dir.join("index.ts"), OPENCLAW_ENTRY.to_string(), false),
            ]
        }
        Harness::Kilo => vec![file(
            match scoped {
                true => root.join(".kilo/plugin/claudinio-brain.js"),
                false => root.join(".config/kilo/plugin/claudinio-brain.js"),
            },
            PLUGIN_JS.to_string(),
            false,
        )],
    };
    Ok(Plan { harness, changes })
}

/// The plugin, compiled in rather than copied from a checkout.
///
/// One source for the file in the repository and the file this writes, so the two
/// cannot say different things.
const PLUGIN_JS: &str = include_str!("../hooks/opencode/brain.js");

/// The OpenClaw plugin, for the same reason: one source for the copy in the
/// repository and the copy this writes.
const OPENCLAW_PKG: &str = include_str!("../hooks/openclaw/package.json");
const OPENCLAW_MANIFEST: &str = include_str!("../hooks/openclaw/openclaw.plugin.json");
const OPENCLAW_ENTRY: &str = include_str!("../hooks/openclaw/index.ts");

/// What unit a harness counts a hook timeout in.
#[derive(Debug, Clone, Copy)]
enum Unit {
    Seconds,
    Milliseconds,
}
use Unit::{Milliseconds, Seconds};

impl Unit {
    fn of(self, seconds: u64) -> u64 {
        match self {
            Self::Seconds => seconds,
            Self::Milliseconds => seconds * 1000,
        }
    }
}

fn file(path: PathBuf, contents: String, executable: bool) -> Change {
    Change {
        existed: path.exists(),
        path,
        contents,
        executable,
    }
}

/// Cline finds hooks by filename, so there is no config to merge -- three files,
/// named exactly, executable.
fn cline_scripts(root: &Path, exe: &str, sfx: &str, scoped: bool) -> Result<Vec<Change>> {
    let dir = match scoped {
        true => root.join(".clinerules/hooks"),
        false => root.join("Documents/Cline/Hooks"),
    };
    Ok([
        ("TaskStart", "context"),
        ("UserPromptSubmit", "recall"),
        ("PreCompact", "flush"),
    ]
    .iter()
    .map(|(event, what)| {
        file(
            dir.join(event),
            format!(
                "#!/bin/sh\n\
                 # claudinio-brain -- Cline {event} hook. Written by `brain hook install`.\n\
                 # Cline finds hooks by filename, so this file's name is load-bearing.\n\
                 exec {exe} hook {what} --format cline{sfx}\n"
            ),
            true,
        )
    })
    .collect())
}

/// Merges into the shape Claude Code, Codex, Gemini CLI and Augment all share:
/// `hooks` -> event -> a list of matcher groups, each holding command entries.
/// `unit` is what turns each [`Wire`]'s timeout into whatever the harness counts
/// in.
fn merge_claude_style(
    path: PathBuf,
    wires: &[Wire],
    unit: Unit,
    want: &Fingerprint,
) -> Result<Change> {
    let mut root = read_object(&path)?;
    let hooks = entry_object(&mut root, "hooks");

    for w in wires {
        let list = hooks
            .entry(w.event.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| {
                InstallError::NotMergeable(
                    path.clone(),
                    format!("`hooks.{}` is not a list", w.event),
                )
            })?;

        // Ours are removed before ours are added, so a second install replaces a
        // first rather than stacking. Everybody else's groups are left exactly
        // where they were.
        list.retain(|group| !is_ours(group.pointer("/hooks"), want));
        // Only the fields these harnesses document. A cosmetic extra would be
        // riding on each of them being lenient about fields it does not know, and
        // one of them parses this with serde.
        let mut entry = json!({
            "type": "command",
            "command": w.command,
            "timeout": unit.of(w.timeout),
        });
        if w.detached {
            entry["async"] = json!(true);
        }
        list.push(json!({ "hooks": [entry] }));
    }
    Ok(file(path, pretty(&Value::Object(root)), false))
}

/// Cursor's shape: a version, then `hooks` -> event -> bare command entries.
fn merge_cursor(path: PathBuf, command: String, want: &Fingerprint) -> Result<Change> {
    let mut root = read_object(&path)?;
    root.insert("version".into(), json!(1));
    let hooks = entry_object(&mut root, "hooks");
    let list = hooks
        .entry("sessionStart".to_string())
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| {
            InstallError::NotMergeable(path.clone(), "`hooks.sessionStart` is not a list".into())
        })?;
    list.retain(|e| !is_our_command(e.get("command").and_then(Value::as_str), want));
    list.push(json!({ "command": command }));
    Ok(file(path, pretty(&Value::Object(root)), false))
}

/// Hermes's shape: `hooks` -> event -> a flat list of entries, each with its own
/// `command`. Closer to Cursor's than to Claude Code's -- there is no matcher
/// group wrapping the command -- and in YAML rather than JSON, because that is
/// the file Hermes keeps the rest of its configuration in.
///
/// Two hooks share `pre_llm_call` here, which is why clearing and adding are two
/// passes rather than one: clearing inside the loop would delete the entry the
/// previous iteration had just added.
fn merge_hermes_yaml(path: PathBuf, want: &Fingerprint, wires: &[Wire]) -> Result<Change> {
    let mut root = read_yaml_mapping(&path)?;
    let hooks = yaml_entry_mapping(&mut root, "hooks").ok_or_else(|| {
        InstallError::NotMergeable(path.clone(), "`hooks` is not a mapping".into())
    })?;

    for w in wires {
        let list = yaml_entry_sequence(hooks, w.event).ok_or_else(|| {
            InstallError::NotMergeable(path.clone(), format!("`hooks.{}` is not a list", w.event))
        })?;
        // Everybody else's entries stay exactly where they were, with whatever
        // fields they carry -- `matcher` and `fail_closed` are theirs, not ours.
        list.retain(|e| {
            !is_our_command(
                e.get("command").and_then(serde_yaml_ng::Value::as_str),
                want,
            )
        });
    }
    for w in wires {
        let Some(list) = yaml_entry_sequence(hooks, w.event) else {
            continue;
        };
        let mut entry = serde_yaml_ng::Mapping::new();
        entry.insert("command".into(), w.command.clone().into());
        // Seconds. Hermes clamps anything over 300 and warns about it, and
        // neither of these is close.
        entry.insert("timeout".into(), w.timeout.into());
        list.push(serde_yaml_ng::Value::Mapping(entry));
    }

    let text = serde_yaml_ng::to_string(&serde_yaml_ng::Value::Mapping(root))
        .map_err(|e| InstallError::NotMergeable(path.clone(), e.to_string()))?;
    Ok(file(path, text, false))
}

/// Reads an existing YAML config, or starts an empty one.
///
/// Same rule as [`read_object`], and it matters more here: this is the file
/// Hermes keeps its models, its gateway and everything else in, so a parse
/// failure that got overwritten would take somebody's whole setup with it.
fn read_yaml_mapping(path: &Path) -> Result<serde_yaml_ng::Mapping> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(serde_yaml_ng::Mapping::new());
    };
    if text.trim().is_empty() {
        return Ok(serde_yaml_ng::Mapping::new());
    }
    match serde_yaml_ng::from_str::<serde_yaml_ng::Value>(&text) {
        Ok(serde_yaml_ng::Value::Mapping(m)) => Ok(m),
        Ok(_) => Err(InstallError::NotMergeable(
            path.to_path_buf(),
            "the top level is not a mapping".into(),
        )),
        Err(e) => Err(InstallError::NotMergeable(
            path.to_path_buf(),
            e.to_string(),
        )),
    }
}

fn yaml_entry_mapping<'a>(
    root: &'a mut serde_yaml_ng::Mapping,
    key: &str,
) -> Option<&'a mut serde_yaml_ng::Mapping> {
    root.entry(key.into())
        .or_insert_with(|| serde_yaml_ng::Value::Mapping(serde_yaml_ng::Mapping::new()))
        .as_mapping_mut()
}

fn yaml_entry_sequence<'a>(
    hooks: &'a mut serde_yaml_ng::Mapping,
    event: &str,
) -> Option<&'a mut serde_yaml_ng::Sequence> {
    hooks
        .entry(event.into())
        .or_insert_with(|| serde_yaml_ng::Value::Sequence(Vec::new()))
        .as_sequence_mut()
}

/// Whether a matcher group is one this wrote.
///
/// Matched on the command rather than only on the name, because the name is a
/// field not every harness keeps and a hand-written entry will not have it. What
/// every one of them does have is a command that runs `brain hook`, and replacing
/// a hand-written entry with the generated one is the correct outcome anyway --
/// that is what somebody running the installer asked for.
fn is_ours(hooks: Option<&Value>, want: &Fingerprint) -> bool {
    hooks.and_then(Value::as_array).is_some_and(|v| {
        v.iter()
            .any(|h| is_our_command(h.get("command").and_then(Value::as_str), want))
    })
}

fn is_our_command(command: Option<&str>, want: &Fingerprint) -> bool {
    command.is_some_and(|c| {
        // Every subcommand this writes, and it has to stay that way: an entry
        // whose command is not recognised is not replaced on the next install,
        // it is left in place beside the new one.
        let runs_hook = c.contains("hook context")
            || c.contains("hook recall")
            || c.contains("hook flush")
            || c.contains("hook capture");
        // ...and only the entry answering from the *same* brain. A global wiring
        // and a local one answer different questions, so installing one must not
        // eat the other -- that deletion is exactly how a user's hand-patched
        // `--global` hook died on reinstall. When in doubt (a command whose
        // selectors do not match what is being installed), the entry is kept:
        // leaving one entry too many is recoverable, deleting somebody's wiring
        // silently is not.
        runs_hook && Fingerprint::of_command(c) == *want
    })
}

/// Which brain a written command answers from: the selector flags it carries.
///
/// This is what makes reinstalling idempotent *per brain* rather than per file.
/// Two commands with the same fingerprint are the same wiring, whatever else
/// (path of the binary, format, timeout) changed between installs.
#[derive(Debug, PartialEq, Eq)]
struct Fingerprint {
    global: bool,
    use_name: Option<String>,
    brain: Option<String>,
}

impl Fingerprint {
    fn of_selection(sel: &Selection) -> Self {
        Self {
            global: sel.global,
            use_name: sel.use_name.clone(),
            brain: sel.brain.as_ref().map(|p| p.display().to_string()),
        }
    }

    fn of_command(command: &str) -> Self {
        let tokens = sh_tokens(command);
        let mut fp = Self {
            global: false,
            use_name: None,
            brain: None,
        };
        let mut i = 0;
        while i < tokens.len() {
            match tokens[i].as_str() {
                "--global" => fp.global = true,
                "--use" => {
                    fp.use_name = tokens.get(i + 1).cloned();
                    i += 1;
                }
                "--brain" => {
                    fp.brain = tokens.get(i + 1).cloned();
                    i += 1;
                }
                t => {
                    if let Some(v) = t.strip_prefix("--use=") {
                        fp.use_name = Some(v.to_string());
                    } else if let Some(v) = t.strip_prefix("--brain=") {
                        fp.brain = Some(v.to_string());
                    }
                }
            }
            i += 1;
        }
        fp
    }
}

/// The selector flags to append to every command, shell-quoted where a value
/// needs it: the command string is eventually handed to a shell, and a path
/// with a space in it must arrive as one argument.
fn selector_args(sel: &Selection) -> String {
    let mut s = String::new();
    if sel.global {
        s.push_str(" --global");
    }
    if let Some(n) = &sel.use_name {
        s.push_str(" --use ");
        s.push_str(&sh_quote(n));
    }
    if let Some(p) = &sel.brain {
        s.push_str(" --brain ");
        s.push_str(&sh_quote(&p.display().to_string()));
    }
    s
}

/// Single-quotes a value when the shell would otherwise split or expand it.
fn sh_quote(v: &str) -> String {
    let plain = !v.is_empty()
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'));
    match plain {
        true => v.to_string(),
        false => format!("'{}'", v.replace('\'', "'\\''")),
    }
}

/// Splits a command string the way the shell that runs it will, far enough to
/// read selector values back out: whitespace separates, quotes group. Used only
/// for comparing fingerprints, where the safe failure is a token read wrong and
/// therefore an entry *kept* rather than deleted.
fn sh_tokens(command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in command.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None if c == '\'' || c == '"' => quote = Some(c),
            None if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            None => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Reads an existing config, or starts an empty one.
///
/// A file that is not JSON is an error rather than something to overwrite. It is
/// somebody's configuration, and the one thing worse than failing to install is
/// installing on top of it.
fn read_object(path: &Path) -> Result<Map<String, Value>> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(Map::new());
    };
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(m)) => Ok(m),
        Ok(_) => Err(InstallError::NotMergeable(
            path.to_path_buf(),
            "the top level is not an object".into(),
        )),
        Err(e) => Err(InstallError::NotMergeable(
            path.to_path_buf(),
            e.to_string(),
        )),
    }
}

fn entry_object<'a>(root: &'a mut Map<String, Value>, key: &str) -> &'a mut Map<String, Value> {
    root.entry(key.to_string())
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .expect("just inserted an object, or found one")
}

fn pretty(v: &Value) -> String {
    let mut s = serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into());
    s.push('\n');
    s
}

/// Writes the plan.
///
/// Directories are created because every one of these lives inside a dot-folder
/// the harness may not have made yet, and "no such file or directory" is a poor
/// answer to a command whose entire job is to put a file there.
pub fn apply(plan: &Plan) -> Result<()> {
    for c in &plan.changes {
        if let Some(dir) = c.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| InstallError::Io(dir.to_path_buf(), e))?;
        }
        std::fs::write(&c.path, &c.contents).map_err(|e| InstallError::Io(c.path.clone(), e))?;
        #[cfg(unix)]
        if c.executable {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&c.path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| InstallError::Io(c.path.clone(), e))?;
        }
    }
    Ok(())
}

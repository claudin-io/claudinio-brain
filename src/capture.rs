//! Remembering the session itself.
//!
//! Everything in [`crate::hook`] reads. That is the honest limit of being there
//! without being called: the brain answers what it already holds, and what a
//! session actually *did* -- the thing it was asked for, the files it changed,
//! what it believed it had finished -- lives in a transcript that gets compacted,
//! closed, and never read again. The next session starts from counts.
//!
//! This module closes that gap from the other side. It reads the harness's own
//! transcript and records what happened as facts about `session/<id>`, so the
//! work survives the context window it was done in.
//!
//! # The narrowed invariant
//!
//! [`crate::hook`] states that a hook never writes, and the reason is still
//! right: a brain that grew a fact every time somebody typed a prompt would be a
//! log. What this module changes is the scope of that rule, not its argument:
//!
//! > **The hooks that answer an event -- `context`, `recall`, `flush` -- never
//! > write. The one hook that writes is `capture`; it writes only facts about the
//! > session itself, into scope `sessions`, and it still never fails, never
//! > blocks and never speaks.**
//!
//! Three properties are what make that safe to install, and each is a test:
//!
//! - **It writes about one subject.** Every assertion this produces names
//!   `session/<id>` and nothing else. It cannot contradict a value a person
//!   recorded, because it never writes to one.
//! - **It converges.** The capture hook runs at the end of every turn and again
//!   when the session ends, always against the same growing transcript. Because
//!   the subject is the session and the single-valued predicates supersede, the
//!   tenth run leaves one session rather than ten -- reasserting what is
//!   unchanged and closing only what actually moved.
//! - **It cannot forge a line.** A transcript holds whatever a tool printed and
//!   whatever somebody pasted, and these facts are read back into a later
//!   session's context. Everything extracted here goes through
//!   [`crate::hook::flatten`] before it is stored, so a recorded value occupies
//!   exactly one line no matter what it says.
//!
//! # Why no model
//!
//! A summary written by a model would be better prose and a worse fact. It costs
//! an API call on a path that must finish inside a hook's timeout, it cannot be
//! rerun to the same answer, and it fails in the one way this whole design
//! refuses: quietly. What is extracted here is what the transcript states
//! literally -- the ask, the paths, the commands, the last thing said -- which is
//! the part a later session actually needs to orient itself. The model-written
//! channel still exists and is still better for durable *project* facts: that is
//! `hook flush`, and it stays where it is.

use crate::brain::{Assertion, Cardinality, Object};
use serde_json::Value;
use std::io::BufRead;

/// The predicates a session is described with.
///
/// Six, and deliberately not more. This vocabulary is paid for twice -- once in
/// the brain and once in whatever reads it back at the start of the next session
/// -- and every predicate added here is one more line competing with the
/// project's own facts. Duration, model, token counts and turn tallies were all
/// candidates; none of them changes what a later session would do next.
pub const HARNESS: &str = "harness";
pub const BRANCH: &str = "branch";
pub const WORKED_ON: &str = "worked_on";
pub const EDITED: &str = "edited";
pub const RAN: &str = "ran";
pub const CONCLUDED: &str = "concluded";

/// The whole of it, as one list.
///
/// So that a reader of a brain can tell this tool's own bookkeeping from the
/// project's vocabulary without knowing six constants by heart -- see
/// [`crate::hook`], which excludes exactly this from the introduction it writes.
pub const PREDICATES: &[&str] = &[HARNESS, BRANCH, WORKED_ON, EDITED, RAN, CONCLUDED];

/// The namespace every captured fact lives in.
///
/// One flat scope rather than `sessions/<id>`, for two reasons that both matter
/// at read time: `fact_vec` partitions by scope, so one value is one partition
/// rather than one per session; and `--not-scope sessions` is a single string
/// somebody can type when they want the brain to stop mentioning its own history.
pub const SCOPE: &str = "sessions";

/// How many files one session may name.
///
/// A session that touched fifty files has one story and fifty rows. The story is
/// what the next session needs; the rows are what git already has, more
/// accurately and without costing context on every recall.
const MAX_EDITED: usize = 20;

/// How many commands one session may name, counting back from the last.
const MAX_RAN: usize = 10;

/// The longest the session's headline may be.
///
/// Shorter than a recalled statement, because this one is quoted inside the
/// introduction that every new session pays for.
const MAX_ASK_CHARS: usize = 200;

/// The longest a recorded command may be.
const MAX_COMMAND_CHARS: usize = 120;

/// The longest the closing belief may be.
const MAX_CONCLUSION_CHARS: usize = 300;

/// The shortest user message that is somebody asking for something.
///
/// "ok", "sim", "continue" name nothing. A session headlined by one of those is
/// worse than an uncaptured session: it is a confident irrelevance that the next
/// session will read as what the last one was about.
const MIN_ASK_CHARS: usize = 8;

/// How much of a transcript is read before it stops being worth reading.
///
/// Real sessions run to megabytes and nearly all of it is tool output. The values
/// wanted here are cheap and near the start or near the end, so a ceiling costs
/// nothing in practice and bounds the one case that would otherwise be unbounded.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// The longest line worth parsing.
///
/// A single line above this is a pasted file or a tool that printed a database.
/// Nothing wanted here is in it, and parsing it is the difference between a hook
/// that costs milliseconds and one that costs a second.
const MAX_LINE_BYTES: usize = 512 * 1024;

/// What a transcript says happened, once the tool output is gone.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Digest {
    /// The substantive things the user asked for, in the order they were asked.
    pub asks: Vec<String>,
    /// The files the session changed. Deduped, capped, in the order first touched.
    pub edited: Vec<String>,
    /// The commands that did something. Deduped, and the last few rather than the
    /// first: a session commits at the end and looks around at the start.
    pub ran: Vec<String>,
    /// The last thing the session said it had done.
    pub concluded: Option<String>,
    /// The branch the work happened on, as of the end of the transcript.
    pub branch: Option<String>,
}

impl Digest {
    /// Whether this is worth a fact at all.
    ///
    /// Most turns produce nothing: a question answered, a file read, no change
    /// made. Recording those would make the brain a log of having been present,
    /// which is exactly the failure the read hooks are built to avoid.
    pub fn is_substantive(&self) -> bool {
        !self.asks.is_empty() || !self.edited.is_empty()
    }
}

/// Reads a transcript and keeps the six things worth remembering.
///
/// Streaming and single-pass, because the input is a file another process is
/// still appending to and is routinely larger than the brain it writes into.
/// Nothing here holds more than one line at a time.
///
/// Every failure mode is a skip rather than an error. A line that is not JSON is
/// ordinary -- the last one is being written while this reads it -- and a
/// transcript dialect that carries a field under a name this does not know is a
/// reason to record less, never a reason to record nothing.
pub fn parse_transcript(mut reader: impl BufRead) -> Digest {
    let mut d = Digest::default();
    let mut read: u64 = 0;
    let mut line: Vec<u8> = Vec::with_capacity(8 * 1024);

    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line) {
            Ok(0) => break,
            Ok(n) => read += n as u64,
            Err(_) => break,
        }
        if read > MAX_BYTES {
            break;
        }
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        let Ok(Value::Object(row)) = serde_json::from_str::<Value>(text.trim()) else {
            continue;
        };
        let row = Value::Object(row);
        absorb(&mut d, &row);
    }
    d
}

/// Folds one transcript row into the digest.
fn absorb(d: &mut Digest, row: &Value) {
    // A subagent's turn is interleaved into its parent's transcript. Its prompt
    // is not what the user asked for, and naming the session after a task it
    // delegated would be naming it after the wrong thing.
    if row.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return;
    }
    // The branch as of the end: a session that switched branches did its work on
    // the one it ended on, and that is where somebody goes looking for it.
    if let Some(b) = row.get("gitBranch").and_then(Value::as_str)
        && !b.trim().is_empty()
    {
        d.branch = Some(crate::hook::flatten(b, MAX_COMMAND_CHARS));
    }

    match row.get("type").and_then(Value::as_str) {
        Some("user") => {
            // The harness writes its own bookkeeping into the same file as the
            // user's words, and marks it.
            if row.get("isMeta").and_then(Value::as_bool) == Some(true) {
                return;
            }
            for text in texts_of(row) {
                if let Some(ask) = as_ask(&text)
                    && !d.asks.contains(&ask)
                {
                    d.asks.push(ask);
                }
            }
        }
        Some("assistant") => {
            for text in texts_of(row) {
                if text.trim().chars().count() >= MIN_ASK_CHARS && !is_harness_noise(&text) {
                    // Last one wins: what the session concluded is what it said
                    // most recently, and every earlier belief was superseded by
                    // the work that came after it.
                    d.concluded = Some(crate::hook::flatten(&text, MAX_CONCLUSION_CHARS));
                }
            }
            // The directory the command ran in, as the transcript states it. A
            // shell path is relative to it and an `Edit` path is absolute, so
            // without this the same file is two rows under two spellings.
            let cwd = row
                .get("cwd")
                .and_then(Value::as_str)
                .filter(|c| c.starts_with('/'));
            for (name, input) in tool_uses(row) {
                absorb_tool(d, name, input, cwd);
            }
        }
        _ => {}
    }
}

/// What a tool call did that is worth remembering a week later.
///
/// Reading, searching and listing are how the work was done; changing a file and
/// running a build are what the work *was*. Only the second kind survives.
fn absorb_tool(d: &mut Digest, name: &str, input: &Value, cwd: Option<&str>) {
    match name {
        "Edit" | "Write" | "MultiEdit" | "NotebookEdit" => {
            let Some(path) = input.get("file_path").and_then(Value::as_str) else {
                return;
            };
            push_edited(d, crate::hook::flatten(path, MAX_COMMAND_CHARS));
        }
        "Bash" => {
            let Some(command) = input.get("command").and_then(Value::as_str) else {
                return;
            };
            // A file replaced by a redirection, a `sed -i` or an interpreter
            // heredoc is as edited as one an Edit call touched. This is not a
            // nicety: a harness can be configured to prefer the shell for file
            // work, and such a session makes no Edit calls at all -- it changes
            // nine files and, before this, said it had changed the two that
            // happened to go through a tool.
            for path in written_paths(command) {
                push_edited(d, absolute(&path, cwd));
            }
            if let Some(c) = notable(command)
                && !d.ran.contains(&c)
            {
                d.ran.push(c);
                // A window on the end rather than a cap on the front, which is
                // the opposite of what `edited` wants and for a reason. A session
                // explores first and commits last: the first ten commands are how
                // it looked around, the last ten are what it did.
                if d.ran.len() > MAX_RAN {
                    d.ran.remove(0);
                }
            }
        }
        _ => {}
    }
}

/// The text blocks of a message, whichever shape it came in.
///
/// A message's content is a string when it is only words and a list of blocks
/// when it is anything else. `tool_result` blocks are deliberately not read: they
/// are the output of a command, they are most of the bytes in a transcript, and
/// nothing in them is somebody saying something.
fn texts_of(row: &Value) -> Vec<String> {
    let Some(content) = row.pointer("/message/content") else {
        return Vec::new();
    };
    match content {
        Value::String(s) => vec![s.clone()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn tool_uses(row: &Value) -> Vec<(&str, &Value)> {
    row.pointer("/message/content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                .filter_map(|b| {
                    Some((
                        b.get("name").and_then(Value::as_str)?,
                        b.get("input").unwrap_or(&Value::Null),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Whether a user message is somebody asking for something.
fn as_ask(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.chars().count() < MIN_ASK_CHARS || is_harness_noise(trimmed) {
        return None;
    }
    Some(crate::hook::flatten(trimmed, MAX_ASK_CHARS))
}

/// Whether a message is the harness talking to itself.
///
/// Slash commands, their stdout, the reminders and notifications a harness
/// splices into the same stream as the user's words. All of them arrive as
/// markup or as a leading slash, and none of them is a request. Matched on the
/// *start* of the message on purpose: a real prompt that happens to quote a
/// system reminder further down is still a real prompt.
fn is_harness_noise(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('<') || t.starts_with('/') || t.starts_with("API Error")
}

/// The commands that changed something, as opposed to the ones that looked.
///
/// An allowlist rather than a blocklist. The set of ways to inspect a repository
/// is open-ended and grows with every tool somebody installs; the set of things
/// worth telling a future session about is small, nameable, and the same
/// everywhere. Getting this wrong in the allowlist direction records nothing,
/// which is the cheap failure.
const NOTABLE: &[&str] = &[
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "make",
    "docker",
    "terraform",
    "pytest",
    "go",
    "mvn",
    "gradle",
    "kubectl",
    "gh",
    "bun",
    "deno",
    "tox",
    "poetry",
    "uv",
];

/// The git subcommands that leave something behind.
const NOTABLE_GIT: &[&str] = &[
    "commit",
    "push",
    "merge",
    "rebase",
    "tag",
    "revert",
    "cherry-pick",
    "reset",
];

/// Wrappers that stand in front of the command actually being run.
const WRAPPERS: &[&str] = &["sudo", "time", "nice", "env", "command", "exec"];

/// A path as the transcript can name it: absolute when the row said where it ran.
///
/// A join, not a resolution -- nothing here touches the filesystem, which may not
/// hold that file any more and, when a hook reads someone else's transcript, was
/// never the filesystem the work happened on.
fn absolute(path: &str, cwd: Option<&str>) -> String {
    let Some(cwd) = cwd.filter(|_| !path.starts_with('/')) else {
        return path.to_string();
    };
    format!(
        "{}/{}",
        cwd.trim_end_matches('/'),
        path.trim_start_matches("./")
    )
}

/// Records a file the session changed, whatever route the change took.
fn push_edited(d: &mut Digest, path: String) {
    if !path.is_empty() && !d.edited.contains(&path) && d.edited.len() < MAX_EDITED {
        d.edited.push(path);
    }
}

/// The roots whose files are scratch rather than work.
///
/// A session writes under `/tmp` constantly and it is never the thing it was
/// asked for. Excluding them here rather than at read time is what keeps the
/// fact honest: `edited` should not name a file nobody would call an edit.
const EPHEMERAL: &[&str] = &["/dev/", "/proc/", "/sys/", "/tmp/", "/var/tmp/"];

/// Commands whose file operands are the files they replaced.
///
/// An allowlist, for the same reason [`NOTABLE`] is one: the ways to read a file
/// are open-ended and grow with every tool installed, while the ways to replace
/// one are few, named, and the same everywhere.
const WRITERS: &[&str] = &[
    "tee", "touch", "truncate", "mv", "cp", "rm", "rmdir", "install", "mkfifo",
];

/// The git subcommands that move a file rather than a commit.
const WRITER_GIT: &[&str] = &["rm", "mv"];

/// A command line split on the shell's own separators.
///
/// Because a command is routinely a chain: `cd repo && cargo test` is a
/// `cargo test`, and reading only the first word of it finds a `cd`.
fn segments(command: &str) -> Vec<String> {
    // One separator, so the split is one pass over one pattern. `||` is replaced
    // before `|` because the shorter one is a prefix of the longer.
    command
        .replace("&&", "\u{1}")
        .replace("||", "\u{1}")
        .replace(['|', ';', '\n'], "\u{1}")
        .split('\u{1}')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Whether a token names a file, as opposed to a number, a flag, or the piece of
/// SQL that happened to sit next to a `>`.
///
/// Deliberately strict. Everything reaching here was found by a heuristic, and a
/// wrong `edited` is worse than a missing one: the missing file is still in git,
/// while the wrong one is a fact the next session reads as true. A name with no
/// extension -- `Makefile`, `Dockerfile` -- is not recognised, and that is the
/// cheap failure being chosen on purpose.
fn path_like(token: &str) -> bool {
    let t = token.trim_matches(|c| c == '"' || c == '\'');
    if t.is_empty() || t.len() > MAX_COMMAND_CHARS || t.starts_with('-') {
        return false;
    }
    if !t
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._/~+-".contains(c))
    {
        return false;
    }
    if EPHEMERAL.iter().any(|root| t.starts_with(root)) {
        return false;
    }
    // A file, not a directory: the last component carries an extension, or is a
    // dotfile. `s/foo/bar/` ends on a separator, so its last component is empty
    // and a sed script cannot be mistaken for the file it edits.
    let last = t.rsplit('/').next().unwrap_or(t);
    let alnum = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric());
    match last.split_once('.') {
        None => false,
        Some(("", rest)) => alnum(rest),
        Some(_) => {
            let ext = last.rsplit('.').next().unwrap_or("");
            ext.len() <= 8 && alnum(ext)
        }
    }
}

/// The targets of the output redirections in one segment.
fn redirect_targets(segment: &str, out: &mut Vec<String>) {
    let words: Vec<&str> = segment.split_whitespace().collect();
    for (i, word) in words.iter().enumerate() {
        let Some(at) = word.find('>') else { continue };
        // `2>file` is a redirection; `"startTime" > now()` is SQL that a split on
        // whitespace happened to walk through.
        if !word[..at].chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let rest = word[at..].trim_start_matches('>');
        // `>&2`, `2>&1`: a descriptor, not a file.
        if rest.starts_with('&') {
            continue;
        }
        let target = if rest.is_empty() {
            match words.get(i + 1) {
                Some(next) => *next,
                None => continue,
            }
        } else {
            rest
        };
        if path_like(target) {
            out.push(target.to_string());
        }
    }
}

/// The file operands of a command that exists to change files.
fn writer_targets(segment: &str, out: &mut Vec<String>) {
    let mut words = segment.split_whitespace().peekable();
    while words.peek().is_some_and(|w| WRAPPERS.contains(w)) {
        words.next();
    }
    let Some(head) = words.next() else { return };
    let head = head.rsplit('/').next().unwrap_or(head);
    let writes = match head {
        "git" => words.next().is_some_and(|sub| WRITER_GIT.contains(&sub)),
        // `sed script file` prints; only `-i` replaces, and the flag may carry a
        // backup suffix.
        "sed" => segment
            .split_whitespace()
            .any(|w| w == "--in-place" || (w.starts_with("-i") && !w.starts_with("--"))),
        other => WRITERS.contains(&other),
    };
    if !writes {
        return;
    }
    for word in words {
        if path_like(word) {
            out.push(word.to_string());
        }
    }
}

/// The files an interpreter opened for writing.
///
/// The one non-shell shape worth reading, because it is the one an agent working
/// through Bash actually uses: a `python3 - <<'PY'` that reads a file, rewrites
/// it in memory and writes it back. The path is in the command literally --
/// either inside the call, or in a plain assignment to the name the call uses --
/// and nothing here guesses past those two forms. `open(paths[i], "w")` records
/// nothing, which is the cheap failure.
fn interpreter_targets(command: &str, out: &mut Vec<String>) {
    for (i, _) in command.match_indices("open(") {
        let after = &command[i + "open(".len()..];
        let Some(close) = after.find(')') else {
            continue;
        };
        let mut args = after[..close].split(',');
        let (Some(first), Some(mode)) = (args.next(), args.next()) else {
            continue;
        };
        let quotes = |c: char| c == '\'' || c == '"';
        // Read modes are most of the calls in a transcript and none of the edits.
        if !mode
            .trim()
            .trim_matches(quotes)
            .starts_with(['w', 'a', 'x'])
        {
            continue;
        }
        let first = first.trim();
        let path = if first.starts_with(quotes) {
            first.trim_matches(quotes).to_string()
        } else {
            match literal_assigned_to(command, first) {
                Some(p) => p,
                None => continue,
            }
        };
        if path_like(&path) {
            out.push(path);
        }
    }
}

/// The literal a plain `name = "value"` assignment binds, if exactly one does.
///
/// Two different bindings of the same name is a loop or a rebind, and which one
/// the write saw is not knowable from the text -- so that records nothing too.
fn literal_assigned_to(command: &str, name: &str) -> Option<String> {
    if name.is_empty()
        || name.starts_with(|c: char| c.is_ascii_digit())
        || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    let mut found: Option<String> = None;
    for line in command.lines() {
        let Some(rest) = line.trim_start().strip_prefix(name) else {
            continue;
        };
        // `p = '...'` binds; `p == '...'` compares, and `path = '...'` is a
        // different name that merely starts with this one.
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '\'' || *c == '"') else {
            continue;
        };
        let Some(value) = rest[quote.len_utf8()..].split(quote).next() else {
            continue;
        };
        if found.as_deref().is_some_and(|f| f != value) {
            return None;
        }
        found = Some(value.to_string());
    }
    found
}

/// The files a command replaced, as opposed to the ones it read.
///
/// Order is the order the command changed them, deduped, so a chain reads back
/// the way it ran.
fn written_paths(command: &str) -> Vec<String> {
    let mut found = Vec::new();
    for segment in segments(command) {
        redirect_targets(&segment, &mut found);
        writer_targets(&segment, &mut found);
    }
    interpreter_targets(command, &mut found);
    let mut out: Vec<String> = Vec::new();
    for path in found {
        let path = crate::hook::flatten(&path, MAX_COMMAND_CHARS);
        if !path.is_empty() && !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

/// The notable part of a command line, if there is one.
fn notable(command: &str) -> Option<String> {
    for segment in segments(command) {
        let segment = segment.as_str();
        let mut words = segment.split_whitespace().peekable();
        // Step past whatever is standing in front of the real command.
        while words.peek().is_some_and(|w| WRAPPERS.contains(w)) {
            words.next();
        }
        let head = words.next()?;
        // A path-qualified binary counts as the binary it is.
        let head = head.rsplit('/').next().unwrap_or(head);
        let is_notable = match head {
            "git" => words.next().is_some_and(|sub| NOTABLE_GIT.contains(&sub)),
            other => NOTABLE.contains(&other),
        };
        if is_notable {
            return Some(crate::hook::flatten(segment, MAX_COMMAND_CHARS));
        }
    }
    None
}

/// Turns a digest into the facts that describe the session.
///
/// Every one names `session/<id>` and carries `scope` and `source`, which is what
/// keeps a captured fact distinguishable from one a person wrote -- at read time,
/// in `brain why`, and in the `--not-scope` that excludes the lot.
///
/// The cardinalities are the idempotency. `worked_on`, `branch`, `harness` and
/// `concluded` hold one value at a time, so a later capture of the same session
/// supersedes rather than accumulates; `edited` and `ran` coexist, so a second
/// run confirms the files it saw before and adds the ones it did not.
pub fn assertions(d: &Digest, session_id: &str, harness: &str) -> Vec<Assertion> {
    if !d.is_substantive() {
        return Vec::new();
    }
    let subject = format!("session/{session_id}");
    let source = format!("{harness}@{session_id}");
    let single = |p: &str, v: &str| {
        Assertion::new(&subject, p, Object::text(v))
            .scope(SCOPE)
            .source(&source)
            .cardinality(Cardinality::Single)
    };

    let mut out = vec![single(HARNESS, harness)];
    if let Some(ask) = d.asks.first() {
        out.push(single(WORKED_ON, ask));
    }
    if let Some(branch) = &d.branch {
        out.push(single(BRANCH, branch));
    }
    if let Some(concluded) = &d.concluded {
        out.push(single(CONCLUDED, concluded));
    }
    for path in &d.edited {
        out.push(
            Assertion::new(&subject, EDITED, Object::text(path))
                .scope(SCOPE)
                .source(&source)
                // Where the change was, in the field made for it: a later session
                // reading this fact gets the path as a locator it can open rather
                // than as a string it has to parse back out of a sentence.
                .locator(serde_json::json!({ "file": path }))
                .cardinality(Cardinality::Multi),
        );
    }
    for command in &d.ran {
        out.push(
            Assertion::new(&subject, RAN, Object::text(command))
                .scope(SCOPE)
                .source(&source)
                .cardinality(Cardinality::Multi),
        );
    }
    out
}

/// Drops the claims this session has already recorded.
///
/// The single-valued predicates need no help: asserting the same headline again
/// reinforces the fact that is already there, and asserting a different one
/// closes it. A multi-valued predicate has no such rule -- values coexist, so
/// nothing supersedes and a second identical assertion is a second identical
/// fact. That is the right default for the model (two people naming the same
/// dependency are two claims) and the wrong one for a hook that re-reads a
/// growing transcript every turn: by the tenth run one edited file would be ten
/// rows saying so.
///
/// Filtering here rather than changing what `multi` means keeps the fact model's
/// rule where it is. This is not "the same value was asserted twice" -- it is one
/// caller re-reading its own append-only source, which is a property of the
/// caller.
pub fn unrecorded(b: &crate::brain::Brain, subject: &str, all: Vec<Assertion>) -> Vec<Assertion> {
    let held = |predicate: &str| -> Vec<String> {
        b.current_all(subject, predicate)
            .unwrap_or_default()
            .iter()
            .filter_map(|f| f.object_text.clone())
            .collect()
    };
    let (edited, ran) = (held(EDITED), held(RAN));
    all.into_iter()
        .filter(|a| {
            let already = match a.predicate.as_str() {
                EDITED => &edited,
                RAN => &ran,
                _ => return true,
            };
            match &a.object {
                Object::Text(v) => !already.contains(v),
                _ => true,
            }
        })
        .collect()
}

/// The session's id, from whatever the harness said, or from the file it wrote.
///
/// Both, because the two disagree about which is present. Every harness that
/// writes a transcript names the file after the session; not every one repeats
/// the id in the hook's input, and a person running this by hand with
/// `--transcript` sends no input at all.
pub fn session_id(input: &Value, transcript: &std::path::Path) -> Option<String> {
    let announced = input
        .get("session_id")
        .or_else(|| input.get("sessionId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let named = transcript
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    announced
        .or(named)
        .map(|s| crate::hook::flatten(s, MAX_COMMAND_CHARS))
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chained_command_is_read_past_its_cd() {
        assert_eq!(
            notable("cd /repo && cargo test --all").as_deref(),
            Some("cargo test --all")
        );
        assert_eq!(notable("ls -la | head").as_deref(), None);
        // The wrapper is stepped past to *decide* notability and kept in what is
        // recorded: the fact should say what was run, not a tidied version of it.
        assert_eq!(
            notable("sudo docker compose up").as_deref(),
            Some("sudo docker compose up")
        );
        assert_eq!(notable("git status").as_deref(), None);
        assert_eq!(
            notable("git commit -m x").as_deref(),
            Some("git commit -m x")
        );
        assert_eq!(
            notable("/usr/bin/make release").as_deref(),
            Some("/usr/bin/make release")
        );
    }

    #[test]
    fn a_file_rewritten_through_the_shell_is_an_edit() {
        // The three shapes that started this, taken from one real session: a
        // heredoc appended to a test, a deletion chained into an in-place sed,
        // and an interpreter rewriting a file it opened through a variable.
        assert_eq!(
            written_paths("cat >> tests/test_wiring.py <<'EOF'\nx = 1\nEOF"),
            vec!["tests/test_wiring.py"]
        );
        assert_eq!(
            written_paths(
                "git rm -q cjk_strip.py && sed -i '\\#/app/cjk_strip.py#d' docker-compose.yml"
            ),
            vec!["cjk_strip.py", "docker-compose.yml"]
        );
        assert_eq!(
            written_paths(
                "python3 - <<'PY'\np='claudinio_prompt/manager.py'\ns=open(p).read()\nopen(p,'w').write(s)\nPY"
            ),
            vec!["claudinio_prompt/manager.py"]
        );
    }

    #[test]
    fn looking_is_still_not_changing() {
        for looked in [
            "grep -n cjk claudinio_prompt/manager.py",
            "docker exec pg psql -c \"select x from t where \\\"startTime\\\" > now()\"",
            "cargo test 2>&1 | tail -5",
            "curl -s https://api/x > /tmp/out.json",
            "python3 - <<'PY'\ns=open('config.yaml').read()\nprint(len(s))\nPY",
        ] {
            assert!(written_paths(looked).is_empty(), "{looked}");
        }
    }

    #[test]
    fn a_torn_last_line_is_not_the_end_of_the_world() {
        let text = "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\
                    \"adiciona a captura de sessao\"}}\n{\"type\":\"assist";
        let d = parse_transcript(text.as_bytes());
        assert_eq!(d.asks.len(), 1);
    }
}

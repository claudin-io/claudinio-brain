//! Being there without being called.
//!
//! A memory an agent has to *decide* to consult is a memory that answers the
//! questions somebody already suspected it could. Everything else -- the value it
//! would have corrected, the decision it would have cited -- it stays silent
//! about, and silence is indistinguishable from having nothing to say.
//!
//! A harness hook closes that gap: the brain is read on every prompt whether or
//! not anyone thought to ask it. What makes that affordable here is the same
//! thing the rest of this project is built on. There is no server to reach, no
//! embedding endpoint to call and no model on the read path, so answering
//! "what does this brain already hold about what the user just typed" is one
//! process, a few milliseconds, and no network. A design that had to call an API
//! per prompt would have to be selective about it, and selective is exactly the
//! failure being fixed.
//!
//! Three rules hold everywhere in this module, and all three exist because a hook
//! is code nobody is watching:
//!
//! - **It never fails.** Any error, any missing brain, any unreadable input
//!   produces `{}` and exit 0. A hook that reports its problems into somebody's
//!   session gets uninstalled the same day, taking the working half with it.
//! - **It never writes.** Reading is safe to do unconditionally; writing is not,
//!   and a brain that grew a fact every time a prompt was typed would be a log.
//!   What is learned still goes through a deliberate `remember`, which is the
//!   agent's decision and stays visible in the transcript.
//! - **It echoes the event it was given.** The payload names whichever event
//!   fired, read from the harness's own input, so one subcommand can be attached
//!   to more than one event without ever claiming to be a different one.

use crate::brain::Brain;
use crate::recall::RecallQuery;
use serde_json::{Value, json};

/// What the hook should say. Named for what it produces, not for when it fires:
/// which event carries it is the harness's decision, and [`payload`] takes that
/// from the input rather than assuming it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum What {
    /// What this brain is, and that it exists at all.
    Context,
    /// What the brain already holds about the prompt just typed.
    Recall,
    /// A reminder to write down what this session learned, while it still can.
    Flush,
}

/// How many facts an injected answer may carry.
///
/// Not a quality setting -- a budget. This is spent on every prompt, so it is
/// paid whether or not it helps, and five statements is roughly one screen of a
/// transcript. The cases it is meant to catch (a value about to be guessed at, a
/// decision about to be contradicted) are answered by the top few or not at all;
/// a hit at rank nine would not have changed what the agent was going to say.
const INJECT_LIMIT: usize = 5;

/// The shortest prompt worth asking the brain about.
///
/// "ok", "sim", "go on" name nothing, and `recall` always returns *something* for
/// them by way of the channels that rank rather than match. Injecting that is
/// worse than injecting nothing: it is a confident irrelevance, attached to the
/// user's own words.
const MIN_PROMPT_CHARS: usize = 8;

/// Builds the hook's answer.
///
/// Deliberately pure: input in, JSON out, no clock, no process, no stdout. The
/// awkward half of a hook is what it does when everything is missing, and that
/// half is only testable if it is reachable without a harness.
///
/// `not_scope` keeps a namespace out of what gets injected. A brain that holds a
/// task list holds a lot of facts that are true, current, and beside the point on
/// every prompt that is not about them; the same `--not-scope` that exists for
/// `recall` is the answer here, and it has to be reachable from a hook nobody
/// passes flags to -- hence `BRAIN_HOOK_NOT_SCOPE` in the environment.
///
/// `brain` is `None` when this directory has no brain, which is the ordinary case
/// in every repository that never ran `brain init`. That is not an error and does
/// not warn: the hook is installed once, globally, and then spends most of its
/// life in projects that do not use it.
pub fn payload(what: What, brain: Option<&Brain>, input: &Value, not_scope: Option<&str>) -> Value {
    let Some(b) = brain else {
        return json!({});
    };
    let context = match what {
        What::Context => introduce(b),
        What::Recall => match input.get("prompt").and_then(Value::as_str) {
            Some(p) => about(b, p, not_scope),
            None => None,
        },
        What::Flush => Some(FLUSH.to_string()),
    };
    match context {
        // `{}` rather than an empty string: nothing to say is a different answer
        // from saying nothing, and only one of them should cost context.
        None => json!({}),
        Some(text) => json!({
            "hookSpecificOutput": {
                "hookEventName": event_name(input, what),
                "additionalContext": text,
            }
        }),
    }
}

/// The event to answer for.
///
/// Taken from the harness's input, so `flush` attached to `PreCompact` and the
/// same `flush` attached to `SessionEnd` each name the event that actually fired.
/// The fallback is the event each subcommand is documented against, for the case
/// where this is run by hand.
fn event_name(input: &Value, what: What) -> String {
    input
        .get("hook_event_name")
        .and_then(Value::as_str)
        .unwrap_or(match what {
            What::Context => "SessionStart",
            What::Recall => "UserPromptSubmit",
            What::Flush => "PreCompact",
        })
        .to_string()
}

/// Says what this brain is and how to ask it things.
///
/// Counts rather than contents. An agent cannot use "you have memory" for
/// anything, but it can use "this brain holds 312 facts, mostly `owner`,
/// `status` and `depends_on`" -- that is enough to tell whether a question is
/// worth asking here, which is the only decision this text has to support.
fn introduce(b: &Brain) -> Option<String> {
    let store = b.store();
    let report = crate::lint::check(store.conn()).ok()?;
    let mut lines = vec![format!(
        "This project has a brain: durable, time-aware memory at {} ({}). \
         It holds {} facts about {} entities, {} of them relations.",
        store.path().display(),
        store.label(),
        report.facts,
        report.entities,
        report.edges,
    )];

    // The vocabulary, because a brain's predicates are learned rather than
    // declared: knowing it records `owner` is what makes `brain which owner` a
    // question somebody can ask.
    if let Ok(predicates) = b.predicates() {
        let top: Vec<String> = predicates
            .iter()
            .filter(|p| p.facts > 0)
            .take(8)
            .map(|p| format!("{} ({})", p.key, p.facts))
            .collect();
        if !top.is_empty() {
            lines.push(format!("It records: {}.", top.join(", ")));
        }
    }

    lines.push(
        "Before answering from assumption about a project-specific value -- a port, an owner, \
         a price, a deadline, a decision and its reason -- ask it: `brain recall \"<question>\"`, \
         `brain get <subject> <predicate>`, `brain which <predicate>`. Record what outlives this \
         session with `brain remember`. Every command takes `--json`."
            .to_string(),
    );
    Some(lines.join("\n"))
}

/// Answers the prompt the user just typed, before anybody asks.
fn about(b: &Brain, prompt: &str, not_scope: Option<&str>) -> Option<String> {
    if prompt.trim().chars().count() < MIN_PROMPT_CHARS {
        return None;
    }
    let mut q = RecallQuery::new(prompt).limit(INJECT_LIMIT);
    if let Some(s) = not_scope {
        q = q.not_scope(s);
    }
    let hits = b.recall(&q).ok()?;
    if hits.is_empty() {
        return None;
    }

    let mut lines = vec![format!(
        "The project's brain already holds this, and it is current as of now \
         (label: {}):",
        b.store().label()
    )];
    for h in &hits {
        // The date is not decoration. These are the current values, and the one
        // thing an agent has to be able to see is how old "current" is -- a
        // decision from January and one from last week are not equally safe to
        // repeat back.
        lines.push(format!(
            "  {}  [since {}]",
            h.fact.statement,
            h.fact.valid_from.strftime("%Y-%m-%d")
        ));
    }
    lines.push(
        "Retrieved by relevance, so some of it may be beside the point -- use what fits and \
         ignore the rest. If one of these contradicts what you were about to say, read \
         `brain history <subject> <predicate>` before overriding it, and record the newer value \
         rather than only mentioning it."
            .to_string(),
    );
    Some(lines.join("\n"))
}

/// What is said before the transcript goes.
///
/// Phrased as a standing instruction rather than a question, because the model
/// reading it is mid-task and will not stop to deliberate about housekeeping. It
/// names the batch form on purpose: several facts in one write are one decision,
/// and a loop of `remember` calls at the end of a session is where half a flush
/// gets lost.
const FLUSH: &str = "\
The transcript is about to be compacted or closed, so anything learned in it that is worth more \
than one session should be recorded now, while it is still readable. Write it in one batch:

  brain remember --batch - <<'JSONL'
  {\"subject\":\"auth\",\"predicate\":\"strategy\",\"value\":\"server-side sessions\",\"source\":\"session\"}
  {\"subject\":\"checkout_service\",\"predicate\":\"owner\",\"entity\":\"platform-team\",\"source\":\"session\"}
  JSONL

Worth recording: a decision and the reason for it, a value somebody stated, an owner, a deadline, \
a constraint, where in the codebase the real answer lives (`locator`). Not worth recording: \
anything the repository or its git history already says, and anything that will not be true next \
week -- unless you give it an `until`, which makes it end by itself. Nothing at all is fine.";

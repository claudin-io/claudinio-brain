// Same rule as the library: stdout belongs to the user's output (and, in MCP
// mode, to the JSON-RPC transport). Diagnostics go to stderr.
#![deny(clippy::print_stdout, clippy::dbg_macro)]

use anyhow::Context as _;
use brain::brain::{Assertion, Brain, FindQuery, Object, WhichQuery};
use brain::cli::{
    AliasArgs, Cli, Cmd, EntityArgs, GetArgs, InitArgs, LinkArgs, RecallArgs, RememberArgs,
    parse_when,
};
use brain::clock::SystemClock;
use brain::config::Config;
use brain::ids::UuidV7Gen;
use brain::locate::Ctx;
use brain::recall::{RecallQuery, When};
use brain::store::Store;
use clap::Parser;
use std::collections::{BTreeMap, BTreeSet};

fn main() -> std::process::ExitCode {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BRAIN_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<std::process::ExitCode> {
    let ctx = Ctx::from_process()?;

    // Every other command either works or fails. `lint` is the exception: a
    // finding is a result, not a failure, so the report prints normally and
    // `--strict` puts it in the exit status for a CI step to gate on.
    if let Cmd::Lint(args) = &cli.cmd {
        return cmd_lint(args, &cli, &ctx);
    }

    match &cli.cmd {
        Cmd::Init(args) => cmd_init(args, &cli, &ctx),
        Cmd::Where => cmd_where(&cli, &ctx),
        Cmd::Stats => cmd_stats(&cli, &ctx),
        // Taken above, before this match is reached.
        Cmd::Lint(_) => Ok(()),
        Cmd::Remember(args) => cmd_remember(args, &cli, &ctx),
        Cmd::Link(args) => cmd_link(args, &cli, &ctx),
        Cmd::Get(args) => cmd_get(args, &cli, &ctx),
        Cmd::Recall(args) => cmd_recall(args, &cli, &ctx),
        Cmd::Find(args) => cmd_find(args, &cli, &ctx),
        Cmd::Which(args) => cmd_which(args, &cli, &ctx),
        Cmd::Predicates => cmd_predicates(&cli, &ctx),
        Cmd::History(args) => cmd_history(args, &cli, &ctx),
        Cmd::Entity(args) => cmd_entity(args, &cli, &ctx),
        Cmd::Alias(args) => cmd_alias(args, &cli, &ctx),
        Cmd::Reindex => cmd_reindex(&cli, &ctx),
        Cmd::Hook { cmd } => cmd_hook(cmd, &cli, &ctx),
        #[cfg(feature = "mcp")]
        Cmd::Serve => cmd_serve(&cli, &ctx),
        Cmd::Export(args) => cmd_export(args, &cli, &ctx),
        #[cfg(feature = "studio")]
        Cmd::Studio(args) => cmd_studio(args, &cli, &ctx),
        Cmd::Why { fact_id } => cmd_why(*fact_id, &cli, &ctx),
        Cmd::Retract { fact_id, reason } => cmd_retract(*fact_id, reason.as_deref(), &cli, &ctx),
        Cmd::Predicate(args) => cmd_predicate(args, &cli, &ctx),
        Cmd::Repair(args) => cmd_repair(args, &cli, &ctx),
    }?;
    Ok(std::process::ExitCode::SUCCESS)
}

/// Opens the brain this invocation selected.
fn open(cli: &Cli, ctx: &Ctx) -> anyhow::Result<Brain> {
    let found = brain::cli::select(cli, ctx)?;
    Ok(Brain::open(
        &found.path,
        Box::new(SystemClock),
        Box::new(UuidV7Gen),
    )?)
}

/// Every answer is stamped with the brain that produced it, so an agent holding
/// two brains can never attribute one's facts to the other.
fn answer(b: &Brain, mut body: serde_json::Value) -> serde_json::Value {
    let mut out = b.store().identity();
    if let Some(map) = body.as_object_mut() {
        for (k, v) in map.iter() {
            out[k] = v.clone();
        }
    }
    out
}

fn cmd_remember(args: &RememberArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    if let Some(path) = &args.batch {
        return cmd_remember_batch(path, args.dry_run, cli, ctx);
    }
    // clap enforces this, but it enforces it about flags; the code that has to
    // read them should say what it needs rather than unwrap and hope.
    let (Some(subject), Some(predicate)) = (&args.subject, &args.predicate) else {
        anyhow::bail!("pass --subject and --predicate, or --batch");
    };

    // Parse everything the user supplied before touching the brain, so a bad date
    // or a malformed locator never reaches a transaction.
    let at = args.at.as_deref().map(parse_when).transpose()?;
    let until = args.until.as_deref().map(parse_when).transpose()?;
    let locator = args
        .locator
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
        .map_err(|e| anyhow::anyhow!("--locator is not valid JSON: {e}"))?;

    let object = match (&args.value, &args.entity) {
        (_, Some(e)) => Object::entity(e),
        (Some(v), None) => match &args.unit {
            Some(u) => Object::parse_literal(v).with_unit(u),
            None => Object::parse_literal(v),
        },
        (None, None) => anyhow::bail!("pass --value or --entity"),
    };

    let mut a = Assertion::new(subject, predicate, object);
    a.valid_from = at;
    a.valid_to = until;
    a.source = args.source.clone();
    a.locator = locator;
    a.confidence = args.confidence;
    a.scope = args.scope.clone();
    a.cardinality = args.cardinality;

    let b = open(cli, ctx)?;
    let outcome = match args.dry_run {
        // A slice of one rather than a second rehearsal entry point. One claim is
        // a batch of one, and the two paths agreeing is worth more than the
        // signature reading nicely.
        true => b
            .rehearse(std::slice::from_ref(&a))?
            .pop()
            .expect("one assertion in, one outcome out"),
        false => b.remember(&a)?,
    };

    // Looked up after the write, and only for a literal: the write is not in
    // doubt. This is the warning that never came the 59 times a relation was
    // recorded as a string, and nothing else will ever raise it, because nothing
    // failed.
    let hint = match args.entity {
        Some(_) => None,
        None => brain::lint::missed_relation(b.store().conn(), &brain::norm::key(predicate))?,
    };

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "outcome": outcome.kind(),
                "dry_run": args.dry_run,
                "fact": outcome.fact(),
                "hint": hint,
            }),
        ))?);
    } else {
        emit(&match args.dry_run {
            true => format!(
                "would {}: {} (nothing written)",
                outcome.would(),
                outcome.fact().statement
            ),
            false => format!("{}: {}", outcome.kind(), outcome.fact().statement),
        });
        if let Some(h) = &hint {
            emit(&format!("warning: {h}"));
        }
    }
    Ok(())
}

/// Records many facts as one write.
///
/// Everything is parsed before the brain is even opened, and then written in a
/// single transaction: a batch either lands whole or does not land. That is what
/// makes it safe for the caller it was built for -- a hook, flushing what a
/// session learned, with nobody watching. A partial batch would leave that caller
/// unable to say what it had already recorded, and its only recovery would be to
/// write everything again and hope reassertion covered the difference.
fn cmd_remember_batch(
    path: &std::path::Path,
    dry_run: bool,
    cli: &Cli,
    ctx: &Ctx,
) -> anyhow::Result<()> {
    let text = if path == std::path::Path::new("-") {
        std::io::read_to_string(std::io::stdin().lock())?
    } else {
        std::fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?
    };
    let assertions = brain::batch::parse(&text)?;

    let b = open(cli, ctx)?;
    // An empty batch is not an error. A hook that learned nothing this session
    // still runs, and a command that fails when there was nothing to do is a
    // command every caller has to special-case.
    let outcomes = match dry_run {
        true => b.rehearse(&assertions)?,
        false => b.remember_all(&assertions)?,
    };

    // One hint per predicate rather than one per fact: a batch importing forty
    // owners stored as strings has one problem, not forty.
    let mut hints: Vec<String> = Vec::new();
    let mut asked: BTreeSet<String> = BTreeSet::new();
    for (a, o) in assertions.iter().zip(&outcomes) {
        if matches!(a.object, Object::Entity(_)) {
            continue;
        }
        let key = brain::norm::key(&o.fact().predicate);
        if !asked.insert(key.clone()) {
            continue;
        }
        if let Some(h) = brain::lint::missed_relation(b.store().conn(), &key)? {
            hints.push(h);
        }
    }

    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for o in &outcomes {
        *counts.entry(o.kind()).or_default() += 1;
    }

    if cli.json {
        let facts: Vec<_> = outcomes
            .iter()
            .map(|o| serde_json::json!({ "outcome": o.kind(), "fact": o.fact() }))
            .collect();
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                // Named for what happened rather than for the flag: a caller
                // reading `wrote` has to be told when the number is hypothetical,
                // and a field that means two things depending on another field is
                // a field somebody will read wrong.
                "wrote": match dry_run { true => 0, false => outcomes.len() },
                "would_write": outcomes.len(),
                "dry_run": dry_run,
                "counts": counts,
                "facts": facts,
                "hints": hints,
            }),
        ))?);
    } else {
        for o in &outcomes {
            emit(&match dry_run {
                true => format!("would {}: {}", o.would(), o.fact().statement),
                false => format!("{}: {}", o.kind(), o.fact().statement),
            });
        }
        let tally: Vec<String> = counts.iter().map(|(k, n)| format!("{n} {k}")).collect();
        emit(&match (outcomes.len(), dry_run) {
            (0, _) => "nothing to record".to_string(),
            (n, false) => format!("{n} facts: {}", tally.join(", ")),
            (n, true) => format!(
                "would record {n} facts: {} (nothing written)",
                tally.join(", ")
            ),
        });
        for h in &hints {
            emit(&format!("warning: {h}"));
        }
    }
    Ok(())
}

/// Answers a harness lifecycle hook.
///
/// The only command here that is not allowed to fail. Everything it could
/// complain about -- no brain in this directory, unreadable input, a query that
/// found nothing -- is answered with `{}`, because the alternative is an error
/// message in somebody's session for a tool they did not invoke. See
/// [`brain::hook`] for why that rule is absolute.
fn cmd_hook(cmd: &brain::cli::HookCmd, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    use brain::cli::HookCmd;
    use brain::hook::What;
    let (what, args) = match cmd {
        HookCmd::Context(a) => (What::Context, a),
        HookCmd::Recall(a) => (What::Recall, a),
        HookCmd::Flush(a) => (What::Flush, a),
        HookCmd::Install(a) => return cmd_hook_install(a, cli, ctx),
        HookCmd::Capture(a) => return cmd_hook_capture(a, cli, ctx),
    };
    cmd_hook_answer(what, args, cli, ctx)
}

/// Records what this session did.
///
/// The one hook that writes, and it is held to the other two's rules anyway:
/// every failure it could have -- no brain here, no transcript, a transcript in a
/// dialect it does not read, a write that could not happen -- is answered with
/// `{}` and exit 0. See [`brain::capture`] for why writing at all is safe here,
/// and for the invariant it narrows rather than breaks.
fn cmd_hook_capture(
    args: &brain::cli::HookCaptureArgs,
    cli: &Cli,
    ctx: &Ctx,
) -> anyhow::Result<()> {
    if std::env::var("BRAIN_HOOK").is_ok_and(|v| v == "off") {
        emit("{}");
        return Ok(());
    }
    // Every arm below that cannot proceed says the same nothing. Written as one
    // fallible function and one caller rather than as a chain of early returns,
    // because the property being enforced -- *no path out of here reports a
    // problem* -- is only readable if there is one place it can be read.
    match capture(args, cli, ctx) {
        Some(report) => emit(&report),
        None => emit("{}"),
    }
    Ok(())
}

/// The capture itself, with every "cannot" spelled `None`.
fn capture(args: &brain::cli::HookCaptureArgs, cli: &Cli, ctx: &Ctx) -> Option<String> {
    let input = match args.transcript {
        // A named transcript is a caller who has already decided what to read,
        // and who is probably not sending stdin at all.
        Some(_) => serde_json::Value::Null,
        None => hook_input(),
    };
    let path = match &args.transcript {
        Some(p) => p.clone(),
        None => std::path::PathBuf::from(
            input
                .get("transcript_path")
                .and_then(serde_json::Value::as_str)?,
        ),
    };
    let session = brain::capture::session_id(&input, &path)?;

    let b = open(cli, ctx).ok()?;
    let file = std::fs::File::open(&path).ok()?;
    let digest = brain::capture::parse_transcript(std::io::BufReader::new(file));
    // What this session did, minus what it has already been recorded as doing:
    // the hook re-reads a growing transcript every turn, and the values that
    // coexist rather than supersede would otherwise pile up. See
    // [`brain::capture::unrecorded`].
    let subject = format!("session/{session}");
    let assertions = brain::capture::unrecorded(
        &b,
        &subject,
        brain::capture::assertions(&digest, &session, &args.harness),
    );

    // An empty capture is the ordinary case, not a failure: most turns read a
    // file and answer a question. It still reports, so `--json` can say so.
    let outcomes = match args.dry_run || assertions.is_empty() {
        true => b.rehearse(&assertions).ok()?,
        false => b.remember_all(&assertions).ok()?,
    };

    if cli.json {
        let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
        for o in &outcomes {
            *counts.entry(o.kind()).or_default() += 1;
        }
        let facts: Vec<_> = outcomes
            .iter()
            .map(|o| serde_json::json!({ "outcome": o.kind(), "fact": o.fact() }))
            .collect();
        return serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "session": subject,
                "dry_run": args.dry_run,
                "wrote": match args.dry_run { true => 0, false => outcomes.len() },
                "would_write": outcomes.len(),
                "counts": counts,
                "facts": facts,
            }),
        ))
        .ok();
    }
    if args.dry_run {
        let lines: Vec<String> = outcomes
            .iter()
            .map(|o| format!("would {}: {}", o.would(), o.fact().statement))
            .collect();
        return Some(match lines.is_empty() {
            true => "nothing to record".to_string(),
            false => lines.join("\n"),
        });
    }
    // Nothing to say, in the shape every other hook says it. Whatever this is
    // attached to may be parsing stdout, and `{}` is the answer that is safe to
    // hand a parser.
    Some("{}".to_string())
}

/// Wires these hooks into a harness's own configuration.
fn cmd_hook_install(
    args: &brain::cli::HookInstallArgs,
    cli: &Cli,
    ctx: &Ctx,
) -> anyhow::Result<()> {
    let project = args.project.then_some(ctx.cwd.as_path());
    let plan = brain::install::plan(args.harness, project)?;

    if cli.json {
        let changes: Vec<_> = plan
            .changes
            .iter()
            .map(|c| {
                serde_json::json!({
                    "path": c.path,
                    "existed": c.existed,
                    "bytes": c.contents.len(),
                })
            })
            .collect();
        if !args.dry_run {
            brain::install::apply(&plan)?;
        }
        emit(&serde_json::to_string_pretty(&serde_json::json!({
            "harness": plan.harness.as_str(),
            "dry_run": args.dry_run,
            "wrote": match args.dry_run { true => 0, false => plan.changes.len() },
            "changes": changes,
            "caveat": plan.harness.caveat(),
        }))?);
        return Ok(());
    }

    for c in &plan.changes {
        // Whether the file was already there is the part somebody wants to see
        // before this runs, not after: one of these two verbs means their existing
        // configuration is being rewritten.
        emit(&format!(
            "{} {}",
            match (c.existed, args.dry_run) {
                (true, true) => "would update",
                (false, true) => "would create",
                (true, false) => "updated",
                (false, false) => "created",
            },
            c.path.display()
        ));
    }
    if !args.dry_run {
        brain::install::apply(&plan)?;
    }
    emit(&match args.dry_run {
        true => format!(
            "{} would be wired up ({} files, nothing written)",
            plan.harness.as_str(),
            plan.changes.len()
        ),
        false => format!("{} is wired up", plan.harness.as_str()),
    });
    if let Some(c) = plan.harness.caveat() {
        emit(&format!("\nstill to do:\n{c}"));
    }
    Ok(())
}

/// Answers one lifecycle event.
fn cmd_hook_answer(
    what: brain::hook::What,
    args: &brain::cli::HookArgs,
    cli: &Cli,
    ctx: &Ctx,
) -> anyhow::Result<()> {
    // An explicit off switch, checked before anything is opened. Turning a hook
    // off should not require editing the settings file that installed it, which
    // is usually somewhere the person debugging is not looking.
    if std::env::var("BRAIN_HOOK").is_ok_and(|v| v == "off") {
        emit("{}");
        return Ok(());
    }

    // An explicit `--prompt` outranks stdin: a caller that passed one is a caller
    // that has already decided what the question is.
    let input = match &args.prompt {
        Some(p) => serde_json::json!({ "prompt": p }),
        None => hook_input(),
    };
    let b = open(cli, ctx).ok();
    // A brain holding a task list holds facts that are true, current and beside
    // the point on every prompt that is not about them. `--not-scope` is the
    // existing answer to that; a hook takes no flags, so it reads it from here.
    let not_scope = std::env::var("BRAIN_HOOK_NOT_SCOPE").ok();
    // `None` is silence, not an empty line -- see `hook::respond`.
    if let Some(out) =
        brain::hook::respond(what, b.as_ref(), &input, not_scope.as_deref(), args.format)
    {
        emit(&out);
    }
    Ok(())
}

/// Reads the harness's JSON from stdin.
///
/// A terminal is checked for first so that `brain hook recall` typed by hand
/// answers instead of hanging on a stdin nobody is going to close -- which is
/// exactly how somebody debugging their hook installation would run it.
fn hook_input() -> serde_json::Value {
    use std::io::IsTerminal as _;
    if std::io::stdin().is_terminal() {
        return serde_json::Value::Null;
    }
    let Ok(raw) = std::io::read_to_string(std::io::stdin().lock()) else {
        return serde_json::Value::Null;
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
        return v;
    }
    // Not JSON, so this is a harness that hands over the prompt as text. Read it
    // as the prompt rather than as nothing.
    //
    // The alternative was silence, and silence is the worst answer available here:
    // a hook never fails and never explains, so a harness whose input shape was
    // not understood would install cleanly, run on every prompt, and do nothing --
    // indistinguishable from a brain that had nothing to say. Guessing at *which*
    // harness this is would be overreach; noticing that somebody sent a prompt is
    // not.
    match raw.trim() {
        "" => serde_json::Value::Null,
        text => serde_json::json!({ "prompt": text }),
    }
}

fn cmd_link(args: &LinkArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let at = args.at.as_deref().map(parse_when).transpose()?;
    let b = open(cli, ctx)?;
    let outcome = b.link(&args.from, &args.rel, &args.to, at)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "outcome": outcome.kind(), "fact": outcome.fact() }),
        ))?);
    } else {
        emit(&format!("{}: {}", outcome.kind(), outcome.fact().statement));
    }
    Ok(())
}

fn cmd_get(args: &GetArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let facts = match args.as_of.as_deref() {
        Some(w) => b
            .as_of(&args.subject, &args.predicate, parse_when(w)?)?
            .into_iter()
            .collect::<Vec<_>>(),
        None => b.current_all(&args.subject, &args.predicate)?,
    };

    if cli.json {
        // `fact` is the single answer callers usually want; `facts` carries them
        // all, which is what a multi-valued predicate needs.
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "fact": facts.first(), "facts": facts }),
        ))?);
    } else if facts.is_empty() {
        emit("(nothing known)");
    } else {
        for f in &facts {
            emit(&f.statement);
        }
    }
    Ok(())
}

fn cmd_recall(args: &RecallArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let mut q = RecallQuery::new(&args.query).limit(args.limit);
    if let Some(w) = &args.as_of {
        q = q.as_of(parse_when(w)?);
    }
    if args.history {
        q = q.history();
    }
    if let Some(s) = &args.scope {
        q = q.scope(s);
    }
    if let Some(s) = &args.not_scope {
        q = q.not_scope(s);
    }
    if let Some(c) = &args.channels {
        q = q.channels(c);
    }
    if args.explain {
        q = q.explaining();
    }

    let b = open(cli, ctx)?;
    let hits = b.recall(&q)?;

    // Learning happens after the answer is settled, and never influences it: the
    // ranking the caller sees is the ranking a replay would produce.
    let learned = if args.learn {
        b.learn_alias(&args.query, &hits)?
    } else {
        None
    };

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "query": args.query, "hits": hits, "learned": learned }),
        ))?);
    } else {
        if hits.is_empty() {
            emit("(nothing known)");
        }
        for h in &hits {
            emit(&h.fact.statement);
            if let Some(x) = &h.explain {
                emit(&format!("    score {:.5}", h.score));
                let votes: Vec<String> = x
                    .votes
                    .iter()
                    .map(|v| format!("{} #{} +{:.5}", v.channel.as_str(), v.rank, v.points))
                    .collect();
                emit(&format!(
                    "    votes {}  (fused {:.5})",
                    votes.join(", "),
                    x.fused
                ));
                if !x.demotions.is_empty() {
                    let rules: Vec<String> = x
                        .demotions
                        .iter()
                        .map(|d| format!("x{:.2} {}", d.factor, d.rule.as_str()))
                        .collect();
                    emit(&format!("    rules {}", rules.join(", ")));
                }
            }
        }
        if let Some(l) = &learned {
            emit(&format!("(learned: {:?} names {})", l.alias, l.entity));
        }
    }
    Ok(())
}

/// Lists every record whose text contains a string.
///
/// Same output contract as `which`, because it makes the same promise: these are
/// all of them, and the count says so when they are not.
fn cmd_find(args: &brain::cli::FindArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let mut q = FindQuery::new(&args.needle).limit(args.limit);
    if let Some(w) = &args.as_of {
        q = q.when(When::AsOf(parse_when(w)?));
    }
    if args.history {
        q = q.when(When::History);
    }
    if let Some(s) = &args.scope {
        q = q.scope(s);
    }
    if let Some(s) = &args.not_scope {
        q = q.not_scope(s);
    }

    let b = open(cli, ctx)?;
    let found = b.find(&q)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "needle": args.needle,
                "matched": found.matched,
                "truncated": found.truncated,
                "facts": found.facts,
            }),
        ))?);
    } else {
        if found.facts.is_empty() {
            emit("(nothing mentions that)");
        }
        for h in &found.facts {
            emit(&h.fact.statement);
        }
        if found.truncated {
            emit(&format!(
                "({} of {} -- raise --limit to see the rest)",
                found.facts.len(),
                found.matched
            ));
        }
    }
    Ok(())
}

/// Lists the properties this brain records.
///
/// The read that makes `which` usable. `which` starts from a predicate key, and
/// until this existed there was no way to learn which keys a brain holds short of
/// opening the file in a SQLite shell -- which is exactly what agents did.
fn cmd_predicates(cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let rows = b.predicates()?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "predicates": rows }),
        ))?);
    } else if rows.is_empty() {
        emit("(this brain records nothing yet)");
    } else {
        for p in &rows {
            let kind = if p.relational { "relation" } else { "literal" };
            emit(&format!(
                "{:<24} {:<8} {:<9} {} facts, {} subjects",
                p.key,
                p.cardinality.as_str(),
                kind,
                p.facts,
                p.subjects
            ));
        }
    }
    Ok(())
}

/// Lists which subjects hold a predicate.
///
/// Prints one statement per line, exactly like `get` and `recall`, so the three
/// reads stay interchangeable in a pipe. The count is only mentioned when the
/// limit cut the answer short -- otherwise the lines *are* the count, and saying
/// so twice invites the reader to wonder which number to trust.
fn cmd_which(args: &brain::cli::WhichArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let mut q = WhichQuery::new(&args.predicate)
        .order(args.order)
        .desc(args.desc)
        .limit(args.limit);
    if let Some(v) = &args.value {
        q = q.value(Object::parse_literal(v));
    }
    if let Some(e) = &args.entity {
        q = q.value(Object::entity(e));
    }
    if let Some(w) = &args.as_of {
        q = q.when(When::AsOf(parse_when(w)?));
    }
    if args.history {
        q = q.when(When::History);
    }
    if let Some(s) = &args.scope {
        q = q.scope(s);
    }

    let b = open(cli, ctx)?;
    let set = b.which(&q)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "predicate": args.predicate,
                "matched": set.matched,
                "truncated": set.truncated,
                "facts": set.facts,
            }),
        ))?);
    } else {
        if set.facts.is_empty() {
            emit("(nothing matches)");
        }
        for f in &set.facts {
            emit(&f.statement);
        }
        if set.truncated {
            emit(&format!(
                "({} of {} -- raise --limit to see the rest)",
                set.facts.len(),
                set.matched
            ));
        }
    }
    Ok(())
}

fn cmd_alias(args: &AliasArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;

    let (body, line) = match (&args.alias, args.forget) {
        (Some(a), true) => {
            let gone = b.forget_alias(&args.entity, a)?;
            (
                serde_json::json!({ "entity": args.entity, "alias": a, "forgotten": gone }),
                if gone {
                    format!("{a:?} no longer names {}", args.entity)
                } else {
                    format!("{a:?} did not name {}", args.entity)
                },
            )
        }
        (Some(a), false) => {
            let alias = b.declare_alias(&args.entity, a)?;
            (
                serde_json::json!({ "entity": args.entity, "alias": alias }),
                format!("{:?} now names {}", alias.key, args.entity),
            )
        }
        (None, _) => {
            let all = b.aliases(&args.entity)?;
            let listing = if all.is_empty() {
                "(no other names)".to_string()
            } else {
                all.iter()
                    .map(|a| format!("{} ({}, {:.2})", a.key, a.source.as_str(), a.weight))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            (
                serde_json::json!({ "entity": args.entity, "aliases": all }),
                listing,
            )
        }
    };

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(&b, body))?);
    } else {
        emit(&line);
    }
    Ok(())
}

/// Hands the brain to an agent over stdio.
///
/// Nothing is printed here, and nothing may be: from this point stdout carries
/// JSON-RPC frames. The brain is resolved before the transport starts, so a
/// missing brain is an ordinary error on stderr rather than a client that
/// connects and then fails every call.
#[cfg(feature = "mcp")]
fn cmd_serve(cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    tracing::info!(brain = %b.store().label(), "serving MCP over stdio");
    brain::mcp::serve(b)
}

/// Writes the brain out as one file.
///
/// Two renderings of the same contents, for two different readers. The HTML page
/// is a photograph -- it carries `live: false`, so it draws the graph and the
/// whole timeline but shows no editor, and nothing in it can drift out of date
/// silently because nothing in it claims to be current. The Markdown is for the
/// reader the page cannot serve: whoever has to review a *change* to the brain,
/// in a diff, next to the code it describes. See [`brain::md`].
fn cmd_export(args: &brain::cli::ExportArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    if args.markdown {
        return cmd_export_markdown(args, cli, ctx);
    }
    #[cfg(not(feature = "studio"))]
    anyhow::bail!("this build has no studio; pass --markdown");
    #[cfg(feature = "studio")]
    cmd_export_html(args, cli, ctx)
}

/// The brain as text, for a pull request.
fn cmd_export_markdown(args: &brain::cli::ExportArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let text = brain::md::render(b.store().conn(), b.store().label())?;

    if args.stdout {
        emit(&text);
        return Ok(());
    }
    let out = args.out.clone().unwrap_or_else(|| ctx.cwd.join("brain.md"));
    std::fs::write(&out, &text)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "exported": out, "bytes": text.len(), "format": "markdown" }),
        ))?);
    } else {
        emit(&format!(
            "exported to {} ({} lines)",
            out.display(),
            text.lines().count()
        ));
    }
    Ok(())
}

#[cfg(feature = "studio")]
fn cmd_export_html(args: &brain::cli::ExportArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let snap = brain::studio::Snapshot::capture(&b, false)?;
    let html = brain::studio::render_page(&snap)?;

    if args.stdout {
        emit(&html);
        return Ok(());
    }

    let out = args
        .out
        .clone()
        .unwrap_or_else(|| ctx.cwd.join("brain-studio.html"));
    std::fs::write(&out, &html)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "exported": out,
                "bytes": html.len(),
                "entities": snap.entities.len(),
                "facts": snap.facts.len(),
            }),
        ))?);
    } else {
        emit(&format!(
            "exported {} entities and {} facts to {} ({} KB)",
            snap.entities.len(),
            snap.facts.len(),
            out.display(),
            html.len() / 1024,
        ));
    }
    Ok(())
}

/// Serves the studio until interrupted.
#[cfg(feature = "studio")]
fn cmd_studio(args: &brain::cli::StudioArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let label = b.store().label().to_string();
    let studio = brain::studio::server::Studio::bind(b, args.port)?;
    let url = studio.url();

    // The URL is a capability -- it carries the token -- so it goes to the
    // user's own output and nowhere else. Emitted before `run` blocks.
    emit(&format!("brain studio: {label}"));
    emit(&format!("  {url}"));
    emit("  (the token in that URL authorizes edits, and changes on every restart)");

    if !args.no_open {
        brain::studio::server::open_in_browser(&url);
    }
    studio.run()?;
    Ok(())
}

fn cmd_reindex(cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let n = b.reindex()?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "reindexed": n }),
        ))?);
    } else {
        emit(&format!("reindexed {n} embeddings"));
    }
    Ok(())
}

fn cmd_history(args: &GetArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let facts = b.history(&args.subject, &args.predicate)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "facts": facts }),
        ))?);
    } else if facts.is_empty() {
        emit("(nothing known)");
    } else {
        for f in &facts {
            // Where the interval sits relative to now, which is the only thing a
            // reader is deciding from. Two facts with an identical `valid_to` are
            // not the same news depending on which side of now it falls on, and a
            // fact whose validity has not started is not "current" however new it
            // is -- printing it as such is what let `history` and `get` disagree.
            let now = jiff::Timestamp::now();
            let state = if f.retracted_at.is_some() {
                "retracted".to_string()
            } else if f.valid_from > now {
                "not yet true".to_string()
            } else {
                match f.valid_to {
                    Some(to) if to <= now => format!("until {to}"),
                    Some(to) => format!("expires {to}"),
                    None => "current".to_string(),
                }
            };
            emit(&format!("[{}] {}  ({state})", f.valid_from, f.statement));
        }
    }
    Ok(())
}

fn cmd_entity(args: &EntityArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let when = match args.as_of.as_deref() {
        Some(w) => When::AsOf(parse_when(w)?),
        None => When::Now,
    };
    // Depth zero unless asked: walking is the expensive part, and `brain entity`
    // is also how one just looks something up.
    let depth = if args.neighbors { args.depth } else { 0 };

    let b = open(cli, ctx)?;
    let Some(view) = b.entity(&args.name, when, depth)? else {
        if cli.json {
            emit(&serde_json::to_string_pretty(&answer(
                &b,
                serde_json::json!({ "entity": serde_json::Value::Null }),
            ))?);
        } else {
            emit(&format!("(no entity named {:?})", args.name));
        }
        return Ok(());
    };

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "entity": view }),
        ))?);
    } else {
        emit(&format!("{} ({})", view.label, view.key));
        for a in &view.aliases {
            emit(&format!("  also: {} ({})", a.key, a.source.as_str()));
        }
        for f in &view.facts {
            emit(&format!("  {}", f.statement));
        }
        if args.neighbors {
            emit("  neighbours:");
            if view.neighbours.is_empty() {
                emit("    (none)");
            }
            for n in &view.neighbours {
                let hop = if n.hops == 1 { "hop" } else { "hops" };
                emit(&format!(
                    "    {} ({} {hop}, via {})",
                    n.entity, n.hops, n.via
                ));
            }
        }
    }
    Ok(())
}

fn cmd_why(fact_id: i64, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let p = b.why(fact_id)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "fact": p.fact,
                "superseded_by": p.superseded_by,
                "supersedes": p.supersedes,
            }),
        ))?);
    } else {
        emit(&p.fact.statement);
        emit(&format!("  recorded: {}", p.fact.recorded_at));
        if let Some(s) = &p.fact.source {
            emit(&format!("  source:   {s}"));
        }
        if let Some(n) = &p.superseded_by {
            emit(&format!("  replaced by: {}", n.statement));
        }
        if let Some(pr) = &p.supersedes {
            emit(&format!("  replaced:    {}", pr.statement));
        }
    }
    Ok(())
}

fn cmd_retract(fact_id: i64, reason: Option<&str>, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    let f = b.retract(fact_id, reason)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "retracted": f }),
        ))?);
    } else {
        emit(&format!("retracted: {}", f.statement));
    }
    Ok(())
}

fn cmd_predicate(args: &brain::cli::PredicateArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    if args.cardinality.is_none() && args.relational.is_none() {
        anyhow::bail!("pass --cardinality or --relational");
    }
    let b = open(cli, ctx)?;
    if let Some(c) = args.cardinality {
        b.set_cardinality(&args.name, c)?;
    }
    if let Some(r) = args.relational {
        b.set_relational(&args.name, r)?;
    }

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "predicate": args.name,
                "cardinality": args.cardinality,
                "relational": args.relational,
            }),
        ))?);
    } else {
        if let Some(c) = args.cardinality {
            emit(&format!("{} is now {}-valued", args.name, c.as_str()));
        }
        if let Some(r) = args.relational {
            emit(&format!(
                "{} now stores its object as {}",
                args.name,
                if r { "an entity" } else { "a literal" }
            ));
            if r {
                emit("existing facts are unchanged -- run `brain repair --relations` for those");
            }
        }
    }
    Ok(())
}

fn cmd_repair(args: &brain::cli::RepairArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    if !args.relations {
        anyhow::bail!("pass --relations (the only repair there is so far)");
    }
    let b = open(cli, ctx)?;
    let report = brain::repair::relations(b.store().conn(), args.apply)?;

    if cli.json {
        let mut body = serde_json::to_value(&report)?;
        if let Some(map) = body.as_object_mut() {
            map.insert("promotions_count".into(), report.promotions.len().into());
        }
        emit(&serde_json::to_string_pretty(&answer(&b, body))?);
    } else {
        for line in report.lines() {
            emit(&line);
        }
    }
    Ok(())
}

fn cmd_init(args: &InitArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let target = args.target(cli, ctx);
    let label = args.label_or_default(&target);

    // Claim the catalogue name *before* creating anything, so a rejected
    // registration never leaves an orphan brain on disk.
    let cfg_path = ctx.config_dir.join("config.toml");
    let mut cfg = Config::load(&cfg_path)?;
    if let Some(name) = &args.name {
        cfg.register(name, &target)?;
    }

    let store = Store::init(&target, &label, &SystemClock, &UuidV7Gen)?;

    if args.name.is_some() {
        cfg.save(&cfg_path)?;
    }

    if cli.json {
        emit(&serde_json::to_string_pretty(&store.identity())?);
    } else {
        emit(&format!(
            "created brain {:?} at {}",
            label,
            target.display()
        ));
        if let Some(name) = &args.name {
            emit(&format!("registered as {name:?}"));
        }
    }
    Ok(())
}

fn cmd_where(cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let found = brain::cli::select(cli, ctx)?;
    let exists = found.path.is_file();

    if cli.json {
        emit(&serde_json::to_string_pretty(&serde_json::json!({
            "brain_path": found.path,
            "exists": exists,
            "reason": found.origin.explain(),
        }))?);
    } else {
        emit(&format!("{}", found.path.display()));
        emit(&format!("  reason: {}", found.origin.explain()));
        if !exists {
            emit("  status: does not exist yet -- run `brain init` to create it");
        }
    }
    Ok(())
}

fn cmd_stats(cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
    let found = brain::cli::select(cli, ctx)?;
    let store = Store::open(&found.path)?;
    let report = brain::lint::check(store.conn())?;

    let mut out = store.identity();
    out["created_at"] = serde_json::json!(store.created_at().to_string());
    out["schema"] = serde_json::json!(store.schema_version());
    out["entities"] = serde_json::json!(report.entities);
    out["facts"] = serde_json::json!(report.facts);
    out["relations"] = serde_json::json!(report.edges);
    out["unreachable_entities"] = serde_json::json!(report.orphans.len());

    if cli.json {
        emit(&serde_json::to_string_pretty(&out)?);
    } else {
        emit(&format!("{} ({})", store.label(), store.path().display()));
        emit(&format!("  id:      {}", store.id()));
        emit(&format!("  created: {}", store.created_at()));
        // Reported next to the identity because it is the only thing that tells
        // two `brain` builds apart: the crate version does not move when the
        // on-disk layout does, so `--version` cannot answer "is my binary old".
        emit(&format!("  schema:  v{}", store.schema_version()));
        emit(&format!(
            "  holds:   {} entities, {} facts, {} of them relations",
            report.entities, report.facts, report.edges
        ));
        // Surfaced here and not only in `lint` because this is the command
        // someone runs to see how the brain is doing, and a brain whose entities
        // cannot reach each other is not doing well.
        if !report.orphans.is_empty() {
            emit(&format!(
                "  warning: {} entities have no open relation -- run `brain lint`",
                report.orphans.len()
            ));
        }
    }
    Ok(())
}

fn cmd_lint(
    args: &brain::cli::LintArgs,
    cli: &Cli,
    ctx: &Ctx,
) -> anyhow::Result<std::process::ExitCode> {
    let found = brain::cli::select(cli, ctx)?;
    let store = Store::open(&found.path)?;
    let report = brain::lint::check(store.conn())?;

    if cli.json {
        let mut out = store.identity();
        if let Some(map) = serde_json::to_value(&report)?.as_object() {
            for (k, v) in map {
                out[k] = v.clone();
            }
        }
        out["clean"] = serde_json::json!(report.is_clean());
        emit(&serde_json::to_string_pretty(&out)?);
    } else {
        for line in report.lines() {
            emit(&line);
        }
    }

    Ok(if args.strict && !report.is_clean() {
        std::process::ExitCode::FAILURE
    } else {
        std::process::ExitCode::SUCCESS
    })
}

/// The single sanctioned path to stdout.
#[allow(clippy::print_stdout)]
fn emit(line: &str) {
    println!("{line}");
}

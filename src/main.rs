// Same rule as the library: stdout belongs to the user's output (and, in MCP
// mode, to the JSON-RPC transport). Diagnostics go to stderr.
#![deny(clippy::print_stdout, clippy::dbg_macro)]

use brain::brain::{Assertion, Brain, Object, WhichQuery};
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
        Cmd::Which(args) => cmd_which(args, &cli, &ctx),
        Cmd::History(args) => cmd_history(args, &cli, &ctx),
        Cmd::Entity(args) => cmd_entity(args, &cli, &ctx),
        Cmd::Alias(args) => cmd_alias(args, &cli, &ctx),
        Cmd::Reindex => cmd_reindex(&cli, &ctx),
        #[cfg(feature = "mcp")]
        Cmd::Serve => cmd_serve(&cli, &ctx),
        #[cfg(feature = "studio")]
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

    let mut a = Assertion::new(&args.subject, &args.predicate, object);
    a.valid_from = at;
    a.valid_to = until;
    a.source = args.source.clone();
    a.locator = locator;
    a.confidence = args.confidence;
    a.scope = args.scope.clone();
    a.cardinality = args.cardinality;

    let b = open(cli, ctx)?;
    let outcome = b.remember(&a)?;

    // Looked up after the write, and only for a literal: the write is not in
    // doubt. This is the warning that never came the 59 times a relation was
    // recorded as a string, and nothing else will ever raise it, because nothing
    // failed.
    let hint = match args.entity {
        Some(_) => None,
        None => brain::lint::missed_relation(b.store().conn(), &brain::norm::key(&args.predicate))?,
    };

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "outcome": outcome.kind(),
                "fact": outcome.fact(),
                "hint": hint,
            }),
        ))?);
    } else {
        emit(&format!("{}: {}", outcome.kind(), outcome.fact().statement));
        if let Some(h) = &hint {
            emit(&format!("warning: {h}"));
        }
    }
    Ok(())
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
        }
        if let Some(l) = &learned {
            emit(&format!("(learned: {:?} names {})", l.alias, l.entity));
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

/// Writes the brain as one HTML file.
///
/// A photograph, explicitly: the page carries `live: false`, so it renders the
/// graph and the whole timeline but shows no editor. Nothing in it can drift out
/// of date silently, because nothing in it claims to be current.
#[cfg(feature = "studio")]
fn cmd_export(args: &brain::cli::ExportArgs, cli: &Cli, ctx: &Ctx) -> anyhow::Result<()> {
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

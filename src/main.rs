// Same rule as the library: stdout belongs to the user's output (and, in MCP
// mode, to the JSON-RPC transport). Diagnostics go to stderr.
#![deny(clippy::print_stdout, clippy::dbg_macro)]

use brain::brain::{Assertion, Brain, Object};
use brain::cli::{Cli, Cmd, GetArgs, InitArgs, LinkArgs, RememberArgs, parse_when};
use brain::clock::SystemClock;
use brain::config::Config;
use brain::ids::UuidV7Gen;
use brain::locate::Ctx;
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
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<()> {
    let ctx = Ctx::from_process()?;
    match &cli.cmd {
        Cmd::Init(args) => cmd_init(args, &cli, &ctx),
        Cmd::Where => cmd_where(&cli, &ctx),
        Cmd::Stats => cmd_stats(&cli, &ctx),
        Cmd::Remember(args) => cmd_remember(args, &cli, &ctx),
        Cmd::Link(args) => cmd_link(args, &cli, &ctx),
        Cmd::Get(args) => cmd_get(args, &cli, &ctx),
        Cmd::History(args) => cmd_history(args, &cli, &ctx),
        Cmd::Why { fact_id } => cmd_why(*fact_id, &cli, &ctx),
        Cmd::Retract { fact_id, reason } => cmd_retract(*fact_id, reason.as_deref(), &cli, &ctx),
        Cmd::Predicate { name, cardinality } => cmd_predicate(name, *cardinality, &cli, &ctx),
    }
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
    let locator = args
        .locator
        .as_deref()
        .map(serde_json::from_str::<serde_json::Value>)
        .transpose()
        .map_err(|e| anyhow::anyhow!("--locator is not valid JSON: {e}"))?;

    let object = match (&args.value, &args.entity) {
        (_, Some(e)) => Object::entity(e),
        // A bare `--value 10` means the number ten, not the string "10": numeric
        // facts are what the temporal model is mostly for.
        (Some(v), None) => match v.parse::<f64>() {
            Ok(n) => {
                let o = Object::num(n);
                match &args.unit {
                    Some(u) => o.with_unit(u),
                    None => o,
                }
            }
            Err(_) => Object::text(v),
        },
        (None, None) => anyhow::bail!("pass --value or --entity"),
    };

    let mut a = Assertion::new(&args.subject, &args.predicate, object);
    a.valid_from = at;
    a.source = args.source.clone();
    a.locator = locator;
    a.confidence = args.confidence;
    a.scope = args.scope.clone();
    a.cardinality = args.cardinality;

    let b = open(cli, ctx)?;
    let outcome = b.remember(&a)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({
                "outcome": outcome.kind(),
                "fact": outcome.fact(),
            }),
        ))?);
    } else {
        emit(&format!("{}: {}", outcome.kind(), outcome.fact().statement));
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
            let until = match (f.retracted_at, f.valid_to) {
                (Some(_), _) => "retracted".to_string(),
                (None, Some(to)) => format!("until {to}"),
                (None, None) => "current".to_string(),
            };
            emit(&format!("[{}] {}  ({until})", f.valid_from, f.statement));
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

fn cmd_predicate(
    name: &str,
    cardinality: brain::brain::Cardinality,
    cli: &Cli,
    ctx: &Ctx,
) -> anyhow::Result<()> {
    let b = open(cli, ctx)?;
    b.set_cardinality(name, cardinality)?;

    if cli.json {
        emit(&serde_json::to_string_pretty(&answer(
            &b,
            serde_json::json!({ "predicate": name, "cardinality": cardinality }),
        ))?);
    } else {
        emit(&format!("{name} is now {}-valued", cardinality.as_str()));
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

    let mut out = store.identity();
    out["created_at"] = serde_json::json!(store.created_at().to_string());

    if cli.json {
        emit(&serde_json::to_string_pretty(&out)?);
    } else {
        emit(&format!("{} ({})", store.label(), store.path().display()));
        emit(&format!("  id:      {}", store.id()));
        emit(&format!("  created: {}", store.created_at()));
    }
    Ok(())
}

/// The single sanctioned path to stdout.
#[allow(clippy::print_stdout)]
fn emit(line: &str) {
    println!("{line}");
}

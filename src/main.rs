// Same rule as the library: stdout belongs to the user's output (and, in MCP
// mode, to the JSON-RPC transport). Diagnostics go to stderr.
#![deny(clippy::print_stdout, clippy::dbg_macro)]

use brain::cli::{Cli, Cmd, InitArgs};
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
    }
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

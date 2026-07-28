//! Command-line surface.
//!
//! Every command accepts the same brain selectors, and every JSON payload
//! carries the brain's identity so an agent can never confuse two brains.

use crate::brain::Cardinality;
use crate::locate::{Ctx, Selection};
use clap::{Args, Parser, Subcommand};
use jiff::Timestamp;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "brain", version, about = "Bitemporal memory for AI agents", long_about = None)]
pub struct Cli {
    #[command(flatten)]
    pub select: BrainSelector,

    /// Emit machine-readable JSON.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub cmd: Cmd,
}

/// Which brain to operate on. Conflicts are rejected by
/// [`crate::locate::resolve`] rather than by clap, so the rule lives in exactly
/// one place and is testable without spawning a process.
#[derive(Args, Debug, Clone, Default)]
pub struct BrainSelector {
    /// Path to a brain file. Wins over every other selector.
    #[arg(long, global = true, value_name = "PATH")]
    pub brain: Option<PathBuf>,

    /// Name of a brain from the config catalogue.
    #[arg(long = "use", global = true, value_name = "NAME")]
    pub use_name: Option<String>,

    /// The global brain.
    #[arg(long, global = true)]
    pub global: bool,
}

impl From<&BrainSelector> for Selection {
    fn from(s: &BrainSelector) -> Self {
        Selection {
            brain: s.brain.clone(),
            use_name: s.use_name.clone(),
            global: s.global,
        }
    }
}

#[derive(Subcommand, Debug)]
pub enum Cmd {
    /// Create a new brain.
    Init(InitArgs),

    /// Show which brain would be used here, and why.
    Where,

    /// Report the brain's identity and contents.
    Stats,

    /// Record a fact.
    Remember(RememberArgs),

    /// Record a relation between two entities.
    Link(LinkArgs),

    /// Read the current value, or the value at a past instant.
    Get(GetArgs),

    /// Search the brain with a natural-language question.
    Recall(RecallArgs),

    /// Show the full trajectory of a subject/predicate pair.
    History(GetArgs),

    /// Show what is known about an entity, and what it connects to.
    Entity(EntityArgs),

    /// Show where a fact came from and what became of it.
    Why { fact_id: i64 },

    /// Mark a fact as never having been true.
    Retract {
        fact_id: i64,
        #[arg(long)]
        reason: Option<String>,
    },

    /// Give an entity another name, list the names it has, or take one away.
    Alias(AliasArgs),

    /// Rebuild the vector index from the stored embeddings.
    Reindex,

    /// Speak MCP over stdio, so an agent can use this brain as a tool.
    #[cfg(feature = "mcp")]
    Serve,

    /// Fix a predicate's cardinality.
    Predicate {
        name: String,
        #[arg(long, value_parser = parse_cardinality)]
        cardinality: Cardinality,
    },
}

#[derive(Args, Debug)]
pub struct RememberArgs {
    #[arg(long)]
    pub subject: String,
    #[arg(long)]
    pub predicate: String,

    /// A literal value. Parsed as a number when it looks like one, else as text.
    #[arg(long, conflicts_with = "entity")]
    pub value: Option<String>,

    /// Another entity, making this fact an edge in the graph.
    #[arg(long, conflicts_with = "value")]
    pub entity: Option<String>,

    #[arg(long)]
    pub unit: Option<String>,

    /// When this became true. Defaults to now. Accepts `2026-07-28` or RFC 3339.
    #[arg(long, value_name = "WHEN")]
    pub at: Option<String>,

    #[arg(long)]
    pub source: Option<String>,

    /// JSON locator: where the answer actually lives, e.g.
    /// `{"file":"src/pricing.rs","lines":"40-52"}`.
    #[arg(long)]
    pub locator: Option<String>,

    #[arg(long)]
    pub confidence: Option<f64>,

    #[arg(long)]
    pub scope: Option<String>,

    #[arg(long, value_parser = parse_cardinality)]
    pub cardinality: Option<Cardinality>,
}

#[derive(Args, Debug)]
pub struct LinkArgs {
    pub from: String,
    pub rel: String,
    pub to: String,
    #[arg(long, value_name = "WHEN")]
    pub at: Option<String>,
}

#[derive(Args, Debug)]
pub struct RecallArgs {
    /// The question, in whatever words the caller has.
    pub query: String,

    /// Answer as of this instant instead of with what currently holds.
    #[arg(long, value_name = "WHEN", conflicts_with = "history")]
    pub as_of: Option<String>,

    /// Return the whole trajectory, closed intervals included.
    #[arg(long)]
    pub history: bool,

    #[arg(long, default_value_t = 10)]
    pub limit: usize,

    #[arg(long)]
    pub scope: Option<String>,

    /// Let the brain keep the name this question used, if the answer is
    /// unambiguous. Off by default: a read that writes is a read that cannot be
    /// replayed.
    #[arg(long)]
    pub learn: bool,
}

#[derive(Args, Debug)]
pub struct AliasArgs {
    /// The entity being named. Must already exist.
    pub entity: String,

    /// The name to add. Omit to list the names this entity already answers to.
    pub alias: Option<String>,

    /// Remove the name instead of adding it.
    #[arg(long, requires = "alias")]
    pub forget: bool,
}

#[derive(Args, Debug)]
pub struct EntityArgs {
    pub name: String,

    /// Also walk the relations out from it.
    #[arg(long)]
    pub neighbors: bool,

    /// How many hops to walk. Only meaningful with --neighbors.
    #[arg(long, default_value_t = crate::graph::MAX_DEPTH, value_name = "N")]
    pub depth: u32,

    /// Show the neighbourhood as it stood at this instant.
    #[arg(long, value_name = "WHEN")]
    pub as_of: Option<String>,
}

#[derive(Args, Debug)]
pub struct GetArgs {
    pub subject: String,
    pub predicate: String,
    /// Answer as of this instant instead of using the latest known value.
    #[arg(long, value_name = "WHEN")]
    pub as_of: Option<String>,
}

fn parse_cardinality(s: &str) -> Result<Cardinality, String> {
    Cardinality::parse(s).ok_or_else(|| format!("expected `single` or `multi`, got {s:?}"))
}

/// Accepts a bare date as well as a full RFC 3339 instant, because `--at
/// 2026-07-28` is what anyone actually types.
pub fn parse_when(s: &str) -> anyhow::Result<Timestamp> {
    if let Ok(t) = s.parse::<Timestamp>() {
        return Ok(t);
    }
    if let Ok(d) = s.parse::<jiff::civil::Date>() {
        return Ok(d.to_zoned(jiff::tz::TimeZone::UTC)?.timestamp());
    }
    anyhow::bail!("could not read {s:?} as a date or timestamp (try `2026-07-28`)")
}

#[derive(Args, Debug)]
pub struct InitArgs {
    /// Where to create it. Defaults to ./.brain/brain.db.
    pub path: Option<PathBuf>,

    /// Human-readable name for this brain, shown in every answer.
    #[arg(long)]
    pub label: Option<String>,

    /// Also register it in the config catalogue under this name.
    #[arg(long)]
    pub name: Option<String>,
}

impl InitArgs {
    /// Where the brain will be created.
    ///
    /// Deliberately simpler than the lookup ladder: an explicit path, or the
    /// global data dir, or the local `.brain/`. `init` never searches upward --
    /// creating a brain is not something to guess at.
    pub fn target(&self, cli: &Cli, ctx: &Ctx) -> PathBuf {
        if let Some(p) = &self.path {
            return if p.is_absolute() {
                p.clone()
            } else {
                ctx.cwd.join(p)
            };
        }
        if cli.select.global {
            return ctx.data_dir.join("brain.db");
        }
        ctx.cwd.join(".brain").join("brain.db")
    }

    /// Falls back to the containing directory's name, which is nearly always
    /// what someone running a bare `brain init` in a project meant.
    pub fn label_or_default(&self, target: &std::path::Path) -> String {
        if let Some(l) = &self.label {
            return l.clone();
        }
        target
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n != ".brain"))
            .or_else(|| target.parent()?.parent())
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "brain".to_string())
    }
}

/// Resolves the brain for this invocation.
pub fn select(cli: &Cli, ctx: &Ctx) -> Result<crate::locate::BrainRef, crate::locate::LocateError> {
    crate::locate::resolve(&Selection::from(&cli.select), ctx)
}

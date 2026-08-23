//! Command-line surface.
//!
//! Every command accepts the same brain selectors, and every JSON payload
//! carries the brain's identity so an agent can never confuse two brains.

use crate::brain::{Cardinality, Order};
use crate::locate::{Ctx, Selection};
use crate::recall::Channel;
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

    /// Report what is structurally wrong: relations stored as strings,
    /// entities nothing can reach, one thing living under two names.
    Lint(LintArgs),

    /// Record a fact.
    Remember(RememberArgs),

    /// Record a relation between two entities.
    Link(LinkArgs),

    /// Read the current value, or the value at a past instant.
    Get(GetArgs),

    /// Search the brain with a natural-language question.
    Recall(RecallArgs),

    /// List every record whose text contains a string.
    Find(FindArgs),

    /// List which subjects hold a predicate, and what the value is.
    Which(WhichArgs),

    /// List the properties this brain records, and how much it holds under each.
    Predicates,

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

    /// Answer a harness lifecycle hook, so the brain is read without anyone
    /// having to remember to ask it.
    ///
    /// Reads the harness's JSON on stdin and writes the harness's JSON on
    /// stdout. It never writes to the brain, never fails, and prints `{}` when
    /// there is nothing to say -- including in every directory that has no
    /// brain. `BRAIN_HOOK=off` in the environment turns it off without
    /// uninstalling anything.
    Hook(HookArgs),

    /// Speak MCP over stdio, so an agent can use this brain as a tool.
    #[cfg(feature = "mcp")]
    Serve,

    /// Write the brain to a single self-contained HTML file.
    #[cfg(feature = "studio")]
    Export(ExportArgs),

    /// Open the brain in a 3D viewer and editor, served from localhost.
    #[cfg(feature = "studio")]
    Studio(StudioArgs),

    /// Fix what a predicate is: how many values it holds, and whether its
    /// object names a thing rather than being a literal.
    Predicate(PredicateArgs),

    /// Repair how facts are stored, without changing what they say.
    Repair(RepairArgs),
}

#[derive(Args, Debug)]
pub struct RememberArgs {
    /// Read many facts from a file of JSON objects, one per line, or from `-`
    /// for stdin. A top-level JSON array is accepted too, because that is what
    /// anything generating the file will produce first.
    ///
    /// Every key is one of the flags below: `subject`, `predicate`, `value` or
    /// `entity`, `unit`, `at`, `until`, `source`, `locator`, `confidence`,
    /// `scope`, `cardinality`. An unknown key is an error naming the line, not a
    /// field quietly dropped -- a batch is written by a machine, and a machine
    /// does not notice that its typo went nowhere.
    ///
    /// The whole batch is one write: if any line is bad, nothing is recorded.
    #[arg(
        long,
        value_name = "PATH",
        conflicts_with_all = [
            "subject", "predicate", "value", "entity", "unit", "at", "until",
            "source", "locator", "confidence", "scope", "cardinality",
        ],
    )]
    pub batch: Option<PathBuf>,

    #[arg(long, required_unless_present = "batch")]
    pub subject: Option<String>,
    #[arg(long, required_unless_present = "batch")]
    pub predicate: Option<String>,

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

    /// When this stops being true, if that is already known. The fact closes
    /// itself at that instant instead of waiting for somebody to come back and
    /// supersede it -- which is what makes a short-lived claim safe to record.
    #[arg(long, value_name = "WHEN")]
    pub until: Option<String>,

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
pub struct HookArgs {
    /// `context` introduces the brain (attach to SessionStart), `recall`
    /// answers the prompt just typed (UserPromptSubmit), `flush` asks for what
    /// the session learned before it is lost (PreCompact, SessionEnd).
    #[arg(value_enum)]
    pub what: crate::hook::What,
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
pub struct PredicateArgs {
    pub name: String,

    #[arg(long, value_parser = parse_cardinality)]
    pub cardinality: Option<Cardinality>,

    /// Whether the object names another entity. `--relational` turns it on,
    /// `--relational false` pins it off so inference never turns it back on.
    #[arg(long, num_args = 0..=1, default_missing_value = "true")]
    pub relational: Option<bool>,
}

#[derive(Args, Debug)]
pub struct RepairArgs {
    /// Give relational predicates the entities their string objects only named.
    #[arg(long)]
    pub relations: bool,

    /// Actually write. Without this the command reports what it would do and
    /// changes nothing.
    #[arg(long)]
    pub apply: bool,
}

#[derive(Args, Debug)]
pub struct LintArgs {
    /// Exit non-zero when anything is found, so an agent or a CI step can gate
    /// on the brain staying connected.
    #[arg(long)]
    pub strict: bool,
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

    /// Keep a namespace out of the answer. Use it to stop something high-churn --
    /// a task list, a run of build states -- from competing with the durable
    /// knowledge on questions that have nothing to do with it.
    #[arg(long = "not-scope", value_name = "SCOPE")]
    pub not_scope: Option<String>,

    /// Let the brain keep the name this question used, if the answer is
    /// unambiguous. Off by default: a read that writes is a read that cannot be
    /// replayed.
    #[arg(long)]
    pub learn: bool,

    /// Which retrievers may answer, comma-separated: `bm25`, `alias`,
    /// `semantic`, `graph`, `kin`. All of them by default.
    ///
    /// Narrowing this is how a surprising ranking gets explained -- `--channels
    /// bm25` is the answer with no guessing in it at all.
    #[arg(long, value_delimiter = ',', value_parser = parse_channel)]
    pub channels: Option<Vec<Channel>>,

    /// Show the arithmetic: which channel voted at which rank, what the votes
    /// summed to, and which re-ranking rule multiplied the result.
    ///
    /// `--channels` answers *what found this*. It cannot answer *why this
    /// outranks that*, because the contest is settled between the channels and
    /// after them -- and re-running the question with a channel switched off is
    /// a bisection, not an explanation.
    #[arg(long)]
    pub explain: bool,
}

/// A literal-string search, filtered exactly the way `which` is.
///
/// No ranking flags, deliberately. This answers *which records mention this*,
/// and a ranked answer to that is a worse answer -- the caller is gathering
/// evidence, not asking a question.
#[derive(Args, Debug)]
pub struct FindArgs {
    /// The text to look for, taken literally. A phrase stays a phrase.
    pub needle: String,

    /// Search what held at this instant instead of what holds now.
    #[arg(long, value_name = "WHEN", conflicts_with = "history")]
    pub as_of: Option<String>,

    /// Include closed intervals, i.e. records that used to hold.
    #[arg(long)]
    pub history: bool,

    /// High for the same reason `which`'s is: the answer always reports how many
    /// matched, so a cut is visible rather than assumed.
    #[arg(long, default_value_t = 200)]
    pub limit: usize,

    #[arg(long)]
    pub scope: Option<String>,

    /// Keep a namespace out of the answer.
    #[arg(long = "not-scope", value_name = "SCOPE")]
    pub not_scope: Option<String>,
}

/// A set question, filtered the same way the fact was written.
///
/// `--value` and `--entity` mirror [`RememberArgs`] deliberately: whichever one
/// recorded the fact is the one that selects it back, so nobody has to remember
/// which column a value landed in.
#[derive(Args, Debug)]
pub struct WhichArgs {
    pub predicate: String,

    /// A literal value, matched exactly. Omit to list every subject holding the
    /// predicate at all.
    #[arg(conflicts_with = "entity")]
    pub value: Option<String>,

    /// Match an entity-valued object by identity rather than by spelling.
    #[arg(long, conflicts_with = "value")]
    pub entity: Option<String>,

    /// Answer as of this instant instead of with what currently holds.
    #[arg(long, value_name = "WHEN", conflicts_with = "history")]
    pub as_of: Option<String>,

    /// Return closed intervals too, not only what holds.
    #[arg(long)]
    pub history: bool,

    /// `subject`, `value` or `since`.
    #[arg(long = "order-by", default_value = "subject", value_parser = parse_order)]
    pub order: Order,

    #[arg(long)]
    pub desc: bool,

    /// Far higher than `recall`'s, because a list that silently stops at ten is
    /// worse than no list. The answer always reports how many matched, so a cut
    /// is visible rather than assumed.
    #[arg(long, default_value_t = 200)]
    pub limit: usize,

    #[arg(long)]
    pub scope: Option<String>,
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
#[cfg(feature = "studio")]
pub struct ExportArgs {
    /// Where to write it. Defaults to `brain-studio.html` in the working
    /// directory.
    #[arg(long, short, value_name = "PATH")]
    pub out: Option<PathBuf>,

    /// Write to stdout instead of a file.
    #[arg(long, conflicts_with = "out")]
    pub stdout: bool,
}

#[derive(Args, Debug)]
#[cfg(feature = "studio")]
pub struct StudioArgs {
    /// Port to listen on. 0 asks the OS for a free one.
    #[arg(long, default_value_t = 0)]
    pub port: u16,

    /// Do not try to open a browser.
    #[arg(long)]
    pub no_open: bool,
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

fn parse_order(s: &str) -> Result<Order, String> {
    Order::parse(s).ok_or_else(|| format!("expected `subject`, `value` or `since`, got {s:?}"))
}

fn parse_channel(s: &str) -> Result<Channel, String> {
    Channel::parse(s)
        .ok_or_else(|| format!("expected `bm25`, `alias`, `semantic`, `graph` or `kin`, got {s:?}"))
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

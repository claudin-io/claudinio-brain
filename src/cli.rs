//! Command-line surface.
//!
//! Every command accepts the same brain selectors, and every JSON payload
//! carries the brain's identity so an agent can never confuse two brains.

use crate::locate::{Ctx, Selection};
use clap::{Args, Parser, Subcommand};
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

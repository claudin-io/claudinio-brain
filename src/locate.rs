//! Deciding which brain to open, and being able to explain why.
//!
//! The isolation guarantee this project promises is only as strong as this
//! module. Two rules make it work:
//!
//! 1. **Every rung is explicit.** A brain is opened because a flag, an env var,
//!    a config entry or a `.brain/` directory pointed at it -- never because it
//!    happened to be lying around.
//! 2. **There is no silent fallback.** When nothing points at a brain we return
//!    [`LocateError::NotFound`]. Quietly opening the global brain is precisely
//!    how one company's data would end up in another's.

use crate::config::Config;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The ambient world, injected so tests never touch the real `$HOME`.
#[derive(Debug, Clone)]
pub struct Ctx {
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    /// Where the global `config.toml` lives.
    pub config_dir: PathBuf,
    /// Default home of the `--global` brain.
    pub data_dir: PathBuf,
}

impl Ctx {
    /// Builds a context from the real process environment.
    ///
    /// `BRAIN_CONFIG_DIR` and `BRAIN_DATA_DIR` override the platform defaults.
    /// These exist so process-level tests stay hermetic: on macOS `directories`
    /// resolves to `~/Library/...` and ignores XDG, so without an explicit
    /// override a CLI test would read and write the developer's real config.
    pub fn from_process() -> anyhow::Result<Self> {
        let env: BTreeMap<String, String> = std::env::vars().collect();
        let dirs = directories::ProjectDirs::from("", "", "brain");
        let pick = |key: &str, fallback: Option<&Path>| -> anyhow::Result<PathBuf> {
            match env.get(key).filter(|s| !s.is_empty()) {
                Some(v) => Ok(PathBuf::from(v)),
                None => fallback.map(Path::to_path_buf).ok_or_else(|| {
                    anyhow::anyhow!("could not determine platform directories; set {key}")
                }),
            }
        };
        Ok(Self {
            cwd: std::env::current_dir()?,
            config_dir: pick("BRAIN_CONFIG_DIR", dirs.as_ref().map(|d| d.config_dir()))?,
            data_dir: pick("BRAIN_DATA_DIR", dirs.as_ref().map(|d| d.data_dir()))?,
            env,
        })
    }

    fn global_config_path(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }
}

/// What the caller asked for on the command line.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    /// `--brain <path>`
    pub brain: Option<PathBuf>,
    /// `--use <name>`
    pub use_name: Option<String>,
    /// `--global`
    pub global: bool,
}

/// Why a particular brain was chosen. Drives `brain where`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    Flag,
    Named(String),
    Global,
    Env,
    LocalConfigDefault(String),
    /// The project directory that contained the `.brain/` we used.
    LocalDir(PathBuf),
    GlobalConfigDefault(String),
}

impl Origin {
    /// A human explanation, for `brain where` and for error context.
    pub fn explain(&self) -> String {
        match self {
            Self::Flag => "--brain flag".into(),
            Self::Named(n) => format!("--use {n} (from config)"),
            Self::Global => "--global flag".into(),
            Self::Env => "BRAIN_PATH environment variable".into(),
            Self::LocalConfigDefault(n) => {
                format!("default_brain = \"{n}\" in local .brain/config.toml")
            }
            Self::LocalDir(d) => format!("nearest .brain/ directory, found at {}", d.display()),
            Self::GlobalConfigDefault(n) => format!("default_brain = \"{n}\" in global config"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrainRef {
    pub path: PathBuf,
    pub origin: Origin,
}

#[derive(Debug, thiserror::Error)]
pub enum LocateError {
    #[error("--brain, --use and --global select different brains; pass only one (got: {given})")]
    ConflictingSelectors { given: String },

    #[error("no brain named {name:?}{}", fmt_known(known))]
    UnknownName { name: String, known: Vec<String> },

    #[error(
        "no brain found searching upward from {}.\n\
         Create one with `brain init`, or point at an existing one with \
         `--brain <path>`, `--use <name>` or `--global`.",
        searched_from.display()
    )]
    NotFound { searched_from: PathBuf },

    #[error(transparent)]
    BadConfig(#[from] crate::config::BadConfig),
}

fn fmt_known(known: &[String]) -> String {
    if known.is_empty() {
        ". No brains are configured.".into()
    } else {
        format!(". Known brains: {}", known.join(", "))
    }
}

/// Walks the resolution ladder and reports both the brain and the reason.
pub fn resolve(sel: &Selection, ctx: &Ctx) -> Result<BrainRef, LocateError> {
    check_conflicts(sel)?;

    // 1. An explicit path beats everything, including a broken config.
    if let Some(p) = &sel.brain {
        return Ok(BrainRef {
            path: absolutize(p, &ctx.cwd),
            origin: Origin::Flag,
        });
    }

    let global_cfg_path = ctx.global_config_path();
    let global_cfg = Config::load(&global_cfg_path)?;

    // 2. A named brain from the catalogue. Local entries shadow global ones.
    if let Some(name) = &sel.use_name {
        let local = nearest_local_config(ctx)?;
        let local_cfg = local.as_ref().map(|(_, c)| c);
        return lookup_name(
            name,
            local_cfg,
            &global_cfg,
            ctx,
            local.as_ref().map(|(d, _)| d.as_path()),
        )
        .map(|path| BrainRef {
            path,
            origin: Origin::Named(name.clone()),
        });
    }

    // 3. The global brain, honouring a data_dir override.
    if sel.global {
        let base = global_cfg
            .data_dir
            .as_ref()
            .map(|d| resolve_config_path(d, &global_cfg_path, ctx))
            .unwrap_or_else(|| ctx.data_dir.clone());
        return Ok(BrainRef {
            path: base.join("brain.db"),
            origin: Origin::Global,
        });
    }

    // 4. The environment.
    if let Some(p) = ctx.env.get("BRAIN_PATH").filter(|s| !s.is_empty()) {
        let p = expand_tilde(Path::new(p), ctx);
        return Ok(BrainRef {
            path: absolutize(&p, &ctx.cwd),
            origin: Origin::Env,
        });
    }

    // 5 & 6. The nearest `.brain/` directory: its configured default first, then
    // the brain.db sitting in it. If an ancestor's `.brain/` yields neither, keep
    // climbing rather than giving up.
    for dir in ctx.cwd.ancestors() {
        let brain_dir = dir.join(".brain");
        if !brain_dir.is_dir() {
            continue;
        }

        let local_cfg_path = brain_dir.join("config.toml");
        let local_cfg = Config::load(&local_cfg_path)?;
        if let Some(name) = &local_cfg.default_brain {
            let path = lookup_name(
                name,
                Some(&local_cfg),
                &global_cfg,
                ctx,
                Some(&local_cfg_path),
            )?;
            return Ok(BrainRef {
                path,
                origin: Origin::LocalConfigDefault(name.clone()),
            });
        }

        let db = brain_dir.join("brain.db");
        if db.is_file() {
            return Ok(BrainRef {
                path: db,
                origin: Origin::LocalDir(dir.to_path_buf()),
            });
        }
    }

    // 7. Only now, with no local brain in sight, does a global default apply.
    if let Some(name) = &global_cfg.default_brain {
        let path = lookup_name(name, None, &global_cfg, ctx, Some(&global_cfg_path))?;
        return Ok(BrainRef {
            path,
            origin: Origin::GlobalConfigDefault(name.clone()),
        });
    }

    // 8. Fail loudly. Never fall through to the global brain.
    Err(LocateError::NotFound {
        searched_from: ctx.cwd.clone(),
    })
}

fn check_conflicts(sel: &Selection) -> Result<(), LocateError> {
    let mut given = Vec::new();
    if sel.brain.is_some() {
        given.push("--brain");
    }
    if sel.use_name.is_some() {
        given.push("--use");
    }
    if sel.global {
        given.push("--global");
    }
    if given.len() > 1 {
        return Err(LocateError::ConflictingSelectors {
            given: given.join(", "),
        });
    }
    Ok(())
}

/// Resolves a catalogue name, preferring a local catalogue over the global one.
fn lookup_name(
    name: &str,
    local: Option<&Config>,
    global: &Config,
    ctx: &Ctx,
    local_cfg_path: Option<&Path>,
) -> Result<PathBuf, LocateError> {
    if let Some(cfg) = local
        && let Some(p) = cfg.brains.get(name)
    {
        let base = local_cfg_path.unwrap_or(&ctx.cwd);
        return Ok(resolve_config_path(p, base, ctx));
    }
    if let Some(p) = global.brains.get(name) {
        return Ok(resolve_config_path(p, &ctx.global_config_path(), ctx));
    }

    // Merge both catalogues for the error message so the user sees every option.
    let mut known = global.brain_names();
    if let Some(cfg) = local {
        known.extend(cfg.brain_names());
    }
    known.sort_unstable();
    known.dedup();
    Err(LocateError::UnknownName {
        name: name.to_string(),
        known,
    })
}

/// Paths in a config may be absolute, `~`-prefixed, or relative to that config.
fn resolve_config_path(p: &Path, config_file: &Path, ctx: &Ctx) -> PathBuf {
    let expanded = expand_tilde(p, ctx);
    if expanded.is_absolute() {
        return expanded;
    }
    let base = config_file.parent().unwrap_or(Path::new("."));
    base.join(expanded)
}

fn expand_tilde(p: &Path, ctx: &Ctx) -> PathBuf {
    let Ok(rest) = p.strip_prefix("~") else {
        return p.to_path_buf();
    };
    match ctx.env.get("HOME").filter(|h| !h.is_empty()) {
        Some(home) => Path::new(home).join(rest),
        None => p.to_path_buf(),
    }
}

/// A relative path is relative to the caller's cwd, not the process's.
fn absolutize(p: &Path, cwd: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Finds the nearest `.brain/config.toml`, returning it with its own path.
fn nearest_local_config(ctx: &Ctx) -> Result<Option<(PathBuf, Config)>, LocateError> {
    for dir in ctx.cwd.ancestors() {
        let p = dir.join(".brain").join("config.toml");
        if p.is_file() {
            let cfg = Config::load(&p)?;
            return Ok(Some((p, cfg)));
        }
    }
    Ok(None)
}

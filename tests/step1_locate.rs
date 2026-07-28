//! Passo 1, part 2: which brain gets opened, and why.
//!
//! The hard requirement behind all of this: two brains on one machine must never
//! bleed into each other. That safety rests on resolution being explicit and on
//! there being NO silent fallback -- notably, never dropping into the global
//! brain just because the current directory has none.
//!
//! Every test here runs against injected paths and an injected environment, so
//! the suite never touches the real $HOME.

use brain::config::Config;
use brain::locate::{Ctx, LocateError, Origin, Selection, resolve};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// A fully isolated world: a project tree, a global config dir, a global data dir.
struct World {
    _tmp: TempDir,
    root: PathBuf,
    config_dir: PathBuf,
    data_dir: PathBuf,
    env: BTreeMap<String, String>,
}

impl World {
    fn new() -> Self {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().to_path_buf();
        let (config_dir, data_dir) = (root.join("xdg/config"), root.join("xdg/data"));
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        Self {
            _tmp: tmp,
            root,
            config_dir,
            data_dir,
            env: BTreeMap::new(),
        }
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn file(&self, rel: &str, body: &str) -> PathBuf {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body).unwrap();
        p
    }

    fn env(mut self, k: &str, v: &str) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }

    fn ctx(&self, cwd: &Path) -> Ctx {
        Ctx {
            cwd: cwd.to_path_buf(),
            env: self.env.clone(),
            config_dir: self.config_dir.clone(),
            data_dir: self.data_dir.clone(),
        }
    }
}

fn nothing() -> Selection {
    Selection::default()
}

// --- the happy path for each rung of the ladder ------------------------------

#[test]
fn explicit_brain_flag_wins_over_everything_else() {
    let w = World::new().env("BRAIN_PATH", "/env/brain.db");
    w.file("proj/.brain/brain.db", "");
    w.file(
        "xdg/config/config.toml",
        "default_brain = \"other\"\n[brains]\nother = \"/global/other.db\"\n",
    );
    let explicit = w.file("anywhere/mine.db", "");

    let sel = Selection {
        brain: Some(explicit.clone()),
        ..nothing()
    };
    let got = resolve(&sel, &w.ctx(&w.dir("proj/src"))).unwrap();

    assert_eq!(got.path, explicit);
    assert_eq!(got.origin, Origin::Flag);
}

#[test]
fn named_brain_resolves_through_the_config_catalogue() {
    let w = World::new();
    let target = w.file("empresas/acme/.brain/brain.db", "");
    w.file(
        "xdg/config/config.toml",
        &format!("[brains]\ntrabalho = \"{}\"\n", target.display()),
    );

    let sel = Selection {
        use_name: Some("trabalho".into()),
        ..nothing()
    };
    let got = resolve(&sel, &w.ctx(&w.dir("somewhere/else"))).unwrap();

    assert_eq!(got.path, target);
    assert_eq!(got.origin, Origin::Named("trabalho".into()));
}

#[test]
fn unknown_named_brain_is_an_error_listing_what_does_exist() {
    let w = World::new();
    w.file(
        "xdg/config/config.toml",
        "[brains]\ntrabalho = \"/a.db\"\npessoal = \"/b.db\"\n",
    );

    let sel = Selection {
        use_name: Some("naoexiste".into()),
        ..nothing()
    };
    let err = resolve(&sel, &w.ctx(&w.dir("proj"))).unwrap_err();

    match err {
        LocateError::UnknownName { name, known } => {
            assert_eq!(name, "naoexiste");
            // Sorted, so the error message is byte-identical run to run.
            assert_eq!(known, vec!["pessoal".to_string(), "trabalho".to_string()]);
        }
        other => panic!("expected UnknownName, got {other:?}"),
    }
}

#[test]
fn global_flag_uses_the_data_dir() {
    let w = World::new();
    let sel = Selection {
        global: true,
        ..nothing()
    };
    let got = resolve(&sel, &w.ctx(&w.dir("proj"))).unwrap();

    assert_eq!(got.path, w.data_dir.join("brain.db"));
    assert_eq!(got.origin, Origin::Global);
}

#[test]
fn global_flag_respects_a_data_dir_override_in_the_config() {
    let w = World::new();
    let custom = w.dir("cofre");
    w.file(
        "xdg/config/config.toml",
        &format!("data_dir = \"{}\"\n", custom.display()),
    );

    let sel = Selection {
        global: true,
        ..nothing()
    };
    let got = resolve(&sel, &w.ctx(&w.dir("proj"))).unwrap();

    assert_eq!(got.path, custom.join("brain.db"));
}

#[test]
fn env_var_is_used_when_no_flag_is_given() {
    let w = World::new();
    let target = w.file("from/env.db", "");
    let w = w.env("BRAIN_PATH", target.to_str().unwrap());

    let got = resolve(&nothing(), &w.ctx(&w.dir("proj"))).unwrap();

    assert_eq!(got.path, target);
    assert_eq!(got.origin, Origin::Env);
}

#[test]
fn local_config_default_brain_beats_the_local_brain_db() {
    let w = World::new();
    let named = w.file("elsewhere/named.db", "");
    w.file("proj/.brain/brain.db", "");
    w.file(
        "proj/.brain/config.toml",
        &format!(
            "default_brain = \"escolhido\"\n[brains]\nescolhido = \"{}\"\n",
            named.display()
        ),
    );

    let got = resolve(&nothing(), &w.ctx(&w.dir("proj/src/deep"))).unwrap();

    assert_eq!(got.path, named);
    assert_eq!(got.origin, Origin::LocalConfigDefault("escolhido".into()));
}

#[test]
fn local_brain_db_is_found_by_walking_up_from_a_nested_cwd() {
    let w = World::new();
    let target = w.file("proj/.brain/brain.db", "");
    let deep = w.dir("proj/a/b/c/d");

    let got = resolve(&nothing(), &w.ctx(&deep)).unwrap();

    assert_eq!(got.path, target);
    assert_eq!(got.origin, Origin::LocalDir(w.root.join("proj")));
}

#[test]
fn global_config_default_is_used_only_when_there_is_no_local_brain() {
    let w = World::new();
    let target = w.file("global/pessoal.db", "");
    w.file(
        "xdg/config/config.toml",
        &format!(
            "default_brain = \"pessoal\"\n[brains]\npessoal = \"{}\"\n",
            target.display()
        ),
    );

    let got = resolve(&nothing(), &w.ctx(&w.dir("no-brain-here"))).unwrap();

    assert_eq!(got.path, target);
    assert_eq!(got.origin, Origin::GlobalConfigDefault("pessoal".into()));
}

// --- the two cases the plan flagged as most confusing -------------------------

#[test]
fn local_brain_wins_over_a_global_default_brain() {
    // The confusing case: you are inside a project that has its own brain, but you
    // also have a global default configured. The local one must win, or work data
    // silently lands in your personal brain.
    let w = World::new();
    let local = w.file("proj/.brain/brain.db", "");
    let global = w.file("global/pessoal.db", "");
    w.file(
        "xdg/config/config.toml",
        &format!(
            "default_brain = \"pessoal\"\n[brains]\npessoal = \"{}\"\n",
            global.display()
        ),
    );

    let got = resolve(&nothing(), &w.ctx(&w.dir("proj/src"))).unwrap();

    assert_eq!(
        got.path, local,
        "the global default leaked into a project directory"
    );
}

#[test]
fn no_brain_anywhere_is_an_error_and_never_falls_back_to_global() {
    // The other confusing case: nothing configured. This must fail loudly. Quietly
    // opening the global brain is exactly how one company's data ends up in another.
    let w = World::new();
    w.file("xdg/data/brain.db", ""); // a global brain EXISTS...
    // ...but nothing points at it.

    let err = resolve(&nothing(), &w.ctx(&w.dir("orphan/dir"))).unwrap_err();

    assert!(
        matches!(err, LocateError::NotFound { .. }),
        "expected NotFound, got {err:?}"
    );
    assert!(
        err.to_string().contains("brain init"),
        "the error must tell the user how to fix it, got: {err}"
    );
}

// --- full precedence ladder ---------------------------------------------------

#[test]
fn every_rung_of_the_ladder_beats_the_one_below_it() {
    let w = World::new();
    let flag = w.file("w/flag.db", "");
    let named = w.file("w/named.db", "");
    let env = w.file("w/env.db", "");
    w.file("proj/.brain/brain.db", "");
    w.file(
        "xdg/config/config.toml",
        &format!("[brains]\nn = \"{}\"\n", named.display()),
    );
    let w = w.env("BRAIN_PATH", env.to_str().unwrap());
    let cwd = w.dir("proj/src");

    // Each selector, applied against an otherwise identical world, must win.
    let ladder: Vec<(Selection, PathBuf)> = vec![
        (
            Selection {
                brain: Some(flag.clone()),
                ..nothing()
            },
            flag,
        ),
        (
            Selection {
                use_name: Some("n".into()),
                ..nothing()
            },
            named,
        ),
        (
            Selection {
                global: true,
                ..nothing()
            },
            w.data_dir.join("brain.db"),
        ),
        (nothing(), env), // env still beats the local .brain/ that exists
    ];
    for (sel, want) in ladder {
        assert_eq!(
            resolve(&sel, &w.ctx(&cwd)).unwrap().path,
            want,
            "for {sel:?}"
        );
    }
}

#[test]
fn lower_rungs_take_over_as_higher_ones_are_removed() {
    let w = World::new();
    let localcfg = w.file("w/localcfg.db", "");
    let globalcfg = w.file("w/globalcfg.db", "");
    let localdir = w.file("proj/.brain/brain.db", "");
    w.file(
        "xdg/config/config.toml",
        &format!(
            "default_brain = \"g\"\n[brains]\ng = \"{}\"\n",
            globalcfg.display()
        ),
    );
    let local_cfg_path = w.file(
        "proj/.brain/config.toml",
        &format!(
            "default_brain = \"l\"\n[brains]\nl = \"{}\"\n",
            localcfg.display()
        ),
    );
    let cwd = w.dir("proj/src");

    // No env, no flags: local config default.
    assert_eq!(resolve(&nothing(), &w.ctx(&cwd)).unwrap().path, localcfg);

    // Drop the local config: the local .brain/brain.db.
    std::fs::remove_file(&local_cfg_path).unwrap();
    assert_eq!(resolve(&nothing(), &w.ctx(&cwd)).unwrap().path, localdir);

    // Drop the local brain too: only now does the global default apply.
    std::fs::remove_file(&localdir).unwrap();
    assert_eq!(resolve(&nothing(), &w.ctx(&cwd)).unwrap().path, globalcfg);
}

// --- flag conflicts and path handling ----------------------------------------

#[test]
fn conflicting_selectors_are_rejected_rather_than_silently_ordered() {
    let w = World::new();
    let cases = vec![
        Selection {
            brain: Some("/a.db".into()),
            use_name: Some("x".into()),
            ..nothing()
        },
        Selection {
            brain: Some("/a.db".into()),
            global: true,
            ..nothing()
        },
        Selection {
            use_name: Some("x".into()),
            global: true,
            ..nothing()
        },
    ];
    for sel in cases {
        let err = resolve(&sel, &w.ctx(&w.dir("proj"))).unwrap_err();
        assert!(
            matches!(err, LocateError::ConflictingSelectors { .. }),
            "expected ConflictingSelectors for {sel:?}, got {err:?}"
        );
    }
}

#[test]
fn tilde_in_config_paths_expands_to_the_home_directory() {
    let w = World::new();
    let home = w.dir("home/victor");
    let target = w.file("home/victor/brains/x.db", "");
    let w = w.env("HOME", home.to_str().unwrap());
    w.file(
        "xdg/config/config.toml",
        "[brains]\nx = \"~/brains/x.db\"\n",
    );

    let sel = Selection {
        use_name: Some("x".into()),
        ..nothing()
    };
    let got = resolve(&sel, &w.ctx(&w.dir("proj"))).unwrap();

    assert_eq!(got.path, target);
}

#[test]
fn a_relative_brain_flag_resolves_against_the_cwd_not_the_process_dir() {
    let w = World::new();
    let cwd = w.dir("proj/src");
    w.file("proj/src/local.db", "");

    let sel = Selection {
        brain: Some(PathBuf::from("local.db")),
        ..nothing()
    };
    let got = resolve(&sel, &w.ctx(&cwd)).unwrap();

    assert_eq!(got.path, cwd.join("local.db"));
}

// --- config parsing -----------------------------------------------------------

#[test]
fn config_brains_iterate_in_sorted_order_for_stable_output() {
    let cfg: Config =
        toml::from_str("[brains]\nzeta = \"/z.db\"\nalpha = \"/a.db\"\nmid = \"/m.db\"\n").unwrap();
    let names: Vec<_> = cfg.brains.keys().cloned().collect();
    assert_eq!(names, vec!["alpha", "mid", "zeta"]);
}

#[test]
fn a_missing_config_file_is_an_empty_config_not_an_error() {
    let w = World::new();
    // No config.toml written at all.
    let got = resolve(
        &Selection {
            global: true,
            ..nothing()
        },
        &w.ctx(&w.dir("proj")),
    );
    assert!(
        got.is_ok(),
        "a missing config should behave as an empty one"
    );
}

#[test]
fn a_malformed_config_file_is_a_loud_error() {
    let w = World::new();
    w.file("xdg/config/config.toml", "this is not = = valid toml [[[");

    let err = resolve(&nothing(), &w.ctx(&w.dir("proj"))).unwrap_err();
    assert!(
        matches!(err, LocateError::BadConfig { .. }),
        "a broken config must not be silently ignored, got {err:?}"
    );
}

//! Opening a brain, and keeping it sealed.
//!
//! A brain is exactly one SQLite file. Two properties make the isolation
//! promise real, and both are enforced here rather than by convention:
//!
//! - **`ATTACH` is impossible.** `SQLITE_LIMIT_ATTACHED` is set to zero on every
//!   connection, so no query can reach a second database file. Without this, one
//!   crafted statement could join another company's facts into a result set.
//! - **A file is only a brain if it says so.** `open` requires a `brain_id` in
//!   the `meta` table, so a stray `.db` is never silently adopted.

use crate::clock::Clock;
use crate::ids::IdGen;
use jiff::Timestamp;
use rusqlite::Connection;
use rusqlite::limits::Limit;
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Bumped whenever the on-disk layout changes in a way older binaries cannot read.
/// v2 added the bitemporal fact model, v3 the FTS5 index, v4 the vector index.
pub const SCHEMA_VERSION: i64 = 4;

const SCHEMA: &str = include_str!("schema.sql");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(
        "no brain at {}.\nCreate one with `brain init`.",
        path.display()
    )]
    Missing { path: PathBuf },

    #[error(
        "{} is not a brain (no brain_id marker).\n\
         Refusing to touch it. If you meant to create a brain, pick a new path \
         and run `brain init`.",
        path.display()
    )]
    NotABrain { path: PathBuf },

    #[error("a brain already exists at {}", path.display())]
    AlreadyExists { path: PathBuf },

    #[error(
        "the brain at {} uses schema version {found}, but this build only \
         understands up to {supported}. Upgrade `brain`.",
        path.display()
    )]
    SchemaTooNew {
        path: PathBuf,
        found: i64,
        supported: i64,
    },

    #[error("corrupt brain metadata at {}: {detail}", path.display())]
    BadMeta { path: PathBuf, detail: String },

    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: std::io::Error,
    },
}

type Result<T> = std::result::Result<T, StoreError>;

/// An open brain.
pub struct Store {
    conn: Connection,
    path: PathBuf,
    id: Uuid,
    label: String,
    created_at: Timestamp,
}

/// Written by hand rather than derived: a `Store` must be safe to put in a log
/// line or an error, so it shows identity only and never any brain content.
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Creates a new brain. Refuses to touch an existing file.
    pub fn init(path: &Path, label: &str, clock: &dyn Clock, ids: &dyn IdGen) -> Result<Self> {
        if path.exists() {
            return Err(StoreError::AlreadyExists {
                path: path.to_path_buf(),
            });
        }
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                context: format!("creating {}", parent.display()),
                source,
            })?;
        }

        // Create the file ourselves so it is never briefly world-readable, and so
        // two concurrent `init`s cannot both believe they won.
        create_private_file(path)?;

        let id = ids.next_id();
        let created_at = clock.now();
        let conn = open_sealed(path)?;
        conn.execute_batch(SCHEMA)?;

        {
            let mut set = conn.prepare("INSERT INTO meta(key, value) VALUES (?, ?)")?;
            set.execute(("brain_id", id.to_string()))?;
            set.execute(("brain_label", label))?;
            set.execute(("schema_version", SCHEMA_VERSION.to_string()))?;
            set.execute(("created_at", created_at.to_string()))?;
        }

        Ok(Self {
            conn,
            path: path.to_path_buf(),
            id,
            label: label.to_string(),
            created_at,
        })
    }

    /// Opens an existing brain. Never creates one.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Err(StoreError::Missing {
                path: path.to_path_buf(),
            });
        }
        let conn = open_sealed(path)?;

        let id_text = read_meta(&conn, "brain_id")?.ok_or_else(|| StoreError::NotABrain {
            path: path.to_path_buf(),
        })?;
        let id = Uuid::parse_str(&id_text).map_err(|e| StoreError::BadMeta {
            path: path.to_path_buf(),
            detail: format!("brain_id {id_text:?} is not a UUID: {e}"),
        })?;

        let found: i64 = read_meta(&conn, "schema_version")?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| StoreError::BadMeta {
                path: path.to_path_buf(),
                detail: "schema_version missing or not a number".into(),
            })?;
        if found > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                path: path.to_path_buf(),
                found,
                supported: SCHEMA_VERSION,
            });
        }

        let label = read_meta(&conn, "brain_label")?.unwrap_or_default();
        let created_at = read_meta(&conn, "created_at")?
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| StoreError::BadMeta {
                path: path.to_path_buf(),
                detail: "created_at missing or unparseable".into(),
            })?;

        Ok(Self {
            conn,
            path: path.to_path_buf(),
            id,
            label,
            created_at,
        })
    }

    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// The identity stamped onto every JSON answer, so an agent holding two
    /// brains can never mistake one for the other.
    pub fn identity(&self) -> serde_json::Value {
        serde_json::json!({
            "brain_id": self.id.to_string(),
            "brain_label": self.label,
            "brain_path": self.path,
        })
    }
}

/// Opens a connection with the isolation guarantees applied.
fn open_sealed(path: &Path) -> Result<Connection> {
    register_extensions();
    let conn = Connection::open(path)?;

    // The seal: no second database file can ever be reachable from this
    // connection, by any path, including `:memory:` and temp databases.
    conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)?;

    // SQL-callable `load_extension()` stays off: we deliberately do not enable
    // rusqlite's `load_extension` feature, and SQLite's default is disabled. A
    // loadable extension could otherwise re-open the seal above. Covered by
    // `extension_loading_is_disabled` in tests/step2_isolation.rs.

    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA synchronous = NORMAL;",
    )?;
    Ok(conn)
}

/// `sqlite-vec` installs itself as an auto-extension, which must happen once per
/// process and strictly before any connection is opened.
fn register_extensions() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(
            sqlite_vec::sqlite3_vec_init as *const ()
        )));
    });
}

fn read_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = match conn.prepare("SELECT value FROM meta WHERE key = ?") {
        Ok(s) => s,
        // No `meta` table at all: a SQLite file, but not one of ours.
        Err(rusqlite::Error::SqliteFailure(_, _)) => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    match stmt.query_row([key], |r| r.get::<_, String>(0)) {
        Ok(v) => Ok(Some(v)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[cfg(unix)]
fn create_private_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map(|_| ())
        .map_err(|source| StoreError::Io {
            context: format!("creating {}", path.display()),
            source,
        })
}

#[cfg(not(unix))]
fn create_private_file(path: &Path) -> Result<()> {
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map(|_| ())
        .map_err(|source| StoreError::Io {
            context: format!("creating {}", path.display()),
            source,
        })
}

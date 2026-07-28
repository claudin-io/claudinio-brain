//! Passo 2: a brain is a sealed box.
//!
//! The promise is that two companies can keep brains on one machine and neither
//! can read the other. That rests on three things, each tested here:
//!
//! - a brain is exactly one file, and nothing can reach outside it (`ATTACH`),
//! - a file is only a brain if it says so, so a stray `.db` is never adopted,
//! - identity travels with every answer, so an agent can tell them apart.

use brain::clock::FixedClock;
use brain::ids::{SeededIdGen, UuidV7Gen};
use brain::store::{Store, StoreError};
use jiff::Timestamp;
use tempfile::TempDir;

fn at(s: &str) -> FixedClock {
    FixedClock::new(s.parse::<Timestamp>().unwrap())
}

fn tmp() -> TempDir {
    TempDir::new().unwrap()
}

fn init(dir: &TempDir, name: &str, label: &str) -> Store {
    Store::init(
        &dir.path().join(name),
        label,
        &at("2026-07-28T10:00:00Z"),
        &SeededIdGen::new(1),
    )
    .unwrap()
}

// --- a brain must declare itself ---------------------------------------------

#[test]
fn init_creates_a_brain_that_knows_its_own_identity() {
    let d = tmp();
    let s = init(&d, "acme.db", "empresa-acme");

    assert_eq!(s.label(), "empresa-acme");
    assert_eq!(
        s.created_at(),
        "2026-07-28T10:00:00Z".parse::<Timestamp>().unwrap()
    );
    assert!(d.path().join("acme.db").is_file());

    // The identity survives a round trip through the file.
    let reopened = Store::open(&d.path().join("acme.db")).unwrap();
    assert_eq!(reopened.id(), s.id());
    assert_eq!(reopened.label(), s.label());
}

#[test]
fn two_brains_created_in_production_get_different_identities() {
    // With the real generator, which is what ships.
    let d = tmp();
    let c = at("2026-07-28T10:00:00Z");
    let g = UuidV7Gen;
    let a = Store::init(&d.path().join("a.db"), "empresa-a", &c, &g).unwrap();
    let b = Store::init(&d.path().join("b.db"), "empresa-b", &c, &g).unwrap();

    assert_ne!(a.id(), b.id(), "two brains shared an identity");
}

#[test]
fn copying_a_brain_file_duplicates_its_id_so_path_is_what_disambiguates() {
    // Worth pinning down rather than pretending otherwise: `brain_id` identifies a
    // lineage, not a file. `cp a.db b.db` yields two files with one id, and no
    // id-generation scheme can prevent that. This is exactly why `identity()`
    // emits `brain_path` alongside `brain_id`.
    let d = tmp();
    let a = init(&d, "a.db", "empresa-a");
    let copy_path = d.path().join("copy.db");
    std::fs::copy(d.path().join("a.db"), &copy_path).unwrap();

    let copy = Store::open(&copy_path).unwrap();
    assert_eq!(copy.id(), a.id(), "a copy shares the lineage id");
    assert_ne!(
        copy.identity()["brain_path"],
        a.identity()["brain_path"],
        "brain_path must distinguish two copies"
    );
}

#[test]
fn a_plain_sqlite_file_is_not_adopted_as_a_brain() {
    let d = tmp();
    let path = d.path().join("notabrain.db");
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute_batch("CREATE TABLE whatever(x); INSERT INTO whatever VALUES (1);")
        .unwrap();
    drop(conn);

    let err = Store::open(&path).unwrap_err();
    assert!(
        matches!(err, StoreError::NotABrain { .. }),
        "expected NotABrain, got {err:?}"
    );
}

#[test]
fn a_file_of_garbage_is_not_adopted_as_a_brain() {
    let d = tmp();
    let path = d.path().join("garbage.db");
    std::fs::write(&path, b"definitely not sqlite").unwrap();

    assert!(Store::open(&path).is_err());
}

#[test]
fn opening_a_missing_brain_says_how_to_create_one_and_creates_nothing() {
    let d = tmp();
    let path = d.path().join("nope.db");

    let err = Store::open(&path).unwrap_err();
    assert!(matches!(err, StoreError::Missing { .. }), "got {err:?}");
    assert!(err.to_string().contains("brain init"), "got: {err}");
    assert!(!path.exists(), "open must never create a brain");
}

#[test]
fn init_refuses_to_clobber_an_existing_brain() {
    let d = tmp();
    let path = d.path().join("acme.db");
    init(&d, "acme.db", "empresa-acme");

    let err = Store::init(
        &path,
        "outra",
        &at("2026-07-28T11:00:00Z"),
        &SeededIdGen::new(2),
    )
    .unwrap_err();
    assert!(
        matches!(err, StoreError::AlreadyExists { .. }),
        "got {err:?}"
    );

    // And the original is untouched.
    assert_eq!(Store::open(&path).unwrap().label(), "empresa-acme");
}

#[test]
fn init_creates_missing_parent_directories() {
    let d = tmp();
    let path = d.path().join("deep/nested/.brain/brain.db");
    Store::init(
        &path,
        "x",
        &at("2026-07-28T10:00:00Z"),
        &SeededIdGen::new(1),
    )
    .unwrap();
    assert!(path.is_file());
}

// --- the sealed box ----------------------------------------------------------

#[test]
fn attach_is_rejected_so_one_brain_can_never_read_another() {
    // This is the mechanism the whole isolation promise rests on. Without it, a
    // single crafted query could pull another company's facts into a join.
    let d = tmp();
    let a = init(&d, "a.db", "empresa-a");
    let b_path = d.path().join("b.db");
    init(&d, "b.db", "empresa-b");

    let err = a
        .conn()
        .execute_batch(&format!("ATTACH DATABASE '{}' AS other;", b_path.display()))
        .unwrap_err();

    assert!(
        err.to_string().to_lowercase().contains("attach"),
        "ATTACH should have been refused, got: {err}"
    );
}

#[test]
fn attach_of_an_in_memory_database_is_also_rejected() {
    // `:memory:` and a temp file are the obvious ways around a path-based block.
    let d = tmp();
    let a = init(&d, "a.db", "empresa-a");

    for target in [":memory:", ""] {
        assert!(
            a.conn()
                .execute_batch(&format!("ATTACH DATABASE '{target}' AS scratch;"))
                .is_err(),
            "ATTACH '{target}' should have been refused"
        );
    }
}

#[test]
fn writes_to_one_brain_are_invisible_to_the_other() {
    let d = tmp();
    let c = at("2026-07-28T10:00:00Z");
    let a = Store::init(
        &d.path().join("a.db"),
        "empresa-a",
        &c,
        &SeededIdGen::new(1),
    )
    .unwrap();
    let b = Store::init(
        &d.path().join("b.db"),
        "empresa-b",
        &c,
        &SeededIdGen::new(2),
    )
    .unwrap();

    a.conn()
        .execute_batch(
            "CREATE TABLE segredo(v TEXT); INSERT INTO segredo VALUES ('folha-de-pagamento');",
        )
        .unwrap();

    let leaked: Result<String, _> = b
        .conn()
        .query_row("SELECT v FROM segredo", [], |r| r.get(0));
    assert!(leaked.is_err(), "brain B could read brain A's table");

    // The one table that exists in both must still report distinct identities.
    let read_id = |s: &Store| -> String {
        s.conn()
            .query_row("SELECT value FROM meta WHERE key='brain_id'", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    assert_ne!(read_id(&a), read_id(&b));
}

#[test]
fn extension_loading_is_disabled() {
    // A loadable extension could re-enable ATTACH or read arbitrary files.
    let d = tmp();
    let a = init(&d, "a.db", "empresa-a");

    let blocked = a
        .conn()
        .query_row("SELECT load_extension('/tmp/whatever.dylib')", [], |r| {
            r.get::<_, String>(0)
        })
        .is_err();
    assert!(blocked, "load_extension() was callable from SQL");
}

#[cfg(unix)]
#[test]
fn the_brain_file_is_not_world_readable() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmp();
    init(&d, "acme.db", "empresa-acme");

    let mode = std::fs::metadata(d.path().join("acme.db"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "brain file mode was {mode:o}, expected 600");
}

// --- schema versioning --------------------------------------------------------

#[test]
fn a_brain_from_a_newer_schema_version_is_refused_rather_than_corrupted() {
    let d = tmp();
    let path = d.path().join("future.db");
    let s = init(&d, "future.db", "x");
    s.conn()
        .execute(
            "UPDATE meta SET value = '9999' WHERE key = 'schema_version'",
            [],
        )
        .unwrap();
    drop(s);

    let err = Store::open(&path).unwrap_err();
    assert!(
        matches!(err, StoreError::SchemaTooNew { .. }),
        "got {err:?}"
    );
}

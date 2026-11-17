use coshell_storage::{migrate, MigrationError, CURRENT_SCHEMA_VERSION};
use rusqlite::Connection;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

struct TestDatabase(std::path::PathBuf);

impl TestDatabase {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "coshell-migration-{}-{stamp}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> std::path::PathBuf {
        self.0.join("test.sqlite")
    }
}

impl Drop for TestDatabase {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn tables(db: &Connection) -> Vec<String> {
    let mut statement = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .unwrap();
    statement
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
}

#[test]
fn initializes_empty_database_with_all_v1_tables() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();

    for name in [
        "schema_migrations",
        "connection_groups",
        "connections",
        "connection_secret_refs",
        "trusted_host_keys",
        "command_history",
        "app_settings",
        "webdav_profiles",
        "backup_runs",
        "secret_write_journal",
    ] {
        assert!(tables(&db).iter().any(|table| table == name), "{name}");
    }
    let version: u32 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
    let recorded: u32 = db
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(recorded, CURRENT_SCHEMA_VERSION);
    let foreign_keys: u8 = db
        .pragma_query_value(None, "foreign_keys", |row| row.get(0))
        .unwrap();
    assert_eq!(foreign_keys, 1);
}

#[test]
fn repeated_migration_preserves_data_and_does_not_duplicate_version() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    db.execute(
        "INSERT INTO app_settings (key, value) VALUES ('theme', 'dark')",
        [],
    )
    .unwrap();
    migrate(&mut db).unwrap();
    let value: String = db
        .query_row(
            "SELECT value FROM app_settings WHERE key='theme'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let count: u32 = db
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(value, "dark");
    assert_eq!(count, CURRENT_SCHEMA_VERSION);
}

#[test]
fn rejects_newer_version_without_modifying_database() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch("CREATE TABLE sentinel(value TEXT); INSERT INTO sentinel VALUES ('keep'); PRAGMA user_version = 99;").unwrap();
    assert!(matches!(
        migrate(&mut db),
        Err(MigrationError::UnsupportedVersion { found: 99, .. })
    ));
    assert_eq!(tables(&db), vec!["sentinel"]);
    let value: String = db
        .query_row("SELECT value FROM sentinel", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, "keep");
    let version: u32 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 99);
}

#[test]
fn failure_rolls_back_all_ddl_and_version() {
    let mut db = Connection::open_in_memory().unwrap();
    db.execute_batch(
        "CREATE TABLE connections (sentinel TEXT); INSERT INTO connections VALUES ('keep');",
    )
    .unwrap();
    assert!(matches!(migrate(&mut db), Err(MigrationError::Database(_))));
    assert_eq!(tables(&db), vec!["connections"]);
    let sentinel: String = db
        .query_row("SELECT sentinel FROM connections", [], |row| row.get(0))
        .unwrap();
    assert_eq!(sentinel, "keep");
    let version: u32 = db
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
}

#[test]
fn schema_enforces_connection_constraints_and_secret_reference_cascade() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    assert!(db.execute(
        "INSERT INTO connections (id, name, host, port, username, authentication) VALUES ('bad', 'bad', 'host', 0, 'user', 'password')",
        [],
    ).is_err());
    db.execute(
        "INSERT INTO connections (id, name, host, username, authentication) VALUES ('c1', 'server', 'host', 'user', 'password')",
        [],
    ).unwrap();
    assert!(db.execute(
        "INSERT INTO connections (id, name, host, username, authentication) VALUES ('c2', 'server', 'host', 'user', 'password')",
        [],
    ).is_err());
    assert!(db.execute(
        "INSERT INTO connection_secret_refs (connection_id, secret_type, secret_id) VALUES ('missing', 'password', 's0')",
        [],
    ).is_err());
    db.execute(
        "INSERT INTO connection_secret_refs (connection_id, secret_type, secret_id) VALUES ('c1', 'password', 's1')",
        [],
    ).unwrap();
    db.execute("DELETE FROM connections WHERE id='c1'", [])
        .unwrap();
    let count: u32 = db
        .query_row("SELECT COUNT(*) FROM connection_secret_refs", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn independent_file_connections_each_enable_foreign_keys() {
    let temp = TestDatabase::new();
    let mut first = Connection::open(temp.path()).unwrap();
    let mut second = Connection::open(temp.path()).unwrap();
    migrate(&mut first).unwrap();
    migrate(&mut second).unwrap();

    for db in [&first, &second] {
        let enabled: u8 = db
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(enabled, 1);
        assert!(db.execute(
            "INSERT INTO connection_secret_refs (connection_id, secret_type, secret_id) VALUES ('missing', 'password', 's1')",
            [],
        ).is_err());
    }
}

#[test]
fn concurrent_file_initializers_create_one_schema_version() {
    let temp = TestDatabase::new();
    let path = temp.path();
    // Force both workers to contend for a real file-level write lock.
    let lock = Connection::open(&path).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let start = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let start = Arc::clone(&start);
            let path = path.clone();
            std::thread::spawn(move || {
                let mut db = Connection::open(path).unwrap();
                db.busy_timeout(Duration::from_secs(5)).unwrap();
                start.wait();
                migrate(&mut db).unwrap();
                let enabled: u8 = db
                    .pragma_query_value(None, "foreign_keys", |row| row.get(0))
                    .unwrap();
                assert_eq!(enabled, 1);
            })
        })
        .collect();
    start.wait();
    lock.execute_batch("COMMIT").unwrap();
    for handle in handles {
        handle.join().unwrap();
    }
    let count: u32 = lock
        .query_row(
            "SELECT COUNT(*) FROM schema_migrations WHERE version = 1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn locked_file_returns_error_without_partial_migration_and_can_retry() {
    let temp = TestDatabase::new();
    let lock = Connection::open(temp.path()).unwrap();
    let mut contender = Connection::open(temp.path()).unwrap();
    contender.busy_timeout(Duration::ZERO).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    assert!(matches!(
        migrate(&mut contender),
        Err(MigrationError::Database(_))
    ));
    assert!(tables(&contender).is_empty());
    let version: u32 = contender
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 0);
    lock.execute_batch("COMMIT").unwrap();
    migrate(&mut contender).unwrap();
    assert!(tables(&contender).iter().any(|name| name == "connections"));
}

#[test]
fn upgrades_existing_v1_database_without_losing_records() {
    let mut db = Connection::open_in_memory().unwrap();
    migrate(&mut db).unwrap();
    // Remove later tables and triggers attached to v1 tables before replaying
    // migrations; otherwise this would be a mixed-version schema, not v1.
    db.execute_batch("DROP TRIGGER restore_ref_pending_insert; DROP TRIGGER restore_ref_pending_update; DROP TRIGGER restore_webdav_pending_insert; DROP TRIGGER restore_webdav_pending_update; DROP TRIGGER restore_ssh_journal_cross; DROP TRIGGER restore_webdav_journal_cross; DROP TABLE restore_secret_journal; DROP TABLE restore_secret_receipts; DROP TRIGGER backup_run_prune_aborted_profile; DROP TRIGGER backup_runs_provisional_insert; DROP TRIGGER backup_runs_provisional_update; DROP VIEW active_webdav_profiles; DROP TRIGGER webdav_provisional_block_update; DROP TRIGGER webdav_provisional_block_delete; DROP TABLE webdav_provisional_profiles; DROP TRIGGER connection_ref_webdav_pending_insert; DROP TRIGGER connection_ref_webdav_pending_update; DROP TRIGGER webdav_ref_webdav_pending_insert; DROP TRIGGER webdav_ref_webdav_pending_update; DROP TRIGGER ssh_journal_cross_operation; DROP TRIGGER webdav_journal_cross_operation; DROP TABLE webdav_secret_journal; DROP TABLE webdav_secret_receipts; DROP TRIGGER secret_ref_reject_pending_insert; DROP TRIGGER secret_ref_reject_pending_update; DROP TRIGGER webdav_secret_reject_pending_insert; DROP TRIGGER webdav_secret_reject_pending_update; DROP TABLE secret_write_journal; DROP TABLE secret_operation_receipts; DELETE FROM schema_migrations WHERE version>=2; PRAGMA user_version=1; INSERT INTO app_settings(key,value) VALUES ('preserve','yes');").unwrap();
    migrate(&mut db).unwrap();
    let value: String = db
        .query_row(
            "SELECT value FROM app_settings WHERE key='preserve'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(value, "yes");
    assert!(tables(&db)
        .iter()
        .any(|name| name == "secret_write_journal"));
    let version: u32 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, CURRENT_SCHEMA_VERSION);
}

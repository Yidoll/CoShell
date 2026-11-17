//! Local persistence and migrations. Application orchestration belongs in `coshell-core`.

use rusqlite::{Connection, TransactionBehavior};
use std::{error::Error, fmt};

mod repository;
pub use repository::*;
pub mod host_key;
pub mod secret_receipt;
pub mod webdav_secret_receipt;

/// The newest on-disk schema understood by this version of CoShell.
pub const CURRENT_SCHEMA_VERSION: u32 = 11;

#[derive(Debug)]
pub enum MigrationError {
    UnsupportedVersion { found: u32, supported: u32 },
    Database(rusqlite::Error),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found, supported } => write!(
                formatter,
                "database schema version {found} is newer than supported version {supported}"
            ),
            Self::Database(error) => write!(formatter, "database migration failed: {error}"),
        }
    }
}

impl Error for MigrationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::UnsupportedVersion { .. } => None,
        }
    }
}

impl From<rusqlite::Error> for MigrationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Database(error)
    }
}

/// Apply pending schema migrations atomically. Newer schemas are never modified.
/// The caller must invoke this once per connection so SQLite foreign keys are enabled.
pub fn migrate(connection: &mut Connection) -> Result<(), MigrationError> {
    // Check before changing even connection-level settings on an unknown schema.
    let version: u32 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(MigrationError::UnsupportedVersion {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    connection.pragma_update(None, "foreign_keys", true)?;
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Re-check after obtaining the write lock, so concurrent initializers do not
    // attempt the same migration based on an out-of-date version.
    let version: u32 = transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(MigrationError::UnsupportedVersion {
            found: version,
            supported: CURRENT_SCHEMA_VERSION,
        });
    }
    if version == 0 {
        transaction.execute_batch(V1_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 1)?;
    }
    if version < 2 {
        repository::normalize_existing_history(&transaction)?;
        transaction.execute_batch(V2_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 2)?;
    }
    if version < 3 {
        transaction.execute_batch(V3_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 3)?;
    }
    if version < 4 {
        transaction.execute_batch(V4_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 4)?;
    }
    if version < 5 {
        transaction.execute_batch(V5_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 5)?;
    }
    if version < 6 {
        transaction.execute_batch(V6_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 6)?;
    }
    if version < 7 {
        transaction.execute_batch(V7_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 7)?;
    }
    if version < 8 {
        transaction.execute_batch(V8_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 8)?;
    }
    if version < 9 {
        transaction.execute_batch(V9_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 9)?;
    }
    if version < 10 {
        transaction.execute_batch(V10_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 10)?;
    }
    if version < 11 {
        transaction.execute_batch(V11_SCHEMA)?;
        transaction.pragma_update(None, "user_version", 11)?;
    }
    transaction.commit()?;
    Ok(())
}

const V11_SCHEMA: &str = r#"
-- Recheck databases that already ran an older v10 migration without validation.
CREATE TEMP TABLE restore_cleanup_v11_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO restore_cleanup_v11_guard(ok)
 SELECT CASE WHEN EXISTS(
  SELECT 1 FROM restore_secret_journal j WHERE j.kind='cleanup' AND (
   NOT EXISTS(SELECT 1 FROM secret_operation_receipts r WHERE r.secret_id=j.secret_id AND r.connection_id=j.connection_id AND r.secret_type=j.secret_type AND r.state='committed')
   OR EXISTS(SELECT 1 FROM connection_secret_refs r WHERE r.secret_id=j.secret_id AND (r.connection_id<>j.connection_id OR r.secret_type<>j.secret_type))
   OR EXISTS(SELECT 1 FROM webdav_profiles w WHERE w.credential_secret_id=j.secret_id)
  )
 ) THEN 0 ELSE 1 END;
DROP TABLE restore_cleanup_v11_guard;
INSERT INTO schema_migrations(version) VALUES(11);
"#;

const V10_SCHEMA: &str = r#"
-- Older cleanup entries must have durable evidence of their exact SSH owner.
-- Never guess ownership or delete an unverifiable secret during migration.
CREATE TEMP TABLE restore_cleanup_migration_guard(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO restore_cleanup_migration_guard(ok)
 SELECT CASE WHEN EXISTS(
  SELECT 1 FROM restore_secret_journal j WHERE j.kind='cleanup' AND (
   NOT EXISTS(SELECT 1 FROM secret_operation_receipts r WHERE r.secret_id=j.secret_id AND r.connection_id=j.connection_id AND r.secret_type=j.secret_type AND r.state='committed')
   OR EXISTS(SELECT 1 FROM connection_secret_refs r WHERE r.secret_id=j.secret_id AND (r.connection_id<>j.connection_id OR r.secret_type<>j.secret_type))
   OR EXISTS(SELECT 1 FROM webdav_profiles w WHERE w.credential_secret_id=j.secret_id)
  )
 ) THEN 0 ELSE 1 END;
DROP TABLE restore_cleanup_migration_guard;
CREATE TRIGGER restore_cleanup_owned BEFORE INSERT ON restore_secret_journal
WHEN NEW.kind='cleanup' AND (
 NOT EXISTS(SELECT 1 FROM connection_secret_refs WHERE connection_id=NEW.connection_id AND secret_type=NEW.secret_type AND secret_id=NEW.secret_id)
 OR EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=NEW.secret_id AND (connection_id<>NEW.connection_id OR secret_type<>NEW.secret_type))
 OR EXISTS(SELECT 1 FROM webdav_profiles WHERE credential_secret_id=NEW.secret_id)
 OR EXISTS(SELECT 1 FROM secret_operation_receipts WHERE secret_id=NEW.secret_id AND (connection_id<>NEW.connection_id OR secret_type<>NEW.secret_type OR state<>'committed'))
)
BEGIN SELECT RAISE(ABORT,'restore cleanup ownership'); END;
INSERT INTO schema_migrations(version) VALUES(10);
"#;

const V9_SCHEMA: &str = r#"
DROP TRIGGER restore_journal_cross;
CREATE TRIGGER restore_journal_cross BEFORE INSERT ON restore_secret_journal
WHEN EXISTS(SELECT 1 FROM secret_write_journal WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id
 UNION ALL SELECT 1 FROM secret_operation_receipts WHERE operation_id=NEW.batch_id OR (NEW.kind='staged' AND secret_id=NEW.secret_id)
 UNION ALL SELECT 1 FROM webdav_secret_journal WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id
 UNION ALL SELECT 1 FROM webdav_secret_receipts WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id)
BEGIN SELECT RAISE(ABORT,'restore operation conflict'); END;
INSERT INTO schema_migrations(version) VALUES(9);
"#;

const V8_SCHEMA: &str = r#"
CREATE TABLE restore_secret_journal (
 batch_id TEXT NOT NULL, connection_id TEXT NOT NULL, secret_type TEXT NOT NULL,
 secret_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL CHECK(kind IN ('staged','cleanup')),
 PRIMARY KEY(batch_id,connection_id,secret_type,kind)
);
CREATE TABLE restore_secret_receipts (batch_id TEXT PRIMARY KEY NOT NULL, state TEXT NOT NULL CHECK(state IN ('staged','committed','aborted')));
CREATE TRIGGER restore_ref_pending_insert BEFORE INSERT ON connection_secret_refs WHEN EXISTS(SELECT 1 FROM restore_secret_journal WHERE secret_id=NEW.secret_id) BEGIN SELECT RAISE(ABORT,'restore secret pending'); END;
CREATE TRIGGER restore_ref_pending_update BEFORE UPDATE OF secret_id ON connection_secret_refs WHEN EXISTS(SELECT 1 FROM restore_secret_journal WHERE secret_id=NEW.secret_id) BEGIN SELECT RAISE(ABORT,'restore secret pending'); END;
CREATE TRIGGER restore_webdav_pending_insert BEFORE INSERT ON webdav_profiles WHEN EXISTS(SELECT 1 FROM restore_secret_journal WHERE secret_id=NEW.credential_secret_id) BEGIN SELECT RAISE(ABORT,'restore secret pending'); END;
CREATE TRIGGER restore_webdav_pending_update BEFORE UPDATE OF credential_secret_id ON webdav_profiles WHEN EXISTS(SELECT 1 FROM restore_secret_journal WHERE secret_id=NEW.credential_secret_id) BEGIN SELECT RAISE(ABORT,'restore secret pending'); END;
CREATE TRIGGER restore_ssh_journal_cross BEFORE INSERT ON secret_write_journal WHEN EXISTS(SELECT 1 FROM restore_secret_journal WHERE batch_id=NEW.operation_id OR secret_id=NEW.secret_id UNION ALL SELECT 1 FROM restore_secret_receipts WHERE batch_id=NEW.operation_id) BEGIN SELECT RAISE(ABORT,'restore operation conflict'); END;
CREATE TRIGGER restore_webdav_journal_cross BEFORE INSERT ON webdav_secret_journal WHEN EXISTS(SELECT 1 FROM restore_secret_journal WHERE batch_id=NEW.operation_id OR secret_id=NEW.secret_id UNION ALL SELECT 1 FROM restore_secret_receipts WHERE batch_id=NEW.operation_id) BEGIN SELECT RAISE(ABORT,'restore operation conflict'); END;
CREATE TRIGGER restore_journal_cross BEFORE INSERT ON restore_secret_journal WHEN EXISTS(SELECT 1 FROM secret_write_journal WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id UNION ALL SELECT 1 FROM secret_operation_receipts WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id UNION ALL SELECT 1 FROM webdav_secret_journal WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id UNION ALL SELECT 1 FROM webdav_secret_receipts WHERE operation_id=NEW.batch_id OR secret_id=NEW.secret_id) BEGIN SELECT RAISE(ABORT,'restore operation conflict'); END;
INSERT INTO schema_migrations(version) VALUES(8);
"#;

const V1_SCHEMA: &str = r#"
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);
CREATE TABLE connection_groups (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL,
    parent_id TEXT REFERENCES connection_groups(id) ON DELETE RESTRICT,
    UNIQUE(parent_id, name)
);
CREATE UNIQUE INDEX connection_groups_root_name ON connection_groups(name) WHERE parent_id IS NULL;
CREATE TABLE connections (
    id TEXT PRIMARY KEY NOT NULL,
    name TEXT NOT NULL UNIQUE,
    host TEXT NOT NULL,
    port INTEGER NOT NULL DEFAULT 22 CHECK (port BETWEEN 1 AND 65535),
    username TEXT NOT NULL,
    authentication TEXT NOT NULL CHECK (authentication IN ('password', 'private_key', 'ssh_agent')),
    group_id TEXT REFERENCES connection_groups(id) ON DELETE SET NULL,
    notes TEXT NOT NULL DEFAULT '',
    production INTEGER NOT NULL DEFAULT 0 CHECK (production IN (0, 1)),
    always_confirm INTEGER NOT NULL DEFAULT 0 CHECK (always_confirm IN (0, 1)),
    version INTEGER NOT NULL DEFAULT 0 CHECK (version >= 0)
);
CREATE TABLE connection_secret_refs (
    connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE CASCADE,
    secret_type TEXT NOT NULL,
    secret_id TEXT NOT NULL UNIQUE,
    PRIMARY KEY(connection_id, secret_type)
);
CREATE TABLE trusted_host_keys (
    host TEXT NOT NULL,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    algorithm TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    PRIMARY KEY(host, port, algorithm)
);
CREATE TABLE command_history (
    id TEXT PRIMARY KEY NOT NULL,
    connection_id TEXT NOT NULL,
    connection_name TEXT NOT NULL,
    production INTEGER NOT NULL CHECK (production IN (0, 1)),
    command TEXT NOT NULL,
    submitted_at TEXT NOT NULL,
    outcome TEXT NOT NULL CHECK (outcome IN ('running', 'succeeded', 'failed', 'unknown')),
    exit_code INTEGER,
    duration_ms INTEGER CHECK (duration_ms IS NULL OR duration_ms >= 0)
);
CREATE INDEX command_history_connection_time ON command_history(connection_id, submitted_at DESC);
CREATE INDEX command_history_time ON command_history(submitted_at DESC);
CREATE TABLE app_settings (
    key TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);
CREATE TABLE webdav_profiles (
    id TEXT PRIMARY KEY NOT NULL,
    url TEXT NOT NULL,
    remote_directory TEXT NOT NULL,
    username TEXT NOT NULL,
    credential_secret_id TEXT,
    auto_backup INTEGER NOT NULL DEFAULT 0 CHECK (auto_backup IN (0, 1))
);
CREATE TABLE backup_runs (
    id TEXT PRIMARY KEY NOT NULL,
    profile_id TEXT NOT NULL REFERENCES webdav_profiles(id) ON DELETE CASCADE,
    snapshot_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled', 'unknown')),
    started_at TEXT NOT NULL,
    finished_at TEXT,
    error_code TEXT,
    UNIQUE(profile_id, snapshot_id)
);
INSERT INTO schema_migrations (version) VALUES (1);
"#;

const V2_SCHEMA: &str = r#"
CREATE TABLE secret_write_journal (
    operation_id TEXT PRIMARY KEY NOT NULL,
    connection_id TEXT NOT NULL REFERENCES connections(id) ON DELETE RESTRICT,
    secret_type TEXT NOT NULL,
    secret_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('staged','cleanup')),
    UNIQUE(connection_id, secret_type)
);
CREATE TRIGGER secret_ref_reject_pending_insert BEFORE INSERT ON connection_secret_refs
WHEN EXISTS(SELECT 1 FROM secret_write_journal WHERE secret_id=NEW.secret_id)
BEGIN SELECT RAISE(ABORT, 'secret pending cleanup'); END;
CREATE TRIGGER secret_ref_reject_pending_update BEFORE UPDATE OF secret_id ON connection_secret_refs
WHEN EXISTS(SELECT 1 FROM secret_write_journal WHERE secret_id=NEW.secret_id)
BEGIN SELECT RAISE(ABORT, 'secret pending cleanup'); END;
CREATE TRIGGER webdav_secret_reject_pending_insert BEFORE INSERT ON webdav_profiles
WHEN EXISTS(SELECT 1 FROM secret_write_journal WHERE secret_id=NEW.credential_secret_id)
BEGIN SELECT RAISE(ABORT, 'secret pending cleanup'); END;
CREATE TRIGGER webdav_secret_reject_pending_update BEFORE UPDATE OF credential_secret_id ON webdav_profiles
WHEN EXISTS(SELECT 1 FROM secret_write_journal WHERE secret_id=NEW.credential_secret_id)
BEGIN SELECT RAISE(ABORT, 'secret pending cleanup'); END;
INSERT INTO schema_migrations (version) VALUES (2);
"#;

const V3_SCHEMA: &str = r#"
CREATE TABLE secret_operation_receipts (
    operation_id TEXT PRIMARY KEY NOT NULL,
    connection_id TEXT NOT NULL,
    secret_type TEXT NOT NULL,
    secret_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL CHECK(state IN ('staged','committed','aborted')),
    cleanup_pending INTEGER NOT NULL CHECK(cleanup_pending IN (0,1))
);
INSERT INTO schema_migrations(version) VALUES(3);
"#;

const V4_SCHEMA: &str = r#"
CREATE TABLE webdav_secret_journal (
 operation_id TEXT PRIMARY KEY NOT NULL,
 profile_id TEXT NOT NULL REFERENCES webdav_profiles(id) ON DELETE RESTRICT,
 secret_id TEXT NOT NULL,
 kind TEXT NOT NULL CHECK(kind IN ('staged','cleanup')),
 UNIQUE(profile_id)
);
CREATE TABLE webdav_secret_receipts (
 operation_id TEXT PRIMARY KEY NOT NULL,
 profile_id TEXT NOT NULL,
 secret_id TEXT NOT NULL UNIQUE,
 state TEXT NOT NULL CHECK(state IN ('staged','committed','aborted')),
 cleanup_pending INTEGER NOT NULL CHECK(cleanup_pending IN (0,1))
);
CREATE TRIGGER webdav_journal_cross_operation BEFORE INSERT ON webdav_secret_journal
WHEN EXISTS(SELECT 1 FROM secret_write_journal WHERE operation_id=NEW.operation_id OR secret_id=NEW.secret_id UNION ALL SELECT 1 FROM secret_operation_receipts WHERE operation_id=NEW.operation_id OR secret_id=NEW.secret_id)
BEGIN SELECT RAISE(ABORT,'secret operation collision'); END;
CREATE TRIGGER ssh_journal_cross_operation BEFORE INSERT ON secret_write_journal
WHEN EXISTS(SELECT 1 FROM webdav_secret_journal WHERE operation_id=NEW.operation_id OR secret_id=NEW.secret_id UNION ALL SELECT 1 FROM webdav_secret_receipts WHERE operation_id=NEW.operation_id OR secret_id=NEW.secret_id)
BEGIN SELECT RAISE(ABORT,'secret operation collision'); END;
CREATE TRIGGER connection_ref_webdav_pending_insert BEFORE INSERT ON connection_secret_refs
WHEN EXISTS(SELECT 1 FROM webdav_secret_journal WHERE secret_id=NEW.secret_id)
BEGIN SELECT RAISE(ABORT,'secret pending cleanup'); END;
CREATE TRIGGER connection_ref_webdav_pending_update BEFORE UPDATE OF secret_id ON connection_secret_refs
WHEN EXISTS(SELECT 1 FROM webdav_secret_journal WHERE secret_id=NEW.secret_id)
BEGIN SELECT RAISE(ABORT,'secret pending cleanup'); END;
CREATE TRIGGER webdav_ref_webdav_pending_insert BEFORE INSERT ON webdav_profiles
WHEN EXISTS(SELECT 1 FROM webdav_secret_journal WHERE secret_id=NEW.credential_secret_id)
BEGIN SELECT RAISE(ABORT,'secret pending cleanup'); END;
CREATE TRIGGER webdav_ref_webdav_pending_update BEFORE UPDATE OF credential_secret_id ON webdav_profiles
WHEN EXISTS(SELECT 1 FROM webdav_secret_journal WHERE secret_id=NEW.credential_secret_id)
BEGIN SELECT RAISE(ABORT,'secret pending cleanup'); END;
INSERT INTO schema_migrations(version) VALUES(4);
"#;

const V5_SCHEMA: &str = r#"
CREATE TABLE webdav_provisional_profiles (
 operation_id TEXT PRIMARY KEY REFERENCES webdav_secret_receipts(operation_id) ON DELETE RESTRICT,
 profile_id TEXT NOT NULL UNIQUE REFERENCES webdav_profiles(id) ON DELETE RESTRICT
);
CREATE TRIGGER webdav_provisional_block_update BEFORE UPDATE ON webdav_profiles
WHEN EXISTS(SELECT 1 FROM webdav_provisional_profiles WHERE profile_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'profile creation pending'); END;
CREATE TRIGGER webdav_provisional_block_delete BEFORE DELETE ON webdav_profiles
WHEN EXISTS(SELECT 1 FROM webdav_provisional_profiles WHERE profile_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'profile creation pending'); END;
INSERT INTO schema_migrations(version) VALUES(5);
"#;

const V6_SCHEMA: &str = r#"
CREATE VIEW active_webdav_profiles AS SELECT p.* FROM webdav_profiles p WHERE NOT EXISTS (SELECT 1 FROM webdav_provisional_profiles x WHERE x.profile_id=p.id);
CREATE TRIGGER backup_runs_provisional_insert BEFORE INSERT ON backup_runs
WHEN EXISTS(SELECT 1 FROM webdav_provisional_profiles WHERE profile_id=NEW.profile_id)
BEGIN SELECT RAISE(ABORT,'profile creation pending'); END;
CREATE TRIGGER backup_runs_provisional_update BEFORE UPDATE OF profile_id ON backup_runs
WHEN EXISTS(SELECT 1 FROM webdav_provisional_profiles WHERE profile_id=NEW.profile_id)
BEGIN SELECT RAISE(ABORT,'profile creation pending'); END;
INSERT INTO schema_migrations(version) VALUES(6);
"#;

const V7_SCHEMA: &str = r#"
CREATE TEMP TABLE empty_aborted_webdav_profiles (profile_id TEXT PRIMARY KEY);
INSERT INTO empty_aborted_webdav_profiles(profile_id)
SELECT p.id FROM webdav_profiles p JOIN webdav_provisional_profiles x ON x.profile_id=p.id
JOIN webdav_secret_receipts r ON r.operation_id=x.operation_id
WHERE r.state='aborted' AND p.credential_secret_id IS NULL
AND NOT EXISTS (SELECT 1 FROM backup_runs b WHERE b.profile_id=p.id);
DELETE FROM webdav_provisional_profiles WHERE profile_id IN (SELECT profile_id FROM empty_aborted_webdav_profiles);
DELETE FROM webdav_profiles WHERE id IN (SELECT profile_id FROM empty_aborted_webdav_profiles);
DROP TABLE empty_aborted_webdav_profiles;
CREATE TRIGGER backup_run_prune_aborted_profile AFTER DELETE ON backup_runs
WHEN EXISTS (SELECT 1 FROM webdav_provisional_profiles x JOIN webdav_secret_receipts r ON r.operation_id=x.operation_id JOIN webdav_profiles p ON p.id=x.profile_id WHERE x.profile_id=OLD.profile_id AND r.state='aborted' AND p.credential_secret_id IS NULL)
AND NOT EXISTS (SELECT 1 FROM backup_runs WHERE profile_id=OLD.profile_id)
BEGIN
 DELETE FROM webdav_provisional_profiles WHERE profile_id=OLD.profile_id;
 DELETE FROM webdav_profiles WHERE id=OLD.profile_id AND credential_secret_id IS NULL;
END;
INSERT INTO schema_migrations(version) VALUES(7);
"#;

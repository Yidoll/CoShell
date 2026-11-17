//! Safe WebDAV profile persistence; connectivity testing is a separate operation.
use crate::feature_flags::{Feature, FeatureFlags};
use crate::secret_compensation::{
    CoordinatorError, NewWebDavProfile, OperationStatus, ReplaceOutcome, SecretCoordinator,
    SecretOperationLock, WebDavSqliteJournal,
};
use coshell_platform::secret_store::{Secret, SecretStore};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::{path::Path, sync::Arc, time::Duration};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebDavProfile {
    pub id: String,
    pub url: String,
    pub remote_directory: String,
    pub username: String,
    pub auto_backup: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedProfile {
    pub profile: WebDavProfile,
    pub cleanup_pending: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    Unsupported,
    InvalidInput,
    CredentialTargetChanged,
    Storage,
    Credential(CoordinatorError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStatus {
    NotFound,
    Staged,
    Aborted,
    AbortedRetainedRuns,
    Committed { cleanup_pending: bool },
}

/// Resume pending credential cleanup after an interruption.
pub fn recover_credentials(
    path: &Path,
    store: &impl SecretStore,
    flags: &FeatureFlags,
) -> Result<(), ConfigError> {
    if !flags.is_enabled(Feature::WebDavBackup) {
        return Err(ConfigError::Unsupported);
    }
    let path = path.canonicalize().map_err(|_| ConfigError::Storage)?;
    let dir = path.parent().ok_or(ConfigError::InvalidInput)?;
    if !store.accepts_data_dir(dir) {
        return Err(ConfigError::InvalidInput);
    }
    let lock = SecretOperationLock::acquire(dir).map_err(|_| ConfigError::Storage)?;
    let db = rusqlite::Connection::open(&path).map_err(|_| ConfigError::Storage)?;
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).map_err(|_| ConfigError::Storage)?);
    SecretCoordinator::new(journal, store, &lock)
        .map_err(ConfigError::Credential)?
        .recover()
        .map_err(ConfigError::Credential)
}

/// Returns only non-secret state; callers can resolve an ambiguous save before retrying.
pub fn operation_status(
    path: &Path,
    store: &impl SecretStore,
    flags: &FeatureFlags,
    profile_id: &str,
    operation: &str,
) -> Result<CredentialStatus, ConfigError> {
    if !flags.is_enabled(Feature::WebDavBackup) {
        return Err(ConfigError::Unsupported);
    }
    let path = path.canonicalize().map_err(|_| ConfigError::Storage)?;
    let dir = path.parent().ok_or(ConfigError::InvalidInput)?;
    if !store.accepts_data_dir(dir) {
        return Err(ConfigError::InvalidInput);
    }
    let lock = SecretOperationLock::acquire(dir).map_err(|_| ConfigError::Storage)?;
    let mut db = rusqlite::Connection::open(&path).map_err(|_| ConfigError::Storage)?;
    coshell_storage::migrate(&mut db).map_err(|_| ConfigError::Storage)?;
    let target: Option<String> = db
        .query_row(
            "SELECT profile_id FROM webdav_secret_receipts WHERE operation_id=?1",
            [operation],
            |r| r.get(0),
        )
        .optional()
        .map_err(|_| ConfigError::Storage)?;
    if target.as_deref().is_some_and(|id| id != profile_id) {
        return Err(ConfigError::Credential(CoordinatorError::OperationConflict));
    }
    if target.is_none() {
        let ssh_used:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM secret_operation_receipts WHERE operation_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE operation_id=?1)",[operation],|r|r.get(0)).map_err(|_|ConfigError::Storage)?;
        if ssh_used {
            return Err(ConfigError::Credential(CoordinatorError::OperationConflict));
        }
    }
    let retained:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM webdav_provisional_profiles p JOIN webdav_secret_receipts r ON r.operation_id=p.operation_id WHERE p.operation_id=?1 AND r.state='aborted')",[operation],|r|r.get(0)).map_err(|_|ConfigError::Storage)?;
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).map_err(|_| ConfigError::Storage)?);
    let coordinator =
        SecretCoordinator::new(journal, store, &lock).map_err(ConfigError::Credential)?;
    Ok(
        match coordinator
            .operation_status(operation)
            .map_err(ConfigError::Credential)?
        {
            OperationStatus::NotFound => CredentialStatus::NotFound,
            OperationStatus::Staged => CredentialStatus::Staged,
            OperationStatus::Aborted if retained => CredentialStatus::AbortedRetainedRuns,
            OperationStatus::Aborted => CredentialStatus::Aborted,
            OperationStatus::Committed(r) => CredentialStatus::Committed {
                cleanup_pending: r.outcome == ReplaceOutcome::CommittedCleanupPending,
            },
        },
    )
}

/// Read only committed profiles through the shared active-profile view.
pub fn get_profile(
    path: &Path,
    flags: &FeatureFlags,
    id: &str,
) -> Result<Option<WebDavProfile>, ConfigError> {
    if !flags.is_enabled(Feature::WebDavBackup) {
        return Err(ConfigError::Unsupported);
    }
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| ConfigError::Storage)?;
    db.query_row("SELECT id,url,remote_directory,username,auto_backup FROM active_webdav_profiles WHERE id=?1",[id],|r|Ok(WebDavProfile{id:r.get(0)?,url:r.get(1)?,remote_directory:r.get(2)?,username:r.get(3)?,auto_backup:r.get(4)?})).optional().map_err(|_|ConfigError::Storage)
}

/// The credential is never returned or stored in SQLite. A profile whose URL
/// or username changes must first be recreated without reusing its credential.
pub fn save_profile(
    path: &Path,
    store: &impl SecretStore,
    flags: &FeatureFlags,
    profile: &WebDavProfile,
    credential: Option<(&str, &Secret)>,
) -> Result<SavedProfile, ConfigError> {
    if !flags.is_enabled(Feature::WebDavBackup) {
        return Err(ConfigError::Unsupported);
    }
    if profile.id.is_empty()
        || profile.id.contains('\0')
        || profile.username.contains('\0')
        || profile.remote_directory.is_empty()
        || profile.remote_directory.contains('\0')
        || !valid_remote_directory(&profile.remote_directory)
    {
        return Err(ConfigError::InvalidInput);
    }
    if let Some((operation, _)) = credential {
        if operation.is_empty()
            || operation.len() > 128
            || !operation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(ConfigError::InvalidInput);
        }
    }
    coshell_backup::webdav::WebDav::new(
        &profile.url,
        &profile.username,
        "",
        Duration::from_secs(5),
    )
    .map_err(|_| ConfigError::InvalidInput)?;
    let path = path.canonicalize().map_err(|_| ConfigError::Storage)?;
    let dir = path.parent().ok_or(ConfigError::InvalidInput)?;
    if !store.accepts_data_dir(dir) {
        return Err(ConfigError::InvalidInput);
    }
    let lock = SecretOperationLock::acquire(dir).map_err(|_| ConfigError::Storage)?;
    let mut db = rusqlite::Connection::open(&path).map_err(|_| ConfigError::Storage)?;
    coshell_storage::migrate(&mut db).map_err(|_| ConfigError::Storage)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| ConfigError::Storage)?;
    if let Some((operation, _)) = credential {
        let used: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM webdav_secret_receipts WHERE operation_id=?1 UNION ALL SELECT 1 FROM webdav_secret_journal WHERE operation_id=?1 UNION ALL SELECT 1 FROM secret_operation_receipts WHERE operation_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE operation_id=?1)", [operation], |r| r.get(0)).map_err(|_| ConfigError::Storage)?;
        if used {
            return Err(ConfigError::Credential(CoordinatorError::OperationConflict));
        }
    }
    let existing: Option<(String, String, String, bool, Option<String>)> = tx
        .query_row(
            "SELECT url,username,remote_directory,auto_backup,credential_secret_id FROM webdav_profiles WHERE id=?1",
            [&profile.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()
        .map_err(|_| ConfigError::Storage)?;
    if existing.as_ref().is_some_and(|(url, username, _, _, id)| {
        id.is_some() && (url != &profile.url || username != &profile.username)
    }) {
        return Err(ConfigError::CredentialTargetChanged);
    }
    // Metadata and credential publication use separate transactions. Do not
    // let a failed credential write silently apply unrelated settings changes.
    if credential.is_some()
        && existing
            .as_ref()
            .is_some_and(|(url, username, directory, auto, _)| {
                url != &profile.url
                    || username != &profile.username
                    || directory != &profile.remote_directory
                    || *auto != profile.auto_backup
            })
    {
        return Err(ConfigError::InvalidInput);
    }
    if credential.is_some() && existing.is_none() && profile.auto_backup {
        return Err(ConfigError::InvalidInput);
    }
    let new_profile = existing.is_none();
    if !(new_profile && credential.is_some()) {
        tx.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,auto_backup) VALUES (?1,?2,?3,?4,?5) ON CONFLICT(id) DO UPDATE SET url=excluded.url,remote_directory=excluded.remote_directory,username=excluded.username,auto_backup=excluded.auto_backup",params![profile.id,profile.url,profile.remote_directory,profile.username,profile.auto_backup]).map_err(|_|ConfigError::Storage)?;
    }
    tx.commit().map_err(|_| ConfigError::Storage)?;
    let mut cleanup_pending = false;
    if let Some((operation, secret)) = credential {
        let journal =
            Arc::new(WebDavSqliteJournal::new(db, &lock).map_err(|_| ConfigError::Storage)?);
        let coordinator =
            SecretCoordinator::new(journal, store, &lock).map_err(ConfigError::Credential)?;
        let result = if new_profile {
            coordinator.create_webdav_profile(
                operation,
                &NewWebDavProfile {
                    id: &profile.id,
                    url: &profile.url,
                    directory: &profile.remote_directory,
                    username: &profile.username,
                    auto_backup: profile.auto_backup,
                },
                secret,
            )
        } else {
            coordinator.replace_idempotent(operation, &profile.id, "webdav_password", secret)
        }
        .map_err(ConfigError::Credential)?;
        cleanup_pending = result.outcome == ReplaceOutcome::CommittedCleanupPending;
    }
    Ok(SavedProfile {
        profile: profile.clone(),
        cleanup_pending,
    })
}

fn valid_remote_directory(value: &str) -> bool {
    if value == "/" {
        return true;
    }
    let value = value.strip_prefix('/').unwrap_or(value);
    !value.is_empty()
        && value.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~'))
        })
}

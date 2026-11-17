//! Crash-recoverable secret replacement across a durable journal and OS store.
//! Callers MUST hold a cross-process data-directory lock for the entire lifetime
//! of every operation and recovery, including when using a custom Journal.
use coshell_platform::secret_store::{Secret, SecretStore};
use fs2::FileExt;
use std::{
    fmt,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

/// An advisory cross-process lock for a single data directory. The caller must
/// keep this guard alive across repository access, OS writes, and recovery.
pub struct SecretOperationLock {
    file: File,
    directory: PathBuf,
    local: Mutex<()>,
}

impl SecretOperationLock {
    pub fn acquire(data_dir: &Path) -> Result<Self, CoordinatorError> {
        let (file, directory) = Self::open(data_dir)?;
        file.lock_exclusive().map_err(|_| CoordinatorError::Lock)?;
        Ok(Self {
            file,
            directory,
            local: Mutex::new(()),
        })
    }

    pub fn try_acquire(data_dir: &Path) -> Result<Self, CoordinatorError> {
        let (file, directory) = Self::open(data_dir)?;
        file.try_lock_exclusive()
            .map_err(|_| CoordinatorError::Lock)?;
        Ok(Self {
            file,
            directory,
            local: Mutex::new(()),
        })
    }

    fn open(data_dir: &Path) -> Result<(File, PathBuf), CoordinatorError> {
        if !data_dir.is_absolute() {
            return Err(CoordinatorError::Lock);
        }
        let directory = data_dir
            .canonicalize()
            .map_err(|_| CoordinatorError::Lock)?;
        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ | FILE_SHARE_WRITE, deliberately no FILE_SHARE_DELETE.
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .share_mode(0x3)
                .open(directory.join(".secret-operations.lock"))
                .map_err(|_| CoordinatorError::Lock)?
        };
        // Lock the data directory itself: unlike a separate lock file it cannot
        // be unlinked while it contains the database. The caller must protect
        // the parent directory from untrusted renames/replacements.
        #[cfg(unix)]
        let file = File::open(&directory).map_err(|_| CoordinatorError::Lock)?;
        #[cfg(not(any(windows, unix)))]
        return Err(CoordinatorError::Lock);
        #[cfg(any(windows, unix))]
        Ok((file, directory))
    }
}

impl Drop for SecretOperationLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Untrusted external error text, URLs and commands may contain arbitrary
/// secrets; never attempt partial string-based scrubbing for log messages.
pub fn safe_log_message(_untrusted: &str) -> &'static str {
    "[REDACTED]"
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoordinatorError {
    Lock,
    RandomUnavailable,
    Journal,
    SecretStore,
    CleanupPending,
    CommitUnknown,
    OperationConflict,
    OperationPending,
    OperationAborted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceOutcome {
    Committed,
    CommittedCleanupPending,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplaceResult {
    pub secret_id: String,
    pub outcome: ReplaceOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationStatus {
    NotFound,
    Staged,
    Aborted,
    Committed(ReplaceResult),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    pub connection_id: String,
    pub secret_type: String,
    pub secret_id: String,
    pub status: OperationStatus,
}

impl fmt::Display for CoordinatorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoordinatorError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalKind {
    Staged,
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JournalEntry {
    pub operation_id: String,
    pub secret_id: String,
    pub kind: JournalKind,
}

pub struct NewWebDavProfile<'a> {
    pub id: &'a str,
    pub url: &'a str,
    pub directory: &'a str,
    pub username: &'a str,
    pub auto_backup: bool,
}

/// Implementations must persist prepare before returning and commit the new
/// reference and cleanup transition in one transaction. IDs are opaque only.
pub trait Journal: Send + Sync {
    fn accepts_lock(&self, _lock: &SecretOperationLock) -> bool {
        false
    }
    fn prepare(
        &self,
        operation: &str,
        connection: &str,
        kind: &str,
        new_id: &str,
    ) -> Result<(), ()>;
    fn commit(&self, operation: &str) -> Result<(), ()>;
    fn pending(&self) -> Result<Vec<JournalEntry>, ()>;
    fn referenced(&self, secret_id: &str) -> Result<bool, ()>;
    fn finish(&self, operation: &str, secret_id: &str) -> Result<(), ()>;
    fn receipt(&self, _operation: &str) -> Result<Option<Receipt>, ()> {
        Err(())
    }
    fn prepare_receipt(
        &self,
        _operation: &str,
        _connection: &str,
        _kind: &str,
        _new_id: &str,
    ) -> Result<Receipt, ()> {
        Err(())
    }
    fn commit_receipt(&self, _operation: &str) -> Result<(), ()> {
        Err(())
    }
    fn prepare_new_webdav(
        &self,
        _operation: &str,
        _profile: &NewWebDavProfile<'_>,
        _id: &str,
    ) -> Result<Receipt, ()> {
        Err(())
    }
}

/// SQLite journal adapter. All secret-reference mutation in the application
/// must use the same data-directory lock before accessing this connection.
pub struct SqliteJournal<'a> {
    db: Mutex<rusqlite::Connection>,
    _lock: &'a SecretOperationLock,
}

impl<'a> SqliteJournal<'a> {
    pub fn new(
        mut connection: rusqlite::Connection,
        lock: &'a SecretOperationLock,
    ) -> Result<Self, CoordinatorError> {
        let database: String = connection
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name = 'main'",
                [],
                |row| row.get(0),
            )
            .map_err(|_| CoordinatorError::Journal)?;
        let path = Path::new(&database)
            .canonicalize()
            .map_err(|_| CoordinatorError::Lock)?;
        if path.parent() != Some(lock.directory.as_path()) {
            return Err(CoordinatorError::Lock);
        }
        if !connection.is_autocommit() {
            return Err(CoordinatorError::Journal);
        }
        coshell_storage::migrate(&mut connection).map_err(|_| CoordinatorError::Journal)?;
        let foreign_keys: u8 = connection
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .map_err(|_| CoordinatorError::Journal)?;
        if foreign_keys != 1 {
            return Err(CoordinatorError::Journal);
        }
        Ok(Self {
            db: Mutex::new(connection),
            _lock: lock,
        })
    }
}

impl Journal for SqliteJournal<'_> {
    fn accepts_lock(&self, lock: &SecretOperationLock) -> bool {
        std::ptr::eq(self._lock, lock)
    }
    fn prepare(
        &self,
        operation: &str,
        connection: &str,
        kind: &str,
        new_id: &str,
    ) -> Result<(), ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        coshell_storage::Repository::new(&mut db)
            .prepare_secret_write(operation, connection, kind, new_id)
            .map_err(|_| ())
    }
    fn commit(&self, operation: &str) -> Result<(), ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        coshell_storage::Repository::new(&mut db)
            .commit_secret_write(operation)
            .map_err(|_| ())
    }
    fn pending(&self) -> Result<Vec<JournalEntry>, ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        coshell_storage::Repository::new(&mut db)
            .pending_secret_cleanup()
            .map_err(|_| ())
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| JournalEntry {
                        operation_id: entry.operation_id,
                        secret_id: entry.secret_id,
                        kind: match entry.kind {
                            coshell_storage::SecretCleanupKind::Staged => JournalKind::Staged,
                            coshell_storage::SecretCleanupKind::Cleanup => JournalKind::Cleanup,
                        },
                    })
                    .collect()
            })
    }
    fn referenced(&self, id: &str) -> Result<bool, ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        coshell_storage::Repository::new(&mut db)
            .secret_id_is_referenced(id)
            .map_err(|_| ())
    }
    fn finish(&self, operation: &str, id: &str) -> Result<(), ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        if coshell_storage::secret_receipt::SecretReceiptRepository::new(&mut db)
            .get(operation)
            .map_err(|_| ())?
            .is_some()
        {
            coshell_storage::secret_receipt::SecretReceiptRepository::new(&mut db)
                .finish_cleanup(operation, id)
                .map_err(|_| ())
        } else {
            coshell_storage::Repository::new(&mut db)
                .finish_secret_cleanup(operation, id)
                .map_err(|_| ())
        }
    }
    fn receipt(&self, operation: &str) -> Result<Option<Receipt>, ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        Ok(
            coshell_storage::secret_receipt::SecretReceiptRepository::new(&mut db)
                .get(operation)
                .map_err(|_| ())?
                .map(map_receipt),
        )
    }
    fn prepare_receipt(
        &self,
        operation: &str,
        connection: &str,
        kind: &str,
        new_id: &str,
    ) -> Result<Receipt, ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        coshell_storage::secret_receipt::SecretReceiptRepository::new(&mut db)
            .prepare(operation, connection, kind, new_id)
            .map(map_receipt)
            .map_err(|_| ())
    }
    fn commit_receipt(&self, operation: &str) -> Result<(), ()> {
        let mut db = self.db.lock().map_err(|_| ())?;
        coshell_storage::secret_receipt::SecretReceiptRepository::new(&mut db)
            .commit(operation)
            .map_err(|_| ())
    }
}

fn map_receipt(r: coshell_storage::secret_receipt::SecretReceipt) -> Receipt {
    use coshell_storage::secret_receipt::ReceiptState;
    let status = match r.state {
        ReceiptState::Staged => OperationStatus::Staged,
        ReceiptState::Aborted => OperationStatus::Aborted,
        ReceiptState::Committed => OperationStatus::Committed(ReplaceResult {
            secret_id: r.secret_id.clone(),
            outcome: if r.cleanup_pending {
                ReplaceOutcome::CommittedCleanupPending
            } else {
                ReplaceOutcome::Committed
            },
        }),
    };
    Receipt {
        connection_id: r.connection_id,
        secret_type: r.secret_type,
        secret_id: r.secret_id,
        status,
    }
}

/// WebDAV credential journal sharing the same lock and recovery protocol.
pub struct WebDavSqliteJournal<'a>(SqliteJournal<'a>);
impl<'a> WebDavSqliteJournal<'a> {
    pub fn new(
        db: rusqlite::Connection,
        lock: &'a SecretOperationLock,
    ) -> Result<Self, CoordinatorError> {
        Ok(Self(SqliteJournal::new(db, lock)?))
    }
}
impl Journal for WebDavSqliteJournal<'_> {
    fn accepts_lock(&self, lock: &SecretOperationLock) -> bool {
        self.0.accepts_lock(lock)
    }
    fn prepare(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), ()> {
        Err(())
    }
    fn commit(&self, _: &str) -> Result<(), ()> {
        Err(())
    }
    fn pending(&self) -> Result<Vec<JournalEntry>, ()> {
        let mut db = self.0.db.lock().map_err(|_| ())?;
        coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut db)
            .pending()
            .map_err(|_| ())
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|(operation_id, secret_id, staged)| JournalEntry {
                        operation_id,
                        secret_id,
                        kind: if staged {
                            JournalKind::Staged
                        } else {
                            JournalKind::Cleanup
                        },
                    })
                    .collect()
            })
    }
    fn referenced(&self, id: &str) -> Result<bool, ()> {
        self.0.referenced(id)
    }
    fn finish(&self, op: &str, id: &str) -> Result<(), ()> {
        let mut db = self.0.db.lock().map_err(|_| ())?;
        coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut db)
            .finish(op, id)
            .map_err(|_| ())
    }
    fn receipt(&self, op: &str) -> Result<Option<Receipt>, ()> {
        let mut db = self.0.db.lock().map_err(|_| ())?;
        coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut db)
            .get(op)
            .map(|r| r.map(map_receipt))
            .map_err(|_| ())
    }
    fn prepare_receipt(
        &self,
        op: &str,
        profile: &str,
        kind: &str,
        id: &str,
    ) -> Result<Receipt, ()> {
        let mut db = self.0.db.lock().map_err(|_| ())?;
        coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut db)
            .prepare(op, profile, kind, id)
            .map(map_receipt)
            .map_err(|_| ())
    }
    fn commit_receipt(&self, op: &str) -> Result<(), ()> {
        let mut db = self.0.db.lock().map_err(|_| ())?;
        coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut db)
            .commit(op)
            .map_err(|_| ())
    }
    fn prepare_new_webdav(
        &self,
        op: &str,
        profile: &NewWebDavProfile<'_>,
        id: &str,
    ) -> Result<Receipt, ()> {
        let mut db = self.0.db.lock().map_err(|_| ())?;
        coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut db)
            .prepare_new_profile(
                op,
                profile.id,
                profile.url,
                profile.directory,
                profile.username,
                profile.auto_backup,
                id,
            )
            .map(map_receipt)
            .map_err(|_| ())
    }
}

fn random_id() -> Result<String, CoordinatorError> {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| CoordinatorError::RandomUnavailable)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub struct SecretCoordinator<'a, J: Journal> {
    journal: Arc<J>,
    store: &'a dyn SecretStore,
    _lock: &'a SecretOperationLock,
}
impl<'a, J: Journal> SecretCoordinator<'a, J> {
    pub fn new(
        journal: Arc<J>,
        store: &'a dyn SecretStore,
        lock: &'a SecretOperationLock,
    ) -> Result<Self, CoordinatorError> {
        if !journal.accepts_lock(lock) || !store.accepts_data_dir(&lock.directory) {
            return Err(CoordinatorError::Lock);
        }
        Ok(Self {
            journal,
            store,
            _lock: lock,
        })
    }

    /// Operation IDs are supplied by the caller and reused for retries. Once an
    /// ID exists, its secret input is never written again, even after a crash.
    pub fn replace_idempotent(
        &self,
        operation: &str,
        connection: &str,
        kind: &str,
        secret: &Secret,
    ) -> Result<ReplaceResult, CoordinatorError> {
        self.replace_inner(operation, connection, kind, secret, None)
    }

    pub fn create_webdav_profile(
        &self,
        operation: &str,
        profile: &NewWebDavProfile<'_>,
        secret: &Secret,
    ) -> Result<ReplaceResult, CoordinatorError> {
        self.replace_inner(
            operation,
            profile.id,
            "webdav_password",
            secret,
            Some(profile),
        )
    }

    fn replace_inner(
        &self,
        operation: &str,
        connection: &str,
        kind: &str,
        secret: &Secret,
        new_profile: Option<&NewWebDavProfile<'_>>,
    ) -> Result<ReplaceResult, CoordinatorError> {
        if operation.is_empty()
            || operation.len() > 128
            || !operation
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(CoordinatorError::OperationConflict);
        }
        let _guard = self
            ._lock
            .local
            .lock()
            .map_err(|_| CoordinatorError::Lock)?;
        if let Some(receipt) = self
            .journal
            .receipt(operation)
            .map_err(|_| CoordinatorError::Journal)?
        {
            if receipt.connection_id != connection || receipt.secret_type != kind {
                return Err(CoordinatorError::OperationConflict);
            }
            // A write request carrying a secret is not a status lookup. We do
            // not persist fingerprints of secret values, so we cannot prove a
            // repeated payload is identical. Callers must query the receipt.
            return Err(CoordinatorError::OperationConflict);
        }
        let id = random_id()?;
        let receipt = match new_profile {
            Some(profile) => self.journal.prepare_new_webdav(operation, profile, &id),
            None => self
                .journal
                .prepare_receipt(operation, connection, kind, &id),
        }
        .map_err(|_| CoordinatorError::Journal)?;
        // Another writer may have prepared the same ID before this transaction.
        if receipt.secret_id != id {
            return Err(CoordinatorError::OperationConflict);
        }
        if self.store.set(&id, secret).is_err() {
            return match self.cleanup(operation, &id) {
                Ok(()) => Err(CoordinatorError::SecretStore),
                Err(_) => Err(CoordinatorError::CleanupPending),
            };
        }
        if self.journal.commit_receipt(operation).is_err() {
            return match self.journal.receipt(operation) {
                Ok(Some(receipt)) => match receipt.status {
                    OperationStatus::Committed(result) => Ok(result),
                    _ => Err(CoordinatorError::CommitUnknown),
                },
                _ => Err(CoordinatorError::CommitUnknown),
            };
        }
        let _ = self.recover_locked();
        match self.journal.receipt(operation) {
            Ok(Some(receipt)) => receipt_result(receipt.status),
            _ => Err(CoordinatorError::CommitUnknown),
        }
    }

    pub fn operation_status(&self, operation: &str) -> Result<OperationStatus, CoordinatorError> {
        let _guard = self
            ._lock
            .local
            .lock()
            .map_err(|_| CoordinatorError::Lock)?;
        Ok(self
            .journal
            .receipt(operation)
            .map_err(|_| CoordinatorError::Journal)?
            .map_or(OperationStatus::NotFound, |r| r.status))
    }

    fn cleanup(&self, operation: &str, id: &str) -> Result<(), CoordinatorError> {
        if self
            .journal
            .referenced(id)
            .map_err(|_| CoordinatorError::CleanupPending)?
        {
            return Err(CoordinatorError::CleanupPending);
        }
        self.store
            .delete(id)
            .map_err(|_| CoordinatorError::CleanupPending)?;
        self.journal
            .finish(operation, id)
            .map_err(|_| CoordinatorError::CleanupPending)
    }

    pub fn recover(&self) -> Result<(), CoordinatorError> {
        let _guard = self
            ._lock
            .local
            .lock()
            .map_err(|_| CoordinatorError::Lock)?;
        self.recover_locked()
    }

    fn recover_locked(&self) -> Result<(), CoordinatorError> {
        let pending = self
            .journal
            .pending()
            .map_err(|_| CoordinatorError::Journal)?;
        let mut failed = false;
        for entry in pending {
            if self.cleanup(&entry.operation_id, &entry.secret_id).is_err() {
                failed = true;
            }
        }
        if failed {
            Err(CoordinatorError::CleanupPending)
        } else {
            Ok(())
        }
    }
}

fn receipt_result(status: OperationStatus) -> Result<ReplaceResult, CoordinatorError> {
    match status {
        OperationStatus::Committed(result) => Ok(result),
        OperationStatus::Staged => Err(CoordinatorError::OperationPending),
        OperationStatus::Aborted => Err(CoordinatorError::OperationAborted),
        OperationStatus::NotFound => Err(CoordinatorError::Journal),
    }
}

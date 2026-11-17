use coshell_core::secret_compensation::{
    safe_log_message, CoordinatorError, Journal, JournalEntry, JournalKind, OperationStatus,
    Receipt, ReplaceOutcome, ReplaceResult, SecretCoordinator, SecretOperationLock, SqliteJournal,
};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct JournalState {
    entries: Vec<JournalEntry>,
    current: Option<String>,
    fail_commit: bool,
    receipts: HashMap<String, Receipt>,
}

#[test]
fn sqlite_journal_survives_restart_and_recovery_removes_staged_secret() {
    let dir = std::env::temp_dir().join(format!("coshell-journal-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("journal.db");
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')", []).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    journal
        .prepare("operation", "c", "password", "staged")
        .unwrap();
    let store = Store::default();
    store
        .set("staged", &Secret::new(b"sensitive".to_vec()))
        .unwrap();
    drop(journal);
    let db = rusqlite::Connection::open(&path).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    SecretCoordinator::new(journal.clone(), &store, &guard)
        .unwrap()
        .recover()
        .unwrap();
    assert!(store.get("staged").unwrap().is_none());
    assert!(journal.pending().unwrap().is_empty());
    drop(journal);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn sqlite_commit_failure_cleans_staged_secret_after_reopen() {
    let dir = std::env::temp_dir().join(format!("coshell-journal-failure-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("journal.db");
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')", []).unwrap();
    db.execute_batch("CREATE TRIGGER reject_secret BEFORE INSERT ON connection_secret_refs BEGIN SELECT RAISE(ABORT, 'test'); END;").unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    let store = Store::default();
    let result = SecretCoordinator::new(journal.clone(), &store, &guard)
        .unwrap()
        .replace_idempotent(
            "failed-op",
            "c",
            "password",
            &Secret::new(b"secret".to_vec()),
        );
    assert_eq!(result, Err(CoordinatorError::CommitUnknown));
    assert_eq!(journal.pending().unwrap().len(), 1);
    drop(journal);
    let db = rusqlite::Connection::open(&path).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    SecretCoordinator::new(journal.clone(), &store, &guard)
        .unwrap()
        .recover()
        .unwrap();
    assert!(store.values.lock().unwrap().is_empty());
    assert!(journal.pending().unwrap().is_empty());
    drop(journal);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn independent_handles_cannot_hold_the_same_lock_concurrently() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-lock-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let first = SecretOperationLock::acquire(&dir).unwrap();
    assert!(SecretOperationLock::try_acquire(&dir).is_err());
    drop(first);
    assert!(SecretOperationLock::try_acquire(&dir).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn journal_rejects_lock_for_an_unrelated_database_directory() {
    let dir = std::env::temp_dir().join(format!("coshell-lock-domain-{}", std::process::id()));
    let other = dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let guard = SecretOperationLock::acquire(&other).unwrap();
    let db = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
    assert!(SqliteJournal::new(db, &guard).is_err());
    let right = SecretOperationLock::acquire(&dir).unwrap();
    let db = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &right).unwrap());
    let store = Store::default();
    assert!(SecretCoordinator::new(journal.clone(), &store, &guard).is_err());
    drop(journal);
    drop(right);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn recover_waits_for_in_progress_secret_write_with_shared_guard() {
    use std::sync::mpsc;
    struct BlockingStore {
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        values: Mutex<HashMap<String, Vec<u8>>>,
    }
    impl SecretStore for BlockingStore {
        fn set(&self, id: &str, value: &Secret) -> Result<(), SecretError> {
            self.values
                .lock()
                .unwrap()
                .insert(id.into(), value.expose().to_vec());
            self.entered.send(()).unwrap();
            self.release.lock().unwrap().recv().unwrap();
            Ok(())
        }
        fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
            Ok(self
                .values
                .lock()
                .unwrap()
                .get(id)
                .cloned()
                .map(Secret::new))
        }
        fn delete(&self, id: &str) -> Result<(), SecretError> {
            self.values.lock().unwrap().remove(id);
            Ok(())
        }
    }
    let dir = std::env::temp_dir().join(format!("coshell-lock-thread-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let journal = Arc::new(FakeJournal::default());
    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let store = BlockingStore {
        entered: entered_tx,
        release: Mutex::new(release_rx),
        values: Mutex::new(HashMap::new()),
    };
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            SecretCoordinator::new(journal.clone(), &store, &guard)
                .unwrap()
                .replace_idempotent("op", "c", "password", &Secret::new(b"value".to_vec()))
        });
        entered_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let journal_for_recovery = journal.clone();
        let store_for_recovery = &store;
        let guard_for_recovery = &guard;
        let recovery = scope.spawn(move || {
            let result = SecretCoordinator::new(
                journal_for_recovery,
                store_for_recovery,
                guard_for_recovery,
            )
            .unwrap()
            .recover();
            done_tx.send(result).unwrap();
        });
        assert!(done_rx
            .recv_timeout(std::time::Duration::from_millis(100))
            .is_err());
        release_tx.send(()).unwrap();
        let id = writer.join().unwrap().unwrap().secret_id;
        recovery.join().unwrap();
        assert!(done_rx.recv().unwrap().is_ok());
        assert_eq!(store.get(&id).unwrap().unwrap().expose(), b"value");
    });
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn logging_arbitrary_external_input_never_discloses_secret() {
    let result = safe_log_message("https://user:password@example.com --token abc123");
    assert!(!result.contains("password"));
    assert!(!result.contains("abc123"));
}
#[derive(Default)]
struct FakeJournal(Mutex<JournalState>);
impl Journal for FakeJournal {
    fn accepts_lock(&self, _lock: &SecretOperationLock) -> bool {
        true
    }
    fn prepare(&self, op: &str, _connection: &str, _kind: &str, new: &str) -> Result<(), ()> {
        self.0.lock().unwrap().entries.push(JournalEntry {
            operation_id: op.into(),
            secret_id: new.into(),
            kind: JournalKind::Staged,
        });
        Ok(())
    }
    fn commit(&self, op: &str) -> Result<(), ()> {
        let mut s = self.0.lock().unwrap();
        if s.fail_commit {
            return Err(());
        }
        let pos = s
            .entries
            .iter()
            .position(|e| e.operation_id == op)
            .ok_or(())?;
        let new = s.entries.remove(pos).secret_id;
        if let Some(old) = s.current.replace(new) {
            s.entries.push(JournalEntry {
                operation_id: op.into(),
                secret_id: old,
                kind: JournalKind::Cleanup,
            });
        }
        Ok(())
    }
    fn pending(&self) -> Result<Vec<JournalEntry>, ()> {
        Ok(self.0.lock().unwrap().entries.clone())
    }
    fn referenced(&self, id: &str) -> Result<bool, ()> {
        Ok(self.0.lock().unwrap().current.as_deref() == Some(id))
    }
    fn finish(&self, op: &str, id: &str) -> Result<(), ()> {
        let mut s = self.0.lock().unwrap();
        let entry = s
            .entries
            .iter()
            .find(|e| e.operation_id == op && e.secret_id == id)
            .cloned();
        s.entries
            .retain(|e| e.operation_id != op || e.secret_id != id);
        if let (Some(entry), Some(receipt)) = (entry, s.receipts.get_mut(op)) {
            receipt.status = if entry.kind == JournalKind::Staged {
                OperationStatus::Aborted
            } else {
                OperationStatus::Committed(ReplaceResult {
                    secret_id: receipt.secret_id.clone(),
                    outcome: ReplaceOutcome::Committed,
                })
            };
        }
        Ok(())
    }
    fn receipt(&self, op: &str) -> Result<Option<Receipt>, ()> {
        Ok(self.0.lock().unwrap().receipts.get(op).cloned())
    }
    fn prepare_receipt(
        &self,
        op: &str,
        connection: &str,
        kind: &str,
        id: &str,
    ) -> Result<Receipt, ()> {
        self.prepare(op, connection, kind, id)?;
        let receipt = Receipt {
            connection_id: connection.into(),
            secret_type: kind.into(),
            secret_id: id.into(),
            status: OperationStatus::Staged,
        };
        self.0
            .lock()
            .unwrap()
            .receipts
            .insert(op.into(), receipt.clone());
        Ok(receipt)
    }
    fn commit_receipt(&self, op: &str) -> Result<(), ()> {
        self.commit(op)?;
        let mut s = self.0.lock().unwrap();
        let pending = s.entries.iter().any(|e| e.operation_id == op);
        let receipt = s.receipts.get_mut(op).ok_or(())?;
        receipt.status = OperationStatus::Committed(ReplaceResult {
            secret_id: receipt.secret_id.clone(),
            outcome: if pending {
                ReplaceOutcome::CommittedCleanupPending
            } else {
                ReplaceOutcome::Committed
            },
        });
        Ok(())
    }
}

#[derive(Default)]
struct Store {
    values: Mutex<HashMap<String, Vec<u8>>>,
    fail_set: Mutex<bool>,
    fail_delete: Mutex<bool>,
}
impl SecretStore for Store {
    fn set(&self, id: &str, secret: &Secret) -> Result<(), SecretError> {
        self.values
            .lock()
            .unwrap()
            .insert(id.to_owned(), secret.expose().to_vec());
        if *self.fail_set.lock().unwrap() {
            return Err(SecretError::StorageFailure);
        }
        Ok(())
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .map(Secret::new))
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        if *self.fail_delete.lock().unwrap() {
            return Err(SecretError::StorageFailure);
        }
        self.values.lock().unwrap().remove(id);
        Ok(())
    }
}

#[test]
fn failed_secret_write_or_db_commit_keeps_recovery_record_until_cleanup() {
    for fail_set in [false, true] {
        let dir = std::env::temp_dir().join(format!(
            "coshell-secret-test-{}-{fail_set}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let guard = SecretOperationLock::acquire(&dir).unwrap();
        let journal = Arc::new(FakeJournal::default());
        let store = Store::default();
        journal.0.lock().unwrap().fail_commit = !fail_set;
        *store.fail_set.lock().unwrap() = fail_set;
        *store.fail_delete.lock().unwrap() = true;
        let c = SecretCoordinator::new(journal.clone(), &store, &guard).unwrap();
        assert_eq!(
            c.replace_idempotent(
                "failed",
                "connection",
                "password",
                &Secret::new(b"secret".to_vec())
            ),
            Err(if fail_set {
                CoordinatorError::CleanupPending
            } else {
                CoordinatorError::CommitUnknown
            })
        );
        assert_eq!(journal.pending().unwrap().len(), 1);
        *store.fail_delete.lock().unwrap() = false;
        drop(c);
        SecretCoordinator::new(journal.clone(), &store, &guard)
            .unwrap()
            .recover()
            .unwrap();
        assert!(journal.pending().unwrap().is_empty());
        assert!(store.values.lock().unwrap().is_empty());
        drop(guard);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

#[test]
fn replacement_preserves_new_reference_and_retries_old_cleanup() {
    let journal = Arc::new(FakeJournal::default());
    let store = Store::default();
    let dir = std::env::temp_dir().join(format!(
        "coshell-secret-test-{}-replacement",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let c = SecretCoordinator::new(journal.clone(), &store, &guard).unwrap();
    let old = c
        .replace_idempotent(
            "first",
            "connection",
            "password",
            &Secret::new(b"old".to_vec()),
        )
        .unwrap()
        .secret_id;
    *store.fail_delete.lock().unwrap() = true;
    let committed = c
        .replace_idempotent(
            "second",
            "connection",
            "password",
            &Secret::new(b"new".to_vec()),
        )
        .unwrap();
    assert_eq!(committed.outcome, ReplaceOutcome::CommittedCleanupPending);
    let new = journal.0.lock().unwrap().current.clone().unwrap();
    assert_eq!(new, committed.secret_id);
    assert_ne!(old, new);
    *store.fail_delete.lock().unwrap() = false;
    c.recover().unwrap();
    assert!(store.get(&old).unwrap().is_none());
    assert_eq!(store.get(&new).unwrap().unwrap().expose(), b"new");
    assert!(journal.pending().unwrap().is_empty());
    drop(c);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[cfg(windows)]
fn lock_file_cannot_be_removed_or_replaced_while_held() {
    let dir = std::env::temp_dir().join(format!("coshell-lock-replace-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let lock = dir.join(".secret-operations.lock");
    assert!(std::fs::remove_file(&lock).is_err());
    assert!(SecretOperationLock::try_acquire(&dir).is_err());
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn native_store_must_match_the_locked_data_directory() {
    use coshell_platform::secret_store::PlatformSecretStore;
    let root = std::env::temp_dir().join(format!("coshell-secret-binding-{}", std::process::id()));
    let a = root.join("a");
    let b = root.join("b");
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    let guard = SecretOperationLock::acquire(&a).unwrap();
    let store = PlatformSecretStore::for_data_dir(&b).unwrap();
    let journal = Arc::new(FakeJournal::default());
    assert!(matches!(
        SecretCoordinator::new(journal, &store, &guard),
        Err(CoordinatorError::Lock)
    ));
    drop(guard);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[cfg(target_os = "macos")]
fn populated_data_directory_cannot_be_removed_and_second_lock_fails() {
    let dir = std::env::temp_dir().join(format!("coshell-dir-lock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("db.sqlite"), b"test").unwrap();
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    assert!(std::fs::remove_dir(&dir).is_err());
    assert!(SecretOperationLock::try_acquire(&dir).is_err());
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

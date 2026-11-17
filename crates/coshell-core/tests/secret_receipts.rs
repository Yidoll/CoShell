use coshell_core::secret_compensation::{
    CoordinatorError, Journal, OperationStatus, ReplaceOutcome, SecretCoordinator,
    SecretOperationLock, SqliteJournal,
};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

struct Ambiguous<'a>(SqliteJournal<'a>);
impl coshell_core::secret_compensation::Journal for Ambiguous<'_> {
    fn accepts_lock(&self, l: &SecretOperationLock) -> bool {
        self.0.accepts_lock(l)
    }
    fn prepare(&self, a: &str, b: &str, c: &str, d: &str) -> Result<(), ()> {
        self.0.prepare(a, b, c, d)
    }
    fn commit(&self, a: &str) -> Result<(), ()> {
        self.0.commit(a)
    }
    fn pending(&self) -> Result<Vec<coshell_core::secret_compensation::JournalEntry>, ()> {
        self.0.pending()
    }
    fn referenced(&self, id: &str) -> Result<bool, ()> {
        self.0.referenced(id)
    }
    fn finish(&self, a: &str, b: &str) -> Result<(), ()> {
        self.0.finish(a, b)
    }
    fn receipt(&self, op: &str) -> Result<Option<coshell_core::secret_compensation::Receipt>, ()> {
        self.0.receipt(op)
    }
    fn prepare_receipt(
        &self,
        a: &str,
        b: &str,
        c: &str,
        d: &str,
    ) -> Result<coshell_core::secret_compensation::Receipt, ()> {
        self.0.prepare_receipt(a, b, c, d)
    }
    fn commit_receipt(&self, op: &str) -> Result<(), ()> {
        self.0.commit_receipt(op)?;
        Err(())
    }
}

#[test]
fn staged_secret_after_crash_is_removed_and_receipt_becomes_aborted() {
    let dir = std::env::temp_dir().join(format!("coshell-secret-staged-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    journal
        .prepare_receipt("staged", "c", "password", "secret-id")
        .unwrap();
    let store = Store::default();
    store
        .set("secret-id", &Secret::new(b"secret".to_vec()))
        .unwrap();
    drop(journal);
    let db = rusqlite::Connection::open(&path).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    let c = SecretCoordinator::new(journal.clone(), &store, &guard).unwrap();
    assert_eq!(
        c.operation_status("staged").unwrap(),
        OperationStatus::Staged
    );
    assert_eq!(
        c.replace_idempotent("staged", "c", "password", &Secret::new(b"new".to_vec()))
            .unwrap_err(),
        CoordinatorError::OperationConflict
    );
    c.recover().unwrap();
    assert_eq!(
        c.operation_status("staged").unwrap(),
        OperationStatus::Aborted
    );
    assert!(store.get("secret-id").unwrap().is_none());
    assert_eq!(*store.writes.lock().unwrap(), 1);
    drop(c);
    drop(journal);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn committed_but_lost_response_is_resolved_from_receipt() {
    let dir = std::env::temp_dir().join(format!("coshell-secret-ambiguous-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let mut db = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    let journal = Arc::new(Ambiguous(SqliteJournal::new(db, &guard).unwrap()));
    let store = Store::default();
    let coordinator = SecretCoordinator::new(journal.clone(), &store, &guard).unwrap();
    let result = coordinator
        .replace_idempotent("op", "c", "password", &Secret::new(b"secret".to_vec()))
        .unwrap();
    assert_eq!(
        coordinator.operation_status("op").unwrap(),
        OperationStatus::Committed(result)
    );
    assert_eq!(*store.writes.lock().unwrap(), 1);
    drop(coordinator);
    drop(journal);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[derive(Default)]
struct Store {
    values: Mutex<HashMap<String, Vec<u8>>>,
    writes: Mutex<usize>,
    fail_delete: Mutex<bool>,
}
impl SecretStore for Store {
    fn set(&self, id: &str, secret: &Secret) -> Result<(), SecretError> {
        *self.writes.lock().unwrap() += 1;
        self.values
            .lock()
            .unwrap()
            .insert(id.into(), secret.expose().into());
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
fn operation_id_survives_restart_and_retry_never_rewrites_secret() {
    let dir = std::env::temp_dir().join(format!("coshell-secret-receipt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
    let store = Store::default();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    let coordinator = SecretCoordinator::new(journal.clone(), &store, &guard).unwrap();
    let first = coordinator
        .replace_idempotent(
            "request-1",
            "c",
            "password",
            &Secret::new(b"original".to_vec()),
        )
        .unwrap();
    assert_eq!(first.outcome, ReplaceOutcome::Committed);
    drop(coordinator);
    drop(journal);
    let db = rusqlite::Connection::open(&path).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    let coordinator = SecretCoordinator::new(journal.clone(), &store, &guard).unwrap();
    assert_eq!(
        coordinator.operation_status("request-1").unwrap(),
        OperationStatus::Committed(first.clone())
    );
    let repeated = coordinator
        .replace_idempotent(
            "request-1",
            "c",
            "password",
            &Secret::new(b"different".to_vec()),
        )
        .unwrap_err();
    assert_eq!(repeated, CoordinatorError::OperationConflict);
    assert_eq!(*store.writes.lock().unwrap(), 1);
    assert_eq!(
        store.get(&first.secret_id).unwrap().unwrap().expose(),
        b"original"
    );
    assert_eq!(
        coordinator
            .replace_idempotent("request-1", "other", "password", &Secret::new(vec![]))
            .unwrap_err(),
        CoordinatorError::OperationConflict
    );
    drop(coordinator);
    drop(journal);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn reopened_journal_rejects_missing_connection_without_writing_secret() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-fk-reopen-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    drop(db);
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.pragma_update(None, "foreign_keys", false).unwrap();
    let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
    let store = Store::default();
    let coordinator = SecretCoordinator::new(journal.clone(), &store, &guard).unwrap();
    assert!(coordinator
        .replace_idempotent(
            "op",
            "missing",
            "password",
            &Secret::new(b"secret".to_vec())
        )
        .is_err());
    assert_eq!(*store.writes.lock().unwrap(), 0);
    drop(coordinator);
    drop(journal);
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn journal_rejects_connection_inside_caller_transaction() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-fk-tx-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute_batch("BEGIN").unwrap();
    let guard = SecretOperationLock::acquire(&dir).unwrap();
    assert!(SqliteJournal::new(db, &guard).is_err());
    drop(guard);
    std::fs::remove_dir_all(dir).unwrap();
}

use coshell_core::secret_compensation::{
    Journal, OperationStatus, SecretCoordinator, SecretOperationLock, WebDavSqliteJournal,
};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Default)]
struct Store(
    Mutex<HashMap<String, Vec<u8>>>,
    std::sync::atomic::AtomicBool,
);
impl SecretStore for Store {
    fn set(&self, id: &str, value: &Secret) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), value.expose().to_vec());
        Ok(())
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.0.lock().unwrap().get(id).cloned().map(Secret::new))
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        if self.1.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SecretError::StorageFailure);
        }
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}

#[test]
fn webdav_replacement_commits_reference_and_recovers_staged_secret() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-webdav-secrets-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let lock = SecretOperationLock::acquire(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username) VALUES ('p','https://example.org','/','u')", []).unwrap();
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).unwrap());
    let store = Store::default();
    let c = SecretCoordinator::new(journal.clone(), &store, &lock).unwrap();
    let first = c
        .replace_idempotent("op1", "p", "webdav_password", &Secret::new(b"one".to_vec()))
        .unwrap();
    store.1.store(true, std::sync::atomic::Ordering::SeqCst);
    let second = c
        .replace_idempotent("op2", "p", "webdav_password", &Secret::new(b"two".to_vec()))
        .unwrap();
    assert_eq!(
        second.outcome,
        coshell_core::secret_compensation::ReplaceOutcome::CommittedCleanupPending
    );
    assert!(store.get(&first.secret_id).unwrap().is_some());
    store.1.store(false, std::sync::atomic::Ordering::SeqCst);
    c.recover().unwrap();
    assert!(store.get(&first.secret_id).unwrap().is_none());
    assert_eq!(
        store.get(&second.secret_id).unwrap().unwrap().expose(),
        b"two"
    );
    let reader = rusqlite::Connection::open(&path).unwrap();
    let current: String = reader
        .query_row(
            "SELECT credential_secret_id FROM webdav_profiles WHERE id='p'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(current, second.secret_id);
    drop(reader);
    journal
        .prepare_receipt("crash", "p", "webdav_password", "staged-id")
        .unwrap();
    store
        .set("staged-id", &Secret::new(b"staged".to_vec()))
        .unwrap();
    drop(c);
    drop(journal);
    let db = rusqlite::Connection::open(&path).unwrap();
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).unwrap());
    let c = SecretCoordinator::new(journal.clone(), &store, &lock).unwrap();
    c.recover().unwrap();
    assert_eq!(
        c.operation_status("crash").unwrap(),
        OperationStatus::Aborted
    );
    assert!(store.get("staged-id").unwrap().is_none());
    drop(c);
    drop(journal);
    drop(lock);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn failed_commit_keeps_staged_receipt_and_restart_recovers_it() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-webdav-failed-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let lock = SecretOperationLock::acquire(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute_batch("INSERT INTO webdav_profiles(id,url,remote_directory,username) VALUES ('p','https://example.org','/','u'); CREATE TRIGGER reject_webdav_commit BEFORE UPDATE OF credential_secret_id ON webdav_profiles BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).unwrap());
    let store = Store::default();
    let coordinator = SecretCoordinator::new(journal.clone(), &store, &lock).unwrap();
    assert_eq!(
        coordinator
            .replace_idempotent(
                "op",
                "p",
                "webdav_password",
                &Secret::new(b"secret".to_vec())
            )
            .unwrap_err(),
        coshell_core::secret_compensation::CoordinatorError::CommitUnknown
    );
    assert_eq!(
        coordinator.operation_status("op").unwrap(),
        OperationStatus::Staged
    );
    let reader = rusqlite::Connection::open(&path).unwrap();
    let staged: String = reader
        .query_row(
            "SELECT secret_id FROM webdav_secret_journal WHERE operation_id='op'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let profile: Option<String> = reader
        .query_row(
            "SELECT credential_secret_id FROM webdav_profiles WHERE id='p'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(profile.is_none());
    assert!(store.get(&staged).unwrap().is_some());
    drop(reader);
    drop(coordinator);
    drop(journal);
    let db = rusqlite::Connection::open(&path).unwrap();
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).unwrap());
    let coordinator = SecretCoordinator::new(journal.clone(), &store, &lock).unwrap();
    coordinator.recover().unwrap();
    assert_eq!(
        coordinator.operation_status("op").unwrap(),
        OperationStatus::Aborted
    );
    assert!(store.get(&staged).unwrap().is_none());
    drop(coordinator);
    drop(journal);
    drop(lock);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn concurrent_webdav_replacements_leave_one_live_reference() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-webdav-concurrent-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let lock = SecretOperationLock::acquire(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username) VALUES ('p','https://example.org','/','u')",[]).unwrap();
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).unwrap());
    let store = Store::default();
    let coordinator = SecretCoordinator::new(journal.clone(), &store, &lock).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let (one, two) = std::thread::scope(|s| {
        let run = |op: &'static str| {
            barrier.wait();
            coordinator
                .replace_idempotent(
                    op,
                    "p",
                    "webdav_password",
                    &Secret::new(op.as_bytes().to_vec()),
                )
                .unwrap()
        };
        let a = s.spawn(move || run("op1"));
        let b = s.spawn(move || run("op2"));
        (a.join().unwrap(), b.join().unwrap())
    });
    let db = rusqlite::Connection::open(&path).unwrap();
    let id: String = db
        .query_row(
            "SELECT credential_secret_id FROM webdav_profiles WHERE id='p'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(id == one.secret_id || id == two.secret_id);
    assert_eq!(store.0.lock().unwrap().len(), 1);
    assert!(store.get(&id).unwrap().is_some());
    drop(db);
    drop(coordinator);
    drop(journal);
    drop(lock);
    std::fs::remove_dir_all(dir).unwrap();
}

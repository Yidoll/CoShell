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
    std::sync::atomic::AtomicBool,
    Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
);
impl SecretStore for Store {
    fn set(&self, id: &str, value: &Secret) -> Result<(), SecretError> {
        if self.2.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(SecretError::StorageFailure);
        }
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), value.expose().to_vec());
        if let Some((staged, proceed)) = self.3.lock().unwrap().take() {
            staged.send(()).unwrap();
            proceed
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
        }
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

#[test]
fn changing_target_keeps_published_config_and_secret_consistent_on_failure_and_retry() {
    use coshell_core::{
        feature_flags::{Feature, FeatureFlags},
        secret_compensation::CoordinatorError,
        webdav_config::{
            get_profile, operation_status, recover_credentials, save_profile, ConfigError,
            CredentialStatus, WebDavProfile,
        },
    };
    let dir = std::env::temp_dir().join(format!(
        "coshell-webdav-target-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let store = Store::default();
    let flags = FeatureFlags::default().with_enabled(Feature::WebDavBackup);
    let old = WebDavProfile {
        id: "p".into(),
        url: "https://old.example/".into(),
        remote_directory: "old".into(),
        username: "alice".into(),
        auto_backup: false,
    };
    let new = WebDavProfile {
        url: "https://new.example/".into(),
        remote_directory: "new".into(),
        username: "bob".into(),
        ..old.clone()
    };
    let old_password = Secret::new(b"old".to_vec());
    let new_password = Secret::new(b"new".to_vec());
    save_profile(&path, &store, &flags, &old, Some(("old", &old_password))).unwrap();
    let active_secret = || -> String {
        db.query_row(
            "SELECT credential_secret_id FROM active_webdav_profiles WHERE id='p'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let old_id = active_secret();
    let assert_old_visible = || {
        assert_eq!(get_profile(&path, &flags, "p").unwrap(), Some(old.clone()));
        assert_eq!(active_secret(), old_id);
        assert_eq!(store.get(&old_id).unwrap().unwrap().expose(), b"old");
    };

    store.2.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(
        save_profile(
            &path,
            &store,
            &flags,
            &new,
            Some(("store-fails", &new_password))
        ),
        Err(ConfigError::Credential(CoordinatorError::SecretStore))
    );
    store.2.store(false, std::sync::atomic::Ordering::SeqCst);
    assert_old_visible();
    assert_eq!(
        operation_status(&path, &store, &flags, "p", "store-fails").unwrap(),
        CredentialStatus::Aborted
    );
    assert_eq!(store.0.lock().unwrap().len(), 1);

    db.execute_batch("CREATE TRIGGER reject_webdav_publish BEFORE UPDATE OF credential_secret_id ON webdav_profiles BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();
    assert_eq!(
        save_profile(
            &path,
            &store,
            &flags,
            &new,
            Some(("commit-fails", &new_password))
        ),
        Err(ConfigError::Credential(CoordinatorError::CommitUnknown))
    );
    assert_old_visible();
    assert_eq!(
        operation_status(&path, &store, &flags, "p", "commit-fails").unwrap(),
        CredentialStatus::Staged
    );
    assert_eq!(store.0.lock().unwrap().len(), 2);
    recover_credentials(&path, &store, &flags).unwrap();
    assert_old_visible();
    assert_eq!(
        operation_status(&path, &store, &flags, "p", "commit-fails").unwrap(),
        CredentialStatus::Aborted
    );
    assert_eq!(store.0.lock().unwrap().len(), 1);
    db.execute_batch("DROP TRIGGER reject_webdav_publish")
        .unwrap();

    store.1.store(true, std::sync::atomic::Ordering::SeqCst);
    let (staged_tx, staged_rx) = std::sync::mpsc::channel();
    let (proceed_tx, proceed_rx) = std::sync::mpsc::channel();
    *store.3.lock().unwrap() = Some((staged_tx, proceed_rx));
    std::thread::scope(|threads| {
        let save = threads.spawn(|| {
            save_profile(
                &path,
                &store,
                &flags,
                &new,
                Some(("committed", &new_password)),
            )
        });
        let staged = staged_rx.recv_timeout(std::time::Duration::from_secs(5));
        assert!(staged.is_ok(), "save must stage before publication");
        // Even with the new password already in the system store, concurrent
        // readers see the complete old profile until the SQL commit.
        let observation = db.query_row(
            "SELECT username,remote_directory,credential_secret_id FROM active_webdav_profiles WHERE id='p'",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?)),
        );
        proceed_tx.send(()).unwrap();
        let _lost_response = save.join().unwrap().unwrap();
        assert_eq!(
            observation.unwrap(),
            (
                old.username.clone(),
                old.remote_directory.clone(),
                old_id.clone()
            )
        );
    });
    assert_eq!(get_profile(&path, &flags, "p").unwrap(), Some(new.clone()));
    let new_id = active_secret();
    assert_ne!(old_id, new_id);
    assert_eq!(store.get(&new_id).unwrap().unwrap().expose(), b"new");
    assert_eq!(
        operation_status(&path, &store, &flags, "p", "committed").unwrap(),
        CredentialStatus::Committed {
            cleanup_pending: true
        }
    );
    assert_eq!(
        save_profile(
            &path,
            &store,
            &flags,
            &old,
            Some(("committed", &old_password))
        ),
        Err(ConfigError::Credential(CoordinatorError::OperationConflict))
    );
    assert_eq!(
        save_profile(
            &path,
            &store,
            &flags,
            &new,
            Some(("committed", &new_password))
        ),
        Err(ConfigError::Credential(CoordinatorError::OperationConflict))
    );
    assert_eq!(active_secret(), new_id);
    assert_eq!(get_profile(&path, &flags, "p").unwrap(), Some(new));
    assert_eq!(store.0.lock().unwrap().len(), 2);
    store.1.store(false, std::sync::atomic::Ordering::SeqCst);
    recover_credentials(&path, &store, &flags).unwrap();
    assert_eq!(
        operation_status(&path, &store, &flags, "p", "committed").unwrap(),
        CredentialStatus::Committed {
            cleanup_pending: false
        }
    );
    assert_eq!(active_secret(), new_id);
    assert!(store.get(&old_id).unwrap().is_none());
    assert_eq!(store.0.lock().unwrap().len(), 1);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

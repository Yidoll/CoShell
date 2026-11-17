use coshell_core::restore_secrets::{self, IncomingSecret, RestoreError};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use coshell_storage::Repository;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};

#[derive(Default)]
struct Store {
    values: Mutex<HashMap<String, Vec<u8>>>,
    fail_delete: AtomicBool,
    fail_set: AtomicBool,
}
impl SecretStore for Store {
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self
            .values
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .map(Secret::new))
    }
    fn set(&self, id: &str, v: &Secret) -> Result<(), SecretError> {
        if self.fail_set.load(Ordering::SeqCst) {
            return Err(SecretError::StorageFailure);
        }
        self.values
            .lock()
            .unwrap()
            .insert(id.into(), v.expose().to_vec());
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(SecretError::StorageFailure);
        }
        self.values.lock().unwrap().remove(id);
        Ok(())
    }
}
fn setup() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "coshell-restore-{}-{}",
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
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES('c','n','h',22,'u','password')",[]).unwrap();
    path
}
#[test]
fn stages_then_atomically_commits_and_recovers_cleanup() {
    let path = setup();
    let store = Store::default();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let expected = Repository::new(&mut db).snapshot().unwrap();
    let mut desired = expected.clone();
    desired.connections[0].notes = "changed".into();
    restore_secrets::apply(
        &path,
        &store,
        "batch1",
        &expected,
        &desired,
        &[IncomingSecret {
            connection_id: "c",
            secret_type: "password",
            value: b"TOP_SECRET_MARKER",
        }],
    )
    .unwrap();
    let after = Repository::new(&mut db).snapshot().unwrap();
    assert_eq!(after.connections[0].notes, "changed");
    assert_eq!(
        store
            .get(&after.secret_refs[0].secret_id)
            .unwrap()
            .unwrap()
            .expose(),
        b"TOP_SECRET_MARKER"
    );
    assert!(!std::fs::read(&path)
        .unwrap()
        .windows(17)
        .any(|w| w == b"TOP_SECRET_MARKER"));
    store.fail_delete.store(true, Ordering::SeqCst);
    assert_eq!(
        restore_secrets::apply(
            &path,
            &store,
            "batch2",
            &after,
            &after,
            &[IncomingSecret {
                connection_id: "c",
                secret_type: "password",
                value: b"new"
            }]
        ),
        Err(RestoreError::RecoveryPending)
    );
    let state = restore_secrets::status(&path, &store, "batch2")
        .unwrap()
        .unwrap();
    assert_eq!(state.state, "committed");
    assert!(state.cleanup_pending);
    let replaced = Repository::new(&mut db).snapshot().unwrap();
    assert_ne!(
        replaced.secret_refs[0].secret_id,
        after.secret_refs[0].secret_id
    );
    store.fail_delete.store(false, Ordering::SeqCst);
    restore_secrets::recover(&path, &store).unwrap();
    assert!(
        !restore_secrets::status(&path, &store, "batch2")
            .unwrap()
            .unwrap()
            .cleanup_pending
    );
    assert!(store
        .get(&after.secret_refs[0].secret_id)
        .unwrap()
        .is_none());
    drop(db);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}
#[test]
fn stale_preview_leaves_staged_secrets_for_recovery_without_modifying_metadata() {
    let path = setup();
    let store = Store::default();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let expected = Repository::new(&mut db).snapshot().unwrap();
    db.execute("UPDATE connections SET notes='new' WHERE id='c'", [])
        .unwrap();
    assert_eq!(
        restore_secrets::apply(
            &path,
            &store,
            "batch2",
            &expected,
            &expected,
            &[IncomingSecret {
                connection_id: "c",
                secret_type: "password",
                value: b"secret"
            }]
        ),
        Err(RestoreError::OutcomeUnknown)
    );
    assert_eq!(store.values.lock().unwrap().len(), 1);
    restore_secrets::recover(&path, &store).unwrap();
    assert!(store.values.lock().unwrap().is_empty());
    assert_eq!(
        Repository::new(&mut db).snapshot().unwrap().connections[0].notes,
        "new"
    );
    drop(db);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn failed_store_write_keeps_metadata_and_recovery_cleans_all_staged_ids() {
    let path = setup();
    let store = Store::default();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let before = Repository::new(&mut db).snapshot().unwrap();
    store.fail_set.store(true, Ordering::SeqCst);
    store.fail_delete.store(true, Ordering::SeqCst);
    assert_eq!(
        restore_secrets::apply(
            &path,
            &store,
            "batch2",
            &before,
            &before,
            &[IncomingSecret {
                connection_id: "c",
                secret_type: "password",
                value: b"secret"
            }]
        ),
        Err(RestoreError::RecoveryPending)
    );
    assert_eq!(Repository::new(&mut db).snapshot().unwrap(), before);
    assert_eq!(
        db.query_row("SELECT count(*) FROM restore_secret_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    store.fail_delete.store(false, Ordering::SeqCst);
    restore_secrets::recover(&path, &store).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM restore_secret_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn restart_recovers_staged_secret_after_interrupted_process() {
    let path = setup();
    let store = Store::default();
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute("INSERT INTO restore_secret_journal VALUES('interrupted','c','password','staged-id','staged')",[]).unwrap();
    }
    store
        .set("staged-id", &Secret::new(b"partial".to_vec()))
        .unwrap();
    restore_secrets::recover(&path, &store).unwrap();
    assert!(store.get("staged-id").unwrap().is_none());
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM restore_secret_journal", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(db);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

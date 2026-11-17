use coshell_core::ssh_key_store;
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use coshell_storage::ConnectionRecord;
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
    fn set(&self, id: &str, value: &Secret) -> Result<(), SecretError> {
        if self.fail_set.load(Ordering::SeqCst) {
            return Err(SecretError::StorageFailure);
        }
        self.values
            .lock()
            .unwrap()
            .insert(id.into(), value.expose().to_vec());
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
        if self.fail_delete.load(Ordering::SeqCst) {
            return Err(SecretError::StorageFailure);
        }
        self.values.lock().unwrap().remove(id);
        Ok(())
    }
}
fn setup() -> (std::path::PathBuf, ConnectionRecord) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "coshell-key-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let record = ConnectionRecord {
        id: "c".into(),
        name: "server".into(),
        host: "host".into(),
        port: 22,
        username: "user".into(),
        authentication: "private_key".into(),
        group_id: None,
        notes: String::new(),
        production: false,
        always_confirm: false,
        version: 0,
    };
    coshell_storage::Repository::new(&mut db)
        .save_connection(&record)
        .unwrap();
    (path, record)
}

#[test]
fn saves_to_secret_store_binds_target_and_never_persists_plaintext() {
    let (path, mut record) = setup();
    let store = Store::default();
    ssh_key_store::save(
        &path,
        &store,
        &record,
        "private-key-sentinel",
        "phrase-sentinel",
        "op1",
    )
    .unwrap();
    let (key, phrase) = ssh_key_store::load(&path, &store, &record)
        .unwrap()
        .unwrap();
    assert_eq!(key.expose(), b"private-key-sentinel");
    assert_eq!(phrase.unwrap().expose(), b"phrase-sentinel");
    let bytes = std::fs::read(&path).unwrap();
    assert!(!bytes.windows(20).any(|v| v == b"private-key-sentinel"));
    assert!(!bytes.windows(15).any(|v| v == b"phrase-sentinel"));
    record.host = "other".into();
    assert!(ssh_key_store::exists(&path, &store, &record).unwrap());
    assert!(ssh_key_store::load(&path, &store, &record).is_err());
    drop(key);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn failed_cleanup_is_recovered_after_restart() {
    let (path, record) = setup();
    let store = Store::default();
    ssh_key_store::save(&path, &store, &record, "key", "", "op1").unwrap();
    store.fail_delete.store(true, Ordering::SeqCst);
    assert!(ssh_key_store::delete(&path, &store, "c", "op2").is_err());
    assert_eq!(store.values.lock().unwrap().len(), 1);
    store.fail_delete.store(false, Ordering::SeqCst);
    assert!(!ssh_key_store::exists(&path, &store, &record).unwrap());
    assert!(store.values.lock().unwrap().is_empty());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn failed_replacement_preserves_existing_key_without_orphan() {
    let (path, record) = setup();
    let store = Store::default();
    ssh_key_store::save(&path, &store, &record, "old-key", "old-phrase", "op1").unwrap();
    assert_eq!(
        ssh_key_store::operation_status(&path, &store, "c", "op1").unwrap(),
        "committed"
    );
    assert!(ssh_key_store::operation_status(&path, &store, "other", "op1").is_err());
    store.fail_set.store(true, Ordering::SeqCst);
    assert!(ssh_key_store::save(&path, &store, &record, "new-key", "new-phrase", "op2").is_err());
    store.fail_set.store(false, Ordering::SeqCst);
    let (key, phrase) = ssh_key_store::load(&path, &store, &record)
        .unwrap()
        .unwrap();
    assert_eq!(key.expose(), b"old-key");
    assert_eq!(phrase.unwrap().expose(), b"old-phrase");
    assert_eq!(store.values.lock().unwrap().len(), 1);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn restore_replaces_existing_key_saved_by_real_coordinator() {
    let (path, record) = setup();
    let store = Store::default();
    ssh_key_store::save(&path, &store, &record, "old-key", "old-phrase", "ssh-op").unwrap();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let expected = coshell_storage::Repository::new(&mut db)
        .snapshot()
        .unwrap();
    let old_id = expected.secret_refs[0].secret_id.clone();
    let bundle =
        br#"{"host":"host","port":22,"username":"user","key":"new-key","passphrase":"new-phrase"}"#;
    coshell_core::restore_secrets::apply(
        &path,
        &store,
        "restore-op",
        &expected,
        &expected,
        &[coshell_core::restore_secrets::IncomingSecret {
            connection_id: "c",
            secret_type: "ssh_private_key_bundle_v1",
            value: bundle,
        }],
    )
    .unwrap();
    let (key, passphrase) = ssh_key_store::load(&path, &store, &record)
        .unwrap()
        .unwrap();
    assert_eq!(key.expose(), b"new-key");
    assert_eq!(passphrase.unwrap().expose(), b"new-phrase");
    assert!(store.get(&old_id).unwrap().is_none());
    drop(db);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn busy_secret_lock_returns_without_waiting() {
    let (path, record) = setup();
    let store = Store::default();
    let held =
        coshell_core::secret_compensation::SecretOperationLock::acquire(path.parent().unwrap())
            .unwrap();
    let start = std::time::Instant::now();
    let result = ssh_key_store::exists(&path, &store, &record);
    assert!(result.is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    drop(held);
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn startup_recovery_removes_staged_secret_and_fails_closed_when_store_fails() {
    use coshell_core::secret_compensation::{Journal, SecretOperationLock, SqliteJournal};
    let (path, _record) = setup();
    let store = Store::default();
    {
        let lock = SecretOperationLock::acquire(path.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        let journal = SqliteJournal::new(db, &lock).unwrap();
        journal
            .prepare_receipt("staged", "c", "ssh_private_key_bundle_v1", "orphan")
            .unwrap();
        store
            .set("orphan", &Secret::new(b"private".to_vec()))
            .unwrap();
    }
    store.fail_delete.store(true, Ordering::SeqCst);
    assert!(ssh_key_store::recover_all(&path, &store).is_err());
    assert!(store.get("orphan").unwrap().is_some());
    store.fail_delete.store(false, Ordering::SeqCst);
    ssh_key_store::recover_all(&path, &store).unwrap();
    assert!(store.get("orphan").unwrap().is_none());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

#[test]
fn delete_commit_failure_is_reported_as_unknown_and_can_be_checked() {
    let (path, record) = setup();
    let store = Store::default();
    ssh_key_store::save(&path, &store, &record, "key", "", "op1").unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE test_parent(id INTEGER PRIMARY KEY); CREATE TABLE test_child(id INTEGER REFERENCES test_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TRIGGER test_deferred_delete AFTER DELETE ON connection_secret_refs BEGIN INSERT INTO test_child VALUES(42); END;").unwrap();
    drop(db);
    let err = ssh_key_store::delete(&path, &store, "c", "op2").unwrap_err();
    assert_eq!(err, "凭据删除结果未知，请查询已保存状态后重试");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("DROP TRIGGER test_deferred_delete;")
        .unwrap();
    drop(db);
    assert!(ssh_key_store::exists(&path, &store, &record).unwrap());
    ssh_key_store::delete(&path, &store, "c", "op3").unwrap();
    assert!(!ssh_key_store::exists(&path, &store, &record).unwrap());
    std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
}

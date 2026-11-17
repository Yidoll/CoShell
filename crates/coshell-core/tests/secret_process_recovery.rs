#![cfg(windows)]

use coshell_core::secret_compensation::{
    Journal, OperationStatus, SecretCoordinator, SecretOperationLock, SqliteJournal,
};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use std::{
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// Test-only store: persists a marker, not secret bytes, to simulate a surviving OS entry.
struct MarkerStore(PathBuf);
impl SecretStore for MarkerStore {
    fn set(&self, id: &str, _: &Secret) -> Result<(), SecretError> {
        std::fs::write(self.0.join(id), b"test-marker").map_err(|_| SecretError::StorageFailure)
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        match std::fs::read(self.0.join(id)) {
            Ok(v) => Ok(Some(Secret::new(v))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(SecretError::StorageFailure),
        }
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        match std::fs::remove_file(self.0.join(id)) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(SecretError::StorageFailure),
        }
    }
}

#[test]
fn child_fixture() {
    let Ok(dir) = std::env::var("COSHELL_PROCESS_TEST_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    match std::env::var("COSHELL_PROCESS_TEST_MODE").unwrap().as_str() {
        "hold" => {
            let guard = SecretOperationLock::acquire(&dir).unwrap();
            let mut db = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
            coshell_storage::migrate(&mut db).unwrap();
            db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES ('c','name','host','user','password')",[]).unwrap();
            let journal = SqliteJournal::new(db, &guard).unwrap();
            journal
                .prepare_receipt("op", "c", "password", "staged-id")
                .unwrap();
            MarkerStore(dir.clone())
                .set("staged-id", &Secret::new(Vec::new()))
                .unwrap();
            std::fs::write(dir.join("ready"), b"ready").unwrap();
            loop {
                std::thread::sleep(Duration::from_secs(1));
            }
        }
        "recover" => {
            let guard = SecretOperationLock::try_acquire(&dir).unwrap();
            let db = rusqlite::Connection::open(dir.join("db.sqlite")).unwrap();
            let journal = Arc::new(SqliteJournal::new(db, &guard).unwrap());
            let store = MarkerStore(dir.clone());
            let coordinator = SecretCoordinator::new(journal, &store, &guard).unwrap();
            assert_eq!(
                coordinator.operation_status("op").unwrap(),
                OperationStatus::Staged
            );
            coordinator.recover().unwrap();
            assert_eq!(
                coordinator.operation_status("op").unwrap(),
                OperationStatus::Aborted
            );
            assert!(store.get("staged-id").unwrap().is_none());
        }
        _ => panic!("unexpected fixture mode"),
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn killed_process_releases_lock_and_next_process_recovers_staged_receipt() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-crash-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut first = ChildGuard(
        Command::new(&exe)
            .args(["--exact", "child_fixture", "--nocapture"])
            .env("COSHELL_PROCESS_TEST_DIR", &dir)
            .env("COSHELL_PROCESS_TEST_MODE", "hold")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    while !dir.join("ready").exists() {
        assert!(
            first.0.try_wait().unwrap().is_none(),
            "holder exited before ready"
        );
        assert!(Instant::now() < deadline, "holder did not become ready");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        SecretOperationLock::try_acquire(&dir).is_err(),
        "second process must not acquire held lock"
    );
    first.0.kill().unwrap();
    assert!(!first.0.wait().unwrap().success());
    let status = Command::new(&exe)
        .args(["--exact", "child_fixture", "--nocapture"])
        .env("COSHELL_PROCESS_TEST_DIR", &dir)
        .env("COSHELL_PROCESS_TEST_MODE", "recover")
        .status()
        .unwrap();
    assert!(
        status.success(),
        "fresh process must recover the staged receipt"
    );
    assert!(!dir.join("staged-id").exists());
    drop(first);
    std::fs::remove_dir_all(dir).unwrap();
}

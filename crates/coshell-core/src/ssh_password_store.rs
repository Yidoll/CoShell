//! Password references use the existing backup format and credential journal.
use crate::secret_compensation::{
    CoordinatorError, OperationStatus, SecretCoordinator, SqliteJournal,
};
use coshell_platform::secret_store::{Secret, SecretStore};
use coshell_storage::ConnectionRecord;
use rusqlite::{params, OptionalExtension, Transaction};
use sha2::{Digest, Sha256};
use std::{path::Path, sync::Arc};

const KIND: &str = "password";
pub(crate) const UNAVAILABLE: &str = "SSH_PASSWORD_UNAVAILABLE";

pub(crate) fn valid_password(bytes: &[u8]) -> bool {
    !bytes.is_empty() && std::str::from_utf8(bytes).is_ok()
}

pub(crate) fn load(
    path: &Path,
    store: &dyn SecretStore,
    connection: &str,
) -> Result<Option<Secret>, &'static str> {
    let (_lock, db) = crate::ssh_key_store::context(path, store).map_err(|_| UNAVAILABLE)?;
    let reference: Option<String> = db.query_row(
        "SELECT secret_id FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2",
        params![connection, KIND], |r| r.get(0),
    ).optional().map_err(|_| UNAVAILABLE)?;
    let Some(reference) = reference else {
        return Ok(None);
    };
    match store.get(&reference) {
        Ok(Some(secret)) if valid_password(secret.expose()) => Ok(Some(secret)),
        _ => Err(UNAVAILABLE),
    }
}

pub(crate) fn save(
    path: &Path,
    store: &dyn SecretStore,
    connection: &str,
    operation: &str,
    secret: &Secret,
) -> Result<(), &'static str> {
    use crate::secret_compensation::Journal;
    let (lock, db) = crate::ssh_key_store::context(path, store).map_err(|_| UNAVAILABLE)?;
    let journal = Arc::new(SqliteJournal::new(db, &lock).map_err(|_| UNAVAILABLE)?);
    let coordinator =
        SecretCoordinator::new(journal.clone(), store, &lock).map_err(|_| UNAVAILABLE)?;
    coordinator
        .recover()
        .map_err(|_| "SSH_PASSWORD_SAVE_FAILED")?;
    let result = coordinator.replace_idempotent(operation, connection, KIND, secret);
    match result {
        Ok(_) => Ok(()),
        Err(CoordinatorError::CommitUnknown) => match journal.receipt(operation) {
            Ok(Some(r))
                if r.connection_id == connection
                    && r.secret_type == KIND
                    && matches!(r.status, OperationStatus::Committed(_)) =>
            {
                Ok(())
            }
            Ok(Some(r))
                if r.connection_id == connection
                    && r.secret_type == KIND
                    && matches!(r.status, OperationStatus::Staged) =>
            {
                let _ = coordinator.recover();
                Err("SSH_PASSWORD_SAVE_FAILED")
            }
            _ => Err("SSH_PASSWORD_SAVE_UNKNOWN"),
        },
        Err(_) => Err("SSH_PASSWORD_SAVE_FAILED"),
    }
}

pub(crate) fn decision_key(record: &ConnectionRecord) -> String {
    let mut hash = Sha256::new();
    for field in [
        &record.id,
        &record.host,
        &record.username,
        &record.authentication,
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    hash.update(record.port.to_be_bytes());
    format!("ssh.password.declined.{:x}", hash.finalize())
}

pub(crate) fn declined(path: &Path, record: &ConnectionRecord) -> rusqlite::Result<bool> {
    let mut db = rusqlite::Connection::open(path)?;
    Ok(coshell_storage::Repository::new(&mut db)
        .get_setting(&decision_key(record))?
        .is_some())
}

pub(crate) fn decline(path: &Path, record: &ConnectionRecord) -> rusqlite::Result<()> {
    let mut db = rusqlite::Connection::open(path)?;
    coshell_storage::Repository::new(&mut db).set_setting(&decision_key(record), "1")
}

/// Remove the old binding atomically with the connection edit. The shared
/// journal keeps platform cleanup recoverable until it finishes.
pub(crate) fn retire(tx: &Transaction<'_>, connection: &str) -> rusqlite::Result<()> {
    let reference: Option<String> = tx.query_row(
        "SELECT secret_id FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2",
        params![connection, KIND], |r| r.get(0),
    ).optional()?;
    if let Some(reference) = reference {
        let batch: String = tx.query_row(
            "SELECT 'password-retire-' || lower(hex(randomblob(32)))",
            [],
            |r| r.get(0),
        )?;
        tx.execute(
            "INSERT INTO restore_secret_receipts(batch_id,state) VALUES(?1,'committed')",
            [&batch],
        )?;
        tx.execute("INSERT INTO restore_secret_journal(batch_id,connection_id,secret_type,secret_id,kind) VALUES(?1,?2,?3,?4,'cleanup')", params![batch, connection, KIND, reference])?;
        tx.execute(
            "DELETE FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2",
            params![connection, KIND],
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use coshell_platform::secret_store::SecretError;

    struct UnwritableStore;
    impl SecretStore for UnwritableStore {
        fn get(&self, _: &str) -> Result<Option<Secret>, SecretError> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &Secret) -> Result<(), SecretError> {
            Err(SecretError::AccessDenied)
        }
        fn delete(&self, _: &str) -> Result<(), SecretError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct TestStore(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);
    impl SecretStore for TestStore {
        fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
            Ok(self.0.lock().unwrap().get(id).cloned().map(Secret::new))
        }
        fn set(&self, id: &str, secret: &Secret) -> Result<(), SecretError> {
            self.0
                .lock()
                .unwrap()
                .insert(id.into(), secret.expose().to_vec());
            Ok(())
        }
        fn delete(&self, id: &str) -> Result<(), SecretError> {
            self.0.lock().unwrap().remove(id);
            Ok(())
        }
    }

    #[test]
    fn interrupted_commit_can_be_retried_without_restarting() {
        let mut bytes = [0; 16];
        getrandom::getrandom(&mut bytes).unwrap();
        let dir = std::env::temp_dir().join(format!(
            "coshell-password-commit-{}",
            bytes.iter().map(|x| format!("{x:02x}")).collect::<String>()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let mut db = rusqlite::Connection::open(&path).unwrap();
        coshell_storage::migrate(&mut db).unwrap();
        db.execute_batch("INSERT INTO connections(id,name,host,username,authentication) VALUES('c','server','host','user','password');
            CREATE TRIGGER fail_password_commit BEFORE INSERT ON connection_secret_refs
            BEGIN SELECT RAISE(ABORT, 'fixture commit failure'); END;").unwrap();
        let store = TestStore::default();
        assert!(save(
            &path,
            &store,
            "c",
            "interrupted",
            &Secret::new(b"first".to_vec())
        )
        .is_err());
        assert!(load(&path, &store, "c").unwrap().is_none());
        db.execute_batch("DROP TRIGGER fail_password_commit")
            .unwrap();
        save(
            &path,
            &store,
            "c",
            "retry",
            &Secret::new(b"second".to_vec()),
        )
        .unwrap();
        assert_eq!(
            load(&path, &store, "c").unwrap().unwrap().expose(),
            b"second"
        );
        assert_eq!(store.0.lock().unwrap().len(), 1);
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn os_write_failure_leaves_no_active_password_reference() {
        let dir = std::env::temp_dir().join(format!(
            "coshell-password-save-failure-{}-{}",
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
        db.execute("INSERT INTO connections(id,name,host,username,authentication) VALUES('c','server','host','user','password')", []).unwrap();
        let password = Secret::new(b"fixture-password".to_vec());
        assert_eq!(
            save(&path, &UnwritableStore, "c", "failed-write", &password),
            Err("SSH_PASSWORD_SAVE_FAILED")
        );
        assert!(load(&path, &UnwritableStore, "c").unwrap().is_none());
        assert!(coshell_storage::Repository::new(&mut db)
            .get_secret_ref("c", KIND)
            .unwrap()
            .is_none());
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

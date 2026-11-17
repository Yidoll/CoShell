use coshell_core::feature_flags::{Feature, FeatureFlags};
use coshell_core::webdav_config::{
    get_profile, operation_status, recover_credentials, save_profile, ConfigError,
    CredentialStatus, WebDavProfile,
};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use std::{collections::HashMap, sync::Mutex};

#[derive(Default)]
struct Store(Mutex<HashMap<String, Vec<u8>>>);
struct FailingStore;
impl SecretStore for FailingStore {
    fn set(&self, _: &str, _: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }
    fn get(&self, _: &str) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::Unavailable)
    }
    fn delete(&self, _: &str) -> Result<(), SecretError> {
        Ok(())
    }
}
impl SecretStore for Store {
    fn set(&self, id: &str, secret: &Secret) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), secret.expose().to_vec());
        Ok(())
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.0.lock().unwrap().get(id).cloned().map(Secret::new))
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}

#[test]
fn profile_validation_and_credential_storage_are_safe() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-webdav-config-{}-{}",
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
    drop(db);
    let flags = FeatureFlags::default().with_enabled(Feature::WebDavBackup);
    let store = Store::default();
    let mut profile = WebDavProfile {
        id: "profile".into(),
        url: "http://example.org/".into(),
        remote_directory: "backups".into(),
        username: "alice".into(),
        auto_backup: false,
    };
    let password = Secret::new(b"secret-sentinel".to_vec());
    assert_eq!(
        save_profile(&path, &store, &FeatureFlags::default(), &profile, None),
        Err(ConfigError::Unsupported)
    );
    assert_eq!(
        save_profile(&path, &store, &flags, &profile, Some(("one", &password))),
        Err(ConfigError::InvalidInput)
    );
    assert!(store.0.lock().unwrap().is_empty());
    profile.url = "https://user:secret-sentinel@example.org/".into();
    assert_eq!(
        save_profile(&path, &store, &flags, &profile, None),
        Err(ConfigError::InvalidInput)
    );
    profile.url = "https://example.org/".into();
    for bad in [
        "%2e%2e",
        "%252e%252e",
        "//server/share",
        "a\\b",
        "a?token=x",
        "a#fragment",
        "a/../b",
        "a//b",
    ] {
        profile.remote_directory = bad.into();
        assert_eq!(
            save_profile(&path, &store, &flags, &profile, None),
            Err(ConfigError::InvalidInput),
            "{bad}"
        );
    }
    profile.remote_directory = "backups".into();
    assert!(save_profile(
        &path,
        &FailingStore,
        &flags,
        &profile,
        Some(("failed", &password))
    )
    .is_err());
    let db = rusqlite::Connection::open(&path).unwrap();
    let rows: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM webdav_profiles WHERE id='profile'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    drop(db);
    let saved = save_profile(&path, &store, &flags, &profile, Some(("one", &password))).unwrap();
    assert_eq!(
        get_profile(&path, &flags, "profile").unwrap(),
        Some(profile.clone())
    );
    assert_eq!(
        operation_status(&path, &store, &flags, "profile", "one").unwrap(),
        CredentialStatus::Committed {
            cleanup_pending: false
        }
    );
    assert_eq!(
        operation_status(&path, &store, &flags, "profile", "missing").unwrap(),
        CredentialStatus::NotFound
    );
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut db)
        .prepare_new_profile(
            "crashed",
            "pending",
            "https://example.org/",
            "backups",
            "alice",
            false,
            "pending-secret",
        )
        .unwrap();
    drop(db);
    store
        .set("pending-secret", &Secret::new(b"temporary".to_vec()))
        .unwrap();
    assert_eq!(
        operation_status(&path, &store, &flags, "pending", "crashed").unwrap(),
        CredentialStatus::Staged
    );
    assert_eq!(
        operation_status(&path, &store, &flags, "other", "one"),
        Err(ConfigError::Credential(
            coshell_core::secret_compensation::CoordinatorError::OperationConflict
        ))
    );
    assert!(get_profile(&path, &flags, "pending").unwrap().is_none());
    let db = rusqlite::Connection::open(&path).unwrap();
    assert!(db.execute("INSERT INTO backup_runs(id,profile_id,snapshot_id,state,started_at) VALUES ('run','pending','snap','queued','now')",[]).is_err());
    drop(db);
    recover_credentials(&path, &store, &flags).unwrap();
    assert_eq!(
        operation_status(&path, &store, &flags, "pending", "crashed").unwrap(),
        CredentialStatus::Aborted
    );
    assert!(store.get("pending-secret").unwrap().is_none());
    let mut legacy_db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut legacy_db).unwrap();
    coshell_storage::webdav_secret_receipt::WebDavSecretReceiptRepository(&mut legacy_db)
        .prepare_new_profile(
            "legacy-op",
            "legacy",
            "https://example.org/",
            "backups",
            "alice",
            false,
            "legacy-secret",
        )
        .unwrap();
    // Recreate the actual v5 schema, including removal of triggers attached to
    // older tables by later migrations. Dropping only the v8 tables leaves
    // those cross-table triggers behind and makes v8 migration fail.
    legacy_db.execute_batch("DROP TRIGGER restore_ref_pending_insert; DROP TRIGGER restore_ref_pending_update; DROP TRIGGER restore_webdav_pending_insert; DROP TRIGGER restore_webdav_pending_update; DROP TRIGGER restore_ssh_journal_cross; DROP TRIGGER restore_webdav_journal_cross; DROP TABLE restore_secret_journal; DROP TABLE restore_secret_receipts; DROP TRIGGER backup_run_prune_aborted_profile; DROP TRIGGER backup_runs_provisional_insert; DROP TRIGGER backup_runs_provisional_update; DROP VIEW active_webdav_profiles; DELETE FROM schema_migrations WHERE version>=6; PRAGMA user_version=5; INSERT INTO backup_runs(id,profile_id,snapshot_id,state,started_at) VALUES ('legacy-run','legacy','snap','failed','now');").unwrap();
    drop(legacy_db);
    store
        .set("legacy-secret", &Secret::new(b"legacy".to_vec()))
        .unwrap();
    recover_credentials(&path, &store, &flags).unwrap();
    assert_eq!(
        operation_status(&path, &store, &flags, "legacy", "legacy-op").unwrap(),
        CredentialStatus::AbortedRetainedRuns
    );
    assert!(get_profile(&path, &flags, "legacy").unwrap().is_none());
    assert!(store.get("legacy-secret").unwrap().is_none());
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("DELETE FROM backup_runs WHERE id='legacy-run'", [])
        .unwrap();
    drop(db);
    assert_eq!(
        operation_status(&path, &store, &flags, "legacy", "legacy-op").unwrap(),
        CredentialStatus::Aborted
    );
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM webdav_profiles WHERE id='pending'",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    drop(db);
    assert_eq!(saved.profile, profile);
    let mut duplicate = profile.clone();
    duplicate.auto_backup = true;
    assert!(save_profile(&path, &store, &flags, &duplicate, Some(("one", &password))).is_err());
    assert!(!format!("{saved:?}").contains("secret-sentinel"));
    let db = rusqlite::Connection::open(&path).unwrap();
    let (url, id): (String, String) = db
        .query_row(
            "SELECT url,credential_secret_id FROM webdav_profiles WHERE id='profile'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(!url.contains("secret-sentinel"));
    let auto: bool = db
        .query_row(
            "SELECT auto_backup FROM webdav_profiles WHERE id='profile'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(!auto);
    assert!(!id.contains("secret-sentinel"));
    assert_eq!(
        store.get(&id).unwrap().unwrap().expose(),
        b"secret-sentinel"
    );
    drop(db);
    profile.url = "https://other.example/".into();
    assert_eq!(
        save_profile(&path, &store, &flags, &profile, None),
        Err(ConfigError::CredentialTargetChanged)
    );
    drop(store);
    std::fs::remove_dir_all(dir).unwrap();
}

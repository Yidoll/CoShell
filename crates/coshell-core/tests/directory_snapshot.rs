use coshell_backup::snapshot_manifest::{parse_manifest, SnapshotMetadata, SnapshotProtection};
use coshell_core::{
    backup_inspection::inspect_directory_for_restore,
    backup_snapshot::{create_directory_snapshot, BackupOptions},
    feature_flags::{Feature, FeatureFlags},
};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
struct Store;
#[derive(Default)]
struct MemoryStore(std::sync::Mutex<std::collections::HashMap<String, Vec<u8>>>);
impl SecretStore for MemoryStore {
    fn set(&self, id: &str, value: &Secret) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(id.to_owned(), value.expose().to_vec());
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
impl SecretStore for Store {
    fn set(&self, _: &str, _: &Secret) -> Result<(), SecretError> {
        unreachable!()
    }
    fn delete(&self, _: &str) -> Result<(), SecretError> {
        unreachable!()
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        assert_eq!(id, "credential-id");
        Ok(Some(Secret::new(b"saved-password".to_vec())))
    }
}
#[test]
fn independent_directory_roundtrips_existing_restore_validation_and_excludes_private_context() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-directory-{}-{}",
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
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication,production,always_confirm) VALUES('c','server','example.org',22,'user','password',1,1)",[]).unwrap();
    db.execute("INSERT INTO connection_secret_refs(connection_id,secret_type,secret_id) VALUES('c','password','credential-id')",[]).unwrap();
    db.execute("INSERT INTO command_history(id,connection_id,connection_name,production,command,submitted_at,outcome) VALUES('h','c','server',1,'excluded-history','2026-01-01T00:00:00Z','unknown')",[]).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES('dav','https://example.org','/','user','excluded-webdav-secret')",[]).unwrap();
    drop(db);
    let flags = FeatureFlags::default().with_enabled(Feature::WebDavBackup);
    for password in [b"".as_slice(), b"backup-password".as_slice()] {
        let artifact = create_directory_snapshot(
            &path,
            &Store,
            password,
            BackupOptions {
                include_secrets: true,
            },
            &flags,
            SnapshotMetadata {
                snapshot_id: "snapshot",
                created_at: "2026-11-16T03:18:18Z",
                app_version: "0.1.0",
            },
        )
        .unwrap();
        let manifest = parse_manifest(&artifact.manifest).unwrap();
        assert!(manifest.includes_credentials);
        assert_eq!(manifest.connection_count, 1);
        assert_eq!(
            manifest.protection,
            if password.is_empty() {
                SnapshotProtection::Plaintext
            } else {
                SnapshotProtection::Encrypted
            }
        );
        let (inspection, secrets) = inspect_directory_for_restore(
            &artifact.manifest,
            &artifact.complete,
            &artifact.connections_package,
            artifact.credentials_package.as_ref().map(|v| v.as_slice()),
            password,
        )
        .unwrap();
        assert_eq!(inspection.connections[0].name, "server");
        assert!(inspection.connections[0].production);
        assert!(inspection.connections[0].always_confirm);
        assert_eq!(secrets[0].value(), b"saved-password");
        assert_eq!(secrets[0].connection_id, "c");
        for bytes in [
            &artifact.manifest,
            &artifact.complete,
            &*artifact.connections_package,
            &**artifact.credentials_package.as_ref().unwrap(),
        ] {
            let text = String::from_utf8_lossy(bytes);
            assert!(!text.contains("excluded-history"));
            assert!(!text.contains("excluded-webdav-secret"));
            assert!(!text.contains("credential-id"));
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn saved_private_key_and_phrase_roundtrip_without_manifest_exposure() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-directory-key-{}-{}",
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
    let record = coshell_storage::ConnectionRecord {
        id: "key-server".into(),
        name: "key-server".into(),
        host: "host".into(),
        port: 22,
        username: "user".into(),
        authentication: "private_key".into(),
        group_id: None,
        notes: String::new(),
        production: true,
        always_confirm: true,
        version: 0,
    };
    coshell_storage::Repository::new(&mut db)
        .save_connection(&record)
        .unwrap();
    drop(db);
    let store = MemoryStore::default();
    coshell_core::ssh_key_store::save(
        &path,
        &store,
        &record,
        "private-key-sentinel",
        "passphrase-sentinel",
        "key-save",
    )
    .unwrap();
    for password in [b"".as_slice(), b"password".as_slice()] {
        let artifact = create_directory_snapshot(
            &path,
            &store,
            password,
            BackupOptions {
                include_secrets: true,
            },
            &FeatureFlags::default().with_enabled(Feature::WebDavBackup),
            SnapshotMetadata {
                snapshot_id: "private-key",
                created_at: "2026-11-16T03:18:18Z",
                app_version: "0.1.0",
            },
        )
        .unwrap();
        let (preview, secrets) = inspect_directory_for_restore(
            &artifact.manifest,
            &artifact.complete,
            &artifact.connections_package,
            artifact.credentials_package.as_ref().map(|v| v.as_slice()),
            password,
        )
        .unwrap();
        assert_eq!(preview.connections[0].authentication, "private_key");
        assert_eq!(secrets[0].secret_type, "ssh_private_key_bundle_v1");
        let bundle: serde_json::Value = serde_json::from_slice(secrets[0].value()).unwrap();
        assert_eq!(bundle["key"], "private-key-sentinel");
        assert_eq!(bundle["passphrase"], "passphrase-sentinel");
        let manifest = String::from_utf8_lossy(&artifact.manifest);
        assert!(!manifest.contains("private-key-sentinel"));
        assert!(!manifest.contains("passphrase-sentinel"));
    }
    std::fs::remove_dir_all(dir).unwrap();
}

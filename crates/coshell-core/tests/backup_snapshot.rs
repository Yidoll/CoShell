use coshell_core::backup_snapshot::{create_backup, BackupOptions, SnapshotError};
use coshell_core::feature_flags::{Feature, FeatureFlags};
use coshell_core::session::SessionSupervisor;
use coshell_drivers::ssh::{SshFuture, TerminalChannel, TerminalSize};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use coshell_storage::{ConnectionRecord, GroupRecord, Repository};

struct Store;
struct ActiveChannel;
impl TerminalChannel for ActiveChannel {
    fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { std::future::pending().await })
    }
    fn write<'a>(&'a self, _: &'a [u8]) -> SshFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn resize<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close<'a>(&'a self) -> SshFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
impl SecretStore for Store {
    fn set(&self, _: &str, _: &Secret) -> Result<(), SecretError> {
        unreachable!()
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        assert_eq!(id, "connection-secret-id");
        Ok(Some(Secret::new(b"connection-secret".to_vec())))
    }
    fn delete(&self, _: &str) -> Result<(), SecretError> {
        unreachable!()
    }
}

struct CountingStore {
    reads: std::sync::atomic::AtomicUsize,
    fail: bool,
}
impl SecretStore for CountingStore {
    fn set(&self, _: &str, _: &Secret) -> Result<(), SecretError> {
        unreachable!()
    }
    fn get(&self, _: &str) -> Result<Option<Secret>, SecretError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail {
            Err(SecretError::Unavailable)
        } else {
            Ok(None)
        }
    }
    fn delete(&self, _: &str) -> Result<(), SecretError> {
        unreachable!()
    }
}

#[test]
fn backup_never_reads_unselected_or_webdav_secrets_and_fails_closed() {
    use std::sync::atomic::Ordering;
    let dir = std::env::temp_dir().join(format!(
        "coshell-backup-errors-{}-{}",
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
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES ('c','host','localhost',22,'u','password')", []).unwrap();
    db.execute("INSERT INTO connection_secret_refs(connection_id,secret_type,secret_id) VALUES ('c','password','shared-id')", []).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('w','https://example.org','/','u','shared-id')", []).unwrap();
    let flags = FeatureFlags::default().with_enabled(Feature::WebDavBackup);
    let store = CountingStore {
        reads: std::sync::atomic::AtomicUsize::new(0),
        fail: false,
    };
    assert!(create_backup(&path, &store, b"password", BackupOptions::default(), &flags).is_ok());
    let plain = create_backup(&path, &store, b"", BackupOptions::default(), &flags).unwrap();
    assert!(String::from_utf8_lossy(&plain).contains("\"protection\":\"none\""));
    assert_eq!(store.reads.load(Ordering::SeqCst), 0);
    assert_eq!(
        create_backup(
            &path,
            &store,
            b"password",
            BackupOptions {
                include_secrets: true
            },
            &flags
        ),
        Err(SnapshotError::InvalidInput)
    );
    assert_eq!(store.reads.load(Ordering::SeqCst), 0);
    db.execute(
        "UPDATE webdav_profiles SET credential_secret_id='other-id'",
        [],
    )
    .unwrap();
    assert_eq!(
        create_backup(
            &path,
            &store,
            b"password",
            BackupOptions {
                include_secrets: true
            },
            &flags
        ),
        Err(SnapshotError::SecretUnavailable)
    );
    assert_eq!(store.reads.load(Ordering::SeqCst), 1);
    let failing = CountingStore {
        reads: std::sync::atomic::AtomicUsize::new(0),
        fail: true,
    };
    assert_eq!(
        create_backup(
            &path,
            &failing,
            b"password",
            BackupOptions {
                include_secrets: true
            },
            &flags
        ),
        Err(SnapshotError::SecretUnavailable)
    );
    assert_eq!(failing.reads.load(Ordering::SeqCst), 1);
    drop(db);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn disabled_backup_does_not_access_storage_or_secret_store() {
    let path = std::path::Path::new("missing/backup.sqlite");
    assert_eq!(
        create_backup(
            path,
            &Store,
            b"password",
            BackupOptions::default(),
            &FeatureFlags::default()
        ),
        Err(SnapshotError::Unsupported)
    );
}

#[tokio::test]
async fn encrypted_snapshot_includes_only_selected_connection_data() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-backup-test-{}-{}",
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
    let mut repo = Repository::new(&mut db);
    repo.save_group(&GroupRecord {
        id: "g".into(),
        name: "group".into(),
        parent_id: None,
    })
    .unwrap();
    repo.save_connection(&ConnectionRecord {
        id: "c".into(),
        name: "host".into(),
        host: "example.org".into(),
        port: 22,
        username: "user".into(),
        authentication: "password".into(),
        group_id: Some("g".into()),
        notes: "".into(),
        production: true,
        always_confirm: false,
        version: 4,
    })
    .unwrap();
    repo.prepare_secret_write("operation", "c", "password", "connection-secret-id")
        .unwrap();
    repo.commit_secret_write("operation").unwrap();
    drop(repo);
    db.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES ('example.org',22,'ssh-ed25519','fingerprint')", []).unwrap();
    db.execute("INSERT INTO command_history(id,connection_id,connection_name,production,command,submitted_at,outcome) VALUES ('h','c','host',1,'history-sentinel','2026-01-01T00:00:00Z','unknown')", []).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,credential_secret_id) VALUES ('w','https://example.org','/','user','webdav-secret-id')", []).unwrap();
    drop(db);
    let sessions = SessionSupervisor::new(8, 128, std::time::Duration::from_secs(1)).unwrap();
    sessions
        .register("session", "c", true, std::sync::Arc::new(ActiveChannel))
        .await
        .unwrap();
    sessions.connected("session").await.unwrap();
    sessions
        .output("session", b"terminal-output-sentinel")
        .await
        .unwrap();
    assert_eq!(
        sessions.scrollback("session").await.unwrap(),
        b"terminal-output-sentinel"
    );
    for include_secrets in [false, true] {
        let encoded = create_backup(
            &path,
            &Store,
            b"password",
            BackupOptions { include_secrets },
            &FeatureFlags::default().with_enabled(Feature::WebDavBackup),
        )
        .unwrap();
        assert!(!String::from_utf8_lossy(&encoded).contains("connection-secret"));
        let clear = coshell_backup::decrypt(&encoded, b"password").unwrap();
        let payload: serde_json::Value = serde_json::from_slice(clear.as_slice()).unwrap();
        assert_eq!(payload["version"], 1);
        assert_eq!(payload["connections"][0]["production"], true);
        assert_eq!(payload["connections"][0]["version"], 4);
        assert_eq!(payload["groups"][0]["id"], "g");
        assert_eq!(payload["host_keys"][0]["fingerprint"], "fingerprint");
        let text = String::from_utf8_lossy(clear.as_slice());
        assert!(!text.contains("history-sentinel"));
        assert!(!text.contains("terminal-output-sentinel"));
        assert!(!text.contains("webdav-secret-id"));
        assert!(!text.contains("connection-secret-id"));
        assert_eq!(
            payload["secrets"].as_array().unwrap().len(),
            usize::from(include_secrets)
        );
        if include_secrets {
            assert_eq!(payload["secrets"][0]["connection_id"], "c");
            assert_eq!(payload["secrets"][0]["secret_type"], "password");
            let bytes: Vec<u8> =
                serde_json::from_value(payload["secrets"][0]["value"].clone()).unwrap();
            assert_eq!(bytes, b"connection-secret");
        }
    }
    assert_eq!(
        sessions.scrollback("session").await.unwrap(),
        b"terminal-output-sentinel"
    );
    sessions.close("session").await.unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

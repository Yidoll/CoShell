use coshell_backup::webdav::{Cancellation, PublishCondition, WebDav};
use coshell_core::{
    backup_inspection::{inspect_for_restore, prepare_restore_with_groups, RestoreChoice},
    backup_snapshot::{create_backup, BackupOptions},
    feature_flags::{Feature, FeatureFlags},
    publication_outbox::Outbox,
    restore_secrets::{self, IncomingSecret},
};
use coshell_platform::secret_store::{Secret, SecretError, SecretStore};
use coshell_storage::Repository;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[derive(Default)]
struct Store(Mutex<HashMap<String, Vec<u8>>>);
impl SecretStore for Store {
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self.0.lock().unwrap().get(id).cloned().map(Secret::new))
    }
    fn set(&self, id: &str, s: &Secret) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), s.expose().to_vec());
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}
async fn stateful_server() -> (
    String,
    Arc<Mutex<std::collections::HashMap<String, (Vec<u8>, usize)>>>,
    tokio::task::JoinHandle<()>,
) {
    stateful_server_with_broken_create(false).await
}

async fn stateful_server_with_broken_create(
    broken: bool,
) -> (
    String,
    Arc<Mutex<std::collections::HashMap<String, (Vec<u8>, usize)>>>,
    tokio::task::JoinHandle<()>,
) {
    stateful_server_with_fault(if broken { 1 } else { 0 }, 0, false).await
}

async fn stateful_server_with_fault(
    broken: u8,
    failure: u16,
    supersede: bool,
) -> (
    String,
    Arc<Mutex<std::collections::HashMap<String, (Vec<u8>, usize)>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let state = Arc::new(Mutex::new(std::collections::HashMap::<
        String,
        (Vec<u8>, usize),
    >::new()));
    let shared = state.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let state = shared.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 2048];
                let (end, length) = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                line.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break (end, length);
                        }
                    }
                };
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let mut lines = headers.lines();
                let mut first = lines.next().unwrap().split_whitespace();
                let method = first.next().unwrap();
                let path = first.next().unwrap().to_owned();
                let body = bytes[end + 4..end + 4 + length].to_vec();
                let mut status = 200;
                let mut output = Vec::new();
                let mut etag = None;
                {
                    let mut map = state.lock().unwrap();
                    let lower = headers.to_ascii_lowercase();
                    match method {
                        "OPTIONS" => {}
                        "PUT" => {
                            let prior = map.get(&path);
                            let none = lower.contains("if-none-match: *");
                            let wanted = lower
                                .lines()
                                .find_map(|l| l.strip_prefix("if-match:").map(str::trim));
                            if (none
                                && prior.is_some()
                                && !((broken == 1 && path.contains("-create"))
                                    || (broken == 2 && path.starts_with("/backup/receipt-"))))
                                || wanted.is_some_and(|w| {
                                    prior.map(|(_, v)| format!("\"{v}\"")).as_deref() != Some(w)
                                })
                            {
                                status = 412;
                            } else {
                                let version = prior.map_or(1, |(_, v)| v + 1);
                                map.insert(path.clone(), (body, version));
                                status = 201;
                            }
                        }
                        "GET" | "HEAD" => {
                            if broken == 3 && path.starts_with("/backup/receipt-") {
                                status = 404;
                            } else if let Some((data, version)) = map.get(&path) {
                                if method == "GET" {
                                    output = data.clone();
                                }
                                etag = Some(format!("\"{version}\""));
                            } else {
                                status = 404;
                            }
                        }
                        "DELETE" => {
                            status = if map.remove(&path).is_some() {
                                200
                            } else {
                                404
                            };
                        }
                        "MOVE" => {
                            status = 500;
                        }
                        _ => status = 405,
                    }
                }
                if method == "PUT" && path == "/backup/latest" && status == 201 && failure != 0 {
                    if supersede {
                        state.lock().unwrap().insert(path.clone(), (br#"{"version":1,"snapshot_id":"later","object":"attempt-later","operation_token":"later","condition":"Absent"}"#.to_vec(), 2));
                    }
                    if failure == 1 {
                        return;
                    }
                    status = failure;
                }
                let response = format!("HTTP/1.1 {status} X\r\nAllow: GET, PUT, DELETE, MOVE\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n", etag.map(|e|format!("ETag: {e}\r\n")).unwrap_or_default(),output.len());
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.write_all(&output).await.unwrap();
            });
        }
    });
    (format!("http://{address}/backup/"), state, task)
}

#[tokio::test]
async fn plaintext_backup_is_remote_visible_but_outbox_is_protected_and_restores() {
    let dir = std::env::temp_dir().join(format!(
        "coshell-dav-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("source.sqlite");
    let mut db = rusqlite::Connection::open(&source).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication) VALUES('c','server','example.org',22,'user','password')",[]).unwrap();
    db.execute(
        "INSERT INTO connection_secret_refs VALUES('c','password','source-id')",
        [],
    )
    .unwrap();
    drop(db);
    let source_store = Store::default();
    source_store
        .set("source-id", &Secret::new(b"secret-marker-123".to_vec()))
        .unwrap();
    let flags = FeatureFlags::default().with_enabled(Feature::WebDavBackup);
    let package = create_backup(
        &source,
        &source_store,
        b"",
        BackupOptions {
            include_secrets: true,
        },
        &flags,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&package).contains("\"protection\":\"none\""));
    let (url, remote, task) = stateful_server().await;
    let client = WebDav::new(&url, "user", "password", Duration::from_secs(3)).unwrap();
    let queue = dir.join("outbox.sqlite");
    let mut q = rusqlite::Connection::open(&queue).unwrap();
    q.pragma_update(None, "journal_mode", "WAL").unwrap();
    let mut outbox = Outbox::new_protected(&mut q, zeroize::Zeroizing::new([42; 32])).unwrap();
    outbox
        .prepare(
            "op",
            &url,
            "snapshot",
            &package,
            b"",
            PublishCondition::Absent,
        )
        .unwrap();
    outbox
        .publish_once("op", &client, &Cancellation::new())
        .await
        .unwrap();
    let latest = client.download_latest(&Cancellation::new()).await.unwrap();
    assert_eq!(latest, package);
    let (preview, secrets) = inspect_for_restore(&latest, b"").unwrap();
    assert_eq!(preview.connections.len(), 1);
    assert_eq!(secrets.len(), 1);
    assert_eq!(secrets[0].value(), b"secret-marker-123");
    let has_remote = remote
        .lock()
        .unwrap()
        .values()
        .any(|(bytes, _)| bytes == &package);
    assert!(has_remote);
    for path in [&queue, &dir.join("outbox.sqlite-wal")] {
        if path.exists() {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes
                .windows(package.len())
                .any(|window| window == package.as_slice()));
            assert!(!bytes
                .windows(b"secret-marker-123".len())
                .any(|w| w == b"secret-marker-123"));
        }
    }
    let target = dir.join("target.sqlite");
    let mut t = rusqlite::Connection::open(&target).unwrap();
    coshell_storage::migrate(&mut t).unwrap();
    let expected = Repository::new(&mut t).snapshot().unwrap();
    let mut choices = HashMap::new();
    choices.insert("c".into(), RestoreChoice::Replace);
    let desired = prepare_restore_with_groups(&preview, &expected, &choices, true).unwrap();
    let restored = Store::default();
    let incoming = secrets
        .iter()
        .map(|s| IncomingSecret {
            connection_id: &s.connection_id,
            secret_type: &s.secret_type,
            value: s.value(),
        })
        .collect::<Vec<_>>();
    restore_secrets::apply(
        &target,
        &restored,
        "restore-op",
        &expected,
        &desired,
        &incoming,
    )
    .unwrap();
    let after = Repository::new(&mut t).snapshot().unwrap();
    assert_eq!(after.connections[0].host, "example.org");
    assert_eq!(
        restored
            .get(&after.secret_refs[0].secret_id)
            .unwrap()
            .unwrap()
            .expose(),
        b"secret-marker-123"
    );
    drop(outbox);
    drop(q);
    drop(t);
    task.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn saved_private_key_plaintext_backup_restores_into_new_directory_and_conflict_copy() {
    use coshell_core::ssh_key_store;
    use coshell_storage::ConnectionRecord;
    let dir = std::env::temp_dir().join(format!(
        "coshell-key-roundtrip-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let source = dir.join("source.sqlite");
    let mut db = rusqlite::Connection::open(&source).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    let record = ConnectionRecord {
        id: "c".into(),
        name: "server".into(),
        host: "example.org".into(),
        port: 22,
        username: "user".into(),
        authentication: "private_key".into(),
        group_id: None,
        notes: "source notes".into(),
        production: true,
        always_confirm: true,
        version: 0,
    };
    Repository::new(&mut db).save_connection(&record).unwrap();
    drop(db);
    let source_store = Store::default();
    ssh_key_store::save(
        &source,
        &source_store,
        &record,
        "PRIVATE_KEY_FIXTURE",
        "phrase",
        "key-op",
    )
    .unwrap();
    let flags = FeatureFlags::default().with_enabled(Feature::WebDavBackup);
    let package = create_backup(
        &source,
        &source_store,
        b"",
        BackupOptions {
            include_secrets: true,
        },
        &flags,
    )
    .unwrap();
    assert!(String::from_utf8_lossy(&package).contains("\"protection\":\"none\""));
    let (preview, secrets) = inspect_for_restore(&package, b"").unwrap();
    assert_eq!(secrets.len(), 1);
    let target_dir = dir.join("target");
    std::fs::create_dir(&target_dir).unwrap();
    let target = target_dir.join("db.sqlite");
    let mut target_db = rusqlite::Connection::open(&target).unwrap();
    coshell_storage::migrate(&mut target_db).unwrap();
    let before = Repository::new(&mut target_db).snapshot().unwrap();
    let choices = HashMap::from([("c".into(), RestoreChoice::Replace)]);
    let desired = prepare_restore_with_groups(&preview, &before, &choices, false).unwrap();
    let target_store = Store::default();
    let incoming = secrets
        .iter()
        .map(|s| IncomingSecret {
            connection_id: &s.connection_id,
            secret_type: &s.secret_type,
            value: s.value(),
        })
        .collect::<Vec<_>>();
    restore_secrets::apply(
        &target,
        &target_store,
        "restore-new",
        &before,
        &desired,
        &incoming,
    )
    .unwrap();
    let restored = Repository::new(&mut target_db).snapshot().unwrap();
    let (key, phrase) = ssh_key_store::load(&target, &target_store, &restored.connections[0])
        .unwrap()
        .unwrap();
    assert_eq!(key.expose(), b"PRIVATE_KEY_FIXTURE");
    assert_eq!(phrase.unwrap().expose(), b"phrase");
    // Existing same-ID connection with a different destination must remain
    // unchanged for Keep; Copy creates a distinct ID and imports its secret.
    let mut conflict = restored.connections[0].clone();
    conflict.name = "local".into();
    conflict.notes = "local notes".into();
    Repository::new(&mut target_db)
        .save_connection(&conflict)
        .unwrap();
    let conflict_snapshot = Repository::new(&mut target_db).snapshot().unwrap();
    let keep = HashMap::from([("c".into(), RestoreChoice::Keep)]);
    let kept = prepare_restore_with_groups(&preview, &conflict_snapshot, &keep, false).unwrap();
    assert_eq!(kept, conflict_snapshot);
    let copy = HashMap::from([(
        "c".into(),
        RestoreChoice::Copy {
            id: "copy".into(),
            name: "server copy".into(),
        },
    )]);
    let desired_copy =
        prepare_restore_with_groups(&preview, &conflict_snapshot, &copy, false).unwrap();
    let copy_incoming = secrets
        .iter()
        .map(|s| IncomingSecret {
            connection_id: "copy",
            secret_type: &s.secret_type,
            value: s.value(),
        })
        .collect::<Vec<_>>();
    restore_secrets::apply(
        &target,
        &target_store,
        "restore-copy",
        &conflict_snapshot,
        &desired_copy,
        &copy_incoming,
    )
    .unwrap();
    let final_snapshot = Repository::new(&mut target_db).snapshot().unwrap();
    assert_eq!(final_snapshot.connections.len(), 2);
    assert_eq!(
        final_snapshot
            .connections
            .iter()
            .find(|c| c.id == "c")
            .unwrap()
            .notes,
        "local notes"
    );
    let copy_record = final_snapshot
        .connections
        .iter()
        .find(|c| c.id == "copy")
        .unwrap();
    assert_eq!(
        ssh_key_store::load(&target, &target_store, copy_record)
            .unwrap()
            .unwrap()
            .0
            .expose(),
        b"PRIVATE_KEY_FIXTURE"
    );
    drop(target_db);
    std::fs::remove_dir_all(dir).unwrap();
}

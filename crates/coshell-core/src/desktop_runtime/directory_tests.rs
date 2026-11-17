use super::*;
use coshell_platform::secret_store::{SecretError, SecretStore};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
#[derive(Default)]
struct Store(Mutex<HashMap<String, Vec<u8>>>);
impl SecretStore for Store {
    fn get(&self, id: &str) -> Result<Option<Secret>, SecretError> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .get(id)
            .map(|v| Secret::new(v.clone())))
    }
    fn set(&self, id: &str, v: &Secret) -> Result<(), SecretError> {
        self.0
            .lock()
            .unwrap()
            .insert(id.into(), v.expose().to_vec());
        Ok(())
    }
    fn delete(&self, id: &str) -> Result<(), SecretError> {
        self.0.lock().unwrap().remove(id);
        Ok(())
    }
}
async fn server() -> (
    String,
    Arc<Mutex<HashMap<String, Vec<u8>>>>,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let objects = Arc::new(Mutex::new(HashMap::<String, Vec<u8>>::from([(
        "/".into(),
        Vec::new(),
    )])));
    let requests = Arc::new(Mutex::new(Vec::new()));
    let o = objects.clone();
    let q = requests.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let objects = o.clone();
            let requests = q.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let (end, size) = loop {
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let h = String::from_utf8_lossy(&bytes[..end]);
                        let len = h
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + len {
                            break (end, len);
                        }
                    }
                    if bytes.len() > 48 * 1024 * 1024 {
                        return;
                    }
                };
                let header = String::from_utf8_lossy(&bytes[..end]);
                let mut line = header.lines().next().unwrap().split_whitespace();
                let method = line.next().unwrap();
                let path = line.next().unwrap();
                requests.lock().unwrap().push(format!("{method} {path}"));
                let (mut status, mut body, mut length) = (200, Vec::new(), 0);
                {
                    let mut m = objects.lock().unwrap();
                    match method {
                        "MKCOL" => {
                            if m.contains_key(path) {
                                status = 405
                            } else {
                                m.insert(path.into(), Vec::new());
                                status = 201;
                            }
                        }
                        "PUT" => {
                            if m.contains_key(path) {
                                status = 412
                            } else {
                                m.insert(path.into(), bytes[end + 4..end + 4 + size].to_vec());
                                status = 201;
                            }
                        }
                        "GET" | "HEAD" => {
                            if let Some(v) = m.get(path) {
                                length = v.len();
                                if method == "GET" {
                                    body = v.clone();
                                }
                            } else {
                                status = 404;
                            }
                        }
                        "PROPFIND" => {
                            if !m.contains_key(path) {
                                status = 404
                            } else {
                                status = 207;
                                let mut xml = "<d:multistatus xmlns:d=\"DAV:\">".to_owned();
                                for entry in m.keys().filter(|p| {
                                    p.starts_with(path)
                                        && !p
                                            .strip_prefix(path)
                                            .unwrap()
                                            .trim_end_matches('/')
                                            .contains('/')
                                }) {
                                    let collection = if entry.ends_with('/') {
                                        "<d:collection/>"
                                    } else {
                                        ""
                                    };
                                    xml.push_str(&format!("<d:response><d:href>{entry}</d:href><d:propstat><d:prop><d:resourcetype>{collection}</d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"));
                                }
                                xml.push_str("</d:multistatus>");
                                body = xml.into_bytes();
                            }
                        }
                        _ => status = 405,
                    }
                }
                if method != "HEAD" {
                    length = body.len();
                }
                let out=format!("HTTP/1.1 {status} Test\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n");
                let _ = stream.write_all(out.as_bytes()).await;
                let _ = stream.write_all(&body).await;
            });
        }
    });
    (base, objects, requests, task)
}

#[tokio::test]
async fn independent_runtime_sync_list_preview_reconcile_and_target_binding() {
    let (base, objects, requests, server) = server().await;
    let dir = std::env::temp_dir().join(format!("coshell-independent-runtime-{}", id().unwrap()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("main.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username) VALUES('default',?1,'backup','u')",[&base]).unwrap();
    db.execute("INSERT INTO connections(id,name,host,port,username,authentication,production,always_confirm)VALUES('c','test','example.org',22,'u','password',1,1)",[]).unwrap();
    let mut runtime = DesktopRuntime::from_connection(
        db,
        FeatureFlags::default().with_enabled(Feature::Connections),
    )
    .unwrap();
    runtime.path = Some(path.clone());
    runtime.store_override = Some(Arc::new(Store::default()));
    assert!(runtime
        .webdav_sync("".into(), "new-snapshot".into())
        .await
        .unwrap()
        .contains("备份完成"));
    assert!(!objects.lock().unwrap().contains_key("/backup/latest"));
    let page = runtime.webdav_list_snapshots(None).await.unwrap();
    let wire = serde_json::to_value(&page).unwrap();
    assert!(wire["snapshots"].is_array());
    assert!(wire["nextCursor"].is_null());
    let item = page.snapshots.iter().find(|v| v.restorable).unwrap();
    let preview = runtime
        .webdav_preview_snapshot("".into(), item.id.clone())
        .await
        .unwrap();
    assert_eq!(preview.connections.len(), 1);
    let manifest_path = format!("/backup/{}/manifest.json", item.directory);
    let manifest_hash = coshell_backup::snapshot_manifest::sha256(
        objects.lock().unwrap().get(&manifest_path).unwrap(),
    );
    let preview_wire = serde_json::to_value(&preview).unwrap();
    assert_eq!(preview_wire["snapshotId"], item.id);
    assert_eq!(preview_wire["manifestSha256"], manifest_hash);
    let local = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(
        local
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key LIKE 'webdav.etag.%'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    let writes = requests
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.starts_with("PUT ") || s.starts_with("MKCOL "))
        .count();
    runtime
        .webdav_sync("".into(), "new-snapshot".into())
        .await
        .unwrap();
    assert_eq!(
        writes,
        requests
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.starts_with("PUT ") || s.starts_with("MKCOL "))
            .count()
    );
    let outbox_path = dir.join("webdav-outbox.sqlite");
    let outbox = rusqlite::Connection::open(&outbox_path).unwrap();
    outbox
        .execute(
            "UPDATE directory_publications SET state='unknown' WHERE operation_id='new-snapshot'",
            [],
        )
        .unwrap();
    assert!(runtime
        .webdav_reconcile("new-snapshot".into())
        .await
        .unwrap()
        .contains("只读核实"));
    local
        .execute("UPDATE webdav_profiles SET remote_directory='other'", [])
        .unwrap();
    let snapshot = preview.token;
    assert!(runtime
        .webdav_apply_restore(
            snapshot,
            vec![WebDavChoiceDto {
                id: "c".into(),
                action: "keep".into(),
                copy_id: None,
                copy_name: None
            }],
            false
        )
        .await
        .unwrap_err()
        .contains("目标已变化"));
    assert!(runtime
        .webdav_sync("".into(), "new-snapshot".into())
        .await
        .unwrap_err()
        .contains("目标已变化"));
    assert!(!objects.lock().unwrap().contains_key("/other/"));
    drop(outbox);
    drop(local);
    drop(runtime);
    server.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

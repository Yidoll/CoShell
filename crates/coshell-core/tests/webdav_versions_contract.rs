use coshell_core::desktop_runtime::DesktopRuntime;
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn desktop_versions_return_the_page_shape_and_preview_timestamped_v2_snapshot() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/", listener.local_addr().unwrap());
    let payload = br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[]}"#;
    let backup = coshell_backup::encode_optional(payload, b"").unwrap();
    let index=serde_json::to_vec(&serde_json::json!({"version":2,"snapshot_id":"snapshot-one","object":"20261116031818/attempt-025437324af4f883c6f665960150ab2c","operation_token":"op-one","condition":"Absent"})).unwrap();
    let objects = Arc::new(std::collections::HashMap::from([
        ("/coshell/latest".to_owned(), index),
        (
            "/coshell/20261116031818/attempt-025437324af4f883c6f665960150ab2c".to_owned(),
            backup,
        ),
    ]));
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let objects = objects.clone();
            tokio::spawn(async move {
                let mut request = Vec::new();
                let mut buf = [0; 2048];
                loop {
                    let n = socket.read(&mut buf).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    request.extend_from_slice(&buf[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                    if request.len() > 16384 {
                        return;
                    }
                }
                let line = String::from_utf8_lossy(&request);
                let mut fields = line.lines().next().unwrap().split_whitespace();
                let method = fields.next().unwrap();
                let path = fields.next().unwrap();
                let (status, body) = match objects.get(path) {
                    Some(v) => ("200 OK", v.as_slice()),
                    None => ("404 Not Found", &[][..]),
                };
                let header=format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nETag: \"stable\"\r\nConnection: close\r\n\r\n",body.len());
                socket.write_all(header.as_bytes()).await.unwrap();
                if method != "HEAD" {
                    socket.write_all(body).await.unwrap();
                }
            });
        }
    });
    let dir = std::env::temp_dir().join(format!(
        "coshell-versions-contract-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("coshell.sqlite");
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    db.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,auto_backup) VALUES('default',?1,'coshell','',0)",[&base]).unwrap();
    drop(db);
    let runtime = DesktopRuntime::open(&path).unwrap();
    // This is the exact page contract expected by the frontend/Tauri boundary.
    let page = runtime.webdav_list_versions(None).await.unwrap();
    let wire = serde_json::to_value(&page).unwrap();
    assert!(wire["versions"].is_array());
    assert!(wire.get("nextCursor").is_some());
    assert_eq!(wire["versions"][0]["id"], "snapshot-one");
    assert_eq!(wire["versions"][0]["timestamp"], "20261116031818");
    assert!(wire["nextCursor"].is_null());
    let preview = runtime
        .webdav_preview_version("".into(), Some("snapshot-one".into()))
        .await
        .unwrap();
    assert!(preview.connections.is_empty());
    assert_eq!(preview.protection, "none");
    drop(runtime);
    task.abort();
    std::fs::remove_dir_all(dir).unwrap();
}

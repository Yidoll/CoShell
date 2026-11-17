use coshell_backup::{
    snapshot_manifest::{self, SnapshotMetadata},
    webdav::{Cancellation, WebDav, WebDavError},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
type Objects = Arc<Mutex<HashMap<String, Vec<u8>>>>;
async fn server() -> (
    String,
    Objects,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    server_with_fault(None).await
}
async fn server_with_fault(
    fault: Option<&'static str>,
) -> (
    String,
    Objects,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let objects: Objects = Arc::new(Mutex::new(HashMap::from([("/backup/".into(), Vec::new())])));
    let events = Arc::new(Mutex::new(Vec::new()));
    let shared = objects.clone();
    let log = events.clone();
    let task = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let shared = shared.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                let mut buf = [0; 4096];
                let (end, len) = loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        return;
                    }
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let len = headers
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
                };
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let mut first = headers.lines().next().unwrap().split_whitespace();
                let method = first.next().unwrap();
                let path = first.next().unwrap();
                log.lock().unwrap().push(format!("{method} {path}"));
                if method == "GET"
                    && fault == Some("slow")
                    && path == "/backup/20261116031819/manifest.json"
                {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                if method == "PUT" && fault == Some("upload") && path.ends_with("connections.pkg") {
                    socket.write_all(b"HTTP/1.1 500 Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    return;
                }
                if method == "HEAD"
                    && fault == Some("package-forbidden")
                    && path.ends_with("connections.pkg")
                {
                    socket.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    return;
                }
                if method == "GET"
                    && fault == Some("manifest-unauthorized")
                    && path.ends_with("manifest.json")
                {
                    socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    return;
                }
                if method == "GET"
                    && fault == Some("complete-read-unauthorized")
                    && path.ends_with("complete.json")
                {
                    socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                    return;
                }
                let mut status = 200;
                let mut body = Vec::new();
                let mut declared = 0;
                {
                    let mut map = shared.lock().unwrap();
                    match method {
                        "MKCOL" => {
                            if map.contains_key(path) {
                                status = 405;
                            } else {
                                map.insert(path.into(), Vec::new());
                                status = 201;
                            }
                        }
                        "PUT" => {
                            if headers.to_ascii_lowercase().contains("if-none-match: *")
                                && map.contains_key(path)
                            {
                                status = 412;
                            } else {
                                map.insert(path.into(), bytes[end + 4..end + 4 + len].to_vec());
                                status = 201;
                            }
                        }
                        "GET" | "HEAD" => {
                            if let Some(value) = map.get(path) {
                                declared = value.len();
                                if method == "GET" {
                                    body = value.clone();
                                }
                            } else {
                                status = 404;
                            }
                        }
                        "PROPFIND" => {
                            if !map.contains_key(path) {
                                status = 404;
                            } else {
                                status = 207;
                                let mut xml = "<d:multistatus xmlns:d=\"DAV:\">".to_owned();
                                for name in map.keys().filter(|p| p.starts_with(path)) {
                                    let tail = name.strip_prefix(path).unwrap();
                                    if tail.trim_end_matches('/').contains('/') {
                                        continue;
                                    }
                                    let collection = if name.ends_with('/') {
                                        "<d:collection/>"
                                    } else {
                                        ""
                                    };
                                    xml.push_str(&format!("<d:response><d:href>{name}</d:href><d:propstat><d:prop><d:resourcetype>{collection}</d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"));
                                }
                                xml.push_str("</d:multistatus>");
                                body = xml.into_bytes();
                                declared = body.len();
                            }
                        }
                        _ => status = 405,
                    }
                }
                if method == "PUT" && fault == Some("complete") && path.ends_with("complete.json") {
                    return;
                }
                if method != "HEAD" {
                    declared = body.len();
                }
                let response=format!("HTTP/1.1 {status} X\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n");
                socket.write_all(response.as_bytes()).await.unwrap();
                socket.write_all(&body).await.unwrap();
            });
        }
    });
    (format!("http://{addr}/backup/"), objects, events, task)
}
fn artifact(id: &str, time: &str) -> snapshot_manifest::SnapshotArtifact {
    let legacy = br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[]}"#;
    let encoded = coshell_backup::encode_optional(legacy, b"").unwrap();
    snapshot_manifest::build_from_legacy(
        &encoded,
        b"",
        SnapshotMetadata {
            snapshot_id: id,
            created_at: time,
            app_version: "0.1.0",
        },
    )
    .unwrap()
}

#[tokio::test]
async fn publishes_complete_last_and_lists_without_global_index() {
    let (url, objects, events, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let a = artifact("a", "2026-11-16T03:18:18Z");
    let cancel = Cancellation::new();
    client
        .publish_snapshot("20261116031818", &a, "operation", &cancel)
        .await
        .unwrap();
    let writes: Vec<_> = events
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.starts_with("PUT"))
        .cloned()
        .collect();
    assert_eq!(
        writes.last().unwrap(),
        "PUT /backup/20261116031818/complete.json"
    );
    assert!(!objects.lock().unwrap().contains_key("/backup/latest"));
    let page = client.list_snapshots(None, &cancel).await.unwrap();
    assert_eq!(page.snapshots.len(), 1);
    assert!(page.snapshots[0].restorable);
    assert_eq!(page.snapshots[0].state, "structurally_complete");
    client
        .verify_snapshot("20261116031818", &a, "operation", &cancel)
        .await
        .unwrap();
    let downloaded = client
        .download_snapshot("20261116031818", &cancel)
        .await
        .unwrap();
    snapshot_manifest::open_packages(
        &downloaded.manifest,
        &downloaded.complete,
        &downloaded.connections_package,
        None,
        b"",
    )
    .unwrap();
    task.abort();
}

#[tokio::test]
async fn damaged_or_incomplete_cards_do_not_hide_valid_snapshot() {
    let (url, objects, _, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let a = artifact("a", "2026-11-16T03:18:18Z");
    client
        .publish_snapshot("20261116031818", &a, "operation", &Cancellation::new())
        .await
        .unwrap();
    {
        let mut m = objects.lock().unwrap();
        m.insert("/backup/20261116031819/".into(), Vec::new());
        m.insert(
            "/backup/20261116031819/manifest.json".into(),
            a.manifest.clone(),
        );
        m.insert("/backup/20261116031820/".into(), Vec::new());
        m.insert(
            "/backup/20261116031820/manifest.json".into(),
            b"garbage".to_vec(),
        );
    }
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(page.snapshots.len(), 3);
    assert_eq!(page.snapshots.iter().filter(|s| s.restorable).count(), 1);
    assert!(page.snapshots.iter().any(|s| s.state == "invalid_manifest"));
    assert!(page.snapshots.iter().any(|s| s.state == "incomplete"));
    task.abort();
}

#[tokio::test]
async fn collision_and_cancel_never_overwrite_or_complete() {
    let (url, objects, _, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let a = artifact("a", "2026-11-16T03:18:18Z");
    objects
        .lock()
        .unwrap()
        .insert("/backup/20261116031818/".into(), Vec::new());
    assert_eq!(
        client
            .publish_snapshot("20261116031818", &a, "op", &Cancellation::new())
            .await,
        Err(WebDavError::Conflict)
    );
    assert!(!objects
        .lock()
        .unwrap()
        .contains_key("/backup/20261116031818/complete.json"));
    let c = Cancellation::new();
    c.cancel();
    assert!(client
        .publish_snapshot("20261116031819", &a, "op", &c)
        .await
        .is_err());
    assert!(!objects
        .lock()
        .unwrap()
        .contains_key("/backup/20261116031819/complete.json"));
    task.abort();
}

#[tokio::test]
async fn manifest_time_orders_across_directory_name_order() {
    let (url, _, _, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    client
        .publish_snapshot(
            "20261116031818",
            &artifact("a", "2026-11-17T03:18:18Z"),
            "op-a",
            &Cancellation::new(),
        )
        .await
        .unwrap();
    client
        .publish_snapshot(
            "20261116031819",
            &artifact("b", "2026-11-16T03:18:19Z"),
            "op-b",
            &Cancellation::new(),
        )
        .await
        .unwrap();
    let p = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(p.snapshots[0].directory, "20261116031818");
    task.abort();
}

#[tokio::test]
async fn missing_required_credentials_and_digest_mismatch_are_distinct() {
    let (url, objects, _, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let cancel = Cancellation::new();
    let legacy=br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[{"connection_id":"x","secret_type":"password","value":[1,2]}]}"#;
    let encoded = coshell_backup::encode_optional(legacy, b"").unwrap();
    let a = snapshot_manifest::build_from_legacy(
        &encoded,
        b"",
        SnapshotMetadata {
            snapshot_id: "secret",
            created_at: "2026-11-16T03:18:18Z",
            app_version: "0.1.0",
        },
    )
    .unwrap();
    client
        .publish_snapshot("20261116031818", &a, "op", &cancel)
        .await
        .unwrap();
    objects
        .lock()
        .unwrap()
        .remove("/backup/20261116031818/credentials.pkg");
    let p = client.list_snapshots(None, &cancel).await.unwrap();
    assert_eq!(p.snapshots[0].state, "missing_credentials");
    assert!(!p.snapshots[0].restorable);
    objects
        .lock()
        .unwrap()
        .insert("/backup/20261116031818/credentials.pkg".into(), vec![0; 3]);
    let p = client.list_snapshots(None, &cancel).await.unwrap();
    assert_eq!(p.snapshots[0].state, "integrity_mismatch");
    task.abort();
}

#[tokio::test]
async fn legacy_index_is_read_only_and_missing_latest_does_not_hide_independent() {
    let (url, objects, events, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let cancel = Cancellation::new();
    let a = artifact("a", "2026-11-16T03:18:18Z");
    client
        .publish_snapshot("20261116031818", &a, "op", &cancel)
        .await
        .unwrap();
    objects.lock().unwrap().insert("/backup/latest".into(),br#"{"version":1,"snapshot_id":"old","object":"attempt-old","operation_token":"oldop","condition":"Absent"}"#.to_vec());
    objects
        .lock()
        .unwrap()
        .insert("/backup/attempt-old".into(), b"old bytes".to_vec());
    events.lock().unwrap().clear();
    let p = client.list_snapshots(None, &cancel).await.unwrap();
    assert!(p
        .snapshots
        .iter()
        .any(|c| c.id == "legacy:old" && c.restorable));
    assert!(p
        .snapshots
        .iter()
        .any(|c| c.id == "snapshot:20261116031818" && c.restorable));
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .all(|e| !e.starts_with("PUT") && !e.starts_with("DELETE") && !e.starts_with("MKCOL")));
    objects
        .lock()
        .unwrap()
        .insert("/backup/latest".into(), b"invalid".to_vec());
    let p = client.list_snapshots(None, &cancel).await.unwrap();
    assert!(p
        .snapshots
        .iter()
        .any(|c| c.source == "independent" && c.restorable));
    assert!(p
        .snapshots
        .iter()
        .any(|c| c.source == "legacy" && c.state == "unavailable"));
    task.abort();
}

#[tokio::test]
async fn lost_completion_response_is_reconciled_only_by_exact_readback() {
    let (url, objects, _, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let a = artifact("a", "2026-11-16T03:18:18Z");
    client
        .publish_snapshot("20261116031818", &a, "op", &Cancellation::new())
        .await
        .unwrap();
    // Model a crash after the server committed complete.json but before the
    // caller recorded success. Reconciliation is read-only and checks every byte.
    client
        .verify_snapshot("20261116031818", &a, "op", &Cancellation::new())
        .await
        .unwrap();
    objects.lock().unwrap().insert(
        "/backup/20261116031818/connections.pkg".into(),
        vec![0; a.connections_package.len()],
    );
    assert_eq!(
        client
            .verify_snapshot("20261116031818", &a, "op", &Cancellation::new())
            .await,
        Err(WebDavError::Integrity)
    );
    task.abort();
}

#[tokio::test]
async fn interrupted_upload_never_marks_complete_and_lost_complete_can_be_verified() {
    for fault in ["upload", "complete"] {
        let (url, objects, _, task) = server_with_fault(Some(fault)).await;
        let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
        let artifact = artifact("a", "2026-11-16T03:18:18Z");
        assert_eq!(
            client
                .publish_snapshot("20261116031818", &artifact, "op", &Cancellation::new())
                .await,
            Err(WebDavError::OutcomeUnknown)
        );
        let exists = objects
            .lock()
            .unwrap()
            .contains_key("/backup/20261116031818/complete.json");
        assert_eq!(exists, fault == "complete");
        if fault == "complete" {
            client
                .verify_snapshot("20261116031818", &artifact, "op", &Cancellation::new())
                .await
                .unwrap();
        } else {
            assert!(client
                .verify_snapshot("20261116031818", &artifact, "op", &Cancellation::new())
                .await
                .is_err());
        }
        task.abort();
    }
}

#[tokio::test]
async fn invalid_directory_date_is_unknown_and_legacy_directory_is_folded_once() {
    let (url, objects, _, task) = server().await;
    {
        let mut map = objects.lock().unwrap();
        map.insert("/backup/20261399259999/".into(), Vec::new());
        map.insert("/backup/20261116031818/".into(), Vec::new());
        map.insert("/backup/20261116031818/attempt-old".into(), b"old".to_vec());
        map.insert("/backup/latest".into(), br#"{"version":2,"snapshot_id":"old","object":"20261116031818/attempt-old","operation_token":"oldop","condition":"Absent"}"#.to_vec());
    }
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(
        page.snapshots
            .iter()
            .filter(|c| c.directory == "20261116031818")
            .count(),
        1
    );
    assert_eq!(
        page.snapshots
            .iter()
            .find(|c| c.directory == "20261116031818")
            .unwrap()
            .state,
        "legacy"
    );
    assert_eq!(
        page.snapshots
            .iter()
            .find(|c| c.directory == "20261399259999")
            .unwrap()
            .created_at,
        None
    );
    task.abort();
}

#[tokio::test]
async fn one_slow_snapshot_becomes_unavailable_without_failing_the_page() {
    let (url, objects, _, task) = server_with_fault(Some("slow")).await;
    objects
        .lock()
        .unwrap()
        .insert("/backup/20261116031819/".into(), Vec::new());
    let writer = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    writer
        .publish_snapshot(
            "20261116031818",
            &artifact("a", "2026-11-16T03:18:18Z"),
            "op",
            &Cancellation::new(),
        )
        .await
        .unwrap();
    let client = WebDav::new(&url, "u", "p", Duration::from_millis(200)).unwrap();
    let page = tokio::time::timeout(
        Duration::from_millis(150),
        client.list_snapshots(None, &Cancellation::new()),
    )
    .await
    .expect("one slow card must not exhaust the page deadline")
    .unwrap();
    assert!(page
        .snapshots
        .iter()
        .any(|c| c.directory == "20261116031818" && c.restorable));
    assert!(page
        .snapshots
        .iter()
        .any(|c| c.directory == "20261116031819" && c.state == "unavailable" && !c.restorable));
    assert_eq!(page.scanned_count, 2);
    task.abort();
}

#[tokio::test]
async fn completed_write_with_unauthorized_readback_is_unknown_not_failed() {
    let (url, objects, _, task) = server_with_fault(Some("complete-read-unauthorized")).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let result = client
        .publish_snapshot(
            "20261116031818",
            &artifact("a", "2026-11-16T03:18:18Z"),
            "op",
            &Cancellation::new(),
        )
        .await;
    assert!(objects
        .lock()
        .unwrap()
        .contains_key("/backup/20261116031818/complete.json"));
    assert_eq!(result, Err(WebDavError::OutcomeUnknown));
    task.abort();
}
#[tokio::test]
async fn legacy_payload_must_exist_and_supply_known_size_before_restore_is_enabled() {
    let (url, objects, _, task) = server().await;
    objects.lock().unwrap().insert("/backup/latest".into(),br#"{"version":1,"snapshot_id":"old","object":"attempt-old","operation_token":"oldop","condition":"Absent"}"#.to_vec());
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    let old = page
        .snapshots
        .iter()
        .find(|s| s.id == "legacy:old")
        .unwrap();
    assert_eq!(old.state, "missing_connections");
    assert!(!old.restorable);
    assert_eq!(old.size_bytes, None);
    objects
        .lock()
        .unwrap()
        .insert("/backup/attempt-old".into(), b"legacy-payload".to_vec());
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    let old = page
        .snapshots
        .iter()
        .find(|s| s.id == "legacy:old")
        .unwrap();
    assert_eq!(old.state, "legacy");
    assert!(old.restorable);
    assert_eq!(old.size_bytes, Some(14));
    task.abort();
}

#[tokio::test]
async fn unsupported_manifest_version_remains_visible_without_invented_metadata() {
    let (url, objects, _, task) = server().await;
    objects
        .lock()
        .unwrap()
        .insert("/backup/20261116031818/".into(), Vec::new());
    objects.lock().unwrap().insert(
        "/backup/20261116031818/manifest.json".into(),
        br#"{"formatVersion":999,"appVersion":"future"}"#.to_vec(),
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(page.snapshots.len(), 1);
    let card = &page.snapshots[0];
    assert_eq!(card.state, "unsupported_format");
    assert_eq!(card.format_version, Some(999));
    assert_eq!(card.app_version, None);
    assert_eq!(card.size_bytes, None);
    assert!(!card.restorable);
    assert!(client
        .download_snapshot("20261116031818", &Cancellation::new())
        .await
        .is_err());
    task.abort();
}

#[tokio::test]
async fn missing_connections_package_is_distinct_from_unavailable_permissions() {
    let (url, objects, _, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    client
        .publish_snapshot(
            "20261116031818",
            &artifact("a", "2026-11-16T03:18:18Z"),
            "op",
            &Cancellation::new(),
        )
        .await
        .unwrap();
    objects
        .lock()
        .unwrap()
        .remove("/backup/20261116031818/connections.pkg");
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(page.snapshots[0].state, "missing_connections");
    assert!(!page.snapshots[0].restorable);
    assert_eq!(
        client
            .download_snapshot("20261116031818", &Cancellation::new())
            .await
            .unwrap_err(),
        WebDavError::NotFound
    );
    task.abort();
    let (url, _, _, task) = server_with_fault(Some("package-forbidden")).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    client
        .publish_snapshot(
            "20261116031818",
            &artifact("a", "2026-11-16T03:18:18Z"),
            "op",
            &Cancellation::new(),
        )
        .await
        .unwrap();
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(page.snapshots[0].state, "unavailable");
    assert!(!page.snapshots[0].restorable);
    assert!(!page.snapshots[0].reason.as_ref().unwrap().contains("缺少"));
    task.abort();
}

#[tokio::test]
async fn same_size_corruption_is_structural_only_in_list_and_rejected_on_download() {
    let (url, objects, _, task) = server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    client
        .publish_snapshot(
            "20261116031818",
            &artifact("a", "2026-11-16T03:18:18Z"),
            "op",
            &Cancellation::new(),
        )
        .await
        .unwrap();
    {
        let mut map = objects.lock().unwrap();
        let bytes = map
            .get_mut("/backup/20261116031818/connections.pkg")
            .unwrap();
        bytes[0] ^= 1;
    }
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(page.snapshots[0].state, "structurally_complete");
    assert!(page.snapshots[0]
        .reason
        .as_ref()
        .unwrap()
        .contains("恢复前仍需完整校验"));
    assert_eq!(
        client
            .download_snapshot("20261116031818", &Cancellation::new())
            .await
            .unwrap_err(),
        WebDavError::Integrity
    );
    task.abort();
}

#[tokio::test]
async fn manifest_authentication_error_is_never_reported_as_missing_manifest() {
    let (url, objects, _, task) = server_with_fault(Some("manifest-unauthorized")).await;
    objects
        .lock()
        .unwrap()
        .insert("/backup/20261116031818/".into(), Vec::new());
    objects.lock().unwrap().insert(
        "/backup/20261116031818/manifest.json".into(),
        artifact("a", "2026-11-16T03:18:18Z").manifest,
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let page = client
        .list_snapshots(None, &Cancellation::new())
        .await
        .unwrap();
    assert_eq!(page.snapshots[0].state, "unavailable");
    assert!(!page.snapshots[0].restorable);
    assert_ne!(page.snapshots[0].state, "missing_manifest");
    task.abort();
}

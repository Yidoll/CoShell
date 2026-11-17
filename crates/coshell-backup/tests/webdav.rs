use coshell_backup::webdav;

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use webdav::{Cancellation, PublishCondition, WebDavError};

#[tokio::test]
async fn version_pages_preserve_timestamped_history_and_download_selected_older_page() {
    use sha2::{Digest, Sha256};
    let (url, state, task) = stateful_server().await;
    let mut previous = None;
    let mut head = Vec::new();
    {
        let mut objects = state.lock().unwrap();
        for index in 0..51 {
            let object = format!("20261116{index:06}/attempt-page-{index}");
            let bytes = format!("payload-{index}").into_bytes();
            objects.insert(format!("/backup/{object}"), (bytes, 1));
            head=serde_json::to_vec(&serde_json::json!({"version":2,"snapshot_id":format!("v-{index}"),"object":object,"operation_token":format!("op-{index}"),"condition":"Absent","previous":previous})).unwrap();
            let history = format!("history-{:x}", Sha256::digest(&head));
            objects.insert(format!("/backup/{history}"), (head.clone(), 1));
            previous = Some(history);
        }
        objects.insert("/backup/latest".into(), (head, 1));
    }
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(10)).unwrap();
    let cancel = Cancellation::new();
    let first = client.list_versions_page(None, &cancel).await.unwrap();
    assert_eq!(first.versions.len(), 50);
    assert_eq!(first.versions[0].id, "v-50");
    assert!(first.versions[0].latest);
    let second = client
        .list_versions_page(first.next_cursor.as_deref(), &cancel)
        .await
        .unwrap();
    assert_eq!(second.versions.len(), 1);
    assert_eq!(second.versions[0].id, "v-0");
    assert!(!second.versions[0].latest);
    assert!(second.next_cursor.is_none());
    assert_eq!(
        client
            .download_version(&second.versions[0].id, &cancel)
            .await
            .unwrap(),
        b"payload-0"
    );
    task.abort();
}

#[tokio::test]
async fn version_history_lists_only_published_snapshots_and_restores_selection() {
    let (url, state, task) = stateful_server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let cancel = Cancellation::new();
    let first = webdav::PublicationAttempt::new_timestamped(
        "op-a",
        &url,
        "v-a",
        b"old",
        PublishCondition::Absent,
        "20261112000000",
    )
    .unwrap();
    client
        .publish_tracked("v-a", b"old", PublishCondition::Absent, &first, &cancel)
        .await
        .unwrap();
    let etag = client.index_etag(&cancel).await.unwrap().unwrap();
    let condition = PublishCondition::Match(etag);
    let second = webdav::PublicationAttempt::new_timestamped(
        "op-b",
        &url,
        "v-b",
        b"new",
        condition.clone(),
        "20261112000001",
    )
    .unwrap();
    client
        .publish_tracked("v-b", b"new", condition, &second, &cancel)
        .await
        .unwrap();
    state.lock().unwrap().insert(
        "/backup/20261112000002/attempt-orphan".into(),
        (b"incomplete".to_vec(), 1),
    );
    let versions = client.list_versions(&cancel).await.unwrap();
    assert_eq!(
        versions.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        ["v-b", "v-a"]
    );
    assert_eq!(versions[0].timestamp.as_deref(), Some("20261112000001"));
    assert!(versions[0].latest);
    assert_eq!(
        client.download_version("v-a", &cancel).await.unwrap(),
        b"old"
    );
    assert!(client.download_version("orphan", &cancel).await.is_err());
    task.abort();
}

#[tokio::test]
async fn history_remains_readable_after_upgrading_a_legacy_latest_index() {
    let (url, state, task) = stateful_server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let cancel = Cancellation::new();
    let old = webdav::PublicationAttempt::new(
        "old-op",
        &url,
        "old",
        b"old-data",
        PublishCondition::Absent,
    )
    .unwrap();
    client
        .publish_tracked("old", b"old-data", PublishCondition::Absent, &old, &cancel)
        .await
        .unwrap();
    // Emulate a legacy v1 published index, which has no predecessor field.
    {
        let mut s = state.lock().unwrap();
        let entry = s.get_mut("/backup/latest").unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&entry.0).unwrap();
        value["version"] = 1.into();
        value.as_object_mut().unwrap().remove("previous");
        entry.0 = serde_json::to_vec(&value).unwrap();
    }
    let etag = client.index_etag(&cancel).await.unwrap().unwrap();
    let condition = PublishCondition::Match(etag);
    let new = webdav::PublicationAttempt::new_timestamped(
        "new-op",
        &url,
        "new",
        b"new-data",
        condition.clone(),
        "20261112000001",
    )
    .unwrap();
    client
        .publish_tracked("new", b"new-data", condition, &new, &cancel)
        .await
        .unwrap();
    let versions = client.list_versions(&cancel).await.unwrap();
    assert_eq!(
        versions.iter().map(|v| v.id.as_str()).collect::<Vec<_>>(),
        ["new", "old"]
    );
    assert_eq!(versions[1].timestamp, None);
    assert_eq!(
        client.download_version("old", &cancel).await.unwrap(),
        b"old-data"
    );
    task.abort();
}

#[tokio::test]
async fn creates_nested_collections_and_timestamped_publication() {
    let (url, state, task) = stateful_server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    client
        .ensure_directory("one/two", &Cancellation::new())
        .await
        .unwrap();
    let nested_url = format!("{url}one/two/");
    let nested = WebDav::new(&nested_url, "u", "p", Duration::from_secs(3)).unwrap();
    let attempt = webdav::PublicationAttempt::new_timestamped(
        "op",
        &nested_url,
        "s1",
        b"cipher",
        PublishCondition::Absent,
        "20261112123456",
    )
    .unwrap();
    nested
        .publish_tracked(
            "s1",
            b"cipher",
            PublishCondition::Absent,
            &attempt,
            &Cancellation::new(),
        )
        .await
        .unwrap();
    assert!(state
        .lock()
        .unwrap()
        .keys()
        .any(|p| p.starts_with("/backup/one/two/20261112123456/attempt-")));
    assert_eq!(
        nested.download_latest(&Cancellation::new()).await.unwrap(),
        b"cipher"
    );
    task.abort();
}

#[tokio::test]
async fn existing_path_requires_verified_collection_response() {
    const METHOD: &str =
        "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const BAD: &str = "HTTP/1.1 207 Multi-Status\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    let (url, _, task) = server(vec![METHOD, BAD]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(2)).unwrap();
    assert_eq!(
        client
            .ensure_directory("folder", &Cancellation::new())
            .await,
        Err(WebDavError::Unsupported)
    );
    task.abort();
}

#[tokio::test]
async fn existing_directory_with_matching_dav_response_is_accepted() {
    const METHOD: &str =
        "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
    const VALID: &str = "HTTP/1.1 207 Multi-Status\r\nConnection: close\r\n\r\n<d:multistatus xmlns:d=\"DAV:\"><d:response><d:href>/backup/folder/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>";
    let (url, _, task) = server(vec![METHOD, VALID]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(2)).unwrap();
    assert_eq!(
        client
            .ensure_directory("folder", &Cancellation::new())
            .await,
        Ok(())
    );
    task.abort();
}

#[tokio::test]
async fn timestamp_collision_does_not_overwrite_first_backup() {
    let (url, state, task) = stateful_server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let first = webdav::PublicationAttempt::new_timestamped(
        "op1",
        &url,
        "s1",
        b"first",
        PublishCondition::Absent,
        "20261112123456",
    )
    .unwrap();
    client
        .publish_tracked(
            "s1",
            b"first",
            PublishCondition::Absent,
            &first,
            &Cancellation::new(),
        )
        .await
        .unwrap();
    let etag = client
        .index_etag(&Cancellation::new())
        .await
        .unwrap()
        .unwrap();
    let second = webdav::PublicationAttempt::new_timestamped(
        "op2",
        &url,
        "s2",
        b"second",
        PublishCondition::Match(etag),
        "20261112123456",
    )
    .unwrap();
    assert_eq!(
        client
            .publish_tracked(
                "s2",
                b"second",
                PublishCondition::Match(
                    client
                        .index_etag(&Cancellation::new())
                        .await
                        .unwrap()
                        .unwrap()
                ),
                &second,
                &Cancellation::new()
            )
            .await
            .unwrap_err()
            .error,
        WebDavError::OutcomeUnknown
    );
    assert_eq!(
        client.download_latest(&Cancellation::new()).await.unwrap(),
        b"first"
    );
    assert_eq!(
        state
            .lock()
            .unwrap()
            .keys()
            .filter(|p| p.starts_with("/backup/20261112123456/attempt-"))
            .count(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn concurrent_publications_in_same_second_never_share_directory() {
    let (url, state, task) = stateful_server().await;
    let first = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let second = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let a = webdav::PublicationAttempt::new_timestamped(
        "op1",
        &url,
        "s1",
        b"first",
        PublishCondition::Absent,
        "20261112123456",
    )
    .unwrap();
    let b = webdav::PublicationAttempt::new_timestamped(
        "op2",
        &url,
        "s2",
        b"second",
        PublishCondition::Absent,
        "20261112123456",
    )
    .unwrap();
    let ca = Cancellation::new();
    let cb = Cancellation::new();
    let (ra, rb) = tokio::join!(
        first.publish_tracked("s1", b"first", PublishCondition::Absent, &a, &ca),
        second.publish_tracked("s2", b"second", PublishCondition::Absent, &b, &cb)
    );
    assert_eq!(usize::from(ra.is_ok()) + usize::from(rb.is_ok()), 1);
    assert_eq!(
        state
            .lock()
            .unwrap()
            .keys()
            .filter(|p| p.starts_with("/backup/20261112123456/attempt-"))
            .count(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn connection_check_reads_collection_without_writes_and_classifies_errors() {
    for (response, expected) in [
        (
            "HTTP/1.1 207 Multi-Status\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Ok(()),
        ),
        (
            "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Err(WebDavError::Unsupported),
        ),
        (
            "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Err(WebDavError::Authentication),
        ),
        (
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Err(WebDavError::NotFound),
        ),
        (
            "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            Err(WebDavError::Unsupported),
        ),
    ] {
        let (url, requests, task) = server(vec![response]).await;
        let client = webdav::WebDav::new(&url, "u", "p", Duration::from_secs(2)).unwrap();
        assert_eq!(
            client.check_connection(&Cancellation::new()).await,
            expected
        );
        let captured = requests.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(captured[0].starts_with("PROPFIND /backup/ HTTP/1.1"));
        assert!(captured[0].to_ascii_lowercase().contains("depth: 0"));
        drop(captured);
        task.abort();
    }
}

#[tokio::test]
async fn download_reads_only_the_published_immutable_object() {
    let (url, state, task) = stateful_server().await;
    state.lock().unwrap().insert("/backup/latest".into(), (br#"{"version":1,"snapshot_id":"s1","object":"attempt-123","operation_token":"op","condition":"Absent"}"#.to_vec(), 1));
    state
        .lock()
        .unwrap()
        .insert("/backup/attempt-123".into(), (b"ciphertext".to_vec(), 1));
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    assert_eq!(
        client.download_latest(&Cancellation::new()).await.unwrap(),
        b"ciphertext"
    );
    state.lock().unwrap().insert("/backup/latest".into(), (br#"{"version":1,"snapshot_id":"s1","object":"../outside","operation_token":"op","condition":"Absent"}"#.to_vec(), 2));
    assert_eq!(
        client
            .download_latest(&Cancellation::new())
            .await
            .unwrap_err(),
        WebDavError::Integrity
    );
    task.abort();
}

// Test adapter drives only the public, tracked production API.
struct WebDav {
    inner: webdav::WebDav,
    url: String,
}
impl WebDav {
    fn new(
        url: &str,
        username: &str,
        password: &str,
        timeout: Duration,
    ) -> Result<Self, WebDavError> {
        Ok(Self {
            inner: webdav::WebDav::new(url, username, password, timeout)?,
            url: url.into(),
        })
    }
    async fn probe(&self, cancel: &Cancellation) -> Result<webdav::Capabilities, WebDavError> {
        let attempt = webdav::PublicationAttempt::new(
            "probe",
            &self.url,
            "probe",
            b"",
            PublishCondition::Absent,
        )?;
        self.inner
            .probe_tracked(&attempt, cancel)
            .await
            .map_err(|e| e.error)
    }
    async fn publish(
        &self,
        id: &str,
        bytes: &[u8],
        condition: PublishCondition,
        _: bool,
        cancel: &Cancellation,
    ) -> Result<webdav::PublishMode, WebDavError> {
        let attempt = webdav::PublicationAttempt::new(id, &self.url, id, bytes, condition.clone())?;
        self.inner
            .publish_tracked(id, bytes, condition, &attempt, cancel)
            .await
            .map(|r| r.mode)
            .map_err(|e| e.error)
    }
}
impl std::ops::Deref for WebDav {
    type Target = webdav::WebDav;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[tokio::test]
async fn tracked_publication_returns_cleanup_pending_and_preserves_operation_token() {
    let (url, _, task) = stateful_server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let attempt = webdav::PublicationAttempt::new(
        "operation-42",
        &url,
        "snapshot",
        b"encrypted",
        PublishCondition::Absent,
    )
    .unwrap();
    let cancel = Cancellation::new();
    cancel.cancel();
    let failure = client
        .publish_tracked(
            "snapshot",
            b"encrypted",
            PublishCondition::Absent,
            &attempt,
            &cancel,
        )
        .await
        .unwrap_err();
    assert_eq!(failure.operation_token, "operation-42");
    assert_eq!(failure.attempt_id, attempt.attempt_id());
    assert!(failure.cleanup_pending);
    assert_eq!(failure.error, WebDavError::Cancelled);
    task.abort();
}

#[tokio::test]
async fn tracked_attempt_rejects_different_target_snapshot_or_payload_before_network() {
    let (url, seen, task) = server(vec![]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    let attempt =
        webdav::PublicationAttempt::new("op", &url, "s1", b"one", PublishCondition::Absent)
            .unwrap();
    let fail = client
        .publish_tracked(
            "s1",
            b"two",
            PublishCondition::Absent,
            &attempt,
            &Cancellation::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(fail.error, WebDavError::InvalidInput);
    assert!(!fail.cleanup_pending);
    assert!(seen.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn tracked_attempt_rejects_changed_condition_and_reports_success_tracking() {
    let (url, state, task) = stateful_server().await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let attempt =
        webdav::PublicationAttempt::new("op", &url, "s1", b"one", PublishCondition::Absent)
            .unwrap();
    let failure = client
        .publish_tracked(
            "s1",
            b"one",
            PublishCondition::Match("\"x\"".into()),
            &attempt,
            &Cancellation::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error, WebDavError::InvalidInput);
    assert!(state.lock().unwrap().is_empty());
    let success = client
        .publish_tracked(
            "s1",
            b"one",
            PublishCondition::Absent,
            &attempt,
            &Cancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(success.attempt_id, attempt.attempt_id());
    assert_eq!(success.operation_token, "op");
    assert!(success.cleanup_pending);
    task.abort();
}

#[tokio::test]
async fn reused_existing_snapshot_still_returns_callers_attempt_for_cleanup() {
    let (url, state, task) = stateful_server().await;
    state
        .lock()
        .unwrap()
        .insert("/backup/attempt-prev".into(), (b"one".to_vec(), 1));
    state.lock().unwrap().insert(
        "/backup/latest".into(),
        (
            br#"{"version":1,"snapshot_id":"s1","object":"attempt-prev","operation_token":"op","condition":"Absent"}"#.to_vec(),
            1,
        ),
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let attempt =
        webdav::PublicationAttempt::new("op", &url, "s1", b"one", PublishCondition::Absent)
            .unwrap();
    let success = client
        .publish_tracked(
            "s1",
            b"one",
            PublishCondition::Absent,
            &attempt,
            &Cancellation::new(),
        )
        .await
        .unwrap();
    assert_eq!(success.attempt_id, attempt.attempt_id());
    assert!(success.cleanup_pending);
    assert!(success
        .possible_objects
        .contains(&attempt.attempt_id().to_owned()));
    task.abort();
}

#[tokio::test]
async fn new_operation_cannot_skip_stale_condition_for_same_payload() {
    let (url, state, task) = stateful_server().await;
    state
        .lock()
        .unwrap()
        .insert("/backup/attempt-prev".into(), (b"one".to_vec(), 1));
    state.lock().unwrap().insert("/backup/latest".into(), (br#"{"version":1,"snapshot_id":"s1","object":"attempt-prev","operation_token":"old-op","condition":"Absent"}"#.to_vec(),2));
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let condition = PublishCondition::Match("\"1\"".into());
    let attempt =
        webdav::PublicationAttempt::new("new-op", &url, "s1", b"one", condition.clone()).unwrap();
    assert!(client
        .publish_tracked("s1", b"one", condition, &attempt, &Cancellation::new())
        .await
        .is_err());
    task.abort();
}

#[tokio::test]
async fn matching_token_with_different_original_condition_cannot_shortcut() {
    let (url, state, task) = stateful_server().await;
    state
        .lock()
        .unwrap()
        .insert("/backup/attempt-prev".into(), (b"one".to_vec(), 1));
    state.lock().unwrap().insert("/backup/latest".into(),(br#"{"version":1,"snapshot_id":"s1","object":"attempt-prev","operation_token":"op","condition":"Absent"}"#.to_vec(),2));
    let condition = PublishCondition::Match("\"1\"".into());
    let attempt =
        webdav::PublicationAttempt::new("op", &url, "s1", b"one", condition.clone()).unwrap();
    let client = webdav::WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    assert_eq!(
        client
            .publish_tracked("s1", b"one", condition, &attempt, &Cancellation::new())
            .await
            .unwrap_err()
            .error,
        WebDavError::Conflict
    );
    assert_eq!(state.lock().unwrap()["/backup/latest"].1, 2);
    task.abort();
}

#[tokio::test]
async fn token_cannot_be_reused_for_different_snapshot_after_latest_changes() {
    let (url, state, task) = stateful_server().await;
    let client = webdav::WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let first = webdav::PublicationAttempt::new("op", &url, "s1", b"one", PublishCondition::Absent)
        .unwrap();
    client
        .publish_tracked(
            "s1",
            b"one",
            PublishCondition::Absent,
            &first,
            &Cancellation::new(),
        )
        .await
        .unwrap();
    state.lock().unwrap().insert("/backup/latest".into(), (br#"{"version":1,"snapshot_id":"later","object":"attempt-prev","operation_token":"later","condition":"Absent"}"#.to_vec(),2));
    let condition = PublishCondition::Match("\"2\"".into());
    let reuse =
        webdav::PublicationAttempt::new("op", &url, "s2", b"two", condition.clone()).unwrap();
    assert_eq!(
        client
            .publish_tracked("s2", b"two", condition, &reuse, &Cancellation::new())
            .await
            .unwrap_err()
            .error,
        WebDavError::Conflict
    );
    task.abort();
}

#[test]
fn oversized_etag_is_rejected_before_attempt_creation() {
    let condition = PublishCondition::Match(format!("\"{}\"", "a".repeat(2048)));
    assert!(
        webdav::PublicationAttempt::new("op", "https://example.org/", "s", b"x", condition)
            .is_err()
    );
}

async fn server(
    responses: Vec<&'static str>,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorded = seen.clone();
    let task = tokio::spawn(async move {
        let mut responses = responses.into_iter();
        let mut receipts = std::collections::HashMap::<String, Vec<u8>>::new();
        while let Some(response) = responses.next() {
            let mut response = Some(response);
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = vec![0; 8192];
                let n = socket.read(&mut request).await.unwrap();
                request.truncate(n);
                let header_end = request.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let mut words = headers.lines().next().unwrap().split_whitespace();
                let method = words.next().unwrap().to_owned();
                let path = words.next().unwrap().to_owned();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                while request.len() < header_end + 4 + length {
                    let mut extra = [0; 8192];
                    let count = socket.read(&mut extra).await.unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&extra[..count]);
                }
                if path.starts_with("/backup/receipt-") {
                    if method == "PUT" {
                        if receipts.contains_key(&path) {
                            socket.write_all(CONFLICT.as_bytes()).await.unwrap();
                        } else {
                            receipts.insert(path, request[header_end + 4..].to_vec());
                            socket.write_all(CREATED.as_bytes()).await.unwrap();
                        }
                    } else if let Some(body) = receipts.get(&path) {
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        socket.write_all(response.as_bytes()).await.unwrap();
                        socket.write_all(body).await.unwrap();
                    } else {
                        socket.write_all(NOT_FOUND.as_bytes()).await.unwrap();
                    }
                    continue;
                }
                recorded
                    .lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).into_owned());
                let response = response.take().unwrap();
                if response == "STALL" {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    break;
                }
                socket.write_all(response.as_bytes()).await.unwrap();
                break;
            }
        }
    });
    (format!("http://{address}/backup/"), seen, task)
}
const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const CREATED: &str = "HTTP/1.1 201 Created\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const ENCRYPTED: &str =
    "HTTP/1.1 200 OK\r\nContent-Length: 9\r\nConnection: close\r\n\r\nencrypted";
const CONFLICT: &str =
    "HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const PROBE: &str = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nprobe";
const PROBE_CHUNKED: &str = "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\nprobe\r\n0\r\n\r\n";
const OPTIONS_MOVE: &str = "HTTP/1.1 200 OK\r\nAllow: GET, PUT, DELETE, MOVE\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const HEAD_ETAG: &str =
    "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const NOT_FOUND: &str = "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
fn verified_probe(moves: bool) -> Vec<&'static str> {
    vec![
        if moves { OPTIONS_MOVE } else { OK },
        CREATED,
        CONFLICT,
        CREATED,
        CONFLICT,
        CONFLICT,
        PROBE,
        HEAD_ETAG,
        OK,
        CONFLICT,
    ]
}

/// Stateful server: conditional PUT is atomic; MOVE deliberately ignores
/// Overwrite:F so tests can detect an unsafe client-side MOVE assumption.
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
                        "MKCOL" => {
                            if map.contains_key(&path) {
                                status = 405;
                            } else {
                                map.insert(path.clone(), (Vec::new(), 1));
                                status = 201;
                            }
                        }
                        "PROPFIND" => {
                            status = if map.contains_key(&path) { 207 } else { 404 };
                        }
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
async fn receipt_sentinel_must_actually_exist_after_success() {
    let (url, state, task) = stateful_server_with_fault(3, 0, false).await;
    let client = webdav::WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let attempt =
        webdav::PublicationAttempt::new("op", &url, "s", b"one", PublishCondition::Absent).unwrap();
    assert!(client
        .publish_tracked(
            "s",
            b"one",
            PublishCondition::Absent,
            &attempt,
            &Cancellation::new()
        )
        .await
        .is_err());
    assert!(!state.lock().unwrap().contains_key("/backup/latest"));
    assert_eq!(
        state
            .lock()
            .unwrap()
            .keys()
            .filter(|p| p.starts_with("/backup/receipt-"))
            .count(),
        1
    );
    task.abort();
}

#[tokio::test]
async fn receipt_namespace_that_ignores_conditional_create_fails_closed() {
    let (url, state, task) = stateful_server_with_fault(2, 0, false).await;
    let client = webdav::WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let attempt =
        webdav::PublicationAttempt::new("receipt-op", &url, "s1", b"one", PublishCondition::Absent)
            .unwrap();
    let result = client
        .publish_tracked(
            "s1",
            b"one",
            PublishCondition::Absent,
            &attempt,
            &Cancellation::new(),
        )
        .await;
    assert_eq!(
        result.unwrap_err().error,
        WebDavError::UnsupportedAtomicCreate
    );
    assert!(!state.lock().unwrap().contains_key("/backup/latest"));
    task.abort();
}

#[tokio::test]
async fn committed_index_with_error_or_drop_is_confirmed_by_same_operation() {
    for failure in [500, 429, 1] {
        let (url, state, task) = stateful_server_with_fault(0, failure, false).await;
        let client = webdav::WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
        let attempt = webdav::PublicationAttempt::new(
            "op-1",
            &url,
            "snapshot",
            b"encrypted",
            PublishCondition::Absent,
        )
        .unwrap();
        let success = client
            .publish_tracked(
                "snapshot",
                b"encrypted",
                PublishCondition::Absent,
                &attempt,
                &Cancellation::new(),
            )
            .await
            .unwrap();
        assert_eq!(success.operation_token, "op-1");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&state.lock().unwrap()["/backup/latest"].0)
                .unwrap()["operation_token"],
            "op-1"
        );
        task.abort();
    }
}

#[tokio::test]
async fn committed_then_superseded_index_returns_unknown() {
    let (url, _, task) = stateful_server_with_fault(0, 500, true).await;
    let client = webdav::WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    let attempt = webdav::PublicationAttempt::new(
        "op-1",
        &url,
        "snapshot",
        b"encrypted",
        PublishCondition::Absent,
    )
    .unwrap();
    let failure = client
        .publish_tracked(
            "snapshot",
            b"encrypted",
            PublishCondition::Absent,
            &attempt,
            &Cancellation::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(failure.error, WebDavError::OutcomeUnknown);
    task.abort();
}

#[tokio::test]
async fn retry_existing_identical_object_finishes_index_update() {
    let (url, state, task) = stateful_server().await;
    state
        .lock()
        .unwrap()
        .insert("/backup/snapshot-a".into(), (b"encrypted".to_vec(), 1));
    state.lock().unwrap().insert(
        "/backup/latest".into(),
        (
            br#"{"version":1,"snapshot_id":"old","object":"attempt-old","operation_token":"old","condition":"Absent"}"#.to_vec(),
            1,
        ),
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    client
        .publish(
            "snapshot-a",
            b"encrypted",
            PublishCondition::Match("\"1\"".into()),
            false,
            &Cancellation::new(),
        )
        .await
        .unwrap();
    let map = state.lock().unwrap();
    let index: serde_json::Value = serde_json::from_slice(&map["/backup/latest"].0).unwrap();
    assert_eq!(index["snapshot_id"], "snapshot-a");
    assert_eq!(
        map[&format!("/backup/{}", index["object"].as_str().unwrap())].0,
        b"encrypted"
    );
    task.abort();
}

#[tokio::test]
async fn retry_after_already_committed_index_reports_success_with_stateful_server() {
    let (url, state, task) = stateful_server().await;
    state.lock().unwrap().insert(
        "/backup/attempt-previous".into(),
        (b"encrypted".to_vec(), 1),
    );
    state.lock().unwrap().insert(
        "/backup/latest".into(),
        (
            br#"{"version":1,"snapshot_id":"snapshot-a","object":"attempt-previous","operation_token":"snapshot-a","condition":{"Match":"\"1\""}}"#.to_vec(),
            2,
        ),
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    assert_eq!(
        client
            .publish(
                "snapshot-a",
                b"encrypted",
                PublishCondition::Match("\"1\"".into()),
                false,
                &Cancellation::new()
            )
            .await
            .unwrap(),
        webdav::PublishMode::ImmutableFallback
    );
    assert_eq!(state.lock().unwrap()["/backup/latest"].1, 2);
    task.abort();
}

#[tokio::test]
async fn move_requested_cannot_replace_existing_snapshot() {
    let (url, state, task) = stateful_server().await;
    state
        .lock()
        .unwrap()
        .insert("/backup/attempt-old".into(), (b"old".to_vec(), 1));
    state.lock().unwrap().insert(
        "/backup/latest".into(),
        (
            br#"{"version":1,"snapshot_id":"snapshot-a","object":"attempt-old","operation_token":"snapshot-a","condition":"Absent"}"#.to_vec(),
            1,
        ),
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    assert!(client
        .publish(
            "snapshot-a",
            b"new",
            PublishCondition::Absent,
            true,
            &Cancellation::new()
        )
        .await
        .is_err());
    assert_eq!(state.lock().unwrap()["/backup/attempt-old"].0, b"old");
    task.abort();
}

#[test]
fn rejects_insecure_remote_and_embedded_credentials() {
    assert!(WebDav::new("http://example.com/", "u", "p", Duration::from_secs(1)).is_err());
    assert!(WebDav::new("http://localhost/", "u", "p", Duration::from_secs(1)).is_err());
    assert!(WebDav::new("https://u:p@example.com/", "u", "p", Duration::from_secs(1)).is_err());
}

#[tokio::test]
async fn reserved_index_name_cannot_be_published_as_snapshot() {
    let (url, seen, task) = server(vec![CREATED]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client
            .publish(
                "latest",
                b"encrypted",
                PublishCondition::Absent,
                false,
                &Cancellation::new()
            )
            .await
            .unwrap_err(),
        WebDavError::InvalidInput
    );
    assert!(seen.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn probe_rejects_server_that_ignores_if_none_match_and_cleans_test_object() {
    let (url, seen, task) = server(vec![OK, CREATED, CONFLICT, CREATED, OK]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client.probe(&Cancellation::new()).await.unwrap_err(),
        WebDavError::UnsupportedAtomicCreate
    );
    task.await.unwrap();
    assert!(!seen
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.starts_with("DELETE ")));
}

#[tokio::test]
async fn probe_accepts_chunked_read_with_valid_body() {
    let mut responses = verified_probe(false);
    responses[6] = PROBE_CHUNKED;
    let (url, _, task) = server(responses).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert!(
        !client
            .probe(&Cancellation::new())
            .await
            .unwrap()
            .supports_move
    );
    task.await.unwrap();
}

#[tokio::test]
async fn rejected_probe_create_does_not_delete_unowned_object() {
    let (url, seen, task) = server(vec![OK, CONFLICT, CONFLICT]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client.probe(&Cancellation::new()).await.unwrap_err(),
        WebDavError::UnsupportedAtomicCreate
    );
    task.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn cancelled_probe_preserves_cancelled_if_cleanup_object_is_already_absent() {
    let (url, seen, task) = server(vec![OK, CREATED, CONFLICT, CREATED, "STALL"]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(2)).unwrap();
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let observed = seen.clone();
    let trigger = tokio::spawn(async move {
        loop {
            if observed.lock().unwrap().len() >= 5 {
                signal.cancel();
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    assert_eq!(
        client.probe(&cancel).await.unwrap_err(),
        WebDavError::Cancelled
    );
    trigger.await.unwrap();
    task.await.unwrap();
    assert!(!seen
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.starts_with("DELETE ")));
}

#[tokio::test]
async fn probe_rejects_two_successful_concurrent_cas_writes() {
    let (url, seen, task) = server(vec![
        OK, CREATED, CONFLICT, CREATED, CONFLICT, CONFLICT, PROBE, HEAD_ETAG, OK, OK,
    ])
    .await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client.probe(&Cancellation::new()).await.unwrap_err(),
        WebDavError::UnsupportedAtomicUpdate
    );
    task.await.unwrap();
    assert!(!seen
        .lock()
        .unwrap()
        .iter()
        .any(|r| r.starts_with("DELETE ")));
}

#[tokio::test]
async fn probe_rejects_non_atomic_concurrent_create() {
    let (url, state, task) = stateful_server_with_broken_create(true).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client.probe(&Cancellation::new()).await.unwrap_err(),
        WebDavError::UnsupportedAtomicCreate
    );
    assert!(state.lock().unwrap().keys().any(|p| p.contains("-create")));
    task.abort();
}

#[tokio::test]
async fn truncated_previous_attempt_is_never_published_or_overwritten_on_retry() {
    let (url, state, task) = stateful_server().await;
    state
        .lock()
        .unwrap()
        .insert("/backup/attempt-truncated".into(), (b"trunc".to_vec(), 1));
    state.lock().unwrap().insert(
        "/backup/latest".into(),
        (
            br#"{"version":1,"snapshot_id":"old","object":"attempt-old","operation_token":"old","condition":"Absent"}"#.to_vec(),
            1,
        ),
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(3)).unwrap();
    client
        .publish(
            "same-id",
            b"complete",
            PublishCondition::Match("\"1\"".into()),
            false,
            &Cancellation::new(),
        )
        .await
        .unwrap();
    let map = state.lock().unwrap();
    assert_eq!(map["/backup/attempt-truncated"].0, b"trunc");
    let index: serde_json::Value = serde_json::from_slice(&map["/backup/latest"].0).unwrap();
    assert_eq!(index["snapshot_id"], "same-id");
    assert_ne!(index["object"], "attempt-truncated");
    assert_eq!(
        map[&format!("/backup/{}", index["object"].as_str().unwrap())].0,
        b"complete"
    );
    task.abort();
}

#[tokio::test]
async fn interrupted_upload_does_not_update_index() {
    let mut responses = verified_probe(true);
    responses.push(NOT_FOUND);
    responses.push(""); // close the socket while PUT is in progress
    responses.push(NOT_FOUND);
    let (url, seen, task) = server(responses).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client
            .publish(
                "snapshot-interrupted",
                b"encrypted",
                PublishCondition::Absent,
                true,
                &Cancellation::new()
            )
            .await
            .unwrap_err(),
        WebDavError::OutcomeUnknown
    );
    task.await.unwrap();
    let requests = seen.lock().unwrap();
    assert!(!requests.iter().any(|r| r.starts_with("PUT /backup/latest")));
}

#[tokio::test]
async fn cancellation_during_upload_stops_before_index() {
    let mut responses = verified_probe(true);
    responses.extend([NOT_FOUND, "STALL"]);
    let (url, seen, task) = server(responses).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(2)).unwrap();
    let cancel = Cancellation::new();
    let signal = cancel.clone();
    let observed = seen.clone();
    let cancel_task = tokio::spawn(async move {
        loop {
            if observed
                .lock()
                .unwrap()
                .iter()
                .any(|r| r.starts_with("PUT /backup/attempt-"))
            {
                signal.cancel();
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    assert_eq!(
        client
            .publish(
                "snapshot-cancel",
                b"encrypted",
                PublishCondition::Absent,
                true,
                &cancel
            )
            .await
            .unwrap_err(),
        WebDavError::OutcomeUnknown
    );
    cancel_task.await.unwrap();
    task.await.unwrap();
    let requests = seen.lock().unwrap();
    assert!(!requests.iter().any(|r| r.starts_with("PUT /backup/latest")));
}

#[tokio::test]
async fn detects_unauthorized_without_retrying() {
    let (url, seen, task) = server(vec![
        "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ])
    .await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client.probe(&Cancellation::new()).await.unwrap_err(),
        WebDavError::Authentication
    );
    task.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn publishes_immutable_snapshot_then_conditional_index_and_reports_conflict() {
    let mut responses = verified_probe(false);
    responses.extend([NOT_FOUND, CREATED, ENCRYPTED, CONFLICT, NOT_FOUND]);
    let (url, seen, task) = server(responses).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    let error = client
        .publish(
            "snapshot-1",
            b"encrypted",
            PublishCondition::Match("\"old\"".into()),
            false,
            &Cancellation::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, WebDavError::OutcomeUnknown);
    task.await.unwrap();
    let all = seen.lock().unwrap().join("\n");
    assert!(all.to_ascii_lowercase().contains("if-match: \"old\""));
    assert!(all.to_ascii_lowercase().contains("if-none-match: *"));
}

#[tokio::test]
async fn cancellation_before_start_sends_no_request() {
    let (url, seen, task) = server(vec![OK]).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    let cancel = Cancellation::new();
    cancel.cancel();
    assert_eq!(
        client.probe(&cancel).await.unwrap_err(),
        WebDavError::Cancelled
    );
    assert!(seen.lock().unwrap().is_empty());
    task.abort();
}

#[tokio::test]
async fn no_move_server_uses_immutable_fallback_and_absent_index() {
    let mut responses = verified_probe(false);
    responses.extend(verified_probe(false));
    responses.extend([NOT_FOUND, CREATED, ENCRYPTED, CREATED]);
    let (url, seen, task) = server(responses).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert!(
        !client
            .probe(&Cancellation::new())
            .await
            .unwrap()
            .supports_move
    );
    assert_eq!(
        client
            .publish(
                "snapshot-2",
                b"encrypted",
                PublishCondition::Absent,
                false,
                &Cancellation::new()
            )
            .await
            .unwrap(),
        webdav::PublishMode::ImmutableFallback
    );
    task.await.unwrap();
    assert_eq!(seen.lock().unwrap().len(), 24);
}

#[tokio::test]
async fn requested_move_safely_falls_back_without_overwriting_index() {
    let mut responses = verified_probe(true);
    responses.extend([NOT_FOUND, CREATED, ENCRYPTED, CONFLICT, NOT_FOUND]);
    let (url, seen, task) = server(responses).await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client
            .publish(
                "snapshot-3",
                b"encrypted",
                PublishCondition::Absent,
                true,
                &Cancellation::new()
            )
            .await
            .unwrap_err(),
        WebDavError::OutcomeUnknown
    );
    task.await.unwrap();
    let requests = seen.lock().unwrap();
    assert!(!requests.iter().any(|r| r.starts_with("MOVE ")));
}

#[tokio::test]
async fn reads_strong_index_etag_and_classifies_retry() {
    let (url, _, task) = server(vec![
        "HTTP/1.1 200 OK\r\nETag: \"v1\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
    ])
    .await;
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client
            .index_etag(&Cancellation::new())
            .await
            .unwrap()
            .as_deref(),
        Some("\"v1\"")
    );
    assert!(WebDavError::Timeout.retryable());
    assert!(!WebDavError::Authentication.retryable());
    task.await.unwrap();
}

#[tokio::test]
async fn retry_after_lost_response_recovers_only_when_object_and_index_match() {
    let (url, state, task) = stateful_server().await;
    state
        .lock()
        .unwrap()
        .insert("/backup/attempt-prev".into(), (b"encrypted".to_vec(), 1));
    state.lock().unwrap().insert(
        "/backup/latest".into(),
        (
            br#"{"version":1,"snapshot_id":"snapshot-1","object":"attempt-prev","operation_token":"snapshot-1","condition":"Absent"}"#.to_vec(),
            1,
        ),
    );
    let client = WebDav::new(&url, "u", "p", Duration::from_secs(1)).unwrap();
    assert_eq!(
        client
            .publish(
                "snapshot-1",
                b"encrypted",
                PublishCondition::Absent,
                false,
                &Cancellation::new()
            )
            .await
            .unwrap(),
        webdav::PublishMode::ImmutableFallback
    );
    task.abort();
}

//! Local desktop connection and command submission boundary.
use crate::{
    command_intent::{Admission, CommandGate, LiveConnection},
    connections::ConnectionService,
    feature_flags::{Feature, FeatureFlags},
    host_key::{HostKeyPrompt, SqliteHostKeyStore, TofuVerifier},
};
pub use coshell_backup::webdav::{BackupVersion, BackupVersionPage, SnapshotPage};
use coshell_drivers::ssh::{
    run_cancellable, Authentication, Cancellation, ConnectionRequest, HostKey, HostKeyDecision,
    RusshConnector, SshConnection, SshConnector, SshError, SshFuture, TerminalChannel,
    TerminalSize,
};
use coshell_platform::secret_store::Secret;
use coshell_storage::{ConnectionRecord, GroupRecord};
use coshell_types::CommandIntent;
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::sync::Mutex as AsyncMutex;
#[cfg(test)]
mod directory_tests;
mod passwords;
struct RuntimeSecretStore(Arc<dyn coshell_platform::secret_store::SecretStore>);
impl coshell_platform::secret_store::SecretStore for RuntimeSecretStore {
    fn accepts_data_dir(&self, path: &Path) -> bool {
        self.0.accepts_data_dir(path)
    }
    fn get(&self, id: &str) -> Result<Option<Secret>, coshell_platform::secret_store::SecretError> {
        self.0.get(id)
    }
    fn set(
        &self,
        id: &str,
        value: &Secret,
    ) -> Result<(), coshell_platform::secret_store::SecretError> {
        self.0.set(id, value)
    }
    fn delete(&self, id: &str) -> Result<(), coshell_platform::secret_store::SecretError> {
        self.0.delete(id)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDto {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub authentication: String,
    pub group_id: Option<String>,
    pub notes: String,
    pub production: bool,
    pub always_confirm: bool,
    pub version: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavConfigDto {
    pub url: String,
    pub remote_directory: String,
    pub username: String,
    pub has_credential: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_pending: Option<bool>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavPreviewDto {
    pub token: String,
    pub snapshot_id: Option<String>,
    pub manifest_sha256: Option<String>,
    pub protection: &'static str,
    pub connections: Vec<crate::backup_inspection::ConnectionDifference>,
    pub groups: Vec<crate::backup_inspection::GroupDifference>,
    pub host_key_count: usize,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavChoiceDto {
    pub id: String,
    pub action: String,
    pub copy_id: Option<String>,
    pub copy_name: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDto {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
}
impl From<GroupRecord> for GroupDto {
    fn from(v: GroupRecord) -> Self {
        Self {
            id: v.id,
            name: v.name,
            parent_id: v.parent_id,
        }
    }
}
impl From<GroupDto> for GroupRecord {
    fn from(v: GroupDto) -> Self {
        Self {
            id: v.id,
            name: v.name,
            parent_id: v.parent_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(windows, target_os = "macos"))]
    #[tokio::test]
    async fn webdav_credentials_are_replaced_together_and_used_for_server_check() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let dir = std::env::temp_dir().join(format!("coshell-webdav-save-test-{}", id().unwrap()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let runtime = DesktopRuntime::open(&path).unwrap();
        let mut config = WebDavConfigDto {
            url,
            remote_directory: "/old".into(),
            username: "old".into(),
            has_credential: false,
            cleanup_pending: None,
        };
        runtime
            .webdav_save_config(config.clone(), Some("old".into()), "old-op".into())
            .await
            .unwrap();
        config.username = "new".into();
        config.remote_directory = "/new-directory".into();
        assert!(
            matches!(runtime.webdav_save_config(config.clone(), None, "missing-op".into()).await, Err(code) if code == "WEBDAV_PASSWORD_REQUIRED")
        );
        assert_eq!(runtime.webdav_config().unwrap().unwrap().username, "old");
        let saved = runtime
            .webdav_save_config(config, Some("fresh".into()), "new-op".into())
            .await
            .unwrap();
        assert!(saved.has_credential);
        assert_eq!(saved.remote_directory, "/new-directory");
        assert_eq!(
            runtime.webdav_config_save_status("new-op").await.unwrap(),
            "committed"
        );
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() < 4096 {
                request.push(stream.read_u8().await.unwrap());
            }
            let request = String::from_utf8(request).unwrap().to_ascii_lowercase();
            assert!(request.starts_with("propfind / http/1.1\r\n"));
            assert!(request.contains("authorization: basic bmv3omzyzxno\r\n"));
            stream
                .write_all(
                    b"HTTP/1.1 207 Multi-Status\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
        });
        runtime
            .webdav_test_config("check-new-target")
            .await
            .unwrap();
        server.await.unwrap();
        let reference = runtime
            .webdav_config_with_ref()
            .unwrap()
            .unwrap()
            .1
            .unwrap();
        use coshell_platform::secret_store::SecretStore;
        runtime.webdav_store().unwrap().delete(&reference).unwrap();
        drop(runtime);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn sync_preflight_reports_missing_collection_without_writes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut data = [0; 4096];
            let n = stream.read(&mut data).await.unwrap();
            let first = String::from_utf8_lossy(&data[..n])
                .lines()
                .next()
                .unwrap()
                .to_owned();
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            first
        });
        let client = coshell_backup::webdav::WebDav::new(
            &format!("http://{addr}/missing/"),
            "user",
            "secret",
            Duration::from_secs(2),
        )
        .unwrap();
        let error = webdav_sync_preflight(&client, &coshell_backup::webdav::Cancellation::new())
            .await
            .unwrap_err();
        assert_eq!(error, "WebDAV 远程目录不存在，请先创建目录或更正路径");
        assert!(server.await.unwrap().starts_with("PROPFIND /missing/ "));
    }
    #[tokio::test]
    async fn disabling_auto_while_waiting_for_gate_prevents_backup() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default(),
            )
            .unwrap(),
        );
        let held = runtime.webdav_gate.lock().await;
        let enabled = Arc::new(AtomicBool::new(true));
        let generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let r = runtime.clone();
        let flag = enabled.clone();
        let epoch = generation.clone();
        let pending = tokio::spawn(async move { r.webdav_auto_sync(&flag, &epoch, 0).await });
        tokio::task::yield_now().await;
        enabled.store(false, Ordering::Release);
        drop(held);
        assert_eq!(pending.await.unwrap().unwrap_err(), "自动备份已关闭");
    }
    #[tokio::test]
    async fn stale_tick_cannot_run_after_disable_and_reenable() {
        let runtime = DesktopRuntime::from_connection(
            rusqlite::Connection::open_in_memory().unwrap(),
            FeatureFlags::default(),
        )
        .unwrap();
        let enabled = AtomicBool::new(true);
        let generation = std::sync::atomic::AtomicU64::new(2);
        assert_eq!(
            runtime
                .webdav_auto_sync(&enabled, &generation, 0)
                .await
                .unwrap_err(),
            "自动备份已关闭"
        );
    }
    use coshell_drivers::ssh::{RemoteExec, SshFuture};
    #[test]
    fn restore_secret_choices_map_to_original_or_copy_and_skip_keep() {
        let input = br#"{"version":1,"groups":[],"connections":[{"id":"a","name":"a","host":"h","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":false,"always_confirm":false,"version":0},{"id":"b","name":"b","host":"h","port":22,"username":"u","authentication":"password","group_id":null,"notes":"","production":false,"always_confirm":false,"version":0}],"host_keys":[],"secrets":[{"connection_id":"a","secret_type":"password","value":[49]},{"connection_id":"b","secret_type":"password","value":[50]}]}"#;
        let encoded = coshell_backup::encode_optional(input, b"").unwrap();
        let (_, secrets) = crate::backup_inspection::inspect_for_restore(&encoded, b"").unwrap();
        let choices = HashMap::from([
            (
                "a".to_string(),
                crate::backup_inspection::RestoreChoice::Keep,
            ),
            (
                "b".to_string(),
                crate::backup_inspection::RestoreChoice::Copy {
                    id: "copy".into(),
                    name: "copy".into(),
                },
            ),
        ]);
        let mapped = map_restore_secrets(&secrets, &choices).unwrap();
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0].connection_id, "copy");
        assert_eq!(mapped[0].value, b"2");
    }
    #[test]
    fn sync_refuses_unseen_or_changed_remote_version() {
        assert!(webdav_sync_condition(Some("\"remote\""), None).is_err());
        assert!(webdav_sync_condition(Some("\"remote\""), Some("\"old\"")).is_err());
        assert!(matches!(
            webdav_sync_condition(Some("\"remote\""), Some("\"remote\"")),
            Ok(coshell_backup::webdav::PublishCondition::Match(_))
        ));
        assert!(matches!(
            webdav_sync_condition(None, None),
            Ok(coshell_backup::webdav::PublishCondition::Absent)
        ));
        assert!(webdav_sync_condition(None, Some("\"old\"")).is_err());
    }
    #[tokio::test]
    async fn webdav_restore_rejects_active_session_before_consuming_preview() {
        let dir = std::env::temp_dir().join(format!("coshell-webdav-runtime-{}", id().unwrap()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let runtime = DesktopRuntime::open(&path).unwrap();
        let encrypted = coshell_backup::encrypt(
            br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[]}"#,
            b"pass",
        )
        .unwrap();
        let inspection = crate::backup_inspection::inspect(&encrypted, b"pass").unwrap();
        let mut db = rusqlite::Connection::open(&path).unwrap();
        let expected = coshell_storage::Repository::new(&mut db)
            .snapshot()
            .unwrap();
        runtime.webdav_previews.lock().unwrap().insert(
            "token".into(),
            WebDavPreviewState {
                inspection,
                secrets: vec![],
                expected,
                expires: std::time::Instant::now() + Duration::from_secs(60),
                baseline: None,
                source_target: None,
                snapshot_binding: None,
            },
        );
        runtime
            .state
            .lock()
            .await
            .connecting
            .lock()
            .unwrap()
            .insert("c".into(), 1);
        assert!(runtime
            .webdav_apply_restore("token".into(), vec![], false)
            .await
            .is_err());
        assert!(runtime
            .webdav_previews
            .lock()
            .unwrap()
            .contains_key("token"));
        runtime
            .state
            .lock()
            .await
            .connecting
            .lock()
            .unwrap()
            .clear();
        runtime
            .webdav_apply_restore("token".into(), vec![], false)
            .await
            .unwrap();
        assert!(runtime
            .webdav_apply_restore("token".into(), vec![], false)
            .await
            .is_err());
        drop(db);
        drop(runtime);
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn cancelled_preview_never_authorizes_overwrite_or_changes_database() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let encrypted = coshell_backup::encrypt(
            br#"{"version":1,"groups":[],"connections":[],"host_keys":[],"secrets":[]}"#,
            b"pass",
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let body = encrypted.clone();
                tokio::spawn(async move {
                    let mut request = vec![0; 8192];
                    let n = stream.read(&mut request).await.unwrap();
                    let req = String::from_utf8_lossy(&request[..n]);
                    let first = req.lines().next().unwrap_or("");
                    let data = if first.starts_with("GET /attempt-one ") {
                        body
                    } else if first.starts_with("GET /latest ") {
                        br#"{"version":1,"snapshot_id":"s","object":"attempt-one","operation_token":"op","condition":"Absent"}"#.to_vec()
                    } else {
                        vec![]
                    };
                    let status = if first.starts_with("PROPFIND ") {
                        "207 Multi-Status"
                    } else {
                        "200 OK"
                    };
                    let header=format!("HTTP/1.1 {status}\r\nETag: \"v1\"\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",data.len());
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(&data).await;
                });
            }
        });
        let dir = std::env::temp_dir().join(format!("coshell-webdav-preview-{}", id().unwrap()));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let runtime = DesktopRuntime::open(&path).unwrap();
        runtime
            .webdav_save_config(
                WebDavConfigDto {
                    url: format!("http://{addr}"),
                    remote_directory: "/".into(),
                    username: "u".into(),
                    has_credential: false,
                    cleanup_pending: None,
                },
                None,
                "op".into(),
            )
            .await
            .unwrap();
        runtime.webdav_preview_restore("pass".into()).await.unwrap();
        let db = rusqlite::Connection::open(&path).unwrap();
        let count: i64 = db
            .query_row(
                "SELECT COUNT(*) FROM app_settings WHERE key LIKE 'webdav.etag.%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
        // Independent snapshots no longer overwrite the legacy latest index;
        // a preview must still leave its local legacy baseline untouched.
        server.abort();
        drop(db);
        drop(runtime);
        std::fs::remove_dir_all(dir).unwrap();
    }
    struct Channel;
    #[tokio::test]
    async fn file_operation_does_not_block_terminal_but_blocks_edit_and_closes() {
        let runtime = DesktopRuntime::from_connection(
            rusqlite::Connection::open_in_memory().unwrap(),
            FeatureFlags::default().with_enabled(Feature::Connections),
        )
        .unwrap();
        let item = ConnectionDto {
            id: "f".into(),
            name: "f".into(),
            host: "h".into(),
            port: 22,
            username: "u".into(),
            authentication: "password".into(),
            group_id: None,
            notes: String::new(),
            production: false,
            always_confirm: false,
            version: 0,
        };
        runtime.save(item.clone()).await.unwrap();
        runtime.state.lock().await.sessions.insert(
            "s".into(),
            Arc::new(Session {
                connection_id: "f".into(),
                connection_version: 0,
                channel: Some(Arc::new(Channel)),
                connection: Box::new(Connection),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        let (session, _, _) = runtime.file_session("s").await.unwrap();
        let guard = FileInflightGuard(session);
        assert!(runtime.write_terminal("s", "a".into()).await.is_ok());
        assert!(runtime.save(item).await.is_err());
        drop(guard);
        assert!(runtime.close("s").await.is_ok());
    }
    impl TerminalChannel for Channel {
        fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { Ok(None) })
        }
        fn write<'a>(&'a self, _: &'a [u8]) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn resize<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self) -> SshFuture<'a, ()> {
            Box::pin(async { Err(SshError::Transport) })
        }
    }
    struct Connection;
    impl SshConnection for Connection {
        fn open_terminal<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, Box<dyn TerminalChannel>> {
            Box::pin(async { Err(SshError::Unsupported) })
        }
        fn remote_exec<'a>(&'a self) -> SshFuture<'a, Box<dyn RemoteExec>> {
            Box::pin(async { Err(SshError::Unsupported) })
        }
        fn close<'a>(&'a self) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }
    #[tokio::test]
    async fn successful_connection_close_releases_session_even_when_channel_close_fails() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default().with_enabled(Feature::Connections),
            )
            .unwrap(),
        );
        runtime.state.lock().await.sessions.insert(
            "session".into(),
            Arc::new(Session {
                connection_id: "x".into(),
                connection_version: 0,
                channel: Some(Arc::new(Channel)),
                connection: Box::new(Connection),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        runtime.close("session").await.unwrap();
        assert!(!runtime.state.lock().await.sessions.contains_key("session"));
        runtime.close("session").await.unwrap();
    }

    struct PartialChannel(Arc<tokio::sync::Notify>);
    impl TerminalChannel for PartialChannel {
        fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { Ok(None) })
        }
        fn write<'a>(&'a self, _: &'a [u8]) -> SshFuture<'a, ()> {
            Box::pin(async move {
                self.0.notify_one();
                std::future::pending().await
            })
        }
        fn resize<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }
    #[tokio::test]
    async fn dropping_partial_write_disables_subsequent_input() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default().with_enabled(Feature::Connections),
            )
            .unwrap(),
        );
        runtime
            .save(ConnectionDto {
                id: "x".into(),
                name: "x".into(),
                host: "example.org".into(),
                port: 22,
                username: "x".into(),
                authentication: "password".into(),
                group_id: None,
                notes: String::new(),
                production: false,
                always_confirm: false,
                version: 0,
            })
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        runtime.state.lock().await.sessions.insert(
            "s".into(),
            Arc::new(Session {
                connection_id: "x".into(),
                connection_version: 0,
                channel: Some(Arc::new(PartialChannel(entered.clone()))),
                connection: Box::new(Connection),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        let r = runtime.clone();
        let call = tokio::spawn(async move { r.write_terminal("s", "echo hi".into()).await });
        entered.notified().await;
        call.abort();
        let _ = call.await;
        assert!(runtime.state.lock().await.sessions["s"]
            .write_unknown
            .load(Ordering::Acquire));
        assert!(runtime
            .submit("s", "echo again".into(), None)
            .await
            .is_err());
        assert!(runtime.write_terminal("s", "x".into()).await.is_err());
    }
    struct PendingExec(Arc<tokio::sync::Notify>);
    impl RemoteExec for PendingExec {
        fn execute<'a>(
            &'a mut self,
            _: &'a str,
        ) -> SshFuture<'a, coshell_drivers::ssh::RemoteOutput> {
            Box::pin(async { Err(SshError::Unsupported) })
        }
        fn execute_bounded<'a>(
            &'a mut self,
            _: &'a str,
            _: usize,
            _: usize,
        ) -> SshFuture<'a, coshell_drivers::ssh::RemoteOutput> {
            Box::pin(async move {
                self.0.notify_one();
                std::future::pending().await
            })
        }
        fn close<'a>(&'a mut self) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn abort(&mut self) {}
    }
    struct ExecConnection(Arc<tokio::sync::Notify>);
    impl SshConnection for ExecConnection {
        fn open_terminal<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, Box<dyn TerminalChannel>> {
            Box::pin(async { Err(SshError::Unsupported) })
        }
        fn remote_exec<'a>(&'a self) -> SshFuture<'a, Box<dyn RemoteExec>> {
            Box::pin(async { Ok(Box::new(PendingExec(self.0.clone())) as Box<dyn RemoteExec>) })
        }
        fn close<'a>(&'a self) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }
    struct OverviewExec(Arc<std::sync::atomic::AtomicUsize>);
    impl RemoteExec for OverviewExec {
        fn execute<'a>(
            &'a mut self,
            _: &'a str,
        ) -> SshFuture<'a, coshell_drivers::ssh::RemoteOutput> {
            Box::pin(async { Err(SshError::Unsupported) })
        }
        fn execute_bounded<'a>(
            &'a mut self,
            command: &'a str,
            _: usize,
            _: usize,
        ) -> SshFuture<'a, coshell_drivers::ssh::RemoteOutput> {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                assert_eq!(command, coshell_drivers::server_overview::COLLECT_COMMAND);
                Ok(coshell_drivers::ssh::RemoteOutput {
                    stdout: b"hostname=fixture\nswap_total=0\nswap_used=0\ncpu_usage=NaN\n"
                        .to_vec(),
                    stderr: vec![],
                    exit_status: Some(0),
                })
            })
        }
        fn close<'a>(&'a mut self) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn abort(&mut self) {}
    }
    struct OverviewConnection(Arc<std::sync::atomic::AtomicUsize>);
    impl SshConnection for OverviewConnection {
        fn open_terminal<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, Box<dyn TerminalChannel>> {
            Box::pin(async { Err(SshError::Unsupported) })
        }
        fn remote_exec<'a>(&'a self) -> SshFuture<'a, Box<dyn RemoteExec>> {
            Box::pin(async { Ok(Box::new(OverviewExec(self.0.clone())) as Box<dyn RemoteExec>) })
        }
        fn close<'a>(&'a self) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
    }
    #[tokio::test]
    async fn overview_returns_real_snapshot_and_preserves_zero_and_unavailable() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default().with_enabled(Feature::Connections),
            )
            .unwrap(),
        );
        runtime
            .save(ConnectionDto {
                id: "c".into(),
                name: "c".into(),
                host: "example.org".into(),
                port: 22,
                username: "u".into(),
                authentication: "password".into(),
                group_id: None,
                notes: String::new(),
                production: false,
                always_confirm: false,
                version: 0,
            })
            .await
            .unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        runtime.state.lock().await.sessions.insert(
            "s".into(),
            Arc::new(Session {
                connection_id: "c".into(),
                connection_version: 0,
                channel: None,
                connection: Box::new(OverviewConnection(count.clone())),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        let snapshot = runtime.server_overview("s", "op").await.unwrap();
        runtime.server_overview("s", "op").await.unwrap();
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(snapshot.connection_id, "c");
        assert_eq!(snapshot.hostname.as_deref(), Some("fixture"));
        assert_eq!(snapshot.swap_total.unwrap().value, Some(0.0));
        let unavailable = snapshot.cpu_usage.unwrap();
        assert_eq!(unavailable.value, None);
        assert!(unavailable.unavailable_reason.is_some());
        let (sender, _) = tokio::sync::watch::channel(None);
        runtime.overview_operations.lock().unwrap().active.insert(
            "c".into(),
            OverviewOperation {
                session_id: "s".into(),
                operation_id: "pending".into(),
                cancellation: Cancellation::new(),
                sender: sender.clone(),
            },
        );
        let other = runtime.clone();
        let waiter = tokio::spawn(async move { other.server_overview("s", "pending").await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        sender.send_replace(Some(Ok(coshell_drivers::server_overview::parse_snapshot(
            "c",
            "2026-01-01T00:00:00Z",
            "hostname=fixture\n",
        ))));
        runtime.state.lock().await.sessions["s"]
            .closed
            .store(true, Ordering::Release);
        assert!(waiter.await.unwrap().is_err());
        runtime.state.lock().await.sessions["s"]
            .closed
            .store(false, Ordering::Release);
        runtime.server_overview_cancel("s", "op").await.unwrap();
        runtime.server_overview("s", "op").await.unwrap();
        let (sender, _) = tokio::sync::watch::channel(None);
        runtime.overview_operations.lock().unwrap().active.insert(
            "c".into(),
            OverviewOperation {
                session_id: "s".into(),
                operation_id: "pending2".into(),
                cancellation: Cancellation::new(),
                sender: sender.clone(),
            },
        );
        let other = runtime.clone();
        let waiter = tokio::spawn(async move { other.server_overview("s", "pending2").await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        let mut changed = runtime.list().await.unwrap().pop().unwrap();
        changed.name = "changed".into();
        runtime.save(changed).await.unwrap();
        sender.send_replace(Some(Ok(coshell_drivers::server_overview::parse_snapshot(
            "c",
            "2026-01-01T00:00:00Z",
            "hostname=fixture\n",
        ))));
        assert!(waiter.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn overview_requires_live_session_and_cancels_independent_exec() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default().with_enabled(Feature::Connections),
            )
            .unwrap(),
        );
        assert!(runtime.server_overview("missing", "op").await.is_err());
        runtime
            .save(ConnectionDto {
                id: "c".into(),
                name: "c".into(),
                host: "example.org".into(),
                port: 22,
                username: "u".into(),
                authentication: "password".into(),
                group_id: None,
                notes: String::new(),
                production: false,
                always_confirm: false,
                version: 0,
            })
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        runtime.state.lock().await.sessions.insert(
            "s".into(),
            Arc::new(Session {
                connection_id: "c".into(),
                connection_version: 0,
                channel: None,
                connection: Box::new(ExecConnection(entered.clone())),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        runtime.server_overview_cancel("s", "early").await.unwrap();
        assert!(runtime.server_overview("s", "early").await.is_err());
        let worker = runtime.clone();
        let call = tokio::spawn(async move { worker.server_overview("s", "op").await });
        entered.notified().await;
        let waiter_runtime = runtime.clone();
        let waiter = tokio::spawn(async move { waiter_runtime.server_overview("s", "op").await });
        tokio::task::yield_now().await;
        assert!(!waiter.is_finished());
        assert!(runtime.server_overview("s", "other").await.is_err());
        runtime.server_overview_cancel("s", "op").await.unwrap();
        assert!(call.await.unwrap().is_err());
        assert!(waiter.await.unwrap().is_err());
    }
    #[tokio::test]
    async fn dropped_production_exec_is_unknown_and_never_replayed() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default().with_enabled(Feature::Connections),
            )
            .unwrap(),
        );
        runtime
            .save(ConnectionDto {
                id: "p".into(),
                name: "p".into(),
                host: "example.org".into(),
                port: 22,
                username: "x".into(),
                authentication: "password".into(),
                group_id: None,
                notes: String::new(),
                production: true,
                always_confirm: false,
                version: 0,
            })
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        runtime.state.lock().await.sessions.insert(
            "s".into(),
            Arc::new(Session {
                connection_id: "p".into(),
                connection_version: 0,
                channel: None,
                connection: Box::new(ExecConnection(entered.clone())),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        let token = match runtime
            .submit("s", "rm -rf /tmp/x".into(), None)
            .await
            .unwrap()
        {
            SubmitResult::ConfirmationRequired { token, .. } => token,
            _ => panic!(),
        };
        let r = runtime.clone();
        let op =
            tokio::spawn(async move { r.submit("s", "rm -rf /tmp/x".into(), Some(token)).await });
        entered.notified().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), runtime.list())
                .await
                .is_ok(),
            "unrelated list should not block on remote execution"
        );
        let record = runtime.list().await.unwrap().remove(0);
        assert!(
            runtime.save(record).await.is_err(),
            "editing the active connection must not race execution"
        );
        op.abort();
        let _ = op.await;
        assert!(runtime.state.lock().await.sessions["s"]
            .write_unknown
            .load(Ordering::Acquire));
        assert!(runtime.submit("s", "echo next".into(), None).await.is_err());
        assert!(runtime.read("s").await.unwrap().closed);
    }

    #[tokio::test]
    async fn closing_a_running_command_cancels_it_without_blocking_other_state() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default().with_enabled(Feature::Connections),
            )
            .unwrap(),
        );
        runtime
            .save(ConnectionDto {
                id: "p".into(),
                name: "p".into(),
                host: "example.org".into(),
                port: 22,
                username: "x".into(),
                authentication: "password".into(),
                group_id: None,
                notes: String::new(),
                production: true,
                always_confirm: false,
                version: 0,
            })
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        runtime.state.lock().await.sessions.insert(
            "s".into(),
            Arc::new(Session {
                connection_id: "p".into(),
                connection_version: 0,
                channel: None,
                connection: Box::new(ExecConnection(entered.clone())),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        let token = match runtime
            .submit("s", "rm -rf /tmp/x".into(), None)
            .await
            .unwrap()
        {
            SubmitResult::ConfirmationRequired { token, .. } => token,
            _ => panic!(),
        };
        let r = runtime.clone();
        let op =
            tokio::spawn(async move { r.submit("s", "rm -rf /tmp/x".into(), Some(token)).await });
        entered.notified().await;
        tokio::time::timeout(Duration::from_secs(1), runtime.close("s"))
            .await
            .unwrap()
            .unwrap();
        assert!(tokio::time::timeout(Duration::from_secs(1), op)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(runtime.submit("s", "echo next".into(), None).await.is_err());
    }
    struct ResizeChannel {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        applied: Arc<Mutex<Vec<u16>>>,
    }
    impl TerminalChannel for ResizeChannel {
        fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { Ok(None) })
        }
        fn write<'a>(&'a self, _: &'a [u8]) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn resize<'a>(&'a self, size: TerminalSize) -> SshFuture<'a, ()> {
            Box::pin(async move {
                if size.columns == 80 {
                    self.entered.notify_one();
                    self.release.notified().await;
                }
                self.applied.lock().unwrap().push(size.columns);
                Ok(())
            })
        }
    }
    #[tokio::test]
    async fn concurrent_resizes_apply_in_request_order() {
        let runtime = Arc::new(
            DesktopRuntime::from_connection(
                rusqlite::Connection::open_in_memory().unwrap(),
                FeatureFlags::default().with_enabled(Feature::Connections),
            )
            .unwrap(),
        );
        runtime
            .save(ConnectionDto {
                id: "r".into(),
                name: "r".into(),
                host: "example.org".into(),
                port: 22,
                username: "x".into(),
                authentication: "password".into(),
                group_id: None,
                notes: String::new(),
                production: false,
                always_confirm: false,
                version: 0,
            })
            .await
            .unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(tokio::sync::Notify::new());
        let applied = Arc::new(Mutex::new(Vec::new()));
        runtime.state.lock().await.sessions.insert(
            "s".into(),
            Arc::new(Session {
                connection_id: "r".into(),
                connection_version: 0,
                channel: Some(Arc::new(ResizeChannel {
                    entered: entered.clone(),
                    release: release.clone(),
                    applied: applied.clone(),
                })),
                connection: Box::new(Connection),
                reader: AsyncMutex::new(Vec::new()),
                write_unknown: AtomicBool::new(false),
                exec_output: AsyncMutex::new(VecDeque::new()),
                inflight: AtomicBool::new(false),
                file_inflight: AtomicBool::new(false),
                closed: AtomicBool::new(false),
                cancel: Cancellation::new(),
                close_lock: AsyncMutex::new(()),
                resize_lock: AsyncMutex::new(()),
                inflight_done: tokio::sync::Notify::new(),
            }),
        );
        let r = runtime.clone();
        let first = tokio::spawn(async move { r.resize_terminal("s", 80, 24).await });
        entered.notified().await;
        let r = runtime.clone();
        let second = tokio::spawn(async move { r.resize_terminal("s", 120, 30).await });
        tokio::task::yield_now().await;
        release.notify_one();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();
        assert_eq!(*applied.lock().unwrap(), vec![80, 120]);
    }
}
impl From<ConnectionRecord> for ConnectionDto {
    fn from(v: ConnectionRecord) -> Self {
        Self {
            id: v.id,
            name: v.name,
            host: v.host,
            port: v.port,
            username: v.username,
            authentication: v.authentication,
            group_id: v.group_id,
            notes: v.notes,
            production: v.production,
            always_confirm: v.always_confirm,
            version: v.version,
        }
    }
}
impl From<ConnectionDto> for ConnectionRecord {
    fn from(v: ConnectionDto) -> Self {
        Self {
            id: v.id,
            name: v.name,
            host: v.host,
            port: v.port,
            username: v.username,
            authentication: v.authentication,
            group_id: v.group_id,
            notes: v.notes,
            production: v.production,
            always_confirm: v.always_confirm,
            version: v.version,
        }
    }
}

#[derive(Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum OpenResult {
    Connected {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        password_save_token: Option<String>,
    },
    HostKeyConfirmation {
        host: String,
        port: u16,
        algorithm: String,
        fingerprint: String,
    },
}
#[derive(Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SubmitResult {
    Sent,
    ConfirmationRequired {
        token: String,
        risk: String,
        categories: Vec<String>,
    },
}
#[derive(Serialize)]
#[serde(
    tag = "status",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SavedRunResult {
    Sent {
        output: String,
    },
    ConfirmationRequired {
        token: String,
        risk: String,
        categories: Vec<String>,
    },
}
#[derive(Serialize)]
pub struct ReadResult {
    pub output: String,
    pub closed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFileEntry {
    pub name: String,
    pub kind: &'static str,
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    pub modified: Option<u32>,
}
impl From<coshell_drivers::ssh::sftp::Entry> for RemoteFileEntry {
    fn from(e: coshell_drivers::ssh::sftp::Entry) -> Self {
        let kind = match e.permissions.map(|v| v & 0o170000) {
            Some(0o040000) => "directory",
            Some(0o100000) => "file",
            Some(0o120000) => "symlink",
            _ => "unknown",
        };
        Self {
            name: e.name,
            kind,
            size: e.size,
            permissions: e.permissions,
            modified: e.modified,
        }
    }
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteFilePermit {
    pub token: String,
    pub confirmation_required: bool,
}
#[derive(Serialize)]
pub struct RemoteFileContent {
    pub bytes: Vec<u8>,
    pub metadata: RemoteFileEntry,
}

struct Prompt {
    expected: Option<HostKey>,
    observed: Mutex<Option<HostKey>>,
}
impl HostKeyPrompt for Prompt {
    fn ask<'a>(&'a self, key: &'a HostKey) -> SshFuture<'a, HostKeyDecision> {
        Box::pin(async move {
            *self.observed.lock().map_err(|_| SshError::Transport)? = Some(key.clone());
            Ok(if self.expected.as_ref() == Some(key) {
                HostKeyDecision::AcceptAndSave
            } else {
                HostKeyDecision::Reject
            })
        })
    }
}
struct Session {
    connection_id: String,
    connection_version: u64,
    channel: Option<Arc<dyn TerminalChannel>>,
    connection: Box<dyn SshConnection>,
    reader: AsyncMutex<Vec<u8>>,
    write_unknown: AtomicBool,
    exec_output: AsyncMutex<VecDeque<String>>,
    inflight: AtomicBool,
    file_inflight: AtomicBool,
    closed: AtomicBool,
    cancel: Cancellation,
    close_lock: AsyncMutex<()>,
    resize_lock: AsyncMutex<()>,
    inflight_done: tokio::sync::Notify,
}
struct InflightGuard(Arc<Session>);
struct FileInflightGuard(Arc<Session>);
impl Drop for FileInflightGuard {
    fn drop(&mut self) {
        self.0.file_inflight.store(false, Ordering::Release);
        self.0.inflight_done.notify_one();
    }
}
impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0.inflight.store(false, Ordering::Release);
        self.0.inflight_done.notify_one();
    }
}
struct Challenge {
    key: HostKey,
    version: u64,
    deadline: std::time::Instant,
}
struct State {
    sessions: HashMap<String, Arc<Session>>,
    challenges: HashMap<String, Challenge>,
    connecting: Arc<Mutex<HashMap<String, usize>>>,
}
struct ConnectingGuard {
    connection_id: String,
    connecting: Arc<Mutex<HashMap<String, usize>>>,
}
impl Drop for ConnectingGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.connecting.lock() {
            if let Some(count) = map.get_mut(&self.connection_id) {
                *count -= 1;
                if *count == 0 {
                    map.remove(&self.connection_id);
                }
            }
        }
    }
}
struct OpenOperation {
    cancellation: Cancellation,
    deadline: std::time::Instant,
    session_id: Option<String>,
    started: bool,
}
pub struct DesktopRuntime {
    #[cfg(test)]
    store_override: Option<Arc<dyn coshell_platform::secret_store::SecretStore>>,
    service: ConnectionService,
    path: Option<PathBuf>,
    state: AsyncMutex<State>,
    gate: CommandGate,
    saved_lock: Mutex<()>,
    saved_pending: Mutex<HashMap<String, (String, u64, std::time::Instant)>>,
    operations: Mutex<HashMap<String, OpenOperation>>,
    passwords: passwords::PasswordOffers,
    overview_operations: Arc<Mutex<OverviewStore>>,
    overview_cancelled: Mutex<HashMap<String, (String, std::time::Instant)>>,
    webdav_previews: Mutex<HashMap<String, WebDavPreviewState>>,
    webdav_gate: AsyncMutex<()>,
    webdav_tests: crate::webdav_test::WebDavTestCoordinator,
    remote_files: crate::remote_files::MutationGate,
    _runtime_lock: Option<std::fs::File>,
}
struct WebDavPreviewState {
    inspection: crate::backup_inspection::Inspection,
    secrets: Vec<crate::backup_inspection::RestoreSecret>,
    expected: coshell_storage::LocalSnapshot,
    expires: std::time::Instant,
    baseline: Option<(String, String)>,
    source_target: Option<String>,
    snapshot_binding: Option<(String, String)>,
}
fn map_restore_secrets<'a>(
    secrets: &'a [crate::backup_inspection::RestoreSecret],
    choices: &'a HashMap<String, crate::backup_inspection::RestoreChoice>,
) -> Result<Vec<crate::restore_secrets::IncomingSecret<'a>>, String> {
    let mut incoming = Vec::new();
    for secret in secrets {
        let target = match choices
            .get(&secret.connection_id)
            .ok_or_else(|| error("恢复选择无效"))?
        {
            crate::backup_inspection::RestoreChoice::Keep => continue,
            crate::backup_inspection::RestoreChoice::Replace => secret.connection_id.as_str(),
            crate::backup_inspection::RestoreChoice::Copy { id, .. } => id.as_str(),
        };
        incoming.push(crate::restore_secrets::IncomingSecret {
            connection_id: target,
            secret_type: &secret.secret_type,
            value: secret.value(),
        });
    }
    Ok(incoming)
}
struct OverviewOperation {
    session_id: String,
    operation_id: String,
    cancellation: Cancellation,
    sender:
        tokio::sync::watch::Sender<Option<Result<coshell_types::ServerOverviewSnapshot, String>>>,
}
#[derive(Default)]
struct OverviewStore {
    active: HashMap<String, OverviewOperation>,
    receipts: HashMap<
        String,
        (
            String,
            Result<coshell_types::ServerOverviewSnapshot, String>,
        ),
    >,
    order: VecDeque<String>,
}
struct OverviewGuard {
    connection_id: String,
    operation_id: String,
    operations: Arc<Mutex<OverviewStore>>,
    completed: bool,
}
impl OverviewGuard {
    fn finish(&mut self, result: Result<coshell_types::ServerOverviewSnapshot, String>) {
        if let Ok(mut store) = self.operations.lock() {
            if store
                .active
                .get(&self.connection_id)
                .is_some_and(|entry| entry.operation_id == self.operation_id)
            {
                let entry = store.active.remove(&self.connection_id).unwrap();
                entry.sender.send_replace(Some(result.clone()));
                store
                    .receipts
                    .insert(self.operation_id.clone(), (entry.session_id, result));
                store.order.push_back(self.operation_id.clone());
                while store.order.len() > 256 {
                    if let Some(id) = store.order.pop_front() {
                        store.receipts.remove(&id);
                    }
                }
            }
        }
        self.completed = true;
    }
}
impl Drop for OverviewGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.finish(Err(error("采集已中断")));
        }
    }
}
fn error(code: &str) -> String {
    code.to_owned()
}
#[cfg(test)]
async fn webdav_sync_preflight(
    client: &coshell_backup::webdav::WebDav,
    cancel: &coshell_backup::webdav::Cancellation,
) -> Result<(), String> {
    use coshell_backup::webdav::WebDavError;
    client
        .check_connection(cancel)
        .await
        .map_err(|reason| match reason {
            WebDavError::NotFound => error("WebDAV 远程目录不存在，请先创建目录或更正路径"),
            WebDavError::Authentication => error("WebDAV 认证失败，请检查凭据"),
            WebDavError::Unsupported => error("WebDAV 服务器不支持目录访问检查"),
            WebDavError::Timeout => error("WebDAV 目录访问检查超时"),
            WebDavError::Cancelled => error("WebDAV 目录访问检查已取消"),
            _ => error("WebDAV 目录访问检查失败"),
        })
}

#[cfg(test)]
fn webdav_sync_condition(
    remote: Option<&str>,
    known: Option<&str>,
) -> Result<coshell_backup::webdav::PublishCondition, String> {
    match (remote, known) {
        (None, None) => Ok(coshell_backup::webdav::PublishCondition::Absent),
        (Some(a), Some(b)) if a == b => {
            Ok(coshell_backup::webdav::PublishCondition::Match(a.into()))
        }
        _ => Err(error("远端备份已变化，请先重新预览")),
    }
}
fn webdav_baseline_key(base: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("webdav.etag.{:x}", Sha256::digest(base.as_bytes()))
}
fn set_webdav_baseline(path: &Path, base: &str, etag: &str) -> Result<(), String> {
    let db = rusqlite::Connection::open(path).map_err(|_| error("保存同步基准失败"))?;
    db.execute("INSERT INTO app_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",rusqlite::params![webdav_baseline_key(base),etag]).map_err(|_|error("保存同步基准失败"))?;
    Ok(())
}
fn ssh_error(e: SshError) -> String {
    match e { SshError::HostKeyChanged{old_algorithm,old_fingerprint,new_algorithm,new_fingerprint}=>format!("主机密钥已变更：原 {old_algorithm} {old_fingerprint}；新 {new_algorithm} {new_fingerprint}，连接已阻止"), SshError::AuthenticationFailed=>"SSH 认证失败".into(), SshError::Cancelled=>"连接已取消".into(), SshError::Timeout=>"SSH 连接超时".into(), SshError::HostKeyBusy=>"主机指纹存储正忙，请重试".into(), SshError::HostKeyRejected=>"主机密钥未被接受".into(), _=>"SSH 连接失败".into() }
}
fn id() -> Result<String, String> {
    let mut b = [0u8; 32];
    getrandom::getrandom(&mut b).map_err(|_| error("随机数生成失败"))?;
    Ok(b.iter().map(|v| format!("{v:02x}")).collect())
}
impl DesktopRuntime {
    pub fn saved_command_list(&self) -> Result<Vec<crate::saved_commands::SavedCommand>, String> {
        let db = rusqlite::Connection::open_with_flags(
            self.webdav_path()?,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| error("命令存储不可用"))?;
        crate::saved_commands::list(&db)
    }
    pub fn saved_command_save(
        &self,
        input: crate::saved_commands::SavedCommand,
    ) -> Result<crate::saved_commands::SavedCommand, String> {
        let _lock = self
            .saved_lock
            .lock()
            .map_err(|_| error("命令存储不可用"))?;
        let mut db = rusqlite::Connection::open_with_flags(
            self.webdav_path()?,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|_| error("命令存储不可用"))?;
        crate::saved_commands::save(&mut db, input)
    }
    pub fn saved_command_delete(&self, id: &str, version: u64) -> Result<bool, String> {
        let _lock = self
            .saved_lock
            .lock()
            .map_err(|_| error("命令存储不可用"))?;
        let mut db = rusqlite::Connection::open_with_flags(
            self.webdav_path()?,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
        )
        .map_err(|_| error("命令存储不可用"))?;
        crate::saved_commands::delete(&mut db, id, version)
    }
    /// Automatic backup is opt-in and uses the explicitly selected plaintext mode.
    pub fn webdav_auto_interval(&self) -> Result<Option<u32>, String> {
        crate::auto_backup::load_interval(self.webdav_path()?)
            .map_err(|_| error("读取自动备份设置失败"))
    }
    pub async fn webdav_set_auto_interval(
        &self,
        interval: Option<u32>,
        accept_plaintext: bool,
    ) -> Result<(), String> {
        let _gate = self.webdav_gate.lock().await;
        let mut binding = None;
        if interval.is_some() {
            if !accept_plaintext {
                return Err(error("启用自动备份需要明确确认明文包含连接凭据"));
            }
            let config = self
                .webdav_config()?
                .ok_or_else(|| error("请先配置 WebDAV"))?;
            binding = Some(
                serde_json::to_string(&(config.url, config.remote_directory, config.username))
                    .map_err(|_| error("自动备份设置无效"))?,
            );
        }
        crate::auto_backup::save_interval(self.webdav_path()?, interval, binding.as_deref())
            .map_err(|_| error("自动备份设置无效或保存失败"))
    }
    pub async fn webdav_auto_sync(
        &self,
        enabled: &AtomicBool,
        generation: &std::sync::atomic::AtomicU64,
        expected_generation: u64,
    ) -> Result<(), String> {
        let _gate = self.webdav_gate.lock().await;
        if !enabled.load(Ordering::Acquire)
            || generation.load(Ordering::Acquire) != expected_generation
        {
            return Err(error("自动备份已关闭"));
        }
        let config = self
            .webdav_config()?
            .ok_or_else(|| error("请先配置 WebDAV"))?;
        let binding =
            serde_json::to_string(&(config.url, config.remote_directory, config.username))
                .map_err(|_| error("自动备份设置无效"))?;
        if !crate::auto_backup::target_matches(self.webdav_path()?, &binding)
            .map_err(|_| error("自动备份设置不可用"))?
        {
            return Err(error("WebDAV 目标已变化，请重新确认自动备份"));
        }
        self.webdav_sync_inner(String::new(), id()?)
            .await
            .map(|_| ())
    }
    fn webdav_path(&self) -> Result<&Path, String> {
        self.path
            .as_deref()
            .ok_or_else(|| error("WebDAV 数据目录不可用"))
    }
    fn webdav_flags() -> FeatureFlags {
        FeatureFlags::default().with_enabled(Feature::WebDavBackup)
    }
    fn webdav_store(&self) -> Result<RuntimeSecretStore, String> {
        #[cfg(test)]
        if let Some(store) = &self.store_override {
            return Ok(RuntimeSecretStore(store.clone()));
        }
        let parent = self
            .webdav_path()?
            .parent()
            .ok_or_else(|| error("WebDAV 数据目录不可用"))?;
        coshell_platform::secret_store::PlatformSecretStore::for_data_dir(parent)
            .map(|store| RuntimeSecretStore(Arc::new(store)))
            .map_err(|_| error("凭据存储不可用"))
    }
    fn outbox_key(
        &self,
        db: &mut rusqlite::Connection,
    ) -> Result<zeroize::Zeroizing<[u8; 32]>, String> {
        use coshell_platform::secret_store::SecretStore;
        let dir = self
            .webdav_path()?
            .parent()
            .ok_or_else(|| error("数据目录不可用"))?;
        let _lock = crate::secret_compensation::SecretOperationLock::try_acquire(dir)
            .map_err(|_| error("凭据存储正忙，请重试"))?;
        let store = self.webdav_store()?;
        const KEY_ID: &str = "coshell-webdav-outbox-key-v1";
        if let Some(existing) = store.get(KEY_ID).map_err(|_| error("同步队列密钥不可用"))?
        {
            let mut key = zeroize::Zeroizing::new([0u8; 32]);
            if existing.expose().len() != key.len() {
                return Err(error("同步队列密钥无效"));
            }
            key.copy_from_slice(existing.expose());
            return Ok(key);
        }
        if crate::directory_outbox::has_directory_rows(db).map_err(|_| error("同步队列不可用"))?
            || crate::publication_outbox::Outbox::new(db)
                .and_then(|outbox| outbox.has_protected_rows())
                .map_err(|_| error("同步队列不可用"))?
        {
            return Err(error("同步队列密钥丢失，无法安全恢复待处理操作"));
        }
        let mut key = zeroize::Zeroizing::new([0u8; 32]);
        getrandom::getrandom(key.as_mut()).map_err(|_| error("无法生成同步队列密钥"))?;
        store
            .set(KEY_ID, &Secret::new(key.to_vec()))
            .map_err(|_| error("无法安全保存同步队列密钥"))?;
        Ok(key)
    }
    // Read the committed metadata and credential reference from the same row.
    fn webdav_config_with_ref(&self) -> Result<Option<(WebDavConfigDto, Option<String>)>, String> {
        use rusqlite::OptionalExtension;
        let db = rusqlite::Connection::open_with_flags(
            self.webdav_path()?,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|_| error("读取 WebDAV 配置失败"))?;
        db.query_row("SELECT url,remote_directory,username,credential_secret_id FROM active_webdav_profiles WHERE id='default'", [], |row| {
            let reference: Option<String> = row.get(3)?;
            Ok((WebDavConfigDto { url: row.get(0)?, remote_directory: row.get(1)?, username: row.get(2)?, has_credential: reference.is_some(), cleanup_pending: None }, reference))
        }).optional().map_err(|_| error("读取 WebDAV 配置失败"))
    }
    pub fn webdav_config(&self) -> Result<Option<WebDavConfigDto>, String> {
        Ok(self.webdav_config_with_ref()?.map(|(config, _)| config))
    }
    pub async fn webdav_save_config(
        &self,
        input: WebDavConfigDto,
        password: Option<String>,
        operation_id: String,
    ) -> Result<WebDavConfigDto, String> {
        let _gate = self.webdav_gate.lock().await;
        let store = self.webdav_store()?;
        let secret = password.map(|s| Secret::new(s.into_bytes()));
        let profile = crate::webdav_config::WebDavProfile {
            id: "default".into(),
            url: input.url,
            remote_directory: input.remote_directory,
            username: input.username,
            auto_backup: false,
        };
        let saved = crate::webdav_config::save_profile(
            self.webdav_path()?,
            &store,
            &Self::webdav_flags(),
            &profile,
            secret.as_ref().map(|s| (operation_id.as_str(), s)),
        )
        .map_err(|e| {
            use crate::secret_compensation::CoordinatorError;
            use crate::webdav_config::ConfigError;
            match e {
                ConfigError::CredentialTargetChanged => error("WEBDAV_PASSWORD_REQUIRED"),
                ConfigError::Credential(
                    CoordinatorError::CommitUnknown | CoordinatorError::CleanupPending,
                ) => error("WEBDAV_CONFIG_SAVE_UNKNOWN"),
                _ => error("WEBDAV_CONFIG_SAVE_FAILED"),
            }
        })?;
        let mut config = self
            .webdav_config()?
            .ok_or_else(|| error("WEBDAV_CONFIG_SAVE_UNKNOWN"))?;
        config.cleanup_pending = saved.cleanup_pending.then_some(true);
        Ok(config)
    }
    pub async fn webdav_config_save_status(&self, operation: &str) -> Result<String, String> {
        use crate::webdav_config::CredentialStatus;
        let _gate = self.webdav_gate.lock().await;
        let path = self.webdav_path()?;
        let store = self.webdav_store()?;
        let flags = Self::webdav_flags();
        let read = || {
            crate::webdav_config::operation_status(path, &store, &flags, "default", operation)
                .map_err(|_| error("WEBDAV_CONFIG_STATUS_UNAVAILABLE"))
        };
        let mut status = read()?;
        if matches!(
            status,
            CredentialStatus::Staged
                | CredentialStatus::Committed {
                    cleanup_pending: true
                }
        ) {
            let _ = crate::webdav_config::recover_credentials(path, &store, &flags);
            status = read()?;
        }
        Ok(match status {
            CredentialStatus::NotFound => "not_found",
            CredentialStatus::Staged => "pending",
            CredentialStatus::Aborted => "aborted",
            CredentialStatus::AbortedRetainedRuns => "aborted_retained_runs",
            CredentialStatus::Committed {
                cleanup_pending: true,
            } => "committed_cleanup_pending",
            CredentialStatus::Committed {
                cleanup_pending: false,
            } => "committed",
        }
        .into())
    }
    fn webdav_source_binding(&self, base: &str) -> Result<String, String> {
        use sha2::{Digest, Sha256};
        let target =
            coshell_backup::webdav::canonical_target(base).map_err(|_| error("WebDAV 目标无效"))?;
        let username = self
            .webdav_config()?
            .ok_or_else(|| error("请先配置 WebDAV"))?
            .username;
        let bytes =
            serde_json::to_vec(&(target, username)).map_err(|_| error("WebDAV 目标无效"))?;
        Ok(format!("{:x}", Sha256::digest(bytes)))
    }
    fn webdav_client(&self) -> Result<(coshell_backup::webdav::WebDav, String), String> {
        self.webdav_client_at(false)
    }
    fn webdav_client_at(
        &self,
        server_only: bool,
    ) -> Result<(coshell_backup::webdav::WebDav, String), String> {
        use coshell_platform::secret_store::SecretStore;
        let path = self.webdav_path()?;
        let directory = path
            .parent()
            .ok_or_else(|| error("WebDAV 数据目录不可用"))?;
        let _lock = crate::secret_compensation::SecretOperationLock::acquire(directory)
            .map_err(|_| error("WebDAV 凭据存储不可用"))?;
        let (config, secret_id) = self
            .webdav_config_with_ref()?
            .ok_or_else(|| error("请先配置 WebDAV"))?;
        let store = self.webdav_store()?;
        let secret = secret_id
            .map(|id| {
                store
                    .get(&id)
                    .map_err(|_| error("读取 WebDAV 凭据失败"))?
                    .ok_or_else(|| error("WebDAV 凭据不存在"))
            })
            .transpose()?;
        let password = secret
            .as_ref()
            .map(|s| std::str::from_utf8(s.expose()).map_err(|_| error("WebDAV 凭据无效")))
            .transpose()?
            .unwrap_or("");
        let directory = config.remote_directory.trim_matches('/');
        let base = if server_only || directory.is_empty() {
            format!("{}/", config.url.trim_end_matches('/'))
        } else {
            format!("{}/{directory}/", config.url.trim_end_matches('/'))
        };
        let client = coshell_backup::webdav::WebDav::new(
            &base,
            &config.username,
            password,
            Duration::from_secs(30),
        )
        .map_err(|_| error("WebDAV 地址无效"))?;
        Ok((client, base))
    }
    /// Checks authenticated directory access only. It creates no remote objects
    /// and does not claim that conditional writes or cleanup are supported.
    pub async fn webdav_test_config(&self, operation_id: &str) -> Result<String, String> {
        self.webdav_tests
            .run(operation_id, |cancel| async move {
                let _gate = self.webdav_gate.lock().await;
                if cancel.is_cancelled() {
                    return Err(error("连接测试已取消"));
                }
                let (client, _) = self.webdav_client_at(true)?;
                if cancel.is_cancelled() {
                    return Err(error("连接测试已取消"));
                }
                client
                    .check_connection(cancel.network())
                    .await
                    .map_err(|e| match e {
                        coshell_backup::webdav::WebDavError::Authentication => {
                            error("WebDAV 认证失败或无访问权限")
                        }
                        coshell_backup::webdav::WebDavError::NotFound => error("WebDAV 目录不存在"),
                        coshell_backup::webdav::WebDavError::Unsupported => {
                            error("服务器不支持 WebDAV 目录检查")
                        }
                        coshell_backup::webdav::WebDavError::Cancelled => error("连接测试已取消"),
                        coshell_backup::webdav::WebDavError::Timeout => error("连接测试超时"),
                        _ => error("WebDAV 连接失败，请检查地址和网络"),
                    })
            })
            .await?;
        Ok("服务器以 WebDAV 多状态码响应；未验证目录内容、写入、删除和条件更新能力。".into())
    }
    pub fn webdav_test_cancel(&self, operation_id: &str) -> Result<(), String> {
        self.webdav_tests.cancel(operation_id)
    }
    pub async fn webdav_sync(
        &self,
        passphrase: String,
        operation_id: String,
    ) -> Result<String, String> {
        let _gate = self.webdav_gate.lock().await;
        self.webdav_sync_inner(passphrase, operation_id).await
    }
    async fn webdav_sync_inner(
        &self,
        passphrase: String,
        operation_id: String,
    ) -> Result<String, String> {
        let passphrase = zeroize::Zeroizing::new(passphrase);
        let (root, _) = self.webdav_client_at(true)?;
        let directory = self
            .webdav_config()?
            .ok_or_else(|| error("请先配置 WebDAV"))?
            .remote_directory;
        let cancel = coshell_backup::webdav::Cancellation::new();
        let (client, base) = self.webdav_client()?;
        let db_path = self.webdav_path()?;
        let path = db_path
            .parent()
            .ok_or_else(|| error("数据目录不可用"))?
            .join("webdav-outbox.sqlite");
        let mut db = rusqlite::Connection::open(path).map_err(|_| error("同步队列不可用"))?;
        let key = self.outbox_key(&mut db)?;
        let mut outbox = crate::directory_outbox::DirectoryOutbox::new(&mut db, key)
            .map_err(|_| error("同步队列不可用"))?;
        if outbox
            .load(&operation_id)
            .map_err(|_| error("同步队列密钥或数据不可用"))?
            .is_none()
        {
            let now = chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now());
            let directory = now.format("%Y%m%d%H%M%S").to_string();
            let created = now.format("%Y-%m-%dT%H:%M:%SZ").to_string();
            let snapshot_id = id()?;
            let artifact = crate::backup_snapshot::create_directory_snapshot(
                db_path,
                &self.webdav_store()?,
                passphrase.as_bytes(),
                crate::backup_snapshot::BackupOptions {
                    include_secrets: true,
                },
                &Self::webdav_flags(),
                coshell_backup::snapshot_manifest::SnapshotMetadata {
                    snapshot_id: &snapshot_id,
                    created_at: &created,
                    app_version: env!("CARGO_PKG_VERSION"),
                },
            )
            .map_err(|_| error("创建独立快照失败"))?;
            let packed = artifact.pack().map_err(|_| error("快照格式无效"))?;
            outbox
                .prepare_account(
                    &operation_id,
                    &base,
                    &directory,
                    &packed,
                    &self
                        .webdav_config()?
                        .ok_or_else(|| error("请先配置 WebDAV"))?
                        .username,
                )
                .map_err(|e| format!("准备备份失败: {e:?}"))?;
        }
        let pending = outbox
            .load(&operation_id)
            .map_err(|_| error("同步队列数据不可用"))?
            .ok_or_else(|| error("备份操作不存在"))?;
        let canonical = coshell_backup::webdav::canonical_target(&base)
            .map_err(|_| error("WebDAV 目标无效"))?;
        if pending.target != canonical
            || pending.account
                != self
                    .webdav_config()?
                    .ok_or_else(|| error("请先配置 WebDAV"))?
                    .username
        {
            return Err(error("备份操作目标已变化，不能发送到新目标"));
        }
        let artifact = coshell_backup::snapshot_manifest::SnapshotArtifact::unpack(&pending.body)
            .map_err(|_| error("待发布快照已损坏"))?;
        if pending.state == "succeeded" {
            return Ok("备份已完成".into());
        }
        if pending.state == "failed" {
            return Err(error("该备份已明确失败，请查看操作记录"));
        }
        if pending.state == "prepared" {
            root.ensure_directory(&directory, &cancel)
                .await
                .map_err(|e| format!("创建 WebDAV 目录失败: {e:?}"))?;
        }
        if !outbox
            .claim(&operation_id)
            .map_err(|e| format!("备份队列忙: {e:?}"))?
        {
            match client
                .verify_snapshot(&pending.directory, &artifact, &operation_id, &cancel)
                .await
            {
                Ok(()) => {
                    outbox
                        .finish(&operation_id, "succeeded", None)
                        .map_err(|_| error("远端已完成，但本地状态保存失败，请核查"))?;
                    return Ok("已核实本次备份完成".into());
                }
                Err(_) => {
                    return Err(format!(
                        "备份结果未知，已停止重复上传；操作标识：{operation_id}"
                    ));
                }
            }
        }
        match client
            .publish_snapshot(&pending.directory, &artifact, &operation_id, &cancel)
            .await
        {
            Ok(()) => {
                outbox
                    .finish(&operation_id, "succeeded", None)
                    .map_err(|_| error("远端已完成，但本地状态保存失败，请核查"))?;
                Ok(format!("备份完成：{}", pending.directory))
            }
            Err(e) => {
                let state = match e {
                    coshell_backup::webdav::WebDavError::Conflict
                    | coshell_backup::webdav::WebDavError::Authentication
                    | coshell_backup::webdav::WebDavError::InvalidInput
                    | coshell_backup::webdav::WebDavError::InvalidUrl => "failed",
                    _ => "unknown",
                };
                let detail = format!("{e:?}");
                outbox
                    .finish(&operation_id, state, Some(&detail))
                    .map_err(|_| error("备份结果未知且状态保存失败，请查看记录"))?;
                Err(format!(
                    "备份未确认完成：{detail}；操作标识：{operation_id}"
                ))
            }
        }
    }
    pub async fn webdav_sync_status(
        &self,
        operation_id: String,
    ) -> Result<Option<crate::publication_outbox::PublicationStatus>, String> {
        let _gate = self.webdav_gate.lock().await;
        let path = self
            .webdav_path()?
            .parent()
            .ok_or_else(|| error("WebDAV 数据目录不可用"))?
            .join("webdav-outbox.sqlite");
        let mut db = rusqlite::Connection::open(path).map_err(|_| error("读取同步状态失败"))?;
        if let Some(status) = crate::directory_outbox::directory_status(&db, &operation_id)
            .map_err(|_| error("读取同步状态失败"))?
        {
            return Ok(Some(status));
        }
        crate::publication_outbox::Outbox::new(&mut db)
            .and_then(|o| o.status(&operation_id))
            .map_err(|_| error("读取同步状态失败"))
    }
    pub async fn webdav_reconcile(&self, operation_id: String) -> Result<String, String> {
        let _gate = self.webdav_gate.lock().await;
        let (client, base) = self.webdav_client()?;
        let path = self
            .webdav_path()?
            .parent()
            .ok_or_else(|| error("数据目录不可用"))?
            .join("webdav-outbox.sqlite");
        let mut db = rusqlite::Connection::open(path).map_err(|_| error("读取同步记录失败"))?;
        let key = self.outbox_key(&mut db)?;
        let mut outbox = crate::directory_outbox::DirectoryOutbox::new(&mut db, key)
            .map_err(|_| error("读取同步记录失败"))?;
        let pending = outbox
            .load(&operation_id)
            .map_err(|_| error("待核实内容或密钥不可用"))?
            .ok_or_else(|| error("此记录为旧格式或不存在，不会自动重发或解除锁定"))?;
        if pending.target
            != coshell_backup::webdav::canonical_target(&base).map_err(|_| error("目标无效"))?
            || pending.account
                != self
                    .webdav_config()?
                    .ok_or_else(|| error("请先配置 WebDAV"))?
                    .username
        {
            return Err(error("当前目标与原操作不一致，拒绝核实"));
        }
        if pending.state == "succeeded" {
            return Ok("备份已完成".into());
        }
        if !matches!(pending.state.as_str(), "running" | "unknown") {
            return Err(error("该操作无需核实，且不会重新发送"));
        }
        let artifact = coshell_backup::snapshot_manifest::SnapshotArtifact::unpack(&pending.body)
            .map_err(|_| error("本地待核实内容损坏"))?;
        client
            .verify_snapshot(
                &pending.directory,
                &artifact,
                &operation_id,
                &coshell_backup::webdav::Cancellation::new(),
            )
            .await
            .map_err(|_| error("尚不能证明此备份完成，保留结果未知；未重发也未删除远端数据"))?;
        outbox
            .finish(&operation_id, "succeeded", None)
            .map_err(|_| error("远端已完成但本地状态保存失败，请重试核实"))?;
        Ok("已只读核实此备份完成，未重复上传".into())
    }
    pub async fn webdav_recent_syncs(
        &self,
    ) -> Result<Vec<crate::publication_outbox::RecentPublication>, String> {
        let _gate = self.webdav_gate.lock().await;
        let path = self
            .webdav_path()?
            .parent()
            .ok_or_else(|| error("WebDAV 数据目录不可用"))?
            .join("webdav-outbox.sqlite");
        let mut db = rusqlite::Connection::open(path).map_err(|_| error("读取同步状态失败"))?;
        let mut current = crate::directory_outbox::directory_recent(&db)
            .map_err(|_| error("读取同步状态失败"))?;
        let legacy = crate::publication_outbox::Outbox::new(&mut db)
            .and_then(|o| o.recent())
            .map_err(|_| error("读取同步状态失败"))?;
        current.extend(legacy);
        current.truncate(20);
        Ok(current)
    }
    pub async fn webdav_preview_restore(
        &self,
        passphrase: String,
    ) -> Result<WebDavPreviewDto, String> {
        self.webdav_preview_version(passphrase, None).await
    }
    pub async fn webdav_list_versions(
        &self,
        cursor: Option<String>,
    ) -> Result<BackupVersionPage, String> {
        let _gate = self.webdav_gate.lock().await;
        let (client, _) = self.webdav_client()?;
        let cancel = coshell_backup::webdav::Cancellation::new();
        let before = client
            .index_etag(&cancel)
            .await
            .map_err(|e| format!("读取备份版本失败: {e:?}"))?;
        if before.is_none() {
            return Ok(BackupVersionPage {
                versions: Vec::new(),
                next_cursor: None,
            });
        }
        let versions = client
            .list_versions_page(cursor.as_deref(), &cancel)
            .await
            .map_err(|e| format!("读取备份版本失败: {e:?}"))?;
        let after = client
            .index_etag(&cancel)
            .await
            .map_err(|e| format!("读取备份版本失败: {e:?}"))?;
        if before != after {
            return Err(error("读取期间远端备份已变化，请重试"));
        }
        Ok(versions)
    }
    pub async fn webdav_list_snapshots(
        &self,
        cursor: Option<String>,
    ) -> Result<coshell_backup::webdav::SnapshotPage, String> {
        let _gate = self.webdav_gate.lock().await;
        let (client, _) = self.webdav_client()?;
        client
            .list_snapshots(
                cursor.as_deref(),
                &coshell_backup::webdav::Cancellation::new(),
            )
            .await
            .map_err(|e| format!("读取备份列表失败: {e:?}"))
    }
    pub async fn webdav_preview_snapshot(
        &self,
        passphrase: String,
        snapshot_id: String,
    ) -> Result<WebDavPreviewDto, String> {
        if let Some(legacy) = snapshot_id.strip_prefix("legacy:") {
            return self
                .webdav_preview_version(passphrase, Some(legacy.into()))
                .await;
        }
        let directory = snapshot_id
            .strip_prefix("snapshot:")
            .filter(|s| s.len() == 14 && s.bytes().all(|b| b.is_ascii_digit()))
            .ok_or_else(|| error("备份标识无效"))?;
        let _gate = self.webdav_gate.lock().await;
        let passphrase = zeroize::Zeroizing::new(passphrase);
        let (client, base) = self.webdav_client()?;
        let package = client
            .download_snapshot(directory, &coshell_backup::webdav::Cancellation::new())
            .await
            .map_err(|e| format!("下载备份失败: {e:?}"))?;
        let manifest = coshell_backup::snapshot_manifest::parse_manifest(&package.manifest)
            .map_err(|_| error("清单无效"))?;
        let protection = match manifest.protection {
            coshell_backup::snapshot_manifest::SnapshotProtection::Encrypted => "encrypted",
            coshell_backup::snapshot_manifest::SnapshotProtection::Plaintext => "none",
        };
        let (inspection, secrets) = crate::backup_inspection::inspect_directory_for_restore(
            &package.manifest,
            &package.complete,
            &package.connections_package,
            package.credentials_package.as_deref().map(|p| p.as_slice()),
            passphrase.as_bytes(),
        )
        .map_err(|e| format!("备份校验失败: {e:?}"))?;
        let mut db =
            rusqlite::Connection::open(self.webdav_path()?).map_err(|_| error("本地数据不可用"))?;
        let expected = coshell_storage::Repository::new(&mut db)
            .snapshot()
            .map_err(|_| error("本地数据不可用"))?;
        let token = id()?;
        let manifest_hash = coshell_backup::snapshot_manifest::sha256(&package.manifest);
        let preview = WebDavPreviewDto {
            token: token.clone(),
            snapshot_id: Some(snapshot_id.clone()),
            manifest_sha256: Some(manifest_hash.clone()),
            protection,
            connections: crate::backup_inspection::compare_connections(
                &inspection,
                &expected.connections,
            )
            .into_iter()
            .map(|mut c| {
                c.backup_has_secret = secrets.iter().any(|s| s.connection_id == c.id);
                c.local_has_secret = expected.secret_refs.iter().any(|s| s.connection_id == c.id);
                c
            })
            .collect(),
            groups: crate::backup_inspection::compare_groups(&inspection, &expected.groups),
            host_key_count: inspection.host_key_count,
        };
        let mut previews = self
            .webdav_previews
            .lock()
            .map_err(|_| error("恢复预览不可用"))?;
        previews.retain(|_, v| v.expires > std::time::Instant::now());
        if previews.len() >= 8 {
            return Err(error("恢复预览过多"));
        }
        previews.insert(
            token,
            WebDavPreviewState {
                inspection,
                secrets,
                expected,
                expires: std::time::Instant::now() + Duration::from_secs(300),
                baseline: None,
                source_target: Some(self.webdav_source_binding(&base)?),
                snapshot_binding: Some((snapshot_id, manifest_hash)),
            },
        );
        // The token owns the verified snapshot in memory; no future network
        // fetch can switch the user's selected directory before application.
        Ok(preview)
    }
    pub async fn webdav_preview_version(
        &self,
        passphrase: String,
        version_id: Option<String>,
    ) -> Result<WebDavPreviewDto, String> {
        let _gate = self.webdav_gate.lock().await;
        let passphrase = zeroize::Zeroizing::new(passphrase);
        let (client, base) = self.webdav_client()?;
        let cancel = coshell_backup::webdav::Cancellation::new();
        let before = client
            .index_etag(&cancel)
            .await
            .map_err(|e| format!("WebDAV 查询失败: {e:?}"))?;
        let encrypted = zeroize::Zeroizing::new(
            match version_id {
                Some(ref version) => client.download_version(version, &cancel).await,
                None => client.download_latest(&cancel).await,
            }
            .map_err(|e| format!("下载备份失败: {e:?}"))?,
        );
        let protection =
            coshell_backup::protection_mode(&encrypted).map_err(|_| error("备份格式无效"))?;
        let (inspection, secrets) =
            crate::backup_inspection::inspect_for_restore(&encrypted, passphrase.as_bytes())
                .map_err(|e| format!("备份校验失败: {e:?}"))?;
        let after = client
            .index_etag(&cancel)
            .await
            .map_err(|e| format!("WebDAV 查询失败: {e:?}"))?;
        if before.is_none() || before != after {
            return Err(error("预览期间远端备份已变化，请重试"));
        }
        let mut db = rusqlite::Connection::open(self.webdav_path()?)
            .map_err(|_| error("读取本地数据失败"))?;
        let expected = coshell_storage::Repository::new(&mut db)
            .snapshot()
            .map_err(|_| error("读取本地数据失败"))?;
        let token = id()?;
        let preview = WebDavPreviewDto {
            token: token.clone(),
            snapshot_id: version_id.clone(),
            manifest_sha256: None,
            protection,
            connections: crate::backup_inspection::compare_connections(
                &inspection,
                &expected.connections,
            )
            .into_iter()
            .map(|mut c| {
                c.backup_has_secret = secrets.iter().any(|s| s.connection_id == c.id);
                c.local_has_secret = expected.secret_refs.iter().any(|s| s.connection_id == c.id);
                c
            })
            .collect(),
            groups: crate::backup_inspection::compare_groups(&inspection, &expected.groups),
            host_key_count: inspection.host_key_count,
        };
        let mut previews = self
            .webdav_previews
            .lock()
            .map_err(|_| error("恢复预览不可用"))?;
        previews.retain(|_, v| v.expires > std::time::Instant::now());
        if previews.len() >= 8 {
            return Err(error("恢复预览过多"));
        }
        previews.insert(
            token,
            WebDavPreviewState {
                inspection,
                secrets,
                expected,
                expires: std::time::Instant::now() + Duration::from_secs(300),
                source_target: Some(self.webdav_source_binding(&base)?),
                baseline: Some((base, before.unwrap())),
                snapshot_binding: None,
            },
        );
        Ok(preview)
    }
    pub async fn webdav_apply_restore(
        &self,
        token: String,
        choices: Vec<WebDavChoiceDto>,
        confirm_groups: bool,
    ) -> Result<(), String> {
        let _gate = self.webdav_gate.lock().await;
        let state = self.state.lock().await;
        if !state.sessions.is_empty()
            || !state
                .connecting
                .lock()
                .map_err(|_| error("会话状态不可用"))?
                .is_empty()
        {
            return Err(error("请先关闭所有会话和连接操作"));
        }
        let previews = self
            .webdav_previews
            .lock()
            .map_err(|_| error("恢复预览不可用"))?;
        let preview = previews
            .get(&token)
            .ok_or_else(|| error("恢复预览已失效"))?;
        if preview
            .snapshot_binding
            .as_ref()
            .is_some_and(|(id, hash)| !id.starts_with("snapshot:") || hash.len() != 64)
        {
            return Err(error("恢复来源绑定无效"));
        }
        if preview.expires <= std::time::Instant::now() {
            return Err(error("恢复预览已过期"));
        }
        if let Some(source) = &preview.source_target {
            let (_, current) = self.webdav_client()?;
            if source != &self.webdav_source_binding(&current)? {
                return Err(error("WebDAV 目标已变化，请重新预览"));
            }
        }
        let mut selected = HashMap::new();
        for c in choices {
            let choice = match c.action.as_str() {
                "keep" => crate::backup_inspection::RestoreChoice::Keep,
                "replace" => crate::backup_inspection::RestoreChoice::Replace,
                "copy" => crate::backup_inspection::RestoreChoice::Copy {
                    id: c.copy_id.ok_or_else(|| error("副本 ID 无效"))?,
                    name: c.copy_name.ok_or_else(|| error("副本名称无效"))?,
                },
                _ => return Err(error("恢复选择无效")),
            };
            if selected.insert(c.id, choice).is_some() {
                return Err(error("恢复选择重复"));
            }
        }
        let desired = crate::backup_inspection::prepare_restore_with_groups(
            &preview.inspection,
            &preview.expected,
            &selected,
            confirm_groups,
        )
        .map_err(|e| format!("恢复计划无效: {e:?}"))?;
        drop(previews);
        let preview = self
            .webdav_previews
            .lock()
            .map_err(|_| error("恢复预览不可用"))?
            .remove(&token)
            .ok_or_else(|| error("恢复预览已失效"))?;
        let mut db =
            rusqlite::Connection::open(self.webdav_path()?).map_err(|_| error("本地数据不可用"))?;
        coshell_storage::migrate(&mut db).map_err(|_| error("本地数据不可用"))?;
        let incoming = map_restore_secrets(&preview.secrets, &selected)?;
        if incoming.is_empty() {
            coshell_storage::Repository::new(&mut db)
                .restore_snapshot_if_matches(&preview.expected, &desired)
                .map_err(|_| error("恢复结果可能未知或存在冲突，请重新预览并核对本地数据"))?;
        } else {
            let store = self.webdav_store()?;
            crate::restore_secrets::apply(
                self.webdav_path()?,
                &store,
                &token,
                &preview.expected,
                &desired,
                &incoming,
            )
            .map_err(|e| format!("恢复结果：{e:?}；操作标识：{token}，请查询状态并核对本地数据"))?;
        }
        if let Some((base, etag)) = preview.baseline {
            set_webdav_baseline(self.webdav_path()?, &base, &etag)
                .map_err(|_| error("恢复已完成，但同步基准保存失败；请重新预览"))?;
        }
        drop(state);
        Ok(())
    }
    pub fn webdav_restore_status(
        &self,
        operation_id: &str,
    ) -> Result<Option<crate::restore_secrets::RestoreStatus>, String> {
        let store = self.webdav_store()?;
        crate::restore_secrets::status(self.webdav_path()?, &store, operation_id)
            .map_err(|_| error("恢复状态不可用"))
    }
    pub fn open_default() -> Result<Self, String> {
        let dir = coshell_platform::data_dir::default_data_dir()
            .map_err(|_| error("无法确定数据目录"))?;
        std::fs::create_dir_all(&dir).map_err(|_| error("无法创建数据目录"))?;
        Self::open(&dir.join("coshell.sqlite"))
    }
    pub fn open(path: &Path) -> Result<Self, String> {
        use fs2::FileExt;
        let parent = path.parent().ok_or_else(|| error("无效的数据目录"))?;
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(parent.join("desktop-runtime.lock"))
            .map_err(|_| error("无法锁定数据目录"))?;
        lock.try_lock_exclusive()
            .map_err(|_| error("CoShell 已在使用此数据目录"))?;
        let db = rusqlite::Connection::open(path).map_err(|_| error("无法打开连接数据库"))?;
        let mut this = Self::from_connection(
            db,
            FeatureFlags::default().with_enabled(Feature::Connections),
        )?;
        let store = coshell_platform::secret_store::PlatformSecretStore::for_data_dir(parent)
            .map_err(|_| error("凭据存储不可用"))?;
        crate::ssh_key_store::recover_all(path, &store).map_err(error)?;
        this.path = Some(path.to_path_buf());
        this._runtime_lock = Some(lock);
        Ok(this)
    }
    pub fn from_connection(db: rusqlite::Connection, flags: FeatureFlags) -> Result<Self, String> {
        Ok(Self {
            service: ConnectionService::new(db, flags).map_err(|e| e.code)?,
            #[cfg(test)]
            store_override: None,
            path: None,
            state: AsyncMutex::new(State {
                sessions: HashMap::new(),
                challenges: HashMap::new(),
                connecting: Arc::new(Mutex::new(HashMap::new())),
            }),
            gate: CommandGate::new(Duration::from_secs(120)).map_err(|_| error("初始化失败"))?,
            saved_lock: Mutex::new(()),
            saved_pending: Mutex::new(HashMap::new()),
            operations: Mutex::new(HashMap::new()),
            passwords: passwords::PasswordOffers::default(),
            overview_operations: Arc::new(Mutex::new(OverviewStore::default())),
            overview_cancelled: Mutex::new(HashMap::new()),
            webdav_previews: Mutex::new(HashMap::new()),
            webdav_gate: AsyncMutex::new(()),
            webdav_tests: crate::webdav_test::WebDavTestCoordinator::default(),
            remote_files: crate::remote_files::MutationGate::default(),
            _runtime_lock: None,
        })
    }
    pub async fn list(&self) -> Result<Vec<ConnectionDto>, String> {
        let _state = self.state.lock().await;
        self.service
            .list()
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(|e| e.code)
    }
    /// Collect only via an existing authenticated session and a separate fixed exec channel.
    pub async fn server_overview(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<coshell_types::ServerOverviewSnapshot, String> {
        if operation_id.is_empty() || operation_id.len() > 256 {
            return Err(error("无效采集操作"));
        }
        let state = self.state.lock().await;
        let session = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| error("会话不存在"))?;
        if session.closed.load(Ordering::Acquire) || session.cancel.is_cancelled() {
            return Err(error("会话已断开"));
        }
        let record = self
            .service
            .get(&session.connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("连接不存在"))?;
        if record.version != session.connection_version {
            return Err(error("连接配置已变化，请重新连接"));
        }
        let cancel = Cancellation::new();
        let shared = {
            let mut cancelled = self
                .overview_cancelled
                .lock()
                .map_err(|_| error("采集不可用"))?;
            cancelled.retain(|_, (_, until)| *until > std::time::Instant::now());
            if let Some((owner, _)) = cancelled.get(operation_id) {
                return Err(if owner == session_id {
                    error("采集已取消")
                } else {
                    error("无效采集操作")
                });
            }
            let mut operations = self
                .overview_operations
                .lock()
                .map_err(|_| error("采集不可用"))?;
            if let Some((owner, result)) = operations.receipts.get(operation_id) {
                return if owner == session_id {
                    result.clone()
                } else {
                    Err(error("无效采集操作"))
                };
            }
            if let Some(entry) = operations
                .active
                .values()
                .find(|entry| entry.operation_id == operation_id)
            {
                if entry.session_id != session_id {
                    return Err(error("无效采集操作"));
                }
                Some(entry.sender.subscribe())
            } else {
                if operations.active.len() >= 64
                    || operations.active.contains_key(&session.connection_id)
                {
                    return Err(error("采集正在进行"));
                }
                let (sender, _) = tokio::sync::watch::channel(None);
                operations.active.insert(
                    session.connection_id.clone(),
                    OverviewOperation {
                        session_id: session_id.into(),
                        operation_id: operation_id.into(),
                        cancellation: cancel.clone(),
                        sender,
                    },
                );
                None
            }
        };
        drop(state);
        if let Some(mut receiver) = shared {
            loop {
                let received = receiver.borrow_and_update().clone();
                if let Some(result) = received {
                    let snapshot = result?;
                    let state = self.state.lock().await;
                    if session.cancel.is_cancelled()
                        || session.closed.load(Ordering::Acquire)
                        || !state
                            .sessions
                            .get(session_id)
                            .is_some_and(|current| Arc::ptr_eq(current, &session))
                        || self
                            .service
                            .get(&session.connection_id)
                            .map_err(|e| e.code)?
                            .is_none_or(|record| record.version != session.connection_version)
                    {
                        return Err(error("采集结果已过期"));
                    }
                    return Ok(snapshot);
                }
                receiver.changed().await.map_err(|_| error("采集已中断"))?;
            }
        }
        let mut guard = OverviewGuard {
            connection_id: session.connection_id.clone(),
            operation_id: operation_id.into(),
            operations: self.overview_operations.clone(),
            completed: false,
        };
        let timestamp: chrono::DateTime<chrono::Utc> = std::time::SystemTime::now().into();
        let collect = async {
            let mut exec = run_cancellable(
                Duration::from_secs(10),
                &cancel,
                session.connection.remote_exec(),
            )
            .await?;
            coshell_drivers::server_overview::collect(
                exec.as_mut(),
                &session.connection_id,
                &timestamp.to_rfc3339(),
                Duration::from_secs(10),
                &cancel,
            )
            .await
        };
        let result = run_cancellable(Duration::from_secs(22), &session.cancel, collect)
            .await
            .map_err(|e| match e {
                SshError::Cancelled => error("采集已取消"),
                SshError::Timeout => error("采集超时"),
                _ => error("服务器概览采集失败"),
            });
        let snapshot = match result {
            Ok(value) => value,
            Err(err) => {
                guard.finish(Err(err.clone()));
                return Err(err);
            }
        };
        let state = self.state.lock().await;
        if cancel.is_cancelled()
            || session.cancel.is_cancelled()
            || session.closed.load(Ordering::Acquire)
            || !state
                .sessions
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
            || self
                .service
                .get(&session.connection_id)
                .map_err(|e| e.code)?
                .is_none_or(|record| record.version != session.connection_version)
        {
            let err = error("采集结果已过期");
            guard.finish(Err(err.clone()));
            return Err(err);
        }
        guard.finish(Ok(snapshot.clone()));
        Ok(snapshot)
    }
    pub async fn server_overview_cancel(
        &self,
        session_id: &str,
        operation_id: &str,
    ) -> Result<(), String> {
        if operation_id.is_empty() || operation_id.len() > 256 {
            return Err(error("无效采集操作"));
        }
        let state = self.state.lock().await;
        if !state.sessions.contains_key(session_id) {
            return Err(error("会话不存在"));
        }
        let mut cancelled = self
            .overview_cancelled
            .lock()
            .map_err(|_| error("采集不可用"))?;
        cancelled.retain(|_, (_, until)| *until > std::time::Instant::now());
        let operations = self
            .overview_operations
            .lock()
            .map_err(|_| error("采集不可用"))?;
        if let Some((owner, _)) = operations.receipts.get(operation_id) {
            return if owner == session_id {
                Ok(())
            } else {
                Err(error("无效采集操作"))
            };
        }
        if operations
            .active
            .values()
            .any(|entry| entry.operation_id == operation_id && entry.session_id != session_id)
        {
            return Err(error("无效采集操作"));
        }
        if let Some((owner, _)) = cancelled.get(operation_id) {
            if owner != session_id {
                return Err(error("无效采集操作"));
            }
        } else {
            if cancelled.len() >= 256 {
                return Err(error("采集取消容量已满"));
            }
            cancelled.insert(
                operation_id.into(),
                (
                    session_id.into(),
                    std::time::Instant::now() + Duration::from_secs(60),
                ),
            );
        }
        if let Some(entry) = operations
            .active
            .values()
            .find(|entry| entry.operation_id == operation_id)
        {
            if entry.session_id != session_id {
                return Err(error("无效采集操作"));
            }
            entry.cancellation.cancel();
        }
        Ok(())
    }
    pub async fn save(&self, input: ConnectionDto) -> Result<ConnectionDto, String> {
        let state = self.state.lock().await;
        if state.sessions.values().any(|s| {
            s.connection_id == input.id
                && (s.inflight.load(Ordering::Acquire) || s.file_inflight.load(Ordering::Acquire))
        }) {
            return Err(error("该连接正在执行命令，请稍后再修改"));
        }
        let key = input.id.clone();
        let record: ConnectionRecord = input.into();
        let previous = self.service.get(&key).map_err(|e| e.code)?;
        self.service.save(&record).map_err(|e| e.code)?;
        if previous
            .as_ref()
            .is_some_and(|previous| !passwords::same_target(previous, &record))
        {
            self.passwords.discard_connection(&key);
            self.recover_password_cleanup();
        }
        self.service
            .get(&key)
            .map_err(|e| e.code)?
            .map(Into::into)
            .ok_or_else(|| error("CONNECTION_NOT_FOUND"))
    }
    pub async fn ssh_key_saved(&self, connection_id: &str) -> Result<bool, String> {
        let _state = self.state.lock().await;
        let record = self
            .service
            .get(connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("CONNECTION_NOT_FOUND"))?;
        let path = self.path.as_ref().ok_or_else(|| error("凭据存储不可用"))?;
        let store = coshell_platform::secret_store::PlatformSecretStore::for_data_dir(
            path.parent().ok_or_else(|| error("凭据存储不可用"))?,
        )
        .map_err(|_| error("凭据存储不可用"))?;
        crate::ssh_key_store::exists(path, &store, &record).map_err(error)
    }
    pub async fn ssh_key_save(
        &self,
        connection_id: &str,
        key: String,
        passphrase: Option<String>,
    ) -> Result<(), String> {
        let key = zeroize::Zeroizing::new(key);
        let passphrase = zeroize::Zeroizing::new(passphrase.unwrap_or_default());
        let state = self.state.lock().await;
        if state
            .sessions
            .values()
            .any(|s| s.connection_id == connection_id)
            || state
                .connecting
                .lock()
                .map_err(|_| error("凭据存储不可用"))?
                .contains_key(connection_id)
        {
            return Err(error("CONNECTION_ACTIVE"));
        }
        let record = self
            .service
            .get(connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("CONNECTION_NOT_FOUND"))?;
        let path = self.path.as_ref().ok_or_else(|| error("凭据存储不可用"))?;
        let store = coshell_platform::secret_store::PlatformSecretStore::for_data_dir(
            path.parent().ok_or_else(|| error("凭据存储不可用"))?,
        )
        .map_err(|_| error("凭据存储不可用"))?;
        let operation = id()?;
        crate::ssh_key_store::save(path, &store, &record, &key, &passphrase, &operation).map_err(
            |e| {
                if e == "凭据保存结果未知" {
                    format!("{e}；操作标识：{operation}")
                } else {
                    error(e)
                }
            },
        )
    }
    pub async fn ssh_key_operation_status(
        &self,
        connection_id: &str,
        operation: &str,
    ) -> Result<String, String> {
        let _state = self.state.lock().await;
        self.service
            .get(connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("CONNECTION_NOT_FOUND"))?;
        let path = self.path.as_ref().ok_or_else(|| error("凭据存储不可用"))?;
        let store = coshell_platform::secret_store::PlatformSecretStore::for_data_dir(
            path.parent().ok_or_else(|| error("凭据存储不可用"))?,
        )
        .map_err(|_| error("凭据存储不可用"))?;
        crate::ssh_key_store::operation_status(path, &store, connection_id, operation)
            .map(str::to_owned)
            .map_err(error)
    }
    pub async fn ssh_key_delete(&self, connection_id: &str) -> Result<(), String> {
        let state = self.state.lock().await;
        if state
            .sessions
            .values()
            .any(|s| s.connection_id == connection_id)
            || state
                .connecting
                .lock()
                .map_err(|_| error("凭据存储不可用"))?
                .contains_key(connection_id)
        {
            return Err(error("CONNECTION_ACTIVE"));
        }
        self.service
            .get(connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("CONNECTION_NOT_FOUND"))?;
        let path = self.path.as_ref().ok_or_else(|| error("凭据存储不可用"))?;
        let store = coshell_platform::secret_store::PlatformSecretStore::for_data_dir(
            path.parent().ok_or_else(|| error("凭据存储不可用"))?,
        )
        .map_err(|_| error("凭据存储不可用"))?;
        crate::ssh_key_store::delete(path, &store, connection_id, &id()?).map_err(error)
    }
    pub async fn copy(&self, source: &str, id: &str, name: &str) -> Result<ConnectionDto, String> {
        let _state = self.state.lock().await;
        self.service
            .copy(source, id, name)
            .map(Into::into)
            .map_err(|e| e.code)
    }
    pub async fn delete(&self, id: &str) -> Result<bool, String> {
        let mut state = self.state.lock().await;
        if state.sessions.values().any(|s| s.connection_id == id)
            || state
                .connecting
                .lock()
                .map_err(|_| error("CONNECTION_STORAGE"))?
                .contains_key(id)
        {
            return Err(error("CONNECTION_ACTIVE"));
        }
        let removed = if self.path.is_some() {
            self.service.delete_with_password_cleanup(id)
        } else {
            self.service.delete(id)
        }
        .map_err(|e| e.code)?;
        if removed {
            state.challenges.remove(id);
            self.passwords.discard_connection(id);
            self.recover_password_cleanup();
        }
        Ok(removed)
    }
    pub async fn groups(&self) -> Result<Vec<GroupDto>, String> {
        let _state = self.state.lock().await;
        self.service
            .list_groups()
            .map(|v| v.into_iter().map(Into::into).collect())
            .map_err(|e| e.code)
    }
    pub async fn save_group(&self, input: GroupDto) -> Result<(), String> {
        let _state = self.state.lock().await;
        self.service.save_group(&input.into()).map_err(|e| e.code)
    }
    pub async fn delete_group(&self, id: &str) -> Result<bool, String> {
        let state = self.state.lock().await;
        let members = self
            .service
            .list()
            .map_err(|e| e.code)?
            .into_iter()
            .filter(|v| v.group_id.as_deref() == Some(id))
            .map(|v| v.id)
            .collect::<Vec<_>>();
        let connecting = state
            .connecting
            .lock()
            .map_err(|_| error("CONNECTION_STORAGE"))?;
        if members.iter().any(|id| {
            connecting.contains_key(id) || state.sessions.values().any(|s| s.connection_id == *id)
        }) {
            return Err(error("CONNECTION_ACTIVE"));
        }
        self.service.delete_group(id).map_err(|e| e.code)
    }
    pub async fn open_ssh(
        &self,
        connection_id: String,
        password: Option<String>,
        accepted_fingerprint: Option<String>,
        operation_id: String,
        private_key: Option<String>,
        passphrase: Option<String>,
    ) -> Result<OpenResult, String> {
        self.open_ssh_with_saved(
            connection_id,
            password,
            accepted_fingerprint,
            operation_id,
            private_key,
            passphrase,
            false,
        )
        .await
    }
    pub async fn open_ssh_with_saved(
        &self,
        connection_id: String,
        password: Option<String>,
        accepted_fingerprint: Option<String>,
        operation_id: String,
        private_key: Option<String>,
        passphrase: Option<String>,
        use_saved_key: bool,
    ) -> Result<OpenResult, String> {
        self.open_ssh_with_credentials(
            connection_id,
            password,
            accepted_fingerprint,
            operation_id,
            private_key,
            passphrase,
            use_saved_key,
            false,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    pub async fn open_ssh_with_credentials(
        &self,
        connection_id: String,
        password: Option<String>,
        accepted_fingerprint: Option<String>,
        operation_id: String,
        private_key: Option<String>,
        passphrase: Option<String>,
        use_saved_key: bool,
        use_saved_password: bool,
    ) -> Result<OpenResult, String> {
        let password = password.map(zeroize::Zeroizing::new);
        let private_key = private_key.map(zeroize::Zeroizing::new);
        let passphrase = passphrase.map(zeroize::Zeroizing::new);
        if operation_id.is_empty() || operation_id.len() > 128 {
            return Err(error("无效的连接操作"));
        }
        let cancellation = {
            let mut ops = self.operations.lock().map_err(|_| error("连接操作失败"))?;
            let now = std::time::Instant::now();
            ops.retain(|_, v| v.deadline > now);
            if let Some(op) = ops.get_mut(&operation_id) {
                if op.started || op.cancellation.is_cancelled() {
                    return Err(error("连接已取消或操作已使用"));
                }
                op.started = true;
                op.cancellation.clone()
            } else {
                if ops.len() >= 1024 {
                    return Err(error("连接操作过多"));
                }
                let cancellation = Cancellation::new();
                ops.insert(
                    operation_id.clone(),
                    OpenOperation {
                        cancellation: cancellation.clone(),
                        deadline: now + Duration::from_secs(300),
                        session_id: None,
                        started: true,
                    },
                );
                cancellation
            }
        };
        let mut state = self.state.lock().await;
        if cancellation.is_cancelled() {
            return Err(error("连接已取消"));
        }
        if state.sessions.len() >= 32 {
            return Err(error("会话数量已达上限"));
        }
        let record = self
            .service
            .get(&connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("CONNECTION_NOT_FOUND"))?;
        if use_saved_password
            && (record.authentication != "password"
                || use_saved_key
                || password.is_some()
                || private_key.is_some()
                || passphrase.is_some())
        {
            return Err(error("SSH_PASSWORD_UNAVAILABLE"));
        }
        if use_saved_key
            && (record.authentication != "private_key"
                || private_key.is_some()
                || passphrase.is_some()
                || password.is_some())
        {
            return Err(error("保存的私钥不能与临时认证信息同时使用"));
        }
        {
            let mut connecting = state
                .connecting
                .lock()
                .map_err(|_| error("CONNECTION_STORAGE"))?;
            *connecting.entry(connection_id.clone()).or_insert(0) += 1;
        }
        let _connecting = ConnectingGuard {
            connection_id: connection_id.clone(),
            connecting: state.connecting.clone(),
        };
        let authentication = match record.authentication.as_str() {
            "password" if use_saved_password => Authentication::Password(
                self.saved_password(&record)?
                    .ok_or_else(|| error("SSH_PASSWORD_UNAVAILABLE"))?,
            ),
            "password" => Authentication::Password(Secret::new(
                password
                    .as_ref()
                    .ok_or_else(|| error("请输入密码"))?
                    .as_bytes()
                    .to_vec(),
            )),
            "ssh_agent" => Authentication::Agent,
            "private_key" => {
                if use_saved_key {
                    let path = self.path.as_ref().ok_or_else(|| error("凭据存储不可用"))?;
                    let store = coshell_platform::secret_store::PlatformSecretStore::for_data_dir(
                        path.parent().ok_or_else(|| error("凭据存储不可用"))?,
                    )
                    .map_err(|_| error("凭据存储不可用"))?;
                    let (key, passphrase) = crate::ssh_key_store::load(path, &store, &record)
                        .map_err(error)?
                        .ok_or_else(|| error("尚未保存私钥"))?;
                    Authentication::PrivateKey { key, passphrase }
                } else {
                    let key = private_key.as_ref().ok_or_else(|| error("请输入私钥"))?;
                    if key.is_empty()
                        || key.len() > 1024 * 1024
                        || passphrase.as_ref().is_some_and(|p| p.len() > 4096)
                    {
                        return Err(error("私钥或口令无效"));
                    }
                    Authentication::PrivateKey {
                        key: Secret::new(key.as_bytes().to_vec()),
                        passphrase: passphrase
                            .as_ref()
                            .filter(|v| !v.is_empty())
                            .map(|v| Secret::new(v.as_bytes().to_vec())),
                    }
                }
            }
            _ => return Err(error("暂不支持该认证方式")),
        };
        let path = self.path.as_ref().ok_or_else(|| error("指纹存储不可用"))?;
        let mut db = rusqlite::Connection::open(path).map_err(|_| error("指纹存储不可用"))?;
        coshell_storage::migrate(&mut db).map_err(|_| error("指纹存储不可用"))?;
        let expected = match accepted_fingerprint {
            Some(fingerprint) => {
                let challenge = state
                    .challenges
                    .remove(&connection_id)
                    .ok_or_else(|| error("主机指纹确认已失效"))?;
                if challenge.deadline <= std::time::Instant::now()
                    || challenge.version != record.version
                    || challenge.key.host != record.host
                    || challenge.key.port != record.port
                    || challenge.key.fingerprint != fingerprint
                {
                    return Err(error("主机指纹或连接已变化，请重新连接"));
                }
                Some(challenge.key)
            }
            None => {
                state.challenges.remove(&connection_id);
                None
            }
        };
        drop(state);
        let prompt = Arc::new(Prompt {
            expected,
            observed: Mutex::new(None),
        });
        let verifier = TofuVerifier::new(Arc::new(SqliteHostKeyStore::new(db)), prompt.clone());
        let connector = RusshConnector::default();
        let result = run_cancellable(
            Duration::from_secs(20),
            &cancellation,
            connector.connect(
                ConnectionRequest {
                    host: record.host.clone(),
                    port: record.port,
                    username: record.username.clone(),
                    authentication,
                },
                &verifier,
            ),
        )
        .await;
        let connection = match result {
            Ok(c) => c,
            Err(SshError::HostKeyRejected) => {
                let observed = prompt
                    .observed
                    .lock()
                    .map_err(|_| error("指纹处理失败"))?
                    .clone();
                if let Some(key) = observed {
                    let mut state = self.state.lock().await;
                    if cancellation.is_cancelled()
                        || self
                            .service
                            .get(&connection_id)
                            .map_err(|e| e.code)?
                            .is_none_or(|v| v.version != record.version)
                    {
                        return Err(error("连接已取消或配置已变化"));
                    }
                    state.challenges.insert(
                        connection_id.clone(),
                        Challenge {
                            key: key.clone(),
                            version: record.version,
                            deadline: std::time::Instant::now() + Duration::from_secs(120),
                        },
                    );
                    return Ok(OpenResult::HostKeyConfirmation {
                        host: key.host,
                        port: key.port,
                        algorithm: key.algorithm,
                        fingerprint: key.fingerprint,
                    });
                }
                return Err(error("主机密钥未被接受"));
            }
            Err(SshError::AuthenticationFailed) if use_saved_password => {
                return Err(error("SSH_SAVED_PASSWORD_REJECTED"));
            }
            Err(e) => return Err(ssh_error(e)),
        };
        let channel = if record.production {
            None
        } else {
            Some(Arc::<dyn TerminalChannel>::from(
                run_cancellable(
                    Duration::from_secs(15),
                    &cancellation,
                    connection.open_terminal(TerminalSize {
                        columns: 100,
                        rows: 30,
                    }),
                )
                .await
                .map_err(ssh_error)?,
            ))
        };
        let session_id = id()?;
        let password_save_token = {
            let mut state = self.state.lock().await;
            if state.sessions.len() >= 32
                || self
                    .service
                    .get(&connection_id)
                    .map_err(|e| e.code)?
                    .is_none_or(|v| v.version != record.version)
            {
                return Err(error("连接配置已变化或会话已满"));
            }
            let mut ops = self.operations.lock().map_err(|_| error("连接操作失败"))?;
            let op = ops
                .get_mut(&operation_id)
                .ok_or_else(|| error("连接操作失效"))?;
            if op.cancellation.is_cancelled() {
                return Err(error("连接已取消"));
            }
            let offer = if record.authentication == "password" {
                password.as_ref().and_then(|password| {
                    self.offer_password(&session_id, &record, password.as_bytes())
                })
            } else {
                None
            };
            op.session_id = Some(session_id.clone());
            state.sessions.insert(
                session_id.clone(),
                Arc::new(Session {
                    connection_id,
                    connection_version: record.version,
                    channel,
                    connection,
                    reader: AsyncMutex::new(Vec::new()),
                    write_unknown: AtomicBool::new(false),
                    exec_output: AsyncMutex::new(VecDeque::new()),
                    inflight: AtomicBool::new(false),
                    file_inflight: AtomicBool::new(false),
                    closed: AtomicBool::new(false),
                    cancel: Cancellation::new(),
                    close_lock: AsyncMutex::new(()),
                    resize_lock: AsyncMutex::new(()),
                    inflight_done: tokio::sync::Notify::new(),
                }),
            );
            offer
        };
        Ok(OpenResult::Connected {
            session_id,
            password_save_token,
        })
    }
    pub async fn cancel_open(&self, operation_id: &str) -> Result<(), String> {
        if operation_id.is_empty() || operation_id.len() > 128 {
            return Err(error("无效的连接操作"));
        }
        let session_id = {
            let mut ops = self.operations.lock().map_err(|_| error("连接操作失败"))?;
            let now = std::time::Instant::now();
            ops.retain(|_, v| v.deadline > now);
            if !ops.contains_key(operation_id) && ops.len() >= 1024 {
                return Err(error("连接操作过多"));
            }
            let op = ops
                .entry(operation_id.to_owned())
                .or_insert_with(|| OpenOperation {
                    cancellation: Cancellation::new(),
                    deadline: now + Duration::from_secs(300),
                    session_id: None,
                    started: false,
                });
            op.cancellation.cancel();
            op.session_id.clone()
        };
        if let Some(id) = session_id {
            self.close(&id).await?;
        }
        Ok(())
    }
    pub async fn read(&self, session_id: &str) -> Result<ReadResult, String> {
        let session = {
            let state = self.state.lock().await;
            state
                .sessions
                .get(session_id)
                .cloned()
                .ok_or_else(|| error("会话不存在"))?
        };
        let Some(channel) = session.channel.as_ref() else {
            return Ok(ReadResult {
                output: session
                    .exec_output
                    .lock()
                    .await
                    .pop_front()
                    .unwrap_or_default(),
                closed: session.closed.load(Ordering::Acquire)
                    || (session.write_unknown.load(Ordering::Acquire)
                        && !session.inflight.load(Ordering::Acquire)),
            });
        };
        let mut pending = session.reader.lock().await;
        match tokio::time::timeout(Duration::from_secs(1), channel.read()).await {
            Err(_) => Ok(ReadResult {
                output: String::new(),
                closed: false,
            }),
            Ok(Err(_)) => {
                self.passwords.discard_session(session_id);
                Err(error("终端读取失败"))
            }
            Ok(Ok(None)) => {
                self.passwords.discard_session(session_id);
                let output = String::from_utf8_lossy(&pending).into_owned();
                pending.clear();
                Ok(ReadResult {
                    output,
                    closed: true,
                })
            }
            Ok(Ok(Some(bytes))) => {
                pending.extend_from_slice(&bytes);
                let (output, used) = match std::str::from_utf8(&pending) {
                    Ok(s) => (s.to_owned(), pending.len()),
                    Err(e) if e.error_len().is_none() => (
                        String::from_utf8_lossy(&pending[..e.valid_up_to()]).into_owned(),
                        e.valid_up_to(),
                    ),
                    Err(_) => (
                        String::from_utf8_lossy(&pending).into_owned(),
                        pending.len(),
                    ),
                };
                pending.drain(..used);
                Ok(ReadResult {
                    output,
                    closed: false,
                })
            }
        }
    }
    /// Send transparent terminal input only for a current, non-production
    /// connection without the always-confirm policy. All checks use local state.
    pub async fn write_terminal(&self, session_id: &str, data: String) -> Result<(), String> {
        if data.is_empty() || data.len() > 64 * 1024 {
            return Err(error("终端输入无效"));
        }
        let state = self.state.lock().await;
        let session = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| error("会话不存在"))?;
        if session.closed.load(Ordering::Acquire) || session.cancel.is_cancelled() {
            return Err(error("会话已断开，请重新连接"));
        }
        let record = self
            .service
            .get(&session.connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("连接不存在"))?;
        if record.version != session.connection_version
            || record.production
            || record.always_confirm
        {
            return Err(error(
                "当前连接不允许直接终端输入，请重新连接或使用命令确认",
            ));
        }
        if session.write_unknown.load(Ordering::Acquire) {
            return Err(error("上次发送结果未知，请关闭并重新连接"));
        }
        if session.inflight.load(Ordering::Acquire) {
            return Err(error("该会话正在发送输入"));
        }
        let channel = session.channel.clone().ok_or_else(|| error("终端不可用"))?;
        session.inflight.store(true, Ordering::Release);
        session.write_unknown.store(true, Ordering::Release);
        let _guard = InflightGuard(session.clone());
        drop(state);
        match run_cancellable(
            Duration::from_secs(10),
            &session.cancel,
            channel.write(data.as_bytes()),
        )
        .await
        {
            Ok(()) => {
                session.write_unknown.store(false, Ordering::Release);
                Ok(())
            }
            Err(_) => Err(error("发送结果未知，请关闭并重新连接")),
        }
    }

    pub async fn resize_terminal(
        &self,
        session_id: &str,
        cols: u16,
        rows: u16,
    ) -> Result<(), String> {
        if cols == 0 || rows == 0 || cols > 1000 || rows > 1000 {
            return Err(error("终端尺寸无效"));
        }
        let state = self.state.lock().await;
        let session = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| error("会话不存在"))?;
        if session.closed.load(Ordering::Acquire) || session.cancel.is_cancelled() {
            return Err(error("会话已断开，请重新连接"));
        }
        let record = self
            .service
            .get(&session.connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("连接不存在"))?;
        if record.version != session.connection_version || record.production {
            return Err(error("当前会话不支持调整终端尺寸"));
        }
        let channel = session.channel.clone().ok_or_else(|| error("终端不可用"))?;
        drop(state);
        let _resize = session.resize_lock.lock().await;
        // Recheck after waiting: the connection may have closed or been edited.
        let state = self.state.lock().await;
        if session.closed.load(Ordering::Acquire)
            || session.cancel.is_cancelled()
            || !state
                .sessions
                .get(session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &session))
        {
            return Err(error("会话已断开，请重新连接"));
        }
        let record = self
            .service
            .get(&session.connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("连接不存在"))?;
        if record.version != session.connection_version || record.production {
            return Err(error("当前会话不支持调整终端尺寸"));
        }
        drop(state);
        run_cancellable(
            Duration::from_secs(10),
            &session.cancel,
            channel.resize(TerminalSize {
                columns: cols,
                rows,
            }),
        )
        .await
        .map_err(|_| error("终端尺寸同步失败"))
    }

    /// Runs a stored definition on a separate exec channel. The terminal PTY
    /// is never written, so existing interactive input cannot be combined.
    pub async fn run_saved(
        &self,
        session_id: &str,
        saved_id: &str,
        expected_version: u64,
        confirm_token: Option<String>,
    ) -> Result<SavedRunResult, String> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| error("会话不存在"))?;
        if session.closed.load(Ordering::Acquire)
            || session.cancel.is_cancelled()
            || session.write_unknown.load(Ordering::Acquire)
        {
            return Err(error("会话已断开或结果未知，请重新连接"));
        }
        if session.inflight.load(Ordering::Acquire) {
            return Err(error("该会话正在执行命令"));
        }
        let record = self
            .service
            .get(&session.connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("连接不存在"))?;
        if record.version != session.connection_version {
            return Err(error("连接配置已变化，请重新连接"));
        }
        let (command, operation, permit) = {
            let _saved = self
                .saved_lock
                .lock()
                .map_err(|_| error("命令存储不可用"))?;
            let command = self
                .saved_command_list()?
                .into_iter()
                .find(|c| c.id == saved_id)
                .ok_or_else(|| error("命令不存在"))?;
            if command.version != expected_version {
                return Err(error("命令已变化，请刷新后重试"));
            }
            let mut pending = self
                .saved_pending
                .lock()
                .map_err(|_| error("命令准入失败"))?;
            let now = std::time::Instant::now();
            pending.retain(|_, (_, _, expires)| *expires > now);
            let operation = match confirm_token {
                Some(token) => {
                    if !pending.get(&token).is_some_and(|(id, version, _)| {
                        id == saved_id && *version == command.version
                    }) {
                        return Err(error("命令已变化，请重新确认"));
                    }
                    token
                }
                None => self
                    .gate
                    .issue_operation()
                    .map_err(|_| error("命令准入失败"))?,
            };
            let live = LiveConnection {
                session_id: session_id.into(),
                connection_id: record.id.clone(),
                version: record.version,
                production: record.production,
                always_confirm: record.always_confirm,
            };
            let intent = CommandIntent {
                operation_id: operation.clone(),
                session_id: session_id.into(),
                connection_id: record.id.clone(),
                connection_version: record.version,
                command: command.content.clone(),
            };
            let permit = match self
                .gate
                .prepare_current(&intent, &live)
                .map_err(|_| error("命令或连接已变化，请重新提交"))?
            {
                Admission::ConfirmationRequired(a) => {
                    if pending.contains_key(&operation) {
                        self.gate
                            .confirm_current(&intent, &live)
                            .map_err(|_| error("确认失效"))?
                    } else {
                        if pending.len() >= 4096 {
                            return Err(error("命令准入繁忙"));
                        }
                        pending.insert(
                            operation.clone(),
                            (
                                command.id.clone(),
                                command.version,
                                now + Duration::from_secs(120),
                            ),
                        );
                        return Ok(SavedRunResult::ConfirmationRequired {
                            token: operation,
                            risk: format!("{:?}", a.level).to_lowercase(),
                            categories: a.categories.iter().map(|v| v.to_string()).collect(),
                        });
                    }
                }
                Admission::Permitted(_, permit) => permit,
            };
            self.gate
                .consume_current(&intent, &live, &permit)
                .map_err(|_| error("确认失效"))?;
            pending.remove(&operation);
            (command, operation, permit)
        };
        let _ = (operation, permit);
        session.inflight.store(true, Ordering::Release);
        let _guard = InflightGuard(session.clone());
        drop(state);
        let mut exec = run_cancellable(
            Duration::from_secs(10),
            &session.cancel,
            session.connection.remote_exec(),
        )
        .await
        .map_err(|_| error("连接已断开，请重新连接"))?;
        session.write_unknown.store(true, Ordering::Release);
        match run_cancellable(
            Duration::from_secs(60),
            &session.cancel,
            exec.execute_bounded(&command.content, 256 * 1024, 256 * 1024),
        )
        .await
        {
            Ok(value) => {
                let mut output = String::from_utf8_lossy(&value.stdout).into_owned();
                output.push_str(&String::from_utf8_lossy(&value.stderr));
                output.push_str(&format!(
                    "\n[退出状态：{}]\n",
                    value
                        .exit_status
                        .map_or_else(|| "未知".to_owned(), |v| v.to_string())
                ));
                session.write_unknown.store(false, Ordering::Release);
                Ok(SavedRunResult::Sent { output })
            }
            Err(_) => {
                session.closed.store(true, Ordering::Release);
                Err(error("执行结果未知，请关闭并重新连接"))
            }
        }
    }

    pub async fn submit(
        &self,
        session_id: &str,
        command: String,
        confirm_token: Option<String>,
    ) -> Result<SubmitResult, String> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .get(session_id)
            .cloned()
            .ok_or_else(|| error("会话不存在"))?;
        if session.closed.load(Ordering::Acquire) || session.cancel.is_cancelled() {
            return Err(error("会话已断开，请重新连接"));
        }
        if session.inflight.load(Ordering::Acquire) {
            return Err(error("该会话正在执行命令"));
        }
        if session.write_unknown.load(Ordering::Acquire) {
            return Err(error("上次发送结果未知，请关闭并重新连接"));
        }
        let record = self
            .service
            .get(&session.connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("连接不存在"))?;
        if record.version != session.connection_version {
            return Err(error("连接配置已变化，请重新连接"));
        }
        let live = LiveConnection {
            session_id: session_id.into(),
            connection_id: record.id.clone(),
            version: record.version,
            production: record.production,
            always_confirm: record.always_confirm,
        };
        let confirmed = confirm_token.is_some();
        let operation = match confirm_token {
            Some(v) => v,
            None => self
                .gate
                .issue_operation()
                .map_err(|_| error("命令准入失败"))?,
        };
        let intent = CommandIntent {
            operation_id: operation.clone(),
            session_id: session_id.into(),
            connection_id: record.id,
            connection_version: record.version,
            command: command.clone(),
        };
        let permit = match self
            .gate
            .prepare_current(&intent, &live)
            .map_err(|_| error("命令或连接已变化，请重新提交"))?
        {
            Admission::ConfirmationRequired(a) => {
                if confirmed {
                    self.gate
                        .confirm_current(&intent, &live)
                        .map_err(|_| error("确认失效"))?
                } else {
                    return Ok(SubmitResult::ConfirmationRequired {
                        token: operation,
                        risk: format!("{:?}", a.level).to_lowercase(),
                        categories: a.categories.iter().map(|v| v.to_string()).collect(),
                    });
                }
            }
            Admission::Permitted(_, p) => p,
        };
        if record.production
            && session
                .exec_output
                .lock()
                .await
                .iter()
                .map(String::len)
                .sum::<usize>()
                > 512 * 1024
        {
            return Err(error("输出尚未读取，请稍后再执行"));
        }
        self.gate
            .consume_current(&intent, &live, &permit)
            .map_err(|_| error("确认失效"))?;
        session.inflight.store(true, Ordering::Release);
        let _guard = InflightGuard(session.clone());
        drop(state);
        if record.production {
            let mut exec = match run_cancellable(
                Duration::from_secs(10),
                &session.cancel,
                session.connection.remote_exec(),
            )
            .await
            {
                Ok(exec) => exec,
                Err(_) => {
                    session.closed.store(true, Ordering::Release);
                    return Err(error("连接已断开，请重新连接"));
                }
            };
            session.write_unknown.store(true, Ordering::Release);
            let result = run_cancellable(
                Duration::from_secs(60),
                &session.cancel,
                exec.execute_bounded(&command, 256 * 1024, 256 * 1024),
            )
            .await;
            return match result {
                Ok(value) => {
                    let mut text = String::from_utf8_lossy(&value.stdout).into_owned();
                    text.push_str(&String::from_utf8_lossy(&value.stderr));
                    text.push_str(&format!(
                        "\n[退出状态：{}]\n",
                        value
                            .exit_status
                            .map_or_else(|| "未知".to_owned(), |v| v.to_string())
                    ));
                    session.exec_output.lock().await.push_back(text);
                    session.write_unknown.store(false, Ordering::Release);
                    Ok(SubmitResult::Sent)
                }
                _ => {
                    session.closed.store(true, Ordering::Release);
                    Err(error("执行结果未知，请关闭并重新连接"))
                }
            };
        }
        let bytes = format!("{command}\n");
        session.write_unknown.store(true, Ordering::Release);
        match run_cancellable(
            Duration::from_secs(10),
            &session.cancel,
            session
                .channel
                .as_ref()
                .ok_or_else(|| error("终端不可用"))?
                .write(bytes.as_bytes()),
        )
        .await
        {
            Ok(()) => {
                session.write_unknown.store(false, Ordering::Release);
                Ok(SubmitResult::Sent)
            }
            _ => {
                session.write_unknown.store(true, Ordering::Release);
                Err(error("发送结果未知，请关闭并重新连接"))
            }
        }
    }
    pub async fn close(&self, session_id: &str) -> Result<(), String> {
        self.remote_files.cancel_session(session_id);
        self.passwords.discard_session(session_id);
        let session = {
            let state = self.state.lock().await;
            let session = state.sessions.get(session_id).cloned();
            if let Some(s) = &session {
                s.cancel.cancel();
                s.closed.store(true, Ordering::Release);
            }
            session
        };
        if let Some(s) = session {
            let _close = s.close_lock.lock().await;
            if !self.state.lock().await.sessions.contains_key(session_id) {
                return Ok(());
            }
            let wait = async {
                loop {
                    let notified = s.inflight_done.notified();
                    if !s.inflight.load(Ordering::Acquire)
                        && !s.file_inflight.load(Ordering::Acquire)
                    {
                        break;
                    }
                    notified.await;
                }
            };
            if tokio::time::timeout(Duration::from_secs(5), wait)
                .await
                .is_err()
            {
                return Err(error("关闭结果未知，请重试"));
            }
            if let Some(channel) = s.channel.as_ref() {
                let _ = tokio::time::timeout(Duration::from_secs(5), channel.close()).await;
            }
            let connection =
                tokio::time::timeout(Duration::from_secs(5), s.connection.close()).await;
            if !matches!(connection, Ok(Ok(()))) {
                return Err(error("关闭结果未知，请重试"));
            }
            self.state.lock().await.sessions.remove(session_id);
        }
        Ok(())
    }

    async fn file_session(&self, id: &str) -> Result<(Arc<Session>, u64, bool), String> {
        let state = self.state.lock().await;
        let session = state
            .sessions
            .get(id)
            .cloned()
            .ok_or_else(|| error("会话不存在"))?;
        if session.closed.load(Ordering::Acquire) || session.cancel.is_cancelled() {
            return Err(error("会话已断开"));
        }
        let record = self
            .service
            .get(&session.connection_id)
            .map_err(|e| e.code)?
            .ok_or_else(|| error("连接不存在"))?;
        if record.version != session.connection_version {
            return Err(error("连接配置已变化，请重新连接"));
        }
        if session.file_inflight.load(Ordering::Acquire) {
            return Err(error("会话正忙"));
        }
        session.file_inflight.store(true, Ordering::Release);
        Ok((
            session,
            record.version,
            record.production || record.always_confirm,
        ))
    }
    pub async fn remote_list(
        &self,
        id: &str,
        path: String,
    ) -> Result<Vec<RemoteFileEntry>, String> {
        if !(crate::remote_files::Mutation::Create { path: path.clone() }).valid() {
            return Err(error("无效路径"));
        }
        let (session, _, _) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session.clone());
        let mut sftp = run_cancellable(
            Duration::from_secs(15),
            &session.cancel,
            session.connection.open_sftp(),
        )
        .await
        .map_err(|_| error("SFTP 连接失败"))?;
        let entries = run_cancellable(Duration::from_secs(30), &session.cancel, sftp.list(&path))
            .await
            .map_err(|_| error("读取目录失败"))?;
        Ok(entries.into_iter().map(Into::into).collect())
    }
    pub async fn remote_realpath(&self, id: &str, path: String) -> Result<String, String> {
        if path.len() > 4096 || path.contains('\0') {
            return Err(error("无效路径"));
        }
        let (session, _, _) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session.clone());
        let mut sftp = run_cancellable(
            Duration::from_secs(15),
            &session.cancel,
            session.connection.open_sftp(),
        )
        .await
        .map_err(|_| error("SFTP 连接失败"))?;
        run_cancellable(
            Duration::from_secs(30),
            &session.cancel,
            sftp.realpath(&path),
        )
        .await
        .map_err(|_| error("解析远程路径失败"))
    }
    pub async fn remote_read(
        &self,
        id: &str,
        path: String,
        offset: u64,
        length: u32,
    ) -> Result<Vec<u8>, String> {
        if !(crate::remote_files::Mutation::Create { path: path.clone() }).valid()
            || length == 0
            || length as usize > coshell_drivers::ssh::sftp::MAX_CHUNK
        {
            return Err(error("无效路径或读取大小"));
        }
        let (session, _, _) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session.clone());
        let mut sftp = run_cancellable(
            Duration::from_secs(15),
            &session.cancel,
            session.connection.open_sftp(),
        )
        .await
        .map_err(|_| error("SFTP 连接失败"))?;
        run_cancellable(
            Duration::from_secs(30),
            &session.cancel,
            sftp.read(&path, offset, length),
        )
        .await
        .map_err(|_| error("读取文件失败"))
    }
    pub async fn remote_stat(&self, id: &str, path: String) -> Result<RemoteFileEntry, String> {
        if !(crate::remote_files::Mutation::Create { path: path.clone() }).valid() {
            return Err(error("无效路径"));
        }
        let (session, _, _) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session.clone());
        let mut sftp = run_cancellable(
            Duration::from_secs(15),
            &session.cancel,
            session.connection.open_sftp(),
        )
        .await
        .map_err(|_| error("SFTP 连接失败"))?;
        run_cancellable(Duration::from_secs(30), &session.cancel, sftp.stat(&path))
            .await
            .map(Into::into)
            .map_err(|_| error("读取文件信息失败"))
    }
    pub async fn remote_read_file(
        &self,
        id: &str,
        path: String,
        limit: usize,
    ) -> Result<RemoteFileContent, String> {
        if !(crate::remote_files::Mutation::Create { path: path.clone() }).valid()
            || limit == 0
            || limit > crate::remote_files::MAX_FILE
        {
            return Err(error("无效路径或读取大小"));
        }
        let (session, _, _) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session.clone());
        let mut sftp = run_cancellable(
            Duration::from_secs(15),
            &session.cancel,
            session.connection.open_sftp(),
        )
        .await
        .map_err(|_| error("SFTP 连接失败"))?;
        let (bytes, metadata) = run_cancellable(
            Duration::from_secs(120),
            &session.cancel,
            sftp.read_file(&path, limit),
        )
        .await
        .map_err(|_| error("文件读取失败或内容已变化"))?;
        Ok(RemoteFileContent {
            bytes,
            metadata: metadata.into(),
        })
    }
    pub async fn remote_prepare(
        &self,
        id: &str,
        mutation: crate::remote_files::Mutation,
    ) -> Result<RemoteFilePermit, String> {
        let (session, version, confirmation_required) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session);
        let token = self
            .remote_files
            .prepare(id, version, mutation, confirmation_required)
            .map_err(error)?;
        Ok(RemoteFilePermit {
            token,
            confirmation_required,
        })
    }
    pub async fn remote_confirm(&self, id: &str, token: String) -> Result<(), String> {
        let (session, version, _) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session);
        self.remote_files
            .confirm(id, version, &token)
            .map_err(error)
    }
    pub async fn remote_commit(&self, id: &str, token: String) -> Result<(), String> {
        use crate::remote_files::Mutation;
        let (session, version, _) = self.file_session(id).await?;
        let _guard = FileInflightGuard(session.clone());
        let mutation = self
            .remote_files
            .consume(id, version, &token)
            .map_err(error)?;
        let mut sftp = run_cancellable(
            Duration::from_secs(15),
            &session.cancel,
            session.connection.open_sftp(),
        )
        .await
        .map_err(|_| error("SFTP 连接失败"))?;
        if let Mutation::Write {
            path,
            overwrite: true,
            expected_digest,
            ..
        } = &mutation
        {
            use sha2::{Digest, Sha256};
            let check = async {
                let meta = sftp.stat(path).await?;
                if !meta.permissions.is_some_and(|p| p & 0o170000 == 0o100000) {
                    return Err(SshError::Transport);
                }
                let size = meta.size.ok_or(SshError::Transport)?;
                if size > crate::remote_files::MAX_FILE as u64 {
                    return Err(SshError::Transport);
                }
                let mut hash = Sha256::new();
                let mut offset = 0u64;
                while offset < size {
                    let len =
                        (size - offset).min(coshell_drivers::ssh::sftp::MAX_CHUNK as u64) as u32;
                    let bytes = sftp.read(path, offset, len).await?;
                    if bytes.is_empty() {
                        return Err(SshError::Transport);
                    }
                    offset += bytes.len() as u64;
                    hash.update(&bytes);
                }
                Ok(format!("{:x}", hash.finalize()))
            };
            let actual = run_cancellable(Duration::from_secs(60), &session.cancel, check)
                .await
                .map_err(|_| error("读取远端文件失败，未执行保存"))?;
            if expected_digest.as_deref() != Some(actual.as_str()) {
                return Err(error("远端文件已变化，请重新读取后再保存"));
            }
        }
        let operation = async {
            match mutation {
                Mutation::Mkdir { path } => sftp.mkdir(&path).await,
                Mutation::Create { path } => sftp.write_file(&path, &[], false).await,
                Mutation::Remove { path, directory } => sftp.remove(&path, directory).await,
                Mutation::Rename { path, destination } => sftp.rename(&path, &destination).await,
                Mutation::Chmod { path, mode } => sftp.chmod(&path, mode).await,
                Mutation::Write {
                    path,
                    data,
                    overwrite,
                    expected_digest: _,
                } => sftp.write_file(&path, &data, overwrite).await,
            }
        };
        run_cancellable(Duration::from_secs(120), &session.cancel, operation)
            .await
            .map_err(|_| error("远端文件操作结果未知，请刷新后核对"))
    }
    pub async fn remote_cancel(&self, id: &str, token: String) {
        self.remote_files.cancel(id, &token);
    }
}

use coshell_core::host_key::{HostKeyPrompt, SqliteHostKeyStore, TofuVerifier};
use coshell_drivers::ssh::{
    run_cancellable, Authentication, Cancellation, ConnectionRequest, HostKey, HostKeyDecision,
    RusshConnector, SshConnector, SshError, SshFuture,
};
use coshell_platform::secret_store::Secret;
use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
static DATABASE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// Public upstream test fixture, not a real credential.
const KEY:&str="-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39\nJ2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl\nKpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5\nFkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ\ne+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx\n7/wNsnDM0T7nLv/Q==\n-----END OPENSSH PRIVATE KEY-----";
// Generated only for this test; the passphrase is public and not a credential.
const OTHER_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jdHIAAAAGYmNyeXB0AAAAGAAAABDbndSZLr\nWmLRcNJFU2VWbMAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAILhL3t0Nd2g4IeQ9\ndNyPgxA8cvesh5TfanMu8CdWshD0AAAAoN4JbBfWvqD6VJ0zn34fvEamWWACtd6YzJXTx6\nD0RFwedYOCNQT3K/z5taVk8KByR6vVomKnMXo5SEtTGY/AZSIETUABdGlKGzm8M3fxs7He\nJsZd8dbnryf3zxv6kOdTtEE7uTn8v+L7tTehFmTIocKFLJGyXk0u3zK5WXG2+U+PcFPD1D\nno6UQEMcvIOErNXRYQhhKCdNOI75lLoy3/ttw=\n-----END OPENSSH PRIVATE KEY-----";
struct Prompt;
impl HostKeyPrompt for Prompt {
    fn ask<'a>(&'a self, _: &'a HostKey) -> SshFuture<'a, HostKeyDecision> {
        Box::pin(async { Ok(HostKeyDecision::AcceptAndSave) })
    }
}
struct Server {
    hang: bool,
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl russh::server::Handler for Server {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        self.entered.notify_one();
        if self.hang {
            self.release.notified().await;
        }
        Ok(if password == "ok" {
            russh::server::Auth::Accept
        } else {
            russh::server::Auth::reject()
        })
    }
}
async fn server(
    hang: bool,
) -> (
    u16,
    Arc<tokio::sync::Notify>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let handler = Server {
        hang,
        entered: entered.clone(),
        release: release.clone(),
    };
    let config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap()],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        if let Ok(session) = russh::server::run_stream(config, socket, handler).await {
            let _ = session.await;
        }
    });
    (port, entered, release, task)
}
fn request(port: u16, password: &str) -> ConnectionRequest {
    ConnectionRequest {
        host: "127.0.0.1".into(),
        port,
        username: "test".into(),
        authentication: Authentication::Password(Secret::new(password.as_bytes().to_vec())),
    }
}
fn database() -> (std::path::PathBuf, rusqlite::Connection) {
    let path = std::env::temp_dir().join(format!(
        "coshell-handshake-{}-{}-{}.sqlite",
        std::process::id(),
        DATABASE_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut db = rusqlite::Connection::open(&path).unwrap();
    coshell_storage::migrate(&mut db).unwrap();
    (path, db)
}
fn count(path: &std::path::Path) -> u32 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("SELECT count(*) FROM trusted_host_keys", [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn successful_authentication_commits_and_failed_authentication_rolls_back() {
    for (password, expected) in [("ok", 1), ("wrong", 0)] {
        let (path, db) = database();
        let verifier = TofuVerifier::new(Arc::new(SqliteHostKeyStore::new(db)), Arc::new(Prompt));
        let (port, _, _, task) = server(false).await;
        let result = RusshConnector::with_timeout(Duration::from_secs(90))
            .connect(request(port, password), &verifier)
            .await;
        assert_eq!(result.is_ok(), expected == 1);
        drop(result);
        assert_eq!(count(&path), expected);
        drop(verifier);
        task.abort();
        std::fs::remove_file(path).unwrap();
    }
}

#[tokio::test]
async fn cancellation_during_authentication_rolls_back_and_releases_write_lock() {
    let (path, db) = database();
    let verifier = TofuVerifier::new(Arc::new(SqliteHostKeyStore::new(db)), Arc::new(Prompt));
    let (port, entered, release, task) = server(true).await;
    let connector = RusshConnector::with_timeout(Duration::from_secs(90));
    let cancel = Cancellation::new();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.busy_timeout(Duration::ZERO).unwrap();
    {
        let operation = run_cancellable(
            Duration::from_secs(90),
            &cancel,
            connector.connect(request(port, "ok"), &verifier),
        );
        tokio::pin!(operation);
        tokio::select! {_=entered.notified()=>{},result=&mut operation=>match result { Ok(_) => panic!("authentication unexpectedly succeeded before password handler"), Err(error) => panic!("authentication failed before password handler: {error:?}") }};
        assert!(db.execute_batch("BEGIN IMMEDIATE").is_err());
        cancel.cancel();
        assert!(matches!(operation.await, Err(SshError::Cancelled)));
    }
    assert_eq!(count(&path), 0);
    db.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").unwrap();
    drop(db);
    release.notify_one();
    drop(verifier);
    task.abort();
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn fixed_host_keys_reject_save_match_and_detect_change_without_overwrite() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Decisions(AtomicUsize);
    impl HostKeyPrompt for Decisions {
        fn ask<'a>(&'a self, _: &'a HostKey) -> SshFuture<'a, HostKeyDecision> {
            Box::pin(async move {
                Ok(if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    HostKeyDecision::Reject
                } else {
                    HostKeyDecision::AcceptAndSave
                })
            })
        }
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let first = russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap();
    let second = russh::keys::decode_secret_key(OTHER_KEY, Some("fixture")).unwrap();
    let first_fingerprint = first
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    let second_fingerprint = second
        .public_key()
        .fingerprint(russh::keys::HashAlg::Sha256)
        .to_string();
    assert_ne!(first_fingerprint, second_fingerprint);
    let task = tokio::spawn(async move {
        for key in [first.clone(), first.clone(), first, second] {
            let (socket, _) = listener.accept().await.unwrap();
            let config = Arc::new(russh::server::Config {
                keys: vec![key],
                auth_rejection_time: Duration::ZERO,
                auth_rejection_time_initial: Some(Duration::ZERO),
                ..Default::default()
            });
            let handler = Server {
                hang: false,
                entered: Arc::new(tokio::sync::Notify::new()),
                release: Arc::new(tokio::sync::Notify::new()),
            };
            tokio::spawn(async move {
                if let Ok(session) = russh::server::run_stream(config, socket, handler).await {
                    let _ = session.await;
                }
            });
        }
    });
    let (path, db) = database();
    let decisions = Arc::new(Decisions(AtomicUsize::new(0)));
    let verifier = TofuVerifier::new(Arc::new(SqliteHostKeyStore::new(db)), decisions.clone());
    let connector = RusshConnector::with_timeout(Duration::from_secs(90));
    assert!(matches!(
        connector.connect(request(port, "ok"), &verifier).await,
        Err(SshError::HostKeyRejected)
    ));
    assert_eq!(count(&path), 0);
    drop(
        connector
            .connect(request(port, "ok"), &verifier)
            .await
            .unwrap(),
    );
    assert_eq!(count(&path), 1);
    drop(
        connector
            .connect(request(port, "ok"), &verifier)
            .await
            .unwrap(),
    );
    assert_eq!(decisions.0.load(Ordering::SeqCst), 2);
    assert!(
        matches!(connector.connect(request(port, "ok"), &verifier).await,
        Err(SshError::HostKeyChanged { old_fingerprint, new_fingerprint, .. })
        if old_fingerprint == first_fingerprint && new_fingerprint == second_fingerprint)
    );
    assert_eq!(decisions.0.load(Ordering::SeqCst), 2);
    let saved: String = rusqlite::Connection::open(&path)
        .unwrap()
        .query_row(
            "SELECT fingerprint FROM trusted_host_keys WHERE host='127.0.0.1' AND port=?1",
            [port],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(saved, first_fingerprint);
    assert_eq!(count(&path), 1);
    task.await.unwrap();
    drop(verifier);
    std::fs::remove_file(path).unwrap();
}

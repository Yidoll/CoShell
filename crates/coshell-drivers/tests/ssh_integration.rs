use coshell_drivers::ssh::{
    Authentication, ConnectionRequest, HostKey, HostKeyDecision, HostKeyVerification,
    HostKeyVerifier, RusshConnector, SshConnector, SshError, SshFuture,
};
use coshell_platform::secret_store::Secret;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

// Public upstream test fixture, password "blabla"; never used as a real credential.
const TEST_KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39\nJ2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl\nKpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5\nFkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ\ne+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx\n7/wNsnDM0T7nLv/Q==\n-----END OPENSSH PRIVATE KEY-----";

struct Verifier {
    decision: HostKeyDecision,
    calls: Arc<AtomicUsize>,
}
impl HostKeyVerifier for Verifier {
    fn verify<'a>(&'a self, key: &'a HostKey) -> SshFuture<'a, HostKeyVerification> {
        Box::pin(async move {
            assert!(key.fingerprint.starts_with("SHA256:"));
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(HostKeyVerification::decision(self.decision))
        })
    }
}

struct PendingVerifier(tokio::sync::Notify);
impl HostKeyVerifier for PendingVerifier {
    fn verify<'a>(&'a self, _: &'a HostKey) -> SshFuture<'a, HostKeyVerification> {
        Box::pin(async move {
            self.0.notify_one();
            std::future::pending().await
        })
    }
}

struct Server {
    auth_calls: Arc<AtomicUsize>,
}
impl russh::server::Handler for Server {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        self.auth_calls.fetch_add(1, Ordering::SeqCst);
        Ok(if user == "test" && password == "correct" {
            russh::server::Auth::Accept
        } else {
            russh::server::Auth::reject()
        })
    }
    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &russh::keys::PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        self.auth_calls.fetch_add(1, Ordering::SeqCst);
        let expected = russh::keys::decode_secret_key(TEST_KEY, Some("blabla")).unwrap();
        Ok(
            if user == "test" && key.key_data() == expected.public_key().key_data() {
                russh::server::Auth::Accept
            } else {
                russh::server::Auth::reject()
            },
        )
    }
}

async fn server() -> (u16, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let calls = Arc::new(AtomicUsize::new(0));
    let calls2 = calls.clone();
    let config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::decode_secret_key(TEST_KEY, Some("blabla")).unwrap()],
        auth_rejection_time: Duration::ZERO,
        auth_rejection_time_initial: Some(Duration::ZERO),
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        if let Ok(session) =
            russh::server::run_stream(config, socket, Server { auth_calls: calls2 }).await
        {
            let _ = session.await;
        }
    });
    (port, calls, task)
}

fn request(port: u16, authentication: Authentication) -> ConnectionRequest {
    ConnectionRequest {
        host: "127.0.0.1".into(),
        port,
        username: "test".into(),
        authentication,
    }
}

#[tokio::test]
async fn host_rejection_happens_before_password_authentication() {
    let (port, auth, task) = server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::Reject,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let result = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await;
    assert!(matches!(result, Err(SshError::HostKeyRejected)));
    assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
    assert_eq!(auth.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn password_and_encrypted_private_key_authenticate_after_host_acceptance() {
    for authentication in [
        Authentication::Password(Secret::new(b"correct".to_vec())),
        Authentication::PrivateKey {
            key: Secret::new(TEST_KEY.as_bytes().to_vec()),
            passphrase: Some(Secret::new(b"blabla".to_vec())),
        },
    ] {
        let (port, auth, task) = server().await;
        let verifier = Verifier {
            decision: HostKeyDecision::AcceptOnce,
            calls: Arc::new(AtomicUsize::new(0)),
        };
        let connection = RusshConnector::default()
            .connect(request(port, authentication), &verifier)
            .await
            .unwrap();
        assert_eq!(verifier.calls.load(Ordering::SeqCst), 1);
        assert!(auth.load(Ordering::SeqCst) > 0);
        connection.close().await.unwrap();
        task.abort();
    }
}

#[tokio::test]
async fn failed_authentication_and_handshake_timeout_are_reported() {
    let (port, _, task) = server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let result = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"wrong".to_vec())),
            ),
            &verifier,
        )
        .await;
    assert!(result.is_err());
    task.abort();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let stalled = tokio::spawn(async move {
        let (_socket, _) = listener.accept().await.unwrap();
        tokio::time::sleep(Duration::from_secs(5)).await;
    });
    let result = RusshConnector::with_timeout(Duration::from_millis(100))
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await;
    assert!(matches!(result, Err(SshError::Timeout)));
    stalled.abort();
}

#[tokio::test]
async fn wrong_private_key_passphrase_is_rejected_before_public_key_authentication() {
    let (port, auth, task) = server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let request = request(
        port,
        Authentication::PrivateKey {
            key: Secret::new(TEST_KEY.as_bytes().to_vec()),
            passphrase: Some(Secret::new(b"wrong".to_vec())),
        },
    );
    let result = RusshConnector::default().connect(request, &verifier).await;
    assert!(matches!(result, Err(SshError::AuthenticationFailed)));
    assert_eq!(auth.load(Ordering::SeqCst), 0);
    task.abort();
}

#[tokio::test]
async fn cancelling_during_host_verification_releases_server_connection() {
    use coshell_drivers::ssh::{run_cancellable, Cancellation};
    let (port, auth, server_task) = server().await;
    let verifier = PendingVerifier(tokio::sync::Notify::new());
    let cancel = Cancellation::new();
    let request = request(
        port,
        Authentication::Password(Secret::new(b"correct".to_vec())),
    );
    let connector = RusshConnector::default();
    let operation = run_cancellable(
        Duration::from_secs(5),
        &cancel,
        connector.connect(request, &verifier),
    );
    tokio::pin!(operation);
    tokio::select! { _=verifier.0.notified()=>{}, _=&mut operation=>panic!("connection ended before verification") }
    cancel.cancel();
    assert!(matches!(operation.await, Err(SshError::Cancelled)));
    tokio::time::timeout(Duration::from_secs(2), server_task)
        .await
        .expect("server connection was not released")
        .unwrap();
    assert_eq!(auth.load(Ordering::SeqCst), 0);
}

struct HangingAuth {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}
impl russh::server::Handler for HangingAuth {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(russh::server::Auth::reject())
    }
}

#[tokio::test]
async fn cancelling_during_authentication_closes_socket() {
    use coshell_drivers::ssh::{run_cancellable, Cancellation};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let notified = Arc::new(tokio::sync::Notify::new());
    let notify = notified.clone();
    let release = Arc::new(tokio::sync::Notify::new());
    let release_server = release.clone();
    let config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::decode_secret_key(TEST_KEY, Some("blabla")).unwrap()],
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        if let Ok(session) = russh::server::run_stream(
            config,
            socket,
            HangingAuth {
                entered: notify,
                release: release_server,
            },
        )
        .await
        {
            let _ = session.await;
        }
    });
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let cancel = Cancellation::new();
    let connector = RusshConnector::default();
    let operation = run_cancellable(
        Duration::from_secs(5),
        &cancel,
        connector.connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        ),
    );
    tokio::pin!(operation);
    tokio::select! {_=notified.notified()=>{}, _=&mut operation=>panic!("ended before auth")}
    cancel.cancel();
    assert!(matches!(operation.await, Err(SshError::Cancelled)));
    release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("server connection was not released")
        .unwrap();
}

#[derive(Clone)]
struct ChannelServer {
    sizes: Arc<std::sync::Mutex<Vec<(u32, u32)>>>,
    closed: Arc<AtomicUsize>,
}

struct PendingPty {
    entered: Arc<tokio::sync::Notify>,
    closed: Arc<AtomicUsize>,
}
impl russh::server::Handler for PendingPty {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        _: russh::ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.entered.notify_one();
        Ok(())
    }
    async fn channel_close(
        &mut self,
        _: russh::ChannelId,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn dropping_pending_terminal_initialization_closes_channel() {
    use coshell_drivers::ssh::TerminalSize;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let entered = Arc::new(tokio::sync::Notify::new());
    let closed = Arc::new(AtomicUsize::new(0));
    let handler = PendingPty {
        entered: entered.clone(),
        closed: closed.clone(),
    };
    let config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::decode_secret_key(TEST_KEY, Some("blabla")).unwrap()],
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        if let Ok(session) = russh::server::run_stream(config, socket, handler).await {
            let _ = session.await;
        }
    });
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let mut opening = Box::pin(connection.open_terminal(TerminalSize {
        columns: 80,
        rows: 24,
    }));
    tokio::select! {_=entered.notified()=>{},_=&mut opening=>panic!("unexpected completion")};
    drop(opening);
    tokio::time::timeout(Duration::from_secs(2), async {
        while closed.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("pending channel leaked");
    drop(connection);
    task.abort();
}
impl russh::server::Handler for ChannelServer {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        _: &str,
        _: &str,
    ) -> Result<russh::server::Auth, Self::Error> {
        Ok(russh::server::Auth::Accept)
    }
    async fn channel_open_session(
        &mut self,
        _: russh::Channel<russh::server::Msg>,
        reply: russh::server::ChannelOpenHandle,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }
    async fn pty_request(
        &mut self,
        id: russh::ChannelId,
        _: &str,
        c: u32,
        r: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.sizes.lock().unwrap().push((c, r));
        s.channel_success(id)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        id: russh::ChannelId,
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        s.channel_success(id)?;
        s.data(id, b"\x1b[32mready\x1b[0m".to_vec())?;
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        _: russh::ChannelId,
        c: u32,
        r: u32,
        _: u32,
        _: u32,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.sizes.lock().unwrap().push((c, r));
        Ok(())
    }
    async fn data(
        &mut self,
        id: russh::ChannelId,
        data: &[u8],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if data.first() == Some(&b'z') {
            tokio::time::sleep(Duration::from_millis(500)).await;
            return Ok(());
        }
        if data == b"eof" {
            s.eof(id)?;
            return Ok(());
        }
        s.data(id, data.to_vec())?;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        id: russh::ChannelId,
        data: &[u8],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        if data == b"early" {
            s.data(id, b"early".to_vec())?;
        }
        s.channel_success(id)?;
        if data == b"disconnect" {
            s.data(id, b"partial".to_vec())?;
            return Err(russh::Error::Disconnect);
        }
        if data == b"flood" {
            s.data(id, vec![b'x'; 128])?;
            s.extended_data(id, 1, vec![b'y'; 128])?;
        } else {
            s.data(id, b"out".to_vec())?;
            s.extended_data(id, 1, b"err".to_vec())?;
            s.exit_status_request(id, 7)?;
            s.eof(id)?;
            s.close(id)?;
        }
        Ok(())
    }
    async fn channel_close(
        &mut self,
        _: russh::ChannelId,
        _: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        self.closed.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn exec_rejects_output_before_request_confirmation() {
    let (port, _, _, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let mut exec = connection.remote_exec().await.unwrap();
    assert!(exec.execute_bounded("early", 100, 100).await.is_err());
    drop(connection);
    task.abort();
}

#[tokio::test]
async fn exec_disconnect_without_exit_status_is_not_success() {
    let (port, _, _, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let mut exec = connection.remote_exec().await.unwrap();
    assert!(exec.execute_bounded("disconnect", 100, 100).await.is_err());
    drop(connection);
    task.abort();
}

async fn channel_server() -> (
    u16,
    Arc<std::sync::Mutex<Vec<(u32, u32)>>>,
    Arc<AtomicUsize>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sizes = Arc::new(std::sync::Mutex::new(Vec::new()));
    let closed = Arc::new(AtomicUsize::new(0));
    let handler = ChannelServer {
        sizes: sizes.clone(),
        closed: closed.clone(),
    };
    let config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::decode_secret_key(TEST_KEY, Some("blabla")).unwrap()],
        window_size: 1024,
        maximum_packet_size: 1024,
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        if let Ok(session) = russh::server::run_stream(config, socket, handler).await {
            let _ = session.await;
        }
    });
    (port, sizes, closed, task)
}

#[tokio::test]
async fn close_interrupts_write_waiting_for_remote_window() {
    use coshell_drivers::ssh::TerminalSize;
    let (port, _, _, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let terminal = connection
        .open_terminal(TerminalSize {
            columns: 80,
            rows: 24,
        })
        .await
        .unwrap();
    terminal.read().await.unwrap();
    let bytes = vec![b'z'; 1024 * 1024];
    let mut write = Box::pin(terminal.write(&bytes));
    assert!(tokio::time::timeout(Duration::from_millis(100), &mut write)
        .await
        .is_err());
    tokio::time::timeout(Duration::from_millis(200), terminal.close())
        .await
        .expect("close blocked by write")
        .unwrap();
    assert!(tokio::time::timeout(Duration::from_millis(200), write)
        .await
        .unwrap()
        .is_err());
    drop(terminal);
    drop(connection);
    task.abort();
}

#[tokio::test]
async fn remote_eof_closes_channel_before_terminal_is_dropped() {
    use coshell_drivers::ssh::TerminalSize;
    let (port, _, closed, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let terminal = connection
        .open_terminal(TerminalSize {
            columns: 80,
            rows: 24,
        })
        .await
        .unwrap();
    terminal.read().await.unwrap();
    terminal.write(b"eof").await.unwrap();
    assert_eq!(terminal.read().await.unwrap(), None);
    tokio::time::timeout(Duration::from_secs(2), async {
        while closed.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("EOF did not close channel");
    drop(terminal);
    drop(connection);
    task.abort();
}

#[tokio::test]
async fn explicit_close_then_drop_sends_only_one_close() {
    use coshell_drivers::ssh::TerminalSize;
    let (port, _, closed, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let terminal = connection
        .open_terminal(TerminalSize {
            columns: 80,
            rows: 24,
        })
        .await
        .unwrap();
    terminal.read().await.unwrap();
    terminal.close().await.unwrap();
    drop(terminal);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(closed.load(Ordering::SeqCst), 1);
    drop(connection);
    task.abort();
}

#[tokio::test]
async fn real_terminal_supports_ansi_concurrent_read_write_resize_and_close() {
    use coshell_drivers::ssh::TerminalSize;
    let (port, sizes, closed, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let terminal = connection
        .open_terminal(TerminalSize {
            columns: 80,
            rows: 24,
        })
        .await
        .unwrap();
    assert_eq!(
        terminal.read().await.unwrap().unwrap(),
        b"\x1b[32mready\x1b[0m"
    );
    let (read, write) = tokio::join!(terminal.read(), async {
        terminal.write(b"hello").await.unwrap();
        terminal
            .resize(TerminalSize {
                columns: 120,
                rows: 40,
            })
            .await
            .unwrap();
    });
    let _ = write;
    assert_eq!(read.unwrap().unwrap(), b"hello");
    terminal.close().await.unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(sizes.lock().unwrap().as_slice(), &[(80, 24), (120, 40)]);
    assert!(closed.load(Ordering::SeqCst) > 0);
    drop(connection);
    task.abort();
}

#[tokio::test]
async fn real_exec_collects_streams_and_enforces_limits_before_eof() {
    let (port, _, closed, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let mut exec = connection.remote_exec().await.unwrap();
    let output = exec.execute_bounded("ok", 16, 16).await.unwrap();
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");
    assert_eq!(output.exit_status, Some(7));
    let mut exec = connection.remote_exec().await.unwrap();
    assert!(tokio::time::timeout(
        Duration::from_secs(2),
        exec.execute_bounded("flood", 16, 16)
    )
    .await
    .unwrap()
    .is_err());
    drop(exec);
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(closed.load(Ordering::SeqCst) > 0);
    drop(connection);
    task.abort();
}

#[tokio::test]
async fn dropping_pending_exec_future_closes_remote_channel() {
    let (port, _, closed, task) = channel_server().await;
    let verifier = Verifier {
        decision: HostKeyDecision::AcceptOnce,
        calls: Arc::new(AtomicUsize::new(0)),
    };
    let connection = RusshConnector::default()
        .connect(
            request(
                port,
                Authentication::Password(Secret::new(b"correct".to_vec())),
            ),
            &verifier,
        )
        .await
        .unwrap();
    let mut exec = connection.remote_exec().await.unwrap();
    let mut future = Box::pin(exec.execute_bounded("flood", 1024, 1024));
    assert!(tokio::time::timeout(Duration::from_millis(50), &mut future)
        .await
        .is_err());
    drop(future);
    tokio::time::timeout(Duration::from_secs(2), async {
        while closed.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop(exec);
    drop(connection);
    task.abort();
}

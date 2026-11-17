use coshell_drivers::ssh::{
    run_cancellable, Authentication, Cancellation, ConnectionRequest, HostKey, HostKeyDecision,
    HostKeyVerification, HostKeyVerifier, RemoteExec, RemoteOutput, SshConnection, SshConnector,
    SshError, SshFuture, TerminalChannel, TerminalSize,
};
use coshell_platform::secret_store::Secret;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

#[derive(Default)]
struct MockTerminal {
    incoming: tokio::sync::Notify,
    output: Mutex<VecDeque<Vec<u8>>>,
    writes: Mutex<Vec<Vec<u8>>>,
    size: Mutex<Option<TerminalSize>>,
    closed: std::sync::atomic::AtomicBool,
}

impl TerminalChannel for MockTerminal {
    fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            loop {
                let notified = self.incoming.notified();
                if let Some(output) = self.output.lock().unwrap().pop_front() {
                    return Ok(Some(output));
                }
                if self.closed.load(Ordering::SeqCst) {
                    return Ok(None);
                }
                notified.await;
            }
        })
    }
    fn write<'a>(&'a self, bytes: &'a [u8]) -> SshFuture<'a, ()> {
        Box::pin(async move {
            self.writes.lock().unwrap().push(bytes.to_vec());
            Ok(())
        })
    }
    fn resize<'a>(&'a self, size: TerminalSize) -> SshFuture<'a, ()> {
        Box::pin(async move {
            *self.size.lock().unwrap() = Some(size);
            Ok(())
        })
    }
    fn close<'a>(&'a self) -> SshFuture<'a, ()> {
        Box::pin(async move {
            self.closed.store(true, Ordering::SeqCst);
            self.incoming.notify_waiters();
            Ok(())
        })
    }
}

#[tokio::test]
async fn pending_read_does_not_block_write_resize_or_close() {
    let terminal = Arc::new(MockTerminal::default());
    let reader = terminal.clone();
    let task = tokio::spawn(async move { reader.read().await });
    tokio::task::yield_now().await;
    terminal.write(b"pwd\n").await.unwrap();
    terminal
        .resize(TerminalSize {
            columns: 120,
            rows: 40,
        })
        .await
        .unwrap();
    terminal.close().await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        None
    );
    assert_eq!(
        terminal.writes.lock().unwrap().as_slice(),
        &[b"pwd\n".to_vec()]
    );
    assert_eq!(
        *terminal.size.lock().unwrap(),
        Some(TerminalSize {
            columns: 120,
            rows: 40
        })
    );
}

#[derive(Clone)]
struct MockConnector {
    delay: Duration,
    live: Arc<AtomicUsize>,
    attempts: Arc<AtomicUsize>,
}

struct Attempt(Arc<AtomicUsize>);
impl Drop for Attempt {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

struct MockConnection(Arc<AtomicUsize>);
impl Drop for MockConnection {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl SshConnector for MockConnector {
    fn connect<'a>(
        &'a self,
        _request: ConnectionRequest,
        verifier: &'a dyn HostKeyVerifier,
    ) -> SshFuture<'a, Box<dyn SshConnection>> {
        Box::pin(async move {
            self.attempts.fetch_add(1, Ordering::SeqCst);
            let _attempt = Attempt(self.attempts.clone());
            tokio::time::sleep(self.delay).await;
            let key = HostKey {
                host: "example.org".into(),
                port: 22,
                algorithm: "ed25519".into(),
                fingerprint: "SHA256:test".into(),
            };
            if verifier.verify(&key).await?.decision == HostKeyDecision::Reject {
                return Err(SshError::HostKeyRejected);
            }
            self.live.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(MockConnection(self.live.clone())) as Box<dyn SshConnection>)
        })
    }
}

impl SshConnection for MockConnection {
    fn open_terminal<'a>(&'a self, _size: TerminalSize) -> SshFuture<'a, Box<dyn TerminalChannel>> {
        Box::pin(async { Err(SshError::Unsupported) })
    }
    fn remote_exec<'a>(&'a self) -> SshFuture<'a, Box<dyn RemoteExec>> {
        Box::pin(async { Err(SshError::Unsupported) })
    }
    fn close<'a>(&'a self) -> SshFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

struct Verifier(HostKeyDecision);
impl HostKeyVerifier for Verifier {
    fn verify<'a>(&'a self, _key: &'a HostKey) -> SshFuture<'a, HostKeyVerification> {
        Box::pin(async move { Ok(HostKeyVerification::decision(self.0)) })
    }
}

fn request() -> ConnectionRequest {
    ConnectionRequest {
        host: "example.org".into(),
        port: 22,
        username: "operator".into(),
        authentication: Authentication::Password(Secret::new(b"test-password".to_vec())),
    }
}

#[tokio::test]
async fn host_verification_precedes_creating_an_active_connection() {
    let live = Arc::new(AtomicUsize::new(0));
    let connector = MockConnector {
        delay: Duration::ZERO,
        live: live.clone(),
        attempts: Arc::new(AtomicUsize::new(0)),
    };
    let reject = Verifier(HostKeyDecision::Reject);
    assert!(matches!(
        connector.connect(request(), &reject).await,
        Err(SshError::HostKeyRejected)
    ));
    assert_eq!(live.load(Ordering::SeqCst), 0);
    let accept = Verifier(HostKeyDecision::AcceptOnce);
    let connection = connector.connect(request(), &accept).await.unwrap();
    assert_eq!(live.load(Ordering::SeqCst), 1);
    drop(connection);
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_and_timeout_drop_pending_connection_attempts() {
    let live = Arc::new(AtomicUsize::new(0));
    let attempts = Arc::new(AtomicUsize::new(0));
    let connector = MockConnector {
        delay: Duration::from_secs(30),
        live: live.clone(),
        attempts: attempts.clone(),
    };
    let verifier = Verifier(HostKeyDecision::AcceptOnce);
    let cancellation = Cancellation::new();
    cancellation.cancel();
    assert!(matches!(
        run_cancellable(
            Duration::from_secs(1),
            &cancellation,
            connector.connect(request(), &verifier)
        )
        .await,
        Err(SshError::Cancelled)
    ));
    let cancellation = Cancellation::new();
    assert!(matches!(
        run_cancellable(
            Duration::from_millis(10),
            &cancellation,
            connector.connect(request(), &verifier)
        )
        .await,
        Err(SshError::Timeout)
    ));
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    let cancellation = Cancellation::new();
    let signal = cancellation.clone();
    let cancel_task = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        signal.cancel();
    });
    assert!(matches!(
        run_cancellable(
            Duration::from_secs(1),
            &cancellation,
            connector.connect(request(), &verifier)
        )
        .await,
        Err(SshError::Cancelled)
    ));
    cancel_task.await.unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    assert_eq!(live.load(Ordering::SeqCst), 0);
}

#[test]
fn authentication_debug_never_reveals_secret_bytes() {
    let request = request();
    assert!(!format!("{request:?}").contains("test-password"));
    let _ = RemoteOutput {
        stdout: vec![],
        stderr: vec![],
        exit_status: Some(0),
    };
}

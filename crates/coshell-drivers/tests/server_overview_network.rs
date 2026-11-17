use coshell_drivers::{
    server_overview::{collect, COLLECT_COMMAND},
    ssh::{
        Authentication, Cancellation, ConnectionRequest, HostKey, HostKeyDecision,
        HostKeyVerification, HostKeyVerifier, RusshConnector, SshConnector, SshError, SshFuture,
        TerminalSize,
    },
};
use coshell_platform::secret_store::Secret;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

// Public russh fixture; password is deliberately non-secret.
const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39\nJ2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl\nKpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5\nFkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ\ne+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx\n7/wNsnDM0T7nLv/Q==\n-----END OPENSSH PRIVATE KEY-----";

struct Verify;
impl HostKeyVerifier for Verify {
    fn verify<'a>(&'a self, _: &'a HostKey) -> SshFuture<'a, HostKeyVerification> {
        Box::pin(async { Ok(HostKeyVerification::decision(HostKeyDecision::AcceptOnce)) })
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Full,
    Partial,
    Flood,
    Stall,
}
struct Server {
    mode: Mode,
    commands: Arc<AtomicUsize>,
    closed: Arc<AtomicUsize>,
    started: Arc<tokio::sync::Notify>,
}
impl russh::server::Handler for Server {
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
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(russh::Pty, u32)],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        s.channel_success(id)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        id: russh::ChannelId,
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        s.channel_success(id)?;
        Ok(())
    }
    async fn data(
        &mut self,
        id: russh::ChannelId,
        data: &[u8],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        s.data(id, data.to_vec())?;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        id: russh::ChannelId,
        data: &[u8],
        s: &mut russh::server::Session,
    ) -> Result<(), Self::Error> {
        assert_eq!(data, COLLECT_COMMAND.as_bytes());
        self.commands.fetch_add(1, Ordering::SeqCst);
        s.channel_success(id)?;
        self.started.notify_one();
        match self.mode {
            Mode::Full => s.data(id,b"hostname=host\nos=Linux\nuptime=12\ncpu_usage=30\nlogical_cores=2\nmemory_total=4096\nmemory_used=1024\nload=1.5\ndisk_total=8192\ndisk_used=4096\nswap_total=0\nswap_used=0\n".to_vec())?,
            Mode::Partial => s.data(id,b"hostname=host\nuptime=12\n".to_vec())?,
            Mode::Flood => { for _ in 0..5 { s.data(id,vec![b'x'; 32768])?; } },
            Mode::Stall => return Ok(()),
        }
        if !matches!(self.mode, Mode::Flood) {
            s.exit_status_request(id, 0)?;
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

async fn setup(
    mode: Mode,
) -> (
    Box<dyn coshell_drivers::ssh::SshConnection>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
    Arc<tokio::sync::Notify>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let commands = Arc::new(AtomicUsize::new(0));
    let closed = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(tokio::sync::Notify::new());
    let handler = Server {
        mode,
        commands: commands.clone(),
        closed: closed.clone(),
        started: started.clone(),
    };
    let config = Arc::new(russh::server::Config {
        keys: vec![russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap()],
        ..Default::default()
    });
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        if let Ok(session) = russh::server::run_stream(config, socket, handler).await {
            let _ = session.await;
        }
    });
    let connection = RusshConnector::default()
        .connect(
            ConnectionRequest {
                host: "127.0.0.1".into(),
                port,
                username: "test".into(),
                authentication: Authentication::Password(Secret::new(b"test".to_vec())),
            },
            &Verify,
        )
        .await
        .unwrap();
    (connection, commands, closed, started, task)
}

#[tokio::test]
async fn real_ssh_exec_collects_full_and_partial_snapshots() {
    for mode in [Mode::Full, Mode::Partial] {
        let (connection, commands, _, _, task) = setup(mode).await;
        let mut exec = connection.remote_exec().await.unwrap();
        let snapshot = collect(
            &mut *exec,
            "c",
            "2026-01-01T00:00:00Z",
            Duration::from_secs(3),
            &Cancellation::new(),
        )
        .await
        .unwrap();
        assert_eq!(snapshot.hostname.as_deref(), Some("host"));
        assert_eq!(snapshot.uptime.unwrap().value, Some(12.0));
        if matches!(mode, Mode::Full) {
            assert_eq!(snapshot.cpu_usage.unwrap().value, Some(30.0));
            assert_eq!(snapshot.swap_total.unwrap().value, Some(0.0));
        } else {
            assert_eq!(snapshot.cpu_usage.unwrap().value, None);
        }
        assert_eq!(commands.load(Ordering::SeqCst), 1);
        drop(connection);
        task.abort();
    }
}

#[tokio::test]
async fn overflow_and_timeout_close_exec_without_disrupting_terminal() {
    for mode in [Mode::Flood, Mode::Stall] {
        let (connection, _, closed, _, task) = setup(mode).await;
        let terminal = connection
            .open_terminal(TerminalSize {
                columns: 80,
                rows: 24,
            })
            .await
            .unwrap();
        let mut exec = connection.remote_exec().await.unwrap();
        let result = collect(
            &mut *exec,
            "c",
            "now",
            Duration::from_millis(300),
            &Cancellation::new(),
        )
        .await;
        assert!(result.is_err());
        if matches!(mode, Mode::Stall) {
            assert_eq!(result.unwrap_err(), SshError::Timeout);
        }
        tokio::time::timeout(Duration::from_secs(2), async {
            while closed.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        terminal.write(b"still-alive").await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), terminal.read())
                .await
                .unwrap()
                .unwrap()
                .unwrap(),
            b"still-alive"
        );
        terminal.close().await.unwrap();
        drop(connection);
        task.abort();
    }
}

#[tokio::test]
async fn cancellation_and_drop_close_inflight_exec() {
    for drop_future in [false, true] {
        let (connection, _, closed, started, task) = setup(Mode::Stall).await;
        let mut exec = connection.remote_exec().await.unwrap();
        let cancel = Cancellation::new();
        let mut future = Box::pin(collect(
            &mut *exec,
            "c",
            "now",
            Duration::from_secs(5),
            &cancel,
        ));
        tokio::select! { _=started.notified()=>{}, _=&mut future=>panic!("unexpected completion") };
        if !drop_future {
            cancel.cancel();
            assert_eq!(future.as_mut().await.unwrap_err(), SshError::Cancelled);
        }
        drop(future);
        tokio::time::timeout(Duration::from_secs(2), async {
            while closed.load(Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        drop(exec);
        drop(connection);
        task.abort();
    }
}

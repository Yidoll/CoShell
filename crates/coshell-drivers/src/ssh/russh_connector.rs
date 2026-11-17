use super::*;
use russh::{
    client,
    keys::{HashAlg, PrivateKeyWithHashAlg},
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::{
    io,
    pin::Pin,
    task::{Context, Poll, Waker},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::{mpsc, oneshot};

type Challenge = (HostKey, oneshot::Sender<bool>);
static KEY_WORK: std::sync::OnceLock<Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
async fn run_key_work<T: Send + 'static>(
    limit: Arc<tokio::sync::Semaphore>,
    work: impl FnOnce() -> Result<T, SshError> + Send + 'static,
) -> Result<T, SshError> {
    let permit = limit
        .try_acquire_owned()
        .map_err(|_| SshError::HostKeyBusy)?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        work()
    })
    .await
    .map_err(|_| SshError::AuthenticationFailed)?
}
async fn decode_private_key(
    key: coshell_platform::secret_store::Secret,
    passphrase: Option<coshell_platform::secret_store::Secret>,
) -> Result<russh::keys::PrivateKey, SshError> {
    let limit = KEY_WORK
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(2)))
        .clone();
    run_key_work(limit, move || {
        if key.expose().len() > 1024 * 1024
            || passphrase.as_ref().is_some_and(|v| v.expose().len() > 4096)
        {
            return Err(SshError::AuthenticationFailed);
        }
        let text = std::str::from_utf8(key.expose()).map_err(|_| SshError::AuthenticationFailed)?;
        let parsed = russh::keys::PrivateKey::from_openssh(text)
            .map_err(|_| SshError::AuthenticationFailed)?;
        match parsed.kdf() {
            russh::keys::ssh_key::Kdf::None => {}
            russh::keys::ssh_key::Kdf::Bcrypt { rounds, .. } if *rounds <= 128 => {}
            _ => return Err(SshError::AuthenticationFailed),
        };
        let pass = passphrase
            .as_ref()
            .map(|v| std::str::from_utf8(v.expose()))
            .transpose()
            .map_err(|_| SshError::AuthenticationFailed)?;
        russh::keys::decode_secret_key(text, pass).map_err(|_| SshError::AuthenticationFailed)
    })
    .await
}

// russh starts a task before key exchange completes. Dropping connect_stream's
// future alone detaches that task, so wake and terminate its underlying stream.
#[derive(Default)]
struct AbortState {
    closed: AtomicBool,
    read_waker: Mutex<Option<Waker>>,
    write_waker: Mutex<Option<Waker>>,
}
struct AbortGuard(Arc<AbortState>);
impl Drop for AbortGuard {
    fn drop(&mut self) {
        self.0.closed.store(true, Ordering::SeqCst);
        if let Some(waker) = self.0.read_waker.lock().unwrap().take() {
            waker.wake();
        }
        if let Some(waker) = self.0.write_waker.lock().unwrap().take() {
            waker.wake();
        }
    }
}
struct AbortableStream {
    inner: tokio::net::TcpStream,
    state: Arc<AbortState>,
}
impl AsyncRead for AbortableStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        *self.state.read_waker.lock().unwrap() = Some(cx.waker().clone());
        if self.state.closed.load(Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "connection cancelled",
            )));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}
impl AsyncWrite for AbortableStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        *self.state.write_waker.lock().unwrap() = Some(cx.waker().clone());
        if self.state.closed.load(Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "connection cancelled",
            )));
        }
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        *self.state.write_waker.lock().unwrap() = Some(cx.waker().clone());
        if self.state.closed.load(Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "connection cancelled",
            )));
        }
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        *self.state.write_waker.lock().unwrap() = Some(cx.waker().clone());
        if self.state.closed.load(Ordering::SeqCst) {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "connection cancelled",
            )));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct Handler {
    host: String,
    port: u16,
    challenge: mpsc::UnboundedSender<Challenge>,
    rejected: Arc<AtomicBool>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if matches!(key, russh::keys::PublicKeyOrCertificate::Certificate(_)) {
            self.rejected.store(true, Ordering::SeqCst);
            return Ok(false);
        }
        let public_key = key.public_key();
        let key = HostKey {
            host: self.host.clone(),
            port: self.port,
            algorithm: public_key.algorithm().to_string(),
            fingerprint: public_key.fingerprint(HashAlg::Sha256).to_string(),
        };
        let (reply, response) = oneshot::channel();
        if self.challenge.send((key, reply)).is_err() {
            return Ok(false);
        }
        let accepted = response.await.unwrap_or(false);
        if !accepted {
            self.rejected.store(true, Ordering::SeqCst);
        }
        Ok(accepted)
    }
}

#[derive(Debug, Clone)]
pub struct RusshConnector {
    timeout: Duration,
}

impl Default for RusshConnector {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
        }
    }
}

impl RusshConnector {
    pub fn with_timeout(timeout: Duration) -> Self {
        Self { timeout }
    }
}

impl SshConnector for RusshConnector {
    fn connect<'a>(
        &'a self,
        request: ConnectionRequest,
        verifier: &'a dyn HostKeyVerifier,
    ) -> SshFuture<'a, Box<dyn SshConnection>> {
        Box::pin(async move {
            match tokio::time::timeout(self.timeout, connect(request, verifier)).await {
                Ok(result) => result,
                Err(_) => Err(SshError::Timeout),
            }
        })
    }
}

async fn connect(
    request: ConnectionRequest,
    verifier: &dyn HostKeyVerifier,
) -> Result<Box<dyn SshConnection>, SshError> {
    let (challenge, mut challenges) = mpsc::unbounded_channel();
    let rejected = Arc::new(AtomicBool::new(false));
    let handler = Handler {
        host: request.host.clone(),
        port: request.port,
        challenge,
        rejected: rejected.clone(),
    };
    let socket = tokio::net::TcpStream::connect((request.host.as_str(), request.port))
        .await
        .map_err(|_| SshError::Transport)?;
    let state = Arc::new(AbortState::default());
    let guard = AbortGuard(state.clone());
    let stream = AbortableStream {
        inner: socket,
        state,
    };
    let handshake = client::connect_stream(
        Arc::new(client::Config {
            inactivity_timeout: Some(Duration::from_secs(30)),
            ..Default::default()
        }),
        stream,
        handler,
    );
    tokio::pin!(handshake);
    let mut permit = None;
    let mut verified = false;
    let mut session = loop {
        tokio::select! {
            result = &mut handshake => break result.map_err(|_| if rejected.load(Ordering::SeqCst) { SshError::HostKeyRejected } else { SshError::Transport })?,
            challenge = challenges.recv() => if let Some((key, reply)) = challenge {
                if verified {let _=reply.send(false);return Err(SshError::HostKeyRejected);}
                verified=true;
                let verification = verifier.verify(&key).await?;
                let accepted = matches!(verification.decision, HostKeyDecision::AcceptOnce | HostKeyDecision::AcceptAndSave);
                permit=verification.permit;
                let _ = reply.send(accepted);
                if !accepted { return Err(SshError::HostKeyRejected); }
            } else { return Err(SshError::Transport); }
        }
    };
    let authenticated = match request.authentication {
        Authentication::Password(password) => {
            let password = std::str::from_utf8(password.expose())
                .map_err(|_| SshError::AuthenticationFailed)?;
            session
                .authenticate_password(&request.username, password)
                .await
                .map_err(|_| SshError::Transport)?
                .success()
        }
        Authentication::PrivateKey { key, passphrase } => {
            let key = decode_private_key(key, passphrase).await?;
            let hash = session
                .best_supported_rsa_hash()
                .await
                .map_err(|_| SshError::Transport)?
                .flatten();
            session
                .authenticate_publickey(
                    &request.username,
                    PrivateKeyWithHashAlg::new(Arc::new(key), hash),
                )
                .await
                .map_err(|_| SshError::Transport)?
                .success()
        }
        Authentication::Agent => authenticate_agent(&mut session, &request.username).await?,
    };
    if !authenticated {
        let _ = session
            .disconnect(
                russh::Disconnect::ByApplication,
                "authentication failed",
                "",
            )
            .await;
        return Err(SshError::AuthenticationFailed);
    }
    if let Some(permit) = permit {
        permit.authenticated()?;
    }
    Ok(Box::new(RusshConnection {
        session,
        _guard: guard,
    }))
}

async fn authenticate_agent(
    session: &mut client::Handle<Handler>,
    username: &str,
) -> Result<bool, SshError> {
    use russh::keys::agent::client::AgentClient;
    #[cfg(unix)]
    let mut agent = AgentClient::connect_env()
        .await
        .map_err(|_| SshError::AuthenticationFailed)?
        .dynamic();
    #[cfg(windows)]
    let mut agent = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent")
        .await
        .map_err(|_| SshError::AuthenticationFailed)?
        .dynamic();
    #[cfg(not(any(unix, windows)))]
    return Err(SshError::Unsupported);
    #[cfg(any(unix, windows))]
    {
        let identities = agent
            .request_identities()
            .await
            .map_err(|_| SshError::AuthenticationFailed)?;
        let hash = session
            .best_supported_rsa_hash()
            .await
            .map_err(|_| SshError::Transport)?
            .flatten();
        for identity in identities {
            if matches!(
                identity,
                russh::keys::agent::AgentIdentity::Certificate { .. }
            ) {
                return Err(SshError::Unsupported);
            }
            if session
                .authenticate_publickey_with(
                    username,
                    identity.public_key().into_owned(),
                    hash,
                    &mut agent,
                )
                .await
                .map_err(|_| SshError::AuthenticationFailed)?
                .success()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

struct RusshConnection {
    session: client::Handle<Handler>,
    _guard: AbortGuard,
}

impl SshConnection for RusshConnection {
    fn open_sftp<'a>(&'a self) -> SshFuture<'a, Box<dyn super::sftp::Sftp>> {
        Box::pin(async move {
            let channel = self
                .session
                .channel_open_session()
                .await
                .map_err(|_| SshError::Transport)?;
            let mut owned = ExecChannel(Some(channel));
            let channel = owned.0.as_mut().ok_or(SshError::Transport)?;
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|_| SshError::Transport)?;
            await_success(channel).await?;
            let stream = owned.0.take().ok_or(SshError::Transport)?.into_stream();
            let client = super::sftp::Client::init(stream).await?;
            Ok(Box::new(client) as Box<dyn super::sftp::Sftp>)
        })
    }
    fn open_terminal<'a>(&'a self, size: TerminalSize) -> SshFuture<'a, Box<dyn TerminalChannel>> {
        Box::pin(async move {
            let channel = self
                .session
                .channel_open_session()
                .await
                .map_err(|_| SshError::Transport)?;
            let mut owned = ExecChannel(Some(channel));
            let channel = owned.0.as_mut().ok_or(SshError::Transport)?;
            channel
                .request_pty(
                    true,
                    "xterm-256color",
                    size.columns.into(),
                    size.rows.into(),
                    0,
                    0,
                    &[],
                )
                .await
                .map_err(|_| SshError::Transport)?;
            await_success(channel).await?;
            channel
                .request_shell(true)
                .await
                .map_err(|_| SshError::Transport)?;
            await_success(channel).await?;
            let (read, write) = owned.0.take().ok_or(SshError::Transport)?.split();
            let (closed, _) = watch::channel(false);
            Ok(Box::new(RusshTerminal {
                read: tokio::sync::Mutex::new(read),
                write: Arc::new(write),
                write_lock: tokio::sync::Mutex::new(()),
                closed,
                close_result: tokio::sync::Mutex::new(None),
            }) as Box<dyn TerminalChannel>)
        })
    }
    fn remote_exec<'a>(&'a self) -> SshFuture<'a, Box<dyn RemoteExec>> {
        Box::pin(async move {
            let channel = self
                .session
                .channel_open_session()
                .await
                .map_err(|_| SshError::Transport)?;
            Ok(Box::new(RusshExec {
                channel: Some(channel),
            }) as Box<dyn RemoteExec>)
        })
    }
    fn close<'a>(&'a self) -> SshFuture<'a, ()> {
        Box::pin(async move {
            self.session
                .disconnect(russh::Disconnect::ByApplication, "closed", "")
                .await
                .map_err(|_| SshError::Transport)
        })
    }
}

type ClientChannel = russh::Channel<client::Msg>;

async fn await_success(channel: &mut ClientChannel) -> Result<(), SshError> {
    loop {
        match channel.wait().await {
            Some(russh::ChannelMsg::Success) => return Ok(()),
            Some(russh::ChannelMsg::Failure) | Some(russh::ChannelMsg::Close) | None => {
                return Err(SshError::Transport)
            }
            Some(russh::ChannelMsg::WindowAdjusted { .. }) => {}
            _ => return Err(SshError::Transport),
        }
    }
}

struct RusshTerminal {
    read: tokio::sync::Mutex<russh::ChannelReadHalf>,
    write: Arc<russh::ChannelWriteHalf<client::Msg>>,
    write_lock: tokio::sync::Mutex<()>,
    closed: watch::Sender<bool>,
    close_result: tokio::sync::Mutex<Option<bool>>,
}

async fn close_once<F, Fut>(
    state: &tokio::sync::Mutex<Option<bool>>,
    send: F,
) -> Result<(), SshError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), SshError>>,
{
    let mut result = state.lock().await;
    if *result == Some(true) {
        return Ok(());
    }
    send().await?;
    *result = Some(true);
    Ok(())
}
impl Drop for RusshTerminal {
    fn drop(&mut self) {
        self.closed.send_replace(true);
        if self
            .close_result
            .try_lock()
            .is_ok_and(|result| *result == Some(true))
        {
            return;
        }
        let writer = self.write.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = writer.close().await;
            });
        }
    }
}
impl TerminalChannel for RusshTerminal {
    fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let mut closed = self.closed.subscribe();
            if *closed.borrow() {
                return Ok(None);
            }
            let mut read = self.read.lock().await;
            if *closed.borrow_and_update() {
                return Ok(None);
            }
            loop {
                tokio::select! { _=closed.changed()=>return Ok(None), message=read.wait()=>match message {Some(russh::ChannelMsg::Data{data})|Some(russh::ChannelMsg::ExtendedData{data,..})=>return Ok(Some(data.to_vec())),Some(russh::ChannelMsg::Eof)|Some(russh::ChannelMsg::Close)|None=>{self.close().await?;return Ok(None)},_=>{}} }
            }
        })
    }
    fn write<'a>(&'a self, bytes: &'a [u8]) -> SshFuture<'a, ()> {
        Box::pin(async move {
            if *self.closed.borrow() {
                return Err(SshError::Transport);
            }
            let mut closed = self.closed.subscribe();
            if *closed.borrow_and_update() {
                return Err(SshError::Transport);
            }
            let _write_order = tokio::select! { biased; _ = closed.changed() => return Err(SshError::Transport), lock = self.write_lock.lock() => lock };
            if *closed.borrow_and_update() {
                return Err(SshError::Transport);
            }
            tokio::select! { biased; _ = closed.changed() => Err(SshError::Transport), result = self.write.data(bytes) => result.map_err(|_| SshError::Transport) }
        })
    }
    fn resize<'a>(&'a self, size: TerminalSize) -> SshFuture<'a, ()> {
        Box::pin(async move {
            if *self.closed.borrow() {
                return Err(SshError::Transport);
            }
            let mut closed = self.closed.subscribe();
            if *closed.borrow_and_update() {
                return Err(SshError::Transport);
            }
            tokio::select! { biased; _=closed.changed()=>Err(SshError::Transport), result=self.write.window_change(size.columns.into(),size.rows.into(),0,0)=>result.map_err(|_|SshError::Transport) }
        })
    }
    fn close<'a>(&'a self) -> SshFuture<'a, ()> {
        Box::pin(async move {
            self.closed.send_replace(true);
            close_once(&self.close_result, || async {
                self.write.close().await.map_err(|_| SshError::Transport)
            })
            .await
        })
    }
}

struct RusshExec {
    channel: Option<ClientChannel>,
}
impl RusshExec {
    fn abort_inner(&mut self) {
        if let Some(channel) = self.channel.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = channel.close().await;
                });
            }
        }
    }
}
impl Drop for RusshExec {
    fn drop(&mut self) {
        self.abort_inner();
    }
}
struct ExecChannel(Option<ClientChannel>);
impl Drop for ExecChannel {
    fn drop(&mut self) {
        if let Some(channel) = self.0.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = channel.close().await;
                });
            }
        }
    }
}
impl RemoteExec for RusshExec {
    fn execute<'a>(&'a mut self, command: &'a str) -> SshFuture<'a, RemoteOutput> {
        self.execute_bounded(command, 1024 * 1024, 1024 * 1024)
    }
    fn execute_bounded<'a>(
        &'a mut self,
        command: &'a str,
        stdout_limit: usize,
        stderr_limit: usize,
    ) -> SshFuture<'a, RemoteOutput> {
        Box::pin(async move {
            let mut owned = ExecChannel(Some(self.channel.take().ok_or(SshError::Transport)?));
            let channel = owned.0.as_mut().ok_or(SshError::Transport)?;
            channel
                .exec(true, command.as_bytes())
                .await
                .map_err(|_| SshError::Transport)?;
            await_success(channel).await?;
            let mut output = BoundedRemoteOutput::new(stdout_limit, stderr_limit);
            let mut status = None;
            loop {
                match channel.wait().await {
                    Some(russh::ChannelMsg::Data { data }) => output.push_stdout(&data)?,
                    Some(russh::ChannelMsg::ExtendedData { data, .. }) => {
                        output.push_stderr(&data)?
                    }
                    Some(russh::ChannelMsg::ExitStatus { exit_status }) => {
                        status = i32::try_from(exit_status).ok()
                    }
                    Some(russh::ChannelMsg::Eof) => {}
                    Some(russh::ChannelMsg::Close) => {
                        if status.is_none() {
                            return Err(SshError::Transport);
                        }
                        break;
                    }
                    None => return Err(SshError::Transport),
                    _ => {}
                }
            }
            Ok(output.finish(status))
        })
    }
    fn close<'a>(&'a mut self) -> SshFuture<'a, ()> {
        Box::pin(async move {
            if let Some(channel) = self.channel.take() {
                let mut owned = ExecChannel(Some(channel));
                owned
                    .0
                    .as_ref()
                    .ok_or(SshError::Transport)?
                    .close()
                    .await
                    .map_err(|_| SshError::Transport)?;
                owned.0.take();
            }
            Ok(())
        })
    }
    fn abort(&mut self) {
        self.abort_inner();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client::Handler as _;
    use russh::keys::ssh_key::certificate::{Builder, CertType};
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn blocking_key_work_is_bounded_and_cancelled_before_authentication() {
        use std::sync::atomic::AtomicUsize;
        let semaphore = Arc::new(tokio::sync::Semaphore::new(1));
        let entered = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let e = entered.clone();
        let r = release.clone();
        let first = tokio::spawn(run_key_work(semaphore.clone(), move || {
            e.notify_one();
            while !r.load(Ordering::Acquire) {
                std::thread::yield_now();
            }
            Ok(())
        }));
        entered.notified().await;
        assert!(matches!(
            run_key_work(semaphore.clone(), || Ok(())).await,
            Err(SshError::HostKeyBusy)
        ));
        first.abort();
        let _ = first.await;
        assert!(matches!(
            run_key_work(semaphore.clone(), || Ok(())).await,
            Err(SshError::HostKeyBusy)
        ));
        release.store(true, Ordering::Release);
        let calls = Arc::new(AtomicUsize::new(0));
        let work_calls = calls.clone();
        tokio::time::timeout(Duration::from_secs(2), async {
            while semaphore.available_permits() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        run_key_work(semaphore, move || {
            work_calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failed_close_is_retryable_and_success_is_cached() {
        let state = tokio::sync::Mutex::new(None);
        let calls = std::sync::atomic::AtomicUsize::new(0);
        assert!(close_once(&state, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(SshError::Transport)
        })
        .await
        .is_err());
        assert!(close_once(&state, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_ok());
        assert!(close_once(&state, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn cancelled_close_does_not_block_retry() {
        let state = tokio::sync::Mutex::new(None);
        let mut first = Box::pin(close_once(&state, || std::future::pending()));
        assert!(tokio::time::timeout(Duration::from_millis(10), &mut first)
            .await
            .is_err());
        drop(first);
        assert!(close_once(&state, || async { Ok(()) }).await.is_ok());
    }

    const KEY: &str = "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAACmFlczI1Ni1jYmMAAAAGYmNyeXB0AAAAGAAAABDLGyfA39\nJ2FcJygtYqi5ISAAAAEAAAAAEAAAAzAAAAC3NzaC1lZDI1NTE5AAAAIN+Wjn4+4Fcvl2Jl\nKpggT+wCRxpSvtqqpVrQrKN1/A22AAAAkOHDLnYZvYS6H9Q3S3Nk4ri3R2jAZlQlBbUos5\nFkHpYgNw65KCWCTXtP7ye2czMC3zjn2r98pJLobsLYQgRiHIv/CUdAdsqbvMPECB+wl/UQ\ne+JpiSq66Z6GIt0801skPh20jxOO3F52SoX1IeO5D5PXfZrfSZlw6S8c7bwyp2FHxDewRx\n7/wNsnDM0T7nLv/Q==\n-----END OPENSSH PRIVATE KEY-----";

    #[tokio::test]
    async fn host_certificate_is_rejected_without_downgrading_to_raw_key() {
        let key = russh::keys::decode_secret_key(KEY, Some("blabla")).unwrap();
        let mut builder = Builder::new(vec![7; 16], key.public_key().clone(), 1, 2).unwrap();
        builder.cert_type(CertType::Host).unwrap();
        builder.valid_principal("host").unwrap();
        let cert = builder.sign(&key).unwrap();
        let (sender, mut receiver) = mpsc::unbounded_channel();
        let rejected = Arc::new(AtomicBool::new(false));
        let mut handler = Handler {
            host: "host".into(),
            port: 22,
            challenge: sender,
            rejected: rejected.clone(),
        };
        assert!(!handler.check_server_key(&cert.into()).await.unwrap());
        assert!(rejected.load(Ordering::SeqCst));
        assert!(receiver.try_recv().is_err());
    }

    #[tokio::test]
    async fn aborted_stream_flush_and_shutdown_fail() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let socket = tokio::net::TcpStream::connect(listener.local_addr().unwrap())
            .await
            .unwrap();
        let (_peer, _) = listener.accept().await.unwrap();
        let state = Arc::new(AbortState::default());
        let guard = AbortGuard(state.clone());
        let mut stream = AbortableStream {
            inner: socket,
            state,
        };
        drop(guard);
        assert_eq!(
            stream.flush().await.unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert_eq!(
            stream.shutdown().await.unwrap_err().kind(),
            io::ErrorKind::ConnectionAborted
        );
    }
}

//! Object-safe SSH interfaces. Production implementations own transport resources;
//! callers must not treat timeout or cancellation as proof a remote write did not run.
use coshell_platform::secret_store::Secret;
use std::{future::Future, pin::Pin, time::Duration};
use tokio::sync::watch;

mod russh_connector;
pub mod sftp;
pub use russh_connector::RusshConnector;

pub type SshFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, SshError>> + Send + 'a>>;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SshError {
    #[error("operation timed out")]
    Timeout,
    #[error("operation was cancelled")]
    Cancelled,
    #[error("host key was rejected")]
    HostKeyRejected,
    #[error("host key trust store is busy")]
    HostKeyBusy,
    #[error("host key changed")]
    HostKeyChanged {
        old_algorithm: String,
        old_fingerprint: String,
        new_algorithm: String,
        new_fingerprint: String,
    },
    #[error("operation is not supported")]
    Unsupported,
    #[error("SSH transport failed")]
    Transport,
    #[error("SSH authentication failed")]
    AuthenticationFailed,
}

#[derive(Debug)]
pub enum Authentication {
    Password(Secret),
    PrivateKey {
        key: Secret,
        passphrase: Option<Secret>,
    },
    Agent,
}

#[derive(Debug)]
pub struct ConnectionRequest {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub authentication: Authentication,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKey {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyDecision {
    AcceptOnce,
    AcceptAndSave,
    Reject,
}

pub trait HostKeyPermit: Send {
    fn authenticated(self: Box<Self>) -> Result<(), SshError>;
}
pub struct HostKeyVerification {
    pub decision: HostKeyDecision,
    pub permit: Option<Box<dyn HostKeyPermit>>,
}
impl HostKeyVerification {
    pub fn decision(decision: HostKeyDecision) -> Self {
        Self {
            decision,
            permit: None,
        }
    }
}

pub trait HostKeyVerifier: Send + Sync {
    fn verify<'a>(&'a self, key: &'a HostKey) -> SshFuture<'a, HostKeyVerification>;
}

pub trait SshConnector: Send + Sync {
    fn connect<'a>(
        &'a self,
        request: ConnectionRequest,
        verifier: &'a dyn HostKeyVerifier,
    ) -> SshFuture<'a, Box<dyn SshConnection>>;
}

pub trait SshConnection: Send + Sync {
    fn open_terminal<'a>(&'a self, size: TerminalSize) -> SshFuture<'a, Box<dyn TerminalChannel>>;
    fn remote_exec<'a>(&'a self) -> SshFuture<'a, Box<dyn RemoteExec>>;
    fn open_sftp<'a>(&'a self) -> SshFuture<'a, Box<dyn sftp::Sftp>> {
        Box::pin(async { Err(SshError::Unsupported) })
    }
    fn close<'a>(&'a self) -> SshFuture<'a, ()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalSize {
    pub columns: u16,
    pub rows: u16,
}

/// Implementations must allow an output read to wait while input, resize and
/// close proceed independently; concurrent writes must preserve their order.
pub trait TerminalChannel: Send + Sync {
    /// `None` indicates EOF; empty buffers are not EOF.
    fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>>;
    fn write<'a>(&'a self, bytes: &'a [u8]) -> SshFuture<'a, ()>;
    fn resize<'a>(&'a self, size: TerminalSize) -> SshFuture<'a, ()>;
    fn close<'a>(&'a self) -> SshFuture<'a, ()>;
}

#[derive(Debug, PartialEq, Eq)]
pub struct RemoteOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub exit_status: Option<i32>,
}

/// Incrementally collects a remote exec response without exceeding either cap.
pub struct BoundedRemoteOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_limit: usize,
    stderr_limit: usize,
}

impl BoundedRemoteOutput {
    pub fn new(stdout_limit: usize, stderr_limit: usize) -> Self {
        Self {
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_limit,
            stderr_limit,
        }
    }
    pub fn push_stdout(&mut self, chunk: &[u8]) -> Result<(), SshError> {
        if chunk.len() > self.stdout_limit.saturating_sub(self.stdout.len()) {
            return Err(SshError::Transport);
        }
        self.stdout.extend_from_slice(chunk);
        Ok(())
    }
    pub fn push_stderr(&mut self, chunk: &[u8]) -> Result<(), SshError> {
        if chunk.len() > self.stderr_limit.saturating_sub(self.stderr.len()) {
            return Err(SshError::Transport);
        }
        self.stderr.extend_from_slice(chunk);
        Ok(())
    }
    pub fn finish(self, exit_status: Option<i32>) -> RemoteOutput {
        RemoteOutput {
            stdout: self.stdout,
            stderr: self.stderr,
            exit_status,
        }
    }
}

pub trait RemoteExec: Send {
    fn execute<'a>(&'a mut self, command: &'a str) -> SshFuture<'a, RemoteOutput>;
    /// Enforce both caps while receiving chunks; never fall back to unbounded execute.
    fn execute_bounded<'a>(
        &'a mut self,
        _command: &'a str,
        _stdout_limit: usize,
        _stderr_limit: usize,
    ) -> SshFuture<'a, RemoteOutput> {
        Box::pin(async { Err(SshError::Unsupported) })
    }
    fn close<'a>(&'a mut self) -> SshFuture<'a, ()>;
    /// Immediately and idempotently terminate in-flight I/O, including on future drop.
    fn abort(&mut self);
}

/// Shared cancellation state stays cancelled for every future subscriber.
#[derive(Clone, Debug)]
pub struct Cancellation {
    sender: watch::Sender<bool>,
}

impl Default for Cancellation {
    fn default() -> Self {
        Self::new()
    }
}
impl Cancellation {
    pub fn new() -> Self {
        let (sender, _) = watch::channel(false);
        Self { sender }
    }
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }
}

/// Bound a single operation. Dropping a pending future is required to release
/// its owned resources; side-effecting callers must record an unknown outcome.
pub async fn run_cancellable<T>(
    duration: Duration,
    cancel: &Cancellation,
    future: impl Future<Output = Result<T, SshError>>,
) -> Result<T, SshError> {
    let mut receiver = cancel.sender.subscribe();
    if *receiver.borrow() {
        return Err(SshError::Cancelled);
    }
    tokio::select! {
        biased;
        changed = receiver.changed() => {
            match changed { Ok(()) if *receiver.borrow() => Err(SshError::Cancelled), _ => Err(SshError::Cancelled) }
        }
        result = tokio::time::timeout(duration, future) => match result { Ok(result) => result, Err(_) => Err(SshError::Timeout) },
    }
}

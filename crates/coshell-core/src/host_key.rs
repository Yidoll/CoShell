//! Trust-on-first-use policy. Host identity is never replaced on mismatch.
use coshell_drivers::ssh::{
    HostKey, HostKeyDecision, HostKeyVerification, HostKeyVerifier, SshError, SshFuture,
};
use coshell_storage::host_key::{HostKeyRepository, HostKeyStatus};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyTrust {
    Unknown,
    Match,
    Changed,
}

pub trait HostKeyStore: Send + Sync {
    fn inspect(&self, key: &HostKey) -> Result<HostKeyTrust, SshError>;
    fn insert_if_unknown(&self, key: &HostKey) -> Result<HostKeyTrust, SshError>;
    fn changed_error(&self, key: &HostKey) -> Result<SshError, SshError>;
    fn acquire_permit(
        &self,
        key: &HostKey,
        save: bool,
        require_known: bool,
    ) -> Result<Box<dyn coshell_drivers::ssh::HostKeyPermit>, SshError>;
}
pub trait HostKeyPrompt: Send + Sync {
    fn ask<'a>(&'a self, key: &'a HostKey) -> SshFuture<'a, HostKeyDecision>;
}

pub struct TofuVerifier<S, P> {
    store: Arc<S>,
    prompt: Arc<P>,
}
impl<S, P> TofuVerifier<S, P> {
    pub fn new(store: Arc<S>, prompt: Arc<P>) -> Self {
        Self { store, prompt }
    }
}
impl<S: HostKeyStore, P: HostKeyPrompt> HostKeyVerifier for TofuVerifier<S, P> {
    fn verify<'a>(&'a self, key: &'a HostKey) -> SshFuture<'a, HostKeyVerification> {
        Box::pin(async move {
            match self.store.inspect(key)? {
                HostKeyTrust::Match => Ok(HostKeyVerification {
                    decision: HostKeyDecision::AcceptOnce,
                    permit: Some(self.store.acquire_permit(key, false, true)?),
                }),
                HostKeyTrust::Changed => Err(self.store.changed_error(key)?),
                HostKeyTrust::Unknown => match self.prompt.ask(key).await? {
                    HostKeyDecision::Reject => {
                        Ok(HostKeyVerification::decision(HostKeyDecision::Reject))
                    }
                    decision => {
                        let permit = self.store.acquire_permit(
                            key,
                            decision == HostKeyDecision::AcceptAndSave,
                            false,
                        )?;
                        Ok(HostKeyVerification {
                            decision,
                            permit: Some(permit),
                        })
                    }
                },
            }
        })
    }
}

pub struct SqliteHostKeyStore {
    db: Arc<Mutex<Option<rusqlite::Connection>>>,
    configured: bool,
}
impl SqliteHostKeyStore {
    /// Takes exclusive ownership of the connection and its busy handler.
    /// Host-key operations never wait for SQLite locks on the async executor.
    pub fn new(db: rusqlite::Connection) -> Self {
        let configured = db.busy_timeout(std::time::Duration::ZERO).is_ok();
        Self {
            db: Arc::new(Mutex::new(Some(db))),
            configured,
        }
    }
}
fn map_storage_error(error: rusqlite::Error) -> SshError {
    if matches!(error, rusqlite::Error::SqliteFailure(code, _) if matches!(code.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
    {
        SshError::HostKeyBusy
    } else {
        SshError::Transport
    }
}
fn map_status(status: HostKeyStatus) -> HostKeyTrust {
    match status {
        HostKeyStatus::Unknown => HostKeyTrust::Unknown,
        HostKeyStatus::Match => HostKeyTrust::Match,
        HostKeyStatus::Changed => HostKeyTrust::Changed,
    }
}
impl HostKeyStore for SqliteHostKeyStore {
    fn acquire_permit(
        &self,
        key: &HostKey,
        save: bool,
        require_known: bool,
    ) -> Result<Box<dyn coshell_drivers::ssh::HostKeyPermit>, SshError> {
        if !self.configured {
            return Err(SshError::Transport);
        }
        let db = self
            .db
            .lock()
            .map_err(|_| SshError::Transport)?
            .take()
            .ok_or(SshError::HostKeyBusy)?;
        let permit = coshell_storage::host_key::HostKeyPermit::acquire_connection(
            db,
            &key.host,
            key.port,
            &key.algorithm,
            &key.fingerprint,
            save,
            require_known,
        );
        match permit {
            Ok(permit) => Ok(Box::new(SqlitePermit {
                permit: Some(permit),
                db: self.db.clone(),
            })),
            Err((db, error)) => {
                *self.db.lock().map_err(|_| SshError::Transport)? = db;
                Err(match error {
                    coshell_storage::host_key::PermitError::Busy => SshError::HostKeyBusy,
                    coshell_storage::host_key::PermitError::TrustMissing => {
                        SshError::HostKeyRejected
                    }
                    coshell_storage::host_key::PermitError::Changed => {
                        self.changed_error(key).unwrap_or_else(|error| error)
                    }
                    coshell_storage::host_key::PermitError::Storage(_) => SshError::Transport,
                })
            }
        }
    }
    fn inspect(&self, key: &HostKey) -> Result<HostKeyTrust, SshError> {
        if !self.configured {
            return Err(SshError::Transport);
        }
        let mut guard = self.db.lock().map_err(|_| SshError::Transport)?;
        let db = guard.as_mut().ok_or(SshError::HostKeyBusy)?;
        HostKeyRepository::new(db)
            .inspect(&key.host, key.port, &key.algorithm, &key.fingerprint)
            .map(map_status)
            .map_err(map_storage_error)
    }
    fn insert_if_unknown(&self, key: &HostKey) -> Result<HostKeyTrust, SshError> {
        if !self.configured {
            return Err(SshError::Transport);
        }
        let mut guard = self.db.lock().map_err(|_| SshError::Transport)?;
        let db = guard.as_mut().ok_or(SshError::HostKeyBusy)?;
        HostKeyRepository::new(db)
            .insert_if_unknown(&key.host, key.port, &key.algorithm, &key.fingerprint)
            .map(map_status)
            .map_err(map_storage_error)
    }
    fn changed_error(&self, key: &HostKey) -> Result<SshError, SshError> {
        if !self.configured {
            return Err(SshError::Transport);
        }
        let mut guard = self.db.lock().map_err(|_| SshError::Transport)?;
        let db = guard.as_mut().ok_or(SshError::HostKeyBusy)?;
        let (old_algorithm, old_fingerprint) = HostKeyRepository::new(db)
            .existing_identity(&key.host, key.port)
            .map_err(map_storage_error)?
            .ok_or(SshError::Transport)?;
        fn safe(value: &str) -> bool {
            !value.is_empty()
                && value.len() <= 256
                && value
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-._:+/=".contains(&b))
        }
        if ![
            old_algorithm.as_str(),
            old_fingerprint.as_str(),
            key.algorithm.as_str(),
            key.fingerprint.as_str(),
        ]
        .iter()
        .all(|v| safe(v))
        {
            return Err(SshError::Transport);
        }
        Ok(SshError::HostKeyChanged {
            old_algorithm,
            old_fingerprint,
            new_algorithm: key.algorithm.clone(),
            new_fingerprint: key.fingerprint.clone(),
        })
    }
}
struct SqlitePermit {
    permit: Option<coshell_storage::host_key::HostKeyPermit>,
    db: Arc<Mutex<Option<rusqlite::Connection>>>,
}
impl coshell_drivers::ssh::HostKeyPermit for SqlitePermit {
    fn authenticated(mut self: Box<Self>) -> Result<(), SshError> {
        let (db, result) = self.permit.take().ok_or(SshError::Transport)?.finish(true);
        *self.db.lock().map_err(|_| SshError::Transport)? = db;
        result.map_err(|_| SshError::Transport)
    }
}
impl Drop for SqlitePermit {
    fn drop(&mut self) {
        if let Some(permit) = self.permit.take() {
            let (db, _) = permit.finish(false);
            if let Ok(mut guard) = self.db.lock() {
                *guard = db;
            }
        }
    }
}

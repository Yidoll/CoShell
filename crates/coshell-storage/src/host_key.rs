//! Atomic TOFU host-key persistence, scoped to a host and port.
use rusqlite::{params, Connection, Result, TransactionBehavior};
use std::net::IpAddr;
use std::path::Path;

/// Normalize DNS names and IP literals to the identity used by the trust store.
pub fn canonical_host(host: &str) -> Result<String> {
    if host.is_empty() || host.trim() != host || host.chars().any(char::is_control) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let literal = if let Some(value) = host.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        value
    } else {
        host
    };
    if let Ok(address) = literal.parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    if literal.contains(':') || host.starts_with('[') || host.ends_with(']') {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let name = host.strip_suffix('.').unwrap_or(host);
    if name.is_empty() || name.ends_with('.') {
        return Err(rusqlite::Error::InvalidQuery);
    }
    idna::domain_to_ascii_strict(name)
        .map(|name| name.to_ascii_lowercase())
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostKeyStatus {
    Unknown,
    Match,
    Changed,
}

/// Holds the SQLite write reservation until authentication finishes. Dropping
/// the connection rolls back automatically, including on cancellation.
pub struct HostKeyPermit {
    db: Connection,
    pending_save: bool,
    busy_timeout_ms: u32,
}
#[derive(Debug)]
pub enum PermitError {
    Busy,
    Changed,
    TrustMissing,
    Storage(rusqlite::Error),
}
impl From<rusqlite::Error> for PermitError {
    fn from(e: rusqlite::Error) -> Self {
        if matches!(&e,rusqlite::Error::SqliteFailure(code,_) if matches!(code.code,rusqlite::ErrorCode::DatabaseBusy|rusqlite::ErrorCode::DatabaseLocked))
        {
            Self::Busy
        } else {
            Self::Storage(e)
        }
    }
}
impl HostKeyPermit {
    pub fn acquire_connection(
        db: Connection,
        host: &str,
        port: u16,
        algorithm: &str,
        fingerprint: &str,
        save: bool,
        require_known: bool,
    ) -> std::result::Result<Self, (Option<Connection>, PermitError)> {
        if !db.is_autocommit() {
            return Err((
                Some(db),
                PermitError::Storage(rusqlite::Error::InvalidQuery),
            ));
        }
        let busy_timeout_ms: u32 = match db.pragma_query_value(None, "busy_timeout", |r| r.get(0)) {
            Ok(value) => value,
            Err(error) => return Err((Some(db), PermitError::Storage(error))),
        };
        let mut began = false;
        let attempt = (|| -> std::result::Result<(), PermitError> {
            db.busy_timeout(std::time::Duration::ZERO)?;
            db.execute_batch("BEGIN IMMEDIATE")?;
            began = true;
            match inspect(&db, host, port, algorithm, fingerprint)? {
                HostKeyStatus::Changed => return Err(PermitError::Changed),
                HostKeyStatus::Unknown if require_known => return Err(PermitError::TrustMissing),
                HostKeyStatus::Unknown if save => {
                    db.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES (?1,?2,?3,?4)",params![canonical_host(host)?,port,algorithm,fingerprint])?;
                }
                _ => {}
            }
            Ok(())
        })();
        match attempt {
            Ok(()) => Ok(Self {
                db,
                pending_save: save,
                busy_timeout_ms,
            }),
            Err(error) => {
                if began && db.execute_batch("ROLLBACK").is_err() {
                    return Err((None, error));
                }
                if db
                    .busy_timeout(std::time::Duration::from_millis(busy_timeout_ms.into()))
                    .is_err()
                {
                    return Err((None, error));
                }
                Err((Some(db), error))
            }
        }
    }
    pub fn finish(self, commit: bool) -> (Option<Connection>, Result<()>) {
        let result = self.db.execute_batch(if commit && self.pending_save {
            "COMMIT"
        } else {
            "ROLLBACK"
        });
        if result.is_err() && !self.db.is_autocommit() && self.db.execute_batch("ROLLBACK").is_err()
        {
            return (None, result);
        }
        if self
            .db
            .busy_timeout(std::time::Duration::from_millis(
                self.busy_timeout_ms.into(),
            ))
            .is_err()
        {
            return (None, result.and(Err(rusqlite::Error::InvalidQuery)));
        }
        (Some(self.db), result)
    }
    pub fn acquire(
        path: &Path,
        host: &str,
        port: u16,
        algorithm: &str,
        fingerprint: &str,
        save: bool,
    ) -> std::result::Result<Self, PermitError> {
        let db = Connection::open(path)?;
        db.busy_timeout(std::time::Duration::ZERO)?;
        db.execute_batch("BEGIN IMMEDIATE")?;
        match inspect(&db, host, port, algorithm, fingerprint)? {
            HostKeyStatus::Changed => return Err(PermitError::Changed),
            HostKeyStatus::Unknown if save => {
                db.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES (?1,?2,?3,?4)",params![canonical_host(host)?,port,algorithm,fingerprint])?;
            }
            _ => {}
        }
        Ok(Self {
            db,
            pending_save: save,
            busy_timeout_ms: 0,
        })
    }
    pub fn authenticated(self) -> Result<()> {
        if self.pending_save {
            self.db.execute_batch("COMMIT")?;
        } else {
            self.db.execute_batch("ROLLBACK")?;
        }
        Ok(())
    }
}

fn inspect(
    db: &Connection,
    host: &str,
    port: u16,
    algorithm: &str,
    fingerprint: &str,
) -> Result<HostKeyStatus> {
    let target = canonical_host(host)?;
    let mut query =
        db.prepare("SELECT host,algorithm,fingerprint FROM trusted_host_keys WHERE port=?1")?;
    let rows = query.query_map([port], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut known = false;
    let mut matched = false;
    let mut conflicting = false;
    for row in rows {
        let (saved_host, saved_algorithm, saved_fingerprint) = row?;
        if canonical_host(&saved_host)? == target {
            known = true;
            if saved_algorithm == algorithm && saved_fingerprint == fingerprint {
                matched = true;
            } else {
                conflicting = true;
            }
        }
    }
    Ok(if conflicting {
        HostKeyStatus::Changed
    } else if matched {
        HostKeyStatus::Match
    } else if known {
        HostKeyStatus::Changed
    } else {
        HostKeyStatus::Unknown
    })
}

pub struct HostKeyRepository<'a> {
    db: &'a mut Connection,
}
impl<'a> HostKeyRepository<'a> {
    pub fn new(db: &'a mut Connection) -> Self {
        Self { db }
    }
    /// Return a deterministic existing identity for a canonical host and port.
    pub fn existing_identity(&self, host: &str, port: u16) -> Result<Option<(String, String)>> {
        let target = canonical_host(host)?;
        let mut query=self.db.prepare("SELECT host,algorithm,fingerprint FROM trusted_host_keys WHERE port=?1 ORDER BY host,algorithm,fingerprint")?;
        let rows = query.query_map([port], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (host, algorithm, fingerprint) = row?;
            if canonical_host(&host)? == target {
                return Ok(Some((algorithm, fingerprint)));
            }
        }
        Ok(None)
    }
    pub fn inspect(
        &self,
        host: &str,
        port: u16,
        algorithm: &str,
        fingerprint: &str,
    ) -> Result<HostKeyStatus> {
        inspect(self.db, host, port, algorithm, fingerprint)
    }
    /// Caller must ask the user before this method. The write lock makes a
    /// second concurrent trust decision re-check against the first commit.
    pub fn insert_if_unknown(
        &mut self,
        host: &str,
        port: u16,
        algorithm: &str,
        fingerprint: &str,
    ) -> Result<HostKeyStatus> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let status = inspect(&tx, host, port, algorithm, fingerprint)?;
        if status == HostKeyStatus::Unknown {
            tx.execute("INSERT INTO trusted_host_keys(host,port,algorithm,fingerprint) VALUES (?1,?2,?3,?4)",params![canonical_host(host)?,port,algorithm,fingerprint])?;
        }
        tx.commit()?;
        Ok(if status == HostKeyStatus::Unknown {
            HostKeyStatus::Match
        } else {
            status
        })
    }
}

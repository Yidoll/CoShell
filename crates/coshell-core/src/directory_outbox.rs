//! Durable independent-directory publications. Packages are always encrypted locally,
//! including when the user's remote backup intentionally has no password.
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    XChaCha20Poly1305, XNonce,
};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use zeroize::Zeroizing;

const MAX_BODY: usize = 48 * 1024 * 1024;
#[derive(Debug, PartialEq, Eq)]
pub enum QueueError {
    Invalid,
    Conflict,
    Storage,
    Authentication,
}
pub struct PendingDirectory {
    pub target: String,
    pub account: String,
    pub directory: String,
    pub body: Zeroizing<Vec<u8>>,
    pub state: String,
}
pub struct DirectoryOutbox<'a> {
    db: &'a mut Connection,
    key: Zeroizing<[u8; 32]>,
}
fn valid_id(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn aad(operation: &str, target: &str, directory: &str, account: &str) -> Vec<u8> {
    let mut out = b"coshell-directory-outbox-v1".to_vec();
    for part in [operation, target, directory, account] {
        out.extend_from_slice(&(part.len() as u64).to_be_bytes());
        out.extend_from_slice(part.as_bytes());
    }
    out
}

/// Also used before generating a new OS key: protected rows must never be made
/// unrecoverable by silently replacing a missing key.
pub fn has_directory_rows(db: &Connection) -> Result<bool, QueueError> {
    let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='directory_publications')",[],|r|r.get(0)).map_err(|_|QueueError::Storage)?;
    if !exists {
        return Ok(false);
    }
    db.query_row(
        "SELECT EXISTS(SELECT 1 FROM directory_publications)",
        [],
        |r| r.get(0),
    )
    .map_err(|_| QueueError::Storage)
}
pub fn directory_status(
    db: &Connection,
    operation: &str,
) -> Result<Option<crate::publication_outbox::PublicationStatus>, QueueError> {
    if !has_directory_rows(db)? {
        return Ok(None);
    }
    db.query_row(
        "SELECT state,error FROM directory_publications WHERE operation_id=?1",
        [operation],
        |r| {
            Ok(crate::publication_outbox::PublicationStatus {
                state: r.get(0)?,
                error: r.get(1)?,
                cleanup_pending: false,
                possible_objects: Vec::new(),
            })
        },
    )
    .optional()
    .map_err(|_| QueueError::Storage)
}
pub fn directory_recent(
    db: &Connection,
) -> Result<Vec<crate::publication_outbox::RecentPublication>, QueueError> {
    if !has_directory_rows(db)? {
        return Ok(Vec::new());
    }
    let mut query = db
        .prepare("SELECT operation_id FROM directory_publications ORDER BY rowid DESC LIMIT 20")
        .map_err(|_| QueueError::Storage)?;
    let ids = query
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|_| QueueError::Storage)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| QueueError::Storage)?;
    ids.into_iter()
        .map(|id| {
            Ok(crate::publication_outbox::RecentPublication {
                status: directory_status(db, &id)?.ok_or(QueueError::Storage)?,
                operation_id: id,
            })
        })
        .collect()
}
impl<'a> DirectoryOutbox<'a> {
    pub fn new(db: &'a mut Connection, key: Zeroizing<[u8; 32]>) -> Result<Self, QueueError> {
        db.execute_batch("PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS directory_publications(operation_id TEXT PRIMARY KEY,target TEXT NOT NULL,account TEXT NOT NULL DEFAULT '',directory TEXT NOT NULL,package BLOB NOT NULL,state TEXT NOT NULL CHECK(state IN ('prepared','running','succeeded','failed','unknown')),error TEXT,UNIQUE(target,directory));").map_err(|_|QueueError::Storage)?;
        let bound:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('directory_publications') WHERE name='account')",[],|r|r.get(0)).map_err(|_|QueueError::Storage)?;
        if !bound {
            if has_directory_rows(db)? {
                return Err(QueueError::Authentication);
            }
            db.execute_batch(
                "ALTER TABLE directory_publications ADD COLUMN account TEXT NOT NULL DEFAULT '';",
            )
            .map_err(|_| QueueError::Storage)?;
        }
        Ok(Self { db, key })
    }
    pub fn has_protected_rows(&self) -> Result<bool, QueueError> {
        has_directory_rows(self.db)
    }
    pub fn prepare(
        &mut self,
        operation: &str,
        target: &str,
        directory: &str,
        body: &[u8],
    ) -> Result<(), QueueError> {
        self.prepare_account(operation, target, directory, body, "")
    }
    pub fn prepare_account(
        &mut self,
        operation: &str,
        target: &str,
        directory: &str,
        body: &[u8],
        account: &str,
    ) -> Result<(), QueueError> {
        let url = reqwestless_target(target)?;
        if account.len() > 1024
            || account.contains('\0')
            || !valid_id(operation)
            || directory.len() != 14
            || !directory.bytes().all(|b| b.is_ascii_digit())
            || body.is_empty()
            || body.len() > MAX_BODY
        {
            return Err(QueueError::Invalid);
        }
        coshell_backup::snapshot_manifest::SnapshotArtifact::unpack(body)
            .map_err(|_| QueueError::Invalid)?;
        if let Some(old) = self.load(operation)? {
            return if old.target == url
                && old.account == account
                && old.directory == directory
                && old.body.as_slice() == body
            {
                Ok(())
            } else {
                Err(QueueError::Conflict)
            };
        }
        let mut nonce = [0u8; 24];
        getrandom::getrandom(&mut nonce).map_err(|_| QueueError::Storage)?;
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())
            .map_err(|_| QueueError::Storage)?;
        let encrypted = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                chacha20poly1305::aead::Payload {
                    msg: body,
                    aad: &aad(operation, &url, directory, account),
                },
            )
            .map_err(|_| QueueError::Storage)?;
        let mut stored = nonce.to_vec();
        stored.extend_from_slice(&encrypted);
        self.db.execute("INSERT INTO directory_publications(operation_id,target,directory,package,state,account)VALUES(?1,?2,?3,?4,'prepared',?5)",params![operation,url,directory,stored,account]).map_err(|e|match e{rusqlite::Error::SqliteFailure(c,_)if c.code==rusqlite::ErrorCode::ConstraintViolation=>QueueError::Conflict,_=>QueueError::Storage})?;
        Ok(())
    }
    pub fn load(&self, operation: &str) -> Result<Option<PendingDirectory>, QueueError> {
        let length: Option<i64> = self
            .db
            .query_row(
                "SELECT length(package) FROM directory_publications WHERE operation_id=?1",
                [operation],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| QueueError::Storage)?;
        if length.is_some_and(|n| n < 40 || n > MAX_BODY as i64 + 40) {
            return Err(QueueError::Authentication);
        }
        let row:Option<(String,String,Vec<u8>,String,String)>=self.db.query_row("SELECT target,directory,package,state,account FROM directory_publications WHERE operation_id=?1 AND length(package)<=50331688",[operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|_|QueueError::Storage)?;
        row.map(|(target, directory, data, state, account)| {
            if data.len() < 40 || data.len() > MAX_BODY + 40 {
                return Err(QueueError::Authentication);
            }
            let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())
                .map_err(|_| QueueError::Storage)?;
            let body = Zeroizing::new(
                cipher
                    .decrypt(
                        XNonce::from_slice(&data[..24]),
                        chacha20poly1305::aead::Payload {
                            msg: &data[24..],
                            aad: &aad(operation, &target, &directory, &account),
                        },
                    )
                    .map_err(|_| QueueError::Authentication)?,
            );
            Ok(PendingDirectory {
                target,
                account,
                directory,
                body,
                state,
            })
        })
        .transpose()
    }
    pub fn claim(&mut self, operation: &str) -> Result<bool, QueueError> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| QueueError::Storage)?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM directory_publications WHERE operation_id=?1",
                [operation],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| QueueError::Storage)?;
        if state.as_deref() != Some("prepared") {
            return Ok(false);
        }
        let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM directory_publications WHERE target=(SELECT target FROM directory_publications WHERE operation_id=?1) AND operation_id<>?1 AND state IN('running','unknown'))",[operation],|r|r.get(0)).map_err(|_|QueueError::Storage)?;
        if busy {
            return Err(QueueError::Conflict);
        }
        if tx.execute("UPDATE directory_publications SET state='running' WHERE operation_id=?1 AND state='prepared'",[operation]).map_err(|_|QueueError::Storage)?!=1{return Err(QueueError::Conflict)}
        tx.commit().map_err(|_| QueueError::Storage)?;
        Ok(true)
    }
    pub fn finish(
        &mut self,
        operation: &str,
        state: &str,
        error: Option<&str>,
    ) -> Result<(), QueueError> {
        if !matches!(state, "succeeded" | "failed" | "unknown") {
            return Err(QueueError::Invalid);
        }
        if self.db.execute("UPDATE directory_publications SET state=?2,error=?3 WHERE operation_id=?1 AND state IN('running','unknown')",params![operation,state,error]).map_err(|_|QueueError::Storage)?!=1{return Err(QueueError::Conflict)}
        Ok(())
    }
    pub fn status(
        &self,
        operation: &str,
    ) -> Result<Option<crate::publication_outbox::PublicationStatus>, QueueError> {
        self.db
            .query_row(
                "SELECT state,error FROM directory_publications WHERE operation_id=?1",
                [operation],
                |r| {
                    Ok(crate::publication_outbox::PublicationStatus {
                        state: r.get(0)?,
                        error: r.get(1)?,
                        cleanup_pending: false,
                        possible_objects: Vec::new(),
                    })
                },
            )
            .optional()
            .map_err(|_| QueueError::Storage)
    }
    pub fn recent(&self) -> Result<Vec<crate::publication_outbox::RecentPublication>, QueueError> {
        let mut query = self
            .db
            .prepare("SELECT operation_id FROM directory_publications ORDER BY rowid DESC LIMIT 20")
            .map_err(|_| QueueError::Storage)?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| QueueError::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| QueueError::Storage)?;
        ids.into_iter()
            .map(|id| {
                Ok(crate::publication_outbox::RecentPublication {
                    status: self.status(&id)?.ok_or(QueueError::Storage)?,
                    operation_id: id,
                })
            })
            .collect()
    }
}
// The actual client validates schemes, host and redirect policy. This value is
// a stable exact target binding, never a network destination by itself.
fn reqwestless_target(value: &str) -> Result<String, QueueError> {
    if value.len() > 8192 {
        return Err(QueueError::Invalid);
    }
    coshell_backup::webdav::canonical_target(value).map_err(|_| QueueError::Invalid)
}

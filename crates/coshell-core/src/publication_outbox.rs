//! Durable encrypted publication attempts. This never stores credentials or plaintext.
use chacha20poly1305::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng},
    XChaCha20Poly1305, XNonce,
};
use coshell_backup::webdav::{PublicationAttempt, PublishCondition};
use rusqlite::{params, Connection, OptionalExtension};
use zeroize::{Zeroize, Zeroizing};

#[derive(Debug, PartialEq, Eq)]
pub enum OutboxError {
    Invalid,
    Conflict,
    Storage,
    OutcomeUnknown,
    TargetBusy,
    Remote(coshell_backup::webdav::WebDavError),
}
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationStatus {
    pub state: String,
    pub error: Option<String>,
    pub cleanup_pending: bool,
    pub possible_objects: Vec<String>,
}
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentPublication {
    pub operation_id: String,
    pub status: PublicationStatus,
}
pub struct PreparedPublication {
    pub target: String,
    pub snapshot_id: String,
    pub ciphertext: Vec<u8>,
    pub condition: PublishCondition,
    pub attempt: PublicationAttempt,
}
impl std::fmt::Debug for PreparedPublication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedPublication")
            .field("target", &self.target)
            .field("snapshot_id", &self.snapshot_id)
            .field("ciphertext", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}
impl Drop for PreparedPublication {
    fn drop(&mut self) {
        self.ciphertext.zeroize();
    }
}
pub struct Outbox<'a> {
    db: &'a mut Connection,
    local_key: Option<Zeroizing<[u8; 32]>>,
}
const LOCAL_PREFIX: &[u8] = b"coshell-outbox-v1\0";
const CREATE_OUTBOX: &str = "CREATE TABLE publication_outbox (operation_id TEXT PRIMARY KEY, target TEXT NOT NULL, snapshot_id TEXT NOT NULL, ciphertext BLOB NOT NULL, condition_json TEXT NOT NULL, attempt_json TEXT NOT NULL, state TEXT NOT NULL DEFAULT 'prepared' CHECK(state IN ('prepared','running','succeeded','failed','unknown')), error_code TEXT, cleanup_pending INTEGER NOT NULL DEFAULT 0, possible_objects TEXT NOT NULL DEFAULT '[]');";
impl<'a> Outbox<'a> {
    pub fn recent(&self) -> Result<Vec<RecentPublication>, OutboxError> {
        let mut query = self
            .db
            .prepare("SELECT operation_id FROM publication_outbox ORDER BY rowid DESC LIMIT 20")
            .map_err(|_| OutboxError::Storage)?;
        let ids = query
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(|_| OutboxError::Storage)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| OutboxError::Storage)?;
        ids.into_iter()
            .map(|id| {
                Ok(RecentPublication {
                    status: self.status(&id)?.ok_or(OutboxError::Storage)?,
                    operation_id: id,
                })
            })
            .collect()
    }
    /// Use a separate application-owned SQLite database in the protected data directory.
    pub fn new(db: &'a mut Connection) -> Result<Self, OutboxError> {
        db.execute_batch("PRAGMA synchronous=FULL;")
            .map_err(|_| OutboxError::Storage)?;
        let tx = db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| OutboxError::Storage)?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='publication_outbox')",[],|r|r.get(0)).map_err(|_|OutboxError::Storage)?;
        if !exists {
            tx.execute_batch(CREATE_OUTBOX)
                .map_err(|_| OutboxError::Storage)?;
        } else {
            // Never copy pre-existing plaintext records during schema migration.
            // Reject in place: rewriting SQLite pages cannot prove erasure of
            // old database/WAL bytes, so operator intervention is required.
            {
                #[derive(serde::Deserialize)]
                struct Version {
                    version: u32,
                }
                let mut query = tx
                    .prepare("SELECT ciphertext FROM publication_outbox")
                    .map_err(|_| OutboxError::Storage)?;
                let rows = query
                    .query_map([], |row| row.get::<_, Vec<u8>>(0))
                    .map_err(|_| OutboxError::Storage)?;
                for row in rows {
                    let value = Zeroizing::new(row.map_err(|_| OutboxError::Storage)?);
                    if serde_json::from_slice::<Version>(&value).is_ok_and(|v| v.version == 3) {
                        return Err(OutboxError::Invalid);
                    }
                }
            }
            let modern:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('publication_outbox') WHERE name='error_code')",[],|r|r.get(0)).map_err(|_|OutboxError::Storage)?;
            if !modern {
                let rows = {
                    let mut q=tx.prepare("SELECT operation_id,snapshot_id,ciphertext,condition_json,attempt_json,state FROM publication_outbox").map_err(|_|OutboxError::Storage)?;
                    let rows = q
                        .query_map([], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, Vec<u8>>(2)?,
                                r.get::<_, String>(3)?,
                                r.get::<_, String>(4)?,
                                r.get::<_, String>(5)?,
                            ))
                        })
                        .map_err(|_| OutboxError::Storage)?
                        .collect::<Result<Vec<_>, _>>()
                        .map_err(|_| OutboxError::Storage)?;
                    rows
                };
                tx.execute_batch(
                    "ALTER TABLE publication_outbox RENAME TO publication_outbox_legacy;",
                )
                .map_err(|_| OutboxError::Storage)?;
                tx.execute_batch(CREATE_OUTBOX)
                    .map_err(|_| OutboxError::Storage)?;
                for (id, snapshot, ciphertext, condition, attempt_json, state) in rows {
                    let attempt: PublicationAttempt =
                        serde_json::from_str(&attempt_json).map_err(|_| OutboxError::Storage)?;
                    let possible_objects = serde_json::to_string(&attempt.possible_objects())
                        .map_err(|_| OutboxError::Storage)?;
                    tx.execute("INSERT INTO publication_outbox(operation_id,target,snapshot_id,ciphertext,condition_json,attempt_json,state,cleanup_pending,possible_objects) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",params![id,attempt.target(),snapshot,ciphertext,condition,attempt_json,state,state != "prepared",if state == "prepared" { "[]" } else { &possible_objects }]).map_err(|_|OutboxError::Storage)?;
                }
                tx.execute_batch("DROP TABLE publication_outbox_legacy;")
                    .map_err(|_| OutboxError::Storage)?;
            }
        }
        tx.commit().map_err(|_| OutboxError::Storage)?;
        Ok(Self {
            db,
            local_key: None,
        })
    }
    /// Key must be generated and durably stored in the OS credential store
    /// before this outbox is opened. Losing it makes queued data unrecoverable.
    pub fn new_protected(
        db: &'a mut Connection,
        key: Zeroizing<[u8; 32]>,
    ) -> Result<Self, OutboxError> {
        let mut result = Self::new(db)?;
        result.local_key = Some(key);
        Ok(result)
    }
    /// The caller must not create a replacement key when protected rows exist.
    pub fn has_protected_rows(&self) -> Result<bool, OutboxError> {
        let mut stmt = self
            .db
            .prepare("SELECT ciphertext FROM publication_outbox")
            .map_err(|_| OutboxError::Storage)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|_| OutboxError::Storage)?;
        for row in rows {
            if row
                .map_err(|_| OutboxError::Storage)?
                .starts_with(LOCAL_PREFIX)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
    fn protect(&self, id: &str, data: &[u8]) -> Result<Vec<u8>, OutboxError> {
        let key = self.local_key.as_ref().ok_or(OutboxError::Invalid)?;
        let mut nonce = [0u8; 24];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| OutboxError::Storage)?;
        let cipher =
            XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| OutboxError::Storage)?;
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                chacha20poly1305::aead::Payload {
                    msg: data,
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| OutboxError::Storage)?;
        let mut result = LOCAL_PREFIX.to_vec();
        result.extend_from_slice(&nonce);
        result.extend_from_slice(&ciphertext);
        Ok(result)
    }
    fn unprotect(&self, id: &str, data: Vec<u8>) -> Result<Vec<u8>, OutboxError> {
        Self::unprotect_with(self.local_key.as_deref(), id, data)
    }
    fn unprotect_with(
        key: Option<&[u8; 32]>,
        id: &str,
        data: Vec<u8>,
    ) -> Result<Vec<u8>, OutboxError> {
        if !data.starts_with(LOCAL_PREFIX) {
            return Ok(data);
        }
        let key = key.ok_or(OutboxError::Storage)?;
        if data.len() < LOCAL_PREFIX.len() + 24 + 16 {
            return Err(OutboxError::Storage);
        }
        let cipher =
            XChaCha20Poly1305::new_from_slice(key.as_ref()).map_err(|_| OutboxError::Storage)?;
        let start = LOCAL_PREFIX.len();
        cipher
            .decrypt(
                XNonce::from_slice(&data[start..start + 24]),
                chacha20poly1305::aead::Payload {
                    msg: &data[start + 24..],
                    aad: id.as_bytes(),
                },
            )
            .map_err(|_| OutboxError::Storage)
    }
    pub fn load(&self, id: &str) -> Result<Option<PreparedPublication>, OutboxError> {
        let row:Option<(String,String,Vec<u8>,String,String)> = self.db.query_row("SELECT target,snapshot_id,ciphertext,condition_json,attempt_json FROM publication_outbox WHERE operation_id=?1",[id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|_|OutboxError::Storage)?;
        row.map(|(target, snapshot_id, ciphertext, condition, attempt)| {
            Ok(PreparedPublication {
                target,
                snapshot_id,
                ciphertext: self.unprotect(id, ciphertext)?,
                condition: serde_json::from_str(&condition).map_err(|_| OutboxError::Storage)?,
                attempt: serde_json::from_str(&attempt).map_err(|_| OutboxError::Storage)?,
            })
        })
        .transpose()
    }
    pub fn prepare(
        &mut self,
        operation: &str,
        target: &str,
        snapshot: &str,
        ciphertext: &[u8],
        password: &[u8],
        condition: PublishCondition,
    ) -> Result<PreparedPublication, OutboxError> {
        self.prepare_inner(
            operation, target, snapshot, ciphertext, password, condition, None,
        )
    }
    pub fn prepare_timestamped(
        &mut self,
        operation: &str,
        target: &str,
        snapshot: &str,
        ciphertext: &[u8],
        password: &[u8],
        condition: PublishCondition,
        directory: &str,
    ) -> Result<PreparedPublication, OutboxError> {
        self.prepare_inner(
            operation,
            target,
            snapshot,
            ciphertext,
            password,
            condition,
            Some(directory),
        )
    }
    fn prepare_inner(
        &mut self,
        operation: &str,
        target: &str,
        snapshot: &str,
        ciphertext: &[u8],
        password: &[u8],
        condition: PublishCondition,
        directory: Option<&str>,
    ) -> Result<PreparedPublication, OutboxError> {
        if ciphertext.is_empty() || ciphertext.len() > 20 * 1024 * 1024 {
            return Err(OutboxError::Invalid);
        }
        // Authenticate the envelope before persisting, so arbitrary plaintext
        // supplied accidentally by a caller cannot be stored in the outbox.
        coshell_backup::decode_optional(ciphertext, password).map_err(|_| OutboxError::Invalid)?;
        let protected = if password.is_empty() {
            Some(self.protect(operation, ciphertext)?)
        } else {
            None
        };
        let local_key = self.local_key.as_deref();
        let attempt = match directory {
            Some(d) => PublicationAttempt::new_timestamped(
                operation,
                target,
                snapshot,
                ciphertext,
                condition.clone(),
                d,
            ),
            None => {
                PublicationAttempt::new(operation, target, snapshot, ciphertext, condition.clone())
            }
        }
        .map_err(|_| OutboxError::Invalid)?;
        let target = attempt.target().to_owned();
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| OutboxError::Storage)?;
        let existing:Option<(String,String,Vec<u8>,String,String)> = tx.query_row("SELECT target,snapshot_id,ciphertext,condition_json,attempt_json FROM publication_outbox WHERE operation_id=?1",[operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional().map_err(|_|OutboxError::Storage)?;
        let condition_json = serde_json::to_string(&condition).map_err(|_| OutboxError::Storage)?;
        let attempt_json = if let Some((
            old_target,
            old_snapshot,
            old_ciphertext,
            old_condition,
            old_attempt,
        )) = existing
        {
            if old_target != target
                || old_snapshot != snapshot
                || Self::unprotect_with(local_key, operation, old_ciphertext)? != ciphertext
                || old_condition != condition_json
            {
                return Err(OutboxError::Conflict);
            }
            old_attempt
        } else {
            let value = serde_json::to_string(&attempt).map_err(|_| OutboxError::Storage)?;
            tx.execute("INSERT INTO publication_outbox(operation_id,target,snapshot_id,ciphertext,condition_json,attempt_json) VALUES (?1,?2,?3,?4,?5,?6)",params![operation,target,snapshot,protected.as_deref().unwrap_or(ciphertext),condition_json,value]).map_err(|_|OutboxError::Storage)?;
            value
        };
        tx.commit().map_err(|_| OutboxError::Storage)?;
        Ok(PreparedPublication {
            target,
            snapshot_id: snapshot.into(),
            ciphertext: ciphertext.into(),
            condition,
            attempt: serde_json::from_str(&attempt_json).map_err(|_| OutboxError::Storage)?,
        })
    }
    pub fn status(&self, operation: &str) -> Result<Option<PublicationStatus>, OutboxError> {
        let row:Option<(String,Option<String>,bool,String)> = self.db.query_row("SELECT state,error_code,cleanup_pending,possible_objects FROM publication_outbox WHERE operation_id=?1",[operation],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional().map_err(|_|OutboxError::Storage)?;
        row.map(|(state, error, cleanup_pending, objects)| {
            Ok(PublicationStatus {
                state,
                error,
                cleanup_pending,
                possible_objects: serde_json::from_str(&objects)
                    .map_err(|_| OutboxError::Storage)?,
            })
        })
        .transpose()
    }
    /// Atomically claim once. A running record after interruption is deliberately
    /// unknown; callers must not issue a second remote write automatically.
    pub fn claim(&mut self, operation: &str) -> Result<bool, OutboxError> {
        let tx = self
            .db
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(|_| OutboxError::Storage)?;
        let state: Option<String> = tx
            .query_row(
                "SELECT state FROM publication_outbox WHERE operation_id=?1",
                [operation],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| OutboxError::Storage)?;
        match state.as_deref() {
            Some("prepared") => {
                let busy:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM publication_outbox WHERE target=(SELECT target FROM publication_outbox WHERE operation_id=?1) AND operation_id<>?1 AND state IN ('running','unknown'))",[operation],|r|r.get(0)).map_err(|_|OutboxError::Storage)?;
                if busy {
                    return Err(OutboxError::TargetBusy);
                }
                let attempt_json: String = tx
                    .query_row(
                        "SELECT attempt_json FROM publication_outbox WHERE operation_id=?1",
                        [operation],
                        |r| r.get(0),
                    )
                    .map_err(|_| OutboxError::Storage)?;
                let attempt: PublicationAttempt =
                    serde_json::from_str(&attempt_json).map_err(|_| OutboxError::Storage)?;
                let objects = serde_json::to_string(&attempt.possible_objects())
                    .map_err(|_| OutboxError::Storage)?;
                tx.execute("UPDATE publication_outbox SET state='running',cleanup_pending=1,possible_objects=?2 WHERE operation_id=?1",params![operation,objects]).map_err(|_|OutboxError::Storage)?;
                tx.commit().map_err(|_| OutboxError::Storage)?;
                Ok(true)
            }
            Some("succeeded") => Ok(false),
            Some("running" | "unknown") => Err(OutboxError::OutcomeUnknown),
            _ => Err(OutboxError::Invalid),
        }
    }
    pub fn finish(&mut self, operation: &str, success: bool) -> Result<(), OutboxError> {
        let changed = self
            .db
            .execute(
                "UPDATE publication_outbox SET state=?1 WHERE operation_id=?2 AND state='running'",
                params![if success { "succeeded" } else { "unknown" }, operation],
            )
            .map_err(|_| OutboxError::Storage)?;
        if changed != 1 {
            return Err(OutboxError::Conflict);
        }
        Ok(())
    }
    pub async fn publish_once(
        &mut self,
        operation: &str,
        client: &coshell_backup::webdav::WebDav,
        cancel: &coshell_backup::webdav::Cancellation,
    ) -> Result<(), OutboxError> {
        let prepared = self.load(operation)?.ok_or(OutboxError::Invalid)?;
        if !self.claim(operation)? {
            return Ok(());
        }
        let result = client
            .publish_tracked(
                &prepared.snapshot_id,
                &prepared.ciphertext,
                prepared.condition.clone(),
                &prepared.attempt,
                cancel,
            )
            .await;
        match result {
            Ok(success) => {
                let objects = serde_json::to_string(&success.possible_objects)
                    .map_err(|_| OutboxError::Storage)?;
                let changed=self.db.execute("UPDATE publication_outbox SET state='succeeded',cleanup_pending=?2,possible_objects=?3 WHERE operation_id=?1 AND state='running'",params![operation,success.cleanup_pending,objects]).map_err(|_|OutboxError::Storage)?;
                if changed != 1 {
                    return Err(OutboxError::Conflict);
                }
                Ok(())
            }
            Err(failure) => {
                use coshell_backup::webdav::WebDavError as E;
                let state = if matches!(
                    failure.error,
                    E::OutcomeUnknown | E::Timeout | E::Transport | E::Cancelled | E::Server
                ) {
                    "unknown"
                } else {
                    "failed"
                };
                let objects = serde_json::to_string(&failure.possible_objects)
                    .map_err(|_| OutboxError::Storage)?;
                let changed=self.db.execute("UPDATE publication_outbox SET state=?2,error_code=?3,cleanup_pending=?4,possible_objects=?5 WHERE operation_id=?1 AND state='running'",params![operation,state,format!("{:?}",failure.error),failure.cleanup_pending,objects]).map_err(|_|OutboxError::Storage)?;
                if changed != 1 {
                    return Err(OutboxError::Conflict);
                }
                if state == "unknown" {
                    Err(OutboxError::OutcomeUnknown)
                } else {
                    Err(OutboxError::Remote(failure.error))
                }
            }
        }
    }
}

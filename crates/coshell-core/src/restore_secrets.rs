//! Crash-recoverable restore of platform secrets; SQLite never stores values.
use crate::secret_compensation::SecretOperationLock;
use coshell_platform::secret_store::{Secret, SecretStore};
use coshell_storage::{LocalSnapshot, Repository, SecretReference};
use rusqlite::{params, TransactionBehavior};
use std::path::Path;

pub struct IncomingSecret<'a> {
    pub connection_id: &'a str,
    pub secret_type: &'a str,
    pub value: &'a [u8],
}
#[derive(Debug, PartialEq, Eq)]
pub enum RestoreError {
    Invalid,
    Busy,
    Storage,
    SecretStore,
    RecoveryPending,
    OutcomeUnknown,
}
fn random_id() -> Result<String, RestoreError> {
    let mut bytes = [0; 32];
    getrandom::getrandom(&mut bytes).map_err(|_| RestoreError::Storage)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn context(
    path: &Path,
    store: &dyn SecretStore,
) -> Result<(SecretOperationLock, rusqlite::Connection), RestoreError> {
    let dir = path.parent().ok_or(RestoreError::Invalid)?;
    let lock = SecretOperationLock::try_acquire(dir).map_err(|_| RestoreError::Busy)?;
    if !store.accepts_data_dir(dir) {
        return Err(RestoreError::Invalid);
    }
    let mut db = rusqlite::Connection::open(path).map_err(|_| RestoreError::Storage)?;
    coshell_storage::migrate(&mut db).map_err(|_| RestoreError::Storage)?;
    Ok((lock, db))
}
pub(crate) fn recover_locked(
    db: &mut rusqlite::Connection,
    store: &dyn SecretStore,
) -> Result<(), RestoreError> {
    let entries = db.prepare("SELECT batch_id,connection_id,secret_type,secret_id,kind FROM restore_secret_journal ORDER BY batch_id")
        .map_err(|_|RestoreError::Storage)?.query_map([], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?)))
        .map_err(|_|RestoreError::Storage)?.collect::<Result<Vec<_>,_>>().map_err(|_|RestoreError::Storage)?;
    for (batch, connection, kind, id, phase) in entries {
        let referenced:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_secret_journal WHERE secret_id=?1)",[&id],|r|r.get(0)).map_err(|_|RestoreError::Storage)?;
        if referenced && phase == "staged" {
            return Err(RestoreError::RecoveryPending);
        }
        if !referenced {
            store
                .delete(&id)
                .map_err(|_| RestoreError::RecoveryPending)?;
        }
        db.execute("DELETE FROM restore_secret_journal WHERE batch_id=?1 AND connection_id=?2 AND secret_type=?3 AND secret_id=?4 AND kind=?5",params![batch,connection,kind,id,phase]).map_err(|_|RestoreError::Storage)?;
    }
    db.execute("UPDATE restore_secret_receipts SET state='aborted' WHERE state='staged' AND NOT EXISTS(SELECT 1 FROM restore_secret_journal WHERE batch_id=restore_secret_receipts.batch_id)",[]).map_err(|_|RestoreError::Storage)?;
    Ok(())
}
pub fn recover(path: &Path, store: &dyn SecretStore) -> Result<(), RestoreError> {
    let (_lock, mut db) = context(path, store)?;
    recover_locked(&mut db, store)
}

/// Hold the desktop admission lock around this call. Any failed write leaves
/// staged IDs recoverable; after commit, cleanup entries retain old IDs.
pub fn apply(
    path: &Path,
    store: &dyn SecretStore,
    batch: &str,
    expected: &LocalSnapshot,
    desired: &LocalSnapshot,
    secrets: &[IncomingSecret<'_>],
) -> Result<(), RestoreError> {
    if secrets.is_empty() || secrets.len() > 1024 {
        return Err(RestoreError::Invalid);
    }
    let (_lock, mut db) = context(path, store)?;
    recover_locked(&mut db, store)?;
    if batch.is_empty()
        || batch.len() > 128
        || !batch
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(RestoreError::Invalid);
    }
    let mut staged = Vec::new();
    for s in secrets {
        let record = desired
            .connections
            .iter()
            .find(|c| c.id == s.connection_id)
            .ok_or(RestoreError::Invalid)?;
        if !match s.secret_type {
            "ssh_private_key_bundle_v1" => {
                crate::ssh_key_store::validate_restored_bundle(s.value, record)
            }
            "password" => {
                record.authentication == "password"
                    && crate::ssh_password_store::valid_password(s.value)
            }
            _ => false,
        } {
            return Err(RestoreError::Invalid);
        }
        if s.connection_id.is_empty()
            || s.secret_type.is_empty()
            || s.value.is_empty()
            || s.value.len() > 1024 * 1024
            || !desired.connections.iter().any(|c| c.id == s.connection_id)
            || staged.iter().any(|(c, k, _): &(String, String, String)| {
                c == s.connection_id && k == s.secret_type
            })
        {
            return Err(RestoreError::Invalid);
        }
        let id = random_id()?;
        if store
            .get(&id)
            .map_err(|_| RestoreError::SecretStore)?
            .is_some()
        {
            return Err(RestoreError::Invalid);
        }
        staged.push((s.connection_id.to_owned(), s.secret_type.to_owned(), id));
    }
    {
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| RestoreError::Storage)?;
        tx.execute(
            "INSERT INTO restore_secret_receipts(batch_id,state) VALUES(?1,'staged')",
            [batch],
        )
        .map_err(|_| RestoreError::Invalid)?;
        for (c, k, id) in &staged {
            tx.execute("INSERT INTO restore_secret_journal(batch_id,connection_id,secret_type,secret_id,kind) VALUES(?1,?2,?3,?4,'staged')",params![batch,c,k,id]).map_err(|_|RestoreError::Storage)?;
        }
        tx.commit().map_err(|_| RestoreError::OutcomeUnknown)?;
    }
    for (s, (_, _, id)) in secrets.iter().zip(&staged) {
        if store.set(id, &Secret::new(s.value.to_vec())).is_err() {
            return if recover_locked(&mut db, store).is_ok() {
                Err(RestoreError::SecretStore)
            } else {
                Err(RestoreError::RecoveryPending)
            };
        }
    }
    let mut snapshot = desired.clone();
    for (c, k, id) in &staged {
        snapshot
            .secret_refs
            .retain(|r| &r.connection_id != c || &r.secret_type != k);
        snapshot.secret_refs.push(SecretReference {
            connection_id: c.clone(),
            secret_type: k.clone(),
            secret_id: id.clone(),
        });
    }
    snapshot.secret_refs.sort_by(|a, b| {
        (&a.connection_id, &a.secret_type).cmp(&(&b.connection_id, &b.secret_type))
    });
    if Repository::new(&mut db)
        .restore_snapshot_with_staged_secrets(expected, &snapshot, batch)
        .is_err()
    {
        return Err(RestoreError::OutcomeUnknown);
    }
    recover_locked(&mut db, store).map_err(|_| RestoreError::RecoveryPending)
}
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreStatus {
    pub state: String,
    pub cleanup_pending: bool,
}
pub fn status(
    path: &Path,
    store: &dyn SecretStore,
    batch: &str,
) -> Result<Option<RestoreStatus>, RestoreError> {
    use rusqlite::OptionalExtension;
    let (_lock, db) = context(path, store)?;
    db.query_row(
        "SELECT state, EXISTS(SELECT 1 FROM restore_secret_journal WHERE batch_id=?1) FROM restore_secret_receipts WHERE batch_id=?1",
        [batch],
        |r| Ok(RestoreStatus { state: r.get(0)?, cleanup_pending: r.get(1)? }),
    )
    .optional()
    .map_err(|_| RestoreError::Storage)
}

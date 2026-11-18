//! SSH private keys are held only in the platform secret store. SQLite stores
//! opaque references and a recoverable cleanup journal, never key material.
use crate::secret_compensation::{
    SecretCoordinator, SecretOperationLock, SqliteJournal, WebDavSqliteJournal,
};
use coshell_platform::secret_store::{Secret, SecretStore};
use coshell_storage::ConnectionRecord;
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};
use zeroize::Zeroizing;

const KIND: &str = "ssh_private_key_bundle_v1";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    host: String,
    port: u16,
    username: String,
    key: String,
    passphrase: String,
}
pub(crate) fn validate_restored_bundle(value: &[u8], record: &ConnectionRecord) -> bool {
    let bundle: Bundle = match serde_json::from_slice(value) {
        Ok(value) => value,
        Err(_) => return false,
    };
    !bundle.key.is_empty()
        && bundle.key.len() <= 1024 * 1024
        && bundle.passphrase.len() <= 4096
        && bundle.host == record.host
        && bundle.port == record.port
        && bundle.username == record.username
        && record.authentication == "private_key"
}
impl Drop for Bundle {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.key.zeroize();
        self.passphrase.zeroize();
    }
}

pub(crate) fn context<'a>(
    path: &'a Path,
    store: &dyn SecretStore,
) -> Result<(SecretOperationLock, rusqlite::Connection), &'static str> {
    let dir = path.parent().ok_or("凭据存储不可用")?;
    let lock = SecretOperationLock::try_acquire(dir).map_err(|_| "凭据存储正忙，请重试")?;
    if !store.accepts_data_dir(dir) {
        return Err("凭据存储不可用");
    }
    let mut db = rusqlite::Connection::open(path).map_err(|_| "凭据存储不可用")?;
    coshell_storage::migrate(&mut db).map_err(|_| "凭据存储不可用")?;
    Ok((lock, db))
}

/// Recover both credential journals before the desktop accepts operations.
pub fn recover_all(path: &Path, store: &dyn SecretStore) -> Result<(), &'static str> {
    let (lock, db) = context(path, store)?;
    let journal = Arc::new(SqliteJournal::new(db, &lock).map_err(|_| "凭据存储不可用")?);
    SecretCoordinator::new(journal, store, &lock)
        .map_err(|_| "凭据存储不可用")?
        .recover()
        .map_err(|_| "SSH 凭据恢复未完成")?;
    let db = rusqlite::Connection::open(path).map_err(|_| "凭据存储不可用")?;
    let journal = Arc::new(WebDavSqliteJournal::new(db, &lock).map_err(|_| "凭据存储不可用")?);
    SecretCoordinator::new(journal, store, &lock)
        .map_err(|_| "凭据存储不可用")?
        .recover()
        .map_err(|_| "WebDAV 凭据恢复未完成")?;
    let mut db = rusqlite::Connection::open(path).map_err(|_| "凭据存储不可用")?;
    crate::restore_secrets::recover_locked(&mut db, store).map_err(|_| "备份凭据恢复未完成")
}

pub fn save(
    path: &Path,
    store: &dyn SecretStore,
    record: &ConnectionRecord,
    key: &str,
    passphrase: &str,
    operation: &str,
) -> Result<(), &'static str> {
    if record.authentication != "private_key"
        || key.is_empty()
        || key.len() > 1024 * 1024
        || passphrase.len() > 4096
    {
        return Err("私钥或口令无效");
    }
    let (lock, db) = context(path, store)?;
    let journal = Arc::new(SqliteJournal::new(db, &lock).map_err(|_| "凭据存储不可用")?);
    let coordinator =
        SecretCoordinator::new(journal, store, &lock).map_err(|_| "凭据存储不可用")?;
    coordinator.recover().map_err(|_| "凭据恢复未完成")?;
    let bundle = Bundle {
        host: record.host.clone(),
        port: record.port,
        username: record.username.clone(),
        key: key.to_owned(),
        passphrase: passphrase.to_owned(),
    };
    let data = Zeroizing::new(serde_json::to_vec(&bundle).map_err(|_| "凭据保存失败")?);
    let result = coordinator
        .replace_idempotent(operation, &record.id, KIND, &Secret::new(data.to_vec()))
        .map_err(|e| match e {
            crate::secret_compensation::CoordinatorError::CommitUnknown => "凭据保存结果未知",
            crate::secret_compensation::CoordinatorError::CleanupPending => "凭据清理待恢复",
            _ => "凭据保存失败，请检查系统凭据存储",
        })?;
    if result.outcome == crate::secret_compensation::ReplaceOutcome::CommittedCleanupPending {
        return Err("凭据已保存，但旧凭据清理未完成");
    }
    Ok(())
}

/// Status contains no secret values and is tied to the expected connection.
pub fn operation_status(
    path: &Path,
    store: &dyn SecretStore,
    connection: &str,
    operation: &str,
) -> Result<&'static str, &'static str> {
    let (lock, db) = context(path, store)?;
    let journal = Arc::new(SqliteJournal::new(db, &lock).map_err(|_| "凭据存储不可用")?);
    use crate::secret_compensation::Journal;
    let receipt = journal
        .receipt(operation)
        .map_err(|_| "凭据状态不可用")?
        .ok_or("凭据操作不存在")?;
    if receipt.connection_id != connection || receipt.secret_type != KIND {
        return Err("凭据操作不匹配");
    }
    Ok(match receipt.status {
        crate::secret_compensation::OperationStatus::Staged => "pending",
        crate::secret_compensation::OperationStatus::Aborted => "aborted",
        crate::secret_compensation::OperationStatus::Committed(result)
            if result.outcome
                == crate::secret_compensation::ReplaceOutcome::CommittedCleanupPending =>
        {
            "committed_cleanup_pending"
        }
        crate::secret_compensation::OperationStatus::Committed(_) => "committed",
        crate::secret_compensation::OperationStatus::NotFound => return Err("凭据操作不存在"),
    })
}

pub fn exists(
    path: &Path,
    store: &dyn SecretStore,
    record: &ConnectionRecord,
) -> Result<bool, &'static str> {
    let (lock, mut db) = context(path, store)?;
    let journal = Arc::new(
        SqliteJournal::new(
            rusqlite::Connection::open(path).map_err(|_| "凭据存储不可用")?,
            &lock,
        )
        .map_err(|_| "凭据存储不可用")?,
    );
    SecretCoordinator::new(journal, store, &lock)
        .map_err(|_| "凭据存储不可用")?
        .recover()
        .map_err(|_| "凭据恢复未完成")?;
    coshell_storage::Repository::new(&mut db)
        .get_secret_ref(&record.id, KIND)
        .map(|reference| reference.is_some())
        .map_err(|_| "凭据存储不可用")
}

/// Never return these values through a transport response.
pub fn load(
    path: &Path,
    store: &dyn SecretStore,
    record: &ConnectionRecord,
) -> Result<Option<(Secret, Option<Secret>)>, &'static str> {
    let (lock, mut db) = context(path, store)?;
    let journal = Arc::new(
        SqliteJournal::new(
            rusqlite::Connection::open(path).map_err(|_| "凭据存储不可用")?,
            &lock,
        )
        .map_err(|_| "凭据存储不可用")?,
    );
    SecretCoordinator::new(journal, store, &lock)
        .map_err(|_| "凭据存储不可用")?
        .recover()
        .map_err(|_| "凭据恢复未完成")?;
    let reference = coshell_storage::Repository::new(&mut db)
        .get_secret_ref(&record.id, KIND)
        .map_err(|_| "凭据存储不可用")?;
    let Some(reference) = reference else {
        return Ok(None);
    };
    let secret = store
        .get(&reference)
        .map_err(|_| "凭据读取失败")?
        .ok_or("已保存的凭据不可用，请重新保存")?;
    let bundle: Bundle =
        serde_json::from_slice(secret.expose()).map_err(|_| "已保存的凭据无效，请重新保存")?;
    if record.authentication != "private_key"
        || bundle.host != record.host
        || bundle.port != record.port
        || bundle.username != record.username
    {
        return Err("连接目标已改变，请重新保存私钥");
    }
    Ok(Some((
        Secret::new(bundle.key.as_bytes().to_vec()),
        if bundle.passphrase.is_empty() {
            None
        } else {
            Some(Secret::new(bundle.passphrase.as_bytes().to_vec()))
        },
    )))
}

pub fn delete(
    path: &Path,
    store: &dyn SecretStore,
    connection: &str,
    operation: &str,
) -> Result<(), &'static str> {
    let (lock, mut db) = context(path, store)?;
    let journal = Arc::new(
        SqliteJournal::new(
            rusqlite::Connection::open(path).map_err(|_| "凭据存储不可用")?,
            &lock,
        )
        .map_err(|_| "凭据存储不可用")?,
    );
    let coordinator =
        SecretCoordinator::new(journal, store, &lock).map_err(|_| "凭据存储不可用")?;
    coordinator.recover().map_err(|_| "凭据恢复未完成")?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|_| "凭据删除失败")?;
    let reference: Option<String> = tx.query_row("SELECT secret_id FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2", params![connection, KIND], |r| r.get(0)).optional().map_err(|_| "凭据删除失败")?;
    if let Some(id) = reference {
        let shared: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 AND NOT (connection_id=?2 AND secret_type=?3) UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1)", params![id, connection, KIND], |r| r.get(0)).map_err(|_| "凭据删除失败")?;
        if shared {
            return Err("凭据仍被其他配置引用");
        }
        tx.execute(
            "DELETE FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2",
            params![connection, KIND],
        )
        .map_err(|_| "凭据删除失败")?;
        tx.execute("INSERT INTO secret_write_journal(operation_id,connection_id,secret_type,secret_id,kind) VALUES(?1,?2,?3,?4,'cleanup')", params![operation,connection,KIND,id]).map_err(|_| "凭据删除失败")?;
    }
    tx.commit()
        .map_err(|_| "凭据删除结果未知，请查询已保存状态后重试")?;
    coordinator.recover().map_err(|_| "凭据删除待恢复")
}

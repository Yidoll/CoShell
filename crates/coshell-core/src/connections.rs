//! Connection metadata only. Session admission, deletion and secrets remain gated.
use crate::feature_flags::{Feature, FeatureFlags};
use coshell_storage::{ConnectionRecord, GroupRecord, Repository};
use coshell_types::{ErrorEnvelope, OperationOutcome};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use std::sync::Mutex;

pub struct ConnectionService {
    db: Mutex<rusqlite::Connection>,
    flags: FeatureFlags,
}
fn error(code: &str) -> ErrorEnvelope {
    ErrorEnvelope {
        code: code.into(),
        message_key: format!("errors.{code}"),
        safe_detail: None,
        operation_id: None,
        outcome: OperationOutcome::NotStarted,
        retryable: false,
    }
}
fn write_error(err: rusqlite::Error) -> ErrorEnvelope {
    if matches!(err, rusqlite::Error::SqliteFailure(code, _) if code.code == rusqlite::ErrorCode::ConstraintViolation)
        || matches!(err, rusqlite::Error::InvalidQuery)
    {
        error("CONNECTION_CONFLICT")
    } else {
        error("CONNECTION_STORAGE")
    }
}
impl ConnectionService {
    pub fn new(mut db: rusqlite::Connection, flags: FeatureFlags) -> Result<Self, ErrorEnvelope> {
        coshell_storage::migrate(&mut db).map_err(|_| error("CONNECTION_STORAGE"))?;
        Ok(Self {
            db: Mutex::new(db),
            flags,
        })
    }
    fn check(&self) -> Result<(), ErrorEnvelope> {
        self.flags
            .run_if_enabled(Feature::Connections, None, || Ok(()))
    }
    pub fn save(&self, record: &ConnectionRecord) -> Result<(), ErrorEnvelope> {
        self.check()?;
        if record.id.is_empty()
            || record.name.trim().is_empty()
            || record.host.trim().is_empty()
            || record.username.trim().is_empty()
            || record.port == 0
            || !matches!(
                record.authentication.as_str(),
                "password" | "private_key" | "ssh_agent"
            )
        {
            return Err(error("CONNECTION_INVALID"));
        }
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| error("CONNECTION_STORAGE"))?;
        let current: Option<u64> = tx
            .query_row(
                "SELECT version FROM connections WHERE id=?1",
                [&record.id],
                |r| r.get(0),
            )
            .optional()
            .map_err(|_| error("CONNECTION_STORAGE"))?;
        match current {
            None => {
                if record.version != 0 {
                    return Err(error("CONNECTION_CONFLICT"));
                }
                tx.execute("INSERT INTO connections(id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0)",params![record.id,record.name,record.host,record.port,record.username,record.authentication,record.group_id,record.notes,record.production,record.always_confirm]).map_err(write_error)?;
            }
            Some(version) => {
                if version != record.version || version >= 9_007_199_254_740_991 {
                    return Err(error("CONNECTION_CONFLICT"));
                }
                let unchanged: bool = tx.query_row(
                    "SELECT host=?2 AND port=?3 AND username=?4 AND authentication=?5 FROM connections WHERE id=?1",
                    params![record.id,record.host,record.port,record.username,record.authentication],
                    |r| r.get(0),
                ).map_err(write_error)?;
                if !unchanged {
                    crate::ssh_password_store::retire(&tx, &record.id).map_err(write_error)?;
                }
                tx.execute("UPDATE connections SET name=?1,host=?2,port=?3,username=?4,authentication=?5,group_id=?6,notes=?7,production=?8,always_confirm=?9,version=version+1 WHERE id=?10 AND version=?11",params![record.name,record.host,record.port,record.username,record.authentication,record.group_id,record.notes,record.production,record.always_confirm,record.id,record.version]).map_err(write_error)?;
            }
        }
        tx.commit().map_err(|_| error("CONNECTION_STORAGE"))
    }
    pub fn get(&self, id: &str) -> Result<Option<ConnectionRecord>, ErrorEnvelope> {
        self.check()?;
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        Repository::new(&mut db)
            .get_connection(id)
            .map_err(|_| error("CONNECTION_STORAGE"))
    }
    pub fn list(&self) -> Result<Vec<ConnectionRecord>, ErrorEnvelope> {
        self.check()?;
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        Repository::new(&mut db)
            .list_connections()
            .map_err(|_| error("CONNECTION_STORAGE"))
    }
    pub fn delete(&self, id: &str) -> Result<bool, ErrorEnvelope> {
        self.check()?;
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        Repository::new(&mut db).delete_connection(id).map_err(|e| {
            if matches!(e, rusqlite::Error::InvalidQuery) {
                error("CONNECTION_HAS_SECRETS")
            } else {
                write_error(e)
            }
        })
    }
    pub(crate) fn delete_with_password_cleanup(&self, id: &str) -> Result<bool, ErrorEnvelope> {
        self.check()?;
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(write_error)?;
        let has_other_secrets: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE connection_id=?1 AND secret_type<>'password')",
            [id],
            |r| r.get(0),
        ).map_err(write_error)?;
        if has_other_secrets {
            return Err(error("CONNECTION_HAS_SECRETS"));
        }
        crate::ssh_password_store::retire(&tx, id).map_err(write_error)?;
        let removed = tx
            .execute("DELETE FROM connections WHERE id=?1", [id])
            .map_err(write_error)?
            != 0;
        tx.commit().map_err(write_error)?;
        Ok(removed)
    }
    pub fn save_group(&self, group: &GroupRecord) -> Result<(), ErrorEnvelope> {
        self.check()?;
        if group.id.is_empty() || group.name.trim().is_empty() {
            return Err(error("CONNECTION_INVALID"));
        }
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        Repository::new(&mut db)
            .save_group(group)
            .map_err(write_error)
    }
    pub fn list_groups(&self) -> Result<Vec<GroupRecord>, ErrorEnvelope> {
        self.check()?;
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        Repository::new(&mut db)
            .list_groups()
            .map_err(|_| error("CONNECTION_STORAGE"))
    }
    pub fn delete_group(&self, id: &str) -> Result<bool, ErrorEnvelope> {
        self.check()?;
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        Repository::new(&mut db)
            .delete_group(id)
            .map_err(write_error)
    }
    pub fn copy(
        &self,
        source: &str,
        id: &str,
        name: &str,
    ) -> Result<ConnectionRecord, ErrorEnvelope> {
        self.check()?;
        if id.is_empty() || name.trim().is_empty() {
            return Err(error("CONNECTION_INVALID"));
        }
        let mut db = self.db.lock().map_err(|_| error("CONNECTION_STORAGE"))?;
        let tx = db
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| error("CONNECTION_STORAGE"))?;
        let mut record:ConnectionRecord=tx.query_row("SELECT id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version FROM connections WHERE id=?1",[source],|r|Ok(ConnectionRecord{id:r.get(0)?,name:r.get(1)?,host:r.get(2)?,port:r.get(3)?,username:r.get(4)?,authentication:r.get(5)?,group_id:r.get(6)?,notes:r.get(7)?,production:r.get(8)?,always_confirm:r.get(9)?,version:r.get(10)?})).optional().map_err(|_|error("CONNECTION_STORAGE"))?.ok_or_else(||error("CONNECTION_NOT_FOUND"))?;
        record.id = id.into();
        record.name = name.into();
        record.version = 0;
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM connections WHERE id=?1)",
                [id],
                |r| r.get(0),
            )
            .map_err(|_| error("CONNECTION_STORAGE"))?;
        if exists {
            return Err(error("CONNECTION_CONFLICT"));
        }
        tx.execute("INSERT INTO connections(id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,0)",params![record.id,record.name,record.host,record.port,record.username,record.authentication,record.group_id,record.notes,record.production,record.always_confirm]).map_err(write_error)?;
        tx.commit().map_err(|_| error("CONNECTION_STORAGE"))?;
        Ok(record)
    }
}

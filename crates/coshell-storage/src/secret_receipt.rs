//! Durable, idempotent receipts for secret replacement. Caller holds the secret operation lock.
use rusqlite::{params, Connection, OptionalExtension, Result, TransactionBehavior};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptState {
    Staged,
    Committed,
    Aborted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretReceipt {
    pub operation_id: String,
    pub connection_id: String,
    pub secret_type: String,
    pub secret_id: String,
    pub state: ReceiptState,
    pub cleanup_pending: bool,
}

pub struct SecretReceiptRepository<'a> {
    db: &'a mut Connection,
}
impl<'a> SecretReceiptRepository<'a> {
    pub fn new(db: &'a mut Connection) -> Self {
        Self { db }
    }
    pub fn get(&self, op: &str) -> Result<Option<SecretReceipt>> {
        get(self.db, op)
    }

    /// Reusing an operation for the same target returns its original receipt,
    /// never changes its secret ID, and never causes a second secret write.
    pub fn prepare(
        &mut self,
        op: &str,
        connection: &str,
        kind: &str,
        new_id: &str,
    ) -> Result<SecretReceipt> {
        if [op, connection, kind, new_id]
            .iter()
            .any(|s| s.is_empty() || s.contains('\0'))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(existing) = get(&tx, op)? {
            if existing.connection_id != connection || existing.secret_type != kind {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.commit()?;
            return Ok(existing);
        }
        let used: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE secret_id=?1 UNION ALL SELECT 1 FROM secret_operation_receipts WHERE secret_id=?1)",[new_id],|r|r.get(0))?;
        if used {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute("INSERT INTO secret_write_journal(operation_id,connection_id,secret_type,secret_id,kind) VALUES (?1,?2,?3,?4,'staged')",params![op,connection,kind,new_id])?;
        tx.execute("INSERT INTO secret_operation_receipts(operation_id,connection_id,secret_type,secret_id,state,cleanup_pending) VALUES (?1,?2,?3,?4,'staged',1)",params![op,connection,kind,new_id])?;
        let result = get(&tx, op)?.ok_or(rusqlite::Error::InvalidQuery)?;
        tx.commit()?;
        Ok(result)
    }

    /// Receipt, new reference, and old-key cleanup are committed together.
    pub fn commit(&mut self, op: &str) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let receipt = get(&tx, op)?.ok_or(rusqlite::Error::InvalidQuery)?;
        if receipt.state == ReceiptState::Committed {
            tx.commit()?;
            return Ok(());
        }
        if receipt.state != ReceiptState::Staged {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let staged: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM secret_write_journal WHERE operation_id=?1 AND secret_id=?2 AND kind='staged')",params![op,receipt.secret_id],|r|r.get(0))?;
        if !staged {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let old:Option<String>=tx.query_row("SELECT secret_id FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2",params![receipt.connection_id,receipt.secret_type],|r|r.get(0)).optional()?;
        tx.execute(
            "DELETE FROM secret_write_journal WHERE operation_id=?1",
            [op],
        )?;
        tx.execute("INSERT INTO connection_secret_refs(connection_id,secret_type,secret_id) VALUES (?1,?2,?3) ON CONFLICT(connection_id,secret_type) DO UPDATE SET secret_id=excluded.secret_id",params![receipt.connection_id,receipt.secret_type,receipt.secret_id])?;
        if let Some(old) = old {
            tx.execute("INSERT INTO secret_write_journal(operation_id,connection_id,secret_type,secret_id,kind) VALUES (?1,?2,?3,?4,'cleanup')",params![op,receipt.connection_id,receipt.secret_type,old])?;
            tx.execute("UPDATE secret_operation_receipts SET state='committed',cleanup_pending=1 WHERE operation_id=?1",[op])?;
        } else {
            tx.execute("UPDATE secret_operation_receipts SET state='committed',cleanup_pending=0 WHERE operation_id=?1",[op])?;
        }
        tx.commit()
    }

    /// Called only after OS deletion. Rejects ID mismatch or a live reference.
    pub fn finish_cleanup(&mut self, op: &str, id: &str) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry: Option<(String, String)> = tx
            .query_row(
                "SELECT secret_id,kind FROM secret_write_journal WHERE operation_id=?1",
                [op],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((recorded, kind)) = entry {
            if recorded != id {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let referenced:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1)",[id],|r|r.get(0))?;
            if referenced {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.execute(
                "DELETE FROM secret_write_journal WHERE operation_id=?1 AND secret_id=?2",
                params![op, id],
            )?;
            tx.execute("UPDATE secret_operation_receipts SET state=CASE WHEN ?2='staged' THEN 'aborted' ELSE state END,cleanup_pending=0 WHERE operation_id=?1",params![op,kind])?;
        }
        tx.commit()
    }
}

fn get(db: &Connection, op: &str) -> Result<Option<SecretReceipt>> {
    db.query_row("SELECT operation_id,connection_id,secret_type,secret_id,state,cleanup_pending FROM secret_operation_receipts WHERE operation_id=?1",[op],|row| {
        let state:String=row.get(4)?;
        let state=match state.as_str() { "staged"=>ReceiptState::Staged, "committed"=>ReceiptState::Committed, "aborted"=>ReceiptState::Aborted, _=>return Err(rusqlite::Error::InvalidQuery) };
        Ok(SecretReceipt { operation_id:row.get(0)?, connection_id:row.get(1)?, secret_type:row.get(2)?, secret_id:row.get(3)?, state, cleanup_pending:row.get(5)? })
    }).optional()
}

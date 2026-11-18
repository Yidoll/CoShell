//! Caller must hold the data-directory secret-operation lock.
use crate::secret_receipt::{ReceiptState, SecretReceipt};
use rusqlite::{params, Connection, OptionalExtension, Result, TransactionBehavior};

pub struct WebDavSecretReceiptRepository<'a>(pub &'a mut Connection);
impl WebDavSecretReceiptRepository<'_> {
    pub fn get(&self, op: &str) -> Result<Option<SecretReceipt>> {
        get(self.0, op)
    }
    pub fn prepare_new_profile(
        &mut self,
        op: &str,
        profile: &str,
        url: &str,
        directory: &str,
        username: &str,
        auto: bool,
        id: &str,
    ) -> Result<SecretReceipt> {
        if [op, profile, url, directory, id]
            .iter()
            .any(|v| v.is_empty() || v.contains('\0'))
            || username.contains('\0')
            || auto
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE secret_id=?1 OR operation_id=?2 UNION ALL SELECT 1 FROM secret_operation_receipts WHERE secret_id=?1 OR operation_id=?2 UNION ALL SELECT 1 FROM webdav_secret_journal WHERE secret_id=?1 OR operation_id=?2 UNION ALL SELECT 1 FROM webdav_secret_receipts WHERE secret_id=?1 OR operation_id=?2)",params![id,op],|r|r.get(0))?;
        if used {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute("INSERT INTO webdav_profiles(id,url,remote_directory,username,auto_backup) VALUES (?1,?2,?3,?4,?5)",params![profile,url,directory,username,auto])?;
        tx.execute("INSERT INTO webdav_secret_journal(operation_id,profile_id,secret_id,kind) VALUES (?1,?2,?3,'staged')",params![op,profile,id])?;
        tx.execute("INSERT INTO webdav_secret_receipts(operation_id,profile_id,secret_id,state,cleanup_pending) VALUES (?1,?2,?3,'staged',1)",params![op,profile,id])?;
        tx.execute(
            "INSERT INTO webdav_provisional_profiles(operation_id,profile_id) VALUES (?1,?2)",
            params![op, profile],
        )?;
        let receipt = get(&tx, op)?.ok_or(rusqlite::Error::InvalidQuery)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn prepare(
        &mut self,
        op: &str,
        profile: &str,
        kind: &str,
        id: &str,
    ) -> Result<SecretReceipt> {
        if kind != "webdav_password"
            || [op, profile, id]
                .iter()
                .any(|v| v.is_empty() || v.contains('\0'))
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(r) = get(&tx, op)? {
            if r.connection_id != profile {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.commit()?;
            return Ok(r);
        }
        let used: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE secret_id=?1 UNION ALL SELECT 1 FROM secret_operation_receipts WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_secret_journal WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_secret_receipts WHERE secret_id=?1)", [id], |r| r.get(0))?;
        if used {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute("INSERT INTO webdav_secret_journal(operation_id,profile_id,secret_id,kind) VALUES (?1,?2,?3,'staged')",params![op,profile,id])?;
        tx.execute("INSERT INTO webdav_secret_receipts(operation_id,profile_id,secret_id,state,cleanup_pending) VALUES (?1,?2,?3,'staged',1)",params![op,profile,id])?;
        let receipt = get(&tx, op)?.ok_or(rusqlite::Error::InvalidQuery)?;
        tx.commit()?;
        Ok(receipt)
    }
    pub fn commit(&mut self, op: &str) -> Result<()> {
        self.commit_inner(op, None)
    }

    /// Publish validated metadata together with its new credential reference.
    /// The fields live in the caller until commit. Recovery of a staged
    /// attempt aborts it and never publishes metadata or a reference.
    pub fn commit_profile(
        &mut self,
        op: &str,
        profile: &str,
        url: &str,
        directory: &str,
        username: &str,
        auto: bool,
    ) -> Result<()> {
        self.commit_inner(op, Some((profile, url, directory, username, auto)))
    }

    fn commit_inner(
        &mut self,
        op: &str,
        profile: Option<(&str, &str, &str, &str, bool)>,
    ) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let r = get(&tx, op)?.ok_or(rusqlite::Error::InvalidQuery)?;
        if profile.is_some_and(|(id, _, _, _, _)| id != r.connection_id) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        if r.state == ReceiptState::Committed {
            tx.commit()?;
            return Ok(());
        }
        if r.state != ReceiptState::Staged {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let staged: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM webdav_secret_journal WHERE operation_id=?1 AND secret_id=?2 AND kind='staged')",params![op,r.secret_id],|r|r.get(0))?;
        if !staged {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let old: Option<String> = tx.query_row(
            "SELECT credential_secret_id FROM webdav_profiles WHERE id=?1",
            [&r.connection_id],
            |r| r.get(0),
        )?;
        tx.execute(
            "DELETE FROM webdav_secret_journal WHERE operation_id=?1",
            [op],
        )?;
        tx.execute(
            "DELETE FROM webdav_provisional_profiles WHERE operation_id=?1",
            [op],
        )?;
        if let Some((id, url, directory, username, auto)) = profile {
            tx.execute(
                "UPDATE webdav_profiles SET credential_secret_id=?1,url=?2,remote_directory=?3,username=?4,auto_backup=?5 WHERE id=?6",
                params![r.secret_id, url, directory, username, auto, id],
            )?;
        } else {
            tx.execute(
                "UPDATE webdav_profiles SET credential_secret_id=?1 WHERE id=?2",
                params![r.secret_id, r.connection_id],
            )?;
        }
        if let Some(ref old) = old {
            tx.execute("INSERT INTO webdav_secret_journal(operation_id,profile_id,secret_id,kind) VALUES (?1,?2,?3,'cleanup')",params![op,r.connection_id,old])?;
        }
        tx.execute("UPDATE webdav_secret_receipts SET state='committed',cleanup_pending=?2 WHERE operation_id=?1",params![op, old.is_some()])?;
        tx.commit()
    }
    pub fn pending(&self) -> Result<Vec<(String, String, bool)>> {
        let mut q=self.0.prepare("SELECT operation_id,secret_id,kind='staged' FROM webdav_secret_journal ORDER BY operation_id")?;
        let rows = q
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect();
        rows
    }
    pub fn finish(&mut self, op: &str, id: &str) -> Result<()> {
        let tx = self
            .0
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let entry: Option<(String, String)> = tx
            .query_row(
                "SELECT secret_id,kind FROM webdav_secret_journal WHERE operation_id=?1",
                [op],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((recorded, kind)) = entry {
            if recorded != id {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let used:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1)",[id],|r|r.get(0))?;
            if used {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.execute(
                "DELETE FROM webdav_secret_journal WHERE operation_id=?1",
                [op],
            )?;
            if kind == "staged" {
                let profile: Option<String> = tx
                    .query_row(
                        "SELECT profile_id FROM webdav_provisional_profiles WHERE operation_id=?1",
                        [op],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(profile) = profile {
                    let used: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM backup_runs WHERE profile_id=?1)",
                        [&profile],
                        |r| r.get(0),
                    )?;
                    // v5 allowed historical runs for a provisional profile.
                    // Preserve them and retain the marker so the profile remains
                    // unavailable; the receipt reports the retained condition.
                    if !used {
                        tx.execute(
                            "DELETE FROM webdav_provisional_profiles WHERE operation_id=?1",
                            [op],
                        )?;
                        let removed = tx.execute(
                        "DELETE FROM webdav_profiles WHERE id=?1 AND credential_secret_id IS NULL",
                        [profile],
                        )?;
                        if removed != 1 {
                            return Err(rusqlite::Error::InvalidQuery);
                        }
                    }
                }
            }
            tx.execute("UPDATE webdav_secret_receipts SET state=CASE WHEN ?2='staged' THEN 'aborted' ELSE state END,cleanup_pending=0 WHERE operation_id=?1",params![op,kind])?;
        }
        tx.commit()
    }
}
fn get(db: &Connection, op: &str) -> Result<Option<SecretReceipt>> {
    db.query_row("SELECT operation_id,profile_id,secret_id,state,cleanup_pending FROM webdav_secret_receipts WHERE operation_id=?1",[op],|r|{
        let state:String=r.get(3)?;
        let state=match state.as_str(){"staged"=>ReceiptState::Staged,"committed"=>ReceiptState::Committed,"aborted"=>ReceiptState::Aborted,_=>return Err(rusqlite::Error::InvalidQuery)};
        Ok(SecretReceipt {operation_id:r.get(0)?,connection_id:r.get(1)?,secret_type:"webdav_password".into(),secret_id:r.get(2)?,state,cleanup_pending:r.get(4)?})
    }).optional()
}

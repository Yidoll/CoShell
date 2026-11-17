use rusqlite::{params, Connection, OptionalExtension, Result, TransactionBehavior};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupRecord {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRecord {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub authentication: String,
    pub group_id: Option<String>,
    pub notes: String,
    pub production: bool,
    pub always_confirm: bool,
    pub version: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretReference {
    pub connection_id: String,
    pub secret_type: String,
    pub secret_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretCleanupKind {
    Staged,
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSecretCleanup {
    pub operation_id: String,
    pub secret_id: String,
    pub kind: SecretCleanupKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKeyRecord {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryRecord {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub production: bool,
    /// The caller must sanitize the command before persistence.
    pub command: String,
    pub submitted_at: String,
    pub outcome: String,
    pub exit_code: Option<i32>,
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Default, Clone)]
pub struct HistoryFilter {
    pub connection_id: Option<String>,
    pub keyword: Option<String>,
    pub production: Option<bool>,
    pub outcome: Option<String>,
    pub from: Option<String>,
    pub until: Option<String>,
}

/// Local rollback snapshot: secret IDs are references, never secret values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSnapshot {
    pub groups: Vec<GroupRecord>,
    pub connections: Vec<ConnectionRecord>,
    pub secret_refs: Vec<SecretReference>,
    pub host_keys: Vec<HostKeyRecord>,
}

pub struct Repository<'a> {
    db: &'a mut Connection,
}

impl<'a> Repository<'a> {
    /// The connection must have been initialized with `migrate`.
    pub fn new(db: &'a mut Connection) -> Self {
        Self { db }
    }

    /// Caller holds the cross-process secret-operation lock throughout the protocol.
    pub fn prepare_secret_write(
        &mut self,
        operation_id: &str,
        connection_id: &str,
        secret_type: &str,
        new_id: &str,
    ) -> Result<()> {
        if [operation_id, connection_id, secret_type, new_id]
            .iter()
            .any(|v| v.is_empty())
        {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let operation_used: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM secret_operation_receipts WHERE operation_id=?1)",
            [operation_id],
            |row| row.get(0),
        )?;
        if operation_used {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let in_use: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE secret_id=?1 UNION ALL SELECT 1 FROM secret_operation_receipts WHERE secret_id=?1)",
            [new_id], |row| row.get(0),
        )?;
        if in_use {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute("INSERT INTO secret_write_journal(operation_id,connection_id,secret_type,secret_id,kind) VALUES (?1,?2,?3,?4,'staged')",params![operation_id,connection_id,secret_type,new_id])?;
        tx.commit()
    }

    /// Atomically publish a staged reference and record any displaced ID for cleanup.
    pub fn commit_secret_write(&mut self, operation_id: &str) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_receipt: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM secret_operation_receipts WHERE operation_id=?1)",
            [operation_id],
            |row| row.get(0),
        )?;
        if has_receipt {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let (connection_id,secret_type,new_id):(String,String,String)=tx.query_row("SELECT connection_id,secret_type,secret_id FROM secret_write_journal WHERE operation_id=?1 AND kind='staged'",[operation_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        let old_id:Option<String>=tx.query_row("SELECT secret_id FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2",params![connection_id,secret_type],|r|r.get(0)).optional()?;
        if old_id.as_deref() == Some(new_id.as_str()) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        // Remove staged inside the transaction before publishing its reference;
        // rollback restores staged if publishing or cleanup insertion fails.
        tx.execute(
            "DELETE FROM secret_write_journal WHERE operation_id=?1",
            [operation_id],
        )?;
        tx.execute("INSERT INTO connection_secret_refs(connection_id,secret_type,secret_id) VALUES (?1,?2,?3) ON CONFLICT(connection_id,secret_type) DO UPDATE SET secret_id=excluded.secret_id",params![connection_id,secret_type,new_id])?;
        if let Some(old_id) = old_id {
            tx.execute(
                "INSERT INTO secret_write_journal(operation_id,connection_id,secret_type,secret_id,kind) VALUES (?1,?2,?3,?4,'cleanup')",
                params![operation_id, connection_id, secret_type, old_id],
            )?;
        }
        tx.commit()
    }

    pub fn pending_secret_cleanup(&self) -> Result<Vec<PendingSecretCleanup>> {
        let mut query = self.db.prepare(
            "SELECT operation_id,secret_id,kind FROM secret_write_journal ORDER BY operation_id",
        )?;
        let rows = query
            .query_map([], |r| {
                let kind: String = r.get(2)?;
                let kind = match kind.as_str() {
                    "staged" => SecretCleanupKind::Staged,
                    "cleanup" => SecretCleanupKind::Cleanup,
                    _ => return Err(rusqlite::Error::InvalidQuery),
                };
                Ok(PendingSecretCleanup {
                    operation_id: r.get(0)?,
                    secret_id: r.get(1)?,
                    kind,
                })
            })?
            .collect();
        rows
    }

    /// Caller must hold the operation lock through this check and OS deletion.
    pub fn secret_id_is_referenced(&self, secret_id: &str) -> Result<bool> {
        self.db.query_row(
            "SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1)",
            [secret_id], |row| row.get(0),
        )
    }

    /// Finish is idempotent for absent entries and refuses mismatched/referenced IDs.
    pub fn finish_secret_cleanup(&mut self, operation_id: &str, secret_id: &str) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_receipt: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM secret_operation_receipts WHERE operation_id=?1)",
            [operation_id],
            |row| row.get(0),
        )?;
        if has_receipt {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let recorded: Option<String> = tx
            .query_row(
                "SELECT secret_id FROM secret_write_journal WHERE operation_id=?1",
                [operation_id],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(recorded) = recorded {
            if recorded != secret_id {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let referenced: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1)",
                [secret_id], |row| row.get(0),
            )?;
            if referenced {
                return Err(rusqlite::Error::InvalidQuery);
            }
            tx.execute(
                "DELETE FROM secret_write_journal WHERE operation_id=?1 AND secret_id=?2",
                params![operation_id, secret_id],
            )?;
        }
        tx.commit()
    }

    pub fn save_group(&mut self, group: &GroupRecord) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(parent_id) = &group.parent_id {
            let cycle: bool = tx.query_row(
                "WITH RECURSIVE ancestors(id) AS (SELECT ?1 UNION SELECT g.parent_id FROM connection_groups g JOIN ancestors a ON g.id=a.id WHERE g.parent_id IS NOT NULL) SELECT EXISTS(SELECT 1 FROM ancestors WHERE id=?2)",
                params![parent_id, group.id], |row| row.get(0),
            )?;
            if cycle {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        tx.execute("INSERT INTO connection_groups (id,name,parent_id) VALUES (?1,?2,?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,parent_id=excluded.parent_id", params![group.id,group.name,group.parent_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_groups(&self) -> Result<Vec<GroupRecord>> {
        let mut query = self
            .db
            .prepare("SELECT id,name,parent_id FROM connection_groups ORDER BY id")?;
        let rows = query
            .query_map([], |row| {
                Ok(GroupRecord {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    parent_id: row.get(2)?,
                })
            })?
            .collect();
        rows
    }

    pub fn delete_group(&mut self, id: &str) -> Result<bool> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let exhausted:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM connections WHERE group_id=?1 AND version>=9007199254740991)",[id],|row|row.get(0))?;
        if exhausted {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.execute(
            "UPDATE connections SET group_id=NULL,version=version+1 WHERE group_id=?1",
            [id],
        )?;
        let removed = tx.execute("DELETE FROM connection_groups WHERE id=?1", [id])? != 0;
        tx.commit()?;
        Ok(removed)
    }

    pub fn save_connection(&mut self, record: &ConnectionRecord) -> Result<()> {
        save_connection(self.db, record)
    }

    pub fn get_connection(&self, id: &str) -> Result<Option<ConnectionRecord>> {
        self.db.query_row("SELECT id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version FROM connections WHERE id=?1", [id], connection_row).optional()
    }

    pub fn list_connections(&self) -> Result<Vec<ConnectionRecord>> {
        let mut query = self.db.prepare("SELECT id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version FROM connections ORDER BY id")?;
        let rows = query.query_map([], connection_row)?.collect();
        rows
    }

    /// Connections with secret references require coordinated cleanup first.
    pub fn delete_connection(&mut self, id: &str) -> Result<bool> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_secrets: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE connection_id=?1)",
            [id],
            |row| row.get(0),
        )?;
        if has_secrets {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let removed = tx.execute("DELETE FROM connections WHERE id=?1", [id])? != 0;
        tx.commit()?;
        Ok(removed)
    }

    /// Compatibility acknowledgement for an already identical reference only.
    /// Creation and replacement must use the coordinated secret-write journal;
    /// this method neither verifies nor writes a platform credential and rejects
    /// missing or different references without changing storage.
    pub fn set_secret_ref(
        &mut self,
        connection_id: &str,
        secret_type: &str,
        secret_id: &str,
    ) -> Result<()> {
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let previous: Option<String> = tx.query_row("SELECT secret_id FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2", params![connection_id,secret_type], |row| row.get(0)).optional()?;
        // This compatibility entrypoint may only acknowledge an already
        // identical reference. Creation/replacement must use the journal so
        // a crash cannot leave an untracked or unverified OS credential.
        if previous.as_deref() != Some(secret_id) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        tx.commit()
    }

    pub fn get_secret_ref(&self, connection_id: &str, secret_type: &str) -> Result<Option<String>> {
        self.db.query_row("SELECT secret_id FROM connection_secret_refs WHERE connection_id=?1 AND secret_type=?2", params![connection_id,secret_type], |row| row.get(0)).optional()
    }

    pub fn set_setting(&mut self, key: &str, value: &str) -> Result<()> {
        self.db.execute("INSERT INTO app_settings (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key,value])?;
        Ok(())
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>> {
        self.db
            .query_row(
                "SELECT value FROM app_settings WHERE key=?1",
                [key],
                |row| row.get(0),
            )
            .optional()
    }

    pub fn delete_setting(&mut self, key: &str) -> Result<bool> {
        Ok(self
            .db
            .execute("DELETE FROM app_settings WHERE key=?1", [key])?
            != 0)
    }

    pub fn save_history(&mut self, record: &HistoryRecord) -> Result<()> {
        let timestamp = normalize_timestamp(&record.submitted_at)?;
        self.db.execute("INSERT INTO command_history (id,connection_id,connection_name,production,command,submitted_at,outcome,exit_code,duration_ms) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9) ON CONFLICT(id) DO UPDATE SET outcome=excluded.outcome,exit_code=excluded.exit_code,duration_ms=excluded.duration_ms", params![record.id,record.connection_id,record.connection_name,record.production,record.command,timestamp,record.outcome,record.exit_code,record.duration_ms])?;
        Ok(())
    }

    pub fn list_history(
        &self,
        filter: &HistoryFilter,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<HistoryRecord>> {
        let from = filter
            .from
            .as_deref()
            .map(normalize_timestamp)
            .transpose()?;
        let until = filter
            .until
            .as_deref()
            .map(normalize_timestamp)
            .transpose()?;
        let mut query = self.db.prepare("SELECT id,connection_id,connection_name,production,command,submitted_at,outcome,exit_code,duration_ms FROM command_history WHERE (?1 IS NULL OR connection_id=?1) AND (?2 IS NULL OR instr(command,?2)>0) AND (?3 IS NULL OR production=?3) AND (?4 IS NULL OR outcome=?4) AND (?5 IS NULL OR submitted_at>=?5) AND (?6 IS NULL OR submitted_at<=?6) ORDER BY submitted_at DESC,id DESC LIMIT ?7 OFFSET ?8")?;
        let rows = query
            .query_map(
                params![
                    filter.connection_id,
                    filter.keyword,
                    filter.production,
                    filter.outcome,
                    from,
                    until,
                    limit.min(1000),
                    offset
                ],
                |row| {
                    Ok(HistoryRecord {
                        id: row.get(0)?,
                        connection_id: row.get(1)?,
                        connection_name: row.get(2)?,
                        production: row.get(3)?,
                        command: row.get(4)?,
                        submitted_at: row.get(5)?,
                        outcome: row.get(6)?,
                        exit_code: row.get(7)?,
                        duration_ms: row.get(8)?,
                    })
                },
            )?
            .collect();
        rows
    }

    pub fn delete_history(
        &mut self,
        connection_id: Option<&str>,
        from: Option<&str>,
        until: Option<&str>,
    ) -> Result<usize> {
        let from = from.map(normalize_timestamp).transpose()?;
        let until = until.map(normalize_timestamp).transpose()?;
        self.db.execute("DELETE FROM command_history WHERE (?1 IS NULL OR connection_id=?1) AND (?2 IS NULL OR submitted_at>=?2) AND (?3 IS NULL OR submitted_at<=?3)", params![connection_id,from,until])
    }

    /// Apply both limits in a single transaction using normalized instants.
    pub fn prune_history(
        &mut self,
        before: Option<&str>,
        max_entries: Option<u32>,
    ) -> Result<usize> {
        let before = before.map(normalize_timestamp).transpose()?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut count = 0;
        if let Some(before) = before {
            count += tx.execute(
                "DELETE FROM command_history WHERE submitted_at < ?1",
                [before],
            )?;
        }
        if let Some(max) = max_entries {
            count += tx.execute("DELETE FROM command_history WHERE id IN (SELECT id FROM command_history ORDER BY submitted_at DESC,id DESC LIMIT -1 OFFSET ?1)", [max])?;
        }
        tx.commit()?;
        Ok(count)
    }

    pub fn snapshot(&self) -> Result<LocalSnapshot> {
        self.snapshot_for_backup().map(|(snapshot, _)| snapshot)
    }

    /// Read WebDAV credential IDs in the same SQLite snapshot as connection
    /// references so callers can reject any shared credential IDs.
    pub fn snapshot_for_backup(
        &self,
    ) -> Result<(LocalSnapshot, std::collections::HashSet<String>)> {
        let tx = self.db.unchecked_transaction()?;
        let snapshot = {
            let mut group_query =
                tx.prepare("SELECT id,name,parent_id FROM connection_groups ORDER BY id")?;
            let groups = group_query
                .query_map([], |row| {
                    Ok(GroupRecord {
                        id: row.get(0)?,
                        name: row.get(1)?,
                        parent_id: row.get(2)?,
                    })
                })?
                .collect::<Result<_>>()?;
            let mut connection_query = tx.prepare("SELECT id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version FROM connections ORDER BY id")?;
            let connections = connection_query
                .query_map([], connection_row)?
                .collect::<Result<_>>()?;
            let mut refs=tx.prepare("SELECT connection_id,secret_type,secret_id FROM connection_secret_refs ORDER BY connection_id,secret_type")?;
            let secret_refs = refs
                .query_map([], |row| {
                    Ok(SecretReference {
                        connection_id: row.get(0)?,
                        secret_type: row.get(1)?,
                        secret_id: row.get(2)?,
                    })
                })?
                .collect::<Result<_>>()?;
            let mut keys=tx.prepare("SELECT host,port,algorithm,fingerprint FROM trusted_host_keys ORDER BY host,port,algorithm")?;
            let host_keys = keys
                .query_map([], |row| {
                    Ok(HostKeyRecord {
                        host: row.get(0)?,
                        port: row.get(1)?,
                        algorithm: row.get(2)?,
                        fingerprint: row.get(3)?,
                    })
                })?
                .collect::<Result<_>>()?;
            LocalSnapshot {
                groups,
                connections,
                secret_refs,
                host_keys,
            }
        };
        let webdav_ids = {
            let mut query = tx.prepare("SELECT credential_secret_id FROM webdav_profiles WHERE credential_secret_id IS NOT NULL")?;
            let rows = query
                .query_map([], |row| row.get(0))?
                .collect::<Result<_>>()?;
            rows
        };
        tx.commit()?;
        Ok((snapshot, webdav_ids))
    }

    /// Restore metadata atomically. The caller owns secret-store compensation and
    /// must not use snapshots as portable backups of credential values.
    pub fn restore_snapshot(&mut self, snapshot: &LocalSnapshot) -> Result<()> {
        self.restore_snapshot_checked(None, snapshot, None)
    }

    /// Atomically reject a restore if any protected metadata changed since preview.
    pub fn restore_snapshot_if_matches(
        &mut self,
        expected: &LocalSnapshot,
        snapshot: &LocalSnapshot,
    ) -> Result<()> {
        self.restore_snapshot_checked(Some(expected), snapshot, None)
    }

    /// Caller holds the secret-operation lock and has written every staged
    /// secret successfully to the platform store before invoking this method.
    pub fn restore_snapshot_with_staged_secrets(
        &mut self,
        expected: &LocalSnapshot,
        snapshot: &LocalSnapshot,
        batch: &str,
    ) -> Result<()> {
        if batch.is_empty() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        self.restore_snapshot_checked(Some(expected), snapshot, Some(batch))
    }

    fn restore_snapshot_checked(
        &mut self,
        expected: Option<&LocalSnapshot>,
        snapshot: &LocalSnapshot,
        batch: Option<&str>,
    ) -> Result<()> {
        validate_group_tree(&snapshot.groups)?;
        let tx = self
            .db
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        if let Some(expected) = expected {
            let groups = tx
                .prepare("SELECT id,name,parent_id FROM connection_groups ORDER BY id")?
                .query_map([], |r| {
                    Ok(GroupRecord {
                        id: r.get(0)?,
                        name: r.get(1)?,
                        parent_id: r.get(2)?,
                    })
                })?
                .collect::<Result<Vec<_>>>()?;
            let connections = tx.prepare("SELECT id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version FROM connections ORDER BY id")?.query_map([], connection_row)?.collect::<Result<Vec<_>>>()?;
            let secret_refs = tx.prepare("SELECT connection_id,secret_type,secret_id FROM connection_secret_refs ORDER BY connection_id,secret_type")?.query_map([], |r| Ok(SecretReference { connection_id:r.get(0)?, secret_type:r.get(1)?, secret_id:r.get(2)? }))?.collect::<Result<Vec<_>>>()?;
            let host_keys = tx.prepare("SELECT host,port,algorithm,fingerprint FROM trusted_host_keys ORDER BY host,port,algorithm")?.query_map([], |r| Ok(HostKeyRecord { host:r.get(0)?, port:r.get(1)?, algorithm:r.get(2)?, fingerprint:r.get(3)? }))?.collect::<Result<Vec<_>>>()?;
            if &(LocalSnapshot {
                groups,
                connections,
                secret_refs,
                host_keys,
            }) != expected
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let current_refs = {
            let mut query = tx.prepare("SELECT connection_id,secret_type,secret_id FROM connection_secret_refs ORDER BY connection_id,secret_type")?;
            let rows = query
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })?
                .collect::<Result<Vec<_>>>()?;
            rows
        };
        let mut incoming_refs: Vec<_> = snapshot
            .secret_refs
            .iter()
            .map(|r| {
                (
                    r.connection_id.clone(),
                    r.secret_type.clone(),
                    r.secret_id.clone(),
                )
            })
            .collect();
        incoming_refs.sort();
        if let Some(batch_id) = batch {
            let staged = tx.prepare("SELECT connection_id,secret_type,secret_id FROM restore_secret_journal WHERE batch_id=?1 AND kind='staged' ORDER BY connection_id,secret_type,secret_id")?.query_map([batch_id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?)))?.collect::<Result<Vec<_>>>()?;
            let added: Vec<_> = incoming_refs
                .iter()
                .filter(|r| !current_refs.contains(r))
                .cloned()
                .collect();
            if staged.is_empty() || staged != added {
                return Err(rusqlite::Error::InvalidQuery);
            }
            for (connection, kind, id) in &staged {
                let duplicate: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM connection_secret_refs WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_profiles WHERE credential_secret_id=?1 UNION ALL SELECT 1 FROM secret_write_journal WHERE secret_id=?1 UNION ALL SELECT 1 FROM secret_operation_receipts WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_secret_journal WHERE secret_id=?1 UNION ALL SELECT 1 FROM webdav_secret_receipts WHERE secret_id=?1)",[id],|r|r.get(0))?;
                if duplicate
                    || !snapshot.connections.iter().any(|c| &c.id == connection)
                    || kind.is_empty()
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
            }
            for (connection, kind, id) in &current_refs {
                if !incoming_refs.contains(&(connection.clone(), kind.clone(), id.clone())) {
                    if !staged.iter().any(|s| &s.0 == connection && &s.1 == kind) {
                        return Err(rusqlite::Error::InvalidQuery);
                    }
                    tx.execute("INSERT INTO restore_secret_journal(batch_id,connection_id,secret_type,secret_id,kind) VALUES(?1,?2,?3,?4,'cleanup')", params![batch_id,connection,kind,id])?;
                }
            }
        } else if current_refs != incoming_refs {
            return Err(rusqlite::Error::InvalidQuery);
        }
        for (connection_id, secret_type, secret_id) in &current_refs {
            if !incoming_refs.contains(&(
                connection_id.clone(),
                secret_type.clone(),
                secret_id.clone(),
            )) {
                continue;
            }
            let old: (String, u16, String, String) = tx.query_row(
                "SELECT host,port,username,authentication FROM connections WHERE id=?1",
                [connection_id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let new = snapshot
                .connections
                .iter()
                .find(|c| &c.id == connection_id)
                .ok_or(rusqlite::Error::InvalidQuery)?;
            if old
                != (
                    new.host.clone(),
                    new.port,
                    new.username.clone(),
                    new.authentication.clone(),
                )
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        let mut incoming_host_keys = std::collections::HashMap::new();
        for key in &snapshot.host_keys {
            let canonical = crate::host_key::canonical_host(&key.host)?;
            let identity = (canonical.clone(), key.port);
            let value = (key.algorithm.as_str(), key.fingerprint.as_str());
            if incoming_host_keys
                .insert(identity, value)
                .is_some_and(|previous| previous != value)
            {
                return Err(rusqlite::Error::InvalidQuery);
            }
            let mut query = tx.prepare(
                "SELECT host,algorithm,fingerprint FROM trusted_host_keys WHERE port=?1",
            )?;
            let rows = query.query_map([key.port], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?;
            for row in rows {
                let (host, algorithm, fingerprint) = row?;
                if crate::host_key::canonical_host(&host)? == canonical
                    && (algorithm != key.algorithm || fingerprint != key.fingerprint)
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
            }
        }
        tx.execute_batch("PRAGMA defer_foreign_keys=ON; DELETE FROM connection_secret_refs; DELETE FROM connections;")?;
        // RESTRICT is immediate even with deferred foreign keys. Remove leaves
        // first without temporarily creating conflicting root group names.
        loop {
            let remaining: i64 =
                tx.query_row("SELECT COUNT(*) FROM connection_groups", [], |row| {
                    row.get(0)
                })?;
            if remaining == 0 {
                break;
            }
            let removed = tx.execute("DELETE FROM connection_groups WHERE NOT EXISTS (SELECT 1 FROM connection_groups child WHERE child.parent_id=connection_groups.id)", [])?;
            if removed == 0 {
                return Err(rusqlite::Error::InvalidQuery);
            }
        }
        for group in &snapshot.groups {
            tx.execute(
                "INSERT INTO connection_groups (id,name,parent_id) VALUES (?1,?2,?3)",
                params![group.id, group.name, group.parent_id],
            )?;
        }
        for record in &snapshot.connections {
            save_connection(&tx, record)?;
        }
        if let Some(batch_id) = batch {
            tx.execute(
                "DELETE FROM restore_secret_journal WHERE batch_id=?1 AND kind='staged'",
                [batch_id],
            )?;
        }
        for reference in &snapshot.secret_refs {
            tx.execute("INSERT INTO connection_secret_refs (connection_id,secret_type,secret_id) VALUES (?1,?2,?3)",params![reference.connection_id,reference.secret_type,reference.secret_id])?;
        }
        if let Some(batch_id) = batch {
            if tx.execute("UPDATE restore_secret_receipts SET state='committed' WHERE batch_id=?1 AND state='staged'", [batch_id])? != 1 { return Err(rusqlite::Error::InvalidQuery); }
        }
        for key in &snapshot.host_keys {
            let canonical = crate::host_key::canonical_host(&key.host)?;
            let mut query = tx.prepare("SELECT host FROM trusted_host_keys WHERE port=?1 AND algorithm=?2 AND fingerprint=?3")?;
            let hosts = query
                .query_map(params![key.port, key.algorithm, key.fingerprint], |row| {
                    row.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>>>()?;
            drop(query);
            let mut exists = false;
            for host in hosts {
                if crate::host_key::canonical_host(&host)? == canonical {
                    exists = true;
                }
            }
            if !exists {
                tx.execute("INSERT INTO trusted_host_keys (host,port,algorithm,fingerprint) VALUES (?1,?2,?3,?4)",params![canonical,key.port,key.algorithm,key.fingerprint])?;
            }
        }
        tx.commit()
    }
}

fn normalize_timestamp(value: &str) -> Result<String> {
    let fraction = value
        .split_once('.')
        .map(|(_, tail)| tail.chars().take_while(char::is_ascii_digit).count())
        .unwrap_or(0);
    if fraction > 9 {
        return Err(rusqlite::Error::InvalidParameterName(
            "timestamp precision exceeds nanoseconds".into(),
        ));
    }
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|_| rusqlite::Error::InvalidParameterName("invalid RFC3339 timestamp".into()))?;
    Ok(parsed
        .with_timezone(&chrono::Utc)
        .to_rfc3339_opts(chrono::SecondsFormat::Nanos, true))
}

pub(crate) fn normalize_existing_history(db: &Connection) -> Result<()> {
    let mut query = db.prepare("SELECT id,submitted_at FROM command_history")?;
    let values = query
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>>>()?;
    drop(query);
    for (id, value) in values {
        let normalized = normalize_timestamp(&value)?;
        if normalized != value {
            db.execute(
                "UPDATE command_history SET submitted_at=?1 WHERE id=?2",
                params![normalized, id],
            )?;
        }
    }
    Ok(())
}

fn validate_group_tree(groups: &[GroupRecord]) -> Result<()> {
    use std::collections::{HashMap, HashSet};
    let parents: HashMap<&str, Option<&str>> = groups
        .iter()
        .map(|g| (g.id.as_str(), g.parent_id.as_deref()))
        .collect();
    if parents.len() != groups.len() {
        return Err(rusqlite::Error::InvalidQuery);
    }
    for group in groups {
        let mut seen = HashSet::new();
        let mut node = Some(group.id.as_str());
        while let Some(id) = node {
            if !seen.insert(id) {
                return Err(rusqlite::Error::InvalidQuery);
            }
            node = *parents.get(id).ok_or(rusqlite::Error::InvalidQuery)?;
        }
    }
    Ok(())
}

fn connection_row(row: &rusqlite::Row<'_>) -> Result<ConnectionRecord> {
    Ok(ConnectionRecord {
        id: row.get(0)?,
        name: row.get(1)?,
        host: row.get(2)?,
        port: row.get(3)?,
        username: row.get(4)?,
        authentication: row.get(5)?,
        group_id: row.get(6)?,
        notes: row.get(7)?,
        production: row.get(8)?,
        always_confirm: row.get(9)?,
        version: row.get(10)?,
    })
}

fn save_connection(db: &Connection, record: &ConnectionRecord) -> Result<()> {
    db.execute("INSERT INTO connections (id,name,host,port,username,authentication,group_id,notes,production,always_confirm,version) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(id) DO UPDATE SET name=excluded.name,host=excluded.host,port=excluded.port,username=excluded.username,authentication=excluded.authentication,group_id=excluded.group_id,notes=excluded.notes,production=excluded.production,always_confirm=excluded.always_confirm,version=excluded.version", params![record.id,record.name,record.host,record.port,record.username,record.authentication,record.group_id,record.notes,record.production,record.always_confirm,record.version])?;
    Ok(())
}

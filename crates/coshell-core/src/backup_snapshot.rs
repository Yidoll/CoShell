//! Build connection snapshots. Empty password explicitly selects plaintext.
use crate::feature_flags::{Feature, FeatureFlags};
use crate::secret_compensation::SecretOperationLock;
use coshell_platform::secret_store::{Secret, SecretStore};
use coshell_storage::{ConnectionRecord, GroupRecord, HostKeyRecord, Repository};
use serde::Serialize;
use std::path::Path;
use zeroize::Zeroizing;

#[derive(Default)]
pub struct BackupOptions {
    pub include_secrets: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SnapshotError {
    Unsupported,
    InvalidInput,
    Storage,
    SecretUnavailable,
    Encryption,
}

#[derive(Serialize)]
struct Group<'a> {
    id: &'a str,
    name: &'a str,
    parent_id: &'a Option<String>,
}
impl<'a> From<&'a GroupRecord> for Group<'a> {
    fn from(g: &'a GroupRecord) -> Self {
        Self {
            id: &g.id,
            name: &g.name,
            parent_id: &g.parent_id,
        }
    }
}
#[derive(Serialize)]
struct Connection<'a> {
    id: &'a str,
    name: &'a str,
    host: &'a str,
    port: u16,
    username: &'a str,
    authentication: &'a str,
    group_id: &'a Option<String>,
    notes: &'a str,
    production: bool,
    always_confirm: bool,
    version: u64,
}
impl<'a> From<&'a ConnectionRecord> for Connection<'a> {
    fn from(c: &'a ConnectionRecord) -> Self {
        Self {
            id: &c.id,
            name: &c.name,
            host: &c.host,
            port: c.port,
            username: &c.username,
            authentication: &c.authentication,
            group_id: &c.group_id,
            notes: &c.notes,
            production: c.production,
            always_confirm: c.always_confirm,
            version: c.version,
        }
    }
}
#[derive(Serialize)]
struct HostKey<'a> {
    host: &'a str,
    port: u16,
    algorithm: &'a str,
    fingerprint: &'a str,
}
impl<'a> From<&'a HostKeyRecord> for HostKey<'a> {
    fn from(k: &'a HostKeyRecord) -> Self {
        Self {
            host: &k.host,
            port: k.port,
            algorithm: &k.algorithm,
            fingerprint: &k.fingerprint,
        }
    }
}
#[derive(Serialize)]
struct StoredSecret<'a> {
    connection_id: &'a str,
    secret_type: &'a str,
    value: &'a [u8],
}
#[derive(Serialize)]
struct Payload<'a> {
    version: u32,
    groups: Vec<Group<'a>>,
    connections: Vec<Connection<'a>>,
    host_keys: Vec<HostKey<'a>>,
    secrets: Vec<StoredSecret<'a>>,
}

/// Acquires the credential-operation lock before reading both the database and
/// credential store. Only the resulting authenticated ciphertext is returned.
pub fn create_backup(
    path: &Path,
    store: &impl SecretStore,
    password: &[u8],
    options: BackupOptions,
    flags: &FeatureFlags,
) -> Result<Vec<u8>, SnapshotError> {
    if !flags.is_enabled(Feature::WebDavBackup) {
        return Err(SnapshotError::Unsupported);
    }
    let path = path.canonicalize().map_err(|_| SnapshotError::Storage)?;
    let dir = path.parent().ok_or(SnapshotError::InvalidInput)?;
    if !store.accepts_data_dir(dir) {
        return Err(SnapshotError::InvalidInput);
    }
    let _lock = SecretOperationLock::acquire(dir).map_err(|_| SnapshotError::Storage)?;
    let mut db =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| SnapshotError::Storage)?;
    let version: u32 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .map_err(|_| SnapshotError::Storage)?;
    if version != coshell_storage::CURRENT_SCHEMA_VERSION {
        return Err(SnapshotError::Storage);
    }
    let (snapshot, webdav_ids) = Repository::new(&mut db)
        .snapshot_for_backup()
        .map_err(|_| SnapshotError::Storage)?;
    let mut values: Vec<Secret> = Vec::new();
    if options.include_secrets {
        for reference in &snapshot.secret_refs {
            if webdav_ids.contains(&reference.secret_id) {
                return Err(SnapshotError::InvalidInput);
            }
            values.push(
                store
                    .get(&reference.secret_id)
                    .map_err(|_| SnapshotError::SecretUnavailable)?
                    .ok_or(SnapshotError::SecretUnavailable)?,
            );
        }
    }
    let secrets = if options.include_secrets {
        snapshot
            .secret_refs
            .iter()
            .zip(&values)
            .map(|(r, s)| StoredSecret {
                connection_id: &r.connection_id,
                secret_type: &r.secret_type,
                value: s.expose(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let payload = Payload {
        version: 1,
        groups: snapshot.groups.iter().map(Group::from).collect(),
        connections: snapshot.connections.iter().map(Connection::from).collect(),
        host_keys: snapshot.host_keys.iter().map(HostKey::from).collect(),
        secrets,
    };
    let clear = Zeroizing::new(serde_json::to_vec(&payload).map_err(|_| SnapshotError::Storage)?);
    coshell_backup::encode_optional(&clear, password).map_err(|_| SnapshotError::Encryption)
}

/// Build a self-contained snapshot directory using the same repository/secret
/// isolation as the legacy builder. The temporary legacy package stays only in
/// zeroized memory; callers must protect local persistence of plaintext mode.
pub fn create_directory_snapshot(
    path: &Path,
    store: &impl SecretStore,
    password: &[u8],
    options: BackupOptions,
    flags: &FeatureFlags,
    metadata: coshell_backup::snapshot_manifest::SnapshotMetadata<'_>,
) -> Result<coshell_backup::snapshot_manifest::SnapshotArtifact, SnapshotError> {
    let encoded = Zeroizing::new(create_backup(path, store, password, options, flags)?);
    coshell_backup::snapshot_manifest::build_from_legacy(&encoded, password, metadata)
        .map_err(|_| SnapshotError::Encryption)
}

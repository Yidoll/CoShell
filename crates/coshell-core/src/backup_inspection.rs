//! Authenticated backup inspection; this module never mutates local state.
use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InspectionError {
    Authentication,
    InvalidPayload,
    SecretsUnsupported,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Payload<T> {
    version: u32,
    groups: Vec<Group>,
    connections: Vec<Connection>,
    host_keys: Vec<HostKey>,
    secrets: Vec<T>,
}
#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
}
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupDifference {
    pub id: String,
    pub backup: Group,
    pub local: Option<Group>,
    pub status: &'static str,
}
pub fn compare_groups(
    incoming: &Inspection,
    current: &[coshell_storage::GroupRecord],
) -> Vec<GroupDifference> {
    incoming
        .groups
        .iter()
        .map(|g| {
            let found = current.iter().find(|v| v.id == g.id);
            let local = found.map(|v| Group {
                id: v.id.clone(),
                name: v.name.clone(),
                parent_id: v.parent_id.clone(),
            });
            let status = match &local {
                None => "new",
                Some(v) if v == g => "unchanged",
                Some(_) => "conflict",
            };
            GroupDifference {
                id: g.id.clone(),
                backup: g.clone(),
                local,
                status,
            }
        })
        .collect()
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq, Clone)]
#[serde(deny_unknown_fields)]
pub struct Connection {
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
#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct HostKey {
    host: String,
    port: u16,
    algorithm: String,
    fingerprint: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Inspection {
    pub connections: Vec<Connection>,
    pub group_count: usize,
    pub host_key_count: usize,
    pub groups: Vec<Group>,
    #[serde(skip)]
    host_keys: Vec<HostKey>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionDifference {
    pub id: String,
    pub name: String,
    pub status: &'static str,
    pub backup: Connection,
    pub local: Option<Connection>,
    pub backup_has_secret: bool,
    pub local_has_secret: bool,
}

/// Compare by stable identifier; version differences also count as a conflict.
pub fn compare_connections(
    incoming: &Inspection,
    current: &[coshell_storage::ConnectionRecord],
) -> Vec<ConnectionDifference> {
    let local: std::collections::HashMap<_, _> =
        current.iter().map(|c| (c.id.as_str(), c)).collect();
    incoming
        .connections
        .iter()
        .map(|c| {
            let status = match local.get(c.id.as_str()) {
                None => "new",
                Some(v)
                    if v.name == c.name
                        && v.host == c.host
                        && v.port == c.port
                        && v.username == c.username
                        && v.authentication == c.authentication
                        && v.group_id == c.group_id
                        && v.notes == c.notes
                        && v.production == c.production
                        && v.always_confirm == c.always_confirm
                        && v.version == c.version =>
                {
                    "unchanged"
                }
                Some(_) => "conflict",
            };
            ConnectionDifference {
                id: c.id.clone(),
                name: c.name.clone(),
                status,
                backup: c.clone(),
                local: local.get(c.id.as_str()).map(|v| Connection {
                    id: v.id.clone(),
                    name: v.name.clone(),
                    host: v.host.clone(),
                    port: v.port,
                    username: v.username.clone(),
                    authentication: v.authentication.clone(),
                    group_id: v.group_id.clone(),
                    notes: v.notes.clone(),
                    production: v.production,
                    always_confirm: v.always_confirm,
                    version: v.version,
                }),
                backup_has_secret: false,
                local_has_secret: false,
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub enum RestoreChoice {
    Keep,
    Replace,
    Copy { id: String, name: String },
}

/// Construct a desired snapshot without changing storage. The caller must
/// keep session admission locked and use `restore_snapshot_if_matches`.
pub fn prepare_restore(
    incoming: &Inspection,
    current: &coshell_storage::LocalSnapshot,
    choices: &std::collections::HashMap<String, RestoreChoice>,
) -> Result<coshell_storage::LocalSnapshot, InspectionError> {
    prepare_restore_with_groups(incoming, current, choices, false)
}

/// `confirm_new_groups` is permitted only after displaying the complete group list.
pub fn prepare_restore_with_groups(
    incoming: &Inspection,
    current: &coshell_storage::LocalSnapshot,
    choices: &std::collections::HashMap<String, RestoreChoice>,
    confirm_new_groups: bool,
) -> Result<coshell_storage::LocalSnapshot, InspectionError> {
    use coshell_storage::{ConnectionRecord, GroupRecord};
    if choices.len() != incoming.connections.len()
        || incoming
            .connections
            .iter()
            .any(|c| !choices.contains_key(&c.id))
    {
        return Err(InspectionError::InvalidPayload);
    }
    let mut desired = current.clone();
    for group in &incoming.groups {
        if desired
            .groups
            .iter()
            .any(|g| g.id == group.id && (g.name != group.name || g.parent_id != group.parent_id))
        {
            return Err(InspectionError::InvalidPayload);
        }
        if !desired.groups.iter().any(|g| g.id == group.id) {
            if !confirm_new_groups {
                return Err(InspectionError::InvalidPayload);
            }
            desired.groups.push(GroupRecord {
                id: group.id.clone(),
                name: group.name.clone(),
                parent_id: group.parent_id.clone(),
            });
        }
    }
    for c in &incoming.connections {
        let choice = choices.get(&c.id).ok_or(InspectionError::InvalidPayload)?;
        if matches!(choice, RestoreChoice::Keep) {
            continue;
        }
        let (id, name) = match choice {
            RestoreChoice::Copy { id, name } => {
                if id.is_empty()
                    || name.trim().is_empty()
                    || desired.connections.iter().any(|v| v.id == *id)
                {
                    return Err(InspectionError::InvalidPayload);
                }
                (id.clone(), name.clone())
            }
            _ => (c.id.clone(), c.name.clone()),
        };
        let existing = desired.connections.iter().position(|v| v.id == id);
        let version = if let Some(index) = existing {
            let old = &desired.connections[index];
            if current.secret_refs.iter().any(|r| r.connection_id == id)
                && (old.host != c.host
                    || old.port != c.port
                    || old.username != c.username
                    || old.authentication != c.authentication)
            {
                return Err(InspectionError::SecretsUnsupported);
            }
            old.version
                .checked_add(1)
                .filter(|v| *v <= 9_007_199_254_740_991)
                .ok_or(InspectionError::InvalidPayload)?
        } else {
            0
        };
        let record = ConnectionRecord {
            id,
            name,
            host: c.host.clone(),
            port: c.port,
            username: c.username.clone(),
            authentication: c.authentication.clone(),
            group_id: c.group_id.clone(),
            notes: c.notes.clone(),
            production: c.production,
            always_confirm: c.always_confirm,
            version,
        };
        if let Some(index) = existing {
            desired.connections[index] = record;
        } else {
            desired.connections.push(record);
        }
    }
    // Backup fingerprints require a separate visible trust decision. Until that
    // flow exists, retain only keys already trusted on this device.
    Ok(desired)
}

pub fn inspect(encrypted: &[u8], password: &[u8]) -> Result<Inspection, InspectionError> {
    let clear = coshell_backup::decode_optional(encrypted, password)
        .map_err(|_| InspectionError::Authentication)?;
    let payload: Payload<serde::de::IgnoredAny> =
        serde_json::from_slice(clear.as_slice()).map_err(|_| InspectionError::InvalidPayload)?;
    if payload.version != 1 {
        return Err(InspectionError::InvalidPayload);
    }
    if !payload.secrets.is_empty() {
        return Err(InspectionError::SecretsUnsupported);
    }
    inspection_from_payload(payload)
}

fn inspection_from_payload<T>(payload: Payload<T>) -> Result<Inspection, InspectionError> {
    if payload.version != 1 {
        return Err(InspectionError::InvalidPayload);
    }
    let mut ids = std::collections::HashSet::new();
    if payload.connections.iter().any(|c| !ids.insert(&c.id)) {
        return Err(InspectionError::InvalidPayload);
    }
    if payload.groups.iter().any(|g| {
        g.id.is_empty() || g.name.is_empty() || g.parent_id.as_ref().is_some_and(|p| p == &g.id)
    }) || payload.connections.iter().any(|c| {
        c.id.is_empty()
            || c.name.is_empty()
            || c.host.is_empty()
            || c.username.is_empty()
            || c.port == 0
            || !matches!(
                c.authentication.as_str(),
                "password" | "private_key" | "ssh_agent"
            )
    }) || payload.host_keys.iter().any(|h| {
        h.host.is_empty() || h.port == 0 || h.algorithm.is_empty() || h.fingerprint.is_empty()
    }) {
        return Err(InspectionError::InvalidPayload);
    }
    Ok(Inspection {
        connections: payload.connections,
        group_count: payload.groups.len(),
        host_key_count: payload.host_keys.len(),
        groups: payload.groups,
        host_keys: payload.host_keys,
    })
}

/// Secret values are never serialized or shown in diagnostics. The caller must
/// stage them through a durable compensation protocol before changing refs.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreSecret {
    pub connection_id: String,
    pub secret_type: String,
    value: SecretBytes,
}
struct SecretBytes(Vec<u8>);
impl Drop for SecretBytes {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}
impl<'de> Deserialize<'de> for SecretBytes {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = SecretBytes;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a bounded byte array")
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<SecretBytes, A::Error> {
                let mut bytes = SecretBytes(Vec::new());
                while let Some(b) = seq.next_element::<u8>()? {
                    if bytes.0.len() >= 1024 * 1024 {
                        return Err(serde::de::Error::custom("secret too large"));
                    }
                    bytes.0.push(b);
                }
                Ok(bytes)
            }
        }
        deserializer.deserialize_seq(Visitor)
    }
}
impl RestoreSecret {
    pub fn value(&self) -> &[u8] {
        &self.value.0
    }
}
impl std::fmt::Debug for RestoreSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RestoreSecret")
            .field("connection_id", &self.connection_id)
            .field("secret_type", &self.secret_type)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Separates untrusted secret material from the public preview. This does not
/// apply credentials; the existing `inspect` remains fail-closed for callers
/// that have not implemented durable multi-secret recovery.
pub fn inspect_for_restore(
    encoded: &[u8],
    password: &[u8],
) -> Result<(Inspection, Vec<RestoreSecret>), InspectionError> {
    let clear = coshell_backup::decode_optional(encoded, password)
        .map_err(|_| InspectionError::Authentication)?;
    inspect_clear_for_restore(clear.as_slice())
}

/// Validate all independent directory files before exposing a public preview.
/// Package plaintext and credentials never leave core restore validation.
pub fn inspect_directory_for_restore(
    manifest: &[u8],
    complete: &[u8],
    connections: &[u8],
    credentials: Option<&[u8]>,
    password: &[u8],
) -> Result<(Inspection, Vec<RestoreSecret>), InspectionError> {
    let opened = coshell_backup::snapshot_manifest::open_packages(
        manifest,
        complete,
        connections,
        credentials,
        password,
    )
    .map_err(|_| InspectionError::Authentication)?;
    inspect_clear_for_restore(&opened.payload)
}

fn inspect_clear_for_restore(
    clear: &[u8],
) -> Result<(Inspection, Vec<RestoreSecret>), InspectionError> {
    let mut payload: Payload<RestoreSecret> =
        serde_json::from_slice(clear).map_err(|_| InspectionError::InvalidPayload)?;
    let secrets = std::mem::take(&mut payload.secrets);
    let preview = inspection_from_payload(payload)?;
    let mut seen = std::collections::HashSet::new();
    if secrets.iter().any(|s| {
        s.connection_id.is_empty()
            || s.secret_type.is_empty()
            || s.secret_type.len() > 128
            || s.value.0.is_empty()
            || !preview.connections.iter().any(|c| c.id == s.connection_id)
            || !seen.insert((s.connection_id.clone(), s.secret_type.clone()))
    }) {
        return Err(InspectionError::InvalidPayload);
    }
    Ok((preview, secrets))
}

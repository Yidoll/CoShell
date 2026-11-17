//! Independent snapshot directory format. Metadata is public; package buffers
//! can contain secrets and are zeroized on drop. Publication belongs in transport.
use crate::BackupError;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub const MANIFEST_FILE: &str = "manifest.json";
pub const CONNECTIONS_PACKAGE: &str = "connections.pkg";
pub const CREDENTIALS_PACKAGE: &str = "credentials.pkg";
pub const COMPLETE_MARKER: &str = "complete.json";
pub const FORMAT_VERSION: u32 = 1;
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_PACKAGE_BYTES: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotProtection {
    #[serde(rename = "encrypted")]
    Encrypted,
    #[serde(rename = "none")]
    Plaintext,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestFile {
    pub path: String,
    pub length: u64,
    pub sha256: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SnapshotManifest {
    pub format_version: u32,
    pub snapshot_id: String,
    pub created_at: String,
    pub time_zone: String,
    pub app_name: String,
    pub app_version: String,
    pub scope: String,
    pub protection: SnapshotProtection,
    pub includes_credentials: bool,
    pub connection_count: u32,
    pub group_count: u32,
    pub files: Vec<ManifestFile>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompleteMarker {
    pub format_version: u32,
    pub snapshot_id: String,
    pub manifest_sha256: String,
    pub completed_at: String,
}
pub struct SnapshotMetadata<'a> {
    pub snapshot_id: &'a str,
    pub created_at: &'a str,
    pub app_version: &'a str,
}
pub struct SnapshotArtifact {
    pub manifest: Vec<u8>,
    pub complete: Vec<u8>,
    pub manifest_sha256: String,
    pub connections_package: Zeroizing<Vec<u8>>,
    pub credentials_package: Option<Zeroizing<Vec<u8>>>,
}
impl SnapshotArtifact {
    pub fn manifest_text(&self) -> &str {
        std::str::from_utf8(&self.manifest).expect("JSON is UTF-8")
    }
}
impl SnapshotArtifact {
    /// Memory-only framing for a caller-owned encrypted local outbox. The
    /// result can contain plaintext credentials and must never be persisted
    /// unless protected with a key held in the platform secret store.
    pub fn pack(&self) -> Result<Zeroizing<Vec<u8>>, BackupError> {
        let manifest = parse_manifest(&self.manifest)?;
        verify_complete(&self.manifest, &self.complete)?;
        if manifest.includes_credentials != self.credentials_package.is_some() {
            return Err(BackupError::InvalidEnvelope);
        }
        verify_package(&manifest.files[0], &self.connections_package)?;
        if let Some(credentials) = self.credentials_package.as_ref() {
            verify_package(&manifest.files[1], credentials)?;
        }
        let chunks = [
            self.manifest.as_slice(),
            self.complete.as_slice(),
            self.connections_package.as_slice(),
            self.credentials_package
                .as_ref()
                .map_or(&[][..], |v| v.as_slice()),
        ];
        let mut out = Zeroizing::new(b"coshell-directory-v1\0".to_vec());
        for chunk in chunks {
            let len: u32 = chunk
                .len()
                .try_into()
                .map_err(|_| BackupError::PayloadTooLarge)?;
            out.extend_from_slice(&len.to_be_bytes());
            out.extend_from_slice(chunk);
        }
        Ok(out)
    }
    pub fn unpack(bytes: &[u8]) -> Result<Self, BackupError> {
        const PREFIX: &[u8] = b"coshell-directory-v1\0";
        if !bytes.starts_with(PREFIX) {
            return Err(BackupError::InvalidEnvelope);
        }
        let mut offset = PREFIX.len();
        let mut chunks = Vec::with_capacity(4);
        for limit in [
            MAX_MANIFEST_BYTES,
            MAX_MANIFEST_BYTES,
            MAX_PACKAGE_BYTES,
            MAX_PACKAGE_BYTES,
        ] {
            let end = offset.checked_add(4).ok_or(BackupError::InvalidEnvelope)?;
            let len_bytes: [u8; 4] = bytes
                .get(offset..end)
                .ok_or(BackupError::InvalidEnvelope)?
                .try_into()
                .map_err(|_| BackupError::InvalidEnvelope)?;
            offset = end;
            let len = u32::from_be_bytes(len_bytes) as usize;
            if len > limit {
                return Err(BackupError::PayloadTooLarge);
            }
            let end = offset
                .checked_add(len)
                .ok_or(BackupError::InvalidEnvelope)?;
            chunks.push(bytes.get(offset..end).ok_or(BackupError::InvalidEnvelope)?);
            offset = end;
        }
        if offset != bytes.len() {
            return Err(BackupError::InvalidEnvelope);
        }
        let m = parse_manifest(chunks[0])?;
        verify_complete(chunks[0], chunks[1])?;
        verify_package(&m.files[0], chunks[2])?;
        if m.includes_credentials {
            verify_package(&m.files[1], chunks[3])?;
        } else if !chunks[3].is_empty() {
            return Err(BackupError::InvalidEnvelope);
        }
        Ok(Self {
            manifest: chunks[0].to_vec(),
            complete: chunks[1].to_vec(),
            manifest_sha256: sha256(chunks[0]),
            connections_package: Zeroizing::new(chunks[2].to_vec()),
            credentials_package: if m.includes_credentials {
                Some(Zeroizing::new(chunks[3].to_vec()))
            } else {
                None
            },
        })
    }
}
pub struct OpenedSnapshot {
    pub manifest: SnapshotManifest,
    pub payload: Zeroizing<Vec<u8>>,
}
impl std::fmt::Debug for SnapshotArtifact {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SnapshotArtifact([PACKAGES REDACTED])")
    }
}
impl std::fmt::Debug for OpenedSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenedSnapshot")
            .field("manifest", &self.manifest)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}
fn text(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn utc_time(value: &str) -> bool {
    if value.len() != 20 || !value.is_ascii() {
        return false;
    }
    let b = value.as_bytes();
    if b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
        || !(0..19)
            .filter(|i| !matches!(i, 4 | 7 | 10 | 13 | 16))
            .all(|i| b[i].is_ascii_digit())
    {
        return false;
    }
    let n = |a: usize, z: usize| value[a..z].parse::<u32>().unwrap_or(9999);
    let (year, month, day) = (n(0, 4), n(5, 7), n(8, 10));
    let max_day = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    year > 0 && day > 0 && day <= max_day && n(11, 13) < 24 && n(14, 16) < 60 && n(17, 19) < 60
}
fn valid_id(value: &str) -> bool {
    text(value, 128)
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}
fn file(path: &str, bytes: &[u8]) -> ManifestFile {
    ManifestFile {
        path: path.into(),
        length: bytes.len() as u64,
        sha256: sha256(bytes),
    }
}

/// Metadata construction from already-protected packages. Raw encrypted package
/// identity is checked by `open_packages`, never inferred from this function.
pub fn build_manifest(
    snapshot_id: &str,
    created_at: &str,
    app_version: &str,
    protection: SnapshotProtection,
    connections_package: &[u8],
    credentials_package: Option<&[u8]>,
    connection_count: usize,
    group_count: usize,
) -> Result<SnapshotArtifact, BackupError> {
    if !valid_id(snapshot_id)
        || !utc_time(created_at)
        || !text(app_version, 64)
        || connections_package.is_empty()
        || connections_package.len() > MAX_PACKAGE_BYTES
        || credentials_package.is_some_and(|p| p.is_empty() || p.len() > MAX_PACKAGE_BYTES)
        || connection_count > 100_000
        || group_count > 100_000
    {
        return Err(BackupError::InvalidInput);
    }
    let mut files = vec![file(CONNECTIONS_PACKAGE, connections_package)];
    if let Some(bytes) = credentials_package {
        files.push(file(CREDENTIALS_PACKAGE, bytes));
    }
    let value = SnapshotManifest {
        format_version: FORMAT_VERSION,
        snapshot_id: snapshot_id.into(),
        created_at: created_at.into(),
        time_zone: "UTC".into(),
        app_name: "CoShell".into(),
        app_version: app_version.into(),
        scope: "connections".into(),
        protection,
        includes_credentials: credentials_package.is_some(),
        connection_count: connection_count as u32,
        group_count: group_count as u32,
        files,
    };
    let manifest = serde_json::to_vec(&value).map_err(|_| BackupError::InvalidEnvelope)?;
    let manifest_sha256 = sha256(&manifest);
    let complete = serde_json::to_vec(&CompleteMarker {
        format_version: FORMAT_VERSION,
        snapshot_id: snapshot_id.into(),
        manifest_sha256: manifest_sha256.clone(),
        completed_at: created_at.into(),
    })
    .map_err(|_| BackupError::InvalidEnvelope)?;
    Ok(SnapshotArtifact {
        manifest,
        complete,
        manifest_sha256,
        connections_package: Zeroizing::new(connections_package.to_vec()),
        credentials_package: credentials_package.map(|p| Zeroizing::new(p.to_vec())),
    })
}
pub fn parse_manifest(bytes: &[u8]) -> Result<SnapshotManifest, BackupError> {
    if bytes.is_empty() || bytes.len() > MAX_MANIFEST_BYTES {
        return Err(BackupError::InvalidEnvelope);
    }
    let m: SnapshotManifest =
        serde_json::from_slice(bytes).map_err(|_| BackupError::InvalidEnvelope)?;
    if m.format_version != FORMAT_VERSION {
        return Err(BackupError::UnsupportedVersion);
    }
    if !valid_id(&m.snapshot_id)
        || !utc_time(&m.created_at)
        || m.time_zone != "UTC"
        || m.app_name != "CoShell"
        || !text(&m.app_version, 64)
        || m.scope != "connections"
        || m.connection_count > 100_000
        || m.group_count > 100_000
        || m.files.len() != if m.includes_credentials { 2 } else { 1 }
    {
        return Err(BackupError::InvalidEnvelope);
    }
    if m.files
        .first()
        .is_none_or(|p| p.path != CONNECTIONS_PACKAGE)
        || m.includes_credentials && m.files.get(1).is_none_or(|p| p.path != CREDENTIALS_PACKAGE)
        || m.files.iter().any(|p| {
            p.length == 0 || p.length > MAX_PACKAGE_BYTES as u64 || !valid_digest(&p.sha256)
        })
    {
        return Err(BackupError::InvalidEnvelope);
    }
    Ok(m)
}
pub fn verify_complete(
    manifest_bytes: &[u8],
    complete_bytes: &[u8],
) -> Result<CompleteMarker, BackupError> {
    let m = parse_manifest(manifest_bytes)?;
    if complete_bytes.is_empty() || complete_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(BackupError::InvalidEnvelope);
    }
    let c: CompleteMarker =
        serde_json::from_slice(complete_bytes).map_err(|_| BackupError::InvalidEnvelope)?;
    if c.format_version != FORMAT_VERSION
        || c.snapshot_id != m.snapshot_id
        || c.manifest_sha256 != sha256(manifest_bytes)
        || !utc_time(&c.completed_at)
    {
        return Err(BackupError::Authentication);
    }
    Ok(c)
}
pub fn verify_package(file: &ManifestFile, bytes: &[u8]) -> Result<(), BackupError> {
    if bytes.len() as u64 != file.length || sha256(bytes) != file.sha256 {
        Err(BackupError::Authentication)
    } else {
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyParts<'a> {
    version: u32,
    #[serde(borrow)]
    groups: &'a RawValue,
    #[serde(borrow)]
    connections: &'a RawValue,
    #[serde(borrow)]
    host_keys: &'a RawValue,
    #[serde(borrow)]
    secrets: &'a RawValue,
}
#[derive(Serialize)]
struct ConnectionBody<'a> {
    version: u32,
    groups: &'a RawValue,
    connections: &'a RawValue,
    host_keys: &'a RawValue,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Package<'a> {
    format_version: u32,
    snapshot_id: String,
    created_at: String,
    app_version: String,
    protection: SnapshotProtection,
    role: String,
    connection_count: u32,
    group_count: u32,
    includes_credentials: bool,
    #[serde(borrow)]
    payload: &'a RawValue,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenConnectionBody<'a> {
    version: u32,
    #[serde(borrow)]
    groups: &'a RawValue,
    #[serde(borrow)]
    connections: &'a RawValue,
    #[serde(borrow)]
    host_keys: &'a RawValue,
}
#[derive(Serialize)]
struct Recombined<'a> {
    version: u32,
    groups: &'a RawValue,
    connections: &'a RawValue,
    host_keys: &'a RawValue,
    secrets: &'a RawValue,
}
fn array_count(raw: &RawValue) -> Result<usize, BackupError> {
    serde_json::from_str::<Vec<serde::de::IgnoredAny>>(raw.get())
        .map(|v| v.len())
        .map_err(|_| BackupError::InvalidEnvelope)
}
fn protected_package(
    m: &SnapshotManifest,
    role: &str,
    payload: &RawValue,
    password: &[u8],
) -> Result<Zeroizing<Vec<u8>>, BackupError> {
    let clear = Zeroizing::new(
        serde_json::to_vec(&Package {
            format_version: FORMAT_VERSION,
            snapshot_id: m.snapshot_id.clone(),
            created_at: m.created_at.clone(),
            app_version: m.app_version.clone(),
            protection: m.protection,
            role: role.into(),
            connection_count: m.connection_count,
            group_count: m.group_count,
            includes_credentials: m.includes_credentials,
            payload,
        })
        .map_err(|_| BackupError::InvalidEnvelope)?,
    );
    crate::encode_optional(&clear, password).map(Zeroizing::new)
}
/// Split an existing validated legacy payload without constructing secret Values.
/// Both packages bind the same snapshot identity and public metadata inside AEAD.
pub fn build_from_legacy(
    encoded: &[u8],
    password: &[u8],
    metadata: SnapshotMetadata<'_>,
) -> Result<SnapshotArtifact, BackupError> {
    let clear = crate::decode_optional(encoded, password)?;
    let p: LegacyParts =
        serde_json::from_slice(clear.as_slice()).map_err(|_| BackupError::InvalidEnvelope)?;
    if p.version != 1 {
        return Err(BackupError::UnsupportedVersion);
    }
    let connection_count = array_count(p.connections)?;
    let group_count = array_count(p.groups)?;
    array_count(p.host_keys)?;
    let secret_count = array_count(p.secrets)?;
    let protection = if password.is_empty() {
        SnapshotProtection::Plaintext
    } else {
        SnapshotProtection::Encrypted
    };
    // This temporary metadata contains no file hashes, avoiding a circular AEAD
    // dependency; the completed manifest binds the actual protected package bytes.
    let public = SnapshotManifest {
        format_version: FORMAT_VERSION,
        snapshot_id: metadata.snapshot_id.into(),
        created_at: metadata.created_at.into(),
        time_zone: "UTC".into(),
        app_name: "CoShell".into(),
        app_version: metadata.app_version.into(),
        scope: "connections".into(),
        protection,
        includes_credentials: secret_count > 0,
        connection_count: connection_count
            .try_into()
            .map_err(|_| BackupError::InvalidInput)?,
        group_count: group_count
            .try_into()
            .map_err(|_| BackupError::InvalidInput)?,
        files: vec![],
    };
    let body = Zeroizing::new(
        serde_json::to_vec(&ConnectionBody {
            version: 1,
            groups: p.groups,
            connections: p.connections,
            host_keys: p.host_keys,
        })
        .map_err(|_| BackupError::InvalidEnvelope)?,
    );
    let raw = RawValue::from_string(
        std::str::from_utf8(&body)
            .map_err(|_| BackupError::InvalidEnvelope)?
            .to_owned(),
    )
    .map_err(|_| BackupError::InvalidEnvelope)?;
    let connections = protected_package(&public, "connections", &raw, password)?;
    let credentials = if secret_count > 0 {
        Some(protected_package(
            &public,
            "credentials",
            p.secrets,
            password,
        )?)
    } else {
        None
    };
    build_manifest(
        metadata.snapshot_id,
        metadata.created_at,
        metadata.app_version,
        protection,
        &connections,
        credentials.as_deref().map(|p| p.as_slice()),
        connection_count,
        group_count,
    )
}
fn check_identity(p: &Package<'_>, m: &SnapshotManifest, role: &str) -> Result<(), BackupError> {
    if p.format_version != FORMAT_VERSION
        || p.snapshot_id != m.snapshot_id
        || p.created_at != m.created_at
        || p.app_version != m.app_version
        || p.protection != m.protection
        || p.role != role
        || p.connection_count != m.connection_count
        || p.group_count != m.group_count
        || p.includes_credentials != m.includes_credentials
    {
        return Err(BackupError::Authentication);
    }
    Ok(())
}
/// Fully verify an independent directory and return its legacy-compatible clear
/// payload for existing core restore validation. This does not mutate local data.
pub fn open_packages(
    manifest_bytes: &[u8],
    complete_bytes: &[u8],
    connections_package: &[u8],
    credentials_package: Option<&[u8]>,
    password: &[u8],
) -> Result<OpenedSnapshot, BackupError> {
    let m = parse_manifest(manifest_bytes)?;
    verify_complete(manifest_bytes, complete_bytes)?;
    if m.includes_credentials != credentials_package.is_some() {
        return Err(BackupError::InvalidEnvelope);
    }
    if (password.is_empty()) != (m.protection == SnapshotProtection::Plaintext) {
        return Err(BackupError::InvalidPassword);
    }
    verify_package(&m.files[0], connections_package)?;
    let clear = crate::decode_optional(connections_package, password)?;
    let package: Package =
        serde_json::from_slice(clear.as_slice()).map_err(|_| BackupError::InvalidEnvelope)?;
    check_identity(&package, &m, "connections")?;
    let body: OpenConnectionBody =
        serde_json::from_str(package.payload.get()).map_err(|_| BackupError::InvalidEnvelope)?;
    if body.version != 1
        || array_count(body.connections)? != m.connection_count as usize
        || array_count(body.groups)? != m.group_count as usize
    {
        return Err(BackupError::InvalidEnvelope);
    }
    array_count(body.host_keys)?;
    let credentials_clear = credentials_package
        .map(|bytes| {
            verify_package(&m.files[1], bytes)?;
            crate::decode_optional(bytes, password)
        })
        .transpose()?;
    let empty = RawValue::from_string("[]".into()).map_err(|_| BackupError::InvalidEnvelope)?;
    let secrets = if let Some(bytes) = credentials_clear.as_ref() {
        let package: Package =
            serde_json::from_slice(bytes.as_slice()).map_err(|_| BackupError::InvalidEnvelope)?;
        check_identity(&package, &m, "credentials")?;
        if array_count(package.payload)? == 0 {
            return Err(BackupError::InvalidEnvelope);
        }
        package.payload
    } else {
        &empty
    };
    let payload = Zeroizing::new(
        serde_json::to_vec(&Recombined {
            version: 1,
            groups: body.groups,
            connections: body.connections,
            host_keys: body.host_keys,
            secrets,
        })
        .map_err(|_| BackupError::InvalidEnvelope)?,
    );
    Ok(OpenedSnapshot {
        manifest: m,
        payload,
    })
}

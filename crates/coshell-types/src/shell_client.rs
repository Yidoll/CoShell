//! Shared wire contracts for connection, terminal, safety and backup workflows.
//!
//! Public response types contain no passwords, private keys, backup passphrases,
//! WebDAV credentials or secret-store references. A secret-bearing request must
//! be handled separately and must never be used as a response.

use serde::{Deserialize, Serialize};
use std::fmt;

// JSON numbers are consumed by JavaScript clients. Reject integers outside its
// exact integer range at both ends instead of silently changing identifiers.
mod js_safe_u64 {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};
    pub(super) const MAX: u64 = 9_007_199_254_740_991;

    pub fn serialize<S: Serializer>(value: &u64, serializer: S) -> Result<S::Ok, S::Error> {
        if *value > MAX {
            return Err(serde::ser::Error::custom(
                "integer exceeds JavaScript safe range",
            ));
        }
        serializer.serialize_u64(*value)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if value > MAX {
            return Err(D::Error::custom("integer exceeds JavaScript safe range"));
        }
        Ok(value)
    }
}

mod optional_js_safe_u64 {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &Option<u64>, serializer: S) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) if *value > super::js_safe_u64::MAX => Err(serde::ser::Error::custom(
                "integer exceeds JavaScript safe range",
            )),
            Some(value) => serializer.serialize_some(value),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<u64>, D::Error> {
        let value = Option::<u64>::deserialize(deserializer)?;
        if value.is_some_and(|value| value > super::js_safe_u64::MAX) {
            return Err(D::Error::custom("integer exceeds JavaScript safe range"));
        }
        Ok(value)
    }
}

mod js_safe_usize {
    use serde::{de::Error as _, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(value: &usize, serializer: S) -> Result<S::Ok, S::Error> {
        let value = u64::try_from(*value).map_err(serde::ser::Error::custom)?;
        super::js_safe_u64::serialize(&value, serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<usize, D::Error> {
        let value = u64::deserialize(deserializer)?;
        if value > super::js_safe_u64::MAX {
            return Err(D::Error::custom("integer exceeds JavaScript safe range"));
        }
        usize::try_from(value).map_err(D::Error::custom)
    }
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthenticationMethod {
    Password,
    PrivateKey,
    SshAgent,
}

/// A connection safe to return to callers. Credentials are never part of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionProfile {
    pub id: String,
    pub name: String,
    pub host: String,
    #[serde(default = "default_ssh_port")]
    pub port: u16,
    pub username: String,
    pub authentication: AuthenticationMethod,
    #[serde(default)]
    pub group_id: Option<String>,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub production: bool,
    #[serde(default)]
    pub always_confirm: bool,
    #[serde(default, with = "js_safe_u64")]
    pub version: u64,
}

/// Secret-bearing input is intentionally not serializable and has redacted Debug.
#[derive(Deserialize)]
pub struct SecretInput {
    value: String,
}

impl SecretInput {
    pub fn expose(&self) -> &str {
        &self.value
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretInput")
            .field("value", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionTestPhase {
    Dns,
    Tcp,
    SshHandshake,
    HostVerification,
    Authentication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionTestResult {
    pub operation_id: String,
    pub phase: ConnectionTestPhase,
    pub state: OperationState,
    #[serde(default)]
    pub server_identity: Option<String>,
    #[serde(default)]
    pub error: Option<ErrorEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyChallenge {
    pub host: String,
    pub port: u16,
    pub algorithm: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closing,
    CloseUnknown,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub connection_id: String,
    pub state: SessionState,
    pub production: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionEvent {
    pub session_id: String,
    #[serde(with = "js_safe_u64")]
    pub sequence: u64,
    #[serde(flatten)]
    pub payload: SessionEventPayload,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    content = "payload",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum SessionEventPayload {
    Output {
        data: Vec<u8>,
    },
    State {
        state: SessionState,
    },
    Error {
        error: ErrorEnvelope,
    },
    Truncated {
        #[serde(with = "js_safe_u64")]
        discarded_bytes: u64,
    },
}

/// A value with an explicit unit; unavailable metrics have no numeric value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Metric {
    pub value: Option<f64>,
    pub unit: String,
    #[serde(default)]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerOverviewSnapshot {
    pub connection_id: String,
    pub collected_at: String,
    pub online: bool,
    #[serde(default)]
    pub hostname: Option<String>,
    #[serde(default)]
    pub operating_system: Option<String>,
    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default)]
    pub architecture: Option<String>,
    #[serde(default)]
    pub boot_time: Option<String>,
    #[serde(default)]
    pub uptime: Option<Metric>,
    #[serde(default)]
    pub cpu_usage: Option<Metric>,
    #[serde(default)]
    pub logical_cores: Option<Metric>,
    #[serde(default)]
    pub memory_total: Option<Metric>,
    #[serde(default)]
    pub memory_used: Option<Metric>,
    #[serde(default)]
    pub load: Option<Metric>,
    #[serde(default)]
    pub disk_total: Option<Metric>,
    #[serde(default)]
    pub disk_used: Option<Metric>,
    #[serde(default)]
    pub swap_total: Option<Metric>,
    #[serde(default)]
    pub swap_used: Option<Metric>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryOutcome {
    Running,
    Succeeded,
    Failed,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandHistoryEntry {
    pub id: String,
    pub connection_id: String,
    pub connection_name: String,
    pub production: bool,
    /// Must already be redacted before this DTO is created.
    pub command: String,
    pub submitted_at: String,
    pub outcome: HistoryOutcome,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default, with = "optional_js_safe_u64")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    Safe,
    Dangerous,
    Unknown,
}

/// Offsets are UTF-8 byte positions in the submitted command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplaySpan {
    #[serde(with = "js_safe_usize")]
    pub start: usize,
    #[serde(with = "js_safe_usize")]
    pub end: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RiskAssessment {
    pub level: RiskLevel,
    pub categories: Vec<String>,
    pub rule_version: String,
    pub display_spans: Vec<DisplaySpan>,
    pub redacted_digest: String,
}

/// Command text may contain secrets; request DTOs are deliberately not Debug or Serialize.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandIntent {
    pub session_id: String,
    pub connection_id: String,
    #[serde(with = "js_safe_u64")]
    pub connection_version: u64,
    pub command: String,
    pub operation_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationState {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    NotStarted,
    Completed,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    pub code: String,
    pub message_key: String,
    #[serde(default)]
    pub safe_detail: Option<String>,
    #[serde(default)]
    pub operation_id: Option<String>,
    pub outcome: OperationOutcome,
    pub retryable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationStatus {
    pub operation_id: String,
    pub state: OperationState,
    #[serde(default)]
    pub error: Option<ErrorEnvelope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupScope {
    #[serde(default)]
    pub include_credentials: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupInfo {
    pub id: String,
    pub format_version: u32,
    pub created_at: String,
    pub includes_credentials: bool,
}

/// Configuration returned to clients excludes the stored WebDAV credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebDavProfile {
    pub id: String,
    pub url: String,
    pub remote_directory: String,
    pub username: String,
    #[serde(default)]
    pub auto_backup: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreAction {
    KeepLocal,
    UseBackup,
    CreateCopy,
    Skip,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreItem {
    pub id: String,
    pub action: RestoreAction,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePlan {
    pub backup_id: String,
    pub items: Vec<RestoreItem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreDisposition {
    New,
    Update,
    Conflict,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreviewItem {
    pub id: String,
    pub name: String,
    pub disposition: RestoreDisposition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RestorePreview {
    pub backup_id: String,
    pub includes_credentials: bool,
    pub connections: Vec<RestorePreviewItem>,
    pub groups: Vec<RestorePreviewItem>,
}

//! Emit representative production DTOs for the frontend cross-language contract check.
use coshell_types::{
    AuthenticationMethod, BackupInfo, BackupScope, CommandHistoryEntry, ConnectionGroup,
    ConnectionProfile, ConnectionTestPhase, ConnectionTestResult, DisplaySpan, ErrorEnvelope,
    HistoryOutcome, HostKeyChallenge, Metric, OperationOutcome, OperationState, OperationStatus,
    RestoreAction, RestoreDisposition, RestoreItem, RestorePlan, RestorePreview,
    RestorePreviewItem, RiskAssessment, RiskLevel, ServerOverviewSnapshot, SessionEvent,
    SessionEventPayload, SessionInfo, SessionState, WebDavProfile,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let error = ErrorEnvelope {
        code: "CONFIRMATION_REQUIRED".into(),
        message_key: "guard.confirm".into(),
        safe_detail: None,
        operation_id: Some("op1".into()),
        outcome: OperationOutcome::NotStarted,
        retryable: false,
    };
    let metric = Metric {
        value: Some(0.0),
        unit: "%".into(),
        unavailable_reason: None,
    };
    let preview_item = RestorePreviewItem {
        id: "c1".into(),
        name: "server".into(),
        disposition: RestoreDisposition::Conflict,
    };
    let output = json!({
        "connection": ConnectionProfile { id: "c1".into(), name: "server".into(), host: "example.org".into(), port: 22, username: "user".into(), authentication: AuthenticationMethod::Password, group_id: None, notes: "".into(), production: true, always_confirm: false, version: 1 },
        "group": ConnectionGroup { id: "g1".into(), name: "group".into(), parent_id: None },
        "connectionTest": ConnectionTestResult { operation_id: "op1".into(), phase: ConnectionTestPhase::HostVerification, state: OperationState::Failed, server_identity: None, error: Some(error.clone()) },
        "hostKey": HostKeyChallenge { host: "example.org".into(), port: 22, algorithm: "ed25519".into(), fingerprint: "SHA256:test".into() },
        "session": SessionInfo { session_id: "s1".into(), connection_id: "c1".into(), state: SessionState::Connected, production: true },
        "events": [
            SessionEvent { session_id: "s1".into(), sequence: 1, payload: SessionEventPayload::Output { data: vec![65] } },
            SessionEvent { session_id: "s1".into(), sequence: 2, payload: SessionEventPayload::State { state: SessionState::Disconnected } },
            SessionEvent { session_id: "s1".into(), sequence: 3, payload: SessionEventPayload::Error { error: error.clone() } },
            SessionEvent { session_id: "s1".into(), sequence: 4, payload: SessionEventPayload::Truncated { discarded_bytes: 8 } }
        ],
        "metric": metric.clone(),
        "overview": ServerOverviewSnapshot { connection_id: "c1".into(), collected_at: "2026-01-01T00:00:00Z".into(), online: true, hostname: Some("server".into()), operating_system: None, kernel: None, architecture: None, boot_time: None, uptime: None, cpu_usage: Some(metric), logical_cores: None, memory_total: None, memory_used: None, load: None, disk_total: None, disk_used: None, swap_total: None, swap_used: None },
        "history": CommandHistoryEntry { id: "h1".into(), connection_id: "c1".into(), connection_name: "server".into(), production: true, command: "pwd".into(), submitted_at: "2026-01-01T00:00:00Z".into(), outcome: HistoryOutcome::Unknown, exit_code: None, duration_ms: None },
        "risk": RiskAssessment { level: RiskLevel::Dangerous, categories: vec!["recursive_delete".into()], rule_version: "1".into(), display_spans: vec![DisplaySpan { start: 0, end: 2 }], redacted_digest: "sha256:abc".into() },
        "operation": OperationStatus { operation_id: "op1".into(), state: OperationState::Failed, error: Some(error.clone()) },
        "error": error,
        "scope": BackupScope { include_credentials: false },
        "backup": BackupInfo { id: "b1".into(), format_version: 1, created_at: "2026-01-01T00:00:00Z".into(), includes_credentials: false },
        "webdav": WebDavProfile { id: "w1".into(), url: "https://example.org".into(), remote_directory: "/backup".into(), username: "user".into(), auto_backup: false },
        "restorePlan": RestorePlan { backup_id: "b1".into(), items: vec![RestoreItem { id: "c1".into(), action: RestoreAction::CreateCopy }] },
        "restorePreview": RestorePreview { backup_id: "b1".into(), includes_credentials: false, connections: vec![preview_item], groups: vec![] }
    });
    println!("{}", serde_json::to_string(&output)?);
    Ok(())
}

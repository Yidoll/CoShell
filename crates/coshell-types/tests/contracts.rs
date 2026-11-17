use coshell_types::{
    AuthenticationMethod, BackupScope, CommandHistoryEntry, ConnectionProfile, ErrorEnvelope,
    HistoryOutcome, Metric, OperationOutcome, OperationState, RestoreAction, RestoreItem,
    RiskAssessment, RiskLevel, SecretInput, ServerOverviewSnapshot, SessionEvent,
    SessionEventPayload, SessionState,
};
use serde_json::{json, Value};

#[test]
fn connection_response_has_stable_public_fields_and_no_secret() {
    let profile: ConnectionProfile = serde_json::from_value(json!({
        "id":"c1", "name":"server", "host":"example.org", "username":"user",
        "authentication":"password", "password":"do-not-return", "privateKey":"secret"
    }))
    .unwrap();
    assert_eq!(profile.port, 22);
    assert!(!profile.production);
    assert_eq!(profile.authentication, AuthenticationMethod::Password);
    let encoded = serde_json::to_value(&profile).unwrap();
    assert_eq!(encoded["host"], "example.org");
    assert_eq!(encoded["authentication"], "password");
    assert!(encoded.get("password").is_none());
    assert!(encoded.get("privateKey").is_none());
    assert!(!format!("{profile:?}").contains("do-not-return"));
}

#[test]
fn secret_input_debug_is_redacted() {
    let input: SecretInput = serde_json::from_value(json!({"value":"sensitive-password"})).unwrap();
    assert!(!format!("{input:?}").contains("sensitive-password"));
}

#[test]
fn event_wire_contract_is_tagged_and_keeps_sequence() {
    let event = SessionEvent {
        session_id: "s1".into(),
        sequence: 17,
        payload: SessionEventPayload::State {
            state: SessionState::Disconnected,
        },
    };
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(
        value,
        json!({"sessionId":"s1", "sequence":17,
        "kind":"state", "payload":{"state":"disconnected"}})
    );
    assert_eq!(
        serde_json::from_value::<SessionEvent>(value).unwrap(),
        event
    );
}

#[test]
fn wire_integers_reject_values_javascript_cannot_represent_exactly() {
    let too_large = 9_007_199_254_740_992_u64;
    let event = SessionEvent {
        session_id: "s1".into(),
        sequence: too_large,
        payload: SessionEventPayload::State {
            state: SessionState::Connected,
        },
    };
    assert!(serde_json::to_value(event).is_err());
    assert!(serde_json::from_value::<SessionEvent>(json!({
        "sessionId":"s1", "sequence":too_large,
        "kind":"state", "payload":{"state":"connected"}
    }))
    .is_err());
    let profile: ConnectionProfile = serde_json::from_value(json!({
        "id":"c1", "name":"server", "host":"example.org", "username":"user",
        "authentication":"password", "version":9_007_199_254_740_991_u64
    }))
    .unwrap();
    assert_eq!(
        serde_json::to_value(profile).unwrap()["version"],
        9_007_199_254_740_991_u64
    );
}

#[test]
fn missing_metric_is_distinct_from_zero() {
    let available = Metric {
        value: Some(0.0),
        unit: "%".into(),
        unavailable_reason: None,
    };
    let missing = Metric {
        value: None,
        unit: "%".into(),
        unavailable_reason: Some("unsupported".into()),
    };
    assert_eq!(serde_json::to_value(&available).unwrap()["value"], 0.0);
    let value = serde_json::to_value(&missing).unwrap();
    assert_eq!(value["value"], Value::Null);
    assert_eq!(value["unavailableReason"], "unsupported");
}

#[test]
fn forward_compatible_snapshots_accept_added_fields() {
    let overview: ServerOverviewSnapshot = serde_json::from_value(json!({
        "connectionId":"c1", "collectedAt":"2026-01-01T00:00:00Z",
        "online":true, "newServerField":"ignored"
    }))
    .unwrap();
    assert_eq!(overview.connection_id, "c1");
    assert!(overview.cpu_usage.is_none());
    let history: CommandHistoryEntry = serde_json::from_value(json!({
        "id":"h1", "connectionId":"c1", "connectionName":"server",
        "production":true, "command":"pwd", "submittedAt":"2026-01-01T00:00:00Z",
        "outcome":"unknown"
    }))
    .unwrap();
    assert_eq!(history.outcome, HistoryOutcome::Unknown);
    assert!(history.exit_code.is_none());
}

#[test]
fn risk_operation_error_and_restore_use_stable_wire_enums() {
    let risk: RiskAssessment = serde_json::from_value(json!({
        "level":"dangerous", "categories":["recursive_delete"],
        "ruleVersion":"1", "displaySpans":[], "redactedDigest":"sha256:abc"
    }))
    .unwrap();
    assert_eq!(risk.level, RiskLevel::Dangerous);
    assert_eq!(
        serde_json::to_value(OperationState::Running).unwrap(),
        "running"
    );
    let error = ErrorEnvelope {
        code: "CONFIRMATION_REQUIRED".into(),
        message_key: "guard.confirm".into(),
        safe_detail: None,
        operation_id: Some("op1".into()),
        outcome: OperationOutcome::NotStarted,
        retryable: false,
    };
    let value = serde_json::to_value(&error).unwrap();
    assert_eq!(value["operationId"], "op1");
    assert_eq!(value["outcome"], "not_started");
    let item = RestoreItem {
        id: "c1".into(),
        action: RestoreAction::CreateCopy,
    };
    assert_eq!(serde_json::to_value(item).unwrap()["action"], "create_copy");
    let scope = BackupScope {
        include_credentials: false,
    };
    assert_eq!(
        serde_json::to_value(scope).unwrap(),
        json!({"includeCredentials":false})
    );
}

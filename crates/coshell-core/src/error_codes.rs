//! Stable, transport-independent errors produced at the core boundary.

use coshell_types::{ErrorEnvelope, OperationOutcome};

/// The requested feature is disabled or unavailable in this runtime.
pub const UNSUPPORTED: &str = "UNSUPPORTED";

/// Construct an error before any operation has started, without exposing inputs.
pub fn unsupported(operation_id: Option<&str>) -> ErrorEnvelope {
    ErrorEnvelope {
        code: UNSUPPORTED.to_owned(),
        message_key: "errors.unsupported".to_owned(),
        safe_detail: None,
        operation_id: operation_id.map(str::to_owned),
        outcome: OperationOutcome::NotStarted,
        retryable: false,
    }
}

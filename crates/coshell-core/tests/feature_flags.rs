use coshell_core::error_codes;
use coshell_core::feature_flags::{Feature, FeatureFlags};
use coshell_types::OperationOutcome;
use std::cell::Cell;

#[test]
fn every_stage_is_disabled_by_default_and_does_not_run_work() {
    let flags = FeatureFlags::default();
    let calls = Cell::new(0);
    for feature in Feature::ALL {
        let error = flags
            .run_if_enabled(feature, Some("operation-1"), || {
                calls.set(calls.get() + 1);
                Ok::<_, coshell_types::ErrorEnvelope>(())
            })
            .unwrap_err();
        assert_eq!(error.code, error_codes::UNSUPPORTED);
        assert_eq!(error.message_key, "errors.unsupported");
        assert_eq!(error.outcome, OperationOutcome::NotStarted);
        assert_eq!(error.operation_id.as_deref(), Some("operation-1"));
        assert!(!error.retryable);
    }
    assert_eq!(calls.get(), 0);
}

#[test]
fn enabling_one_stage_does_not_enable_others_and_runs_real_operation() {
    let flags = FeatureFlags::default().with_enabled(Feature::Connections);
    let calls = Cell::new(0);
    let result = flags.run_if_enabled(Feature::Connections, None, || {
        calls.set(calls.get() + 1);
        Ok::<_, coshell_types::ErrorEnvelope>(42)
    });
    assert_eq!(result.unwrap(), 42);
    assert_eq!(calls.get(), 1);
    assert!(!flags.is_enabled(Feature::WebDavBackup));
}

#[test]
fn enabled_stage_propagates_the_operation_error() {
    let flags = FeatureFlags::default().with_enabled(Feature::Connections);
    let expected = error_codes::unsupported(None);
    let actual = flags
        .run_if_enabled(Feature::Connections, None, || {
            Err::<(), _>(expected.clone())
        })
        .unwrap_err();
    assert_eq!(actual, expected);
}

use coshell_core::command_intent::{Admission, CommandGate, GateError, LiveConnection};
use coshell_types::CommandIntent;
use std::time::Duration;

fn intent(op: &str, cmd: &str) -> CommandIntent {
    CommandIntent {
        session_id: "session".into(),
        connection_id: "connection".into(),
        connection_version: 1,
        command: cmd.into(),
        operation_id: op.into(),
    }
}
fn live() -> LiveConnection {
    LiveConnection {
        session_id: "session".into(),
        connection_id: "connection".into(),
        version: 1,
        production: true,
        always_confirm: false,
    }
}

#[test]
fn confirmation_is_bound_one_time_and_duplicate_prepare_is_idempotent() {
    let gate = CommandGate::new(Duration::from_secs(10)).unwrap();
    let id = gate.issue_operation().unwrap();
    let input = intent(&id, "rm -rf /tmp/x");
    assert!(matches!(
        gate.prepare_current(&input, &live()).unwrap(),
        Admission::ConfirmationRequired(_)
    ));
    assert!(matches!(
        gate.prepare_current(&input, &live()).unwrap(),
        Admission::ConfirmationRequired(_)
    ));
    let permit = gate.confirm_current(&input, &live()).unwrap();
    assert_eq!(gate.confirm_current(&input, &live()).unwrap(), permit);
    assert_eq!(
        gate.consume_current(&intent(&id, "rm -rf /tmp/y"), &live(), &permit),
        Err(GateError::Conflict)
    );
    gate.consume_current(&input, &live(), &permit).unwrap();
    assert_eq!(
        gate.consume_current(&input, &live(), &permit),
        Err(GateError::AlreadyConsumed)
    );
}

#[test]
fn expiration_connection_changes_and_missing_permit_fail_closed() {
    let gate = CommandGate::new(Duration::from_secs(2)).unwrap();
    let id = gate.issue_operation().unwrap();
    let input = intent(&id, "rm -rf /tmp/x");
    gate.prepare_current(&input, &live()).unwrap();
    let permit = gate.confirm_current(&input, &live()).unwrap();
    let mut changed = live();
    changed.production = false;
    assert_eq!(
        gate.consume_current(&input, &changed, &permit),
        Err(GateError::Conflict)
    );
    changed = live();
    changed.version += 1;
    assert_eq!(
        gate.consume_current(&input, &changed, &permit),
        Err(GateError::Conflict)
    );
    assert_eq!(
        gate.consume_current(&input, &live(), "invalid"),
        Err(GateError::InvalidPermit)
    );
}

#[test]
fn unknown_production_requires_confirmation_but_nonproduction_can_proceed() {
    let gate = CommandGate::new(Duration::from_secs(10)).unwrap();
    let id = gate.issue_operation().unwrap();
    let input = intent(&id, "custom command");
    assert!(matches!(
        gate.prepare_current(&input, &live()).unwrap(),
        Admission::ConfirmationRequired(_)
    ));
    let mut nonprod = live();
    nonprod.production = false;
    let other_id = gate.issue_operation().unwrap();
    let other = intent(&other_id, "rm -rf /tmp/x");
    let Admission::Permitted(_, permit) = gate.prepare_current(&other, &nonprod).unwrap() else {
        panic!("expected permit")
    };
    gate.consume_current(&other, &nonprod, &permit).unwrap();
    nonprod.always_confirm = true;
    assert!(matches!(
        gate.prepare_current(
            &intent(&gate.issue_operation().unwrap(), "rm -rf /tmp/x"),
            &nonprod
        )
        .unwrap(),
        Admission::ConfirmationRequired(_)
    ));
}

#[test]
fn concurrent_consumers_cannot_reuse_a_permit() {
    use std::sync::{Arc, Barrier};
    let gate = Arc::new(CommandGate::new(Duration::from_secs(10)).unwrap());
    let id = gate.issue_operation().unwrap();
    let input = intent(&id, "rm -rf /tmp/x");
    gate.prepare_current(&input, &live()).unwrap();
    let token = gate.confirm_current(&input, &live()).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..2)
        .map(|_| {
            let gate = gate.clone();
            let barrier = barrier.clone();
            let token = token.clone();
            let id = id.clone();
            std::thread::spawn(move || {
                barrier.wait();
                gate.consume_current(&intent(&id, "rm -rf /tmp/x"), &live(), &token)
            })
        })
        .collect();
    barrier.wait();
    let outcomes: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(outcomes.iter().filter(|x| x.is_ok()).count(), 1);
    assert_eq!(
        outcomes
            .iter()
            .filter(|x| **x == Err(GateError::AlreadyConsumed))
            .count(),
        1
    );
}

#[test]
fn session_binding_and_operation_id_conflicts_are_rejected() {
    let gate = CommandGate::new(Duration::from_secs(10)).unwrap();
    let id = gate.issue_operation().unwrap();
    let input = intent(&id, "rm -rf /tmp/x");
    gate.prepare_current(&input, &live()).unwrap();
    let mut changed = intent(&id, "rm -rf /tmp/x");
    changed.session_id = "another".into();
    let mut changed_live = live();
    changed_live.session_id = "another".into();
    assert_eq!(
        gate.confirm_current(&changed, &changed_live),
        Err(GateError::Conflict)
    );
    assert_eq!(
        gate.prepare_current(&intent(&id, "echo other"), &live()),
        Err(GateError::Conflict)
    );
    assert_eq!(
        gate.consume_current(&intent("unprepared", "x"), &live(), "anything"),
        Err(GateError::NotPrepared)
    );
}

#[test]
fn issuing_operations_is_bounded_without_evicting_live_tombstones() {
    let gate = CommandGate::new(Duration::from_secs(30)).unwrap();
    let id = gate.issue_operation().unwrap();
    let input = intent(&id, "test");
    gate.prepare_current(&input, &live()).unwrap();
    let token = gate.confirm_current(&input, &live()).unwrap();
    gate.consume_current(&input, &live(), &token).unwrap();
    for _ in 1..4096 {
        gate.issue_operation().unwrap();
    }
    assert_eq!(gate.issue_operation(), Err(GateError::Capacity));
    assert_eq!(
        gate.prepare_current(&input, &live()),
        Err(GateError::AlreadyConsumed)
    );
}

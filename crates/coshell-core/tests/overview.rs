use coshell_core::{
    feature_flags::{Feature, FeatureFlags},
    overview::{Collector, OverviewCoordinator, OverviewFuture},
};
use coshell_drivers::{server_overview::parse_snapshot, ssh::Cancellation};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

struct Fake {
    calls: AtomicUsize,
    release: tokio::sync::Notify,
}
impl Collector for Fake {
    fn collect<'a>(&'a self, id: &'a str, _cancel: Cancellation) -> OverviewFuture<'a> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.release.notified().await;
            Ok(parse_snapshot(
                id,
                "2026-01-01T00:00:00Z",
                "hostname=test\n",
            ))
        })
    }
}
fn enabled() -> FeatureFlags {
    FeatureFlags::default().with_enabled(Feature::ServerOverview)
}
fn fake() -> Arc<Fake> {
    Arc::new(Fake {
        calls: AtomicUsize::new(0),
        release: tokio::sync::Notify::new(),
    })
}

#[tokio::test]
async fn same_connection_shares_one_collection_and_cache() {
    let f = fake();
    let service = OverviewCoordinator::new(enabled(), f.clone(), Duration::from_secs(2));
    let a = service.refresh("c", "op-a").await.unwrap();
    let b = service.refresh("c", "op-b").await.unwrap();
    tokio::task::yield_now().await;
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.release.notify_one();
    assert_eq!(a.wait().await.unwrap().hostname.as_deref(), Some("test"));
    assert_eq!(b.wait().await.unwrap().hostname.as_deref(), Some("test"));
    assert!(service.cached("c").await.is_some());
}

#[tokio::test]
async fn invalidation_discards_stale_result_and_cancel_is_idempotent() {
    let f = fake();
    let service = OverviewCoordinator::new(enabled(), f.clone(), Duration::from_secs(2));
    let run = service.refresh("old", "old-op").await.unwrap();
    tokio::task::yield_now().await;
    service.invalidate("old").await;
    assert_eq!(run.wait().await.unwrap_err().code, "OVERVIEW_CANCELLED");
    assert!(service.cached("old").await.is_none());
    service.cancel("old", "old-op").await.unwrap();
    service.cancel("old", "old-op").await.unwrap();
}

#[tokio::test]
async fn timeout_and_disabled_gate_have_stable_correlated_errors() {
    let f = fake();
    let disabled = OverviewCoordinator::new(
        FeatureFlags::default(),
        f.clone(),
        Duration::from_millis(30),
    );
    let error = disabled.refresh("c", "op").await.err().unwrap();
    assert_eq!(error.code, "UNSUPPORTED");
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    let service = OverviewCoordinator::new(enabled(), f.clone(), Duration::from_millis(10));
    let error = service
        .refresh("c", "timeout-op")
        .await
        .unwrap()
        .wait()
        .await
        .unwrap_err();
    assert_eq!(error.code, "OVERVIEW_TIMEOUT");
    assert_eq!(error.operation_id.as_deref(), Some("timeout-op"));
    assert!(service.cached("c").await.is_none());
}

#[tokio::test]
async fn unrelated_cancel_cannot_abort_another_operation() {
    let f = fake();
    let service = OverviewCoordinator::new(enabled(), f.clone(), Duration::from_secs(2));
    let run = service.refresh("c", "real-op").await.unwrap();
    tokio::task::yield_now().await;
    service.cancel("c", "other-op").await.unwrap();
    f.release.notify_one();
    assert!(run.wait().await.is_ok());
}

#[tokio::test]
async fn collector_panic_completes_waiter_and_does_not_leave_stuck_flight() {
    struct Panics;
    impl Collector for Panics {
        fn collect<'a>(&'a self, _: &'a str, _: Cancellation) -> OverviewFuture<'a> {
            Box::pin(async { panic!("fixture") })
        }
    }
    let service = OverviewCoordinator::new(enabled(), Arc::new(Panics), Duration::from_secs(1));
    let handle = service.refresh("c", "op").await.unwrap();
    let error = tokio::time::timeout(Duration::from_millis(200), handle.wait())
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, "OVERVIEW_FAILED");
    let again = service.refresh("c", "op2").await.unwrap();
    assert_eq!(
        tokio::time::timeout(Duration::from_millis(200), again.wait())
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "OVERVIEW_FAILED"
    );
}

#[tokio::test]
async fn collector_returning_cancel_or_timeout_does_not_leave_stuck_flight() {
    struct Returns(bool);
    impl Collector for Returns {
        fn collect<'a>(&'a self, _: &'a str, _: Cancellation) -> OverviewFuture<'a> {
            Box::pin(async move {
                Err(if self.0 {
                    coshell_drivers::ssh::SshError::Cancelled
                } else {
                    coshell_drivers::ssh::SshError::Timeout
                })
            })
        }
    }
    for cancel in [true, false] {
        let service =
            OverviewCoordinator::new(enabled(), Arc::new(Returns(cancel)), Duration::from_secs(1));
        for operation in ["one", "two"] {
            let handle = service.refresh("c", operation).await.unwrap();
            let result = tokio::time::timeout(Duration::from_millis(200), handle.wait())
                .await
                .unwrap()
                .unwrap_err();
            assert_eq!(
                result.code,
                if cancel {
                    "OVERVIEW_CANCELLED"
                } else {
                    "OVERVIEW_TIMEOUT"
                }
            );
        }
    }
}

#[tokio::test]
async fn cancel_one_waiter_preserves_other_and_completed_operation_is_idempotent() {
    let f = fake();
    let service = OverviewCoordinator::new(enabled(), f.clone(), Duration::from_secs(1));
    let a = service.refresh("c", "a").await.unwrap();
    let b = service.refresh("c", "b").await.unwrap();
    tokio::task::yield_now().await;
    service.cancel("c", "a").await.unwrap();
    assert_eq!(a.wait().await.unwrap_err().code, "OVERVIEW_CANCELLED");
    f.release.notify_one();
    assert!(b.wait().await.is_ok());
    assert!(service
        .refresh("c", "b")
        .await
        .unwrap()
        .wait()
        .await
        .is_ok());
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn uncooperative_collector_is_aborted_after_bounded_cleanup() {
    let f = fake();
    let service = OverviewCoordinator::new(enabled(), f, Duration::from_millis(10));
    let first = service.refresh("c", "one").await.unwrap();
    assert_eq!(first.wait().await.unwrap_err().code, "OVERVIEW_TIMEOUT");
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(service.refresh("c", "two").await.is_ok());
}

#[tokio::test]
async fn cancelling_waits_for_old_collector_cleanup_before_new_flight() {
    struct SlowStop {
        calls: AtomicUsize,
        release: tokio::sync::Notify,
    }
    impl Collector for SlowStop {
        fn collect<'a>(&'a self, id: &'a str, _: Cancellation) -> OverviewFuture<'a> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.release.notified().await;
                Ok(parse_snapshot(id, "now", "hostname=old\n"))
            })
        }
    }
    let collector = Arc::new(SlowStop {
        calls: AtomicUsize::new(0),
        release: tokio::sync::Notify::new(),
    });
    let service = OverviewCoordinator::new(enabled(), collector.clone(), Duration::from_secs(2));
    let first = service.refresh("c", "op1").await.unwrap();
    tokio::task::yield_now().await;
    service.cancel("c", "op1").await.unwrap();
    assert_eq!(first.wait().await.unwrap_err().code, "OVERVIEW_CANCELLED");
    assert!(service.refresh("c", "op2").await.is_err());
    assert_eq!(collector.calls.load(Ordering::SeqCst), 1);
    collector.release.notify_one();
    for _ in 0..100 {
        if service.refresh("c", "op2").await.is_ok() {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(service.cached("c").await.is_none());
}

#[tokio::test]
async fn timeout_cancel_retry_preserves_first_result() {
    let f = fake();
    let service = OverviewCoordinator::new(enabled(), f, Duration::from_millis(10));
    let handle = service.refresh("c", "op").await.unwrap();
    assert_eq!(handle.wait().await.unwrap_err().code, "OVERVIEW_TIMEOUT");
    service.cancel("c", "op").await.unwrap();
    assert_eq!(
        service
            .refresh("c", "op")
            .await
            .unwrap()
            .wait()
            .await
            .unwrap_err()
            .code,
        "OVERVIEW_TIMEOUT"
    );
}

#[tokio::test]
async fn invalidated_operation_is_a_cancelled_tombstone_and_cannot_switch_connection() {
    let service = OverviewCoordinator::new(enabled(), fake(), Duration::from_secs(1));
    let handle = service.refresh("a", "op").await.unwrap();
    service.invalidate("a").await;
    assert_eq!(handle.wait().await.unwrap_err().code, "OVERVIEW_CANCELLED");
    assert_eq!(
        service
            .refresh("a", "op")
            .await
            .unwrap()
            .wait()
            .await
            .unwrap_err()
            .code,
        "OVERVIEW_CANCELLED"
    );
    assert!(service.refresh("b", "op").await.is_err());
}

#[tokio::test]
async fn cancellation_rejects_wrong_connection_for_active_and_completed_operation() {
    let f = fake();
    let service = OverviewCoordinator::new(enabled(), f.clone(), Duration::from_secs(1));
    let handle = service.refresh("a", "op").await.unwrap();
    tokio::task::yield_now().await;
    assert!(service.cancel("b", "op").await.is_err());
    f.release.notify_one();
    handle.wait().await.unwrap();
    assert!(service.cancel("b", "op").await.is_err());
}

#[tokio::test]
async fn rejects_oversized_collector_fields() {
    struct Oversize;
    impl Collector for Oversize {
        fn collect<'a>(&'a self, id: &'a str, _: Cancellation) -> OverviewFuture<'a> {
            Box::pin(async move {
                let mut s = parse_snapshot(id, "now", "");
                s.hostname = Some("x".repeat(65537));
                Ok(s)
            })
        }
    }
    let service = OverviewCoordinator::new(enabled(), Arc::new(Oversize), Duration::from_secs(1));
    assert_eq!(
        service
            .refresh("c", "op")
            .await
            .unwrap()
            .wait()
            .await
            .unwrap_err()
            .code,
        "OVERVIEW_FAILED"
    );
    assert!(service.cached("c").await.is_none());
}

#[tokio::test]
async fn in_flight_reservations_bound_future_cache_entries() {
    struct Immediate;
    impl Collector for Immediate {
        fn collect<'a>(&'a self, id: &'a str, _: Cancellation) -> OverviewFuture<'a> {
            Box::pin(async move { Ok(parse_snapshot(id, "now", "")) })
        }
    }
    let service = OverviewCoordinator::new(enabled(), Arc::new(Immediate), Duration::from_secs(2));
    for i in 0..255 {
        service
            .refresh(&format!("c{i}"), &format!("o{i}"))
            .await
            .unwrap()
            .wait()
            .await
            .unwrap();
    }
    let a = service.refresh("a", "a-op").await.unwrap();
    assert!(service.refresh("b", "b-op").await.is_err());
    a.wait().await.unwrap();
}

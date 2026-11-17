use coshell_core::session::{SessionError, SessionSupervisor};
use coshell_drivers::ssh::{SshError, SshFuture, TerminalChannel, TerminalSize};
use coshell_types::{SessionEventPayload, SessionState};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;

struct Channel {
    closes: Arc<AtomicUsize>,
    close_pending: bool,
}
impl TerminalChannel for Channel {
    fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { std::future::pending().await })
    }
    fn write<'a>(&'a self, _: &'a [u8]) -> SshFuture<'a, ()> {
        Box::pin(async { Err(SshError::Unsupported) })
    }
    fn resize<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
    fn close<'a>(&'a self) -> SshFuture<'a, ()> {
        Box::pin(async move {
            self.closes.fetch_add(1, Ordering::SeqCst);
            if self.close_pending {
                std::future::pending().await
            } else {
                Ok(())
            }
        })
    }
}
fn channel(pending: bool) -> (Arc<dyn TerminalChannel>, Arc<AtomicUsize>) {
    let closes = Arc::new(AtomicUsize::new(0));
    (
        Arc::new(Channel {
            closes: closes.clone(),
            close_pending: pending,
        }),
        closes,
    )
}

#[tokio::test]
async fn lifecycle_sequences_and_duplicate_close() {
    let s = SessionSupervisor::new(8, 16, Duration::from_millis(30)).unwrap();
    let (c, closes) = channel(false);
    s.register("s", "c", false, c).await.unwrap();
    s.connected("s").await.unwrap();
    s.output("s", b"abc").await.unwrap();
    s.close("s").await.unwrap();
    s.close("s").await.unwrap();
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    let events = s.drain_events("s").await.unwrap();
    assert!(events.windows(2).all(|v| v[0].sequence < v[1].sequence));
    assert!(matches!(
        events.last().unwrap().payload,
        SessionEventPayload::State {
            state: SessionState::Closed
        }
    ));
    assert_eq!(
        s.output("s", b"later").await.unwrap_err(),
        SessionError::InvalidState
    );
}

#[tokio::test]
async fn output_is_bounded_and_truncation_is_observable() {
    let s = SessionSupervisor::new(3, 4, Duration::from_millis(30)).unwrap();
    let (c, _) = channel(false);
    s.register("s", "c", false, c).await.unwrap();
    s.connected("s").await.unwrap();
    s.output("s", b"abc").await.unwrap();
    s.output("s", b"def").await.unwrap();
    assert_eq!(s.scrollback("s").await.unwrap(), b"cdef");
    let events = s.drain_events("s").await.unwrap();
    assert!(events.len() <= 3);
    assert!(events
        .iter()
        .any(|e| matches!(e.payload, SessionEventPayload::Truncated { .. })));
}

#[tokio::test]
async fn disconnect_and_shutdown_cleanup_are_bounded() {
    let s = SessionSupervisor::new(8, 16, Duration::from_millis(20)).unwrap();
    let (c, closes) = channel(true);
    s.register("s", "c", false, c).await.unwrap();
    s.connected("s").await.unwrap();
    assert_eq!(s.disconnect("s").await.unwrap_err(), SessionError::Timeout);
    tokio::task::yield_now().await;
    assert_eq!(closes.load(Ordering::SeqCst), 1);
    assert_eq!(s.state("s").await.unwrap(), SessionState::CloseUnknown);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), s.shutdown())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn concurrent_close_does_not_report_success_while_first_is_pending() {
    let s = Arc::new(SessionSupervisor::new(8, 16, Duration::from_millis(40)).unwrap());
    let (c, closes) = channel(true);
    s.register("s", "c", false, c).await.unwrap();
    s.connected("s").await.unwrap();
    let first = {
        let s = s.clone();
        tokio::spawn(async move { s.close("s").await })
    };
    tokio::task::yield_now().await;
    let second = s.close("s").await;
    assert_eq!(first.await.unwrap(), Err(SessionError::Timeout));
    assert_eq!(second, Err(SessionError::Timeout));
    assert!((1..=2).contains(&closes.load(Ordering::SeqCst)));
}

#[tokio::test]
async fn cancelled_close_keeps_channel_available_for_retry() {
    let s = SessionSupervisor::new(8, 16, Duration::from_secs(1)).unwrap();
    let (c, closes) = channel(true);
    s.register("s", "c", false, c).await.unwrap();
    let mut close = Box::pin(s.close("s"));
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut close)
        .await
        .is_err());
    drop(close);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), s.close("s"))
            .await
            .is_err()
    );
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn shutdown_has_one_total_time_budget() {
    let s = SessionSupervisor::new(8, 16, Duration::from_millis(30)).unwrap();
    for id in ["a", "b", "c", "d"] {
        let (c, _) = channel(true);
        s.register(id, "c", false, c).await.unwrap();
    }
    let start = tokio::time::Instant::now();
    assert!(s.shutdown().await.is_err());
    assert!(start.elapsed() < Duration::from_millis(100));
}

#[tokio::test]
async fn limits_sessions_and_exposes_event_gap() {
    let s = SessionSupervisor::with_limits(2, 3, 4, Duration::from_millis(20)).unwrap();
    for id in ["a", "b"] {
        let (c, _) = channel(false);
        s.register(id, "c", false, c).await.unwrap();
        s.connected(id).await.unwrap();
    }
    let (c, _) = channel(false);
    assert_eq!(
        s.register("c", "c", false, c).await,
        Err(SessionError::Capacity)
    );
    for _ in 0..5 {
        s.output("a", b"x").await.unwrap();
    }
    let batch = s.events_since("a", 0).await.unwrap();
    assert!(batch.gap);
    assert!(batch.events.len() <= 3);
    assert_eq!(s.scrollback("a").await.unwrap(), b"xxxx");
}

#[tokio::test]
async fn timed_out_close_can_be_retried() {
    let s = SessionSupervisor::new(8, 16, Duration::from_millis(15)).unwrap();
    let (c, closes) = channel(true);
    s.register("s", "c", false, c).await.unwrap();
    assert_eq!(s.close("s").await, Err(SessionError::Timeout));
    tokio::task::yield_now().await;
    assert_eq!(s.state("s").await.unwrap(), SessionState::CloseUnknown);
    assert!(s.drain_events("s").await.unwrap().iter().all(|e| !matches!(
        e.payload,
        SessionEventPayload::State {
            state: SessionState::Closed
        }
    )));
    assert_eq!(s.close("s").await, Err(SessionError::Timeout));
    assert_eq!(closes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn closing_blocks_output_and_only_reports_closed_after_success() {
    struct GatedChannel {
        entered: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    impl TerminalChannel for GatedChannel {
        fn read<'a>(&'a self) -> SshFuture<'a, Option<Vec<u8>>> {
            Box::pin(async { std::future::pending().await })
        }
        fn write<'a>(&'a self, _: &'a [u8]) -> SshFuture<'a, ()> {
            Box::pin(async { Err(SshError::Unsupported) })
        }
        fn resize<'a>(&'a self, _: TerminalSize) -> SshFuture<'a, ()> {
            Box::pin(async { Ok(()) })
        }
        fn close<'a>(&'a self) -> SshFuture<'a, ()> {
            Box::pin(async move {
                self.entered.notify_one();
                self.release.notified().await;
                Ok(())
            })
        }
    }
    let s = Arc::new(SessionSupervisor::new(8, 16, Duration::from_secs(10)).unwrap());
    let c = Arc::new(GatedChannel {
        entered: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
    });
    s.register("s", "c", false, c.clone()).await.unwrap();
    s.connected("s").await.unwrap();
    let supervisor = s.clone();
    let close = tokio::spawn(async move { supervisor.close("s").await });
    c.entered.notified().await;
    assert!(!close.is_finished());
    assert_eq!(s.state("s").await.unwrap(), SessionState::Closing);
    assert_eq!(s.output("s", b"x").await, Err(SessionError::InvalidState));
    c.release.notify_one();
    assert_eq!(close.await.unwrap(), Ok(()));
    assert_eq!(s.state("s").await.unwrap(), SessionState::Closed);
}

#[tokio::test]
async fn shutdown_deadline_includes_waiting_for_existing_close_lock() {
    let s = Arc::new(SessionSupervisor::new(8, 16, Duration::from_millis(60)).unwrap());
    let (c, _) = channel(true);
    s.register("s", "c", false, c).await.unwrap();
    let first = {
        let s = s.clone();
        tokio::spawn(async move { s.close("s").await })
    };
    tokio::time::sleep(Duration::from_millis(10)).await;
    let start = tokio::time::Instant::now();
    assert_eq!(s.shutdown().await, Err(SessionError::Timeout));
    assert!(start.elapsed() < Duration::from_millis(90));
    assert_eq!(first.await.unwrap(), Err(SessionError::Timeout));
}

#[tokio::test]
async fn terminal_session_can_be_released_and_snapshot_has_atomic_watermark() {
    let s = SessionSupervisor::with_limits(1, 3, 4, Duration::from_millis(20)).unwrap();
    let (c, _) = channel(false);
    s.register("a", "c", false, c).await.unwrap();
    assert_eq!(s.release("a").await, Err(SessionError::InvalidState));
    s.connected("a").await.unwrap();
    s.output("a", b"abc").await.unwrap();
    let snapshot = s.resync("a").await.unwrap();
    assert_eq!(snapshot.scrollback, b"abc");
    assert_eq!(snapshot.state, SessionState::Connected);
    assert!(snapshot.watermark > 0);
    assert!(
        s.events_since("a", snapshot.watermark + 1)
            .await
            .unwrap()
            .gap
    );
    s.close("a").await.unwrap();
    assert_eq!(s.resync("a").await.unwrap().state, SessionState::Closed);
    s.release("a").await.unwrap();
    let (c, _) = channel(false);
    s.register("b", "c", false, c).await.unwrap();
}

#[tokio::test]
async fn dropping_close_future_eventually_reports_unknown() {
    let s = SessionSupervisor::new(8, 16, Duration::from_millis(20)).unwrap();
    let (c, _) = channel(true);
    s.register("s", "c", false, c).await.unwrap();
    let mut close = Box::pin(s.close("s"));
    assert!(tokio::time::timeout(Duration::from_millis(2), &mut close)
        .await
        .is_err());
    drop(close);
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(s.state("s").await.unwrap(), SessionState::CloseUnknown);
}

#[tokio::test]
async fn rejects_unbounded_session_identifiers() {
    let s = SessionSupervisor::new(8, 16, Duration::from_millis(20)).unwrap();
    let (c, _) = channel(false);
    assert_eq!(
        s.register(&"x".repeat(1025), "c", false, c).await,
        Err(SessionError::InvalidConfig)
    );
}

#[tokio::test]
async fn many_concurrent_close_callers_share_one_attempt() {
    let s = Arc::new(SessionSupervisor::new(8, 16, Duration::from_millis(50)).unwrap());
    let (c, closes) = channel(true);
    s.register("s", "c", false, c).await.unwrap();
    let mut tasks = Vec::new();
    for _ in 0..100 {
        let s = s.clone();
        tasks.push(tokio::spawn(async move { s.close("s").await }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), Err(SessionError::Timeout));
    }
    assert_eq!(closes.load(Ordering::SeqCst), 1);
}

#[test]
fn storage_budget_rejects_many_small_events_with_long_ids() {
    assert!(matches!(
        SessionSupervisor::with_limits(64, 1000, 1, Duration::from_secs(1)),
        Err(SessionError::InvalidConfig)
    ));
}

use coshell_core::webdav_test::WebDavTestCoordinator;
use std::{sync::Arc, time::Duration};

#[tokio::test]
async fn cancellation_before_and_during_test_prevents_or_stops_network_work() {
    let c = Arc::new(WebDavTestCoordinator::default());
    c.cancel("before").unwrap();
    let ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = ran.clone();
    assert!(c
        .run("before", move |_| async move {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })
        .await
        .is_err());
    assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
    let started = Arc::new(tokio::sync::Notify::new());
    let signal = started.clone();
    let worker = c.clone();
    let task = tokio::spawn(async move {
        worker
            .run("during", move |cancel| async move {
                signal.notify_one();
                loop {
                    if cancel.is_cancelled() {
                        return Err("cancelled".into());
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
    });
    started.notified().await;
    c.cancel("during").unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap()
        .is_err());
}

#[tokio::test]
async fn cancellation_interrupts_waiting_for_a_busy_gate() {
    let c = Arc::new(WebDavTestCoordinator::default());
    let gate = Arc::new(tokio::sync::Mutex::new(()));
    let held = gate.lock().await;
    let waiting_gate = gate.clone();
    let worker = c.clone();
    let task = tokio::spawn(async move {
        worker
            .run("waiting", move |_| async move {
                let _guard = waiting_gate.lock().await;
                Ok(())
            })
            .await
    });
    tokio::task::yield_now().await;
    c.cancel("waiting").unwrap();
    let result = tokio::time::timeout(Duration::from_millis(250), task).await;
    drop(held);
    assert!(result
        .expect("cancel must interrupt gate wait")
        .unwrap()
        .is_err());
}

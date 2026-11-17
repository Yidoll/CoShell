//! Bounded lifecycle for read-only WebDAV connection tests.
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub struct TestCancellation {
    cancelled: Arc<AtomicBool>,
    signal: tokio::sync::watch::Sender<bool>,
    network: coshell_backup::webdav::Cancellation,
}
impl TestCancellation {
    fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            signal: tokio::sync::watch::channel(false).0,
            network: coshell_backup::webdav::Cancellation::new(),
        }
    }
    fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
        self.signal.send_replace(true);
        self.network.cancel();
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
    pub fn network(&self) -> &coshell_backup::webdav::Cancellation {
        &self.network
    }
}
struct Entry {
    cancel: TestCancellation,
    started: bool,
    deadline: Instant,
}
#[derive(Default)]
pub struct WebDavTestCoordinator {
    entries: Mutex<HashMap<String, Entry>>,
}
impl WebDavTestCoordinator {
    fn valid(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 128
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    }
    pub fn cancel(&self, id: &str) -> Result<(), String> {
        if !Self::valid(id) {
            return Err("连接测试标识无效".into());
        }
        let mut entries = self.entries.lock().map_err(|_| "连接测试不可用")?;
        let now = Instant::now();
        entries.retain(|_, e| e.deadline > now);
        if !entries.contains_key(id) && entries.len() >= 256 {
            return Err("连接测试数量过多".into());
        }
        let entry = entries.entry(id.into()).or_insert_with(|| Entry {
            cancel: TestCancellation::new(),
            started: false,
            deadline: now + Duration::from_secs(120),
        });
        entry.cancel.cancel();
        Ok(())
    }
    pub async fn run<F, Fut>(&self, id: &str, operation: F) -> Result<(), String>
    where
        F: FnOnce(TestCancellation) -> Fut,
        Fut: Future<Output = Result<(), String>>,
    {
        if !Self::valid(id) {
            return Err("连接测试标识无效".into());
        }
        let token = {
            let mut entries = self.entries.lock().map_err(|_| "连接测试不可用")?;
            let now = Instant::now();
            entries.retain(|_, e| e.deadline > now);
            if !entries.contains_key(id) && entries.len() >= 256 {
                return Err("连接测试数量过多".into());
            }
            let entry = entries.entry(id.into()).or_insert_with(|| Entry {
                cancel: TestCancellation::new(),
                started: false,
                deadline: now + Duration::from_secs(120),
            });
            if entry.started {
                return Err("连接测试标识已使用".into());
            }
            entry.started = true;
            if entry.cancel.is_cancelled() {
                return Err("连接测试已取消".into());
            }
            entry.cancel.clone()
        };
        let mut receiver = token.signal.subscribe();
        if *receiver.borrow() {
            return Err("连接测试已取消".into());
        }
        tokio::select! {
            biased;
            _ = receiver.changed() => Err("连接测试已取消".into()),
            result = tokio::time::timeout(Duration::from_secs(35), operation(token.clone())) => match result {
                Ok(_) if token.is_cancelled() => Err("连接测试已取消".into()),
                Ok(result) => result,
                Err(_) => { token.cancel(); Err("连接测试超时".into()) }
            }
        }
    }
}

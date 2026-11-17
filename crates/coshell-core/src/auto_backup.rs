//! Periodic scheduling and durable configuration for manual WebDAV snapshots.
//! The callback owns credential access and publication; no secret is stored here.
use std::{future::Future, path::Path, pin::Pin, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinHandle};

const KEY: &str = "webdav.auto_interval_minutes";
const TARGET_KEY: &str = "webdav.auto_target_binding";
pub const ALLOWED_MINUTES: [u32; 5] = [5, 10, 30, 60, 1440];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoBackupError {
    InvalidInterval,
    Storage,
}

/// A missing value disables automatic backups. Never store a password in settings.
pub fn load_interval(path: &Path) -> Result<Option<u32>, AutoBackupError> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| AutoBackupError::Storage)?;
    let value: Option<String> = rusqlite::OptionalExtension::optional(db.query_row(
        "SELECT value FROM app_settings WHERE key=?1",
        [KEY],
        |r| r.get(0),
    ))
    .map_err(|_| AutoBackupError::Storage)?;
    match value {
        None => Ok(None),
        Some(value) => {
            let minutes = value.parse::<u32>().map_err(|_| AutoBackupError::Storage)?;
            if ALLOWED_MINUTES.contains(&minutes) {
                Ok(Some(minutes))
            } else {
                Err(AutoBackupError::Storage)
            }
        }
    }
}

pub fn save_interval(
    path: &Path,
    interval: Option<u32>,
    binding: Option<&str>,
) -> Result<(), AutoBackupError> {
    if interval.is_some_and(|minutes| !ALLOWED_MINUTES.contains(&minutes)) {
        return Err(AutoBackupError::InvalidInterval);
    }
    if interval.is_some() && binding.filter(|s| !s.is_empty()).is_none() {
        return Err(AutoBackupError::InvalidInterval);
    }
    let mut db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(|_| AutoBackupError::Storage)?;
    let tx = db
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|_| AutoBackupError::Storage)?;
    match interval {
        Some(minutes) => {
            tx.execute("INSERT INTO app_settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![KEY, minutes.to_string()]).map_err(|_| AutoBackupError::Storage)?;
            tx.execute("INSERT INTO app_settings(key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", rusqlite::params![TARGET_KEY, binding]).map_err(|_| AutoBackupError::Storage)?;
        }
        None => {
            tx.execute("DELETE FROM app_settings WHERE key=?1", [KEY])
                .map_err(|_| AutoBackupError::Storage)?;
            tx.execute("DELETE FROM app_settings WHERE key=?1", [TARGET_KEY])
                .map_err(|_| AutoBackupError::Storage)?;
        }
    }
    tx.commit().map_err(|_| AutoBackupError::Storage)
}

pub fn target_matches(path: &Path, binding: &str) -> Result<bool, AutoBackupError> {
    let db =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|_| AutoBackupError::Storage)?;
    let stored: Option<String> = rusqlite::OptionalExtension::optional(db.query_row(
        "SELECT value FROM app_settings WHERE key=?1",
        [TARGET_KEY],
        |r| r.get(0),
    ))
    .map_err(|_| AutoBackupError::Storage)?;
    Ok(stored.as_deref() == Some(binding))
}

pub type BackupFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'static>>;
pub type BackupAction = Arc<dyn Fn() -> BackupFuture + Send + Sync + 'static>;

/// The first run is due one full interval after start or configuration change.
/// Calls never overlap; errors do not stop future scheduled runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoBackupStatus {
    Waiting,
    Running,
    Succeeded,
    Failed,
}
pub struct AutoBackupScheduler {
    configuration: watch::Sender<Option<Duration>>,
    status: watch::Receiver<AutoBackupStatus>,
    task: JoinHandle<()>,
}
impl AutoBackupScheduler {
    pub fn start(interval: Option<u32>, action: BackupAction) -> Result<Self, AutoBackupError> {
        let duration = to_duration(interval)?;
        let (configuration, receiver) = watch::channel(duration);
        let (status_tx, status) = watch::channel(AutoBackupStatus::Waiting);
        let task = tokio::spawn(run(receiver, status_tx, action));
        Ok(Self {
            configuration,
            status,
            task,
        })
    }
    pub fn set_interval(&self, interval: Option<u32>) -> Result<(), AutoBackupError> {
        self.configuration.send_replace(to_duration(interval)?);
        Ok(())
    }
    pub fn status(&self) -> AutoBackupStatus {
        *self.status.borrow()
    }
}
impl Drop for AutoBackupScheduler {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn to_duration(interval: Option<u32>) -> Result<Option<Duration>, AutoBackupError> {
    match interval {
        None => Ok(None),
        Some(minutes) if ALLOWED_MINUTES.contains(&minutes) => {
            Ok(Some(Duration::from_secs(u64::from(minutes) * 60)))
        }
        Some(_) => Err(AutoBackupError::InvalidInterval),
    }
}
async fn run(
    mut receiver: watch::Receiver<Option<Duration>>,
    status: watch::Sender<AutoBackupStatus>,
    action: BackupAction,
) {
    loop {
        let interval = *receiver.borrow_and_update();
        match interval {
            None => {
                if receiver.changed().await.is_err() {
                    return;
                }
            }
            Some(delay) => {
                tokio::select! {
                    changed = receiver.changed() => if changed.is_err() { return; },
                    _ = tokio::time::sleep(delay) => {
                        status.send_replace(AutoBackupStatus::Running);
                        let result = action().await;
                        status.send_replace(if result.is_ok() { AutoBackupStatus::Succeeded } else { AutoBackupStatus::Failed });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn interval_persists_and_only_allowed_values_are_accepted() {
        let path = std::env::temp_dir().join(format!(
            "coshell-auto-{}-{}.db",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut db = rusqlite::Connection::open(&path).unwrap();
        coshell_storage::migrate(&mut db).unwrap();
        drop(db);
        assert_eq!(load_interval(&path), Ok(None));
        for minutes in ALLOWED_MINUTES {
            save_interval(&path, Some(minutes), Some("target-a")).unwrap();
            assert_eq!(load_interval(&path), Ok(Some(minutes)));
        }
        assert_eq!(
            save_interval(&path, Some(7), Some("target-a")),
            Err(AutoBackupError::InvalidInterval)
        );
        assert_eq!(load_interval(&path), Ok(Some(1440)));
        assert!(target_matches(&path, "target-a").unwrap());
        assert!(!target_matches(&path, "target-b").unwrap());
        save_interval(&path, None, None).unwrap();
        assert_eq!(load_interval(&path), Ok(None));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn scheduler_serializes_runs_and_disabling_stops_future_runs() {
        let count = Arc::new(AtomicUsize::new(0));
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let (entered_tx, mut entered_rx) = tokio::sync::mpsc::unbounded_channel();
        let release = Arc::new(tokio::sync::Notify::new());
        let action: BackupAction = Arc::new({
            let count = count.clone();
            let active = active.clone();
            let maximum = maximum.clone();
            let release = release.clone();
            move || {
                let count = count.clone();
                let active = active.clone();
                let maximum = maximum.clone();
                let release = release.clone();
                let entered_tx = entered_tx.clone();
                Box::pin(async move {
                    count.fetch_add(1, Ordering::SeqCst);
                    let n = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(n, Ordering::SeqCst);
                    entered_tx.send(()).unwrap();
                    release.notified().await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    Err("failure".into())
                })
            }
        });
        let (tx, rx) = watch::channel(Some(Duration::from_millis(5)));
        let (status, mut status_rx) = watch::channel(AutoBackupStatus::Waiting);
        let task = tokio::spawn(run(rx, status, action));
        tokio::time::timeout(Duration::from_secs(2), entered_rx.recv())
            .await
            .unwrap()
            .unwrap();
        tx.send_replace(None);
        release.notify_one();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                status_rx.changed().await.unwrap();
                if *status_rx.borrow() == AutoBackupStatus::Failed {
                    break;
                }
            }
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(maximum.load(Ordering::SeqCst), 1);
        task.abort();
    }
}

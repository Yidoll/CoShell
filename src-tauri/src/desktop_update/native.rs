use super::{is_newer_stable, valid_operation_id, DesktopUpdate, Inner, Snapshot, Status};
use coshell_core::desktop_runtime::DesktopRuntime;
use std::{future::Future, sync::Arc, time::Duration};
use tauri::{AppHandle, Manager, Runtime, State};
use tauri_plugin_updater::{Error, Update, Updater, UpdaterExt};
use tokio::sync::Notify;

const CHECK_TIMEOUT: Duration = Duration::from_secs(15);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CANCELLED: &str = "UPDATE_CANCELLED";

pub(super) struct Candidate(Update);

pub(super) fn public_key() -> Option<&'static str> {
    option_env!("COSHELL_UPDATER_PUBLIC_KEY")
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn platform_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("macos", "x86_64") => Some("darwin-x86_64"),
        ("macos", "aarch64") => Some("darwin-aarch64"),
        _ => None,
    }
}

fn build_updater<R: Runtime>(app: &AppHandle<R>) -> Result<Updater, &'static str> {
    let builder = app
        .updater_builder()
        .pubkey(public_key().ok_or("UPDATE_NOT_CONFIGURED")?)
        .target(platform_target().ok_or("UPDATE_PLATFORM_UNAVAILABLE")?)
        .configure_client(|client| client.https_only(true))
        .timeout(CHECK_TIMEOUT)
        .version_comparator(|current, remote| {
            is_newer_stable(&current.to_string(), &remote.version.to_string())
        });
    // The default Windows hook hides all windows before launching the installer.
    // Keep CoShell visible if launching the installer fails.
    #[cfg(windows)]
    let builder = builder.on_before_exit(|| {});
    builder.build().map_err(|_| "UPDATE_NOT_CONFIGURED")
}

fn check_error(error: Error) -> &'static str {
    match error {
        Error::Reqwest(error) if error.is_decode() => "UPDATE_MANIFEST",
        error => update_error(error),
    }
}

fn update_error(error: Error) -> &'static str {
    match error {
        Error::TargetNotFound(_)
        | Error::TargetsNotFound(_)
        | Error::UnsupportedOs
        | Error::UnsupportedArch => "UPDATE_PLATFORM_UNAVAILABLE",
        Error::Base64(_)
        | Error::Minisign(_)
        | Error::SignatureUtf8(_)
        | Error::SignedVersionMismatch { .. }
        | Error::MissingSignedVersion => "UPDATE_SIGNATURE",
        Error::Reqwest(error) if error.is_timeout() => "UPDATE_TIMEOUT",
        Error::Network(_) | Error::Reqwest(_) | Error::ReleaseNotFound => "UPDATE_NETWORK",
        _ => "UPDATE_MANIFEST",
    }
}

async fn interruptible<T>(
    cancel: &Notify,
    deadline: Duration,
    work: impl Future<Output = Result<T, &'static str>>,
) -> Result<T, &'static str> {
    tokio::select! {
        biased;
        _ = cancel.notified() => Err(CANCELLED),
        result = tokio::time::timeout(deadline, work) => result.unwrap_or(Err("UPDATE_TIMEOUT")),
    }
}

fn valid_download_url(update: &Update) -> bool {
    let url = &update.download_url;
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path().starts_with("/Yidoll/CoShell/releases/download/")
        && url.query().is_none()
        && url.fragment().is_none()
}

#[cfg(test)]
mod tests;

impl Inner {
    fn replay(&self, id: &str) -> Result<bool, String> {
        if self.snapshot.status == Status::Unsupported || self.matches(id) {
            return Ok(true);
        }
        if !valid_operation_id(id) {
            return Err("UPDATE_INVALID_OPERATION".into());
        }
        if self.recent.iter().any(|old| old == id) {
            return Err("UPDATE_STALE".into());
        }
        if matches!(
            self.snapshot.status,
            Status::Checking | Status::Downloading | Status::Installing
        ) {
            return Err("UPDATE_BUSY".into());
        }
        Ok(false)
    }

    fn matches(&self, id: &str) -> bool {
        self.snapshot.operation_id.as_deref() == Some(id)
    }

    fn start(&mut self, id: String, status: Status) -> Arc<Notify> {
        if let Some(old) = self.snapshot.operation_id.replace(id) {
            if self.recent.len() == 32 {
                self.recent.pop_front();
            }
            self.recent.push_back(old);
        }
        self.snapshot.status = status;
        self.snapshot.error = None;
        self.snapshot.blocking_count = None;
        self.snapshot.downloaded_bytes = None;
        self.snapshot.total_bytes = None;
        let cancel = Arc::new(Notify::new());
        self.cancel = Some(cancel.clone());
        cancel
    }

    fn finish_download(&mut self, id: &str, result: Result<Vec<u8>, &'static str>) {
        if !self.matches(id) || self.snapshot.status != Status::Downloading {
            return;
        }
        let result = if self.cancel.take().is_none() {
            Err(CANCELLED)
        } else {
            result
        };
        match result {
            Ok(package) => {
                self.snapshot.status = Status::Ready;
                self.snapshot.downloaded_bytes = Some(package.len() as u64);
                self.package = Some(package);
            }
            Err(code) => {
                self.snapshot.status = Status::Available;
                self.snapshot.error = Some(code.into());
                self.snapshot.downloaded_bytes = None;
                self.snapshot.total_bytes = None;
            }
        }
    }
}

#[tauri::command]
pub(crate) fn desktop_update_check(
    app: AppHandle,
    updater: State<'_, DesktopUpdate>,
    operation_id: String,
) -> Result<Snapshot, String> {
    let updater = updater.inner().clone();
    let (cancel, snapshot) = {
        let mut inner = updater.lock()?;
        if inner.replay(&operation_id)? {
            return Ok(inner.snapshot.clone());
        }
        if inner.snapshot.status == Status::Ready {
            return Err("UPDATE_BUSY".into());
        }
        let cancel = inner.start(operation_id.clone(), Status::Checking);
        inner.candidate = None;
        inner.snapshot.version = None;
        inner.snapshot.notes = None;
        (cancel, inner.snapshot.clone())
    };
    tauri::async_runtime::spawn(async move {
        let result = interruptible(&cancel, CHECK_TIMEOUT, async {
            let mut candidate = build_updater(&app)?.check().await.map_err(check_error)?;
            if let Some(update) = candidate.as_mut() {
                if !valid_download_url(update) {
                    return Err("UPDATE_MANIFEST");
                }
                update.timeout = Some(DOWNLOAD_TIMEOUT);
            }
            Ok(candidate)
        })
        .await;
        if let Ok(mut inner) = updater.lock() {
            if !inner.matches(&operation_id) {
                return;
            }
            let cancelled = inner.cancel.take().is_none();
            match (cancelled, result) {
                (true, _) | (_, Err(CANCELLED)) => inner.snapshot.status = Status::Idle,
                (false, Ok(Some(update))) => {
                    inner.snapshot.version = Some(update.version.clone());
                    inner.snapshot.notes = update
                        .body
                        .as_deref()
                        .map(|text| text.chars().take(12_000).collect());
                    inner.snapshot.status = Status::Available;
                    inner.candidate = Some(Candidate(update));
                }
                (false, Ok(None)) => inner.snapshot.status = Status::Current,
                (false, Err(code)) => {
                    inner.snapshot.status = Status::Failed;
                    inner.snapshot.error = Some(code.into());
                }
            }
        }
    });
    Ok(snapshot)
}

#[tauri::command]
pub(crate) fn desktop_update_download(
    updater: State<'_, DesktopUpdate>,
    operation_id: String,
) -> Result<Snapshot, String> {
    let updater = updater.inner().clone();
    let (candidate, cancel, snapshot) = {
        let mut inner = updater.lock()?;
        if inner.replay(&operation_id)? {
            return Ok(inner.snapshot.clone());
        }
        if inner.snapshot.status != Status::Available {
            return Err("UPDATE_STALE".into());
        }
        let candidate = inner.candidate.as_ref().ok_or("UPDATE_STALE")?.0.clone();
        let cancel = inner.start(operation_id.clone(), Status::Downloading);
        inner.snapshot.downloaded_bytes = Some(0);
        (candidate, cancel, inner.snapshot.clone())
    };
    tauri::async_runtime::spawn(async move {
        let mut progress = 0_u64;
        let result = interruptible(&cancel, DOWNLOAD_TIMEOUT, async {
            candidate
                .download(
                    |chunk, total| {
                        progress = progress.saturating_add(chunk as u64);
                        if let Ok(mut inner) = updater.lock() {
                            if inner.matches(&operation_id) && inner.cancel.is_some() {
                                inner.snapshot.downloaded_bytes = Some(progress);
                                inner.snapshot.total_bytes = total;
                            }
                        }
                    },
                    || {},
                )
                .await
                .map_err(update_error)
        })
        .await;
        if let Ok(mut inner) = updater.lock() {
            inner.finish_download(&operation_id, result);
        }
    });
    Ok(snapshot)
}

#[tauri::command]
pub(crate) fn desktop_update_cancel(
    updater: State<'_, DesktopUpdate>,
    operation_id: String,
) -> Result<Snapshot, String> {
    let mut inner = updater.lock()?;
    if inner.snapshot.status == Status::Unsupported {
        return Ok(inner.snapshot.clone());
    }
    if !inner.matches(&operation_id) {
        return Err("UPDATE_STALE".into());
    }
    if inner.snapshot.status == Status::Installing {
        return Err("UPDATE_BUSY".into());
    }
    if let Some(cancel) = inner.cancel.take() {
        cancel.notify_one();
    }
    Ok(inner.snapshot.clone())
}

#[tauri::command]
pub(crate) fn desktop_update_install(
    app: AppHandle,
    updater: State<'_, DesktopUpdate>,
    operation_id: String,
) -> Result<Snapshot, String> {
    let updater = updater.inner().clone();
    let (candidate, package, snapshot) = {
        let mut inner = updater.lock()?;
        if inner.snapshot.status == Status::Unsupported {
            return Ok(inner.snapshot.clone());
        }
        if !inner.matches(&operation_id) {
            return Err("UPDATE_STALE".into());
        }
        if inner.snapshot.status == Status::Installing {
            return Ok(inner.snapshot.clone());
        }
        if inner.snapshot.status != Status::Ready {
            return Err("UPDATE_STALE".into());
        }
        let candidate = inner.candidate.as_ref().ok_or("UPDATE_STALE")?.0.clone();
        let package = inner.package.take().ok_or("UPDATE_STALE")?;
        inner.snapshot.status = Status::Installing;
        inner.snapshot.error = None;
        inner.snapshot.blocking_count = None;
        (candidate, package, inner.snapshot.clone())
    };
    tauri::async_runtime::spawn(async move {
        let runtime = app.state::<DesktopRuntime>();
        let (error, blocking_count, package) = match runtime.begin_update_install().await {
            Ok(guard) => {
                let work = tauri::async_runtime::spawn_blocking(move || {
                    let result = candidate.install(&package);
                    (result, package)
                })
                .await;
                match work {
                    Ok((Ok(()), _)) => {
                        guard.commit();
                        #[cfg(target_os = "macos")]
                        app.request_restart();
                        return;
                    }
                    Ok((Err(_), package)) => ("UPDATE_INSTALL_FAILED", None, Some(package)),
                    Err(_) => ("UPDATE_INSTALL_FAILED", None, None),
                }
            }
            Err(blocked) => (blocked.code, blocked.count, Some(package)),
        };
        if let Ok(mut inner) = updater.lock() {
            inner.snapshot.status = if package.is_some() {
                Status::Ready
            } else {
                Status::Available
            };
            inner.snapshot.error = Some(error.into());
            inner.snapshot.blocking_count = blocking_count;
            inner.package = package;
        }
    });
    Ok(snapshot)
}

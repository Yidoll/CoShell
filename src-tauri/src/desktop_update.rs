use serde::Serialize;
use std::sync::{Arc, Mutex, MutexGuard};

#[cfg(any(windows, target_os = "macos"))]
use native::Candidate;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Unsupported,
    Idle,
    Checking,
    Current,
    Available,
    Downloading,
    Ready,
    Installing,
    Failed,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Snapshot {
    current_version: String,
    status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    operation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downloaded_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    blocking_count: Option<usize>,
}

#[derive(Clone)]
pub(crate) struct DesktopUpdate(Arc<Mutex<Inner>>);

struct Inner {
    snapshot: Snapshot,
    #[cfg(any(windows, target_os = "macos"))]
    candidate: Option<Candidate>,
    #[cfg(any(windows, target_os = "macos"))]
    package: Option<Vec<u8>>,
    #[cfg(any(windows, target_os = "macos"))]
    cancel: Option<Arc<tokio::sync::Notify>>,
    #[cfg(any(windows, target_os = "macos"))]
    recent: std::collections::VecDeque<String>,
}

impl DesktopUpdate {
    pub(crate) fn new(current_version: String) -> Self {
        #[cfg(any(windows, target_os = "macos"))]
        let enabled = native::public_key().is_some() && native::platform_target().is_some();
        #[cfg(not(any(windows, target_os = "macos")))]
        let enabled = false;
        let (status, error) = if enabled {
            (Status::Idle, None)
        } else {
            (Status::Unsupported, Some("UPDATE_NOT_CONFIGURED".into()))
        };
        Self(Arc::new(Mutex::new(Inner {
            snapshot: Snapshot {
                current_version,
                status,
                error,
                blocking_count: None,
                operation_id: None,
                version: None,
                notes: None,
                downloaded_bytes: None,
                total_bytes: None,
            },
            #[cfg(any(windows, target_os = "macos"))]
            candidate: None,
            #[cfg(any(windows, target_os = "macos"))]
            package: None,
            #[cfg(any(windows, target_os = "macos"))]
            cancel: None,
            #[cfg(any(windows, target_os = "macos"))]
            recent: Default::default(),
        })))
    }

    fn lock(&self) -> Result<MutexGuard<'_, Inner>, String> {
        self.0.lock().map_err(|_| "UPDATE_STATE_UNAVAILABLE".into())
    }
}

#[tauri::command]
pub(crate) fn desktop_update_state(
    updater: tauri::State<'_, DesktopUpdate>,
) -> Result<Snapshot, String> {
    Ok(updater.lock()?.snapshot.clone())
}

#[cfg(any(windows, target_os = "macos"))]
mod native;

#[cfg(any(windows, target_os = "macos"))]
pub(crate) use native::{
    __cmd__desktop_update_cancel, __cmd__desktop_update_check, __cmd__desktop_update_download,
    __cmd__desktop_update_install, __tauri_command_name_desktop_update_cancel,
    __tauri_command_name_desktop_update_check, __tauri_command_name_desktop_update_download,
    __tauri_command_name_desktop_update_install, desktop_update_cancel, desktop_update_check,
    desktop_update_download, desktop_update_install,
};

#[cfg(not(any(windows, target_os = "macos")))]
mod unsupported {
    use super::{DesktopUpdate, Snapshot};

    #[tauri::command]
    pub(crate) fn desktop_update_check(
        updater: tauri::State<'_, DesktopUpdate>,
        operation_id: String,
    ) -> Result<Snapshot, String> {
        let _ = operation_id;
        super::desktop_update_state(updater)
    }
    #[tauri::command]
    pub(crate) fn desktop_update_download(
        updater: tauri::State<'_, DesktopUpdate>,
        operation_id: String,
    ) -> Result<Snapshot, String> {
        let _ = operation_id;
        super::desktop_update_state(updater)
    }
    #[tauri::command]
    pub(crate) fn desktop_update_cancel(
        updater: tauri::State<'_, DesktopUpdate>,
        operation_id: String,
    ) -> Result<Snapshot, String> {
        let _ = operation_id;
        super::desktop_update_state(updater)
    }
    #[tauri::command]
    pub(crate) fn desktop_update_install(
        updater: tauri::State<'_, DesktopUpdate>,
        operation_id: String,
    ) -> Result<Snapshot, String> {
        let _ = operation_id;
        super::desktop_update_state(updater)
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(crate) use unsupported::{
    __cmd__desktop_update_cancel, __cmd__desktop_update_check, __cmd__desktop_update_download,
    __cmd__desktop_update_install, __tauri_command_name_desktop_update_cancel,
    __tauri_command_name_desktop_update_check, __tauri_command_name_desktop_update_download,
    __tauri_command_name_desktop_update_install, desktop_update_cancel, desktop_update_check,
    desktop_update_download, desktop_update_install,
};

#[cfg(any(test, windows, target_os = "macos"))]
fn valid_operation_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                matches!(byte, b'0'..=b'9' | b'a'..=b'f')
            }
        })
}

#[cfg(any(test, windows, target_os = "macos"))]
fn is_newer_stable(current: &str, candidate: &str) -> bool {
    let (Ok(current), Ok(candidate)) = (
        semver::Version::parse(current),
        semver::Version::parse(candidate),
    ) else {
        return false;
    };
    candidate.pre.is_empty() && candidate.cmp_precedence(&current).is_gt()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_newer_stable_versions_are_eligible() {
        assert!(is_newer_stable("0.1.1", "0.1.2"));
        assert!(!is_newer_stable("0.1.1", "0.1.1"));
        assert!(!is_newer_stable("0.1.1", "0.1.0"));
        assert!(!is_newer_stable("0.1.1", "0.2.0-beta.1"));
    }

    #[test]
    fn operation_ids_must_be_canonical_uuids() {
        assert!(valid_operation_id("2a664ea6-a04c-4e81-8e7c-cb2ce001a8a0"));
        assert!(!valid_operation_id("../coshell"));
        assert!(!valid_operation_id("2A664ea6-a04c-4e81-8e7c-cb2ce001a8a0"));
    }
}

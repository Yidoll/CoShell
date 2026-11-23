use coshell_core::desktop_runtime::{
    ConnectionDto, DesktopRuntime, GroupDto, OpenResult, ReadResult, SubmitResult, WebDavChoiceDto,
    WebDavConfigDto, WebDavPreviewDto,
};
use coshell_core::RuntimeService;
use coshell_types::HealthReport;
use tauri::Manager;

#[tauri::command]
async fn remote_list(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    path: String,
) -> Result<Vec<coshell_core::desktop_runtime::RemoteFileEntry>, String> {
    runtime.remote_list(&session_id, path).await
}
#[tauri::command]
async fn remote_realpath(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    path: String,
) -> Result<String, String> {
    runtime.remote_realpath(&session_id, path).await
}
#[tauri::command]
async fn remote_read(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    path: String,
    offset: u64,
    length: u32,
) -> Result<Vec<u8>, String> {
    runtime.remote_read(&session_id, path, offset, length).await
}
#[tauri::command]
async fn remote_stat(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    path: String,
) -> Result<coshell_core::desktop_runtime::RemoteFileEntry, String> {
    runtime.remote_stat(&session_id, path).await
}
#[tauri::command]
async fn remote_read_file(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    path: String,
    limit: usize,
) -> Result<coshell_core::desktop_runtime::RemoteFileContent, String> {
    runtime.remote_read_file(&session_id, path, limit).await
}
#[tauri::command]
async fn remote_prepare(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    mutation: coshell_core::remote_files::Mutation,
) -> Result<coshell_core::desktop_runtime::RemoteFilePermit, String> {
    runtime.remote_prepare(&session_id, mutation).await
}
#[tauri::command]
async fn remote_commit(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    token: String,
) -> Result<(), String> {
    runtime.remote_commit(&session_id, token).await
}
#[tauri::command]
async fn remote_confirm(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    token: String,
) -> Result<(), String> {
    runtime.remote_confirm(&session_id, token).await
}
#[tauri::command]
async fn remote_cancel(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    token: String,
) -> Result<(), String> {
    runtime.remote_cancel(&session_id, token).await;
    Ok(())
}

struct AutoBackupState {
    scheduler: tokio::sync::Mutex<coshell_core::auto_backup::AutoBackupScheduler>,
    enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
    generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[tauri::command]
async fn webdav_auto_get(
    runtime: tauri::State<'_, DesktopRuntime>,
    scheduler: tauri::State<'_, AutoBackupState>,
) -> Result<(Option<u32>, String), String> {
    let state = scheduler.scheduler.lock().await;
    let interval = runtime.webdav_auto_interval()?;
    if interval.is_some() && !scheduler.enabled.load(std::sync::atomic::Ordering::Acquire) {
        return Ok((interval, "paused".to_owned()));
    }
    Ok((interval, format!("{:?}", state.status()).to_lowercase()))
}

#[tauri::command]
async fn webdav_auto_set(
    runtime: tauri::State<'_, DesktopRuntime>,
    scheduler: tauri::State<'_, AutoBackupState>,
    interval_minutes: Option<u32>,
    accept_plaintext: bool,
) -> Result<(), String> {
    let state = scheduler.scheduler.lock().await;
    scheduler
        .enabled
        .store(false, std::sync::atomic::Ordering::Release);
    scheduler
        .generation
        .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    state
        .set_interval(None)
        .map_err(|_| "自动备份间隔无效".to_owned())?;
    runtime
        .webdav_set_auto_interval(interval_minutes, accept_plaintext)
        .await?;
    state
        .set_interval(interval_minutes)
        .map_err(|_| "自动备份间隔无效".to_owned())?;
    scheduler.enabled.store(
        interval_minutes.is_some(),
        std::sync::atomic::Ordering::Release,
    );
    Ok(())
}

#[tauri::command]
fn health() -> HealthReport {
    RuntimeService::new().health()
}
#[tauri::command]
fn saved_command_list(
    runtime: tauri::State<'_, DesktopRuntime>,
) -> Result<Vec<coshell_core::saved_commands::SavedCommand>, String> {
    runtime.saved_command_list()
}
#[tauri::command]
fn saved_command_save(
    runtime: tauri::State<'_, DesktopRuntime>,
    input: coshell_core::saved_commands::SavedCommand,
) -> Result<coshell_core::saved_commands::SavedCommand, String> {
    runtime.saved_command_save(input)
}
#[tauri::command]
fn saved_command_delete(
    runtime: tauri::State<'_, DesktopRuntime>,
    id: String,
    version: u64,
) -> Result<bool, String> {
    runtime.saved_command_delete(&id, version)
}

#[tauri::command]
fn webdav_get_config(
    runtime: tauri::State<'_, DesktopRuntime>,
) -> Result<Option<WebDavConfigDto>, String> {
    runtime.webdav_config()
}
#[tauri::command]
async fn webdav_test_config(
    runtime: tauri::State<'_, DesktopRuntime>,
    operation_id: String,
) -> Result<String, String> {
    runtime.webdav_test_config(&operation_id).await
}
#[tauri::command]
fn webdav_test_cancel(
    runtime: tauri::State<'_, DesktopRuntime>,
    operation_id: String,
) -> Result<(), String> {
    runtime.webdav_test_cancel(&operation_id)
}
#[tauri::command]
async fn webdav_save_config(
    runtime: tauri::State<'_, DesktopRuntime>,
    input: WebDavConfigDto,
    password: Option<String>,
    operation_id: String,
) -> Result<WebDavConfigDto, String> {
    runtime
        .webdav_save_config(input, password, operation_id)
        .await
}
#[tauri::command]
async fn webdav_config_save_status(
    runtime: tauri::State<'_, DesktopRuntime>,
    operation_id: String,
) -> Result<String, String> {
    runtime.webdav_config_save_status(&operation_id).await
}
#[tauri::command]
async fn webdav_sync(
    runtime: tauri::State<'_, DesktopRuntime>,
    passphrase: String,
    operation_id: String,
) -> Result<String, String> {
    runtime.webdav_sync(passphrase, operation_id).await
}
#[tauri::command]
async fn webdav_sync_status(
    runtime: tauri::State<'_, DesktopRuntime>,
    operation_id: String,
) -> Result<Option<coshell_core::publication_outbox::PublicationStatus>, String> {
    runtime.webdav_sync_status(operation_id).await
}
#[tauri::command]
async fn webdav_recent_syncs(
    runtime: tauri::State<'_, DesktopRuntime>,
) -> Result<Vec<coshell_core::publication_outbox::RecentPublication>, String> {
    runtime.webdav_recent_syncs().await
}
#[tauri::command]
async fn webdav_preview_restore(
    runtime: tauri::State<'_, DesktopRuntime>,
    passphrase: String,
    version_id: Option<String>,
) -> Result<WebDavPreviewDto, String> {
    runtime.webdav_preview_version(passphrase, version_id).await
}
#[tauri::command]
async fn webdav_list_versions(
    runtime: tauri::State<'_, DesktopRuntime>,
    cursor: Option<String>,
) -> Result<coshell_core::desktop_runtime::BackupVersionPage, String> {
    runtime.webdav_list_versions(cursor).await
}
#[tauri::command]
async fn webdav_list_snapshots(
    runtime: tauri::State<'_, DesktopRuntime>,
    cursor: Option<String>,
) -> Result<coshell_core::desktop_runtime::SnapshotPage, String> {
    runtime.webdav_list_snapshots(cursor).await
}
#[tauri::command]
async fn webdav_preview_snapshot(
    runtime: tauri::State<'_, DesktopRuntime>,
    passphrase: String,
    snapshot_id: String,
) -> Result<WebDavPreviewDto, String> {
    runtime
        .webdav_preview_snapshot(passphrase, snapshot_id)
        .await
}
#[tauri::command]
async fn webdav_reconcile(
    runtime: tauri::State<'_, DesktopRuntime>,
    operation_id: String,
) -> Result<String, String> {
    runtime.webdav_reconcile(operation_id).await
}
#[tauri::command]
async fn webdav_apply_restore(
    runtime: tauri::State<'_, DesktopRuntime>,
    token: String,
    choices: Vec<WebDavChoiceDto>,
    confirm_groups: bool,
) -> Result<(), String> {
    runtime
        .webdav_apply_restore(token, choices, confirm_groups)
        .await
}
#[tauri::command]
fn webdav_restore_status(
    runtime: tauri::State<'_, DesktopRuntime>,
    operation_id: String,
) -> Result<Option<coshell_core::restore_secrets::RestoreStatus>, String> {
    runtime.webdav_restore_status(&operation_id)
}

#[tauri::command]
async fn connection_list(
    runtime: tauri::State<'_, DesktopRuntime>,
) -> Result<Vec<ConnectionDto>, String> {
    runtime.list().await
}
#[tauri::command]
async fn connection_save(
    runtime: tauri::State<'_, DesktopRuntime>,
    input: ConnectionDto,
) -> Result<ConnectionDto, String> {
    runtime.save(input).await
}
#[tauri::command]
async fn connection_copy(
    runtime: tauri::State<'_, DesktopRuntime>,
    source: String,
    id: String,
    name: String,
) -> Result<ConnectionDto, String> {
    runtime.copy(&source, &id, &name).await
}
#[tauri::command]
async fn connection_delete(
    runtime: tauri::State<'_, DesktopRuntime>,
    id: String,
) -> Result<bool, String> {
    runtime.delete(&id).await
}
#[tauri::command]
async fn group_list(runtime: tauri::State<'_, DesktopRuntime>) -> Result<Vec<GroupDto>, String> {
    runtime.groups().await
}
#[tauri::command]
async fn group_save(
    runtime: tauri::State<'_, DesktopRuntime>,
    input: GroupDto,
) -> Result<(), String> {
    runtime.save_group(input).await
}
#[tauri::command]
async fn group_delete(
    runtime: tauri::State<'_, DesktopRuntime>,
    id: String,
) -> Result<bool, String> {
    runtime.delete_group(&id).await
}
#[tauri::command]
async fn ssh_open(
    runtime: tauri::State<'_, DesktopRuntime>,
    connection_id: String,
    password: Option<String>,
    accepted_fingerprint: Option<String>,
    operation_id: String,
    private_key: Option<String>,
    passphrase: Option<String>,
    use_saved_key: Option<bool>,
    use_saved_password: Option<bool>,
) -> Result<OpenResult, String> {
    runtime
        .open_ssh_with_credentials(
            connection_id,
            password,
            accepted_fingerprint,
            operation_id,
            private_key,
            passphrase,
            use_saved_key.unwrap_or(false),
            use_saved_password.unwrap_or(false),
        )
        .await
}
#[tauri::command]
async fn ssh_password_state(
    runtime: tauri::State<'_, DesktopRuntime>,
    connection_id: String,
) -> Result<String, String> {
    runtime
        .ssh_password_state(&connection_id)
        .await
        .map(str::to_owned)
}
#[tauri::command]
async fn ssh_password_decision(
    runtime: tauri::State<'_, DesktopRuntime>,
    token: String,
    operation_id: String,
    save: bool,
) -> Result<(), String> {
    runtime
        .ssh_password_decision(&token, &operation_id, save)
        .await
}
#[tauri::command]
async fn ssh_key_saved(
    runtime: tauri::State<'_, DesktopRuntime>,
    connection_id: String,
) -> Result<bool, String> {
    runtime.ssh_key_saved(&connection_id).await
}
#[tauri::command]
async fn ssh_key_save(
    runtime: tauri::State<'_, DesktopRuntime>,
    connection_id: String,
    private_key: String,
    passphrase: Option<String>,
) -> Result<(), String> {
    runtime
        .ssh_key_save(&connection_id, private_key, passphrase)
        .await
}
#[tauri::command]
async fn ssh_key_delete(
    runtime: tauri::State<'_, DesktopRuntime>,
    connection_id: String,
) -> Result<(), String> {
    runtime.ssh_key_delete(&connection_id).await
}
#[tauri::command]
async fn ssh_key_operation_status(
    runtime: tauri::State<'_, DesktopRuntime>,
    connection_id: String,
    operation_id: String,
) -> Result<String, String> {
    runtime
        .ssh_key_operation_status(&connection_id, &operation_id)
        .await
}
#[tauri::command]
async fn ssh_cancel(
    runtime: tauri::State<'_, DesktopRuntime>,
    operation_id: String,
) -> Result<(), String> {
    runtime.cancel_open(&operation_id).await
}
#[tauri::command]
async fn session_read(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
) -> Result<ReadResult, String> {
    runtime.read(&session_id).await
}
#[tauri::command]
async fn session_submit(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    command: String,
    confirm_token: Option<String>,
) -> Result<SubmitResult, String> {
    runtime.submit(&session_id, command, confirm_token).await
}
#[tauri::command]
async fn session_run_saved(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    saved_command_id: String,
    expected_version: u64,
    confirm_token: Option<String>,
) -> Result<coshell_core::desktop_runtime::SavedRunResult, String> {
    runtime
        .run_saved(
            &session_id,
            &saved_command_id,
            expected_version,
            confirm_token,
        )
        .await
}
#[tauri::command]
async fn session_write(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    data: String,
) -> Result<(), String> {
    runtime.write_terminal(&session_id, data).await
}
#[tauri::command]
async fn session_resize(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    runtime.resize_terminal(&session_id, cols, rows).await
}
#[tauri::command]
async fn session_close(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
) -> Result<(), String> {
    runtime.close(&session_id).await
}

#[tauri::command]
async fn server_overview(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    operation_id: String,
) -> Result<coshell_types::ServerOverviewSnapshot, String> {
    runtime.server_overview(&session_id, &operation_id).await
}
#[tauri::command]
async fn server_overview_cancel(
    runtime: tauri::State<'_, DesktopRuntime>,
    session_id: String,
    operation_id: String,
) -> Result<(), String> {
    runtime
        .server_overview_cancel(&session_id, &operation_id)
        .await
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let runtime = DesktopRuntime::open_default().map_err(std::io::Error::other)?;
            let interval = runtime
                .webdav_auto_interval()
                .map_err(std::io::Error::other)?;
            app.manage(runtime);
            let app_handle = app.handle().clone();
            let enabled =
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(interval.is_some()));
            let generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let action_enabled = enabled.clone();
            let action_generation = generation.clone();
            let action: coshell_core::auto_backup::BackupAction = std::sync::Arc::new(move || {
                let handle = app_handle.clone();
                let enabled = action_enabled.clone();
                let generation = action_generation.clone();
                let expected = generation.load(std::sync::atomic::Ordering::Acquire);
                Box::pin(async move {
                    handle
                        .state::<DesktopRuntime>()
                        .webdav_auto_sync(&enabled, &generation, expected)
                        .await
                })
            });
            let scheduler = tauri::async_runtime::block_on(async {
                coshell_core::auto_backup::AutoBackupScheduler::start(interval, action)
            })
            .map_err(|_| std::io::Error::other("自动备份初始化失败"))?;
            app.manage(AutoBackupState {
                scheduler: tokio::sync::Mutex::new(scheduler),
                enabled,
                generation,
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            health,
            saved_command_list,
            saved_command_save,
            saved_command_delete,
            remote_list,
            remote_realpath,
            remote_read,
            remote_stat,
            remote_read_file,
            remote_prepare,
            remote_commit,
            remote_confirm,
            remote_cancel,
            webdav_get_config,
            webdav_auto_get,
            webdav_auto_set,
            webdav_test_config,
            webdav_test_cancel,
            webdav_save_config,
            webdav_config_save_status,
            webdav_sync,
            webdav_sync_status,
            webdav_recent_syncs,
            webdav_preview_restore,
            webdav_list_versions,
            webdav_list_snapshots,
            webdav_preview_snapshot,
            webdav_reconcile,
            webdav_apply_restore,
            webdav_restore_status,
            connection_list,
            connection_save,
            connection_copy,
            connection_delete,
            group_list,
            group_save,
            group_delete,
            ssh_open,
            ssh_password_state,
            ssh_password_decision,
            ssh_key_saved,
            ssh_key_save,
            ssh_key_delete,
            ssh_key_operation_status,
            ssh_cancel,
            session_read,
            session_submit,
            session_run_saved,
            session_write,
            session_resize,
            session_close,
            server_overview,
            server_overview_cancel
        ])
        .run(tauri::generate_context!())
        .expect("error while running CoShell desktop");
}

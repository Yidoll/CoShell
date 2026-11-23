import type { DesktopUpdateState } from '@/lib/backend/types';
import type { ConnectionGroup, ConnectionProfile, HealthOptions, HealthStatus, ServerOverviewSnapshot, SessionReadResult, SessionSubmitResult, SshOpenResult } from "@/lib/backend/types";
import { decodeVersionPage } from './webdav-versions';
import { decodeSnapshotPage } from './webdav-snapshots';

export async function health(options: HealthOptions = {}): Promise<HealthStatus> {
  if (options.signal?.aborted) throw new DOMException("Request aborted", "AbortError");
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<HealthStatus>("health");
}

async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(command, args);
}
export function connectionList() { return invokeCommand<ConnectionProfile[]>("connection_list"); }
export function remoteRealpath(sessionId:string,path:string){return invokeCommand<string>('remote_realpath',{sessionId,path});}
export function remoteList(sessionId:string,path:string){return invokeCommand<import('./types').RemoteFileEntry[]>('remote_list',{sessionId,path});}
export function remoteStat(sessionId:string,path:string){return invokeCommand<import('./types').RemoteFileEntry>('remote_stat',{sessionId,path});}
export function remoteRead(sessionId:string,path:string,offset:number,length:number){return invokeCommand<number[]>('remote_read',{sessionId,path,offset,length});}
export function remoteReadFile(sessionId:string,path:string,limit:number){return invokeCommand<{bytes:number[];metadata:import('./types').RemoteFileEntry}>('remote_read_file',{sessionId,path,limit});}
export function remotePrepare(sessionId:string,mutation:import('./types').RemoteFileMutation){return invokeCommand<{token:string;confirmationRequired:boolean}>('remote_prepare',{sessionId,mutation});}
export function remoteCommit(sessionId:string,token:string){return invokeCommand<void>('remote_commit',{sessionId,token});}
export function remoteConfirm(sessionId:string,token:string){return invokeCommand<void>('remote_confirm',{sessionId,token});}
export function remoteCancel(sessionId:string,token:string){return invokeCommand<void>('remote_cancel',{sessionId,token});}
export function savedCommandList() { return invokeCommand<import('./types').SavedCommand[]>('saved_command_list'); }
export function savedCommandSave(input:import('./types').SavedCommand) { return invokeCommand<import('./types').SavedCommand>('saved_command_save',{input}); }
export function savedCommandDelete(id:string,version:number) { return invokeCommand<boolean>('saved_command_delete',{id,version}); }
export function sessionRunSaved(sessionId:string,savedCommandId:string,expectedVersion:number,confirmToken?:string){return invokeCommand<import('./types').SessionSubmitResult&{output?:string}>('session_run_saved',{sessionId,savedCommandId,expectedVersion,confirmToken:confirmToken??null});}
export function connectionSave(input: ConnectionProfile) { return invokeCommand<ConnectionProfile>("connection_save", { input }); }
export function connectionCopy(source: string, id: string, name: string) { return invokeCommand<ConnectionProfile>("connection_copy", { source, id, name }); }
export function connectionDelete(id: string) { return invokeCommand<boolean>("connection_delete", { id }); }
export function groupList() { return invokeCommand<ConnectionGroup[]>("group_list"); }
export function groupSave(input: ConnectionGroup) { return invokeCommand<void>("group_save", { input }); }
export function groupDelete(id: string) { return invokeCommand<boolean>("group_delete", { id }); }
export function sshOpen(connectionId: string, password: string | null, operationId: string, acceptedFingerprint?: string, privateKey?: string | null, passphrase?: string | null, useSavedKey = false, useSavedPassword = false) {
  return invokeCommand<SshOpenResult>("ssh_open", {
    connectionId, password, operationId, acceptedFingerprint: acceptedFingerprint ?? null,
    privateKey: privateKey ?? null, passphrase: passphrase ?? null, useSavedKey, useSavedPassword,
  });
}
export function sshPasswordState(connectionId: string) {
  return invokeCommand<import('./types').SshPasswordState>('ssh_password_state', { connectionId });
}
export function sshPasswordDecision(token: string, operationId: string, save: boolean) {
  return invokeCommand<void>('ssh_password_decision', { token, operationId, save });
}
export function sshKeySaved(connectionId: string) { return invokeCommand<boolean>('ssh_key_saved', { connectionId }); }
export function sshKeySave(connectionId: string, privateKey: string, passphrase: string | null) { return invokeCommand<void>('ssh_key_save', { connectionId, privateKey, passphrase }); }
export function sshKeyDelete(connectionId: string) { return invokeCommand<void>('ssh_key_delete', { connectionId }); }
export function sshKeyOperationStatus(connectionId: string, operationId: string) { return invokeCommand<string>('ssh_key_operation_status', { connectionId, operationId }); }
export function webdavGetConfig() { return invokeCommand<import('./types').DavConfig | null>('webdav_get_config'); }
export function webdavTestConfig(operationId: string) { return invokeCommand<string>('webdav_test_config', { operationId }); }
export function webdavTestCancel(operationId: string) { return invokeCommand<void>('webdav_test_cancel', { operationId }); }
export function webdavAutoGet() { return invokeCommand<[number | null, string]>('webdav_auto_get'); }
export function webdavAutoSet(intervalMinutes: number | null, acceptPlaintext: boolean) { return invokeCommand<void>('webdav_auto_set', { intervalMinutes, acceptPlaintext }); }
export function webdavSaveConfig(input: import('./types').DavConfig, password: string | null, operationId: string) { return invokeCommand<import('./types').DavConfig>('webdav_save_config', { input, password, operationId }); }
export function webdavConfigSaveStatus(operationId: string) { return invokeCommand<import('./types').DavConfigSaveStatus>('webdav_config_save_status', { operationId }); }
export function webdavSync(passphrase: string, operationId: string) { return invokeCommand<string>('webdav_sync', { passphrase, operationId }); }
export function webdavReconcile(operationId:string){return invokeCommand<string>('webdav_reconcile',{operationId});}
export function webdavSyncStatus(operationId: string) { return invokeCommand<{state:string;error:string|null;cleanupPending:boolean;possibleObjects:string[]}|null>('webdav_sync_status', { operationId }); }
export function webdavRecentSyncs() { return invokeCommand<{operationId:string;status:{state:string;error:string|null;cleanupPending:boolean;possibleObjects:string[]}}[]>('webdav_recent_syncs'); }
export function webdavListVersions(cursor?: string) { return invokeCommand<unknown>('webdav_list_versions', { cursor: cursor ?? null }).then(decodeVersionPage); }
export function webdavListSnapshots(cursor?: string) { return invokeCommand<unknown>('webdav_list_snapshots', { cursor: cursor ?? null }).then(decodeSnapshotPage); }
export function webdavPreviewSnapshot(passphrase:string,snapshotId:string) { return invokeCommand<import('./types').DavPreview>('webdav_preview_snapshot',{passphrase,snapshotId}); }
export function webdavPreviewRestore(passphrase: string, versionId?: string) { return invokeCommand<import('./types').DavPreview>('webdav_preview_restore', { passphrase, versionId: versionId ?? null }); }
export function webdavApplyRestore(token: string, choices: import('./types').DavChoice[], confirmGroups: boolean) { return invokeCommand<void>('webdav_apply_restore', { token, choices, confirmGroups }); }
export function webdavRestoreStatus(operationId: string) { return invokeCommand<{state:string;cleanupPending:boolean}|null>('webdav_restore_status', { operationId }); }
export function sshCancel(operationId: string) { return invokeCommand<void>("ssh_cancel", { operationId }); }
export function sessionRead(sessionId: string) { return invokeCommand<SessionReadResult>("session_read", { sessionId }); }
export function sessionWrite(sessionId: string, data: string) { return invokeCommand<void>("session_write", { sessionId, data }); }
export function sessionResize(sessionId: string, cols: number, rows: number) { return invokeCommand<void>("session_resize", { sessionId, cols, rows }); }
export function sessionSubmit(sessionId: string, command: string, confirmToken?: string) { return invokeCommand<SessionSubmitResult>("session_submit", { sessionId, command, confirmToken: confirmToken ?? null }); }
export function sessionClose(sessionId: string) { return invokeCommand<void>("session_close", { sessionId }); }
export function serverOverview(sessionId: string, operationId: string) { return invokeCommand<ServerOverviewSnapshot>("server_overview", { sessionId, operationId }); }
export function serverOverviewCancel(sessionId: string, operationId: string) { return invokeCommand<void>("server_overview_cancel", { sessionId, operationId }); }

export function desktopUpdateState() { return invokeCommand<DesktopUpdateState>('desktop_update_state'); }
export function desktopUpdateCheck(operationId: string) { return invokeCommand<DesktopUpdateState>('desktop_update_check', {operationId}); }
export function desktopUpdateDownload(operationId: string) { return invokeCommand<DesktopUpdateState>('desktop_update_download', {operationId}); }
export function desktopUpdateCancel(operationId: string) { return invokeCommand<DesktopUpdateState>('desktop_update_cancel', {operationId}); }
export function desktopUpdateInstall(operationId: string) { return invokeCommand<DesktopUpdateState>('desktop_update_install', {operationId}); }

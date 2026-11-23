import type { Backend, ConnectionGroup, ConnectionProfile, HealthOptions, HealthStatus } from "@/lib/backend/types";
import { detectBackendRuntime, type BackendRuntime } from "@/lib/backend/runtime";

let backend: Backend | undefined;
export async function remoteRealpath(sessionId:string,path:string){return(await resolveBackend()).remoteRealpath(sessionId,path);}
export async function remoteList(sessionId:string,path:string){return(await resolveBackend()).remoteList(sessionId,path);}
export async function remoteStat(sessionId:string,path:string){return(await resolveBackend()).remoteStat(sessionId,path);}
export async function remoteRead(sessionId:string,path:string,offset:number,length:number){return(await resolveBackend()).remoteRead(sessionId,path,offset,length);}
export async function remoteReadFile(sessionId:string,path:string,limit:number){return(await resolveBackend()).remoteReadFile(sessionId,path,limit);}
export async function remotePrepare(sessionId:string,mutation:import('./types').RemoteFileMutation){return(await resolveBackend()).remotePrepare(sessionId,mutation);}
export async function remoteCommit(sessionId:string,token:string){return(await resolveBackend()).remoteCommit(sessionId,token);}
export async function remoteConfirm(sessionId:string,token:string){return(await resolveBackend()).remoteConfirm(sessionId,token);}
export async function remoteCancel(sessionId:string,token:string){return(await resolveBackend()).remoteCancel(sessionId,token);}
export async function savedCommandList() { return (await resolveBackend()).savedCommandList(); }
export async function savedCommandSave(input: import('./types').SavedCommand) { return (await resolveBackend()).savedCommandSave(input); }
export async function savedCommandDelete(id:string,version:number) { return (await resolveBackend()).savedCommandDelete(id,version); }
export async function sessionRunSaved(sessionId:string,savedCommandId:string,expectedVersion:number,confirmToken?:string){return(await resolveBackend()).sessionRunSaved(sessionId,savedCommandId,expectedVersion,confirmToken);}

async function resolveBackend(): Promise<Backend> {
  if (backend) return backend;
  backend = detectBackendRuntime() === "tauri" ? await import("@/lib/backend/tauri") : await import("@/lib/backend/http");
  return backend;
}

export function backendRuntime(): BackendRuntime {
  return detectBackendRuntime();
}

export async function health(options?: HealthOptions): Promise<HealthStatus> {
  return (await resolveBackend()).health(options);
}

export async function connectionList() { return (await resolveBackend()).connectionList(); }
export async function connectionSave(input: ConnectionProfile) { return (await resolveBackend()).connectionSave(input); }
export async function connectionCopy(source: string, id: string, name: string) { return (await resolveBackend()).connectionCopy(source, id, name); }
export async function connectionDelete(id: string) { return (await resolveBackend()).connectionDelete(id); }
export async function groupList() { return (await resolveBackend()).groupList(); }
export async function groupSave(input: ConnectionGroup) { return (await resolveBackend()).groupSave(input); }
export async function groupDelete(id: string) { return (await resolveBackend()).groupDelete(id); }
export async function sshOpen(connectionId: string, password: string | null, operationId: string, acceptedFingerprint?: string, privateKey?: string | null, passphrase?: string | null, useSavedKey?: boolean, useSavedPassword?: boolean) { return (await resolveBackend()).sshOpen(connectionId, password, operationId, acceptedFingerprint, privateKey, passphrase, useSavedKey, useSavedPassword); }
export async function sshPasswordState(connectionId: string) { return (await resolveBackend()).sshPasswordState(connectionId); }
export async function sshPasswordDecision(token: string, operationId: string, save: boolean) { return (await resolveBackend()).sshPasswordDecision(token, operationId, save); }
export async function sshKeySaved(connectionId: string) { return (await resolveBackend()).sshKeySaved(connectionId); }
export async function sshKeySave(connectionId: string, privateKey: string, passphrase: string | null) { return (await resolveBackend()).sshKeySave(connectionId, privateKey, passphrase); }
export async function sshKeyDelete(connectionId: string) { return (await resolveBackend()).sshKeyDelete(connectionId); }
export async function sshKeyOperationStatus(connectionId: string, operationId: string) { return (await resolveBackend()).sshKeyOperationStatus(connectionId, operationId); }
export async function webdavGetConfig() { return (await resolveBackend()).webdavGetConfig(); }
export async function webdavTestConfig(operationId: string) { return (await resolveBackend()).webdavTestConfig(operationId); }
export async function webdavTestCancel(operationId: string) { return (await resolveBackend()).webdavTestCancel(operationId); }
export async function webdavAutoGet() { return (await resolveBackend()).webdavAutoGet(); }
export async function webdavAutoSet(intervalMinutes: number | null, acceptPlaintext: boolean) { return (await resolveBackend()).webdavAutoSet(intervalMinutes, acceptPlaintext); }
export async function webdavSaveConfig(input: import('./types').DavConfig, password: string | null, operationId: string) { return (await resolveBackend()).webdavSaveConfig(input, password, operationId); }
export async function webdavConfigSaveStatus(operationId: string) { return (await resolveBackend()).webdavConfigSaveStatus(operationId); }
export async function webdavSync(passphrase: string, operationId: string) { return (await resolveBackend()).webdavSync(passphrase, operationId); }
export async function webdavReconcile(operationId:string){return(await resolveBackend()).webdavReconcile(operationId);}
export async function webdavSyncStatus(operationId: string) { return (await resolveBackend()).webdavSyncStatus(operationId); }
export async function webdavRecentSyncs() { return (await resolveBackend()).webdavRecentSyncs(); }
export async function webdavListVersions(cursor?: string) { return (await resolveBackend()).webdavListVersions(cursor); }
export async function webdavListSnapshots(cursor?: string) { return (await resolveBackend()).webdavListSnapshots(cursor); }
export async function webdavPreviewSnapshot(passphrase: string, snapshotId: string) { return (await resolveBackend()).webdavPreviewSnapshot(passphrase, snapshotId); }
export async function webdavPreviewRestore(passphrase: string, versionId?: string) { return (await resolveBackend()).webdavPreviewRestore(passphrase, versionId); }
export async function webdavApplyRestore(token: string, choices: import('./types').DavChoice[], confirmGroups: boolean) { return (await resolveBackend()).webdavApplyRestore(token, choices, confirmGroups); }
export async function webdavRestoreStatus(operationId: string) { return (await resolveBackend()).webdavRestoreStatus(operationId); }
export async function sshCancel(operationId: string) { return (await resolveBackend()).sshCancel(operationId); }
export async function sessionRead(sessionId: string) { return (await resolveBackend()).sessionRead(sessionId); }
export async function sessionWrite(sessionId: string, data: string) { return (await resolveBackend()).sessionWrite(sessionId, data); }
export async function sessionResize(sessionId: string, cols: number, rows: number) { return (await resolveBackend()).sessionResize(sessionId, cols, rows); }
export async function sessionSubmit(sessionId: string, command: string, confirmToken?: string) { return (await resolveBackend()).sessionSubmit(sessionId, command, confirmToken); }
export async function sessionClose(sessionId: string) { return (await resolveBackend()).sessionClose(sessionId); }
export async function serverOverview(sessionId: string, operationId: string) { return (await resolveBackend()).serverOverview(sessionId, operationId); }
export async function serverOverviewCancel(sessionId: string, operationId: string) { return (await resolveBackend()).serverOverviewCancel(sessionId, operationId); }

export function resetBackendForTests(): void {
  backend = undefined;
}

export type { HealthStatus } from "@/lib/backend/types";

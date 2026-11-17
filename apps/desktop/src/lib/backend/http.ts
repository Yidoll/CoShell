import type { HealthOptions, HealthStatus } from "@/lib/backend/types";
import type { ConnectionGroup, ConnectionProfile } from "@/lib/backend/types";
import type { ServerOverviewSnapshot, SessionReadResult, SessionSubmitResult, SshOpenResult } from "@/lib/backend/types";

export async function health(options: HealthOptions = {}): Promise<HealthStatus> {
  const response = await fetch("/api/health", {
    method: "GET",
    headers: { Accept: "application/json" },
    signal: options.signal,
  });
  if (!response.ok) {
    throw new Error(`Core 状态请求失败（HTTP ${response.status}）`);
  }
  return (await response.json()) as HealthStatus;
}

// HTTP has no authenticated session transport yet; fail closed until it does.
function unsupported(): never { throw new Error("当前 Web 模式暂不支持 SSH 连接，请使用桌面版。"); }
export async function remoteRealpath(_sessionId:string,_path:string):Promise<string>{return unsupported();}
export async function remoteList(_sessionId:string,_path:string):Promise<import('./types').RemoteFileEntry[]>{return unsupported();}
export async function remoteStat(_sessionId:string,_path:string):Promise<import('./types').RemoteFileEntry>{return unsupported();}
export async function remoteRead(_sessionId:string,_path:string,_offset:number,_length:number):Promise<number[]>{return unsupported();}
export async function remoteReadFile(_sessionId:string,_path:string,_limit:number):Promise<{bytes:number[];metadata:import('./types').RemoteFileEntry}>{return unsupported();}
export async function remotePrepare(_sessionId:string,_mutation:import('./types').RemoteFileMutation):Promise<{token:string;confirmationRequired:boolean}>{return unsupported();}
export async function remoteCommit(_sessionId:string,_token:string):Promise<void>{return unsupported();}
export async function remoteConfirm(_sessionId:string,_token:string):Promise<void>{return unsupported();}
export async function remoteCancel(_sessionId:string,_token:string):Promise<void>{return unsupported();}
export async function savedCommandList():Promise<import('./types').SavedCommand[]> { return unsupported(); }
export async function savedCommandSave(_input:import('./types').SavedCommand):Promise<import('./types').SavedCommand> { return unsupported(); }
export async function savedCommandDelete(_id:string,_version:number):Promise<boolean> { return unsupported(); }
export async function sessionRunSaved(_sessionId:string,_savedCommandId:string,_expectedVersion:number,_confirmToken?:string):Promise<import('./types').SessionSubmitResult&{output?:string}>{return unsupported();}
export async function connectionList(): Promise<ConnectionProfile[]> { return unsupported(); }
export async function connectionSave(_input: ConnectionProfile): Promise<ConnectionProfile> { return unsupported(); }
export async function connectionCopy(_source: string, _id: string, _name: string): Promise<ConnectionProfile> { return unsupported(); }
export async function connectionDelete(_id: string): Promise<boolean> { return unsupported(); }
export async function groupList(): Promise<ConnectionGroup[]> { return unsupported(); }
export async function groupSave(_input: ConnectionGroup): Promise<void> { return unsupported(); }
export async function groupDelete(_id: string): Promise<boolean> { return unsupported(); }
export async function sshOpen(_connectionId: string, _password: string | null, _operationId: string, _acceptedFingerprint?: string, _privateKey?: string | null, _passphrase?: string | null, _useSavedKey?: boolean): Promise<SshOpenResult> { return unsupported(); }
export async function sshKeySaved(_connectionId: string): Promise<boolean> { return unsupported(); }
export async function sshKeySave(_connectionId: string, _privateKey: string, _passphrase: string | null): Promise<void> { return unsupported(); }
export async function sshKeyDelete(_connectionId: string): Promise<void> { return unsupported(); }
export async function sshKeyOperationStatus(_connectionId: string, _operationId: string): Promise<string> { return unsupported(); }
export async function webdavGetConfig(): Promise<import('./types').DavConfig | null> { return unsupported(); }
export async function webdavTestConfig(_operationId: string): Promise<string> { return unsupported(); }
export async function webdavTestCancel(_operationId: string): Promise<void> { return unsupported(); }
export async function webdavAutoGet(): Promise<[number | null, string]> { return unsupported(); }
export async function webdavAutoSet(_intervalMinutes: number | null, _acceptPlaintext: boolean): Promise<void> { return unsupported(); }
export async function webdavSaveConfig(_input: import('./types').DavConfig, _password: string | null, _operationId: string): Promise<void> { return unsupported(); }
export async function webdavSync(_passphrase: string, _operationId: string): Promise<string> { return unsupported(); }
export async function webdavReconcile(_operationId:string):Promise<string>{return unsupported();}
export async function webdavSyncStatus(_operationId: string): Promise<{state:string;error:string|null;cleanupPending:boolean;possibleObjects:string[]}|null> { return unsupported(); }
export async function webdavRecentSyncs(): Promise<{operationId:string;status:{state:string;error:string|null;cleanupPending:boolean;possibleObjects:string[]}}[]> { return unsupported(); }
export async function webdavListVersions(_cursor?: string): Promise<import('./types').DavVersionPage> { return unsupported(); }
export async function webdavListSnapshots(_cursor?:string):Promise<import('./types').DavSnapshotPage>{return unsupported();}
export async function webdavPreviewSnapshot(_passphrase:string,_snapshotId:string):Promise<import('./types').DavPreview>{return unsupported();}
export async function webdavPreviewRestore(_passphrase: string, _versionId?: string): Promise<import('./types').DavPreview> { return unsupported(); }
export async function webdavApplyRestore(_token: string, _choices: import('./types').DavChoice[], _confirmGroups: boolean): Promise<void> { return unsupported(); }
export async function webdavRestoreStatus(_operationId: string): Promise<{state:string;cleanupPending:boolean}|null> { return unsupported(); }
export async function sshCancel(_operationId: string): Promise<void> { return unsupported(); }
export async function sessionRead(_sessionId: string): Promise<SessionReadResult> { return unsupported(); }
export async function sessionWrite(_sessionId: string, _data: string): Promise<void> { return unsupported(); }
export async function sessionResize(_sessionId: string, _cols: number, _rows: number): Promise<void> { return unsupported(); }
export async function sessionSubmit(_sessionId: string, _command: string, _confirmToken?: string): Promise<SessionSubmitResult> { return unsupported(); }
export async function sessionClose(_sessionId: string): Promise<void> { return unsupported(); }
export async function serverOverview(_sessionId: string, _operationId: string): Promise<ServerOverviewSnapshot> { return unsupported(); }
export async function serverOverviewCancel(_sessionId: string, _operationId: string): Promise<void> { return unsupported(); }

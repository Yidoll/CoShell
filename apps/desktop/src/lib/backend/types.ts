export interface HealthComponent {
  name: string;
  state: "ready" | "degraded";
  detail: string;
}

export interface HealthStatus {
  service: string;
  version: string;
  state: "ready" | "degraded";
  components: HealthComponent[];
}

export interface HealthOptions {
  signal?: AbortSignal;
}

export interface Backend {
  remoteRealpath(sessionId:string,path:string):Promise<string>;
  remoteList(sessionId:string,path:string):Promise<RemoteFileEntry[]>;
  remoteStat(sessionId:string,path:string):Promise<RemoteFileEntry>;
  remoteRead(sessionId:string,path:string,offset:number,length:number):Promise<number[]>;
  remoteReadFile(sessionId:string,path:string,limit:number):Promise<{bytes:number[];metadata:RemoteFileEntry}>;
  remotePrepare(sessionId:string,mutation:RemoteFileMutation):Promise<{token:string;confirmationRequired:boolean}>;
  remoteCommit(sessionId:string,token:string):Promise<void>;
  remoteConfirm(sessionId:string,token:string):Promise<void>;
  remoteCancel(sessionId:string,token:string):Promise<void>;
  savedCommandList(): Promise<SavedCommand[]>;
  savedCommandSave(input: SavedCommand): Promise<SavedCommand>;
  savedCommandDelete(id: string, version: number): Promise<boolean>;
  sessionRunSaved(sessionId:string,savedCommandId:string,expectedVersion:number,confirmToken?:string):Promise<(SessionSubmitResult & {output?:string})>;
  health(options?: HealthOptions): Promise<HealthStatus>;
  connectionList(): Promise<ConnectionProfile[]>;
  connectionSave(input: ConnectionProfile): Promise<ConnectionProfile>;
  connectionCopy(source: string, id: string, name: string): Promise<ConnectionProfile>;
  connectionDelete(id: string): Promise<boolean>;
  groupList(): Promise<ConnectionGroup[]>;
  groupSave(input: ConnectionGroup): Promise<void>;
  groupDelete(id: string): Promise<boolean>;
  sshOpen(connectionId: string, password: string | null, operationId: string, acceptedFingerprint?: string, privateKey?: string | null, passphrase?: string | null, useSavedKey?: boolean, useSavedPassword?: boolean): Promise<SshOpenResult>;
  sshPasswordState(connectionId: string): Promise<SshPasswordState>;
  sshPasswordDecision(token: string, operationId: string, save: boolean): Promise<void>;
  sshKeySaved(connectionId: string): Promise<boolean>;
  sshKeySave(connectionId: string, privateKey: string, passphrase: string | null): Promise<void>;
  sshKeyDelete(connectionId: string): Promise<void>;
  sshKeyOperationStatus(connectionId: string, operationId: string): Promise<string>;
  webdavGetConfig(): Promise<DavConfig | null>;
  webdavTestConfig(operationId: string): Promise<string>;
  webdavTestCancel(operationId: string): Promise<void>;
  webdavAutoGet(): Promise<[number | null, string]>;
  webdavAutoSet(intervalMinutes: number | null, acceptPlaintext: boolean): Promise<void>;
  webdavSaveConfig(input: DavConfig, password: string | null, operationId: string): Promise<DavConfig>;
  webdavConfigSaveStatus(operationId: string): Promise<DavConfigSaveStatus>;
  webdavSync(passphrase: string, operationId: string): Promise<string>;
  webdavReconcile(operationId: string): Promise<string>;
  webdavSyncStatus(operationId: string): Promise<{state:string;error:string|null;cleanupPending:boolean;possibleObjects:string[]}|null>;
  webdavRecentSyncs(): Promise<{operationId:string;status:{state:string;error:string|null;cleanupPending:boolean;possibleObjects:string[]}}[]>;
  webdavListVersions(cursor?: string): Promise<DavVersionPage>;
  webdavListSnapshots(cursor?: string): Promise<DavSnapshotPage>;
  webdavPreviewSnapshot(passphrase: string, snapshotId: string): Promise<DavPreview>;
  webdavPreviewRestore(passphrase: string, versionId?: string): Promise<DavPreview>;
  webdavApplyRestore(token: string, choices: DavChoice[], confirmGroups: boolean): Promise<void>;
  webdavRestoreStatus(operationId: string): Promise<{state:string;cleanupPending:boolean}|null>;
  sshCancel(operationId: string): Promise<void>;
  sessionRead(sessionId: string): Promise<SessionReadResult>;
  sessionWrite(sessionId: string, data: string): Promise<void>;
  sessionResize(sessionId: string, cols: number, rows: number): Promise<void>;
  sessionSubmit(sessionId: string, command: string, confirmToken?: string): Promise<SessionSubmitResult>;
  sessionClose(sessionId: string): Promise<void>;
  serverOverview(sessionId: string, operationId: string): Promise<ServerOverviewSnapshot>;
  serverOverviewCancel(sessionId: string, operationId: string): Promise<void>;
}
export interface SavedCommand { id:string; name:string; content:string; category:string; version:number }
export interface RemoteFileEntry { name:string; kind:'directory'|'file'|'symlink'|'unknown'; size:number|null; permissions:number|null; modified:number|null }
export type RemoteFileMutation = {action:'mkdir'|'create';path:string}|{action:'remove';path:string;directory:boolean}|{action:'rename';path:string;destination:string}|{action:'chmod';path:string;mode:number}|{action:'write';path:string;data:number[];overwrite:boolean;expectedDigest?:string|null};
export interface DavConfig { url: string; remoteDirectory: string; username: string; hasCredential: boolean; cleanupPending?: boolean }
export type DavConfigSaveStatus = 'not_found' | 'pending' | 'aborted' | 'aborted_retained_runs' | 'committed' | 'committed_cleanup_pending';
export interface DavVersion { id: string; timestamp: string | null; latest: boolean }
export interface DavVersionPage { versions: DavVersion[]; nextCursor: string | null }
export type DavSnapshotState = 'structurally_complete' | 'missing_manifest' | 'invalid_manifest' | 'unsupported_format' | 'incomplete' | 'missing_connections' | 'missing_credentials' | 'integrity_mismatch' | 'unavailable' | 'legacy' | 'legacy_unconfirmed';
export interface DavSnapshot { id:string; directory:string; name:string; createdAt:string|null; appVersion:string|null; formatVersion:number|null; sizeBytes:number|null; protection:'none'|'encrypted'|'unknown'; containsCredentials:boolean|null; state:DavSnapshotState; reason:string|null; restorable:boolean; source:'independent'|'legacy' }
export interface DavSnapshotPage { snapshots:DavSnapshot[]; nextCursor:string|null; scannedCount?:number }
export interface DavConnection { id: string; name: string; host: string; port: number; username: string; authentication: string; group_id: string | null; notes: string; production: boolean; always_confirm: boolean; version: number }
export interface DavGroup { id: string; name: string; parent_id: string | null }
export interface DavPreview { token: string; snapshotId?:string|null; manifestSha256?:string|null; protection: 'none'|'encrypted'; connections: { id: string; name: string; status: 'new' | 'unchanged' | 'conflict'; backup: DavConnection; local: DavConnection | null; backupHasSecret:boolean; localHasSecret:boolean }[]; groups: { id: string; status: 'new' | 'unchanged' | 'conflict'; backup: DavGroup; local: DavGroup | null }[]; hostKeyCount: number }
export interface DavChoice { id: string; action: 'keep' | 'replace' | 'copy'; copyId?: string; copyName?: string }

export type SshPasswordState = 'available' | 'missing' | 'unavailable';
export type SshOpenResult = { status: "connected"; sessionId: string; passwordSaveToken?: string } | ({ status: "host_key_confirmation" } & HostKeyChallenge);
export interface SessionReadResult { output: string; closed: boolean }
export type SessionSubmitResult = { status: "sent" } | { status: "confirmation_required"; token: string; risk: RiskLevel; categories: string[] };

// Shared Rust DTOs use camelCase fields and snake_case enum values on the wire.
export type AuthenticationMethod = "password" | "private_key" | "ssh_agent";

/** Public connection data; secret material and secret-store identifiers are excluded. */
export interface ConnectionProfile {
  id: string;
  name: string;
  host: string;
  port: number;
  username: string;
  authentication: AuthenticationMethod;
  groupId: string | null;
  notes: string;
  production: boolean;
  alwaysConfirm: boolean;
  version: number;
}

export interface ConnectionGroup {
  id: string;
  name: string;
  parentId: string | null;
}

export type ConnectionTestPhase = "dns" | "tcp" | "ssh_handshake" | "host_verification" | "authentication";

export interface ConnectionTestResult {
  operationId: string;
  phase: ConnectionTestPhase;
  state: OperationState;
  serverIdentity: string | null;
  error: ErrorEnvelope | null;
}

export interface HostKeyChallenge {
  host: string;
  port: number;
  algorithm: string;
  fingerprint: string;
}

export type SessionState = "connecting" | "connected" | "disconnected" | "failed" | "closing" | "close_unknown" | "closed";

export interface SessionInfo {
  sessionId: string;
  connectionId: string;
  state: SessionState;
  production: boolean;
}

export type SessionEvent = { sessionId: string; sequence: number } & (
  | { kind: "output"; payload: { data: number[] } }
  | { kind: "state"; payload: { state: SessionState } }
  | { kind: "error"; payload: { error: ErrorEnvelope } }
  | { kind: "truncated"; payload: { discardedBytes: number } }
);

export interface Metric {
  value: number | null;
  unit: string;
  unavailableReason: string | null;
}

export interface ServerOverviewSnapshot {
  connectionId: string;
  collectedAt: string;
  online: boolean;
  hostname: string | null;
  operatingSystem: string | null;
  kernel: string | null;
  architecture: string | null;
  bootTime: string | null;
  uptime: Metric | null;
  cpuUsage: Metric | null;
  logicalCores: Metric | null;
  memoryTotal: Metric | null;
  memoryUsed: Metric | null;
  load: Metric | null;
  diskTotal: Metric | null;
  diskUsed: Metric | null;
  swapTotal: Metric | null;
  swapUsed: Metric | null;
}

export type HistoryOutcome = "running" | "succeeded" | "failed" | "unknown";

export interface CommandHistoryEntry {
  id: string;
  connectionId: string;
  connectionName: string;
  production: boolean;
  command: string;
  submittedAt: string;
  outcome: HistoryOutcome;
  exitCode: number | null;
  durationMs: number | null;
}

export type RiskLevel = "safe" | "dangerous" | "unknown";

/** Span positions are UTF-8 byte offsets, not JavaScript string indices. */
export interface DisplaySpan {
  start: number;
  end: number;
}

export interface RiskAssessment {
  level: RiskLevel;
  categories: string[];
  ruleVersion: string;
  displaySpans: DisplaySpan[];
  redactedDigest: string;
}

/** Request only: command text can contain sensitive values and must not be logged. */
export interface CommandIntent {
  sessionId: string;
  connectionId: string;
  connectionVersion: number;
  command: string;
  operationId: string;
}

export type OperationState = "queued" | "running" | "succeeded" | "failed" | "cancelled" | "unknown";
export type OperationOutcome = "not_started" | "completed" | "failed" | "cancelled" | "unknown";

export interface ErrorEnvelope {
  code: string;
  messageKey: string;
  safeDetail: string | null;
  operationId: string | null;
  outcome: OperationOutcome;
  retryable: boolean;
}

export interface OperationStatus {
  operationId: string;
  state: OperationState;
  error: ErrorEnvelope | null;
}

export interface BackupScope {
  includeCredentials: boolean;
}

export interface BackupInfo {
  id: string;
  formatVersion: number;
  createdAt: string;
  includesCredentials: boolean;
}

/** WebDAV credentials are handled separately from the returned profile. */
export interface WebDavProfile {
  id: string;
  url: string;
  remoteDirectory: string;
  username: string;
  autoBackup: boolean;
}


export type RestoreAction = "keep_local" | "use_backup" | "create_copy" | "skip";
export type RestoreDisposition = "new" | "update" | "conflict" | "skipped";

export interface RestoreItem {
  id: string;
  action: RestoreAction;
}

export interface RestorePlan {
  backupId: string;
  items: RestoreItem[];
}

export interface RestorePreviewItem {
  id: string;
  name: string;
  disposition: RestoreDisposition;
}

export interface RestorePreview {
  backupId: string;
  includesCredentials: boolean;
  connections: RestorePreviewItem[];
  groups: RestorePreviewItem[];
}

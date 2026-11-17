<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, watch, type Directive } from "vue";
import { connectionCopy, connectionDelete, connectionList, connectionSave, groupDelete, groupList, groupSave, serverOverview, serverOverviewCancel, sessionClose, sessionRead, sessionSubmit, sessionWrite, sessionResize, sshCancel, sshOpen, sshKeySaved, sshKeySave, sshKeyDelete, sshKeyOperationStatus, sessionRunSaved } from "@/lib/backend/api";
import type { ConnectionGroup, ConnectionProfile, HostKeyChallenge, Metric, ServerOverviewSnapshot, SessionSubmitResult, SavedCommand } from "@/lib/backend/types";
import TerminalView from './TerminalView.vue';
import RemoteFilesPanel from './RemoteFilesPanel.vue';
import SavedCommandsPanel from './SavedCommandsPanel.vue';
import WorkbenchLayout from './WorkbenchLayout.vue';
import { SquareTerminal, FolderOpen } from '@lucide/vue';
import { editTerminalLine } from '@/lib/backend/terminal-input';

const props = withDefaults(defineProps<{ initialProfile?: ConnectionProfile; active?: boolean }>(), { active: true });
const emit = defineEmits<{ connected: [value: { id: string; profile: ConnectionProfile }]; closed: []; disconnected: [] }>();

const connections = ref<ConnectionProfile[]>([]);
const groups = ref<ConnectionGroup[]>([]);
const search = ref("");
const groupFilter = ref("all");
const groupDialog = ref(false);
const groupName = ref("");
const groupEditing = ref<ConnectionGroup | null>(null);
const deletingGroup = ref<ConnectionGroup | null>(null);
const selectedGroup = computed(() => groups.value.find((group) => group.id === groupFilter.value));
const deleting = ref<ConnectionProfile | null>(null);
const copying = ref<ConnectionProfile | null>(null);
const copyName = ref("");
const filteredConnections = computed(() => {
  const term = search.value.trim().toLocaleLowerCase();
  return connections.value.filter((item) => (groupFilter.value === "all" || (groupFilter.value === "ungrouped" ? !item.groupId : item.groupId === groupFilter.value)) && (!term || [item.name, item.host, item.username].some((value) => value.toLocaleLowerCase().includes(term))));
});
const selected = ref<ConnectionProfile | null>(null);
const editor = ref<ConnectionProfile | null>(null);
const password = ref("");
const privateKey = ref("");
const passphrase = ref("");
const keySaved = ref(false);
const useSavedKey = ref(false);
const saveKey = ref(false);
const savingKey = ref(false);
const connectDialog = ref(false);
const challenge = ref<HostKeyChallenge | null>(null);
const busy = ref(false);
const error = ref("");
const sessionId = ref("");
const command = ref("");
const terminalView = ref<InstanceType<typeof TerminalView> | null>(null);
const layout = ref<InstanceType<typeof WorkbenchLayout> | null>(null);
const commandsPanel = ref<InstanceType<typeof SavedCommandsPanel> | null>(null);
const workspaceRoot = ref<HTMLElement>();
async function createCommand() { layout.value?.openCommands(); await nextTick(); await commandsPanel.value?.create(); }
const savedPending=ref<{command:SavedCommand;token:string;risk:string;categories:string[];session:string;generation:number}|null>(null);
const pastePending = ref('');
let writeQueue: Promise<void> = Promise.resolve();
let queuedBytes = 0;
const MAX_QUEUED_BYTES = 64 * 1024;
let resizeRunning = false;
let pendingResize: { id: string; generation: number; cols: number; rows: number } | undefined;
const pending = ref<Extract<SessionSubmitResult, { status: "confirmation_required" }> | null>(null);
const pendingCommand = ref("");
const disconnected = ref(false);
const closing = ref(false);
const closeUnknown = ref(false);
let pollTimer: ReturnType<typeof setTimeout> | undefined;
let disposed = false;
let openGeneration = 0;
let sessionGeneration = 0;
let openingOperation = "";
const overview = ref<ServerOverviewSnapshot | null>(null);
const overviewLoading = ref(false);
const overviewError = ref("");
const autoRefresh = ref(true);
const overviewStale = ref(false);
let overviewLastSuccess = 0;
let overviewTimer: ReturnType<typeof setInterval> | undefined;
let overviewRequest: { session: string; operation: string } | undefined;
let overviewGeneration = 0;

function cancelOverview() {
  overviewGeneration++;
  const request = overviewRequest; overviewRequest = undefined; overviewLoading.value = false;
  if (request) void serverOverviewCancel(request.session, request.operation).catch(() => {});
}
async function refreshOverview() {
  if (!sessionId.value || disconnected.value || overviewLoading.value) return;
  const session = sessionId.value;
  const request = { session, operation: crypto.randomUUID() };
  const generation = ++overviewGeneration;
  overviewRequest = request; overviewLoading.value = true; overviewError.value = "";
  try {
    const value = await serverOverview(session, request.operation);
    if (!disposed && generation === overviewGeneration && sessionId.value === session && !disconnected.value && value.connectionId === selected.value?.id) { overview.value = value; overviewLastSuccess = Date.now(); overviewStale.value = !value.online; }
  } catch (e) {
    if (!disposed && generation === overviewGeneration) overviewError.value = message(e);
  } finally {
    if (generation === overviewGeneration) { overviewRequest = undefined; overviewLoading.value = false; }
  }
}
function metricText(metric: Metric | null | undefined): string {
  if (!metric || metric.value === null) return `不可用${metric?.unavailableReason ? `（${metric.unavailableReason}）` : ""}`;
  return `${metric.value} ${metric.unit}`.trim();
}
watch([sessionId, disconnected], ([id, offline], [previous]) => {
  if (offline) { pastePending.value = ''; if (id) emit('disconnected'); }
  cancelOverview();
  if (overviewTimer) clearInterval(overviewTimer); overviewTimer = undefined;
  if (id !== previous) { overview.value = null; overviewError.value = ""; overviewLastSuccess = 0; overviewStale.value = false; }
  if (id && !offline) {
    void refreshOverview();
    overviewTimer = setInterval(() => {
      if (overview.value && Date.now() - overviewLastSuccess >= 45_000) overviewStale.value = true;
      if (autoRefresh.value) void refreshOverview();
    }, 15_000);
  }
});
watch(() => props.active, async (active) => {
  if (!active) return;
  void load();
  await nextTick();
  if (!props.active) return;
  const dialog = workspaceRoot.value?.querySelector<HTMLElement>('.modal-backdrop, [role="alertdialog"]');
  if (dialog) (dialog.querySelector<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled)') ?? dialog).focus();
  else if (sessionId.value) terminalView.value?.activate();
});

const vFocusDialog: Directive<HTMLElement> = {
  mounted(element, binding) {
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    if (!element.hasAttribute("tabindex")) element.tabIndex = -1;
    const focusable = () => Array.from(element.querySelectorAll<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex="0"]'));
    const focusin = (event: FocusEvent) => {
      if (element.closest('[data-workspace-active="false"]')) return;
      const topModal = document.querySelector('[data-workspace-active="true"] .modal-backdrop');
      if (topModal && topModal !== element && !topModal.contains(element)) return;
      if (element.isConnected && event.target instanceof Node && !element.contains(event.target)) (focusable()[0] ?? element).focus();
    };
    const keydown = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); if (typeof binding.value === "function") binding.value(); else element.click(); return; }
      if (event.key !== "Tab") return;
      const items = focusable(); if (!items.length) { event.preventDefault(); return; }
      const first = items[0]!; const last = items[items.length - 1]!;
      if (event.shiftKey && (document.activeElement === first || !element.contains(document.activeElement))) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && (document.activeElement === last || !element.contains(document.activeElement))) { event.preventDefault(); first.focus(); }
    };
    element.addEventListener("keydown", keydown);
    document.addEventListener("focusin", focusin);
    (element as HTMLElement & { cleanupDialog?: () => void }).cleanupDialog = () => { element.removeEventListener("keydown", keydown); document.removeEventListener("focusin", focusin); if (!element.closest('[data-workspace-active="false"]') && previous?.isConnected) previous.focus(); };
    queueMicrotask(() => { if (element.isConnected && !element.closest('[data-workspace-active="false"]')) focusable()[0]?.focus(); });
  },
  updated(element) { queueMicrotask(() => { if (element.isConnected && !element.closest('[data-workspace-active="false"]') && !element.contains(document.activeElement)) element.querySelector<HTMLElement>('button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled)')?.focus(); }); },
  unmounted(element) { (element as HTMLElement & { cleanupDialog?: () => void }).cleanupDialog?.(); },
};

function message(value: unknown): string {
  if (value === "CONNECTION_ACTIVE") return "该连接存在活动会话，请先关闭会话。";
  if (value === "CONNECTION_HAS_SECRETS") return "该连接仍有关联凭据，暂无法删除。";
  if (value === "CONNECTION_CONFLICT") return "名称或分组冲突，请检查后重试。";
  if (typeof value === "string") return value;
  if (value instanceof Error) return value.message;
  if (value && typeof value === "object" && "safeDetail" in value && typeof value.safeDetail === "string") return value.safeDetail;
  return "操作失败，请检查连接信息后重试。";
}
let loadGeneration = 0;
async function load() {
  const generation = ++loadGeneration;
  try { const [items, availableGroups] = await Promise.all([connectionList(), groupList()]); if (!disposed && generation === loadGeneration) { connections.value = items; groups.value = availableGroups; } }
  catch (e) { if (!disposed && generation === loadGeneration) error.value = message(e); }
}
async function saveGroup() {
  if (!groupName.value.trim() || busy.value) return;
  busy.value = true; error.value = "";
  try { await groupSave({ id: groupEditing.value?.id ?? crypto.randomUUID(), name: groupName.value.trim(), parentId: groupEditing.value?.parentId ?? null }); groupName.value = ""; groupEditing.value = null; groupDialog.value = false; await load(); }
  catch (e) { error.value = message(e); } finally { busy.value = false; }
}
function requestGroupEdit(group?: ConnectionGroup) { groupEditing.value = group ?? null; groupName.value = group?.name ?? ""; groupDialog.value = true; error.value = ""; }
async function removeGroup() {
  if (!deletingGroup.value || busy.value) return;
  busy.value = true; error.value = "";
  try { await groupDelete(deletingGroup.value.id); deletingGroup.value = null; groupFilter.value = "all"; await load(); }
  catch (e) { error.value = message(e); } finally { busy.value = false; }
}
function requestCopy(item: ConnectionProfile) { copying.value = item; copyName.value = `${item.name} 副本`; error.value = ""; }
async function copy() {
  if (busy.value || !copying.value || !copyName.value.trim()) return;
  busy.value = true; error.value = "";
  try { await connectionCopy(copying.value.id, crypto.randomUUID(), copyName.value.trim()); copying.value = null; await load(); }
  catch (e) { error.value = message(e); } finally { busy.value = false; }
}
async function remove() {
  if (!deleting.value || busy.value) return;
  busy.value = true; error.value = "";
  try { await connectionDelete(deleting.value.id); deleting.value = null; await load(); }
  catch (e) { error.value = message(e); } finally { busy.value = false; }
}
function create() {
  editor.value = { id: crypto.randomUUID(), name: "", host: "", port: 22, username: "", authentication: "password", groupId: null, notes: "", production: false, alwaysConfirm: false, version: 0 };
  error.value = "";
}
function edit(item: ConnectionProfile) { editor.value = { ...item }; error.value = ""; }
async function save() {
  if (!editor.value) return;
  busy.value = true; error.value = "";
  try { await connectionSave({ ...editor.value, port: Number(editor.value.port) }); editor.value = null; await load(); }
  catch (e) { error.value = message(e); }
  finally { busy.value = false; }
}
function clearSecrets() { password.value = ""; privateKey.value = ""; passphrase.value = ""; }
function requestConnect(item: ConnectionProfile) {
  const lookupGeneration = ++openGeneration;
  selected.value = item; clearSecrets(); keySaved.value = false; useSavedKey.value = false; saveKey.value = false; challenge.value = null; connectDialog.value = true; error.value = "";
  if (item.authentication === 'private_key') void sshKeySaved(item.id).then(saved => { if (lookupGeneration === openGeneration && selected.value?.id === item.id && connectDialog.value) { keySaved.value = saved; useSavedKey.value = saved; } }).catch(e => { if (lookupGeneration === openGeneration && selected.value?.id === item.id && connectDialog.value) error.value = message(e); });
}
async function forgetKey() { if (!selected.value || busy.value) return; busy.value = true; savingKey.value = true; const id = selected.value.id; try { await sshKeyDelete(id); keySaved.value = false; useSavedKey.value = false; } catch (e) { error.value = message(e); if (error.value.includes('结果未知')) { try { keySaved.value = await sshKeySaved(id); useSavedKey.value = keySaved.value; error.value += keySaved.value ? ' 当前仍有已保存私钥。' : ' 当前已无已保存私钥。'; } catch { error.value += ' 状态暂不可确认，请稍后重试。'; } } } finally { savingKey.value = false; busy.value = false; } }
function cancelConnect() { if (savingKey.value && !disposed) return; openGeneration++; const operation = openingOperation; openingOperation = ""; if (operation) void sshCancel(operation).catch(() => {}); connectDialog.value = false; challenge.value = null; clearSecrets(); busy.value = false; }
async function open(fingerprint?: string) {
  if (!selected.value || busy.value) return;
  const generation = ++openGeneration;
  const operation = crypto.randomUUID(); openingOperation = operation;
  busy.value = true; error.value = "";
  try {
    if (selected.value.authentication === 'private_key' && saveKey.value && !useSavedKey.value) { savingKey.value = true; try { await sshKeySave(selected.value.id, privateKey.value, passphrase.value || null); } catch (e) { const detail = message(e); const operationId = detail.match(/操作标识：([A-Za-z0-9_-]+)/)?.[1]; if (!operationId) throw e; const status = await sshKeyOperationStatus(selected.value.id, operationId); if (!['committed', 'committed_cleanup_pending'].includes(status)) throw new Error(`密钥保存状态：${status}。请稍后检查，操作标识：${operationId}`); } finally { savingKey.value = false; } if (disposed || generation !== openGeneration || !selected.value) return; keySaved.value = true; saveKey.value = false; }
    const result = await sshOpen(selected.value.id, selected.value.authentication === "password" ? password.value : null, operation, fingerprint, selected.value.authentication === "private_key" && !useSavedKey.value ? privateKey.value : null, selected.value.authentication === "private_key" && !useSavedKey.value ? passphrase.value : null, useSavedKey.value);
    if (disposed || generation !== openGeneration) {
      if (result.status === "connected") void sessionClose(result.sessionId).catch(() => {});
      return;
    }
    if (result.status === "host_key_confirmation") { challenge.value = result; return; }
    clearSecrets(); challenge.value = null; connectDialog.value = false;
    sessionGeneration++; sessionId.value = result.sessionId; disconnected.value = false; closeUnknown.value = false;
    emit('connected', { id: result.sessionId, profile: selected.value });
    await nextTick();
    if (selected.value?.production) terminalView.value?.write('$ ');
    else void poll(result.sessionId);
  } catch (e) { if (!disposed && generation === openGeneration) { clearSecrets(); challenge.value = null; error.value = message(e); } }
  finally { if (openingOperation === operation) openingOperation = ""; if (generation === openGeneration) busy.value = false; }
}
async function poll(id: string) {
  if (disposed || sessionId.value !== id || disconnected.value) return;
  const generation = sessionGeneration;
  try {
    const result = await sessionRead(id);
    if (disposed || sessionId.value !== id || generation !== sessionGeneration || disconnected.value) return;
    if (result.output) {
      terminalView.value?.write(selected.value?.production ? result.output.replace(/(?<!\r)\n/g, '\r\n') : result.output);
      // Wait for xterm to consume the chunk before asking for more, keeping
      // sustained output bounded without adding a delay to each chunk.
      if (terminalView.value && !await terminalView.value.ready()) { disconnected.value = true; error.value = '终端显示状态不一致，请关闭会话并重新连接。'; return; }
      if (disposed || sessionId.value !== id || generation !== sessionGeneration || disconnected.value) return;
    }
    if (result.closed) { disconnected.value = true; return; }
    // PTY read already waits up to one second; keep a small yield for empty
    // replies. Production exec reads return immediately when idle.
    if (result.output) void poll(id);
    else pollTimer = setTimeout(() => void poll(id), selected.value?.production ? 300 : 10);
  } catch (e) { if (!disposed && sessionId.value === id && generation === sessionGeneration) { disconnected.value = true; error.value = message(e); } }
}
function sendRaw(data: string) {
  const id = sessionId.value; const generation = sessionGeneration;
  const size = new TextEncoder().encode(data).length;
  if (size + queuedBytes > MAX_QUEUED_BYTES) { disconnected.value = true; error.value = '终端输入积压过多，请关闭会话并重新连接。'; return; }
  queuedBytes += size;
  writeQueue = writeQueue.then(async () => {
    if (disposed || !id || id !== sessionId.value || generation !== sessionGeneration || disconnected.value) return;
    try { await sessionWrite(id, data); }
    catch (e) { if (id === sessionId.value && generation === sessionGeneration) { disconnected.value = true; error.value = `${message(e)}。请关闭会话并重新连接。`; } }
  }).finally(() => { queuedBytes -= size; });
}
function terminalInput(data: string) {
  if (!props.active || !sessionId.value || disconnected.value || busy.value || pending.value || savedPending.value) return;
  if (!selected.value?.production && !selected.value?.alwaysConfirm) { sendRaw(data); return; }
  const before = command.value; const next = editTerminalLine(before, data);
  if (next.submitted) { if (before) { terminalView.value?.write('\r\n'); void submit(false); } return; }
  command.value = next.line;
  if (next.line.length < before.length) terminalView.value?.eraseLast(before.slice(next.line.length));
  else if (next.line.startsWith(before)) terminalView.value?.write(next.line.slice(before.length));
}
function terminalPaste(data: string) {
  if (!props.active || !data) return;
  if (new TextEncoder().encode(data).length > MAX_QUEUED_BYTES) { error.value = '粘贴内容过大，请分批输入。'; return; }
  if (/[\r\n]/.test(data)) { pastePending.value = data; return; }
  terminalInput(data);
}
function acceptPaste() {
  const value = pastePending.value; pastePending.value = '';
  if (!selected.value?.production && !selected.value?.alwaysConfirm) sendRaw(value);
  else { const safe = value.replace(/\r\n?/g, '\n').replace(/[\x00-\x09\x0b-\x1f\x7f]/g, ''); command.value += safe; terminalView.value?.write(safe.replace(/\n/g, '\r\n')); }
  terminalView.value?.focus();
}
function resizeTerminal(cols: number, rows: number) {
  const id = sessionId.value;
  if (!id || disconnected.value || selected.value?.production) return;
  pendingResize = { id, generation: sessionGeneration, cols, rows };
  if (resizeRunning) return;
  resizeRunning = true;
  void (async () => {
    try {
      while (pendingResize) {
        const next = pendingResize; pendingResize = undefined;
        if (next.id !== sessionId.value || next.generation !== sessionGeneration || disconnected.value) continue;
        try { await sessionResize(next.id, next.cols, next.rows); } catch { /* next resize may recover */ }
      }
    } finally { resizeRunning = false; }
  })();
}
watch(command, () => { pending.value = null; pendingCommand.value = ""; });
async function submit(confirmed = false) {
  if (!sessionId.value || !command.value || busy.value || disconnected.value) return;
  const current = command.value;
  const id = sessionId.value;
  const generation = sessionGeneration;
  if (confirmed && (!pending.value || pendingCommand.value !== current)) return;
  busy.value = true; error.value = "";
  try {
    if (!await terminalView.value?.ready()) { disconnected.value = true; error.value = '终端输入显示状态不一致，请关闭会话并重新连接。'; return; }
    if (generation !== sessionGeneration || id !== sessionId.value || disposed) return;
    const result = await sessionSubmit(id, current, confirmed ? pending.value?.token : undefined);
    if (generation !== sessionGeneration || id !== sessionId.value || disposed) return;
    if (result.status === "confirmation_required") { if (command.value === current) { pending.value = result; pendingCommand.value = current; } }
    else {
      pending.value = null; pendingCommand.value = ""; if (command.value === current) command.value = "";
      if (selected.value?.production) {
        // Production exec is complete before submit returns; drain its result
        // before drawing the next prompt, so output cannot appear after it.
        let result;
        try { result = await sessionRead(id); }
        catch { if (!disposed && generation === sessionGeneration && id === sessionId.value) { disconnected.value = true; error.value = '命令已发送，但读取结果失败，请关闭会话并重新连接。'; } return; }
        if (generation !== sessionGeneration || id !== sessionId.value || disposed) return;
        if (result.output) terminalView.value?.write(result.output.replace(/(?<!\r)\n/g, '\r\n'));
        if (result.closed) { disconnected.value = true; return; }
        terminalView.value?.write('$ ');
      }
    }
  } catch (e) { if (generation === sessionGeneration && id === sessionId.value) { pending.value = null; const detail = message(e); if (detail.includes("发送结果未知") || detail.includes("执行结果未知")) { disconnected.value = true; error.value = `${detail}。请关闭会话并重新连接。`; } else error.value = detail; } }
  finally { if (generation === sessionGeneration && id === sessionId.value) busy.value = false; }
}
async function runSavedCommand(item:SavedCommand,confirmed=false) {
  if(!sessionId.value||disconnected.value||busy.value||pending.value||pastePending.value||command.value){error.value='请先完成或取消当前输入，再运行保存命令。';return;}
  const id=sessionId.value;const generation=sessionGeneration;const prior=savedPending.value;
  if(confirmed&&(!prior||prior.command.id!==item.id||prior.session!==id||prior.generation!==generation))return;
  busy.value=true;error.value='';
  try { const result=await sessionRunSaved(id,item.id,item.version,confirmed?prior?.token:undefined);if(disposed||id!==sessionId.value||generation!==sessionGeneration)return;
    if(result.status==='confirmation_required'){savedPending.value={command:item,token:result.token,risk:result.risk,categories:result.categories,session:id,generation};}
    else {savedPending.value=null;terminalView.value?.write(`\r\n[保存命令：${item.name}]\r\n${(result.output??'').replace(/(?<!\r)\n/g,'\r\n')}\r\n`);if(selected.value?.production)terminalView.value?.write('$ ');}
  }catch(e){if(!disposed&&id===sessionId.value&&generation===sessionGeneration){savedPending.value=null;error.value=message(e);if(error.value.includes('结果未知'))disconnected.value=true;}}
  finally{if(id===sessionId.value&&generation===sessionGeneration)busy.value=false;}
}
watch([sessionId,disconnected],()=>{savedPending.value=null;});
async function close() {
  if (closing.value) return false;
  if (savingKey.value) { error.value = '密钥操作进行中，请等待结果后关闭标签。'; return false; }
  if (!sessionId.value) return true;
  const id = sessionId.value; sessionGeneration++; pending.value = null; pastePending.value = ''; command.value = ""; busy.value = false; closing.value = true; disconnected.value = true;
  if (pollTimer) clearTimeout(pollTimer);
  try { await sessionClose(id); if (sessionId.value === id) { sessionId.value = ""; closeUnknown.value = false; emit('closed'); } return true; }
  catch (e) { if (sessionId.value === id) { closeUnknown.value = true; error.value = message(e); } return false; }
  finally { closing.value = false; }
}
async function disconnect() {
  if (closing.value || !sessionId.value) return false;
  const id = sessionId.value; sessionGeneration++; pending.value = null; pastePending.value = ''; command.value = ''; disconnected.value = true; closing.value = true;
  if (pollTimer) clearTimeout(pollTimer);
  try { await sessionClose(id); closeUnknown.value = false; emit('disconnected'); return true; }
  catch (e) { closeUnknown.value = true; error.value = message(e); return false; }
  finally { closing.value = false; }
}
defineExpose({ close, disconnect, requestConnect, create, createCommand });
onMounted(() => { void load(); if (props.initialProfile) requestConnect(props.initialProfile); });
onUnmounted(() => { disposed = true; sessionGeneration++; pastePending.value = ''; cancelOverview(); if (overviewTimer) clearInterval(overviewTimer); cancelConnect(); if (pollTimer) clearTimeout(pollTimer); if (sessionId.value) void sessionClose(sessionId.value).catch(() => {}); });
</script>

<template>
  <section ref="workspaceRoot" class="connections-page" :data-workspace-active="active" :inert="!active">
    <p v-if="error" class="inline-error workspace-error" role="alert">{{ error }}</p>
    <WorkbenchLayout ref="layout">
      <template #connections>
<section class="connections-card">
        <div class="connections-card__heading"><span>{{ filteredConnections.length }} 个连接</span><button data-test="new" type="button" aria-label="新建连接" title="新建连接" @click="create">＋</button></div>
        <div class="connection-toolbar"><label>搜索<input v-model="search" data-test="search" type="search" placeholder="名称、主机或用户名" /></label><label>分组<select v-model="groupFilter" data-test="group-filter"><option value="all">全部分组</option><option value="ungrouped">未分组</option><option v-for="group in groups" :key="group.id" :value="group.id">{{ group.name }}</option></select></label><button data-test="new-group" type="button" :disabled="busy" @click="requestGroupEdit()">新建分组</button><button v-if="selectedGroup" data-test="edit-group" type="button" :disabled="busy" @click="requestGroupEdit(selectedGroup)">编辑分组</button><button v-if="selectedGroup" data-test="delete-group" type="button" :disabled="busy" @click="deletingGroup = selectedGroup; error = ''">删除分组</button></div>
      <section class="connection-list" aria-label="连接列表">
        <div v-if="filteredConnections.length" class="connection-list__labels" aria-hidden="true"><span>名称 / 主机</span><span>操作</span></div>
        <p v-if="connections.length === 0" class="empty-state">尚无连接，点击“新建连接”开始。</p>
        <p v-else-if="filteredConnections.length === 0" class="empty-state">没有匹配的连接。</p>
        <article v-for="item in filteredConnections" :key="item.id" class="connection-row" :class="{ 'is-selected': selected?.id === item.id }" @dblclick="!sessionId && !busy && requestConnect(item)">
          <div class="connection-info"><strong>{{ item.name }}</strong><span v-if="item.production" class="production-badge">生产</span><small>{{ item.username }}@{{ item.host }}:{{ item.port }} · {{ groups.find(g => g.id === item.groupId)?.name || '未分组' }}</small></div>
          <div class="row-actions"><button type="button" :disabled="!!sessionId || busy" title="关闭会话后可编辑连接" @click="edit(item)">编辑</button><button data-test="copy" type="button" :disabled="busy" @click="requestCopy(item)">复制</button><button data-test="delete" type="button" :disabled="busy" @click="deleting = item">删除</button><button class="primary-button" data-test="connect" type="button" :disabled="!!sessionId || busy" @click="requestConnect(item)">连接</button></div>
        </article>
      </section>
      </section>
      </template>
      <template #overview>
<aside class="server-panel" aria-label="服务器概览">
        <div class="server-panel__heading"><span>服务器概览</span><span class="server-state" :class="{ 'server-state--online': sessionId && !disconnected }">{{ sessionId ? (disconnected ? '已断开' : '已连接') : '未连接' }}</span></div>
        <div class="server-identity"><div class="server-identity__icon" aria-hidden="true">›_</div><div><strong>{{ sessionId ? selected?.name : '尚未连接服务器' }}</strong><small>{{ sessionId ? `${selected?.username}@${selected?.host}:${selected?.port}` : '选择连接以开始会话' }}</small></div></div>
        <div class="server-details"><div><span>主机</span><strong>{{ sessionId ? selected?.host : '—' }}</strong></div><div><span>用户</span><strong>{{ sessionId ? selected?.username : '—' }}</strong></div><div><span>环境</span><strong>{{ sessionId ? (selected?.production ? '生产环境' : '普通环境') : '—' }}</strong></div></div>
        <div class="server-panel__section"><div class="server-panel__section-heading"><strong>系统信息</strong><button type="button" data-test="refresh-overview" :disabled="!sessionId || disconnected || overviewLoading" @click="refreshOverview">{{ overviewLoading ? '刷新中' : '刷新' }}</button></div><label class="overview-auto"><input v-model="autoRefresh" type="checkbox" /> 每 15 秒自动刷新</label><span>运行时间、负载与资源使用</span>
          <template v-if="overview"><p v-if="disconnected || overviewError || overviewStale" class="overview-notice">{{ disconnected ? '连接已断开，以下为上次采集数据' : overviewError ? '刷新失败，以下为上次采集数据' : '数据已过期，请刷新' }}</p><div class="overview-values"><div><span>系统</span><strong>{{ overview.operatingSystem || '不可用' }}</strong></div><div><span>运行时间</span><strong>{{ metricText(overview.uptime) }}</strong></div><div><span>CPU</span><strong>{{ metricText(overview.cpuUsage) }}</strong></div><div><span>内存</span><strong>{{ metricText(overview.memoryUsed) }} / {{ metricText(overview.memoryTotal) }}</strong></div><div><span>负载</span><strong>{{ metricText(overview.load) }}</strong></div><div><span>交换</span><strong>{{ metricText(overview.swapUsed) }} / {{ metricText(overview.swapTotal) }}</strong></div><div><span>磁盘</span><strong>{{ metricText(overview.diskUsed) }} / {{ metricText(overview.diskTotal) }}</strong></div></div></template>
          <p v-else>{{ overviewLoading ? '正在采集系统信息…' : sessionId ? '系统指标不可用' : '连接后可查看服务器状态' }}</p><p v-if="overviewError" class="overview-error" role="status">{{ overviewError }}</p>
        </div>
        <p class="server-panel__foot">{{ overview ? `采集时间：${overview.collectedAt}` : '指标通过独立连接通道采集。' }}</p>
      </aside>
      </template>
      <template #terminal>
<section v-if="sessionId" class="terminal-panel" aria-label="SSH 会话">
        <div class="terminal-heading"><div class="terminal-heading__title"><span class="connection-dot" :class="{ 'connection-dot--offline': disconnected }" aria-hidden="true"></span><strong>{{ selected?.name }}</strong><span v-if="selected?.production" class="production-badge">生产</span><span>{{ closeUnknown ? '关闭结果未知' : disconnected ? '已断开' : '已连接' }}</span></div><button type="button" :disabled="closing" @click="close">{{ closing ? '正在关闭' : closeUnknown ? '重试关闭' : '关闭会话' }}</button></div>
        <TerminalView ref="terminalView" :active="active" :disabled="disconnected || busy || !!pending || !!savedPending || !!pastePending" @data="terminalInput" @paste="terminalPaste" @resize="resizeTerminal" />
        <p v-if="!selected?.production && !selected?.alwaysConfirm" class="terminal-hint">在终端内直接输入；多行粘贴需要确认。</p>
        <p v-else-if="!selected?.production" class="terminal-hint">本连接启用命令确认，在终端内本地编辑整行；远程行编辑及 Tab 补全不可用。</p>
        <p v-else class="warning-box" role="status">生产连接每条命令独立执行，不保留工作目录和环境变量等 Shell 状态。命令结束后显示输出；不支持交互式程序、远程行编辑或 Tab 补全。执行期间可关闭会话以中断本地等待，但远端命令结果未知；超时或发生错误后需重新连接。</p>
        <div v-if="pastePending" v-focus-dialog="() => { pastePending = ''; terminalView?.focus() }" class="warning-box" role="alertdialog" aria-label="确认多行粘贴"><strong>确认粘贴多行文本</strong><pre>{{ pastePending.slice(0, 500) }}</pre><button type="button" @click="pastePending = ''; terminalView?.focus()">取消</button><button type="button" @click="acceptPaste">确认粘贴</button></div>
        <div v-if="pending" v-focus-dialog="() => { if (!busy) pending = null }" class="warning-box" role="alertdialog" aria-label="确认命令" tabindex="-1">
          <strong>{{ pending.risk === 'dangerous' ? '危险命令' : '未知风险命令' }}</strong><p>连接：{{ selected?.name }} <span v-if="selected?.production">（生产）</span></p><pre>{{ pendingCommand }}</pre><p>风险类别：{{ pending.categories.join('、') || '无法确定' }}</p><p>请确认操作可能产生的影响。</p>
          <button data-test="cancel-command" type="button" :disabled="busy" @click="pending = null">取消</button><button class="danger-button" type="button" :disabled="busy" @click="submit(true)">确认执行</button>
        </div>
      </section>
        <section v-else class="workbench-empty" aria-label="终端待连接"><SquareTerminal :size="36" aria-hidden="true" /><h2>开始一个 SSH 会话</h2><p>从左侧选择连接，或点击顶部“新建连接”</p></section>
      </template>
      <template #files><RemoteFilesPanel v-if="sessionId" :session-id="sessionId" :disabled="disconnected||closing||busy" /><div v-else class="workbench-empty workbench-empty--files"><FolderOpen :size="28" aria-hidden="true" /><p>连接服务器后浏览远程文件</p></div></template>
      <template #commands><SavedCommandsPanel ref="commandsPanel" :disabled="!sessionId||disconnected||closing||busy||!!savedPending" @run="runSavedCommand" /></template>
    </WorkbenchLayout>
    <div v-if="savedPending" class="modal-backdrop"><section v-focus-dialog="()=>{if(!busy)savedPending=null}" class="dialog warning-box" role="alertdialog" aria-modal="true" aria-label="确认保存命令"><strong>{{savedPending.risk==='dangerous'?'危险命令':'未知风险命令'}}</strong><p>{{selected?.name}} {{selected?.production?'· 生产':''}} · {{savedPending.categories.join('、')}}</p><pre>{{savedPending.command.content}}</pre><p>将通过独立命令执行；确认后无法保证撤回远端操作。</p><button type="button" :disabled="busy" @click="savedPending=null">取消</button><button type="button" :disabled="busy" @click="runSavedCommand(savedPending.command,true)">确认执行</button></section></div>
    <div v-if="editor" v-focus-dialog class="modal-backdrop" @click.self="!busy && (editor = null)"><section class="dialog" role="dialog" aria-modal="true" aria-labelledby="editor-title"><h2 id="editor-title">{{ connections.some(c => c.id === editor?.id) ? '编辑连接' : '新建连接' }}</h2><p v-if="error" class="inline-error" role="alert">{{ error }}</p>
      <form @submit.prevent="save"><label>名称<input v-model.trim="editor.name" data-test="name" required maxlength="128" /></label><label>主机<input v-model.trim="editor.host" data-test="host" required maxlength="255" /></label><div class="form-row"><label>端口<input v-model.number="editor.port" type="number" min="1" max="65535" required /></label><label>用户名<input v-model.trim="editor.username" data-test="username" required /></label></div>
      <label>认证方式<select v-model="editor.authentication"><option value="password">密码</option><option value="private_key">私钥</option><option value="ssh_agent">SSH Agent</option></select></label><label>分组<select v-model="editor.groupId"><option :value="null">未分组</option><option v-for="group in groups" :key="group.id" :value="group.id">{{ group.name }}</option></select></label><label>备注<textarea v-model="editor.notes" rows="2"></textarea></label><label class="check-label"><input v-model="editor.production" data-test="production" type="checkbox" />生产环境</label><label class="check-label"><input v-model="editor.alwaysConfirm" type="checkbox" />始终确认危险或未知命令</label><div class="dialog-actions"><button type="button" :disabled="busy" @click="editor = null">取消</button><button class="primary-button" type="submit" :disabled="busy">保存</button></div></form></section></div>
    <div v-if="groupDialog" v-focus-dialog class="modal-backdrop" @click.self="!busy && (groupDialog = false)"><section class="dialog" role="dialog" aria-modal="true" aria-labelledby="group-title"><h2 id="group-title">{{ groupEditing ? '编辑分组' : '新建分组' }}</h2><p v-if="error" class="inline-error" role="alert">{{ error }}</p><form @submit.prevent="saveGroup"><label>分组名称<input v-model.trim="groupName" data-test="group-name" required maxlength="128" /></label><div class="dialog-actions"><button type="button" :disabled="busy" @click="groupDialog = false">取消</button><button class="primary-button" data-test="save-group" type="submit" :disabled="busy || !groupName.trim()">保存</button></div></form></section></div>
    <div v-if="deletingGroup" v-focus-dialog class="modal-backdrop" @click.self="!busy && (deletingGroup = null)"><section class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="delete-group-title"><h2 id="delete-group-title">删除分组</h2><p>确定删除分组“{{ deletingGroup.name }}”？关联连接将移至未分组；含子分组、正在连接或存在活动会话时无法删除，请先关闭相关会话。</p><p v-if="error" class="inline-error" role="alert">{{ error }}</p><div class="dialog-actions"><button type="button" :disabled="busy" @click="deletingGroup = null">取消</button><button class="danger-button" data-test="confirm-delete-group" type="button" :disabled="busy" @click="removeGroup">删除</button></div></section></div>
    <div v-if="copying" v-focus-dialog class="modal-backdrop" @click.self="!busy && (copying = null)"><section class="dialog" role="dialog" aria-modal="true" aria-labelledby="copy-title"><h2 id="copy-title">复制连接</h2><p>仅复制连接资料，不复制认证秘密。</p><p v-if="error" class="inline-error" role="alert">{{ error }}</p><form data-test="copy-form" @submit.prevent="copy"><label>新连接名称<input v-model.trim="copyName" data-test="copy-name" required maxlength="128" /></label><div class="dialog-actions"><button type="button" :disabled="busy" @click="copying = null">取消</button><button class="primary-button" type="submit" :disabled="busy || !copyName.trim()">复制</button></div></form></section></div>
    <div v-if="deleting" v-focus-dialog class="modal-backdrop" @click.self="!busy && (deleting = null)"><section class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="delete-title"><h2 id="delete-title">删除连接</h2><p>确定删除连接“{{ deleting.name }}”？此操作不可撤销。</p><p v-if="error" class="inline-error" role="alert">{{ error }}</p><div class="dialog-actions"><button data-test="cancel-delete" type="button" :disabled="busy" @click="deleting = null">取消</button><button class="danger-button" data-test="confirm-delete" type="button" :disabled="busy" @click="remove">删除</button></div></section></div>
    <div v-if="connectDialog" v-focus-dialog class="modal-backdrop" @click.self="cancelConnect"><section class="dialog" role="dialog" aria-modal="true" aria-labelledby="connect-title"><h2 id="connect-title">{{ challenge ? '确认服务器身份' : '连接服务器' }}</h2><p v-if="error" class="inline-error" role="alert">{{ error }}</p>
      <template v-if="challenge"><p>首次连接，请核对服务器指纹。确认前不会进行认证。</p><dl class="fingerprint"><dt>主机</dt><dd>{{ challenge.host }}:{{ challenge.port }}</dd><dt>算法</dt><dd>{{ challenge.algorithm }}</dd><dt>指纹</dt><dd>{{ challenge.fingerprint }}</dd></dl><div class="dialog-actions"><button data-test="reject-host" type="button" @click="cancelConnect">拒绝</button><button class="primary-button" type="button" :disabled="busy" @click="open(challenge.fingerprint)">接受并连接</button></div></template>
      <template v-else><p>{{ selected?.username }}@{{ selected?.host }}:{{ selected?.port }} <span v-if="selected?.production" class="production-badge">生产</span></p><label v-if="selected?.authentication === 'password'">密码<input v-model="password" data-test="password" type="password" autocomplete="off" /></label><template v-if="selected?.authentication === 'private_key'"><label v-if="keySaved" class="check-label"><input v-model="useSavedKey" type="checkbox" data-test="use-saved-key" />使用已保存的私钥</label><button v-if="keySaved" type="button" :disabled="busy" @click="forgetKey">删除已保存私钥</button><template v-if="!useSavedKey"><label>OpenSSH 私钥<textarea v-model="privateKey" data-test="private-key" rows="5" autocomplete="off" spellcheck="false"></textarea></label><label>私钥口令（可选）<input v-model="passphrase" data-test="passphrase" type="password" autocomplete="off" /></label><label class="check-label"><input v-model="saveKey" data-test="save-key" type="checkbox" />安全保存私钥及口令到系统凭据存储</label></template></template><p class="muted">{{ savingKey ? "正在保存到系统凭据存储，请稍候。" : "未选择保存的密码、私钥和口令仅用于本次连接。" }}</p><div class="dialog-actions"><button type="button" :disabled="savingKey" @click="cancelConnect">取消</button><button class="primary-button" data-test="start-connect" type="button" :disabled="busy || (selected?.authentication === 'password' && !password) || (selected?.authentication === 'private_key' && !useSavedKey && !privateKey)" @click="open()">连接</button></div></template>
    </section></div>
  </section>
</template>

<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from 'vue';
import { desktopUpdateCancel, desktopUpdateCheck, desktopUpdateDownload, desktopUpdateInstall, desktopUpdateState } from '@/lib/backend/api';
import type { DesktopUpdateState } from '@/lib/backend/types';

const state = ref<DesktopUpdateState>();
const request = ref<'check' | 'download' | 'install'>();
const confirming = ref(false);
const cancelling = ref(false);
const notice = ref('');
let generation = 0;
let disposed = false;
let poll: ReturnType<typeof setTimeout> | undefined;
const working = computed(() => !!request.value || ['checking', 'downloading', 'installing'].includes(state.value?.status ?? ''));
const errorText: Record<string, string> = {
  UPDATE_NOT_CONFIGURED: '此构建尚未配置正式更新源。',
  UPDATE_NETWORK: '无法连接更新服务，请检查网络后重试。',
  UPDATE_TIMEOUT: '更新请求超时，请重试。',
  UPDATE_MANIFEST: '更新清单无法读取，请稍后重试。',
  UPDATE_INVALID_MANIFEST: '更新清单无法读取，请稍后重试。',
  UPDATE_PLATFORM_UNAVAILABLE: '暂时没有适用于此设备的更新包。',
  UPDATE_SIGNATURE: '更新包签名校验失败，请重新下载。',
  UPDATE_CANCELLED: '已取消本次下载。',
  UPDATE_FILES_ACTIVE: '请先完成文件传输或其他文件操作。',
  UPDATE_WEBDAV_ACTIVE: '请等待 WebDAV 备份、还原或其他 WebDAV 操作结束。',
  UPDATE_CONNECTION_ACTIVE: '请先关闭 SSH 会话或取消正在建立的连接。',
  UPDATE_WORK_ACTIVE: '请等待当前操作结束后重试。',
  UPDATE_INSTALL_FAILED: '未能安装更新。可重试，或使用官方安装包。',
  UPDATE_BUSY: '已有更新操作正在进行。',
  UPDATE_STALE: '更新状态已变化，请重新检查。',
};
const displayedError = computed(() => {
  if (!state.value?.error) return '';
  const message = errorText[state.value.error] ?? '更新操作失败，请重试。';
  const count = state.value.blockingCount;
  return count ? `当前有 ${count} 项工作阻止安装。${message}` : message;
});
const statusText = computed(() => {
  if (request.value === 'check' || state.value?.status === 'checking') return '正在检查更新…';
  if (state.value?.status === 'current') return '当前已是最新版本。';
  if (state.value?.status === 'installing') return '正在启动安装程序…';
  return '';
});
const downloadVisible = computed(() => request.value === 'download' || state.value?.status === 'downloading');
const canCancel = computed(() => state.value?.status === 'downloading' && !!state.value.operationId);

function show(next: DesktopUpdateState, current: number) {
  if (!disposed && current === generation) state.value = next;
}

function canPoll() {
  return ['checking', 'downloading', 'installing'].includes(state.value?.status ?? '');
}

function schedulePoll(current: number) {
  clearTimeout(poll);
  poll = setTimeout(async () => {
    try {
      show(await desktopUpdateState(), current);
    } catch {
      // A transient status read does not cancel the underlying operation.
    }
    if (!disposed && current === generation && canPoll()) schedulePoll(current);
  }, 250);
}

onMounted(async () => {
  const current = generation;
  try {
    const next = await desktopUpdateState();
    show(next, current);
    if (!disposed && current === generation && canPoll()) schedulePoll(current);
  } catch {
    if (!disposed) notice.value = '无法读取更新状态。请重新打开设置。';
  }
});
onUnmounted(() => {
  disposed = true;
  clearTimeout(poll);
});

async function run(action: 'check' | 'download' | 'install') {
  if (working.value) return;
  const id = action === 'install' ? state.value?.operationId : crypto.randomUUID();
  if (!id) return;
  const current = ++generation;
  confirming.value = false;
  notice.value = '';
  request.value = action;
  clearTimeout(poll);
  try {
    const call = {check: desktopUpdateCheck, download: desktopUpdateDownload, install: desktopUpdateInstall}[action];
    show(await call(id), current);
  } catch (error) {
    if (!disposed && current === generation) notice.value = errorText[String(error)] ?? '更新操作失败，请重试。';
  } finally {
    if (!disposed && current === generation) {
      request.value = undefined;
      if (canPoll()) schedulePoll(current);
    }
  }
}

async function cancel() {
  const id = state.value?.operationId;
  if (!canCancel.value || !id || cancelling.value) return;
  cancelling.value = true;
  const current = generation;
  try {
    const next = await desktopUpdateCancel(id);
    if (disposed || current !== generation) return;
    // A download may finish before the cancellation reaches the host.
    const cancellationGeneration = ++generation;
    clearTimeout(poll);
    state.value = next;
    request.value = undefined;
    if (canPoll()) schedulePoll(cancellationGeneration);
  } catch (error) {
    if (!disposed && current === generation) notice.value = errorText[String(error)] ?? '无法取消下载。';
  } finally {
    cancelling.value = false;
  }
}
</script>

<template>
  <section class="update-settings" aria-label="软件更新">
    <h2>软件更新</h2>
    <p v-if="state?.currentVersion">当前版本 {{ state.currentVersion }}</p>
    <p v-if="state?.status === 'unsupported'">{{ state.currentVersion ? displayedError || '此构建未启用在线升级。' : '在线升级仅适用于桌面版。' }}</p>
    <template v-else>
      <p v-if="statusText" role="status">{{ statusText }}</p>
      <p v-if="state?.version">可用版本 {{ state.version }}</p>
      <pre v-if="state?.notes" class="update-notes">{{ state.notes }}</pre>
      <template v-if="downloadVisible">
        <progress v-if="state?.totalBytes" :max="state.totalBytes" :value="state.downloadedBytes ?? 0" aria-label="更新下载进度" />
        <progress v-else aria-label="更新下载进度" />
        <p v-if="state?.downloadedBytes">{{ (state.downloadedBytes / 1048576).toFixed(1) }} MiB 已下载</p>
      </template>
      <p v-if="displayedError" role="status">{{ displayedError }}</p>
      <p v-if="notice" role="status">{{ notice }}</p>
      <p v-if="confirming">安装将关闭 CoShell。请先结束会话、文件操作和备份，再确认安装。</p>
      <div v-if="state" class="dialog-actions">
        <template v-if="confirming">
          <button type="button" @click="confirming = false">返回</button>
          <button type="button" class="primary-button" data-test="update-confirm" @click="run('install')">确认安装</button>
        </template>
        <template v-else-if="state.status === 'ready' && !working">
          <button type="button" data-test="update-later" @click="notice = '可以稍后返回软件更新继续安装。'">稍后</button>
          <button type="button" class="primary-button" data-test="update-install" @click="confirming = true">安装并重启</button>
        </template>
        <button v-else-if="canCancel" type="button" data-test="update-cancel" :disabled="cancelling" @click="cancel">取消下载</button>
        <template v-else>
          <button type="button" data-test="update-check" :disabled="working" @click="run('check')">检查更新</button>
          <button v-if="state.status === 'available'" type="button" class="primary-button" data-test="update-download" :disabled="working" @click="run('download')">下载更新</button>
        </template>
      </div>
    </template>
  </section>
</template>

<style scoped>
.update-settings { border-top: 1px solid var(--border); padding-top: 16px; margin-top: 18px; }
.update-notes { white-space: pre-wrap; overflow-wrap: anywhere; max-height: 10rem; overflow: auto; font: inherit; }
progress { width: 100%; }
</style>

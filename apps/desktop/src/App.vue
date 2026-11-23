<script setup lang="ts">
import { computed, nextTick, ref } from 'vue';
import { RefreshCw, Server, SquareTerminal, FilePlus, Cloud, ArrowLeft, Settings } from '@lucide/vue';
import { backendRuntime } from '@/lib/backend/api';
import { setThemeMode, themeMode, type ThemeMode } from '@/lib/theme';
import { useSystemStore } from '@/stores/systemStore';
import ConnectionsPage from '@/components/ConnectionsPage.vue';
import WebDavSettings from '@/components/WebDavSettings.vue';

const system = useSystemStore();
const runtimeLabel = computed(() => backendRuntime() === 'tauri' ? 'Tauri Desktop' : 'Web / HTTP');
const page = ref<'connections' | 'webdav'>('connections');
const connectionsPage = ref<InstanceType<typeof ConnectionsPage> | null>(null);
const settings = ref<HTMLDialogElement | null>(null);
const settingsButton = ref<HTMLButtonElement | null>(null);
const themeSaved = ref(true);
const themeOptions: { value: ThemeMode; label: string }[] = [
  { value: 'light', label: '白色' },
  { value: 'dark', label: '黑暗' },
  { value: 'system', label: '跟随系统' },
];

function selectTheme(mode: ThemeMode) {
  themeSaved.value = setThemeMode(mode);
}

function closeSettings() {
  settings.value?.close();
  settingsButton.value?.focus();
}
async function createConnection() {
  page.value = 'connections'; await nextTick(); connectionsPage.value?.create();
}
async function createCommand() {
  page.value = 'connections'; await nextTick(); connectionsPage.value?.createCommand();
}
</script>

<template>
  <div class="shell">
    <header class="topbar">
      <nav class="workbench-actions" aria-label="常用操作">
        <button data-test="top-action-connections" type="button" @click="createConnection"><SquareTerminal :size="15" aria-hidden="true" />新建连接</button>
        <button data-test="top-action-commands" type="button" @click="createCommand"><FilePlus :size="15" aria-hidden="true" />新建命令</button>
        <button data-test="top-action-webdav" type="button" :aria-pressed="page === 'webdav'" @click="page = 'webdav'"><Cloud :size="15" aria-hidden="true" />WebDAV 配置</button>
      </nav>
      <div class="topbar__actions">
        <span class="runtime-badge"><Server :size="14" aria-hidden="true" />{{ runtimeLabel }}</span>
        <button class="icon-button" type="button" :disabled="system.loading" title="刷新 Core 状态" aria-label="刷新 Core 状态" @click="system.refresh"><RefreshCw :size="15" :class="{ spinning: system.loading }" aria-hidden="true" /></button>
        <button ref="settingsButton" data-test="open-settings" class="icon-button" type="button" title="设置" aria-label="设置" aria-haspopup="dialog" @click="settings?.showModal()"><Settings :size="15" aria-hidden="true" /></button>
      </div>
    </header>
    <main class="workbench-content">
      <ConnectionsPage v-show="page === 'connections'" ref="connectionsPage" :active="page === 'connections'" />
      <section v-show="page === 'webdav'" class="webdav-workspace" aria-label="WebDAV 配置工作区">
        <div class="workbench-page-toolbar"><button data-test="return-workspace" type="button" @click="page = 'connections'"><ArrowLeft :size="14" aria-hidden="true" />返回会话</button><span>WebDAV 配置</span></div>
        <WebDavSettings :active="page === 'webdav'" />
      </section>
    </main>
    <dialog ref="settings" class="app-settings dialog" aria-label="设置" @cancel.prevent="closeSettings">
      <header class="app-settings__header">
        <h2>设置</h2>
        <button data-test="close-settings" type="button" @click="closeSettings">关闭</button>
      </header>
      <fieldset class="app-settings__appearance">
        <legend>外观</legend>
        <label v-for="option in themeOptions" :key="option.value" class="check-label">
          <input type="radio" name="app-theme" :value="option.value" :checked="themeMode === option.value" @change="selectTheme(option.value)" />
          {{ option.label }}
        </label>
        <p v-if="!themeSaved" role="status">主题无法保存，本次切换仍有效。</p>
      </fieldset>
    </dialog>
  </div>
</template>

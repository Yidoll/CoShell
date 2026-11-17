<script setup lang="ts">
import { computed, nextTick, ref } from 'vue';
import { RefreshCw, Server, SquareTerminal, FilePlus, Cloud, ArrowLeft } from '@lucide/vue';
import { backendRuntime } from '@/lib/backend/api';
import { useSystemStore } from '@/stores/systemStore';
import ConnectionsPage from '@/components/ConnectionsPage.vue';
import WebDavSettings from '@/components/WebDavSettings.vue';

const system = useSystemStore();
const runtimeLabel = computed(() => backendRuntime() === 'tauri' ? 'Tauri Desktop' : 'Web / HTTP');
const page = ref<'connections' | 'webdav'>('connections');
const connectionsPage = ref<InstanceType<typeof ConnectionsPage> | null>(null);
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
      </div>
    </header>
    <main class="workbench-content">
      <ConnectionsPage v-show="page === 'connections'" ref="connectionsPage" :active="page === 'connections'" />
      <section v-show="page === 'webdav'" class="webdav-workspace" aria-label="WebDAV 配置工作区">
        <div class="workbench-page-toolbar"><button data-test="return-workspace" type="button" @click="page = 'connections'"><ArrowLeft :size="14" aria-hidden="true" />返回会话</button><span>WebDAV 配置</span></div>
        <WebDavSettings :active="page === 'webdav'" />
      </section>
    </main>
  </div>
</template>

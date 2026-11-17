<script setup lang="ts">
import { nextTick, onUnmounted, ref } from 'vue';
import SessionWorkspace from './SessionWorkspace.vue';
import type { ConnectionProfile } from '@/lib/backend/types';

const props = withDefaults(defineProps<{ active?: boolean }>(), { active: true });
type Tab = { key: number; profile?: ConnectionProfile; sessionId?: string; disconnected?: boolean };
const tabs = ref<Tab[]>([{ key: 1 }]);
const active = ref(1);
const menu = ref<number | null>(null);
const operating = ref(false);
const confirmKeys = ref<number[] | null>(null);
const menuPosition = ref({ x: 0, y: 0 });
let nextKey = 2;
const workspaces = new Map<number, InstanceType<typeof SessionWorkspace>>();
function bind(key: number, value: unknown) {
  if (value) workspaces.set(key, value as InstanceType<typeof SessionWorkspace>); else workspaces.delete(key);
}
function add(profile?: ConnectionProfile) {
  if (tabs.value.length >= 32) { menu.value = null; return; }
  const key = nextKey++; tabs.value.push({ key, profile }); active.value = key; menu.value = null;
}
function connected(key: number, value: { id: string; profile: ConnectionProfile }) {
  const tab = tabs.value.find(t => t.key === key); if (tab) { tab.profile = value.profile; tab.sessionId = value.id; tab.disconnected = false; }
}
function workspaceClosed(key: number) {
  const tab = tabs.value.find(t => t.key === key); if (tab) { tab.sessionId = undefined; tab.disconnected = false; tab.profile = undefined; }
}
async function disconnect(key: number) {
  if (operating.value) return; menu.value = null; operating.value = true;
  try { if (await workspaces.get(key)?.disconnect()) { const tab = tabs.value.find(t => t.key === key); if (tab) tab.disconnected = true; } }
  finally { operating.value = false; }
}
async function remove(keys: number[]) {
  if (operating.value) return; menu.value = null; operating.value = true;
  try {
    for (const key of keys) {
      const workspace = workspaces.get(key);
      if (workspace && !(await workspace.close())) { active.value = key; break; }
      tabs.value = tabs.value.filter(t => t.key !== key);
    }
    if (!tabs.value.length) add();
    if (!tabs.value.some(t => t.key === active.value)) active.value = tabs.value[0]!.key;
    await nextTick();
  } finally { operating.value = false; }
}
function requestRemove(keys: number[]) {
  menu.value = null;
  if (keys.some(key => tabs.value.find(t => t.key === key)?.sessionId)) confirmKeys.value = keys;
  else void remove(keys);
}
function openMenu(event: MouseEvent, key: number) {
  active.value = key; menuPosition.value = { x: Math.min(event.clientX, Math.max(0, window.innerWidth - 150)), y: Math.min(event.clientY, Math.max(0, window.innerHeight - 210)) }; menu.value = key;
}
function outside(event: MouseEvent) { if (!(event.target as HTMLElement).closest('.session-tabs')) menu.value = null; }
document.addEventListener('click', outside);
function create() { workspaces.get(active.value)?.create(); }
function createCommand() { workspaces.get(active.value)?.createCommand(); }
defineExpose({ create, createCommand });
onUnmounted(() => document.removeEventListener('click', outside));
</script>

<template>
  <div class="session-tabs" aria-label="会话标签">
    <div class="session-tabs__bar" role="tablist" aria-label="会话">
      <div v-for="tab in tabs" :key="tab.key" class="session-tabs__item">
        <button data-test="session-tab" type="button" role="tab" :aria-selected="active === tab.key" :disabled="operating" @click="active = tab.key; menu = null" @contextmenu.prevent="openMenu($event, tab.key)">
          <span :class="['session-tabs__dot', { 'session-tabs__dot--offline': !tab.sessionId || tab.disconnected }]"></span>{{ tab.profile?.name || '新建会话' }}<span v-if="tab.profile?.production"> · 生产</span>
        </button>
        <div v-if="menu === tab.key" class="session-tabs__menu" :style="{ left: `${menuPosition.x}px`, top: `${menuPosition.y}px` }" role="menu" @keydown.esc="menu = null">
          <button data-test="duplicate-tab" type="button" role="menuitem" :disabled="!tab.profile || operating || tabs.length >= 32" @click="add(tab.profile)">复制标签</button>
          <button data-test="disconnect-tab" type="button" role="menuitem" :disabled="!tab.sessionId || tab.disconnected || operating" @click="disconnect(tab.key)">断开</button>
          <button data-test="close-tab" type="button" role="menuitem" :disabled="operating" @click="requestRemove([tab.key])">关闭</button>
          <button data-test="close-others" type="button" role="menuitem" :disabled="tabs.length < 2 || operating" @click="requestRemove(tabs.filter(t => t.key !== tab.key).map(t => t.key))">关闭其他</button>
          <button data-test="close-all" type="button" role="menuitem" :disabled="operating" @click="requestRemove(tabs.map(t => t.key))">关闭全部</button>
        </div>
      </div>
      <button type="button" class="session-tabs__add" aria-label="新建会话" :disabled="operating || tabs.length >= 32" @click="add()">＋</button>
    </div>
    <SessionWorkspace v-for="tab in tabs" v-show="active === tab.key" :key="tab.key" :ref="value => bind(tab.key, value)" :active="props.active && active === tab.key" :initial-profile="tab.profile && !tab.sessionId ? tab.profile : undefined" @connected="connected(tab.key, $event)" @closed="workspaceClosed(tab.key)" @disconnected="tab.disconnected = true" />
    <div v-if="confirmKeys" class="tabs-confirm" @keydown.esc="confirmKeys = null"><section role="alertdialog" aria-modal="true" aria-label="关闭会话"><h2>关闭会话</h2><p>将断开选中的活动会话，确定继续吗？</p><button data-test="cancel-close-tabs" type="button" @click="confirmKeys = null">取消</button><button data-test="confirm-close-tabs" type="button" @click="remove(confirmKeys); confirmKeys = null">关闭</button></section></div>
  </div>
</template>

<style scoped>
.session-tabs__bar { display:flex; gap:4px; align-items:center; border-bottom:1px solid #d9e1e6; margin-bottom:12px; overflow-x:auto; }
.session-tabs__item { position:relative; flex:none; }
.session-tabs__item > button { border:0; border-bottom:2px solid transparent; border-radius:0; background:transparent; padding:9px 14px; }
.session-tabs__item > button[aria-selected="true"] { border-bottom-color:#147d67; background:#fff; }
.session-tabs__dot { display:inline-block; width:7px; height:7px; border-radius:50%; background:#16855f; margin-right:7px; }
.session-tabs__dot--offline { background:#9ca8ad; }
.session-tabs__add { border:0; background:transparent; font-size:18px; }
.session-tabs__menu { position:fixed; z-index:20; display:flex; flex-direction:column; min-width:120px; padding:4px; border:1px solid #d6dfe4; border-radius:5px; background:white; box-shadow:0 6px 20px #172a3830; }
.session-tabs__menu button { text-align:left; border:0; background:transparent; padding:7px 12px; }
.session-tabs__menu button:hover:not(:disabled) { background:#e7f5f0; }
.tabs-confirm { position:fixed; inset:0; z-index:100; display:grid; place-items:center; background:#0008; }
.tabs-confirm section { background:white; padding:24px; border-radius:8px; max-width:400px; }
.tabs-confirm button { margin-right:10px; }
</style>

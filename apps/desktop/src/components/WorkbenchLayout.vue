<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { PanelLeft, PanelRight, ChevronDown, ChevronUp } from '@lucide/vue';

function read(key: string, fallback: number) {
  try { const raw = localStorage.getItem(key); const n = raw === null ? fallback : Number(raw); return Number.isFinite(n) ? n : fallback; } catch { return fallback; }
}
function collapsed(panel: string) { try { return localStorage.getItem(`coshell.panel.${panel}`) === 'collapsed'; } catch { return false; } }
function store(key: string, value: string | number) { try { localStorage.setItem(key, String(value)); } catch { /* Optional UI preferences. */ } }
const root = ref<HTMLElement>();
const center = ref<HTMLElement>();
const width = ref(window.innerWidth);
const leftWidth = ref(Math.max(210, Math.min(380, read('coshell.layout.left', 250))));
const rightWidth = ref(Math.max(240, Math.min(400, read('coshell.layout.right', 280))));
const terminalRatio = ref(Math.max(35, Math.min(78, read('coshell.layout.terminal', 64))));
const leftCollapsed = ref(collapsed('connections'));
const rightCollapsed = ref(collapsed('commands'));
const filesCollapsed = ref(collapsed('files'));
const leftOverlay = ref(false);
const rightOverlay = ref(false);
const narrow = computed(() => width.value < 800);
const compact = computed(() => width.value < 1100);
const leftVisible = computed(() => !leftCollapsed.value && (!narrow.value || leftOverlay.value));
const rightVisible = computed(() => !rightCollapsed.value && (!compact.value || rightOverlay.value));
const layoutStyle = computed(() => ({ '--left-width': `${Math.min(leftWidth.value, width.value * .32)}px`, '--right-width': `${Math.min(rightWidth.value, width.value * .32)}px`, '--terminal-ratio': `${terminalRatio.value}fr`, '--file-ratio': `${100 - terminalRatio.value}fr` }));
function toggleLeft() {
  if (narrow.value) { leftOverlay.value = !leftVisible.value; if (leftOverlay.value) leftCollapsed.value = false; }
  else leftCollapsed.value = !leftCollapsed.value;
  store('coshell.panel.connections', leftCollapsed.value ? 'collapsed' : 'open');
}
function toggleRight() {
  if (compact.value) { rightOverlay.value = !rightVisible.value; if (rightOverlay.value) rightCollapsed.value = false; }
  else rightCollapsed.value = !rightCollapsed.value;
  store('coshell.panel.commands', rightCollapsed.value ? 'collapsed' : 'open');
}
function openCommands() { rightCollapsed.value = false; rightOverlay.value = compact.value; store('coshell.panel.commands', 'open'); }
watch(narrow, () => { leftOverlay.value = false; });
watch(compact, () => { rightOverlay.value = false; });
function toggleFiles() { filesCollapsed.value = !filesCollapsed.value; store('coshell.panel.files', filesCollapsed.value ? 'collapsed' : 'open'); }
type Split = 'left' | 'right' | 'terminal';
function adjust(split: Split, value: number) {
  if (split === 'left') { leftWidth.value = Math.max(210, Math.min(380, width.value * .32, value)); store('coshell.layout.left', leftWidth.value); }
  if (split === 'right') { rightWidth.value = Math.max(240, Math.min(400, width.value * .32, value)); store('coshell.layout.right', rightWidth.value); }
  if (split === 'terminal') { terminalRatio.value = Math.max(35, Math.min(78, value)); store('coshell.layout.terminal', terminalRatio.value); }
}
let stopDrag: (() => void) | undefined;
function drag(event: PointerEvent, split: Split) {
  if (event.button !== 0) return;
  event.preventDefault(); stopDrag?.();
  const bounds = (split === 'terminal' ? center.value : root.value)?.getBoundingClientRect();
  if (!bounds) return;
  const move = (e: PointerEvent) => adjust(split, split === 'left' ? e.clientX - bounds.left : split === 'right' ? bounds.right - e.clientX : (e.clientY - bounds.top) / Math.max(1, bounds.height) * 100);
  stopDrag = () => { window.removeEventListener('pointermove', move); window.removeEventListener('pointerup', stop); window.removeEventListener('pointercancel', stop); stopDrag = undefined; };
  const stop = () => stopDrag?.();
  window.addEventListener('pointermove', move); window.addEventListener('pointerup', stop); window.addEventListener('pointercancel', stop);
}
function keyboard(event: KeyboardEvent, split: Split) {
  const delta = event.key === 'ArrowRight' || event.key === 'ArrowDown' ? 1 : event.key === 'ArrowLeft' || event.key === 'ArrowUp' ? -1 : 0;
  if (!delta) return;
  event.preventDefault();
  adjust(split, split === 'left' ? leftWidth.value + delta * 16 : split === 'right' ? rightWidth.value - delta * 16 : terminalRatio.value + delta * 3);
}
let observer: ResizeObserver | undefined;
function resize() { const measured = root.value?.getBoundingClientRect().width; width.value = measured || window.innerWidth; }
onMounted(() => { resize(); window.addEventListener('resize', resize); if (typeof ResizeObserver !== 'undefined' && root.value) { observer = new ResizeObserver(resize); observer.observe(root.value); } });
onUnmounted(() => { stopDrag?.(); observer?.disconnect(); window.removeEventListener('resize', resize); });
defineExpose({ openCommands });
</script>

<template>
  <div ref="root" class="workbench-layout" data-test="connection-workspace-layout" :class="{ 'is-narrow': narrow, 'is-compact': compact }" :style="layoutStyle">
    <aside class="workbench-left" :class="{ 'is-collapsed': !leftVisible, 'is-overlay': narrow && leftVisible }" aria-label="连接管理">
      <header class="workbench-panel-heading"><strong v-if="leftVisible">连接管理</strong><button data-test="toggle-connections-sidebar" type="button" :aria-label="leftVisible ? '收起连接管理' : '展开连接管理'" :aria-expanded="leftVisible" @click="toggleLeft"><PanelLeft :size="15" aria-hidden="true" /></button></header>
      <div v-show="leftVisible" class="workbench-left-content" :inert="!leftVisible"><slot name="connections" /><details class="workbench-overview"><summary>服务器信息</summary><slot name="overview" /></details></div>
    </aside>
    <div v-show="leftVisible && !narrow" class="workbench-splitter" role="separator" aria-label="调整连接栏宽度" aria-orientation="vertical" :aria-valuenow="leftWidth" :aria-valuemin="210" :aria-valuemax="380" tabindex="0" @pointerdown="drag($event, 'left')" @keydown="keyboard($event, 'left')"></div>
    <div ref="center" class="workbench-center" :class="{ 'files-collapsed': filesCollapsed }">
      <div class="workbench-terminal"><slot name="terminal" /></div>
      <div v-show="!filesCollapsed" class="workbench-splitter workbench-splitter--horizontal" role="separator" aria-label="调整终端与文件高度" aria-orientation="horizontal" :aria-valuenow="terminalRatio" :aria-valuemin="35" :aria-valuemax="78" tabindex="0" @pointerdown="drag($event, 'terminal')" @keydown="keyboard($event, 'terminal')"></div>
      <section class="workbench-files" aria-label="服务器文件夹"><header class="workbench-panel-heading"><strong>服务器文件夹</strong><small>SFTP</small><button data-test="toggle-files-panel" type="button" :aria-label="filesCollapsed ? '展开文件面板' : '收起文件面板'" :aria-expanded="!filesCollapsed" @click="toggleFiles"><component :is="filesCollapsed ? ChevronUp : ChevronDown" :size="15" aria-hidden="true" /></button></header><div v-show="!filesCollapsed" class="workbench-file-content" :inert="filesCollapsed"><slot name="files" /></div></section>
    </div>
    <div v-show="rightVisible && !compact" class="workbench-splitter" role="separator" aria-label="调整命令栏宽度" aria-orientation="vertical" :aria-valuenow="rightWidth" :aria-valuemin="240" :aria-valuemax="400" tabindex="0" @pointerdown="drag($event, 'right')" @keydown="keyboard($event, 'right')"></div>
    <aside class="workbench-right" :class="{ 'is-collapsed': !rightVisible, 'is-overlay': compact && rightVisible }" aria-label="右侧命令管理"><header class="workbench-panel-heading"><strong v-if="rightVisible">命令管理</strong><button data-test="toggle-command-sidebar" type="button" :aria-label="rightVisible ? '收起命令管理' : '展开命令管理'" :aria-expanded="rightVisible" @click="toggleRight"><PanelRight :size="15" aria-hidden="true" /><span class="sr-only">{{ rightVisible ? '收起' : '展开' }}</span></button></header><div v-show="rightVisible" class="workbench-command-content" :inert="!rightVisible"><slot name="commands" /></div></aside>
  </div>
</template>

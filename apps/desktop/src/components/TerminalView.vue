<script setup lang="ts">
import { onMounted, onUnmounted, ref } from 'vue';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { TerminalRenderQueue } from '@/lib/backend/terminal-render';

const props = withDefaults(defineProps<{ disabled: boolean; active?: boolean }>(), { active: true });
const emit = defineEmits<{ data: [value: string]; paste: [value: string]; resize: [cols: number, rows: number] }>();
const container = ref<HTMLElement>();
let terminal: Terminal | undefined;
let fit: FitAddon | undefined;
let observer: ResizeObserver | undefined;
let renderQueue: TerminalRenderQueue | undefined;
function write(value: string) { renderQueue?.write(value); }
function focus() { if (props.active) terminal?.focus(); }
function activate() { if (!props.active || !terminal || !fit || !container.value?.clientWidth || !container.value.clientHeight) return; fit.fit(); emit('resize', terminal.cols, terminal.rows); terminal.focus(); }
function eraseLast(grapheme: string) { renderQueue?.eraseLast(grapheme); }
function ready() { return renderQueue?.idle() ?? Promise.resolve(false); }
defineExpose({ write, focus, activate, eraseLast, ready });
onMounted(() => {
  if (!container.value) return;
  terminal = new Terminal({ cursorBlink: true, scrollback: 5000, fontFamily: 'Consolas, monospace', fontSize: 13, theme: { background: '#172a38', foreground: '#e8f3f1' }, allowProposedApi: false });
  renderQueue = new TerminalRenderQueue(terminal);
  // Never let a remote OSC 52 request access the local clipboard.
  terminal.parser.registerOscHandler(52, () => true);
  fit = new FitAddon(); terminal.loadAddon(fit); terminal.open(container.value);
  terminal.onData((data) => { if (props.active && !props.disabled) emit('data', data); });
  container.value.addEventListener('paste', onPaste, true);
  const resize = () => { if (!terminal || !fit || !container.value?.clientWidth || !container.value.clientHeight) return; fit.fit(); emit('resize', terminal.cols, terminal.rows); };
  if (typeof ResizeObserver !== 'undefined') { observer = new ResizeObserver(resize); observer.observe(container.value); }
  resize(); if (props.active) terminal.focus();
});
function onPaste(event: ClipboardEvent) { event.preventDefault(); event.stopImmediatePropagation(); if (props.active && !props.disabled) emit('paste', event.clipboardData?.getData('text/plain') ?? ''); }
onUnmounted(() => { observer?.disconnect(); container.value?.removeEventListener('paste', onPaste, true); terminal?.dispose(); });
</script>

<template><div class="terminal-screen" data-test="terminal"><div ref="container" class="terminal-host" aria-label="SSH 终端"></div></div></template>

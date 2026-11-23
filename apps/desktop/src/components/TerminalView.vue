<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from 'vue';
import { Terminal, type ITheme } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { TerminalRenderQueue } from '@/lib/backend/terminal-render';
import { resolvedTheme, type ResolvedTheme } from '@/lib/theme';

const palettes: Record<ResolvedTheme, ITheme> = {
  light: {
    background: '#ffffff', foreground: '#262626', cursor: '#262626',
    selectionBackground: '#c8daf5', selectionForeground: '#262626',
    black: '#303030', red: '#a31e17', green: '#216539', yellow: '#715400',
    blue: '#1c4ea0', magenta: '#764c83', cyan: '#00656a', white: '#595959',
    brightBlack: '#626262', brightRed: '#b42318', brightGreen: '#226947',
    brightYellow: '#785200', brightBlue: '#285bb1', brightMagenta: '#87518e',
    brightCyan: '#006e78', brightWhite: '#262626',
  },
  dark: {
    background: '#131416', foreground: '#e0e0e4', cursor: '#e0e0e4',
    selectionBackground: '#354a6c', selectionForeground: '#f5f5f5',
    black: '#777982', red: '#ff8b82', green: '#7bcf9a', yellow: '#e9c46d',
    blue: '#95b9ff', magenta: '#d2a8e2', cyan: '#71cdd5', white: '#e0e0e4',
    brightBlack: '#a1a1a9', brightRed: '#ffa69f', brightGreen: '#a4e1b5',
    brightYellow: '#f5d691', brightBlue: '#b3caff', brightMagenta: '#e1bdef',
    brightCyan: '#9ee3e7', brightWhite: '#ffffff',
  },
};

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
watch(resolvedTheme, (theme) => {
  if (terminal) terminal.options.theme = palettes[theme];
});
onMounted(() => {
  if (!container.value) return;
  terminal = new Terminal({
    cursorBlink: true,
    scrollback: 5000,
    fontFamily: 'Consolas, monospace',
    fontSize: 13,
    theme: palettes[resolvedTheme.value],
    allowProposedApi: false,
  });
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

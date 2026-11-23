import { mount } from '@vue/test-utils';
import { expect, it, vi } from 'vitest';
import '../../styles/global.css';
import { setThemeMode } from '@/lib/theme';

const fitParent = vi.hoisted(() => ({
  element: null as HTMLElement | null,
  focus: vi.fn(),
  onData: undefined as ((value: string) => void) | undefined,
  options: undefined as { theme?: Record<string, string> } | undefined,
  construct: vi.fn(),
  dispose: vi.fn(),
}));
vi.mock('@xterm/xterm', () => ({ Terminal: class {
  cols = 80; rows = 24; element?: HTMLElement;
  parser = { registerOscHandler: vi.fn() };
  constructor(public options: { theme?: Record<string, string> }) {
    fitParent.construct();
    fitParent.options = options;
  }
  loadAddon() {} open(parent: HTMLElement) { this.element = document.createElement('div'); parent.appendChild(this.element); fitParent.element = parent; }
  onData(callback: (value: string) => void) { fitParent.onData = callback; } focus() { fitParent.focus(); } dispose() { fitParent.dispose(); } write(_value: string, callback?: () => void) { callback?.(); }
} }));
vi.mock('@xterm/addon-fit', () => ({ FitAddon: class { fit() {} } }));
import TerminalView from '../TerminalView.vue';

it('fits inside an unpadded content box so the last terminal row remains visible', () => {
  const wrapper = mount(TerminalView, { props: { disabled: false }, attachTo: document.body });
  const viewport = wrapper.get('[data-test="terminal"]').element as HTMLElement;
  const host = fitParent.element;
  expect(host).not.toBe(viewport);
  expect(host?.parentElement).toBe(viewport);
  expect(getComputedStyle(host!).paddingTop).toBe('');
  expect(getComputedStyle(host!).paddingBottom).toBe('');
  wrapper.unmount();
});

it('does not focus a terminal mounted after its workspace became inactive', () => {
  fitParent.focus.mockClear();
  const wrapper = mount(TerminalView, { props: { disabled: false, active: false }, attachTo: document.body });
  expect(fitParent.focus).not.toHaveBeenCalled();
  wrapper.unmount();
});


it('隐藏终端忽略输入和粘贴，也不抢回焦点', async () => {
  const w = mount(TerminalView, { props: { disabled: false, active: true }, attachTo: document.body });
  await w.setProps({ active: false }); fitParent.focus.mockClear();
  fitParent.onData?.('rm -rf /tmp/example\r');
  fitParent.element!.dispatchEvent(new Event('paste', { bubbles: true }));
  w.vm.focus();
  expect(w.emitted('data')).toBeUndefined();
  expect(w.emitted('paste')).toBeUndefined();
  expect(fitParent.focus).not.toHaveBeenCalled();
  w.unmount();
});

it('切换主题只替换原终端选项，并保留同一个终端实例及输入处理', async () => {
  setThemeMode('light');
  fitParent.construct.mockClear();
  fitParent.dispose.mockClear();
  fitParent.focus.mockClear();
  const w = mount(TerminalView, { props: { disabled: false }, attachTo: document.body });
  const originalHost = fitParent.element;
  const originalInput = fitParent.onData;
  const options = fitParent.options!;
  const light = options.theme!;
  fitParent.focus.mockClear();

  setThemeMode('dark');
  await w.vm.$nextTick();
  const dark = options.theme;
  expect(dark?.background).not.toBe(light.background);
  expect(dark?.foreground).not.toBe(light.foreground);
  expect(dark?.selectionBackground).not.toBe(light.selectionBackground);
  expect(dark?.cursor).not.toBe(light.cursor);
  expect(fitParent.construct).toHaveBeenCalledTimes(1);
  expect(fitParent.dispose).not.toHaveBeenCalled();
  expect(fitParent.focus).not.toHaveBeenCalled();
  expect(fitParent.element).toBe(originalHost);
  expect(fitParent.onData).toBe(originalInput);
  originalInput?.('x');
  expect(w.emitted('data')).toEqual([['x']]);
  w.unmount();
  setThemeMode('system');
  localStorage.removeItem('coshell.theme');
});

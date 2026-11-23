import { mount, flushPromises } from '@vue/test-utils';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { defineComponent, h } from 'vue';
import App from '@/App.vue';
import { initializeTheme } from '@/lib/theme';

const connections = vi.hoisted(() => ({ create: vi.fn(), createCommand: vi.fn() }));
const runtime = vi.hoisted(() => ({ value: 'tauri' }));
vi.mock('@/components/ConnectionsPage.vue', () => ({ default: defineComponent({ name: 'ConnectionsPage', props: ['active'], setup(_, { expose }) { expose(connections); return () => h('section', { 'data-test': 'connections-page' }, '连接工作台'); } }) }));
vi.mock('@/components/WebDavSettings.vue', () => ({ default: defineComponent({ name: 'WebDavSettings', props: ['active'], setup: () => () => h('section', { 'data-test': 'webdav-page' }, 'WebDAV') }) }));
vi.mock('@/stores/systemStore', () => ({ useSystemStore: () => ({ loading: false, refresh: vi.fn() }) }));
vi.mock('@/lib/backend/api', () => ({ backendRuntime: () => runtime.value }));

beforeEach(() => {
  vi.spyOn(HTMLDialogElement.prototype, 'showModal').mockImplementation(function (this: HTMLDialogElement) {
    this.setAttribute('open', '');
  });
  vi.spyOn(HTMLDialogElement.prototype, 'close').mockImplementation(function (this: HTMLDialogElement) {
    this.removeAttribute('open');
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.removeItem('coshell.theme');
  runtime.value = 'tauri';
});

describe('紧凑顶部操作栏', () => {
  it('从左侧直接提供三项操作，去除品牌和重复标签', async () => {
    const wrapper = mount(App);
    expect(wrapper.find('.brand').exists()).toBe(false);
    expect(wrapper.findAll('.top-tab')).toHaveLength(0);
    const toolbar = wrapper.get('[aria-label="常用操作"]');
    expect(toolbar.findAll('button').map(b => b.text())).toEqual(['新建连接', '新建命令', 'WebDAV 配置']);
    await wrapper.get('[data-test=top-action-commands]').trigger('click'); await flushPromises();
    expect(connections.createCommand).toHaveBeenCalled();
    expect(wrapper.findAllComponents({ name: 'SavedCommandsPanel' })).toHaveLength(0);
    wrapper.unmount();
  });
  it('配置和会话切换保持实例，返回后恢复当前会话活动状态', async () => {
    const wrapper = mount(App);
    const instance = wrapper.getComponent({ name: 'ConnectionsPage' }).element;
    await wrapper.get('[data-test=top-action-webdav]').trigger('click'); await flushPromises();
    expect(wrapper.getComponent({ name: 'ConnectionsPage' }).props('active')).toBe(false);
    await wrapper.get('[data-test=return-workspace]').trigger('click'); await flushPromises();
    expect(wrapper.getComponent({ name: 'ConnectionsPage' }).element).toBe(instance);
    expect(wrapper.getComponent({ name: 'ConnectionsPage' }).props('active')).toBe(true);
    await wrapper.get('[data-test=top-action-connections]').trigger('click'); await flushPromises();
    expect(connections.create).toHaveBeenCalled();
    wrapper.unmount();
  });

  it.each(['tauri', 'http'])('在 %s 设置中选择主题，且不提供应用内更新', async (value) => {
    runtime.value = value;
    const stopTheme = initializeTheme();
    const wrapper = mount(App, { attachTo: document.body });
    const opener = wrapper.get<HTMLButtonElement>('[data-test=open-settings]');
    const session = wrapper.getComponent({ name: 'ConnectionsPage' }).element;

    await opener.trigger('click');
    expect(wrapper.get('dialog[aria-label="设置"]').attributes('open')).toBeDefined();
    await wrapper.get<HTMLInputElement>('input[value=dark]').setValue(true);
    expect(document.documentElement.dataset.theme).toBe('dark');
    expect(localStorage.getItem('coshell.theme')).toBe('dark');
    await wrapper.get<HTMLInputElement>('input[value=light]').setValue(true);
    expect(document.documentElement.dataset.theme).toBe('light');
    expect(wrapper.get('dialog[aria-label="设置"]').text()).not.toContain('软件更新');

    await wrapper.get('[data-test=close-settings]').trigger('click');
    expect(document.activeElement).toBe(opener.element);
    expect(wrapper.getComponent({ name: 'ConnectionsPage' }).element).toBe(session);
    expect(wrapper.getComponent({ name: 'ConnectionsPage' }).props('active')).toBe(true);
    wrapper.unmount();
    stopTheme();
  });

  it('浏览器禁止保存时提示本次有效', async () => {
    const wrapper = mount(App);
    vi.stubGlobal('localStorage', { setItem() { throw new Error('blocked'); } });
    await wrapper.get('[data-test=open-settings]').trigger('click');
    await wrapper.get('input[value=dark]').setValue(true);
    expect(wrapper.get('[role=status]').text()).toContain('本次');
    wrapper.unmount();
  });
});

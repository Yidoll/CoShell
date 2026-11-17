import { mount, flushPromises } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';
import { defineComponent, h } from 'vue';
import App from '@/App.vue';

const connections = vi.hoisted(() => ({ create: vi.fn(), createCommand: vi.fn() }));
vi.mock('@/components/ConnectionsPage.vue', () => ({ default: defineComponent({ name: 'ConnectionsPage', props: ['active'], setup(_, { expose }) { expose(connections); return () => h('section', { 'data-test': 'connections-page' }, '连接工作台'); } }) }));
vi.mock('@/components/WebDavSettings.vue', () => ({ default: defineComponent({ name: 'WebDavSettings', props: ['active'], setup: () => () => h('section', { 'data-test': 'webdav-page' }, 'WebDAV') }) }));
vi.mock('@/stores/systemStore', () => ({ useSystemStore: () => ({ loading: false, refresh: vi.fn() }) }));
vi.mock('@/lib/backend/api', () => ({ backendRuntime: () => 'tauri' }));

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
});

import { flushPromises, mount } from '@vue/test-utils';
import { defineComponent, h } from 'vue';
import { describe, expect, it, vi } from 'vitest';
import ConnectionsPage from '../ConnectionsPage.vue';

const api = vi.hoisted(() => ({ disconnect: vi.fn(), close: vi.fn() }));
vi.mock('../SessionWorkspace.vue', () => ({ default: defineComponent({
  name: 'SessionWorkspace', props: ['initialProfile', 'active'], emits: ['connected', 'closed', 'disconnected'],
  setup(props, { emit, expose }) {
    expose({ disconnect: () => api.disconnect(), close: () => api.close(), requestConnect: () => {} });
    return () => h('button', { 'data-test': 'mock-connect', onClick: () => emit('connected', { id: 's', profile: props.initialProfile || { id: 'c', name: 'server' } }) }, 'connect');
  },
}) }));

describe('会话标签', () => {
  it('复制标签创建独立工作区并提供右键操作', async () => {
    api.close.mockResolvedValue(true); api.disconnect.mockResolvedValue(true);
    const w = mount(ConnectionsPage);
    await w.get('[data-test=mock-connect]').trigger('click');
    await w.get('[data-test=session-tab]').trigger('contextmenu');
    await w.get('[data-test=duplicate-tab]').trigger('click'); await flushPromises();
    expect(w.findAll('[data-test=session-tab]')).toHaveLength(2);
    await w.findAll('[data-test=session-tab]')[0]!.trigger('contextmenu');
    await w.get('[data-test=close-others]').trigger('click'); await flushPromises();
    expect(w.findAll('[data-test=session-tab]')).toHaveLength(1);
    w.unmount();
  });
  it('关闭失败保留标签，断开保留标签', async () => {
    api.close.mockResolvedValue(false); api.disconnect.mockResolvedValue(true);
    const w = mount(ConnectionsPage); await w.get('[data-test=mock-connect]').trigger('click');
    await w.get('[data-test=session-tab]').trigger('contextmenu'); await w.get('[data-test=disconnect-tab]').trigger('click');
    expect(api.disconnect).toHaveBeenCalled(); expect(w.findAll('[data-test=session-tab]')).toHaveLength(1);
    await w.get('[data-test=session-tab]').trigger('contextmenu'); await w.get('[data-test=close-tab]').trigger('click'); await flushPromises();
    await w.get('[data-test=confirm-close-tabs]').trigger('click'); await flushPromises();
    expect(w.findAll('[data-test=session-tab]')).toHaveLength(1); w.unmount();
  });
  it('关闭活动会话前可取消，取消不调用关闭', async () => {
    api.close.mockClear(); const w = mount(ConnectionsPage); await w.get('[data-test=mock-connect]').trigger('click');
    await w.get('[data-test=session-tab]').trigger('contextmenu'); await w.get('[data-test=close-all]').trigger('click');
    await w.get('[data-test=cancel-close-tabs]').trigger('click');
    expect(api.close).not.toHaveBeenCalled(); expect(w.findAll('[data-test=session-tab]')).toHaveLength(1); w.unmount();
  });
  it('切换标签时只有当前工作区处于活动状态，断线标记跟随会话', async () => {
    const w = mount(ConnectionsPage); await w.get('[data-test=mock-connect]').trigger('click');
    await w.get('[data-test=session-tab]').trigger('contextmenu'); await w.get('[data-test=duplicate-tab]').trigger('click'); await flushPromises();
    const sessions = w.findAllComponents({ name: 'SessionWorkspace' });
    expect(sessions.map(s => s.props('active'))).toEqual([false, true]);
    sessions[0]!.vm.$emit('disconnected'); await flushPromises();
    expect(w.findAll('[data-test=session-tab]')[0]!.find('.session-tabs__dot--offline').exists()).toBe(true);
    await w.findAll('[data-test=session-tab]')[0]!.trigger('click'); await flushPromises();
    expect(sessions.map(s => s.props('active'))).toEqual([true, false]);
    await w.setProps({ active: false });
    expect(sessions.map(s => s.props('active'))).toEqual([false, false]);
    w.unmount();
  });
});

import { flushPromises, mount } from '@vue/test-utils';
import { defineComponent, h } from 'vue';
import { expect, it, vi } from 'vitest';
import SessionWorkspace from '../SessionWorkspace.vue';

const api = vi.hoisted(() => ({
  connectionList: vi.fn().mockResolvedValue([]), groupList: vi.fn().mockResolvedValue([]),
  connectionCopy: vi.fn(), connectionDelete: vi.fn(), connectionSave: vi.fn(), groupDelete: vi.fn(), groupSave: vi.fn(),
  serverOverview: vi.fn(), serverOverviewCancel: vi.fn().mockResolvedValue(undefined), sessionClose: vi.fn().mockResolvedValue(undefined),
  sessionRead: vi.fn(), sessionSubmit: vi.fn(), sessionWrite: vi.fn(), sessionResize: vi.fn(), sshCancel: vi.fn(), sshOpen: vi.fn(),
  savedCommandList: vi.fn().mockResolvedValue([]), savedCommandSave: vi.fn(), savedCommandDelete: vi.fn(),
  connectionKeyStatus: vi.fn().mockResolvedValue(false), connectionKeySave: vi.fn(), connectionKeyDelete: vi.fn(),
}));
vi.mock('@/lib/backend/api', () => api);
vi.mock('../TerminalView.vue', () => ({ default: defineComponent({ props: ['active'], setup(props, { expose }) { expose({ activate: () => {}, write: () => {} }); return () => h('div', { 'data-test': 'mock-terminal', 'data-active': String(props.active) }); } }) }));

it('后台标签迟到的连接不会把终端设为活动状态', async () => {
  const profile = { id: 'late', name: '服务器', host: 'example.org', port: 22, username: 'root', authentication: 'password', groupId: null, notes: '', production: false, alwaysConfirm: false, version: 0 } as const;
  let complete!: (result: { status: 'connected'; sessionId: string }) => void;
  api.sshOpen.mockReturnValueOnce(new Promise(resolve => { complete = resolve; }));
  api.sessionRead.mockResolvedValue({ output: '', closed: true });
  api.serverOverview.mockRejectedValue('unavailable');
  const workspace = mount(SessionWorkspace, { props: { active: true, initialProfile: profile } });
  await flushPromises();
  await workspace.get('[data-test=password]').setValue('temporary');
  await workspace.get('[data-test=start-connect]').trigger('click');
  await workspace.setProps({ active: false });
  complete({ status: 'connected', sessionId: 'late-session' });
  await flushPromises();
  expect(workspace.get('[data-test=mock-terminal]').attributes('data-active')).toBe('false');
  workspace.unmount();
});

it('隐藏工作区的对话框不抢占活动工作区焦点，切回时恢复对话框焦点', async () => {
  const host = document.createElement('div'); document.body.appendChild(host);
  const hidden = mount(SessionWorkspace, { attachTo: host, props: { active: true } });
  const visible = mount(SessionWorkspace, { attachTo: host, props: { active: false } });
  await flushPromises();
  await hidden.get('[data-test=new]').trigger('click'); await flushPromises();
  await hidden.setProps({ active: false }); await visible.setProps({ active: true }); await flushPromises();
  const activeButton = visible.get('[data-test=new]').element as HTMLElement;
  activeButton.focus(); await flushPromises(); expect(document.activeElement).toBe(activeButton);
  await visible.setProps({ active: false }); await hidden.setProps({ active: true }); await flushPromises();
  expect(hidden.get('[role=dialog]').element.contains(document.activeElement)).toBe(true);
  hidden.unmount(); visible.unmount(); host.remove();
});

it('卸载隐藏工作区时不恢复旧焦点或抢走活动标签焦点', async () => {
  const host = document.createElement('div'); document.body.appendChild(host);
  const previous = document.createElement('button'); document.body.appendChild(previous); previous.focus();
  const hidden = mount(SessionWorkspace, { attachTo: host, props: { active: true } });
  const visible = mount(SessionWorkspace, { attachTo: host, props: { active: false } });
  await flushPromises(); await hidden.get('[data-test=new]').trigger('click'); await flushPromises();
  await hidden.setProps({ active: false }); await visible.setProps({ active: true }); await flushPromises();
  const button = visible.get('[data-test=new]').element as HTMLElement; button.focus(); await flushPromises();
  hidden.unmount(); await flushPromises(); expect(document.activeElement).toBe(button);
  visible.unmount(); previous.remove(); host.remove();
});

it('切回标签刷新连接列表，不覆盖正在编辑的表单', async () => {
  const profile = { id: 'id', name: '旧名称', host: 'example.org', port: 22, username: 'root', authentication: 'password', groupId: null, notes: '', production: false, alwaysConfirm: false, version: 0 };
  api.connectionList.mockResolvedValueOnce([profile]).mockResolvedValue([{ ...profile, name: '新名称' }]);
  const w = mount(SessionWorkspace, { props: { active: true } }); await flushPromises();
  await w.findAll('button').find(b => b.text() === '编辑')!.trigger('click');
  await w.get('[data-test=name]').setValue('本地编辑中');
  await w.setProps({ active: false }); await w.setProps({ active: true }); await flushPromises();
  expect(api.connectionList).toHaveBeenCalledTimes(2);
  expect((w.get('[data-test=name]').element as HTMLInputElement).value).toBe('本地编辑中');
  await w.findAll('[role=dialog] button').find(b => b.text() === '取消')!.trigger('click');
  expect(w.text()).toContain('新名称'); w.unmount();
});


it('从设置返回后新建命令聚焦名称，重复点击保留草稿', async () => {
  const w = mount(SessionWorkspace, { attachTo: document.body, props: { active: false } });
  await flushPromises(); await w.setProps({ active: true });
  await w.vm.createCommand(); await flushPromises();
  const input = w.get('[data-test=command-name]');
  expect(document.activeElement).toBe(input.element);
  await input.setValue('未保存草稿');
  await w.vm.createCommand(); await flushPromises();
  expect(document.activeElement).toBe(input.element);
  expect((input.element as HTMLInputElement).value).toBe('未保存草稿');
  w.unmount();
});

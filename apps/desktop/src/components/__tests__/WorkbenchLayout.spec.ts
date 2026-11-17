import { mount, flushPromises } from '@vue/test-utils';
import { beforeEach, describe, expect, it } from 'vitest';
import WorkbenchLayout from '../WorkbenchLayout.vue';

describe('工作台面板边界', () => {
  beforeEach(() => { localStorage.clear(); Object.defineProperty(window, 'innerWidth', { value: 1280, configurable: true }); });
  it('收起文件区域保留目录/编辑内容实例，展开不重新挂载', async () => {
    const w = mount(WorkbenchLayout, { slots: { files: '<textarea aria-label="文件草稿">尚未保存</textarea>' } });
    const editor = w.get('textarea').element;
    await w.get('[data-test=toggle-files-panel]').trigger('click');
    expect(w.get('.workbench-file-content').attributes('inert')).toBeDefined();
    expect(w.get('.workbench-center').classes()).toContain('files-collapsed');
    await w.get('[data-test=toggle-files-panel]').trigger('click');
    expect(w.get('textarea').element).toBe(editor);
    expect((editor as HTMLTextAreaElement).value).toBe('尚未保存');
    w.unmount();
  });
  it('分隔条支持键盘和指针调整，限制宽度并持久化', async () => {
    const w = mount(WorkbenchLayout);
    const left = w.get('[aria-label="调整连接栏宽度"]');
    await left.trigger('keydown', { key: 'ArrowRight' });
    expect(left.attributes('aria-valuenow')).toBe('266');
    await left.trigger('pointerdown', { button: 0 });
    window.dispatchEvent(new PointerEvent('pointermove', { clientX: 10000 }));
    window.dispatchEvent(new PointerEvent('pointerup')); await flushPromises();
    expect(left.attributes('aria-valuenow')).toBe('380');
    const horizontal = w.get('[aria-label="调整终端与文件高度"]');
    await horizontal.trigger('keydown', { key: 'ArrowDown' });
    expect(horizontal.attributes('aria-valuenow')).toBe('67');
    expect(localStorage.getItem('coshell.layout.terminal')).toBe('67');
    w.unmount();
    const restored = mount(WorkbenchLayout);
    expect(restored.get('[aria-label="调整连接栏宽度"]').attributes('aria-valuenow')).toBe('380');
    expect(restored.get('[aria-label="调整终端与文件高度"]').attributes('aria-valuenow')).toBe('67');
    restored.unmount();
  });
  it('窄窗口先收起命令栏，再把连接栏作为覆盖面板打开', async () => {
    Object.defineProperty(window, 'innerWidth', { value: 700, configurable: true });
    const w = mount(WorkbenchLayout);
    expect(w.get('.workbench-right').classes()).toContain('is-collapsed');
    expect(w.get('.workbench-left').classes()).toContain('is-collapsed');
    await w.get('[data-test=toggle-connections-sidebar]').trigger('click');
    expect(w.get('.workbench-left').classes()).toContain('is-overlay');
    expect(w.find('.workbench-center').exists()).toBe(true);
    w.unmount();
  });
});

import { flushPromises, mount } from '@vue/test-utils';
import { describe, expect, it, vi } from 'vitest';
import OnlineUpdate from '@/components/OnlineUpdate.vue';

const api = vi.hoisted(() => ({
  desktopUpdateState: vi.fn(),
  desktopUpdateCheck: vi.fn(),
  desktopUpdateDownload: vi.fn(),
  desktopUpdateCancel: vi.fn(),
  desktopUpdateInstall: vi.fn(),
}));
vi.mock('@/lib/backend/api', () => api);

describe('软件更新', () => {
  it('说明 Web 入口不支持在线升级', async () => {
    api.desktopUpdateState.mockResolvedValue({status: 'unsupported', currentVersion: ''});
    const wrapper = mount(OnlineUpdate);
    await flushPromises();
    expect(wrapper.text()).toContain('在线升级仅适用于桌面版');
    expect(wrapper.find('[data-test=update-check]').exists()).toBe(false);
    wrapper.unmount();
  });

  it('轮询下载进度，直到取消在桌面端生效', async () => {
    vi.useFakeTimers();
    let wrapper: ReturnType<typeof mount> | undefined;
    try {
      const available = {status: 'available', currentVersion: '0.1.1', version: '0.1.2'};
      const downloading = {...available, status: 'downloading', operationId: 'active'};
      api.desktopUpdateState.mockResolvedValueOnce(available)
        .mockResolvedValueOnce({...downloading, downloadedBytes: 1048576})
        .mockResolvedValueOnce({...available, error: 'UPDATE_CANCELLED'});
      api.desktopUpdateDownload.mockResolvedValue(downloading);
      api.desktopUpdateCancel.mockResolvedValue(downloading);
      wrapper = mount(OnlineUpdate);
      await flushPromises();
      await wrapper.get('[data-test=update-download]').trigger('click');
      await flushPromises();
      await vi.advanceTimersByTimeAsync(250);
      expect(wrapper.text()).toContain('1.0 MiB 已下载');
      await wrapper.get('[data-test=update-cancel]').trigger('click');
      await flushPromises();
      expect(api.desktopUpdateCancel).toHaveBeenCalledWith('active');
      await vi.advanceTimersByTimeAsync(250);
      expect(wrapper.text()).toContain('已取消本次下载');
      expect(wrapper.find('[data-test=update-download]').exists()).toBe(true);
    } finally {
      wrapper?.unmount();
      vi.useRealTimers();
    }
  });

  it('轮询安装交接，失败后恢复安装按钮', async () => {
    vi.useFakeTimers();
    let wrapper: ReturnType<typeof mount> | undefined;
    try {
      const ready = {status: 'ready', currentVersion: '0.1.1', version: '0.1.2', operationId: 'download'};
      api.desktopUpdateState.mockResolvedValueOnce(ready)
        .mockResolvedValueOnce({...ready, error: 'UPDATE_INSTALL_FAILED'});
      api.desktopUpdateInstall.mockResolvedValue({...ready, status: 'installing'});
      wrapper = mount(OnlineUpdate);
      await flushPromises();
      await wrapper.get('[data-test=update-install]').trigger('click');
      await wrapper.get('[data-test=update-confirm]').trigger('click');
      await flushPromises();
      expect(wrapper.text()).toContain('正在启动安装程序');
      await vi.advanceTimersByTimeAsync(250);
      expect(wrapper.text()).toContain('未能安装更新');
      expect(wrapper.find('[data-test=update-install]').exists()).toBe(true);
    } finally {
      wrapper?.unmount();
      vi.useRealTimers();
    }
  });

  it('等待桌面端确认下载已开始，再允许取消当前操作', async () => {
    const available = {status: 'available', currentVersion: '0.1.1', version: '0.1.2', operationId: 'previous-check'};
    api.desktopUpdateState.mockResolvedValue(available);
    api.desktopUpdateDownload.mockImplementationOnce((id: string) => new Promise((resolve) => {
      acknowledge = () => resolve({...available, operationId: id, status: 'downloading'});
    }));
    let acknowledge!: () => void;
    const wrapper = mount(OnlineUpdate);
    try {
      await flushPromises();
      await wrapper.get('[data-test=update-download]').trigger('click');
      expect(wrapper.find('[data-test=update-cancel]').exists()).toBe(false);
      acknowledge();
      await flushPromises();
      expect(wrapper.find('[data-test=update-cancel]').exists()).toBe(true);
    } finally {
      wrapper.unmount();
    }
  });

  it('下载后保留安装选择，确认才安装，工作未结束时可以重试', async () => {
    const available = {status: 'available', currentVersion: '0.1.1', version: '0.1.2', notes: '<b>发行说明</b>'};
    api.desktopUpdateState.mockResolvedValue({status: 'idle', currentVersion: '0.1.1'});
    api.desktopUpdateCheck.mockResolvedValue(available);
    api.desktopUpdateDownload.mockResolvedValue({...available, status: 'ready', operationId: 'download'});
    api.desktopUpdateInstall.mockResolvedValue({...available, status: 'ready', operationId: 'download', error: 'UPDATE_FILES_ACTIVE', blockingCount: 2});
    const wrapper = mount(OnlineUpdate);
    await flushPromises();
    await wrapper.get('[data-test=update-check]').trigger('click');
    await flushPromises();
    expect(wrapper.text()).toContain('<b>发行说明</b>');
    expect(wrapper.find('b').exists()).toBe(false);
    await wrapper.get('[data-test=update-download]').trigger('click');
    await flushPromises();
    await wrapper.get('[data-test=update-later]').trigger('click');
    expect(api.desktopUpdateInstall).not.toHaveBeenCalled();
    await wrapper.get('[data-test=update-install]').trigger('click');
    expect(api.desktopUpdateInstall).not.toHaveBeenCalled();
    await wrapper.get('[data-test=update-confirm]').trigger('click');
    await flushPromises();
    expect(api.desktopUpdateInstall).toHaveBeenCalledWith('download');
    expect(wrapper.text()).toContain('文件传输');
    expect(wrapper.text()).toContain('当前有 2 项');
    expect(wrapper.find('[data-test=update-install]').exists()).toBe(true);
    wrapper.unmount();
  });
});

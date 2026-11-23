import { describe, expect, it, vi } from 'vitest';
import * as http from '../http';
import * as tauri from '../tauri';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

describe('桌面升级传输契约', () => {
  it('Web 返回明确的不支持状态且不请求网络', async () => {
    const fetch = vi.spyOn(globalThis, 'fetch');
    expect(await http.desktopUpdateState()).toMatchObject({status: 'unsupported'});
    expect(await http.desktopUpdateCheck('check')).toMatchObject({status: 'unsupported'});
    expect(await http.desktopUpdateDownload('download')).toMatchObject({status: 'unsupported'});
    expect(await http.desktopUpdateCancel('download')).toMatchObject({status: 'unsupported'});
    expect(await http.desktopUpdateInstall('download')).toMatchObject({status: 'unsupported'});
    expect(fetch).not.toHaveBeenCalled();
    fetch.mockRestore();
  });

  it('Tauri 返回版本及可用性，调用仅传操作标识', async () => {
    const current = { status: 'current', currentVersion: '0.1.1' };
    const available = { status: 'available', currentVersion: '0.1.1', version: '0.1.2' };
    const missing = { status: 'failed', currentVersion: '0.1.1', error: 'UPDATE_PLATFORM_UNAVAILABLE' };
    invoke.mockResolvedValueOnce(current).mockResolvedValueOnce(available).mockResolvedValueOnce(missing);
    expect(await tauri.desktopUpdateState()).toEqual(current);
    expect(await tauri.desktopUpdateCheck('check')).toEqual(available);
    expect(await tauri.desktopUpdateCheck('check-other')).toEqual(missing);
    expect(invoke).toHaveBeenNthCalledWith(1, 'desktop_update_state', undefined);
    expect(invoke).toHaveBeenNthCalledWith(2, 'desktop_update_check', {operationId: 'check'});
  });
});

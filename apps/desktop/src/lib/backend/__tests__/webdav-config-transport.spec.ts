import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import * as tauri from '@/lib/backend/tauri';
import * as http from '@/lib/backend/http';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

describe('WebDAV config transports', () => {
  beforeEach(() => vi.resetAllMocks());

  it('saves config and looks up the same operation without passing its credential', async () => {
    const input = {url:'https://example.org', remoteDirectory:'/new', username:'new', hasCredential:true};
    vi.mocked(invoke).mockResolvedValueOnce(input).mockResolvedValueOnce('committed_cleanup_pending');
    await expect(tauri.webdavSaveConfig(input, 'test-secret', 'op-1')).resolves.toEqual(input);
    await expect(tauri.webdavConfigSaveStatus('op-1')).resolves.toBe('committed_cleanup_pending');
    expect(invoke).toHaveBeenNthCalledWith(1, 'webdav_save_config', {input, password:'test-secret', operationId:'op-1'});
    expect(invoke).toHaveBeenNthCalledWith(2, 'webdav_config_save_status', {operationId:'op-1'});
  });

  it('reports that the HTTP runtime does not support WebDAV', async () => {
    await expect(http.webdavConfigSaveStatus('op-1')).rejects.toThrow('WEBDAV_CONFIG_UNSUPPORTED');
    const input = {url:'https://example.org', remoteDirectory:'/', username:'', hasCredential:false};
    await expect(http.webdavSaveConfig(input, null, 'op-1')).rejects.toThrow('WEBDAV_CONFIG_UNSUPPORTED');
  });
});

import { mount, flushPromises } from '@vue/test-utils';
import { beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('@/lib/backend/api', () => ({ webdavGetConfig: vi.fn(), webdavSaveConfig: vi.fn(), webdavSync: vi.fn(), webdavReconcile:vi.fn(), webdavSyncStatus: vi.fn(), webdavRecentSyncs: vi.fn(), webdavListSnapshots: vi.fn(), webdavPreviewSnapshot: vi.fn(), webdavApplyRestore: vi.fn(), webdavRestoreStatus: vi.fn(), webdavTestConfig: vi.fn(), webdavTestCancel: vi.fn(), webdavAutoGet: vi.fn(), webdavAutoSet: vi.fn() }));
import * as api from '@/lib/backend/api';
import WebDavSettings from '../WebDavSettings.vue';
const snapshot=(id='latest',createdAt='2026-11-16T03:18:18Z')=>({id,directory:id,name:id,createdAt,appVersion:'0.1.0',formatVersion:1,sizeBytes:1024,protection:'none' as const,containsCredentials:false,state:'structurally_complete' as const,reason:null,restorable:true,source:'independent' as const});
describe('WebDAV', () => {
  beforeEach(() => { vi.clearAllMocks(); vi.mocked(api.webdavGetConfig).mockResolvedValue(null); vi.mocked(api.webdavRecentSyncs).mockResolvedValue([]); vi.mocked(api.webdavListSnapshots).mockResolvedValue({snapshots:[snapshot('latest')],nextCursor:null}); vi.mocked(api.webdavAutoGet).mockResolvedValue([null, 'waiting']); });
  it('坏快照保留显示但不可恢复，不阻断结构完整快照',async()=>{vi.mocked(api.webdavListSnapshots).mockResolvedValue({snapshots:[{...snapshot('bad'),state:'missing_manifest',appVersion:null,formatVersion:null,sizeBytes:null,restorable:false,reason:'manifest 缺失'},snapshot('good')],nextCursor:null,scannedCount:2});const w=mount(WebDavSettings);await flushPromises();const bad=w.get('[data-snapshot-id=bad]');expect(bad.text()).toContain('应用版本未知');expect(bad.text()).toContain('大小未知');expect(bad.get('button').attributes('disabled')).toBeDefined();expect(w.get('[data-snapshot-id=good]').get('button').attributes('disabled')).toBeUndefined();await bad.get('button').trigger('click');expect(api.webdavPreviewSnapshot).not.toHaveBeenCalled();w.unmount();});
  it('加密快照点击恢复后才请求该快照口令',async()=>{vi.mocked(api.webdavListSnapshots).mockResolvedValue({snapshots:[{...snapshot('encrypted'),protection:'encrypted'}],nextCursor:null});vi.mocked(api.webdavPreviewSnapshot).mockResolvedValue({token:'token',protection:'encrypted',connections:[],groups:[],hostKeyCount:0});const w=mount(WebDavSettings);await flushPromises();await w.get('[data-test=restore-snapshot]').trigger('click');expect(api.webdavPreviewSnapshot).not.toHaveBeenCalled();expect(w.find('[aria-label=解锁备份]').exists()).toBe(true);await w.get('[data-test=restore-passphrase]').setValue('snapshot-password');await w.get('[aria-label=解锁备份] form').trigger('submit');await flushPromises();expect(api.webdavPreviewSnapshot).toHaveBeenCalledWith('snapshot-password','encrypted');expect(api.webdavApplyRestore).not.toHaveBeenCalled();expect(w.find('[data-test=restore-passphrase]').exists()).toBe(false);w.unmount();});
  it('旧格式保护未知时可为其填写口令，不假定未加密',async()=>{vi.mocked(api.webdavListSnapshots).mockResolvedValue({snapshots:[{...snapshot('legacy:one'),source:'legacy',protection:'unknown',state:'legacy'}],nextCursor:null});vi.mocked(api.webdavPreviewSnapshot).mockResolvedValue({token:'token',protection:'encrypted',connections:[],groups:[],hostKeyCount:0});const w=mount(WebDavSettings);await flushPromises();await w.get('[data-test=restore-snapshot]').trigger('click');expect(api.webdavPreviewSnapshot).not.toHaveBeenCalled();await w.get('[data-test=restore-passphrase]').setValue('old-password');await w.get('[aria-label=解锁备份] form').trigger('submit');await flushPromises();expect(api.webdavPreviewSnapshot).toHaveBeenCalledWith('old-password','legacy:one');w.unmount();});
  it('分页追加去重并保持降序，旧页错误不清空已有快照',async()=>{vi.mocked(api.webdavListSnapshots).mockResolvedValueOnce({snapshots:[snapshot('new','2026-11-16T00:00:00Z')],nextCursor:'cursor',scannedCount:1}).mockResolvedValueOnce({snapshots:[snapshot('new','2026-11-16T00:00:00Z'),snapshot('old','2026-11-15T00:00:00Z')],nextCursor:null,scannedCount:2});const w=mount(WebDavSettings);await flushPromises();await w.get('[data-test=load-more-versions]').trigger('click');await flushPromises();expect(api.webdavListSnapshots).toHaveBeenLastCalledWith('cursor');expect(w.findAll('[data-snapshot-id]').map(c=>c.attributes('data-snapshot-id'))).toEqual(['new','old']);w.unmount();});
  it('按时间降序展示备份版本并预览所选版本', async () => {
    vi.mocked(api.webdavListSnapshots).mockResolvedValue({snapshots:[snapshot('old','2026-11-11T01:01:01Z'),snapshot('new','2026-11-12T01:01:01Z')],nextCursor:null});
    vi.mocked(api.webdavPreviewSnapshot).mockResolvedValue({token:'token',protection:'encrypted',connections:[],groups:[],hostKeyCount:0});
    const w=mount(WebDavSettings); await flushPromises();
    const options=w.findAll('[data-snapshot-id]');
    expect(options.map(o=>o.attributes('data-snapshot-id'))).toEqual(['new','old']);
    await options[1]!.get('[data-test=restore-snapshot]').trigger('click'); await flushPromises();
    expect(api.webdavPreviewSnapshot).toHaveBeenCalledWith('', 'old');
    expect(api.webdavApplyRestore).not.toHaveBeenCalled(); w.unmount();
  });
  it('配置变化后不显示旧目标的迟到版本', async () => {
    let resolve!: (value: Awaited<ReturnType<typeof api.webdavListSnapshots>>) => void;
    vi.mocked(api.webdavListSnapshots).mockReturnValue(new Promise(r=>{resolve=r;}));
    const w=mount(WebDavSettings); await flushPromises();
    await w.get('[data-test=url]').setValue('https://other.example');
    resolve({snapshots:[snapshot('old')],nextCursor:null}); await flushPromises();
    expect(w.findAll('[data-snapshot-id]')).toHaveLength(0); w.unmount();
  });
  it('在保存按钮右侧检查已保存服务器连接并说明范围', async () => { vi.mocked(api.webdavTestConfig).mockResolvedValue('服务器可访问；未检查写入'); const w=mount(WebDavSettings); await flushPromises(); const save=w.get('[data-test=save-config]'); const check=w.get('[data-test=test-config]'); expect(save.element.nextElementSibling).toBe(check.element); expect(check.text()).toBe('检查服务器连接'); expect(w.text()).toContain('不验证目录内容'); await check.trigger('click'); await flushPromises(); expect(api.webdavTestConfig).toHaveBeenCalled(); expect(w.text()).toContain('未检查写入'); });
  it('配置或密码未保存时不能检查旧服务器配置', async () => { vi.mocked(api.webdavGetConfig).mockResolvedValue({url:'https://old.example',remoteDirectory:'/',username:'user',hasCredential:true}); const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=url]').setValue('https://new.example'); expect(w.get('[data-test=test-config]').attributes('disabled')).toBeDefined(); expect(w.text()).toContain('请先保存配置'); await w.get('[data-test=url]').setValue('https://old.example'); await w.get('[data-test=password]').setValue('new-password'); expect(w.get('[data-test=test-config]').attributes('disabled')).toBeDefined(); expect(api.webdavTestConfig).not.toHaveBeenCalled(); w.unmount(); });
  it('不填备份口令时可同步并显示未加密提示', async () => { vi.mocked(api.webdavSync).mockResolvedValue('同步成功'); const w=mount(WebDavSettings); await flushPromises(); expect(w.text()).toContain('未加密'); await w.get('[data-test=sync]').trigger('click'); await flushPromises(); expect(api.webdavSync).toHaveBeenCalledWith('', expect.any(String)); });
  it('设置自动备份间隔须确认明文', async () => { vi.mocked(api.webdavAutoSet).mockResolvedValue(); const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=auto-interval]').setValue('30'); await w.get('[data-test=accept-plaintext]').setValue(true); await w.get('[data-test=save-auto]').trigger('click'); await flushPromises(); expect(api.webdavAutoSet).toHaveBeenCalledWith(30, true); });
  it('迟到的自动备份设置不会覆盖用户选择', async () => { let resolve!: (v:[number|null,string])=>void; vi.mocked(api.webdavAutoGet).mockReturnValue(new Promise(r=>{resolve=r;})); const w=mount(WebDavSettings); await w.get('[data-test=auto-interval]').setValue('30'); resolve([5,'waiting']); await flushPromises(); expect((w.get('[data-test=auto-interval]').element as HTMLSelectElement).value).toBe('30'); w.unmount(); });
  it('保存密码后清除输入', async () => { vi.mocked(api.webdavSaveConfig).mockResolvedValue(); const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=url]').setValue('https://example.com'); await w.get('[data-test=password]').setValue('secret'); await w.get('form').trigger('submit'); await flushPromises(); expect(api.webdavSaveConfig).toHaveBeenCalled(); expect((w.get('[data-test=password]').element as HTMLInputElement).value).toBe(''); });
  it('恢复前必须预览并确认', async () => { vi.mocked(api.webdavPreviewSnapshot).mockResolvedValue({token:'token',protection:'encrypted',connections:[],groups:[],hostKeyCount:0}); const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=passphrase]').setValue('secret'); await w.get('[data-test=restore-snapshot]').trigger('click'); await flushPromises(); expect(api.webdavApplyRestore).not.toHaveBeenCalled(); expect(w.find('[data-test=apply]').exists()).toBe(true); });
  it('初次配置迟到不能覆盖用户编辑', async () => {
    let resolve!: (v: any) => void; vi.mocked(api.webdavGetConfig).mockReturnValue(new Promise(r => { resolve = r; }));
    const w=mount(WebDavSettings); await w.get('[data-test=url]').setValue('https://new.example');
    resolve({url:'https://old.example',remoteDirectory:'/',username:'old',hasCredential:true}); await flushPromises();
    expect((w.get('[data-test=url]').element as HTMLInputElement).value).toBe('https://new.example'); w.unmount();
  });
  it('保存期间配置字段被禁用且保存的是快照', async () => {
    let resolve!: () => void; vi.mocked(api.webdavSaveConfig).mockReturnValue(new Promise(r => { resolve = r; }));
    const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=url]').setValue('https://example.com'); await w.get('form').trigger('submit'); await flushPromises();
    expect(w.get('[data-test=url]').attributes('disabled')).toBeDefined(); expect(vi.mocked(api.webdavSaveConfig).mock.calls[0]![0].url).toBe('https://example.com');
    resolve(); await flushPromises(); w.unmount();
  });
  it('修改配置或同步会使旧恢复预览失效', async () => {
    vi.mocked(api.webdavPreviewSnapshot).mockResolvedValue({token:'token',protection:'encrypted',connections:[],groups:[],hostKeyCount:0}); vi.mocked(api.webdavSync).mockResolvedValue('成功');
    const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=passphrase]').setValue('secret'); await w.get('[data-test=restore-snapshot]').trigger('click'); await flushPromises();
    await w.get('[data-test=url]').setValue('https://changed.example'); expect(w.find('[data-test=apply]').exists()).toBe(false);
    w.unmount();const next=mount(WebDavSettings);await flushPromises();await next.get('[data-test=restore-snapshot]').trigger('click');await flushPromises();await next.get('[data-test=sync]').trigger('click');await flushPromises();expect(next.find('[data-test=apply]').exists()).toBe(false);next.unmount();
  });
  it('恢复预览展示备份和本地全部连接字段与分组层级', async () => {
    const connection = {id:'c', name:'备份名', host:'h', port:22, username:'u', authentication:'password', group_id:'g', notes:'备份备注', production:true, always_confirm:true, version:1};
    vi.mocked(api.webdavPreviewSnapshot).mockResolvedValue({token:'token',protection:'encrypted',connections:[{id:'c',name:'备份名',status:'conflict',backupHasSecret:true,localHasSecret:true,backup:connection,local:{...connection,name:'本地名',notes:'本地备注',always_confirm:false}}],groups:[{id:'g',status:'conflict',backup:{id:'g',name:'备份子组',parent_id:'parent'},local:{id:'g',name:'本地子组',parent_id:null}}],hostKeyCount:0});
    const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=passphrase]').setValue('secret'); await w.get('[data-test=restore-snapshot]').trigger('click'); await flushPromises();
    const text=w.get('[aria-label=恢复预览]').text(); for(const expected of ['备份名','本地名','备份备注','本地备注','分组 ID：g','始终确认：是','始终确认：否','备份子组','本地子组','上级 ID：parent','上级 ID：无','conflict']) expect(text).toContain(expected); w.unmount();
  });
  it('切离页面清除口令，进行中操作结果仍保留', async () => {
    let resolve!: (v: string) => void; vi.mocked(api.webdavSync).mockReturnValue(new Promise(r => { resolve=r; }));
    const w=mount(WebDavSettings,{props:{active:true}}); await flushPromises(); await w.get('[data-test=passphrase]').setValue('secret'); await w.get('[data-test=sync]').trigger('click');
    await w.setProps({active:false}); expect((w.get('[data-test=passphrase]').element as HTMLInputElement).value).toBe('');
    resolve('同步完成'); await flushPromises(); await w.setProps({active:true}); expect(w.text()).toContain('同步完成'); w.unmount();
  });
  it('可刷新最近同步并查看未知结果与待清理操作标识', async () => {
    vi.mocked(api.webdavRecentSyncs).mockResolvedValueOnce([]).mockResolvedValueOnce([{operationId:'op-123',status:{state:'unknown',error:null,cleanupPending:true,possibleObjects:[]}}]);
    const w=mount(WebDavSettings); await flushPromises(); await w.get('[data-test=refresh-recent]').trigger('click'); await flushPromises();
    expect(w.get('[aria-label=最近同步]').text()).toContain('op-123'); expect(w.text()).toContain('unknown'); expect(w.text()).toContain('待清理'); w.unmount();
  });
  it('未知发布可只读核实同一操作而不新建备份',async()=>{vi.mocked(api.webdavRecentSyncs).mockResolvedValue([{operationId:'unknown-op',status:{state:'unknown',error:null,cleanupPending:false,possibleObjects:[]}}]);vi.mocked(api.webdavReconcile).mockResolvedValue('已核实本次备份完成');const w=mount(WebDavSettings);await flushPromises();await w.get('[data-test=reconcile-backup]').trigger('click');await flushPromises();expect(api.webdavReconcile).toHaveBeenCalledWith('unknown-op');expect(api.webdavSync).not.toHaveBeenCalled();expect(w.text()).toContain('已核实本次备份完成');w.unmount();});
});

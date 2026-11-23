<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue';
import { webdavGetConfig, webdavSaveConfig, webdavConfigSaveStatus, webdavSync, webdavReconcile, webdavSyncStatus, webdavRecentSyncs, webdavListSnapshots, webdavPreviewSnapshot, webdavApplyRestore, webdavRestoreStatus, webdavTestConfig, webdavTestCancel, webdavAutoGet, webdavAutoSet } from '@/lib/backend/api';
import BackupSnapshotCard from './BackupSnapshotCard.vue';
import type { DavChoice, DavConfig, DavPreview, DavSnapshot } from '@/lib/backend/types';
const props = withDefaults(defineProps<{ active?: boolean }>(), { active: true });
const config = ref<DavConfig>({url:'',remoteDirectory:'/',username:'',hasCredential:false});
const savedConfig = ref<DavConfig>({...config.value});
const password=ref(''); const passphrase=ref(''); const busy=ref(false); const message=ref('');
const pendingSave = ref<{operationId: string; metadata?: DavConfig} | null>(null);
const pendingCleanup = ref<string | null>(null);
const targetChanged = computed(() => config.value.url !== savedConfig.value.url || config.value.username !== savedConfig.value.username);
const needsNewPassword = computed(() => savedConfig.value.hasCredential && targetChanged.value);
const configDirty = computed(() => !!pendingSave.value || !!password.value || targetChanged.value || config.value.remoteDirectory !== savedConfig.value.remoteDirectory);
const autoInterval=ref(''); const autoStatus=ref(''); const acceptPlaintext=ref(false); const testOperation=ref('');
let autoRevision=0; let autoGeneration=0;
function editAuto() { autoRevision++; }
const preview=ref<DavPreview|null>(null); const choices=ref<DavChoice[]>([]); const confirmGroups=ref(false); const acceptRestoreRisk=ref(false);
const snapshots=ref<DavSnapshot[]>([]); const nextCursor=ref<string|null>(null); const versionError=ref(''); const versionLoading=ref(false); const scannedCount=ref(0); const restoringSnapshot=ref<DavSnapshot|null>(null); const restorePassphrase=ref(''); const previewName=ref(''); const previewSource=ref<DavSnapshot|null>(null); let versionGeneration=0;
const compareSnapshots=(a:DavSnapshot,b:DavSnapshot)=>(b.createdAt??'').localeCompare(a.createdAt??'')||a.directory.localeCompare(b.directory)||a.id.localeCompare(b.id);
async function refreshVersions(){const generation=++versionGeneration;versionLoading.value=true;versionError.value='';nextCursor.value=null;snapshots.value=[];scannedCount.value=0;try{const page=await webdavListSnapshots();if(disposed||generation!==versionGeneration)return;snapshots.value=[...page.snapshots].sort(compareSnapshots);nextCursor.value=page.nextCursor;scannedCount.value=page.scannedCount??page.snapshots.length;}catch(e){if(!disposed&&generation===versionGeneration)versionError.value=typeof e==='string'?e:'无法读取备份';}finally{if(generation===versionGeneration)versionLoading.value=false;}}
async function loadMoreVersions(){const cursor=nextCursor.value;if(!cursor||versionLoading.value)return;const generation=versionGeneration;versionLoading.value=true;try{const page=await webdavListSnapshots(cursor);if(disposed||generation!==versionGeneration)return;const seen=new Set(snapshots.value.map(v=>v.id));snapshots.value=[...snapshots.value,...page.snapshots.filter(v=>!seen.has(v.id))].sort(compareSnapshots);nextCursor.value=page.nextCursor;scannedCount.value=Math.max(scannedCount.value,page.scannedCount??snapshots.value.length);}catch(e){if(!disposed&&generation===versionGeneration)versionError.value=typeof e==='string'?e:'无法读取更多备份';}finally{if(generation===versionGeneration)versionLoading.value=false;}}
function requestRestore(snapshotId:string){if(busy.value||configDirty.value)return;const item=snapshots.value.find(s=>s.id===snapshotId);if(!item?.restorable)return;preview.value=null;restoringSnapshot.value=item;restorePassphrase.value='';message.value='';if(item.protection==='none')void prepareSnapshot();}
function cancelRestore(){if(busy.value)return;restoringSnapshot.value=null;restorePassphrase.value='';}
async function prepareSnapshot(){const item=restoringSnapshot.value;if(!item||busy.value||configDirty.value)return;if(item.protection==='encrypted'&&!restorePassphrase.value){message.value='请输入该备份的口令';return;}const generation=versionGeneration;busy.value=true;message.value='';preview.value=null;try{const result=await webdavPreviewSnapshot(item.protection!=='none'?restorePassphrase.value:'',item.id);if(disposed||generation!==versionGeneration)return;preview.value=result;previewName.value=item.name||item.directory;previewSource.value={...item};choices.value=result.connections.map(c=>({id:c.id,action:c.status==='new'?'replace':'keep'}));confirmGroups.value=false;acceptRestoreRisk.value=false;restoringSnapshot.value=null;}catch(e){if(!disposed&&generation===versionGeneration)failure(e);}finally{restorePassphrase.value='';busy.value=false;}}
const recent=ref<Awaited<ReturnType<typeof webdavRecentSyncs>>>([]); const recentError=ref(''); const recentLoading=ref(false);
async function refreshRecent() { if (recentLoading.value) return; recentLoading.value=true; recentError.value=''; try { recent.value=await webdavRecentSyncs(); } catch(e) { recentError.value=typeof e==='string'?e:'无法读取同步记录'; } finally { recentLoading.value=false; } }
async function reconcile(operationId:string){if(busy.value||configDirty.value)return;busy.value=true;message.value='';try{message.value=await webdavReconcile(operationId);void refreshVersions();}catch(e){failure(e);}finally{busy.value=false;void refreshRecent();}}
const unsupportedConfigMessage = '当前 Web 模式暂不支持 WebDAV 配置，请使用桌面版。';
function failure(e:unknown) { const raw=typeof e==='string'?e:e instanceof Error?e.message:'操作失败'; if (raw === 'WEBDAV_CONFIG_UNSUPPORTED') { message.value = unsupportedConfigMessage; return; } const reasons: Record<string,string>={UnsupportedAtomicCreate:'服务器不支持原子的条件创建（If-None-Match）',UnsupportedAtomicUpdate:'服务器不支持原子的条件更新（If-Match）',UnsupportedReadback:'服务器读取结果与写入不一致',UnsupportedStrongEtag:'服务器未提供可用于条件更新的强 ETag'}; const found=Object.entries(reasons).find(([code])=>raw.includes(code)); message.value=found ? `${found[1]}。${raw}` : raw; }
let revision=0; let loadGeneration=0; let disposed=false;
watch(config, () => { revision++; preview.value=null; versionGeneration++; versionLoading.value=false; snapshots.value=[]; nextCursor.value=null; restoringSnapshot.value=null; restorePassphrase.value=''; }, {deep:true,flush:'sync'});
watch(() => props.active, active => { if (!active) { password.value=''; passphrase.value=''; restorePassphrase.value=''; } else { void loadAuto(); } });
async function load() { const generation=++loadGeneration; const before=revision; try { const result=await webdavGetConfig(); if(!disposed && generation===loadGeneration && result) { savedConfig.value={...result}; if(revision===before) config.value={...result}; } } catch(e) { if (!disposed && generation===loadGeneration) failure(e); } }
const passwordRequiredMessage = '请输入新目标的密码。服务器地址或用户名变更时需要重新填写密码。';

function showSavedConfig(saved: DavConfig, cleanupOperation?: string) {
  const {cleanupPending: _cleanupPending, ...profile} = saved;
  savedConfig.value = {...profile};
  config.value = {...profile};
  pendingSave.value = null;
  if (cleanupOperation) pendingCleanup.value = cleanupOperation;
  message.value = '配置已保存';
  void refreshVersions();
}

async function resolveSaveStatus({operationId, metadata}: NonNullable<typeof pendingSave.value>) {
  try {
    if (metadata) {
      const saved = await webdavGetConfig();
      if (disposed) return;
      if (saved && saved.url === metadata.url && saved.username === metadata.username && saved.remoteDirectory === metadata.remoteDirectory) {
        showSavedConfig(saved);
      } else {
        if (saved) savedConfig.value = {...saved};
        pendingSave.value = null;
        message.value = '配置未保存。请检查当前配置后重试。';
      }
      return;
    }
    const status = await webdavConfigSaveStatus(operationId);
    if (disposed) return;
    switch (status) {
      case 'committed':
      case 'committed_cleanup_pending': {
        const saved = await webdavGetConfig();
        if (disposed) return;
        if (saved) {
          showSavedConfig(saved, status === 'committed_cleanup_pending' ? operationId : undefined);
          return;
        }
        break;
      }
      case 'not_found':
      case 'aborted':
      case 'aborted_retained_runs':
        pendingSave.value = null;
        message.value = status === 'aborted_retained_runs'
          ? '配置未保存，原配置仍可用。凭据清理待完成，可重新填写后重试。'
          : '配置未保存，原配置仍可用。请重新填写后重试。';
        return;
    }
  } catch {
    if (disposed) return;
  }
  message.value = `保存结果暂不可确认。操作标识：${operationId}`;
}

async function checkSaveStatus() {
  if (busy.value || !pendingSave.value) return;
  busy.value = true;
  try {
    await resolveSaveStatus(pendingSave.value);
  } finally {
    busy.value = false;
  }
}

async function checkCleanupStatus() {
  if (busy.value || !pendingCleanup.value) return;
  busy.value = true;
  try {
    const status = await webdavConfigSaveStatus(pendingCleanup.value);
    if (disposed) return;
    if (status === 'committed') {
      pendingCleanup.value = null;
      message.value = '旧凭据已清理。';
    } else {
      message.value = '旧凭据清理尚未确认。';
    }
  } catch {
    if (!disposed) message.value = '暂时无法核实旧凭据清理状态。';
  } finally {
    busy.value = false;
  }
}

async function save() {
  if (busy.value || pendingSave.value) return;
  if (needsNewPassword.value && !password.value) {
    message.value = passwordRequiredMessage;
    return;
  }
  busy.value = true;
  preview.value = null;
  message.value = '';
  const snapshot = {...config.value};
  const operationId = crypto.randomUUID();
  const secret = password.value || null;
  password.value = '';
  loadGeneration++;
  try {
    const saved = await webdavSaveConfig(snapshot, secret, operationId);
    if (!disposed) showSavedConfig(saved, saved.cleanupPending ? operationId : undefined);
  } catch (e) {
    if (disposed) return;
    const code = typeof e === 'string' ? e : e instanceof Error ? e.message : '';
    if (code === 'WEBDAV_PASSWORD_REQUIRED') {
      message.value = passwordRequiredMessage;
    } else if (code === 'WEBDAV_CONFIG_UNSUPPORTED') {
      message.value = unsupportedConfigMessage;
    } else if (code === 'WEBDAV_CONFIG_SAVE_FAILED') {
      message.value = 'WebDAV 配置保存失败。请检查当前配置后重试。';
    } else {
      pendingSave.value = secret === null ? {operationId, metadata: snapshot} : {operationId};
      await resolveSaveStatus(pendingSave.value);
    }
  } finally {
    busy.value = false;
  }
}
async function sync() { if(busy.value || configDirty.value)return; busy.value=true; preview.value=null; message.value=''; const operation=crypto.randomUUID(); try { message.value=await webdavSync(passphrase.value,operation); void refreshVersions(); } catch(e) { failure(e); try { const status=await webdavSyncStatus(operation); if(status) message.value+=`（操作 ${operation}：${status.state}${status.cleanupPending?'，远端临时对象待清理':''}）`; } catch { message.value+=`（操作 ${operation} 状态暂不可查询）`; } } finally { passphrase.value=''; busy.value=false; void refreshRecent(); } }
async function testConfig() { if(busy.value || configDirty.value)return; busy.value=true; message.value=''; const id=crypto.randomUUID(); testOperation.value=id; try { message.value=await webdavTestConfig(id); } catch(e) { failure(e); } finally { testOperation.value=''; busy.value=false; } }
async function cancelTest() { if(testOperation.value) { try { await webdavTestCancel(testOperation.value); } catch(e) { failure(e); } } }
async function loadAuto() { const generation=++autoGeneration; const before=autoRevision; try { const [interval,status]=await webdavAutoGet(); if(!disposed && generation===autoGeneration) { if(before===autoRevision) autoInterval.value=interval?.toString() ?? ''; autoStatus.value=status; } } catch(e) { if(!disposed && generation===autoGeneration) failure(e); } }
async function saveAuto() { if(busy.value)return; const interval=autoInterval.value ? Number(autoInterval.value) : null; if(interval && !acceptPlaintext.value) { message.value='请确认自动备份会明文上传连接信息和已保存凭据。'; return; } busy.value=true; try { await webdavAutoSet(interval,acceptPlaintext.value); await loadAuto(); message.value='自动备份设置已保存'; } catch(e) { failure(e); message.value+=' 当前进程已暂停自动备份；旧设置可能在重启后恢复，请重试保存。'; } finally { busy.value=false; } }
async function apply() { if(!preview.value||busy.value||!acceptRestoreRisk.value)return; busy.value=true; message.value=''; const token=preview.value.token; try { const ready=choices.value.map(c=>c.action==='copy'?{...c,copyId:crypto.randomUUID(),copyName:`${preview.value!.connections.find(x=>x.id===c.id)?.name} 副本`}:c); await webdavApplyRestore(token,ready,confirmGroups.value); preview.value=null; message.value='恢复已完成'; } catch(e) { preview.value=null; failure(e); try { const status=await webdavRestoreStatus(token); if(status) message.value+=` 恢复状态：${status.state}${status.cleanupPending?'，凭据清理待完成':''}；操作标识：${token}。`; } catch { message.value+=` 恢复状态暂不可查询，操作标识：${token}。`; } message.value+=' 请核对本地数据后再操作。'; } finally { busy.value=false; } }
onMounted(() => { void load().then(() => refreshVersions()); void refreshRecent(); void loadAuto(); }); onUnmounted(()=>{disposed=true;loadGeneration++;versionGeneration++;password.value='';passphrase.value='';restorePassphrase.value=''; if(testOperation.value) void webdavTestCancel(testOperation.value).catch(()=>{});});
</script>
<template>
  <section class="capability-section webdav-settings"><div class="page-heading"><div><p class="eyebrow">BACKUP</p><h1>WebDAV 备份与恢复</h1></div></div>
    <p>同步范围：连接信息及已保存的连接凭据。填写备份口令时加密；留空时未加密，WebDAV 服务端可直接读取全部备份内容。</p><p v-if="message" role="status">{{ message }}</p>
    <form class="dialog" @submit.prevent="save">
      <fieldset :disabled="busy || !!pendingSave">
        <label>服务器地址<input v-model.trim="config.url" data-test="url" :disabled="busy || !!pendingSave" type="url" required placeholder="https://" /></label>
        <label>远程目录<input v-model.trim="config.remoteDirectory" data-test="remote-directory" required /></label>
        <label>用户名<input v-model="config.username" data-test="username" /></label>
        <label>WebDAV 密码<input v-model="password" data-test="password" type="password" autocomplete="new-password" :placeholder="needsNewPassword ? '请输入新目标的密码' : savedConfig.hasCredential ? '已保存，留空保持不变' : ''" /></label>
        <p v-if="needsNewPassword">更换服务器地址或用户名时，请填写新目标的密码。</p>
        <div class="webdav-settings__actions">
          <button data-test="save-config" type="submit" :disabled="busy || !!pendingSave">保存配置</button>
          <button data-test="test-config" type="button" :disabled="busy || configDirty" @click="testConfig">检查服务器连接</button>
        </div>
        <p v-if="configDirty && !pendingSave" role="status">配置有未保存的更改，请先保存配置再检查。</p>
        <p class="webdav-settings__hint">使用已保存的服务器配置发送只读 PROPFIND 并检查响应；不验证目录内容、认证是否被服务端实际强制、写入或条件更新。</p>
      </fieldset>
      <button v-if="pendingSave" data-test="check-config-save-status" type="button" :disabled="busy" @click="checkSaveStatus">核实保存结果</button>
      <div v-if="pendingCleanup" class="webdav-settings__actions">
        <p role="status">旧凭据待清理。操作标识：{{ pendingCleanup }}</p>
        <button data-test="check-config-cleanup-status" type="button" :disabled="busy" @click="checkCleanupStatus">核实清理状态</button>
      </div>
      <button v-if="testOperation" type="button" @click="cancelTest">取消检查</button>
    </form>
    <div class="dialog"><h2>创建备份</h2><label>备份口令（可选）<input v-model="passphrase" data-test="passphrase" type="password" autocomplete="off" :disabled="busy" /></label><p>口令仅用于本次备份。留空会生成未加密备份，远端可直接读取连接信息和其中已保存的 SSH 密码及其他连接凭据。</p><div class="dialog-actions"><button data-test="sync" class="primary-button" type="button" :disabled="busy || configDirty" @click="sync">备份连接信息</button></div></div>
    <section class="dialog snapshot-browser" aria-label="查看备份"><div class="backup-versions__header"><div><h2>查看备份</h2><p>按创建时间倒序 · 结构检查通过后可进入恢复</p></div><button data-test="refresh-snapshots" type="button" :disabled="busy || versionLoading || configDirty" @click="refreshVersions">刷新备份</button></div><p v-if="configDirty" role="status">请先保存配置，再查看该目标的备份。</p><p v-if="versionError" role="alert">{{versionError}}</p><p v-if="versionLoading" role="status">正在检查备份…</p><p v-else-if="!snapshots.length && !versionError" class="snapshot-empty">暂无备份</p><div class="snapshot-list"><BackupSnapshotCard v-for="snapshot in snapshots" :key="snapshot.id" :snapshot="snapshot" :disabled="busy || configDirty" @restore="requestRestore" /></div><div class="snapshot-browser__footer"><span v-if="snapshots.length">已显示 {{snapshots.length}} 个条目 · 已扫描 {{scannedCount}} 个目录</span><button v-if="nextCursor" data-test="load-more-versions" type="button" :disabled="busy || versionLoading || configDirty" @click="loadMoreVersions">加载更多</button></div></section>
    <div v-if="restoringSnapshot" class="dialog snapshot-unlock" role="region" aria-label="解锁备份"><h2>恢复 {{restoringSnapshot.name || restoringSnapshot.directory}}</h2><p>{{restoringSnapshot.protection==='unknown'?'旧格式的保护方式尚未确定；如备份使用口令请填写，否则留空。':'请输入该备份的口令。'}}完整校验通过后再展示恢复差异。</p><form @submit.prevent="prepareSnapshot"><label>备份口令<input v-model="restorePassphrase" data-test="restore-passphrase" type="password" autocomplete="off" :disabled="busy" /></label><div class="dialog-actions"><button type="button" :disabled="busy" @click="cancelRestore">取消</button><button data-test="unlock-snapshot" type="submit" :disabled="busy || (restoringSnapshot.protection==='encrypted' && !restorePassphrase)">校验并预览</button></div></form></div>
    <div class="dialog"><h2>自动备份</h2><label>备份间隔<select v-model="autoInterval" data-test="auto-interval" :disabled="busy" @change="editAuto"><option value="">关闭</option><option value="5">5 分钟</option><option value="10">10 分钟</option><option value="30">30 分钟</option><option value="60">60 分钟</option><option value="1440">24 小时</option></select></label><label v-if="autoInterval" class="check-label"><input v-model="acceptPlaintext" data-test="accept-plaintext" type="checkbox" />我了解自动备份未加密，远端可直接读取连接信息和其中已保存的 SSH 密码及其他连接凭据</label><p>仅应用运行时执行；重启后等待完整间隔。状态：{{ autoStatus }}</p><button data-test="save-auto" type="button" :disabled="busy" @click="saveAuto">保存自动备份设置</button></div>
    <div v-if="preview" class="dialog" role="region" aria-label="恢复预览">
      <h2>恢复预览 · {{previewName}}</h2><p v-if="previewSource" class="snapshot-preview-source">来源：{{savedConfig.url}} · {{savedConfig.remoteDirectory}} / {{previewSource.directory}}；快照标识：{{previewSource.id}}</p><p v-if="preview.manifestSha256" class="snapshot-preview-source">已校验清单摘要：{{preview.manifestSha256}}</p><p>确认前不会修改本地连接。备份中的主机指纹不会自动信任，首次连接需重新确认。</p><p v-if="preview.protection === 'none'" role="alert">此备份未加密，也不能验证来源真实性；远端内容可能被更改。</p><p v-if="!preview.connections.length">备份中没有连接。</p>
      <div v-for="(item,index) in preview.connections" :key="item.id"><strong>{{ item.name }}</strong> · {{ item.status }}
        <dl v-for="entry in [{ label: '备份', value: item.backup }, ...(item.local ? [{ label: '本地', value: item.local }] : [])]" :key="entry.label">
          <dt>{{ entry.label }}</dt><dd>名称：{{ entry.value.name }}；主机：{{ entry.value.username }}@{{ entry.value.host }}:{{ entry.value.port }}；认证：{{ entry.value.authentication }}；分组 ID：{{ entry.value.group_id || '未分组' }}；环境：{{ entry.value.production ? '生产' : '普通' }}；始终确认：{{ entry.value.always_confirm ? '是' : '否' }}；备注：{{ entry.value.notes || '无' }}</dd>
        </dl>
        <p>备份凭据：{{ item.backupHasSecret ? '包含' : '不包含' }}；本地凭据：{{ item.localHasSecret ? '已保存' : '无' }}。选择使用备份可能替换已有凭据。</p><select v-if="choices[index]" v-model="choices[index]!.action" :aria-label="`${item.name} 的恢复方式`"><option value="keep">保留本地</option><option value="replace">使用备份</option><option value="copy">创建副本</option></select>
      </div>
      <div v-if="preview.groups.length"><p>备份分组：</p><ul><li v-for="group in preview.groups" :key="group.id">{{ group.status }}：备份 {{ group.backup.name }}（ID：{{ group.backup.id }}；上级 ID：{{ group.backup.parent_id || '无' }}）<span v-if="group.local">；本地 {{ group.local.name }}（ID：{{ group.local.id }}；上级 ID：{{ group.local.parent_id || '无' }}）</span></li></ul><label class="check-label"><input v-model="confirmGroups" type="checkbox" />允许恢复列出的新分组</label></div>
      <label class="check-label"><input v-model="acceptRestoreRisk" type="checkbox" />我已核对备份来源、连接变化与凭据替换</label><div class="dialog-actions"><button type="button" :disabled="busy" @click="preview=null">取消</button><button data-test="apply" type="button" :disabled="busy || !acceptRestoreRisk" @click="apply">确认恢复</button></div>
    </div>
    <section class="dialog" aria-label="最近同步"><h2>最近同步</h2><button type="button" data-test="refresh-recent" :disabled="recentLoading" @click="refreshRecent">刷新记录</button><p v-if="recentError" role="status">{{ recentError }}</p><p v-else-if="!recent.length">暂无同步记录</p><ul v-else><li v-for="item in recent" :key="item.operationId">操作 {{ item.operationId }}：{{ item.status.state }}<span v-if="item.status.cleanupPending">，远端临时对象待清理</span><span v-if="item.status.error">，{{ item.status.error }}</span><button v-if="['unknown','running'].includes(item.status.state)" data-test="reconcile-backup" type="button" :disabled="busy||configDirty" @click="reconcile(item.operationId)">核实结果</button></li></ul></section>
  </section>
</template>

<style scoped>
.webdav-settings > .dialog { margin-top: 20px; }
.webdav-settings__actions { display: flex; flex-wrap: wrap; gap: 10px; margin-top: 16px; }
.webdav-settings__hint { margin: 12px 0 0; }
.webdav-settings :deep(fieldset) { border: 0; margin: 0; padding: 0; min-width: 0; }
.webdav-settings > .dialog { width: min(720px, 100%); max-height: none; overflow: visible; border: 1px solid var(--border); box-shadow: none; }
.webdav-settings button { border: 1px solid var(--border); border-radius: 6px; background: var(--surface); padding: 8px 12px; cursor: pointer; }
.webdav-settings button:disabled { cursor: not-allowed; opacity: .55; }
.webdav-settings button.primary-button { background: var(--accent-button); color: white; border-color: var(--accent-button); }
.backup-versions { margin-top: 20px; padding-top: 16px; border-top: 1px solid var(--border); }
.backup-versions__header { display:flex; align-items:center; justify-content:space-between; gap:12px; }
.backup-versions__header h3 { margin:0; font-size:14px; }
.snapshot-list { display:grid; gap:10px; margin-top:14px; }
.snapshot-browser__footer { display:flex; justify-content:space-between; align-items:center; gap:12px; margin-top:16px; font-size:12px; color:var(--muted); }
.snapshot-empty { padding:28px; text-align:center; border:1px dashed var(--border); border-radius:6px; color:var(--muted); }
.backup-versions__list { display:grid; gap:4px; margin:12px 0 !important; max-height:240px; overflow:auto; }
.backup-versions__list legend { color:var(--muted); font-size:12px; }
.backup-versions__item { display:flex !important; align-items:center; gap:10px; margin:0 !important; padding:9px 10px; border:1px solid var(--border); border-radius:6px; font-weight:400 !important; }
.backup-versions__item small { margin-left:8px; color:var(--accent); }
</style>

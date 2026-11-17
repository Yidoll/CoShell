import type {DavSnapshot,DavSnapshotPage,DavSnapshotState}from'./types';
const states=new Set<DavSnapshotState>(['structurally_complete','missing_manifest','invalid_manifest','unsupported_format','incomplete','missing_connections','missing_credentials','integrity_mismatch','unavailable','legacy','legacy_unconfirmed']);
export function decodeSnapshotPage(value:unknown):DavSnapshotPage{
 const invalid=()=>new Error('备份列表响应格式不兼容，请升级到匹配的客户端后重试。');
 if(!value||typeof value!=='object'||Array.isArray(value))throw invalid();const page=value as Record<string,unknown>;
 if(!Array.isArray(page.snapshots)||page.snapshots.length>50||!(page.nextCursor===null||(typeof page.nextCursor==='string'&&page.nextCursor.length>0&&page.nextCursor.length<=4096))||!(page.scannedCount===undefined||(typeof page.scannedCount==='number'&&Number.isSafeInteger(page.scannedCount)&&page.scannedCount>=0)))throw invalid();
 for(const raw of page.snapshots){
  if(!raw||typeof raw!=='object'||Array.isArray(raw))throw invalid();const s=raw as Record<string,unknown>;
  if(typeof s.id!=='string'||!s.id||s.id.length>512||typeof s.directory!=='string'||s.directory.length>256||typeof s.name!=='string'||s.name.length>512||!(s.createdAt===null||(typeof s.createdAt==='string'&&s.createdAt.length<=128&&Number.isFinite(Date.parse(s.createdAt))))||!(s.appVersion===null||(typeof s.appVersion==='string'&&s.appVersion.length<=64))||!(s.formatVersion===null||(typeof s.formatVersion==='number'&&Number.isSafeInteger(s.formatVersion)&&s.formatVersion>=0))||!(s.sizeBytes===null||(typeof s.sizeBytes==='number'&&Number.isSafeInteger(s.sizeBytes)&&s.sizeBytes>=0))||!['none','encrypted','unknown'].includes(String(s.protection))||!(s.containsCredentials===null||typeof s.containsCredentials==='boolean')||!states.has(s.state as DavSnapshotState)||!(s.reason===null||(typeof s.reason==='string'&&s.reason.length<=4096))||typeof s.restorable!=='boolean'||!['independent','legacy'].includes(String(s.source)))throw invalid();
  if(s.restorable&&!['structurally_complete','legacy'].includes(String(s.state)))throw invalid();
 }
 return {snapshots:page.snapshots as DavSnapshot[],nextCursor:page.nextCursor as string|null,...(page.scannedCount===undefined?{}:{scannedCount:page.scannedCount as number})};
}

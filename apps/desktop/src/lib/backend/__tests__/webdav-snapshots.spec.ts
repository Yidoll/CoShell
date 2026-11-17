import {describe,expect,it}from'vitest';
import {decodeSnapshotPage}from'../webdav-snapshots';
const item={id:'snapshot:20261116031818',directory:'20261116031818',name:'备份',createdAt:null,appVersion:null,formatVersion:null,sizeBytes:null,protection:'unknown',containsCredentials:null,state:'missing_manifest',reason:'manifest 缺失',restorable:false,source:'independent'};
describe('快照卡片契约',()=>{
 it('接受未知元数据的异常条目，不套用默认大小或版本',()=>{expect(decodeSnapshotPage({snapshots:[item],nextCursor:null,scannedCount:1})).toEqual({snapshots:[item],nextCursor:null,scannedCount:1});});
 it('不接受旧版versions形状或错误可恢复状态',()=>{expect(()=>decodeSnapshotPage({versions:[],nextCursor:null})).toThrow();expect(()=>decodeSnapshotPage({snapshots:[{...item,restorable:true}],nextCursor:null})).toThrow();expect(()=>decodeSnapshotPage({snapshots:[{...item,sizeBytes:-1}],nextCursor:null})).toThrow();});
});

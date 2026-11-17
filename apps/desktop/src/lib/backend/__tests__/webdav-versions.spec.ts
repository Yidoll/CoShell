import {describe,expect,it} from 'vitest';
import {decodeVersionPage} from '../webdav-versions';
describe('WebDAV 版本响应',()=>{
 it('接收分页对象，拒绝旧数组响应并提示版本不兼容',()=>{const item={id:'snap-1',timestamp:'20261116031818',latest:true};expect(decodeVersionPage({versions:[item],nextCursor:null})).toEqual({versions:[item],nextCursor:null});expect(()=>decodeVersionPage([item])).toThrow('备份版本响应格式不兼容');});
 it('拒绝缺失分页字段、无效版本和游标',()=>{for(const payload of [{versions:[]},{versions:null,nextCursor:null},{versions:[{id:'s',timestamp:'yesterday',latest:true}],nextCursor:null},{versions:[],nextCursor:3}])expect(()=>decodeVersionPage(payload)).toThrow('备份版本响应格式不兼容');});
});

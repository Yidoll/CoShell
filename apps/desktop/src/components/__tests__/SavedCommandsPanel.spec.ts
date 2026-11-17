import { mount, flushPromises } from '@vue/test-utils';
import { beforeEach, describe, expect, it, vi } from 'vitest';
vi.mock('@/lib/backend/api',()=>({savedCommandList:vi.fn(),savedCommandSave:vi.fn(),savedCommandDelete:vi.fn()}));
import * as api from '@/lib/backend/api';
import SavedCommandsPanel from '../SavedCommandsPanel.vue';
const command={id:'one',name:'检查主机',content:'uname -a',category:'默认分类',version:0};
describe('命令管理',()=>{
 beforeEach(()=>{vi.resetAllMocks();vi.mocked(api.savedCommandList).mockResolvedValue([command]);});
 it('双击仅提交当前选定的完整命令，断线阻止执行',async()=>{const w=mount(SavedCommandsPanel,{props:{disabled:false}});await flushPromises();await w.get('[data-test=command-item]').trigger('dblclick');expect(w.emitted('run')?.[0]).toEqual([command]);await w.setProps({disabled:true});await w.get('[data-test=command-item]').trigger('dblclick');expect(w.emitted('run')).toHaveLength(1);w.unmount();});
 it('创建时保存名称和命令，执行不在持久化方法内发生',async()=>{vi.mocked(api.savedCommandSave).mockResolvedValue({...command,id:'new'});const w=mount(SavedCommandsPanel,{props:{disabled:false}});await flushPromises();await w.get('[data-test=new-command]').trigger('click');await w.get('[data-test=command-name]').setValue('检查主机');await w.get('[data-test=command-content]').setValue('uname -a');await w.get('form').trigger('submit');await flushPromises();expect(api.savedCommandSave).toHaveBeenCalledWith(expect.objectContaining({name:'检查主机',content:'uname -a'}));expect(w.emitted('run')).toBeUndefined();w.unmount();});
});

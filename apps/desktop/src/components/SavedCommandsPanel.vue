<script setup lang="ts">
import { computed,nextTick,onMounted,ref } from 'vue';
import { savedCommandList,savedCommandSave,savedCommandDelete } from '@/lib/backend/api';
import type { SavedCommand } from '@/lib/backend/types';
const props=defineProps<{disabled:boolean}>(); const emit=defineEmits<{run:[command:SavedCommand]}>();
const items=ref<SavedCommand[]>([]); const editor=ref<SavedCommand|null>(null); const selected=ref<SavedCommand|null>(null);const search=ref('');const error=ref('');const busy=ref(false);const removing=ref(false);
const visible=computed(()=>items.value.filter(c=>`${c.name} ${c.category}`.toLowerCase().includes(search.value.toLowerCase())));
const nameInput=ref<HTMLInputElement>();
const dirty=computed(()=>!!editor.value && (selected.value ? ['name','content','category'].some(key=>editor.value![key as keyof SavedCommand]!==selected.value![key as keyof SavedCommand]) : !!(editor.value.name||editor.value.content||editor.value.category!=='默认分类')));
function message(e:unknown){return typeof e==='string'?e:e instanceof Error?e.message:'命令操作失败';}
async function load(){try{items.value=await savedCommandList();}catch(e){error.value=message(e);}}
async function create(){if(busy.value)return;if(dirty.value){error.value='已有未保存的命令，请先保存或取消编辑。';await nextTick();nameInput.value?.focus();return;}editor.value={id:crypto.randomUUID(),name:'',content:'',category:'默认分类',version:0};selected.value=null;removing.value=false;error.value='';await nextTick();nameInput.value?.focus();}
function select(item:SavedCommand){selected.value=item;editor.value={...item};removing.value=false;error.value='';}
function run(item:SavedCommand){if(!props.disabled&&!busy.value)emit('run',{...item});}
async function save(){if(!editor.value||busy.value)return;busy.value=true;error.value='';try{const value=await savedCommandSave({...editor.value});editor.value=value;selected.value=value;await load();}catch(e){error.value=message(e);}finally{busy.value=false;}}
async function remove(){if(!selected.value||busy.value)return;busy.value=true;error.value='';try{await savedCommandDelete(selected.value.id,selected.value.version);editor.value=null;selected.value=null;removing.value=false;await load();}catch(e){error.value=message(e);}finally{busy.value=false;}}
defineExpose({ create });
onMounted(load);
</script>
<template>
 <section class="commands-panel" aria-label="命令管理"><div class="resource-toolbar"><input v-model="search" aria-label="搜索命令" placeholder="搜索命令" /><button data-test="new-command" type="button" :disabled="busy" @click="create">新建命令</button><button type="button" :disabled="busy" @click="load">刷新</button><small>双击独立执行 · 不保留工作目录与 Shell 状态</small></div><p v-if="error" role="alert" class="inline-error">{{error}}</p>
 <p class="command-notice">命令内容保存在本机配置中，请勿在命令中保存密码、私钥或访问令牌。</p><div class="command-browser"><div class="command-catalog" aria-label="保存的命令"><p v-if="!visible.length" class="empty-state">暂无命令</p><button v-for="item in visible" :key="item.id" data-test="command-item" type="button" :class="{'is-selected':selected?.id===item.id}" @click="select(item)" @dblclick="run(item)" @keydown.enter.prevent="run(item)"><span>{{item.name}}</span><small>{{item.category||'默认分类'}}</small></button></div>
 <form v-if="editor" class="command-editor" @submit.prevent="save"><label>名称<input ref="nameInput" v-model="editor.name" data-test="command-name" required maxlength="128" :disabled="busy" /></label><label>分类<input v-model="editor.category" maxlength="128" :disabled="busy" /></label><label class="command-editor__content">命令<textarea v-model="editor.content" data-test="command-content" required spellcheck="false" rows="6" :disabled="busy" /></label><div class="resource-actions"><button type="submit" :disabled="busy">保存</button><button type="button" :disabled="busy" @click="editor=null;selected=null;removing=false;error=''">取消编辑</button><button type="button" :disabled="disabled||busy||!selected" @click="selected&&run(selected)">运行已保存命令</button><button type="button" :disabled="busy||!selected" @click="removing=true">删除</button></div><div v-if="removing" role="alert"><span>删除保存命令“{{selected?.name}}”？</span><button type="button" :disabled="busy" @click="remove">确认删除</button><button type="button" :disabled="busy" @click="removing=false">取消</button></div></form><div v-else class="command-editor empty-state">选择命令进行编辑，或新建常用命令。</div></div>
 </section>
</template>
<style scoped>
.commands-panel{font-size:12px}.command-browser{display:grid;grid-template-columns:minmax(150px,26%) minmax(0,1fr);min-height:240px}.command-catalog{max-height:350px;overflow:auto;border-right:1px solid var(--border)}.command-catalog>button{display:flex;flex-direction:column;gap:4px;text-align:left;width:100%;padding:10px 14px;border:0;border-bottom:1px solid #edf1f3;border-radius:0;background:white}.command-catalog>button.is-selected{background:#e6f4ef;color:#096b58}.command-catalog small{color:var(--muted)}.command-editor{display:grid;gap:10px;padding:14px;align-content:start}.command-editor label{display:grid;gap:5px}.command-editor textarea{font:13px/1.5 Consolas,monospace;resize:vertical;min-height:110px}.command-editor input,.command-editor textarea{width:100%;box-sizing:border-box;border:1px solid #c4cfd6;border-radius:5px;padding:8px}.command-editor .resource-actions{display:flex;gap:8px;flex-wrap:wrap}@media(max-width:720px){.command-browser{grid-template-columns:1fr}.command-catalog{border-right:0;max-height:150px}}
</style>

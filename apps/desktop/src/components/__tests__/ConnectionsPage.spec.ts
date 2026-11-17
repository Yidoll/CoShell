import { flushPromises, mount } from "@vue/test-utils";
import { beforeEach, describe, expect, it, vi } from "vitest";
import ConnectionsPage from "@/components/ConnectionsPage.vue";
import TerminalView from "@/components/TerminalView.vue";
const terminalWrite = vi.hoisted(() => vi.fn());
vi.mock("@/components/TerminalView.vue", async () => {
  const { defineComponent, h } = await import("vue");
  return { default: defineComponent({ name: "TerminalView", props: ["output", "disabled"], emits: ["data", "paste", "resize"], setup(_, { expose }) { expose({ write: terminalWrite, focus: () => {}, activate: () => {}, eraseLast: () => {}, ready: async () => true }); return () => h("div", { "data-test": "terminal", tabindex: 0 }); } }) };
});
async function typeLine(wrapper: any, text: string) { const t = wrapper.getComponent(TerminalView); t.vm.$emit("data", text); await wrapper.vm.$nextTick(); t.vm.$emit("data", "\r"); await flushPromises(); }


const backend = vi.hoisted(() => ({
  connectionList: vi.fn(), connectionSave: vi.fn(), connectionCopy: vi.fn(), connectionDelete: vi.fn(), groupList: vi.fn(), groupSave: vi.fn(), sshOpen: vi.fn(), sshCancel: vi.fn(),
  groupDelete: vi.fn(),
  sessionRead: vi.fn(), sessionSubmit: vi.fn(), sessionClose: vi.fn(), sessionWrite: vi.fn(), sessionResize: vi.fn(),
  serverOverview: vi.fn(), serverOverviewCancel: vi.fn(),
  sshKeySaved: vi.fn(), sshKeySave: vi.fn(), sshKeyDelete: vi.fn(), sshKeyOperationStatus: vi.fn(),
  remoteRealpath:vi.fn(),remoteList:vi.fn(),remoteStat:vi.fn(),remoteRead:vi.fn(),remotePrepare:vi.fn(),remoteConfirm:vi.fn(),remoteCommit:vi.fn(),remoteCancel:vi.fn(),
  savedCommandList:vi.fn(),savedCommandSave:vi.fn(),savedCommandDelete:vi.fn(),sessionRunSaved:vi.fn(),
}));
vi.mock("@/lib/backend/api", () => backend);

const profile = { id: "c1", name: "服务器", host: "example.org", port: 22, username: "root", authentication: "password", groupId: null, notes: "", production: false, alwaysConfirm: true, version: 0 };

describe("连接管理", () => {
  beforeEach(() => {
    localStorage.clear();
    Object.defineProperty(window, 'innerWidth', { value: 1280, configurable: true });
    vi.resetAllMocks();
    terminalWrite.mockReset();
    backend.connectionList.mockResolvedValue([profile]);
    backend.groupList.mockResolvedValue([]);
    backend.sessionRead.mockResolvedValue({ output: "", closed: false });
    backend.sessionClose.mockResolvedValue(undefined);
    backend.sessionWrite.mockResolvedValue(undefined); backend.sessionResize.mockResolvedValue(undefined);
    backend.sshCancel.mockResolvedValue(undefined);
    backend.sshKeySaved.mockResolvedValue(false); backend.sshKeySave.mockResolvedValue(undefined); backend.sshKeyDelete.mockResolvedValue(undefined);
    backend.serverOverview.mockRejectedValue("不可用");
    backend.serverOverviewCancel.mockResolvedValue(undefined);
    backend.remoteRealpath.mockResolvedValue('/root');backend.remoteList.mockResolvedValue([]);backend.remoteCancel.mockResolvedValue(undefined);backend.savedCommandList.mockResolvedValue([]);
  });

  it('连接工作台按连接侧栏、终端、文件夹和命令侧栏组织', async () => {
    backend.sshOpen.mockResolvedValue({ status: 'connected', sessionId: 's1' });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    expect(wrapper.find('[data-test=connection-workspace-layout]').exists()).toBe(true);
    expect(wrapper.find('[aria-label="连接管理"]').exists()).toBe(true);
    expect(wrapper.find('[aria-label="SSH 会话"]').exists()).toBe(true);
    expect(wrapper.find('[aria-label="服务器文件夹"]').exists()).toBe(true);
    expect(wrapper.find('[aria-label="右侧命令管理"]').exists()).toBe(true);
    wrapper.unmount();
  });

  it('面板收起状态可恢复且不取消会话', async () => {
    localStorage.clear();
    backend.sshOpen.mockResolvedValue({ status: 'connected', sessionId: 's1' });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    await wrapper.get('[data-test=toggle-command-sidebar]').trigger('click');
    expect(localStorage.getItem('coshell.panel.commands')).toBe('collapsed');
    expect(wrapper.get('[aria-label="右侧命令管理"]').text()).toContain('展开');
    expect(wrapper.find('[aria-label="SSH 会话"]').exists()).toBe(true);
    await wrapper.get('[data-test=toggle-command-sidebar]').trigger('click');
    expect(localStorage.getItem('coshell.panel.commands')).toBe('open');
    wrapper.unmount();
  });

  it('未连接也常驻三列，连接列表和命令分别处于左右侧栏', async () => {
    const wrapper = mount(ConnectionsPage); await flushPromises();
    const layout = wrapper.get('[data-test=connection-workspace-layout]');
    expect(layout.element.contains(wrapper.get('[data-test=connect]').element)).toBe(true);
    expect(wrapper.get('[data-test=connect]').element.closest('.workbench-left')).not.toBeNull();
    expect(wrapper.get('[aria-label="右侧命令管理"]').element.parentElement).toBe(layout.element);
    expect(wrapper.find('[aria-label="服务器文件夹"]').exists()).toBe(true);
    expect(wrapper.find('.workspace-heading').exists()).toBe(false);
    expect(wrapper.find('.resource-tabs').exists()).toBe(false);
    wrapper.unmount();
  });

  it('顶栏新建命令使用右侧唯一实例，收起再展开保留草稿', async () => {
    const wrapper = mount(ConnectionsPage); await flushPromises();
    (wrapper.vm as unknown as {createCommand: () => void}).createCommand(); await flushPromises();
    const input = wrapper.get('[data-test=command-name]');
    await input.setValue('未保存的命令');
    const editor = input.element;
    await wrapper.get('[data-test=toggle-command-sidebar]').trigger('click');
    await wrapper.get('[data-test=toggle-command-sidebar]').trigger('click');
    expect(wrapper.get('[data-test=command-name]').element).toBe(editor);
    expect((editor as HTMLInputElement).value).toBe('未保存的命令');
    (wrapper.vm as unknown as {createCommand: () => void}).createCommand(); await flushPromises();
    expect((wrapper.get('[data-test=command-name]').element as HTMLInputElement).value).toBe('未保存的命令');
    expect(backend.savedCommandSave).not.toHaveBeenCalled();
    wrapper.unmount();
  });

  it('双击保存命令绑定版本独立执行，确认前不执行',async()=>{
    backend.connectionList.mockResolvedValue([{...profile,production:true}]);
    backend.sshOpen.mockResolvedValue({status:'connected',sessionId:'s1'});
    backend.savedCommandList.mockResolvedValue([{id:'cmd',name:'查看日志',content:'tail /var/log/test',category:'默认分类',version:3}]);
    backend.sessionRunSaved.mockResolvedValueOnce({status:'confirmation_required',token:'permit',risk:'unknown',categories:[]}).mockResolvedValueOnce({status:'sent',output:'done\n'});
    const wrapper=mount(ConnectionsPage);await flushPromises();await wrapper.get('[data-test=connect]').trigger('click');await wrapper.get('[data-test=password]').setValue('secret');await wrapper.get('[data-test=start-connect]').trigger('click');await flushPromises();
    await flushPromises();await wrapper.get('[data-test=command-item]').trigger('dblclick');await flushPromises();
    expect(backend.sessionRunSaved).toHaveBeenLastCalledWith('s1','cmd',3,undefined);expect(backend.sessionWrite).not.toHaveBeenCalled();expect(backend.sessionSubmit).not.toHaveBeenCalled();
    const dialog=wrapper.get('[aria-label=确认保存命令]');expect(dialog.text()).toContain('tail /var/log/test');await dialog.findAll('button').find(b=>b.text()==='确认执行')!.trigger('click');await flushPromises();expect(backend.sessionRunSaved).toHaveBeenLastCalledWith('s1','cmd',3,'permit');expect(terminalWrite).toHaveBeenCalledWith(expect.stringContaining('done'));wrapper.unmount();
  });

  it('删除已保存私钥期间无法取消连接对话框', async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, authentication: 'private_key' }]); backend.sshKeySaved.mockResolvedValue(true);
    let done!: () => void; backend.sshKeyDelete.mockReturnValue(new Promise<void>(resolve => { done = resolve; }));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await flushPromises();
    const dialog = wrapper.get('[role=dialog]');
    await dialog.findAll('button').find(b => b.text().includes('删除已保存私钥'))!.trigger('click');
    await dialog.trigger('keydown', { key: 'Escape' }); await flushPromises();
    expect(wrapper.find('[role=dialog]').exists()).toBe(true);
    done(); await flushPromises(); wrapper.unmount();
  });

  it('保存私钥进行中不能关闭其标签', async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, authentication: 'private_key' }]);
    let done!: () => void; backend.sshKeySave.mockReturnValue(new Promise<void>(resolve => { done = resolve; }));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await flushPromises(); await wrapper.get('[data-test=private-key]').setValue('temporary'); await wrapper.get('[data-test=save-key]').setValue(true);
    await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    await wrapper.get('[data-test=session-tab]').trigger('contextmenu'); await wrapper.get('[data-test=close-tab]').trigger('click'); await flushPromises();
    expect(wrapper.find('[data-test=private-key]').exists()).toBe(true);
    done(); await flushPromises(); wrapper.unmount();
  });

  it('复制会话需要重新认证，不同标签的按键只发送到当前会话', async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValueOnce({ status: 'connected', sessionId: 's1' }).mockResolvedValueOnce({ status: 'connected', sessionId: 's2' });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('first'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    await wrapper.get('[data-test=session-tab]').trigger('contextmenu'); await wrapper.get('[data-test=duplicate-tab]').trigger('click'); await flushPromises();
    const dialogs = wrapper.findAll('[data-test=password]'); expect(dialogs).toHaveLength(1); expect((dialogs[0]!.element as HTMLInputElement).value).toBe('');
    await dialogs[0]!.setValue('second'); await wrapper.findAll('[data-test=start-connect]')[0]!.trigger('click'); await flushPromises();
    const terminals = wrapper.findAllComponents(TerminalView); expect(terminals).toHaveLength(2);
    terminals[1]!.vm.$emit('data', 'b'); await flushPromises(); expect(backend.sessionWrite).toHaveBeenLastCalledWith('s2', 'b');
    await wrapper.findAll('[data-test=session-tab]')[0]!.trigger('click'); terminals[0]!.vm.$emit('data', 'a'); await flushPromises();
    expect(backend.sessionWrite).toHaveBeenLastCalledWith('s1', 'a'); wrapper.unmount();
  });

  it("普通会话按顺序发送终端按键，多行粘贴取消时不发送", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    let finish!: () => void;
    backend.sessionWrite.mockReturnValueOnce(new Promise<void>((resolve) => { finish = resolve; })).mockResolvedValue(undefined);
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    const terminal = wrapper.getComponent(TerminalView);
    terminal.vm.$emit('data', 'a'); terminal.vm.$emit('data', '\t'); await flushPromises();
    expect(backend.sessionWrite).toHaveBeenCalledTimes(1);
    finish(); await flushPromises(); expect(backend.sessionWrite).toHaveBeenNthCalledWith(2, 's1', '\t');
    terminal.vm.$emit('paste', 'one\ntwo'); await flushPromises();
    expect(wrapper.find('[aria-label="确认多行粘贴"]').exists()).toBe(true);
    await wrapper.get('[aria-label="确认多行粘贴"] button').trigger('click'); await flushPromises();
    expect(backend.sessionWrite).toHaveBeenCalledTimes(2); wrapper.unmount();
  });

  it("收到输出后立即继续读取，不对每个输出块等待 300 毫秒", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionRead.mockResolvedValueOnce({ output: "first", closed: false }).mockReturnValue(new Promise(() => {}));
    const timer = vi.spyOn(globalThis, 'setTimeout');
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    expect(backend.sessionRead).toHaveBeenCalledTimes(2);
    expect(timer.mock.calls.some(([, delay]) => delay === 300)).toBe(false);
    wrapper.unmount(); timer.mockRestore();
  });

  it("生产命令完成后先显示输出再显示提示符，并正确处理换行", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, production: true }]);
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionSubmit.mockResolvedValue({ status: "sent" });
    backend.sessionRead.mockResolvedValueOnce({ output: "one\ntwo\n", closed: false });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    terminalWrite.mockClear(); await typeLine(wrapper, 'pwd');
    const writes = terminalWrite.mock.calls.map(([value]) => value).join('');
    expect(writes).toContain('one\r\ntwo\r\n');
    expect(writes.indexOf('one')).toBeLessThan(writes.lastIndexOf('$ '));
    wrapper.unmount();
  });

  it("输入失败后丢弃排队按键，resize 顺序合并为最新尺寸", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    let fail!: (reason: string) => void; let resized!: () => void;
    backend.sessionWrite.mockReturnValueOnce(new Promise<void>((_, reject) => { fail = reject; }));
    backend.sessionResize.mockReturnValueOnce(new Promise<void>((resolve) => { resized = resolve; })).mockResolvedValue(undefined);
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    const terminal = wrapper.getComponent(TerminalView);
    terminal.vm.$emit('resize', 80, 24); terminal.vm.$emit('resize', 90, 25); terminal.vm.$emit('resize', 100, 30);
    await flushPromises(); expect(backend.sessionResize).toHaveBeenCalledTimes(1);
    resized(); await flushPromises(); expect(backend.sessionResize).toHaveBeenNthCalledWith(2, 's1', 100, 30);
    terminal.vm.$emit('data', 'a'); terminal.vm.$emit('data', 'b'); await flushPromises();
    fail('发送结果未知'); await flushPromises();
    expect(backend.sessionWrite).toHaveBeenCalledTimes(1);
    expect(terminal.props('disabled')).toBe(true); wrapper.unmount();
  });

  it("关闭旧会话会丢弃待确认粘贴，不向新会话发送", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValueOnce({ status: 'connected', sessionId: 's1' }).mockResolvedValueOnce({ status: 'connected', sessionId: 's2' });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    wrapper.getComponent(TerminalView).vm.$emit('paste', 'one\ntwo'); await flushPromises();
    await wrapper.get('.terminal-heading button').trigger('click'); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    expect(wrapper.find('[aria-label="确认多行粘贴"]').exists()).toBe(false);
    expect(backend.sessionWrite).not.toHaveBeenCalled(); wrapper.unmount();
  });

  it("输入发送失败导致断线时关闭粘贴确认", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValue({ status: 'connected', sessionId: 's1' });
    let fail!: (reason: string) => void;
    backend.sessionWrite.mockReturnValueOnce(new Promise<void>((_, reject) => { fail = reject; }));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    const terminal = wrapper.getComponent(TerminalView); terminal.vm.$emit('data', 'a'); await flushPromises();
    terminal.vm.$emit('paste', 'one\ntwo'); await flushPromises(); expect(wrapper.find('[aria-label="确认多行粘贴"]').exists()).toBe(true);
    fail('发送结果未知'); await flushPromises(); expect(wrapper.find('[aria-label="确认多行粘贴"]').exists()).toBe(false); wrapper.unmount();
  });

  it("卸载后不发送队列中剩余的按键", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValue({ status: 'connected', sessionId: 's1' });
    let finish!: () => void;
    backend.sessionWrite.mockReturnValueOnce(new Promise<void>((resolve) => { finish = resolve; }));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    wrapper.getComponent(TerminalView).vm.$emit('data', 'a'); wrapper.getComponent(TerminalView).vm.$emit('data', 'b'); await flushPromises();
    wrapper.unmount(); finish(); await flushPromises(); expect(backend.sessionWrite).toHaveBeenCalledTimes(1);
  });

  it("输入积压超过上限时停止后续发送", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sshOpen.mockResolvedValue({ status: 'connected', sessionId: 's1' });
    let finish!: () => void; backend.sessionWrite.mockReturnValueOnce(new Promise<void>((resolve) => { finish = resolve; }));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await wrapper.get('[data-test=password]').setValue('secret'); await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    const terminal = wrapper.getComponent(TerminalView); terminal.vm.$emit('data', 'a'); await flushPromises();
    terminal.vm.$emit('data', 'x'.repeat(65536)); await flushPromises();
    expect(wrapper.text()).toContain('输入积压过多'); finish(); await flushPromises();
    expect(backend.sessionWrite).toHaveBeenCalledTimes(1); expect(terminal.props('disabled')).toBe(true); wrapper.unmount();
  });

  it("展示真实指标中的零值、缺失原因，并在断线后标记非实时", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.serverOverview.mockResolvedValue({ connectionId: "c1", collectedAt: "2026-11-11T00:00:00Z", online: true, hostname: "host", operatingSystem: "Linux", kernel: null, architecture: null, bootTime: null, uptime: { value: 0, unit: "s", unavailableReason: null }, cpuUsage: { value: null, unit: "%", unavailableReason: "权限不足" }, logicalCores: null, memoryTotal: null, memoryUsed: null, load: null, diskTotal: null, diskUsed: null, swapTotal: null, swapUsed: null });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    expect(wrapper.get("[aria-label='服务器概览']").text()).toContain("0 s");
    expect(wrapper.get("[aria-label='服务器概览']").text()).toContain("权限不足");
    expect(backend.serverOverview).toHaveBeenCalledWith("s1", expect.any(String));
    wrapper.unmount();
  });

  it("切换或关闭会话时取消采集并忽略迟到结果", async () => {
    let finish!: (value: unknown) => void;
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.serverOverview.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    await wrapper.get(".terminal-heading button").trigger("click"); await flushPromises();
    expect(backend.serverOverviewCancel).toHaveBeenCalledWith("s1", expect.any(String));
    finish({ connectionId: "c1", hostname: "迟到主机" }); await flushPromises();
    expect(wrapper.get("[aria-label='服务器概览']").text()).not.toContain("迟到主机"); wrapper.unmount();
  });

  it("定时刷新保持单次采集，过期数据标记陈旧并在关闭时取消", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    const snapshot = { connectionId: "c1", collectedAt: "2026-11-11T00:00:00Z", online: true, hostname: "host", operatingSystem: "Linux", kernel: null, architecture: null, bootTime: null, uptime: null, cpuUsage: null, logicalCores: null, memoryTotal: null, memoryUsed: null, load: null, diskTotal: null, diskUsed: null, swapTotal: null, swapUsed: null };
    backend.serverOverview.mockResolvedValueOnce(snapshot).mockReturnValue(new Promise(() => {}));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    vi.useFakeTimers({ toFake: ["Date", "setInterval", "clearInterval"] });
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    await vi.advanceTimersByTimeAsync(60_000);
    expect(backend.serverOverview).toHaveBeenCalledTimes(2);
    expect(wrapper.get("[aria-label='服务器概览']").text()).toContain("数据已过期");
    await wrapper.get(".terminal-heading button").trigger("click"); await flushPromises();
    expect(backend.serverOverviewCancel).toHaveBeenCalledWith("s1", expect.any(String));
    await vi.advanceTimersByTimeAsync(30_000);
    expect(backend.serverOverview).toHaveBeenCalledTimes(2);
    wrapper.unmount(); vi.useRealTimers();
  });

  it("连接编辑弹窗捕获键盘焦点，Esc 关闭后返回触发按钮", async () => {
    const wrapper = mount(ConnectionsPage, { attachTo: document.body }); await flushPromises();
    const trigger = wrapper.get<HTMLButtonElement>("[data-test=new]"); trigger.element.focus();
    await trigger.trigger("click"); await flushPromises();
    const dialog = wrapper.get("[role=dialog]");
    expect(dialog.element.contains(document.activeElement)).toBe(true);
    const controls = dialog.element.querySelectorAll<HTMLElement>("button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled)");
    controls[controls.length - 1]!.focus();
    await wrapper.get(".modal-backdrop").trigger("keydown", { key: "Tab" });
    expect(document.activeElement).toBe(controls[0]);
    await wrapper.get(".modal-backdrop").trigger("keydown", { key: "Escape" }); await flushPromises();
    expect(wrapper.find("[role=dialog]").exists()).toBe(false);
    expect(document.activeElement).toBe(trigger.element);
    wrapper.unmount();
  });

  it("命令确认接管焦点，Esc 取消并返回命令输入", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionSubmit.mockResolvedValue({ status: "confirmation_required", token: "token", risk: "dangerous", categories: [] });
    const wrapper = mount(ConnectionsPage, { attachTo: document.body }); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    const input = wrapper.get<HTMLElement>("[data-test=terminal]"); input.element.focus(); await typeLine(wrapper, "rm -rf /tmp/x");
    const dialog = wrapper.get("[aria-label='确认命令']"); expect(dialog.element.contains(document.activeElement)).toBe(true);
    await dialog.trigger("keydown", { key: "Escape" }); await flushPromises();
    expect(wrapper.find("[aria-label='确认命令']").exists()).toBe(false); expect(document.activeElement).toBe(input.element);
    expect(backend.sessionSubmit).toHaveBeenCalledTimes(1); wrapper.unmount();
  });

  it("确认请求进行时不能取消，并阻止焦点离开确认区域", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    let finish!: (value: { status: "sent" }) => void;
    backend.sessionSubmit.mockResolvedValueOnce({ status: "confirmation_required", token: "token", risk: "dangerous", categories: [] }).mockReturnValueOnce(new Promise((resolve) => { finish = resolve; }));
    const wrapper = mount(ConnectionsPage, { attachTo: document.body }); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    await typeLine(wrapper, "rm -rf /tmp/x");
    const dialog = wrapper.get("[aria-label='确认命令']");
    wrapper.get<HTMLButtonElement>("[data-test=new]").element.focus();
    expect(dialog.element.contains(document.activeElement)).toBe(true);
    await dialog.get(".danger-button").trigger("click");
    expect(dialog.get("[data-test=cancel-command]").attributes("disabled")).toBeDefined();
    await dialog.trigger("keydown", { key: "Escape" });
    expect(wrapper.find("[aria-label='确认命令']").exists()).toBe(true);
    finish({ status: "sent" }); await flushPromises(); wrapper.unmount();
  });

  it("搜索与分组筛选连接，并创建分组", async () => {
    backend.groupList.mockResolvedValue([{ id: "g1", name: "运维", parentId: null }]);
    backend.connectionList.mockResolvedValue([profile, { ...profile, id: "c2", name: "数据库", host: "db.local", groupId: "g1" }]);
    backend.groupSave.mockResolvedValue(undefined);
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=search]").setValue("db.local");
    expect(wrapper.text()).toContain("数据库"); expect(wrapper.text()).not.toContain("root@example.org");
    await wrapper.get("[data-test=search]").setValue(""); await wrapper.get("[data-test=group-filter]").setValue("g1");
    expect(wrapper.text()).toContain("数据库"); expect(wrapper.text()).not.toContain("root@example.org");
    await wrapper.get("[data-test=new-group]").trigger("click"); await wrapper.get("[data-test=group-name]").setValue("研发"); await wrapper.get(".dialog form").trigger("submit"); await flushPromises();
    expect(backend.groupSave).toHaveBeenCalledWith(expect.objectContaining({ name: "研发" })); wrapper.unmount();
  });

  it("可编辑分组，并确认删除分组", async () => {
    backend.groupList.mockResolvedValue([{ id: "g1", name: "运维", parentId: null }]);
    backend.groupSave.mockResolvedValue(undefined); backend.groupDelete.mockResolvedValue(true);
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=group-filter]").setValue("g1");
    await wrapper.get("[data-test=edit-group]").trigger("click"); await wrapper.get("[data-test=group-name]").setValue("运维组"); await wrapper.get(".dialog form").trigger("submit"); await flushPromises();
    expect(backend.groupSave).toHaveBeenCalledWith({ id: "g1", name: "运维组", parentId: null });
    await wrapper.get("[data-test=delete-group]").trigger("click"); await wrapper.get("[data-test=confirm-delete-group]").trigger("click"); await flushPromises();
    expect(backend.groupDelete).toHaveBeenCalledWith("g1"); wrapper.unmount();
  });

  it("复制连接并允许取消删除", async () => {
    backend.connectionCopy.mockResolvedValue({ ...profile, id: "copy", name: "服务器 副本" });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=copy]").trigger("click"); await wrapper.get("[data-test=copy-form]").trigger("submit"); await flushPromises();
    expect(backend.connectionCopy).toHaveBeenCalledWith("c1", expect.any(String), "服务器 副本");
    await wrapper.get("[data-test=copy]").trigger("click"); await wrapper.get("[data-test=copy-name]").setValue("第二份副本"); await wrapper.get("[data-test=copy-form]").trigger("submit"); await flushPromises();
    expect(backend.connectionCopy).toHaveBeenLastCalledWith("c1", expect.any(String), "第二份副本");
    await wrapper.get("[data-test=delete]").trigger("click"); expect(wrapper.text()).toContain("确定删除");
    await wrapper.get("[data-test=cancel-delete]").trigger("click"); expect(backend.connectionDelete).not.toHaveBeenCalled(); wrapper.unmount();
  });

  it("删除失败时保留连接并展示活动会话提示", async () => {
    backend.connectionDelete.mockRejectedValue("该连接存在活动会话，请先关闭会话");
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=delete]").trigger("click"); await wrapper.get("[data-test=confirm-delete]").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("该连接存在活动会话"); expect(wrapper.text()).toContain("root@example.org"); wrapper.unmount();
  });

  it("创建连接并保留生产标识", async () => {
    backend.connectionSave.mockImplementation(async (value) => value);
    const wrapper = mount(ConnectionsPage);
    await flushPromises();
    expect(wrapper.text()).toContain("服务器");
    await wrapper.get("[data-test=new]").trigger("click");
    await wrapper.get("[data-test=name]").setValue("测试机");
    await wrapper.get("[data-test=host]").setValue("example.net");
    await wrapper.get("[data-test=username]").setValue("alice");
    await wrapper.get("[data-test=production]").setValue(true);
    await wrapper.get("form").trigger("submit");
    await flushPromises();
    expect(backend.connectionSave).toHaveBeenCalledWith(expect.objectContaining({ name: "测试机", port: 22, production: true }));
    wrapper.unmount();
  });

  it("取消主机指纹确认不重试连接", async () => {
    backend.sshOpen.mockResolvedValue({ status: "host_key_confirmation", host: "example.org", port: 22, algorithm: "ssh-ed25519", fingerprint: "SHA256:test" });
    const wrapper = mount(ConnectionsPage);
    await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click");
    await wrapper.get("[data-test=password]").setValue("secret");
    await wrapper.get("[data-test=start-connect]").trigger("click");
    await flushPromises();
    expect(wrapper.text()).toContain("SHA256:test");
    await wrapper.get("[data-test=reject-host]").trigger("click");
    expect(backend.sshOpen).toHaveBeenCalledTimes(1);
    expect(wrapper.text()).not.toContain("SHA256:test");
    wrapper.unmount();
  });

  it("需要确认的命令取消后不会执行", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionSubmit.mockResolvedValue({ status: "confirmation_required", token: "token", risk: "dangerous", categories: ["recursive_delete"] });
    const wrapper = mount(ConnectionsPage);
    await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click");
    await wrapper.get("[data-test=password]").setValue("secret");
    await wrapper.get("[data-test=start-connect]").trigger("click");
    await flushPromises();
    await typeLine(wrapper, "rm -rf /tmp/demo");
    await flushPromises();
    expect(wrapper.text()).toContain("危险");
    await wrapper.get("[data-test=cancel-command]").trigger("click");
    expect(backend.sessionSubmit).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it("生产命令确认后才执行，并说明独立执行限制", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, production: true }]);
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionSubmit.mockResolvedValueOnce({ status: "confirmation_required", token: "token", risk: "dangerous", categories: ["recursive_delete"] }).mockResolvedValueOnce({ status: "sent" });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("每条命令独立执行");
    expect(wrapper.text()).toContain("不保留工作目录和环境变量");
    await typeLine(wrapper, "rm -rf /tmp/demo");
    expect(backend.sessionSubmit).toHaveBeenCalledTimes(1);
    expect(backend.sessionSubmit).toHaveBeenLastCalledWith("s1", "rm -rf /tmp/demo", undefined);
    await wrapper.get(".danger-button").trigger("click"); await flushPromises();
    expect(backend.sessionSubmit).toHaveBeenLastCalledWith("s1", "rm -rf /tmp/demo", "token");
    wrapper.unmount();
  });

  it("修改命令后使确认失效且不回显临时密码", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionSubmit.mockResolvedValue({ status: "confirmation_required", token: "token", risk: "unknown", categories: [] });
    const wrapper = mount(ConnectionsPage);
    await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click");
    await wrapper.get("[data-test=password]").setValue("private-password-value");
    await wrapper.get("[data-test=start-connect]").trigger("click");
    await flushPromises();
    expect(wrapper.html()).not.toContain("private-password-value");
    await typeLine(wrapper, "echo first");
    await flushPromises();
    expect(wrapper.find("[role=alertdialog]").exists()).toBe(true);
    await wrapper.get("[data-test=cancel-command]").trigger("click");
    expect(wrapper.find("[role=alertdialog]").exists()).toBe(false);
    expect(backend.sessionSubmit).toHaveBeenCalledTimes(1);
    wrapper.unmount();
  });

  it("取消正在连接的请求后关闭迟到的会话", async () => {
    let finish!: (value: { status: "connected"; sessionId: string }) => void;
    backend.sshOpen.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    const wrapper = mount(ConnectionsPage);
    await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click");
    await wrapper.get("[data-test=password]").setValue("secret");
    await wrapper.get("[data-test=start-connect]").trigger("click");
    await wrapper.get(".dialog-actions button").trigger("click");
    expect(backend.sshCancel).toHaveBeenCalledOnce();
    finish({ status: "connected", sessionId: "late" });
    await flushPromises();
    expect(backend.sessionClose).toHaveBeenCalledWith("late");
    expect(wrapper.find(".terminal-panel").exists()).toBe(false);
    await wrapper.get("[data-test=connect]").trigger("click");
    await wrapper.get("[data-test=password]").setValue("new-secret");
    expect(wrapper.get("[data-test=start-connect]").attributes("disabled")).toBeUndefined();
    wrapper.unmount();
  });

  it("旧会话迟到的确认结果不影响新会话", async () => {
    backend.sshOpen.mockResolvedValueOnce({ status: "connected", sessionId: "s1" }).mockResolvedValueOnce({ status: "connected", sessionId: "s2" });
    let finish!: (value: { status: "confirmation_required"; token: string; risk: "dangerous"; categories: string[] }) => void;
    backend.sessionSubmit.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    const wrapper = mount(ConnectionsPage);
    await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    await typeLine(wrapper, "rm -rf /tmp/demo");
    await wrapper.get(".terminal-heading button").trigger("click");
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    wrapper.getComponent(TerminalView).vm.$emit("data", "rm -rf /tmp/demo");
    finish({ status: "confirmation_required", token: "old", risk: "dangerous", categories: [] }); await flushPromises();
    expect(wrapper.find("[role=alertdialog]").exists()).toBe(false);
    wrapper.unmount();
  });

  it("关闭结果未知时保留会话并可重试", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionClose.mockRejectedValueOnce(new Error("关闭结果未知"));
    const wrapper = mount(ConnectionsPage);
    await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    await wrapper.get(".terminal-heading button").trigger("click"); await flushPromises();
    expect(wrapper.text()).toContain("关闭结果未知");
    expect(wrapper.find(".terminal-panel").exists()).toBe(true);
    await wrapper.get(".terminal-heading button").trigger("click"); await flushPromises();
    expect(wrapper.find(".terminal-panel").exists()).toBe(false);
    wrapper.unmount();
  });

  it("保存期间不能取消编辑并发起其他操作", async () => {
    let finish!: (value: typeof profile) => void;
    backend.connectionSave.mockReturnValue(new Promise((resolve) => { finish = resolve; }));
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=new]").trigger("click");
    await wrapper.get("[data-test=name]").setValue("测试"); await wrapper.get("[data-test=host]").setValue("host"); await wrapper.get("[data-test=username]").setValue("user");
    await wrapper.get("form").trigger("submit");
    expect(wrapper.get(".dialog-actions button").attributes("disabled")).toBeDefined();
    await wrapper.get(".modal-backdrop").trigger("click");
    expect(wrapper.find("form").exists()).toBe(true);
    finish(profile); await flushPromises(); wrapper.unmount();
  });

  it("发送结果未知后禁止继续输入并提示重连", async () => {
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.connectionList.mockResolvedValue([{ ...profile, alwaysConfirm: false }]);
    backend.sessionWrite.mockRejectedValue("发送结果未知，请勿自动重试");
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    await typeLine(wrapper, "ls");
    expect(wrapper.getComponent(TerminalView).props("disabled")).toBe(true);
    expect(wrapper.text()).toContain("请关闭会话并重新连接");
    wrapper.unmount();
  });

  it("生产执行结果未知后立即禁止输入", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, production: true }]);
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    backend.sessionSubmit.mockRejectedValue("执行结果未知，请关闭并重新连接");
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click"); await wrapper.get("[data-test=password]").setValue("secret"); await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    await typeLine(wrapper, "ls");
    expect(wrapper.getComponent(TerminalView).props("disabled")).toBe(true);
    wrapper.unmount();
  });

  it("私钥与口令仅用于本次连接并在成功后从界面移除", async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, authentication: "private_key" }]);
    backend.sshOpen.mockResolvedValue({ status: "connected", sessionId: "s1" });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get("[data-test=connect]").trigger("click");
    await wrapper.get("[data-test=private-key]").setValue("test-private-key-material");
    await wrapper.get("[data-test=passphrase]").setValue("test-private-passphrase");
    await wrapper.get("[data-test=start-connect]").trigger("click"); await flushPromises();
    expect(backend.sshOpen).toHaveBeenCalledWith("c1", null, expect.any(String), undefined, "test-private-key-material", "test-private-passphrase", false);
    expect(wrapper.html()).not.toContain("test-private-key-material");
    expect(wrapper.html()).not.toContain("test-private-passphrase");
    wrapper.unmount();
  });

  it('保存私钥到系统存储，并在下次选择已保存密钥连接', async () => {
    backend.connectionList.mockResolvedValue([{ ...profile, authentication: 'private_key' }]);
    backend.sshOpen.mockResolvedValue({ status: 'connected', sessionId: 's1' });
    const wrapper = mount(ConnectionsPage); await flushPromises();
    await wrapper.get('[data-test=connect]').trigger('click'); await flushPromises();
    await wrapper.get('[data-test=private-key]').setValue('private-key');
    await wrapper.get('[data-test=passphrase]').setValue('passphrase');
    await wrapper.get('[data-test=save-key]').setValue(true);
    await wrapper.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    expect(backend.sshKeySave).toHaveBeenCalledWith('c1', 'private-key', 'passphrase');
    expect(wrapper.html()).not.toContain('private-key');
    wrapper.unmount();
    backend.sshKeySaved.mockResolvedValue(true);
    const next = mount(ConnectionsPage); await flushPromises();
    await next.get('[data-test=connect]').trigger('click'); await flushPromises();
    expect(next.find('[data-test=private-key]').exists()).toBe(false);
    await next.get('[data-test=start-connect]').trigger('click'); await flushPromises();
    expect(backend.sshOpen).toHaveBeenLastCalledWith('c1', null, expect.any(String), undefined, null, null, true);
    next.unmount();
  });
});

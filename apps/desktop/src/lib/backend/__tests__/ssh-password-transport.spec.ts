import { describe, expect, it, vi } from "vitest";
import * as http from "@/lib/backend/http";
import * as tauri from "@/lib/backend/tauri";

const invoke = vi.hoisted(() => vi.fn());
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

describe("SSH 密码保存传输契约", () => {
  it("Tauri 只传输保存决定的令牌、操作标识和同意状态", async () => {
    invoke.mockResolvedValueOnce("available").mockResolvedValueOnce(undefined);
    await expect(tauri.sshPasswordState("c1")).resolves.toBe("available");
    expect(invoke).toHaveBeenLastCalledWith("ssh_password_state", { connectionId: "c1" });
    await tauri.sshPasswordDecision("token", "operation", true);
    expect(invoke).toHaveBeenLastCalledWith("ssh_password_decision", { token: "token", operationId: "operation", save: true });
  });

  it("使用已存密码时不发送密码值，仍可提交主机指纹确认", async () => {
    invoke.mockResolvedValueOnce({ status: "connected", sessionId: "s1" });
    await tauri.sshOpen("c1", null, "operation", "SHA256:test", null, null, false, true);
    expect(invoke).toHaveBeenLastCalledWith("ssh_open", {
      connectionId: "c1", password: null, operationId: "operation", acceptedFingerprint: "SHA256:test",
      privateKey: null, passphrase: null, useSavedKey: false, useSavedPassword: true,
    });
  });

  it("HTTP 明确返回不支持，不发起凭据请求", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);
    try {
      await expect(http.sshPasswordState("c1")).rejects.toThrow("当前 Web 模式暂不支持 SSH");
      await expect(http.sshPasswordDecision("token", "operation", true)).rejects.toThrow("当前 Web 模式暂不支持 SSH");
      expect(fetchMock).not.toHaveBeenCalled();
    } finally {
      vi.unstubAllGlobals();
    }
  });
});

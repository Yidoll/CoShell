# CoShell Agents

`agents/` 用于需要在主进程外运行的驱动或集成。Agent 通过 stdin/stdout 上的版本化 JSON-RPC 2.0 与 CoShell 通信，不直接依赖桌面、Web、CLI 或 MCP 入口。

约束：

- 每个请求必须带请求 ID，响应必须关联原始 ID；
- 启动后先完成版本和能力握手；
- 长操作必须支持超时、取消和进程退出清理；
- stdout 只写协议帧，日志写 stderr；
- 不在日志或错误详情中输出凭据；
- 新 Agent 必须提供独立构建、测试和版本说明。

具体驱动放在 [`drivers/`](drivers/README.md)。协议实现将在首个真实 Agent 需求中加入。

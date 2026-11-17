# CoShell Rust 模块边界

## 依赖方向

```text
desktop / web / cli / mcp
            |
       coshell-core
            +-- coshell-drivers -- coshell-sql -- coshell-types
            |       +-- coshell-platform
            +-- coshell-storage
            +-- coshell-plugin-runtime -- coshell-types + coshell-platform
            +-- coshell-ai-provider -- coshell-platform
            +-- coshell-formats -- coshell-types
            +-- coshell-types
            +-- coshell-platform
```

入口 crate 只负责协议和宿主适配。应用用例与跨组件编排属于 `coshell-core`；下层 crate 不得反向依赖 `coshell-core`。

`coshell-shell` 和 `coshell-backup` 已建立为独立 workspace crate，计划在对应能力实现时由 `coshell-core` 接入；当前尚无依赖连线。

## 所有权

| Crate | 职责 |
| --- | --- |
| `coshell-types` | 跨入口共享的序列化 DTO 和稳定契约 |
| `coshell-platform` | 进程、路径、下载和宿主平台原语 |
| `coshell-storage` | SQLite 结构、迁移、本地数据持久化和恢复快照 |
| `coshell-shell` | Shell 命令解析、风险规则和秘密脱敏，不执行命令 |
| `coshell-backup` | 备份格式、加密和 WebDAV 传输，不决定业务冲突策略 |
| `coshell-sql` | SQL 解析、方言、风险分类和 SQL 生成 |
| `coshell-formats` | 文本、表格、归档和数据格式编码 |
| `coshell-ai-provider` | 模型 Provider、流式协议和 Token 用量 |
| `coshell-plugin-runtime` | 插件包、签名、安装、权限和 Host API |
| `coshell-drivers` | 数据源驱动、连接、执行、隧道和 Agent 分发 |
| `coshell-core` | 连接、查询、任务、AI 工具和安全策略的应用编排 |
| `coshell-web` | Axum HTTP/SSE/WebSocket 适配 |
| `coshell-cli` | 命令行参数与输出适配 |
| `coshell-mcp` | MCP 协议与工具适配 |
| `coshell-desktop` | Tauri 窗口、事件和 Command 适配 |

## 最小纵向链路

所有入口通过 `coshell_core::RuntimeService` 获取统一健康状态：

- Tauri：`health` Command；
- Web：`GET /api/health`；
- CLI：`coshell health`；
- MCP：`coshell_health` Tool。

这个链路是新业务接入的参考：入口解析协议，core 编排，下层 crate 提供具体能力。

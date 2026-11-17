# CoShell

CoShell 是一个以共享 Rust Core 为中心的多入口应用骨架。桌面端、Web 服务、CLI 和 MCP 使用同一套业务编排与类型契约，Vue 前端通过统一 Backend API 在 Tauri 与 HTTP 传输间切换。

当前仓库处于基础架构阶段，目标是先建立可验证的模块边界和最小纵向链路，再按领域逐步加入产品能力。

## 架构概览

```text
Vue 3 工作台
    +-- Tauri transport -> Tauri commands --+
    +-- HTTP transport  -> Axum routes -----+-> coshell-core
CLI ----------------------------------------+
MCP ----------------------------------------+
                                                |
                                                +-- coshell-drivers
                                                +-- coshell-sql
                                                +-- coshell-types
                                                +-- coshell-formats
                                                +-- coshell-ai-provider
                                                +-- coshell-plugin-runtime
                                                +-- coshell-platform
```

设计原则：

- 入口层只处理协议和运行时装配，业务编排进入 `coshell-core`；
- 下层 crate 不反向依赖 core 或任何入口应用；
- 前端功能默认同时支持 Tauri 与 HTTP 两种传输；
- Agent 和插件通过版本化协议扩展，不直接侵入核心模块；
- 架构边界由自动化测试持续校验。

详细说明见 [CoShell 架构文档](docs/coshell-architecture.md)。设计基准见 [DBX 技术结构文档](docs/dbx-technical-architecture.md)。

## 仓库结构

| 路径 | 职责 |
| --- | --- |
| `apps/desktop/` | Vue 3 工作台和双传输 Backend API |
| `src-tauri/` | Tauri 桌面入口与 Command 适配 |
| `crates/` | Rust 类型、底层能力、业务编排和服务入口 |
| `agents/` | 独立进程 Agent 的协议和驱动扩展区 |
| `plugins/` | 插件契约、SDK 与示例扩展区 |
| `packages/` | npm 启动器和平台分发包扩展区 |
| `scripts/` | 架构守卫和仓库自动化 |
| `docs/` | 架构与开发文档 |

## 环境要求

- Node.js `22.23.3`（见 `.nvmrc`）
- pnpm `10.34.7`
- Rust stable，建议 `1.88` 或更高
- Tauri 平台依赖，参见 Tauri 2 官方 prerequisites

## 开发命令

```bash
pnpm install
pnpm dev              # Vue 前端（HTTP/Web 模式）
pnpm dev:web          # Vue 前端（HTTP/Web 模式别名）
pnpm dev:backend      # Axum 后端，默认监听 127.0.0.1:8787
pnpm dev:tauri        # Tauri 桌面应用
pnpm spec:validate    # 严格校验 OpenSpec changes/specs
pnpm architecture
pnpm typecheck
pnpm test
pnpm build
```

Rust 验证：

```bash
cargo fmt --all -- --check
cargo check --workspace
cargo test --workspace
```

Web、CLI 和 MCP 可直接按 package 启动：

```bash
cargo run -p coshell-web
cargo run -p coshell-cli -- health
cargo run -p coshell-mcp
```

桌面应用需要 Rust 和当前平台的 Tauri prerequisites。运行 `pnpm dev:tauri` 时，Tauri 会按 `src-tauri/tauri.conf.json` 自动启动前端开发服务。

## 变更约定

1. 先确定代码所有者层，再实现功能，避免跨层复制业务逻辑。
2. 新增前端后端调用时同步维护 `api.ts`、`tauri.ts`、`http.ts`、Tauri Command 和 Web Route。
3. 修改 crate 依赖或新增入口时运行 `node --test scripts/check-architecture.mjs`。
4. 测试范围与变更风险匹配；协议、安全和共享状态变更需要跨层测试。
5. 不将凭据、生产数据、构建产物或本地运行状态提交到仓库。

## OpenSpec 工作流

仓库使用 OpenSpec `1.17.0` 管理规格驱动开发，配置位于 `openspec/config.yaml`，Codex 工作流技能位于 `.agents/skills/`。

```bash
pnpm exec openspec list
pnpm exec openspec new change <change-name>
pnpm exec openspec status --change <change-name>
pnpm spec:validate
```

提案、规格、设计和任务默认使用简体中文书写，但保留 OpenSpec 结构标题和 `SHALL`/`MUST` 关键字。模块所有权、双传输一致性、安全边界和验证要求已写入项目上下文。

## 当前边界

初始化版本只提供健康检查/运行信息等最小纵向能力。数据库连接、查询执行、持久化、认证、插件签名和 Agent 生命周期将在明确产品需求后按现有边界实现。

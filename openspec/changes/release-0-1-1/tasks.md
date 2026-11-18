# CoShell 0.1.1 实施任务

审核通过后按顺序执行。每项先补能复现问题的测试，再实现并整理代码。验证通过后勾选该项。

## 1. 修复 WebDAV 配置保存

- [x] 1.1 在 `crates/coshell-core/tests/webdav_config.rs` 补充已有凭据时修改用户名并提供新密码的回归测试，覆盖同时更新目录、目标变更未填密码、目录单独变更和同目标密码替换。更新 `webdav_config.rs` 的校验及保存分支，验证保存后读取的配置与凭据匹配。
- [x] 1.2 在 `crates/coshell-core/tests/webdav_secret_receipts.rs` 补充系统存储写入失败、提交前中断、响应丢失、重复操作、旧凭据清理失败和并发读取覆盖。调整共享协调器与 `crates/coshell-storage/src/webdav_secret_receipt.rs`，验证回执、配置和引用的生效时点，以及应用重启后的恢复行为。
- [x] 1.3 更新 `WebDavSettings.vue`、`src-tauri/src/lib.rs` 及 `apps/desktop/src/lib/backend/` 的契约，补充 `WebDavSettings.spec.ts` 测试。验证切换账号时的密码提示、保存结果、重试、状态未知处理，以及新配置保存后仍可单独检查服务器。
- [x] 1.4 评审本批改动，核对失败后原配置可用、地址或用户名变化时不会混用凭据。运行相关 Rust 测试、Vue 测试和接口契约测试。

## 2. 支持 SSH 密码保存和自动认证

- [x] 2.1 为 `crates/coshell-core/src/desktop_runtime.rs` 及凭据协调器补测试并实现保存状态、确认令牌及系统凭据写入。验证认证完成前不保存、同一令牌重复提交、取消后继续使用会话、令牌过期、存储失败及目标变化时拒绝使用旧令牌。若新增迁移，在 `crates/coshell-storage/tests/migrations.rs` 验证旧库升级。
- [x] 2.2 实现绑定校验及已存密码认证，补充同一目标重启后自动连接、目标或认证方式变更、连接复制或删除、主机指纹变化、凭据读取失败和服务器拒绝旧密码的测试。验证手动连接不反复发送旧密码，且只在新密码验证成功并获同意后替换。
- [x] 2.3 更新 `SessionWorkspace.vue`、`src-tauri/src/lib.rs` 及 `apps/desktop/src/lib/backend/`，以组件和接口契约测试验证首次弹窗、取消后不重复提示、自动连接、手动输入回退及 HTTP 明确报告不支持。
- [x] 2.4 在 `crates/coshell-core/tests/backup_snapshot.rs`、`restore_secrets.rs` 对应测试与 `WebDavSettings.spec.ts` 覆盖 SSH 密码备份和恢复，验证口令加密、未加密备份提示、取消保存的密码不进入备份、预览不显示密码，以及恢复后绑定目标一致。
- [x] 2.5 评审本批改动，核对主机校验顺序、密码生命周期、备份凭据范围和现有私钥、Agent 行为。运行相关 Rust 测试、Vue 测试和接口契约测试。

## 3. 集成验收

- [ ] 3.1 在 Windows 和 macOS 验证现有 WebDAV 配置更换账号及 SSH 密码保存流程，覆盖取消、应用重启后免输、失效后手动连接。记录实际验证的平台和结果。
- [x] 3.2 运行 `corepack pnpm architecture`、`corepack pnpm typecheck`、`corepack pnpm test`、`corepack pnpm build`、`cargo fmt --all -- --check`、`cargo check --workspace`、`cargo test --workspace` 和 `corepack pnpm exec openspec validate --all --strict`。记录通过项、失败项及原因。

## 验证记录

- WebDAV 批次评审核对了配置与凭据的原子发布和读写锁，补齐了旧凭据待清理及仅更新配置字段时响应丢失的界面处理。新增回归用例在修复前失败，修复后通过。
- SSH 批次评审核对了主机指纹校验顺序、确认令牌及连接目标绑定，补齐了数据库提交中断后无需重启即可重新保存的处理。新增回归用例在修复前失败，修复后通过。
- Windows 使用系统凭据存储和本地 WebDAV、SSH 服务验证更换账号、取消保存、重启后免输及旧密码失效后的手动连接。本环境没有可用于验收的 macOS 主机，任务 3.1 保持待完成。
- 架构检查 9 项、前端测试 126 项、Rust 工作区测试 381 项通过，TypeScript 类型检查、前端构建、Rust 格式检查及工作区编译检查通过。Windows GNU 链接器使用本机工具链参数，前端契约测试使用预先编译的 Rust 测试样例以避免并行构建等待。OpenSpec 严格校验通过，2 项变更无错误。

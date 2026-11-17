# 本地数据与秘密存储

目前已实现 SQLite 和操作系统凭据存储的底层组件，尚未接入 Core、Tauri 或 Web 运行时。接线后的目标是用本地 SQLite 保存连接分组、连接元数据、命令历史、设置和备份运行记录；连接密码、私钥口令及 WebDAV 凭据由操作系统安全存储持有，数据库只保存引用。连接备注和命令历史也可能包含敏感信息，应遵循应用中的脱敏和保留策略。

## 数据目录

平台目录解析函数位于 [data_dir.rs](../crates/coshell-platform/src/data_dir.rs)：

- Windows：`%LOCALAPPDATA%\CoShell`；
- macOS：`~/Library/Application Support/CoShell`。

目录解析只计算路径，不创建目录；运行时接线完成前，应用不会自动在该目录创建数据库。环境变量缺失或不是绝对路径时返回错误。应用应在启动时显式创建具备当前用户访问权限的目录，并使用事务打开数据库。

## Schema 和凭据

[SQLite 迁移](../crates/coshell-storage/src/lib.rs) 使用 `PRAGMA user_version` 跟踪版本；当前为 v11。v2–v7 引入秘密补偿、回执和 WebDAV 配置恢复；v8–v11 增加批量恢复秘密日志、引用归属约束及历史记录迁移校验。无法证明旧清理记录归属时迁移会保守失败并保留原数据。迁移在事务中执行，版本高于当前程序支持范围时拒绝修改。每个连接都需要启用外键约束。

[SecretStore](../crates/coshell-platform/src/secret_store.rs) 在 Windows 使用 Credential Manager，在 macOS 使用 Keychain；访问失败时不会降级到明文文件。真实凭据条目依赖当前登录用户，且按规范化数据目录路径隔离：复制或移动数据目录后，新路径无法直接读取旧路径的凭据，需要显式导入或重新认证。WebDAV 的跨设备恢复须通过独立加密备份处理凭据，不应依赖直接复制数据目录。

## 本地测试隔离

常规迁移和 repository 测试使用内存 SQLite 或独立临时目录；`test-support` feature 提供独立内存凭据替身，生产构建默认不启用。真实凭据存储测试默认忽略，仅在目标平台主动运行；它创建唯一测试条目并在结束时清理。

```bash
pnpm architecture
cargo test -p coshell-storage
cargo test -p coshell-platform --features test-support
cargo test -p coshell-platform --test secret_store -- --ignored
```

验证数据迁移和恢复时，使用单独的测试数据目录；不要让测试连接用户真实数据目录或继承真实连接凭据。跨平台备份和恢复的实现与验收由 OpenSpec change 中的后续任务跟踪。

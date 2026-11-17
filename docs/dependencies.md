# Shell 客户端依赖基线

本表记录 `develop-shell-client` 选用的直接依赖。Rust 依赖以 `Cargo.lock` 固定传递版本，JavaScript 依赖以 `pnpm-lock.yaml` 固定；所有许可证均以对应发布包的元数据为准。

| 能力 | 依赖及版本 | 许可证 | Windows / macOS 说明 |
| --- | --- | --- | --- |
| SSH/PTY | `russh` 0.63.1，`ring`、`rsa` | Apache-2.0 | Rust 异步实现；SSH Agent、密钥格式和真实目标机兼容性仍须按任务 3.2 验证。 |
| SQLite | `rusqlite` 0.37.0，`bundled`、`backup` | MIT；SQLite 公共领域 | 随应用编译 SQLite；两平台须分别验证本地数据库和备份。 |
| 平台凭据 | `keyring` 3.6.3 | MIT OR Apache-2.0 | Windows 启用 Credential Manager，macOS 启用 Keychain；须分别验证交互与权限。 |
| 认证加密 | `chacha20poly1305` 0.10.1、`argon2` 0.5.3 | MIT OR Apache-2.0 | 纯 Rust 实现；跨平台格式与测试向量在任务 7.1 验证。 |
| WebDAV HTTP | `reqwest` 0.12.28，`rustls-tls` | MIT OR Apache-2.0 | 使用 Rust TLS；WebDAV 服务的 MOVE/ETag 差异在任务 7.2 验证。 |
| 敏感值 | `secrecy` 0.10.3、`zeroize` 1.9.1 | MIT OR Apache-2.0 | 两平台均可使用；不能替代日志脱敏或系统凭据存储。 |
| 规则摘要 | `sha2` 0.10.9 | MIT OR Apache-2.0 | 两平台均可使用；安全摘要不作为执行许可。 |
| 终端渲染 | `@xterm/xterm` 6.0.0、`@xterm/addon-fit` 0.11.0 | MIT | WebView 支持；键盘、输入法和尺寸同步须在 Windows/macOS 实机验证。 |

依赖版本和锁文件只能证明可解析；这里的平台结论尚不代表真实平台测试已通过。验证时使用 `cargo metadata --locked`，并在各目标平台执行对应任务的编译和行为测试。Rust workspace 的最低版本为 1.88；重新生成锁文件时应设置 Cargo 的 `resolver.incompatible-rust-versions = "fallback"`，避免选入需要更高 Rust 版本的传递依赖。

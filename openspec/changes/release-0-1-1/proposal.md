# CoShell 0.1.1 提案

## Why

已配置 WebDAV 密码时，[保存逻辑](../../../crates/coshell-core/src/webdav_config.rs#L198) 会在用户名或地址变化时返回 `CredentialTargetChanged`。这个判断发生在凭据写入前，即使用户提供新密码，也无法直接保存新账号。SSH 密码认证目前每次都要求输入密码，用户希望在首次连通后自行决定是否保存。

## What Changes

- WebDAV 已保存密码时，更换用户名或地址可以填写新密码并一次保存。若留空，界面提示补填密码。同一目标留空沿用已保存密码。
- SSH 密码连接首次使用手动密码成功后，弹窗询问是否保存。确认后存入系统凭据存储，下次打开同一连接时自动使用。取消后当前会话继续使用，之后手动输入密码连接。
- 密码认证失败或系统凭据无法读取时，提供手动输入入口。保存密码的行为复用现有 Windows 和 macOS 凭据存储及连接备份机制。
- 本变更范围限于 WebDAV 配置保存和 SSH 密码认证流程。

## Capabilities

### New Capabilities

- `webdav-credential-management`：管理 WebDAV 配置与凭据的替换、状态反馈以及失败恢复。
- `ssh-password-persistence`：管理 SSH 密码保存同意、自动使用和失败后的手动连接。

### Modified Capabilities

当前没有已归档的主规格。上述规格补充活动变更 `develop-shell-client` 的 `webdav-connection-backup`、`remote-connections` 和 `desktop-platform-support`，沿用其中的连接检查、指纹校验和备份保护约定。

## Impact

变更涉及桌面会话和 WebDAV 配置界面、共享 Rust 核心、系统凭据存储和 Tauri 接口。HTTP 适配层提供一致的调用签名，当前仅提供健康检查的 Axum 入口明确报告不支持。现有连接资料和备份继续读取，凭据引用通过现有存储与恢复路径管理。连接密码会进入已保存连接凭据的备份范围，留空备份口令时沿用现有未加密规则，并在上传前说明包含范围。

# CoShell 桌面安装包发布

推送形如 `vMAJOR.MINOR.PATCH` 的标签后，GitHub Actions 构建三份安装包。测试、版本检查和全部目标构建通过后，工作流将安装包和 `SHA256SUMS.txt` 上传到对应的 GitHub Release 草稿。

| 平台 | 安装包 |
| --- | --- |
| Windows x64 | `CoShell_<版本>_windows-x64.exe` |
| macOS Intel | `CoShell_<版本>_macos-x64.dmg` |
| macOS Apple Silicon | `CoShell_<版本>_macos-arm64.dmg` |

## 发布步骤

1. 将 `Cargo.toml`、`package.json`、`apps/desktop/package.json` 和 `src-tauri/tauri.conf.json` 的版本统一为拟发布的版本，并同步 `Cargo.lock`。更新[发行说明](release-notes-desktop.md)。
2. 验证发布提交后，为该提交推送对应版本标签。工作流会检查标签与四处版本一致。旧标签运行的是该标签提交中的发布流程。新流程由包含此次改动的后续标签触发。
3. 打开 Release 草稿，按 `SHA256SUMS.txt` 核对安装包。Windows 可用 `Get-FileHash -Algorithm SHA256 <路径>`，macOS 可用 `shasum -a 256 <路径>`。
4. 在 Windows x64、macOS Intel 和 Apple Silicon 验证安装、启动及覆盖升级，并检查已有连接、已保存凭据和主题偏好。确认可用后，在 GitHub 发布草稿。

草稿上传中断时可重跑同一标签的失败任务。重跑会复用原草稿并刷新对应文件，已公开的 Release 会保留原资产。Windows 安装包未作 Authenticode 签名，macOS 应用使用临时签名且未公证。此前 `v0.1.1` 标签的发布任务使用旧配置，重跑仍会要求旧流程中的密钥。

用户可从仓库的 GitHub Releases 页下载对应安装包。安装前退出运行中的 CoShell，再覆盖安装。遇到安装中断时，可从同一正式 Release 重新下载，并保留原应用数据目录。

本地可运行 `node --test scripts/desktop-release.test.mjs` 验证版本检查、三平台校验文件和草稿重跑逻辑。

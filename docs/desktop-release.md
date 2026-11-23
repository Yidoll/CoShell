# CoShell 桌面稳定版发布

发布工作流从正式 `vMAJOR.MINOR.PATCH` 标签构建 Windows x64、macOS Intel 和 Apple Silicon 安装包。三个版本的更新资产和签名组成一个 `latest.json`。工作流在草稿阶段核对每个文件的版本、签名及上传摘要，然后生成稳定版。安装端需要能匿名访问 GitHub 的正式清单及资产。

## 首次接入

1. 设置 GitHub Actions 的 `desktop-release` 环境。正式签名参数与密钥通过该环境提供给构建作业。
2. 使用 `pnpm exec tauri signer generate --write-keys <私钥文件路径>` 生成独立的更新签名密钥。将输出公钥文件的完整内容放入 GitHub 环境变量 `COSHELL_UPDATER_PUBLIC_KEY`。将私钥文件的完整内容和密码分别保存在 GitHub 环境密钥 `TAURI_SIGNING_PRIVATE_KEY` 与 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。备份私钥及恢复所需的密码，避免签名材料进入 Git 或 CI 工件。
3. 按下表配置平台签名。Tauri 更新签名验证下载的工件和版本。Windows Authenticode 与 macOS Developer ID 签名、公证独立用于系统安装。

| 平台 | 环境变量 | GitHub 环境密钥 |
| --- | --- | --- |
| Windows x64 | `WINDOWS_TIMESTAMP_URL` 为证书签名使用的 RFC 3161 时间戳地址 | `WINDOWS_CERTIFICATE` 为 Base64 编码的代码签名 PFX，`WINDOWS_CERTIFICATE_PASSWORD` 为 PFX 口令 |
| macOS Intel 和 Apple Silicon | `APPLE_SIGNING_IDENTITY` 为证书的完整 Developer ID Application 标识 | `APPLE_CERTIFICATE` 为 Base64 编码的 P12，`APPLE_CERTIFICATE_PASSWORD` 为 P12 口令，`APPLE_API_ISSUER` 与 `APPLE_API_KEY` 为 App Store Connect 的发行者和密钥 ID，`APPLE_API_PRIVATE_KEY` 为对应 P8 文件的内容 |

证书必须允许在对应 GitHub runner 上签名。如果 Windows 证书只能由硬件或托管签名服务使用，先按团队的签名方案调整发布工作流的签名步骤，并保留对 Authenticode、证书和时间戳的验证。开发构建可使用独立的临时签名密钥。临时测试和正式发布分别使用各自的公钥。

## 发布一次稳定版

1. 将 `Cargo.toml` 中的工作区版本和 `src-tauri/tauri.conf.json` 内的版本同时改为拟发布的版本。更新 Cargo 锁文件及仓库使用的版本字段。更新 [发行说明](release-notes-online-update.md)，保留给 0.1.1 及更早用户的手动安装步骤。
2. 在 Windows x64、macOS Intel、Apple Silicon 分别用两个递增的测试版本验证升级。先安装较低版本并保留一份测试连接、系统凭据和主题偏好，再以已配置的测试清单检查、下载和安装较高版本。重启后确认实际版本及原数据可用。另验证在一个 SSH 会话或备份作业进行时，安装会等待。记录三个平台的实测结果。
3. 发布提交通过项目检查后，为该提交推送同版本正式标签。发布工作流核对标签、Cargo 与 Tauri 版本。Windows 检查可执行文件和 NSIS 安装包的版本、签名及 PE 架构。macOS 检查升级归档和 DMG 中各自的 App 版本、签名、公证及架构。
4. 正式发布前，工作流验证更新签名覆盖预期的版本及包内容，并比较草稿中上传对象的 SHA-256 与本地文件。只有三个目标及各自安装包都完整时，草稿才转为最新稳定版。发布完成后，在 Release 下载页核对三份安装包，并从已接入更新通道的旧版检查更新。

## 本次验收记录

| 平台 | 结果 |
| --- | --- |
| Windows x64 | 通过 NSIS 测试安装包从 92.0.3 升至 92.0.4。软件更新拒绝了篡改包和错误签名。下载后打开 SSH 会话时，安装被拦截。关闭会话后可使用同一个已验证的包安装。应用自动重启，设置和已安装程序均显示新版。深色主题与连接资料保留，并可用已保存的密码重新连接。 |
| macOS Intel | 待在 Intel 设备上验证测试安装包、自动重启及资料和凭据保留。 |
| macOS Apple Silicon | 待在 Apple Silicon 设备上验证测试安装包、自动重启及资料和凭据保留。 |

Windows 验收使用独立的应用标识、数据目录、临时更新签名和本机 HTTPS 服务。GitHub 正式发布、Authenticode 签名与时间戳由正式发布环境验证。

## 中断与恢复

- 发布在草稿阶段中断后，可在 Actions 重新运行工作流。再次运行同一标签时复用该标签的草稿；已公开的标签不可通过重跑替换发布文件。工作流拒绝把比已公开稳定版更旧的标签设为最新版本。
- 发现已公开版本存在问题时，先撤下有问题的版本在稳定版通道上的曝光，确认稳定版清单指向有效版本。保留上一版资产与安装说明。已升级设备用更高版本的修复包恢复，并核对数据库兼容性。
- 旧签名私钥仍可用时，通过受旧客户端信任的过渡版本替换公钥。旧私钥丢失时，通过正式安装包手动建立新公钥的信任。公开说明受影响的版本及手动接入方式。
- 下载或安装失败后，先通过原入口打开应用并核对版本。必要时使用对应架构的正式安装包补装，保留现有应用数据目录和系统凭据存储。

发布脚本的本地检查命令为 `node --test scripts/desktop-release.test.mjs`。测试会生成临时密钥，验证仓库锁定的 Tauri CLI 输出的签名格式和版本绑定。

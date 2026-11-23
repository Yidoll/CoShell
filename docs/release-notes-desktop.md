# CoShell 桌面版发行说明

- 顶栏“设置 → 外观”支持“白色”“黑暗”和“跟随系统”。切换时，终端保留会话和历史输出。
- 更新时，退出 CoShell，再从 GitHub Releases 下载新版安装包并覆盖安装。Windows x64 使用 EXE。macOS 可在“关于本机”确认芯片类型，Intel 使用 x64 DMG，Apple 芯片使用 arm64 DMG。打开 DMG 后，将 CoShell 拖到“应用程序”并替换同名应用。
- Release 中的 `SHA256SUMS.txt` 可用来核对下载文件。Windows 包未签名，macOS 包使用临时签名且未公证，系统可能显示安全提示。

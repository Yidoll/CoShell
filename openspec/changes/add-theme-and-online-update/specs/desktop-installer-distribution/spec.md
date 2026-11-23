# 桌面安装包分发

## ADDED Requirements

### Requirement: 手动获取桌面版本
桌面版本 SHALL 通过 GitHub Releases 提供 Windows x64、macOS Intel 和 Apple Silicon 安装包及 SHA-256 校验文件。客户端 SHALL 通过手动安装完成升级。

#### Scenario: 完整版本等待验收
- **WHEN** 正式标签与应用版本一致，三个目标构建均通过
- **THEN** 发布流程创建或更新包含三种安装包和校验文件的草稿

#### Scenario: 缺少目标或版本不匹配
- **WHEN** 任一目标尚未完成或版本与标签不一致
- **THEN** 发布流程停止生成完整草稿

#### Scenario: 使用应用时不会请求升级
- **WHEN** 用户打开设置或运行桌面客户端
- **THEN** 客户端显示外观设置，软件版本通过下载对应安装包手动更新

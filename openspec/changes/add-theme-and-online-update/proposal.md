# 桌面主题与安装包分发

## Why

工作台能随系统改变部分颜色。用户需要固定主题，已打开的终端也需要同步配色。桌面用户从 GitHub Releases 下载安装包并手动升级。此变更最初计划接入应用内升级，现已撤回该部分。

## What Changes

- 在顶栏右侧提供设置入口，支持白色、黑暗和跟随系统。偏好保存在本机，并应用到启动页、工作台和终端。
- 移除应用内的软件更新入口及升级器。正式版本使用 Windows x64、macOS Intel 和 Apple Silicon 安装包分发。

## Capabilities

### New Capabilities

- `appearance-preferences` 定义主题选择、持久化及运行中切换。
- `desktop-installer-distribution` 定义安装包和校验文件的分发方式。

## Impact

共用 Vue 界面继续提供主题设置。版本标签触发三平台构建，安装包汇集到 GitHub Release 草稿供维护者验收。

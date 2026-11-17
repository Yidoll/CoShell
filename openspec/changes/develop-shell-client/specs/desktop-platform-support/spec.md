# Spec Delta

## Purpose

定义 CoShell 桌面客户端在 Windows 与 macOS 上的可安装、可启动、核心功能一致性及平台安全存储要求，为跨平台发布和验收提供稳定边界。

## ADDED Requirements

### Requirement: 支持 Windows 与 macOS 桌面平台
系统 SHALL 为受支持的 Windows 与 macOS 版本提供可独立安装和启动的桌面客户端，且不要求用户另行安装 Node.js、Rust 或浏览器运行时。

#### Scenario: 在 Windows 上安装并启动
- **GIVEN** 用户使用受支持的 Windows 10 或 Windows 11 x64 系统
- **WHEN** 用户完成 CoShell 安装并启动应用
- **THEN** 系统显示主工作台并能够进入连接管理

#### Scenario: 在 macOS 上安装并启动
- **GIVEN** 用户使用受支持的 macOS 13 或更高版本，处理器为 Apple Silicon 或 Intel
- **WHEN** 用户完成 CoShell 安装并启动应用
- **THEN** 系统显示主工作台并能够进入连接管理

### Requirement: 核心工作流跨平台一致
系统 MUST 在 Windows 与 macOS 上提供一致的连接管理、终端会话、服务器概览、命令历史、生产守护和 WebDAV 备份行为；平台差异仅限原生窗口、快捷键和安全存储适配。

#### Scenario: 同一连接配置跨平台恢复
- **GIVEN** 用户在一个受支持平台导出符合当前版本的加密连接备份
- **WHEN** 用户在另一个受支持平台恢复该备份
- **THEN** 系统恢复等价的非平台专属连接字段、分组和生产环境标识

### Requirement: 使用操作系统安全凭据存储
系统 SHALL 将本地保存的连接密码、私钥口令和 WebDAV 凭据写入操作系统提供的安全凭据存储，不得将其以明文写入普通配置文件、日志或前端持久化存储。

#### Scenario: 保存带密码的连接
- **GIVEN** 用户在受支持的桌面平台创建密码认证连接并选择保存密码
- **WHEN** 系统持久化连接
- **THEN** 普通连接记录仅保存秘密引用，实际密码保存在 Windows Credential Manager 或 macOS Keychain

#### Scenario: 安全存储不可用
- **GIVEN** 操作系统安全凭据存储不可用或拒绝访问
- **WHEN** 用户尝试保存秘密
- **THEN** 系统拒绝以明文降级保存并显示可操作的错误信息

### Requirement: 平台能力失败可诊断
系统 SHALL 对平台不支持、权限不足、凭据存储失败和本地路径不可访问等错误返回稳定错误码，并向用户提供不包含秘密的诊断信息。

#### Scenario: 平台适配失败
- **WHEN** 平台适配层无法完成请求
- **THEN** 系统显示稳定错误码、失败操作和建议处理方式
- **AND** 诊断信息不包含密码、私钥内容、口令或 WebDAV Token

### Requirement: 采用统一的 dbx 风格工作台布局

系统 SHALL 在 Windows 与 macOS 上使用统一的顶部操作和多面板工作区，并以 dbx 桌面端的中性设计令牌、细分隔线、紧凑间距、低圆角、浅色/深色主题和可收起面板作为视觉基线。该参考只约束 CoShell 的视觉与交互结构，不引入 dbx 的业务依赖。

#### Scenario: 显示三个顶部操作

- **GIVEN** 用户已启动 CoShell 主工作台
- **WHEN** 工作台完成加载
- **THEN** 顶部最左侧依次显示“新建连接”“新建命令”“WebDAV 配置”三个直接操作
- **AND** 不显示重复的工作流标签、大品牌区或页面大标题

#### Scenario: 窄窗口保护主操作区域

- **GIVEN** 用户缩小 Windows 或 macOS 窗口宽度
- **WHEN** 可用空间不足以同时展示所有面板
- **THEN** 系统优先收起右侧命令管理，再将左侧连接管理转为可唤起的覆盖面板
- **AND** 终端输入区和服务器文件夹面板保留可用的最小尺寸

#### Scenario: 跨平台保持布局语义一致

- **GIVEN** 用户在 Windows 和 macOS 上分别打开同一工作流
- **WHEN** 用户切换配置区域、调整侧栏或重新打开工作台
- **THEN** 三项顶部操作、面板职责、连接上下文和状态提示保持一致
- **AND** 仅原生快捷键、窗口控件和安全存储适配可以存在平台差异

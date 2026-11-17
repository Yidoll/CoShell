# CoShell Plugins

`plugins/` 是可选能力的扩展边界。插件通过版本化 Manifest 声明身份、入口、能力和权限，由 `coshell-plugin-runtime` 负责发现、校验、启动和回收。

初始化阶段只保留目录和安全约束，不接受未校验插件执行：

- 包内路径必须是相对路径，拒绝父目录穿越和符号链接；
- 正式来源必须校验内容哈希和签名；
- 权限在激活前显式批准，并按最小权限授予；
- 插件 UI 与宿主使用版本化 Host API；
- sidecar 使用结构化协议并受超时、取消和输出限制控制。

SDK 放在 [`sdk/`](sdk/README.md)，示例放在 [`examples/`](examples/README.md)。

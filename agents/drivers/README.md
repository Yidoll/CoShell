# Agent Drivers

每个子目录代表一个可独立构建和发布的 Agent 驱动。优先选择成熟、许可证兼容的原生 Rust/Go SDK；只有缺少可靠原生实现时才使用 Java/JDBC。

建议目录契约：

```text
drivers/<driver-id>/
  README.md
  manifest.json
  src/ or cmd/
  tests/
```

驱动清单至少声明 ID、协议版本、实现版本、运行时要求、支持能力和构建产物名称。

# AW 开发者指南

[English](../../en/aw/index.md)

面向把 AW 接入应用、部署 worker 或编写 Provider 的开发者。首次安装和日常操作见[用户手册](../../../user-guide/zh/aw/index.md)。

## 接入应用与规则

- [接入教程](integration.md) · 用本地脚本跑通允许、阻断和故障处理。
- [Agent / SDK 接入](native-agents.md) · 按框架选择版本、工厂和启动方式。
- [自定义 Hook](providers/hooks.md) · 布尔脚本与原生命令。
- [Provider 开发](providers/index.md) · 协议、sec-core、Tokenless 和只读展示。

## 部署与验证

- [常驻 SDK](deployment/resident.md) · 任务工厂、MCP 与客户端清理。
- [服务与容器](deployment/lifecycle.md) · 持久化、退出、升级和容量。
- [策略更新](deployment/policy-updates.md) · 预热、排空、激活和失败恢复。
- [外部进程接入](deployment/process-attach.md) · 首请求前挂载与移除。
- [原生效果验收](validation.md) · 工具结果与审计证据。
- [本地服务演示](local-service.md) · 无模型的 socket 与审计验证。

## 按需查阅

[配置字段](configuration.md) · [配置编辑](reference/configuration-editor.md) · [CLI 自动化](reference/cli.md) · [包管理](reference/packages.md) · [AgentSight](agentsight.md)

修改 AW 实现时，按[贡献指南](../../../../src/aw/CONTRIBUTING_zh.md)构建、测试和提交。
架构决策与协议不变量集中在[设计索引](../../../../src/aw/docs/design/zh/README.md)。

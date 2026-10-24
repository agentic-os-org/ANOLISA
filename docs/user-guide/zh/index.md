# ANOLISA 用户指南

[English](../en/index.md)

本手册说明如何为 Agent 工作负载安装、配置和使用 ANOLISA 组件。首次使用可从
[快速开始](getting-started/quickstart.md)跑通第一个结果，也可直接打开组件手册
查看安装步骤和参考章节。共用的安装、构建、测试和排障说明集中在「开始使用」。

<!-- navigation:start -->

## 开始使用

- [快速开始](getting-started/quickstart.md)
- [镜像与云服务入口](getting-started/images.md)
- [安装](getting-started/installation.md)
- [ANOLISA CLI](getting-started/anolisa-cli.md)
- [构建与部署](getting-started/building.md)
- [测试](getting-started/testing.md)
- [排障](getting-started/troubleshooting.md)

## 用户入口层

- [cosh (cosh-ng)](cosh-ng/index.md)：保持现有 Shell 使用体验的 AI 原生终端，可通过自然语言完成任务，并调用工具与 Skills。
- [OS Skills](os-skills/index.md)：面向 AI Agent 的 OS 使用说明书，覆盖系统管理、监控、安全和 DevOps 自动化等场景。

## 统一接入层

- [AW](aw/index.md)：以统一架构向上对接 Agent framework，向下衔接可扩展的 AI 原生系统软件。

## Agent 语义层

- [AgentSight](agentsight/index.md)：Agent 可观测性工具，用于查看运行轨迹、LLM 调用和 Token 消耗。
- [Agent Security Core](agent-sec-core/index.md)：面向 AI Agent 的 OS 级安全内核。
- [Tokenless](tokenless/index.md)：通过工具输出压缩、工具 Schema 精简、命令改写等方式，减少 AI Agent 的 Token 消耗。
- [Agent Memory](agent-memory/index.md)：为 Agent 提供可检索的持久化记忆，让信息在不同会话之间保留和复用。
- [SkillFS](skillfs/index.md)：将 Skills 组织成稳定的文件系统视图，供 Agent 按需发现和读取。
- [Workspace Checkpoint](ws-ckpt/index.md)：为 Agent 工作区创建快照、比较文件差异，并在需要时回滚变更。
- [ktuner](ktuner/index.md)：检查 Linux 内核参数，给出调优建议，并支持应用和回滚改动。

<!-- navigation:end -->

## 术语速查

| 术语 | 含义 |
|------|------|
| 组件（Component） | 实现某项功能的软件单元，如 `tokenless` |
| 适配器（Adapter） | 将组件接入 Agent 框架的桥接包 |
| system mode | 需要 root 权限的安装模式（`sudo anolisa install`） |
| user mode | 安装到用户目录，无需 sudo |

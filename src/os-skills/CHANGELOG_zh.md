# 更新日志

[English](CHANGELOG.md)

本文档记录 OS Skills 的所有重要变更。

格式基于 [Keep a Changelog](https://keepachangelog.com/en/1.1.0/)，
项目遵循 [Semantic Versioning](https://semver.org/spec/v2.0.0.html)。

## [未发布]

## [0.6.3] - 2026-08-21

### 修复

- RPM 现在提供 `anolisa-component(os-skills)`，当仓库侧组件索引不可用时，
  `anolisa upgrade` 可通过 RPM metadata 为已有 OS Skills 组件解析软件包
  ([#2576](https://github.com/alibaba/anolisa/pull/2576))。

## [0.6.2] - 2026-08-07

- 新增 `ktuner` Skill，支持确定性的内核诊断、调优和回滚。（#1278）
- 从源码和 RPM 安装内容中移除旧版 OpenClaw 与 Hermes adapter 脚本。（#1172）
- 更新 `anolisa-guide`，补充带认证的 Skill Ledger 恢复与篡改检测说明。（#2185）

## [0.6.1] - 2026-07-03

- 重写 `sysom-diagnosis` Skill，并移除旧版 CLI。（#1241）
- 修复 `install-openclaw` Skill 对 OpenClaw gateway 写入范围的验证。（#1205）

## [0.6.0] - 2026-06-29

- 新增 anolisa 组件契约，包括 component.toml、Makefile 和 RPM spec。（#1159）
- 为 `install-openclaw` Skill 新增 OpenClaw bootstrap 指引。（#1051）
- 在启动 gateway 前为 `install-openclaw` Skill 新增模型 endpoint 预检。（#1031）
- 为 `anolisa-guide` Skill 新增静态知识库更新脚本。（#1010）
- 新增 `anolisa-guide` Skill。（#849）
- 修复 uv 和 qwenpaw 安装过程中的阿里云镜像 fallback。（#968）
- 将 `install-claude-code` Skill 的 DashScope proxy URL 更新为新的 Anthropic
  endpoint。（#858）
- 在 OS Skills 中将 `copaw` 重命名为 `qwenpaw`。（#968）

## [0.5.0] - 2026-06-11

- 新增 `anolisa-register` Skill。（#829）

## [0.4.0] - 2026-06-08

- 新增 Agent 安装 Skill 自动安装 Tokenless plugin 的能力。（#731）
- 新增 OpenClaw 依赖项预检。（#719）
- 改进 OpenClaw 非交互式配置流程。（#687）
- 新增 Hermes adapter runner。（#617）
- 新增独立的 ANOLISA adapter 入口。（#549）
- 修复 OpenClaw state dir 路径规范化。（#641）
- 改进 Makefile 安装路径与组件契约。（#541）

## [0.3.0] - 2026-04-26

- 新增 `hermes-agent-install` Skill。（#353）
- 新增 `clawhub-skill-mng` Skill，支持 npm 安装和 YAML 描述匹配。（#315）
- 修复 AgentSight 自定义数据库路径问题，改用默认路径。（#366）
- 修复 AgentSight Token 节省量查询支持。（#355）
- 修复 AgentSight 中断 CLI，并统一使用 `conversation_id` 命名。（#334）

## [0.2.2] - 2026-04-15

- 支持通过 `agentsight` Skill 启用 AgentSight dashboard。（#222）

## [0.2.1] - 2026-04-14

- 使用 MiniMax 开源实现升级 `xlsx` Skill。（#218）
- 将 Skill 描述中的“适用于 alinux4”更新为“适用于 RPM-based Linux”。（#182）

## [0.2] - 2026-04-12

- 新增 `humanizer`、`image-gen`、`pdf-reader` 和 `xlsx` Skill。（#178）
- 新增 `cosh-guide` Skill。（#23）
- 为 `sysom-diagnosis` Skill 新增网络、I/O 和系统负载诊断能力。（#163）

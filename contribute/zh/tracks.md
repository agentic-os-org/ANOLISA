# 贡献方向与类目

[English](../en/tracks.md)

ANOLISA 覆盖运维技能、框架适配器、系统服务与内核探针等多层软件栈。每个方向相对独立，按你的熟悉领域与本地环境选择对应类目即可上手。

选定方向后，按 [Roadmap、Issue 与认领流程](workflow.md) 开始参与。

## 1. 体验反馈、测试样本与评测基准

*适合人群：早期用户、Agent 重度使用者、QA 与评测研究者（不限操作系统）。*

- **结构化 Bug 报告与复现**：通过仓库 Issue 模板提交可复现的问题或平台兼容性缺口。
- **脱敏真实样本（Fixtures）**：为 `tokenless` 贡献真实工具输出样本（如 Gradle、Maven、cargo、kubectl 日志等），或为 `agent-sec-core`、`agentsight` 提供误报/漏报用例。
- **文档与指南走查**：在真实环境中验证快速开始与组件手册，修正过时步骤或中英文不一致之处。
- **跨组件评测**：扩展 [`benchmark/`](../../benchmark/README_zh.md) 中的评测任务集与复现脚本。

## 2. OS Skills、内核调优规则与安全策略

*适合人群：SRE、Linux 系统管理员与安全工程师（OS Skills 测试支持 Linux 与 macOS）。*

- **OS Skills**：在 [`src/os-skills/`](../../src/os-skills/README_zh.md) 贡献或完善系统运维技能（`SKILL.md`、确定性辅助脚本与回归测试）。
- **内核参数规则**：为 `src/ktuner/` 补充确定性内核参数诊断规则与调优依据。
- **安全规则与策略**：为 `src/agent-sec-core/` 贡献 Cedar 权限策略模板、静态代码危险操作特征或提示词安全测试用例。

## 3. 输出压缩器与独立扩展（AW Providers）

*适合人群：AI Infra 开发者与系统/CLI 工具作者。*

- **Tokenless 压缩器**：在 `src/tokenless/` 新增特定 CLI 工具或结构化文本的输出压缩器——输入输出契约清晰，本地即可通过单元测试验证。
- **AW Provider**：遵循 `aw-provider/v1alpha1`（stdio）协议，将面向 Agent 优化的系统能力接入 ANOLISA。Provider 可收录在 [`providers/`](../../providers/README_zh.md)，也可在外部仓库用任意编程语言独立开发和分发。

## 4. Agent 框架适配与插件生态

*适合人群：Agent 框架维护者与 TypeScript / Python / Rust SDK 开发者。*

- **AW 原生适配器**：通过 `aw-adapter` 为更多 Agent CLI、SDK 或 Harness 接入统一 Hook、MCP 与策略生命周期。
- **组件插件**：维护与扩展 `tokenless`、`agent-memory`、`ws-ckpt`、`agent-sec-core` 面向各类 Agent 宿主的插件与 MCP 集成。

## 5. 发行版适配、打包与平台移植

*适合人群：Linux 发行版维护者、容器/沙箱平台工程师及 macOS/Windows 贡献者。*

- **Linux 发行版支持**：为 `cosh-ng`（见 [cosh-ng 添加发行版](cosh-ng/adding-distros.md)）及 `anolisa` 扩展发行版识别、平台适配与 RPM/DEB/raw 打包支持。
- **镜像与云环境目录**：向[镜像与云服务入口](../../docs/user-guide/zh/getting-started/images.md)贡献容器基础镜像、预装系统镜像或沙箱预置方案。
- **跨平台子集体验**：完善 macOS/Windows 受支持子集（`anolisa` CLI、`cosh-ng` 受限模式、`agentsight` 轨迹与 Dashboard 前端）。

## 6. 核心系统组件研发与架构演进

*适合人群：Rust 系统程序员、内核/eBPF/FUSE/安全专家及长期贡献者。*

- **核心守护进程与底层子系统**：参与 eBPF 探针（`agentsight`）、FUSE 视图（`skillfs`）、btrfs 快照（`ws-ckpt`）、bubblewrap/seccomp 沙箱与 SkillSec（`agent-sec-core`）、混合记忆检索（`agent-memory`）、PTY 终端交互（`cosh-ng`，见[新增 CLI 命令](cosh-ng/adding-commands.md)）及 AW 运行时（`aw`）演进。
- **跨组件架构提案**：参与公共协议与多组件契约的 RFC 讨论（见[社区治理](../../GOVERNANCE_zh.md)）。

## 查找可参与的任务

- **适合新手的任务**：查看开放的 [`good first issue`](https://github.com/agentic-os-org/ANOLISA/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) 与 [`action:helpwanted`](https://github.com/agentic-os-org/ANOLISA/issues?q=is%3Aissue+is%3Aopen+label%3A%22action%3Ahelpwanted%22) 列表。
- **组件路线图**：浏览 [Community Roadmap Discussions](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap) 中的候选方向。
- **构建与提交门禁**：查阅[通用贡献指南](../../CONTRIBUTING_zh.md)获取本地验证命令与提交规范。

# 更新日志

[English](CHANGELOG.md)

## [0.1.1] - 2026-11-12

### 新增

- **AW**：更新至 v0.1.1，管理八个 Adapter 已准备的原生实例，并在任务排空后更新策略。
- **AW**：更新至 v0.1.1，导出有界公共审计与运行事实，供应用查看实例状态和已完成任务。
- **AW**：更新至 v0.1.1，允许受支持的执行前 Provider 替换经过检查的工具输入，
  再交给原生权限检查与执行。
- **AW**：更新至 v0.1.1，允许 sec-core Provider 在检查预算内等待后端 socket，
  支持后端与 Agent 并行启动（[#9881](https://github.com/agentic-os-org/ANOLISA/pull/9881)）。

### 变更

- **AW**：更新至 v0.1.1，通过私有 Registry 注册缩短 QwenPaw 普通启动流程，
  并识别 pip／uv 生成的启动脚本（[#9959](https://github.com/agentic-os-org/ANOLISA/pull/9959)）。
- **AW**：更新至 v0.1.1，为 Qoder CLI、OpenClaw、Hermes、Pi、Claude SDK 和
  AgentScope 接入原生启动准备与空闲策略更新。
- **AW**：更新至 v0.1.1，在共享 Service 连接可读时立即唤醒，降低各 Adapter
  的控制调用延迟（[#9837](https://github.com/agentic-os-org/ANOLISA/pull/9837)）。

### 修复

- **AW**：更新至 v0.1.1，保留原生拒绝与事件超时的失败动作，避免受拒或超时的任务继续执行。
- **AW**：更新至 v0.1.1，允许新前缀安装 Provider 时保留共享数据库中的 0.1.0
  旧记录和回退副本（[#9763](https://github.com/agentic-os-org/ANOLISA/pull/9763)）。

### 支持与部署

Linux 上八个 Adapter 的支持范围见 [Hook 矩阵](../../docs/user-guide/zh/aw/capabilities.md#hook-支持矩阵)。
其中七个支持执行前受检输入替换，OpenClaw 暂不支持。执行后结果替换仅作用于获准的
文本位置，原生权限检查保持优先。各 Adapter 和启动模式的 MCP 与空闲策略更新边界见
[支持表](../../docs/user-guide/zh/aw/capabilities.md#mcp-与策略更新)。本地 Skill 检查按
[能力说明](../../docs/user-guide/zh/aw/capabilities.md#skill-校验)接入 sec-core Provider。
部署时分别安装所选 Provider 与后端，准备原生启动入口，并在 AW 配置中启用策略。
回退时保留上一版 0.1.0 包及配置。

## [0.1.0] - 2026-11-07

### 新增

- **AW**：首次发布 v0.1.0，用一份策略配置接入多个原生 Agent，统一审计、取消和实例清理
  （[#9222](https://github.com/agentic-os-org/ANOLISA/pull/9222)）。
- **AW**：将用户提交和工具 Hook 接入结构化 Provider、脚本及原生命令。
  提交前和工具执行前可阻断；结构化后置 Provider 可替换获准的结果文本。
- **AW**：独立提供 sec-core、Tokenless 桥接包。sec-core 检查提示词注入、输入／输出
  PII、代码及受支持的本地 Skill；Tokenless 压缩工具结果并提供命令重试建议。
- **AW**：支持多个已配置 Agent 共享本地服务、按实例停止，以及受支持常驻模式下任务排空后的策略更新。
- **AW**：通过 Makefile 分阶段安装并保留原生包管理器的文件归属，为独立 Provider 提供本地 bundle 安装入口。

### 支持范围矩阵

支持 Linux。下表适用于普通启动模式；受限热更新模式有额外准入要求。

| Adapter | 工具执行前观察／阻断 | 工具执行后观察／文本替换 | 用户提交观察／阻断 |
| --- | --- | --- | --- |
| AgentScope | 支持 | 支持 | 支持 |
| Claude SDK | 支持 | 支持 | 支持 |
| Hermes | 支持 | 支持 | 支持 |
| OpenClaw | 支持 | 支持 | 暂不支持 |
| Pi | 支持 | 支持 | 支持 |
| Qoder CLI | 支持 | 支持 | 支持 |
| Qoder SDK | 支持 | 支持 | 支持 |
| QwenPaw | 支持 | 支持 | 支持 |

提交检查覆盖各 Adapter 的已接入文本入口。结果替换作用于已接入的文本位置，保留原生元数据和失败状态。
Qoder CLI 后置检查覆盖成功的工具调用。Skill 检查覆盖 Qoder SDK、QwenPaw、AgentScope、
Hermes 的本地来源及 Pi 的 Skill 提交。Qoder SDK、QwenPaw 和 AgentScope 的普通常驻／工厂模式支持空闲策略更新。

MCP、原生配置、私有凭据及各模式的具体范围见[能力说明](../../docs/user-guide/zh/aw/capabilities.md)，
可复现版本基线见[原生测试矩阵](tests/native-matrix.json)。

### 部署

- 配置框架、业务工厂、模型及后端服务，安装 Provider 后通过配置启用策略。
- Provider 通过进程协议独立接入。
- 预览版升级时同步调整 Provider 路径与策略配置；保留上一版产物及配置用于回退。

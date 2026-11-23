# 更新日志

[English](CHANGELOG.md)

## [0.2.0] - 2026-11-20

### 新增

- **AW**：更新至 v0.2.0，交付 Python 和 TypeScript 运行时 SDK，以及可选的 Claude、
  QwenPaw、Pi 和 Qoder 适配包。应用按需安装对应适配包，沿用原生启动和配置
  （[#10211](https://github.com/agentic-os-org/ANOLISA/pull/10211)、
  [#10325](https://github.com/agentic-os-org/ANOLISA/pull/10325)）。
- **AW**：更新至 v0.2.0，支持通过 `aw config descriptor` 离线编译目标专用启动信息。
  SDK 应用可用一份策略配置接入多个目标，并在安装回调前校验各自的 Descriptor
  （[#10244](https://github.com/agentic-os-org/ANOLISA/pull/10244)）。
- **AW**：更新至 v0.2.0，支持应用持有的 Claude 和 Pi worker 在受保护任务开始前挂接，
  并为固定版本 Hermes 0.23.1 Gateway 提供后台挂接。常驻任务在回调和清理排空前继续使用
  开始时的策略（[#10290](https://github.com/agentic-os-org/ANOLISA/pull/10290)、
  [#10304](https://github.com/agentic-os-org/ANOLISA/pull/10304)）。
- **AW**：更新至 v0.2.0，新增 Codex 和 DeepSeek Harness 原生适配，并在已验证的
  Qoder SDK、Claude SDK、OpenClaw 和 DeepSeek Harness profile 中支持最终结果检查。
  具体版本、传输和结果类型见[能力说明](../../docs/user-guide/zh/aw/capabilities.md#hook-支持矩阵)。

### 变更

- **AW**：更新至 v0.2.0，统一实例连接、策略更新和服务控制命令。使用旧命令的脚本需在切换
  二进制前按下表迁移（[#10279](https://github.com/agentic-os-org/ANOLISA/pull/10279)）。
- **AW**：更新至 v0.2.0，从同一源码版本交付六个 SDK 包，附 `aw-sdk-candidate/v2` 校验记录。
  四个适配包均依赖对应语言的 0.2.0 核心 SDK，旧导入路径转发到已安装的适配包
  （[#10325](https://github.com/agentic-os-org/ANOLISA/pull/10325)）。
- **AW**：更新至 v0.2.0，Provider 合同支持决策、限定位置的修改与生命周期事件。
  原生执行需要对应 Hook 和效果通过接入准入
  （[#10321](https://github.com/agentic-os-org/ANOLISA/pull/10321)）。

### 修复

- **AW**：更新至 v0.2.0，恢复受支持 QwenPaw 工具结果在流式和模型交付前的受检文本替换。
  必需检查失败时扣留原始结果（[#10201](https://github.com/agentic-os-org/ANOLISA/pull/10201)）。
- **AW**：更新至 v0.2.0，将 Hermes 工具输出与发起调用对应起来，使生效策略检查对应的结果
  （[#10304](https://github.com/agentic-os-org/ANOLISA/pull/10304)）。

### 支持与部署

Linux 支持 [AW 受管启动、已准备的原生启动和应用持有的 SDK 接入](../../docs/developer-guide/zh/aw/deployment/lifecycle.md)。
各 Agent 和模式的观察、阻断、受检输入替换与文本结果替换范围见
[Hook 矩阵](../../docs/user-guide/zh/aw/capabilities.md#hook-支持矩阵)。
无框架的 `sdk/application` profile 支持输入和工具执行前观察及阻断，以及工具执行后观察。
各模式支持的来源、受管 MCP 路径与空闲更新条件见
[Skill 校验](../../docs/user-guide/zh/aw/capabilities.md#skill-校验)和
[MCP 与策略更新](../../docs/user-guide/zh/aw/capabilities.md#mcp-与策略更新)。

| 原入口 | 0.2.0 入口 |
| --- | --- |
| `aw agent connect` 或 `aw install --config FILE --agent TARGET` | `aw connect --config FILE --agent TARGET` |
| `aw instance attach` | `aw connect`，按原生 `--agent` 和 `--pid` 或 SDK 实例选择 |
| `aw instance attach --remove` 或 `aw instance detach` | `aw disconnect`，按原生目标和 PID 或 `--sdk-instance` 选择 |
| `aw instance update` | `aw update` |
| `aw serve`、`aw status`、`aw stop` | `aw service serve`、`aw service status`、`aw service stop` |
| `aw validate` | `aw config validate` |

应用运行时和凭据继续由原生环境管理。选装与 AW 同版本的 Provider 桥接包，并单独部署后端。
升级前停止受影响实例，使用新二进制校验配置，重新生成使用中的 Descriptor，再通过选定入口
启动服务和应用。回退所需的旧版二进制、Provider 和 SDK 包、配置及启动脚本应成套保留。

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

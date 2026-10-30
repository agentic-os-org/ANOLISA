<div align="center">

<picture>
  <source
    media="(prefers-color-scheme: dark)"
    srcset="docs/images/brand/anolisa-lockup-dark.svg"
  >
  <source
    media="(prefers-color-scheme: light)"
    srcset="docs/images/brand/anolisa-lockup-light.svg"
  >
  <img
    src="docs/images/brand/anolisa-lockup-light.svg"
    alt="ANOLISA"
    width="320"
  >
</picture>

<sub>**A**gentic **N**exus **O**perating **L**ayer & **I**nterface **S**ystem **A**rchitecture</sub>

**面向 AI Agent 的系统软件栈。**

[English](README.md) · [网站](https://agentic-os.sh/zh/) ·
[快速开始](#快速开始) · [用户指南](https://agentic-os.sh/zh/docs/user-guide/) ·
[参与贡献](CONTRIBUTING_zh.md)

[![License](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-lightgrey.svg)](https://agentic-os.sh/zh/docs/user-guide/getting-started/installation/)
[![AARM Aligned](https://img.shields.io/badge/AARM-Aligned-blue.svg)](https://aarm.dev/builders/agentseccore-anolisa)
[![OWASP Agentic Top 10: 7 Full, 3 Partial](https://img.shields.io/badge/OWASP_Agentic_Top_10-7_Full,_3_Partial-blue)](docs/user-guide/zh/agent-sec-core/owasp-agentic-top10.md)

</div>

**ANOLISA 是面向 AI Agent 工作负载的多组件系统软件栈。** 它以现有操作系统的
内核和基础设施为基础，为 Agent 应用、框架和平台提供上下文管理、Skills 供给、
工作区恢复、安全与观测能力。
**用户**可以通过保留熟悉 Shell 交互方式的原生终端完成任务，并为 Agent 的运行环境启用系统级优化能力。
**开发者**可以通过 [AW 开放架构](https://agentic-os.sh/zh/docs/developer-guide/aw/providers/)，
将面向 Agent 工作负载优化的自有系统软件接入 ANOLISA，扩展系统软件栈的能力。

> **ANOLISA vs. Agentic OS:** Agentic OS 指面向 AI Agent 工作负载的操作系统形态。
> 本社区以中立、开放的方式参与这一方向的探索；
> ANOLISA 是本社区设计的 Agentic OS 架构中的核心系统软件栈。

<a id="快速开始"></a>

## 1. 快速开始

你可以通过三种方式使用 ANOLISA：通过**预装镜像**体验完整的 Agentic OS 能力；
在已有 OS 上安装 ANOLISA 系统软件栈，将原有系统**扩展为 Agentic OS**；
或者为已有的 Agent 应用、框架或 Harness **接入可配置的系统级优化能力**。

<a id="预装镜像与-agentic-computer"></a>

### 1.1 从镜像开始体验

在开箱即用的预装系统环境中，你可以在 cosh 中使用熟悉的 Shell 交互方式，
并用自然语言调用系统工具与 Skills。完成相关组件的安装与配置后，你还可以使用
上下文压缩、工作区保存与恢复、安全与观测等 Agent 语义层能力。
此外，你也可以基于容器基础镜像搭建自己的 Agent 运行环境。

[镜像与云服务入口](docs/user-guide/zh/getting-started/images.md)目录列出了可用镜像、
镜像预装内容、下载或拉取地址，以及云服务使用入口。厂商和社区贡献者都可以
向该目录补充镜像或云服务条目与使用说明。

<a id="集成到-agent-platform-镜像"></a>

Agent 平台与 Agent 运行基础设施的开发者，可以在 Agent、沙箱或业务镜像中预置 ANOLISA 软件包。
具体做法见[基础镜像目录](docs/user-guide/zh/getting-started/images.md#容器基础镜像)与
[软件包安装指南](https://agentic-os.sh/zh/docs/user-guide/getting-started/installation/)。

<a id="将已有-os-转为-agentic-os"></a>

### 1.2 将已有 OS 转为 Agentic OS

如果你已经在使用受支持的 Linux 系统，可以在该系统上安装 ANOLISA 系统软件栈，
为原有系统增加面向 Agent 工作负载的运行能力。你可以安装 cosh、OS Skills 和
所需的 Agent 语义层能力组件，使用 cosh 作为终端入口，并完成模型接入与相关服务的配置。

请先阅读[安装指南](https://agentic-os.sh/zh/docs/user-guide/getting-started/installation/)，
确认平台要求，再完成软件包安装与服务配置。随后，你可以参照
[cosh 首次使用指南](https://agentic-os.sh/zh/docs/user-guide/cosh-ng/)，开始使用终端。

<a id="为已有-agent-framework-安装插件包"></a>

<a id="12-为已有-agent-启用-anolisa"></a>

### 1.3 为已有 Agent 启用 ANOLISA

如果你希望继续使用已有的 Agent 应用、框架或 Harness，可以通过适配器与插件，
为现有运行环境启用 ANOLISA 的 headless 系统能力。接入后，你仍可使用原有 Agent
的界面、模型账号与工作方式。

当前可用的接入流程见[安装与适配器说明](https://agentic-os.sh/zh/docs/user-guide/getting-started/installation/)。

<a id="为什么需要-anolisa"></a>

## 2. 为什么需要 ANOLISA

**ANOLISA 是系统软件栈，用于增强 Agent 应用、框架和 Harness，不替代它们。**

Agent 在规划任务和调用工具时，也需要运行环境提供上下文管理、技能发现、操作约束
和运行记录等系统支持。ANOLISA 从以下几个方面改善 Agent 的工作环境：

- **把上下文留给有用的信息。** 工具输出压缩可以精简冗长的结果，持久化记忆可以让 Agent 跨会话检索和复用信息，减少重复读取。
- **按任务获取系统技能。** Agent 可以按任务查阅 OS 操作说明，并发现和读取所需的 Skills。
- **让操作有边界，工作区有退路。** 安全策略和底层隔离机制约束 Agent 的操作；工作区快照让用户可以检查文件变更，并在需要时回滚变更。
- **看清一次任务如何完成。** 你可以查看 Agent 的模型与工具调用、运行轨迹和 Token 消耗，排查任务失败的原因并分析运行开销。

我们欢迎 **OS 发行商与社区**采用并共建 ANOLISA，将其集成到现有发行版，
或在设计新的 Agentic OS 时就将其纳入整体架构。

**企业 Agent 治理平台的开发者**与**沙箱服务提供方**也可以在运行环境中预置 ANOLISA，
为这些环境中的 Agent 工作负载提供上下文管理、安全与观测等系统支持。

<a id="示例"></a>

<a id="5-示例"></a>

## 3. 示例

<table width="100%">
  <tr>
    <td width="33%" align="center" valign="top">
      <video
        controls
        muted
        preload="metadata"
        width="240"
        src="https://github.com/user-attachments/assets/ed6952c6-19da-44c7-ae35-85d753bdccf9"
      ></video>
      <br>
      <sub><strong>可观测：观察一次 Agent 运行</strong><br>查看模型与工具调用、运行轨迹和 Token 消耗。<br><a href="https://agentic-os.sh/zh/docs/user-guide/agentsight/">复现指南</a></sub>
    </td>
    <td width="33%" align="center" valign="top">
      <video
        controls
        muted
        preload="metadata"
        width="240"
        src="https://github.com/user-attachments/assets/b372ae72-44fa-492f-9feb-e6cd137b631a"
      ></video>
      <br>
      <sub><strong>上下文压缩：查看工具输出压缩效果</strong><br>体验 Tokenless，查看工具输出压缩与 Token 消耗变化。<br><a href="https://agentic-os.sh/zh/docs/user-guide/tokenless/measuring-savings/">复现指南</a></sub>
    </td>
    <td width="33%" align="center" valign="top">
      <video
        controls
        muted
        preload="metadata"
        width="240"
        src="https://github.com/user-attachments/assets/aad6e296-7c5a-4a81-be2e-ea4f49e43637"
      ></video>
      <br>
      <sub><strong>安全：检测 Skill 文件变更</strong><br>使用 Qoder 检查 Skill 文件变更，查看重新扫描结果。<br><a href="https://agentic-os.sh/zh/docs/user-guide/agent-sec-core/qoder-skill-ledger-demo/">复现指南</a></sub>
    </td>
  </tr>
</table>

<a id="组件"></a>

<a id="3-组件"></a>

## 4. 组件

ANOLISA 的组件分为用户入口层、统一接入层与 Agent 语义层。各层职责见[术语表](#术语表)。

<a id="31-用户入口层"></a>

### 4.1 用户入口层

| 组件 | 提供什么 |
|---|---|
| [cosh (cosh-ng)](https://agentic-os.sh/zh/docs/user-guide/cosh-ng/) | 提供保留熟悉 Shell 交互方式的 AI 原生终端，让用户通过自然语言调用工具与 Skills、完成任务。 |
| [OS Skills](https://agentic-os.sh/zh/docs/user-guide/os-skills/) | 面向 AI Agent 的 OS 使用说明书，覆盖系统管理、监控、安全和 DevOps 自动化等场景。 |

<a id="32-统一接入层"></a>

### 4.2 统一接入层

| 组件 | 提供什么 |
|---|---|
| [AW](https://agentic-os.sh/zh/docs/user-guide/aw/) | 以统一架构向上对接 Agent framework，向下衔接可扩展的 AI 原生系统软件。 |

<a id="33-agent-语义层"></a>

### 4.3 Agent 语义层

| 组件 | 提供什么 |
|---|---|
| [Tokenless](https://agentic-os.sh/zh/docs/user-guide/tokenless/) | 通过工具输出压缩、工具 Schema 精简、命令改写等方式，减少 AI Agent 的 Token 消耗。 |
| [Agent Memory](https://agentic-os.sh/zh/docs/user-guide/agent-memory/) | 为 Agent 保存持久化记忆，供 Agent 在后续会话中检索和复用。 |
| [SkillFS](https://agentic-os.sh/zh/docs/user-guide/skillfs/) | 将 Skills 组织成稳定的文件系统视图，供 Agent 按需发现和读取。 |
| [Workspace Checkpoint (ws-ckpt)](https://agentic-os.sh/zh/docs/user-guide/ws-ckpt/) | 创建 Agent 工作区快照，让用户可以比较文件差异，并在需要时回滚变更。 |
| [Agent Security Core](https://agentic-os.sh/zh/docs/user-guide/agent-sec-core/) | 面向 AI Agent 的 OS 级安全内核。 |
| [AgentSight](https://agentic-os.sh/zh/docs/user-guide/agentsight/) | Agent 可观测性工具，用于查看运行轨迹、LLM 调用和 Token 消耗。 |

[ANOLISA CLI](distribution/anolisa/README_zh.md) 负责组件安装与生命周期管理。
[ktuner](https://agentic-os.sh/zh/docs/user-guide/ktuner/) 检查 Linux 内核参数，
给出调优建议，并支持应用或回滚内核参数变更。各组件支持的平台见
[安装指南](https://agentic-os.sh/zh/docs/user-guide/getting-started/installation/)。

<a id="开放架构"></a>

<a id="4-开放架构"></a>

## 5. 开放架构

AW 位于 ANOLISA 的统一接入层，负责连接上层 Agent 与下层系统软件。开发者可以
为尚未支持的 Agent 开发适配器，也可以编写面向 Agent 工作负载优化的系统软件，
再通过 Provider 协议将这些软件的能力接入 ANOLISA。原有 Agent 应用或框架继续
负责模型、界面和权限管理。

目前，AW 已接入命令安全检查和工具内容优化，也支持通过可选配置关联 AgentSight
运行记录。现有操作系统、内核和沙箱负责提供底层资源与隔离机制。ANOLISA 面向
AI PC 与云桌面、Agent 应用与框架、Agent 平台，以及 Agent 运行基础设施等使用
环境设计。

<a id="配置与运行示例"></a>

<a id="41-配置与运行示例"></a>

### 5.1 配置与运行示例

以下示例通过一份配置同时启用安全检查与上下文优化。运行该示例前，请先按
[AW 安装指南](https://agentic-os.sh/zh/docs/user-guide/aw/installation/)安装 Linux
Preview 版的 `aw-all-in-one`，设置 `AW_PREFIX` 与 `PATH`，再按
[安全后台准备步骤](https://agentic-os.sh/zh/docs/user-guide/aw/security/)启动 sec-core。
CI 安装包要求使用 Linux x86_64 系统，且 glibc 版本不低于 2.39。你还需要安装
Qoder CLI 1.1.64 或更新版本，并完成登录。

请使用平时运行 Qoder 的用户账号执行以下命令，创建新的 `anolisa.yaml` 并启动
Agent。示例中的 socket 路径用于连接安全指南中的本地试用后台；如果你要连接
已有的其他安全后台，请将 `AW_SEC_SOCKET` 改为该服务实际使用的 socket 路径。

```bash
export AW_HOME="$HOME/aw"
export AW_SEC_SOCKET=/run/aw-preview-sec/daemon.sock
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/anolisa.yaml" --state-dir "$AW_HOME/anolisa-state" \
  --qoder "$(command -v qodercli)" \
  --provider sec-core+tokenless --socket "$AW_SEC_SOCKET"
aw validate --config "$AW_HOME/anolisa.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/anolisa.yaml" --agent qoder
```

对于配置覆盖的工具调用，sec-core 在工具执行前进行安全检查。工具成功执行后，
Tokenless 会尝试压缩受支持的文本结果。Agent 调用工具后，你可以打开另一个终端，
设置与启动 Agent 时所用终端相同的 `PATH` 和 `AW_HOME`，再运行以下命令查看运行记录：

```bash
aw status --config "$AW_HOME/anolisa.yaml"
aw inspect --config "$AW_HOME/anolisa.yaml" --json
```

退出 Agent 后，请执行 `aw stop --config "$AW_HOME/anolisa.yaml"` 停止 AW 本地服务，
再根据安全后台的启动方式，使用相应的命令停止该服务。其他 Agent 的接入方式、规则配置
与观测能力接入方法，见[AW 用户指南](https://agentic-os.sh/zh/docs/user-guide/aw/)。

<a id="开发与扩展"></a>

<a id="42-开发与扩展"></a>

### 5.2 开发与扩展

开发者可以根据接入目标选择相应指南：

- [Agent 与框架接入](https://agentic-os.sh/zh/docs/developer-guide/aw/)：介绍如何把 AW 接入 Agent 应用或框架的原生执行流程。
- [Provider 开发与协议](https://agentic-os.sh/zh/docs/developer-guide/aw/providers/)：介绍如何通过 Provider 协议将自有系统软件接入 ANOLISA。
- [SDK 指南](https://agentic-os.sh/zh/docs/developer-guide/sdk/)：介绍如何在应用代码中调用组件能力。

<a id="文档与开发"></a>

## 6. 文档与开发

| 目标 | 入口 |
|---|---|
| 安装、配置和使用组件 | [用户指南](https://agentic-os.sh/zh/docs/user-guide/) · [安装指南](https://agentic-os.sh/zh/docs/user-guide/getting-started/installation/) · [排障](https://agentic-os.sh/zh/docs/user-guide/getting-started/troubleshooting/) |
| 在应用中嵌入组件能力 | [SDK 指南](https://agentic-os.sh/zh/docs/developer-guide/sdk/) |
| 为 Agent 或框架接入 AW | [AW 接入指南](https://agentic-os.sh/zh/docs/developer-guide/aw/) |
| 开发和分发 AW Provider | [Provider 开发与协议](https://agentic-os.sh/zh/docs/developer-guide/aw/providers/) · [Provider 实现](providers/README_zh.md) |
| 参与 ANOLISA 开发 | [贡献者指南](CONTRIBUTING_zh.md) · [分组件章节](https://agentic-os.sh/zh/docs/contribute/) |

<a id="仓库目录"></a>

## 7. 仓库目录

仓库分别存放运行能力组件、发行工具、评测工具与保留的旧版实现。组件源码迁移到
其他目录后，该组件的命令、包名和发布 tag 保持不变。

| 目录 | 内容 |
|---|---|
| [`src/`](src/) | 运行能力组件，包括 AW、cosh-ng，以及提供可观测、安全、记忆和沙箱能力的组件。各组件保留自己的构建与测试入口。 |
| [`providers/`](providers/README_zh.md) | AW Provider 协作空间，每个 Provider 实现包含独立的 workspace、示例和契约测试。 |
| [`distribution/`](distribution/) | 发行工具与镜像。`anolisa/` 包含安装 CLI、组件清单和打包资源；`docker/` 定义整套 ANOLISA 容器镜像。 |
| [`benchmark/`](benchmark/) | 跨组件评测工具，包括 ClawEval、SWE 和 Terminal Bench runner。组件专用 benchmark 仍保留在各组件内。 |
| [`deprecated/`](deprecated/) | 保留的旧版实现及其归档文档。 |
| [`scripts/`](scripts/) | 仓库级构建、打包和校验脚本。 |
| [`tests/`](tests/) | 仓库级回归检查与测试编排。组件测试仍放在各组件内。 |
| [`contribute/`](contribute/) | 按组件组织的贡献指南，仓库根目录的贡献指南提供了这些章节的入口。 |
| [`docs/`](docs/) | 用户和开发者手册、源码构建说明及共享文档资源。 |
| [`specs/`](specs/) | 贡献规范、组件接入规则和文档标准。 |
| [`website/`](website/) | 公共网站源码和生成器。生成物不作为另一份文档来源。 |
| [`.github/`](.github/) | CI、发布流程、组件路由元数据和维护归属。 |

修改文件前，请先阅读 [AGENTS.md](AGENTS.md)，了解仓库规则、组件位置和构建测试
命令，再阅读对应组件内的 `AGENTS.md`（如有）。贡献流程见
[CONTRIBUTING_zh.md](CONTRIBUTING_zh.md)。调整源码路径时，请同步更新
`.github/components.json`，以及构建、发布、维护归属配置和文档入口中引用的路径，
同时保持组件的命令、包名和发布 tag 不变。

<a id="社区"></a>

## 8. 社区

<div align="center">

<img src="docs/images/readme/dingtalk-qr.png" alt="ANOLISA 钉钉社区二维码" width="180"/>

使用钉钉扫码加入 ANOLISA 社区。

</div>

- 如果你遇到问题，或希望项目支持新的 Agent 使用场景，欢迎[提交 Issue](https://github.com/agentic-os-org/ANOLISA/issues)。
- 提交 Pull Request 前，请先阅读[贡献指南](CONTRIBUTING_zh.md)。
- 如果你发现安全漏洞，请通过[安全策略](SECURITY.md)中列出的渠道报告。

<a id="许可证"></a>

## 9. 许可证

ANOLISA 基于 [Apache License 2.0](LICENSE) 发布。

<a id="更多资源"></a>

## 10. 更多资源

[用户指南](https://agentic-os.sh/zh/docs/user-guide/) ·
[故障排查](https://agentic-os.sh/zh/docs/user-guide/getting-started/troubleshooting/) ·
[源码构建](https://agentic-os.sh/zh/docs/user-guide/getting-started/building/) ·
[变更日志](https://agentic-os.sh/zh/changelog/) ·
[Agent 入口](https://agentic-os.sh/zh/agents/)

Agent Security Core 的相关安全参考资料如下：
[AARM 登记条目（Aligned）](https://aarm.dev/builders/agentseccore-anolisa) ·
[OWASP Agentic Top 10 控制映射](docs/user-guide/zh/agent-sec-core/owasp-agentic-top10.md)。

Blaze 已迁移至独立仓库
[blaze-rs](https://github.com/agentic-os-org/blaze-rs)。

<a id="术语表"></a>

## 11. 术语表

| 术语 | 含义 |
|---|---|
| ANOLISA | 面向 AI Agent 工作负载的多组件系统软件栈。 |
| Agentic OS | 面向 AI Agent 工作负载的操作系统形态。 |
| 用户入口层（User Entrypoint Layer） | cosh 为用户提供终端交互，OS Skills 为 Agent 提供 OS 使用说明。 |
| 统一接入层（AW Unified Stack Layer） | AW 在这一层向上对接 Agent 应用与框架，向下衔接可扩展的系统软件。 |
| Agent 语义层（Agent Semantic Layer） | 提供上下文管理、记忆、Skills 供给、工作区恢复、安全与观测等能力的组件层。 |

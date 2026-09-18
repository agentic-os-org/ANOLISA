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

**面向 Agent 工作负载的操作系统层。**

让 Agent 在你的终端里直接指挥系统干活，并在工具响应进入模型之前去掉冗余，
同时保留你现有的 Shell、Agent 框架和沙箱。

[English](README.md) · [项目网站](https://agentic-os.sh/zh/) ·
[快速开始](https://agentic-os.sh/zh/docs/quickstart/) ·
[用户指南](https://agentic-os.sh/zh/docs/user-guide/) ·
[参与贡献](https://github.com/alibaba/anolisa/blob/main/CONTRIBUTING_zh.md)

[![许可证](https://img.shields.io/badge/License-Apache_2.0-blue.svg)](https://github.com/alibaba/anolisa/blob/main/LICENSE)
[![平台](https://img.shields.io/badge/Platform-Linux%20%7C%20macOS-lightgrey.svg)](https://agentic-os.sh/zh/docs/user-guide/installation/)
[![AARM Aligned](https://img.shields.io/badge/AARM-Aligned-blue.svg)](https://aarm.dev/builders/agentseccore-anolisa)
[![OWASP Agentic Top 10: 7 Full, 3 Partial](https://img.shields.io/badge/OWASP_Agentic_Top_10-7_Full,_3_Partial-blue)](docs/user-guide/zh/agent-security/owasp-agentic-top10.md)

</div>

---

ANOLISA 是面向 AI Agent 工作负载的服务端操作系统层。它从终端入口、Token
开销和执行环境三个方向解决 Agent 运行中的关键问题，同时保留现有的 Shell、
Agent 框架和沙箱。ANOLISA CLI 提供统一的安装入口，各项能力可以按需启用。

**第一次使用 ANOLISA？**
[从快速入门选择你的第一个目标 →](https://agentic-os.sh/zh/docs/quickstart/)

## 组件

<table width="100%">
  <thead>
    <tr>
      <th width="340">Agent 入口</th>
      <th width="340">上下文效率</th>
      <th width="340">运行与安全</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/user-entrypoint/cosh-ng/quickstart/">cosh-ng</a></strong><br><sub>Shell AI 副驾</sub></td>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/token-saving/tokenless/quickstart/">Token-less</a></strong><br><sub>压缩工具输出</sub></td>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/runtime/ws-ckpt/">ws-ckpt</a></strong><br><sub>检查点与回滚</sub></td>
    </tr>
    <tr>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/user-entrypoint/os-skills/">OS Skills</a></strong><br><sub>系统与 DevOps 经验</sub></td>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/agent-observability/agentsight/">AgentSight</a></strong><br><sub>轨迹与 Token 观测</sub></td>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/runtime/skillfs/">SkillFS</a></strong><br><sub>聚焦的 Skill 视图</sub></td>
    </tr>
    <tr>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/user-entrypoint/ktuner/">ktuner</a></strong><br><sub>内核调优</sub></td>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/token-saving/agent-memory/">Agent Memory</a></strong><br><sub>跨会话记忆</sub></td>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/agent-security/agent-sec-core/quickstart/">Agent Sec Core</a></strong><br><sub>沙箱与安全校验</sub></td>
    </tr>
    <tr>
      <td></td>
      <td></td>
      <td><strong><a href="https://agentic-os.sh/zh/docs/user-guide/runtime/blaze/">Blaze</a></strong><br><sub>沙箱生命周期</sub></td>
    </tr>
  </tbody>
</table>

## 解决什么问题

<p align="center"><strong>01 · AGENT INTERFACE</strong></p>

<h3 align="center">让 Agent 直接在终端工作</h3>

cosh-ng 是面向 AI 时代重构的 Linux 终端。它保留熟悉的 Bash/Zsh 行为，同时加入
能理解意图、调用工具与 Skills、并在高风险操作前请求确认的 Agent。Shell 命令和
自然语言共用一个终端，不必切换到独立的聊天应用。

[开始使用 cosh-ng →](https://agentic-os.sh/zh/docs/user-guide/user-entrypoint/cosh-ng/quickstart/)

<p align="center"><strong>02 · CONTEXT EFFICIENCY</strong></p>

<h3 align="center">看清 Token 去向，在内容进入模型前减少无效消耗</h3>

[Token-less](https://agentic-os.sh/zh/docs/user-guide/token-saving/tokenless/quickstart/)
在工具 Schema 和响应进入模型前去掉冗余。
[Agent Memory](https://agentic-os.sh/zh/docs/user-guide/token-saving/agent-memory/)
复用跨会话信息。
[SkillFS](https://agentic-os.sh/zh/docs/user-guide/runtime/skillfs/)
只在当前视图展示任务需要的 Skills，其余能力需要时仍可发现。
[AgentSight](https://agentic-os.sh/zh/docs/user-guide/agent-observability/agentsight/)
记录 Token 花在哪里。

### 从内核层看清一次 Agent 运行

在 Linux 上，AgentSight 使用 eBPF 观测 Agent，无需改动其代码。沿着用户输入
查看模型和工具调用，Token 消耗与子代理分支也在同一视图里。

[打开 AgentSight 指南 →](https://agentic-os.sh/zh/docs/user-guide/agent-observability/agentsight/)

<table align="center" cellpadding="0" cellspacing="0">
  <tr>
    <td>
      <video
        controls
        muted
        preload="metadata"
        src="https://github.com/user-attachments/assets/ed6952c6-19da-44c7-ae35-85d753bdccf9"
      ></video>
    </td>
  </tr>
</table>

### 使用 Claude Code，3 分钟体验 Token-less

安装 Token-less，并将它接入 Claude Code：

```bash
curl -fsSL https://get.agentic-os.sh | bash
export PATH="$HOME/.local/bin:$PATH"
anolisa install tokenless
anolisa adapter enable tokenless claude-code
```

重启 Claude Code，运行一次工具密集型任务，再查看结果：

```bash
tokenless stats summary
tokenless stats list --limit 5
```

[打开完整 Token-less 快速体验 →](https://agentic-os.sh/zh/docs/user-guide/token-saving/tokenless/quickstart/)
· [阅读完整用户手册](https://agentic-os.sh/zh/docs/user-guide/token-saving/tokenless/user-manual/)

<table align="center" cellpadding="0" cellspacing="0">
  <tr>
    <td>
      <video
        controls
        muted
        src="https://github.com/user-attachments/assets/b372ae72-44fa-492f-9feb-e6cd137b631a"
      ></video>
    </td>
  </tr>
</table>

<p align="center">
  <sub>
    在一次编码任务的单次观测中，Token-less 节省了 317K Tokens（40.5%，
    基于 AgentSight 观测）。
    实际效果因工作负载而异。
  </sub>
</p>

`debug`、`trace` 命中字段黑名单，`metadata` 为 null，`tags` / `extra` 为空值，
均被移除。压缩在 Agent 与模型之间执行，无需改动 Agent 框架代码；被截断的数组
元素可通过 `<<tokenless:KEY>>` 标记取回，压缩过程可逆。

| 工具响应 | 工具 Schema | 整体压缩 |
|----------|-------------|----------|
| **Token 减少 65.8%** | **Token 减少 47.3%** | **Token 减少 62.9%** |
| ResponseCompressor · 46.85 µs | SchemaCompressor · 11.44 µs | 198.91 µs |

节省比例针对进入上下文的工具响应，不代表整个会话的账单。具体工作负载的估算方法
见 [Token-less 用户手册](https://agentic-os.sh/zh/docs/user-guide/token-saving/tokenless/user-manual/)。

<p align="center"><strong>03 · EXECUTION RUNTIME</strong></p>

<h3 align="center">让 Agent 的每次执行都有边界，也留有退路</h3>

ANOLISA 正在完善面向 Agent 的执行环境。
[Agent Sec Core](https://agentic-os.sh/zh/docs/user-guide/agent-security/agent-sec-core/quickstart/)
隔离高风险操作，[ws-ckpt](https://agentic-os.sh/zh/docs/user-guide/runtime/ws-ckpt/)
为工作区变更保留恢复点。

### 在 Skill 运行前发现它已被改动

已签名的 Skill 发生变化后，Agent 会在再次调用前报告 `drifted`。
重新扫描发现阻断级风险时，新版本记录为 `deny`。

[查看 Agent 演示 →](https://agentic-os.sh/zh/docs/user-guide/agent-security/agent-sec-core/qoder-skill-ledger-demo/)
· [Skill Ledger 手册](https://agentic-os.sh/zh/docs/user-guide/agent-security/agent-sec-core/skill-ledger/)

<table align="center" cellpadding="0" cellspacing="0">
  <tr>
    <td>
      <video
        controls
        muted
        preload="metadata"
        src="https://github.com/user-attachments/assets/aad6e296-7c5a-4a81-be2e-ea4f49e43637"
      ></video>
    </td>
  </tr>
</table>

[选择运行与安全能力的开始位置 →](https://agentic-os.sh/zh/docs/quickstart/)
· [通过 ANOLISA CLI 开始](https://agentic-os.sh/zh/docs/user-guide/user-entrypoint/anolisa-cli/)

## 安装

ANOLISA CLI 是统一的安装入口。cosh-ng 使用 system mode 安装；Token-less 和
其他能力可独立按需添加。

```bash
curl -fsSL https://get.agentic-os.sh | bash

sudo anolisa --install-mode system install cosh-ng
anolisa install tokenless
```

运行 `cosh` 进入 AI 原生终端。Token-less 也可直接优化现有 Agent 的工具调用，
无需更换 Agent 框架。

[查看快速开始 →](https://agentic-os.sh/zh/docs/quickstart/)

## 标准与合规

| 标准／框架 | 状态与说明 |
|---|---|
| [AARM](https://aarm.dev/builders/agentseccore-anolisa) | **Aligned** · 登记条目：AgentSecCore (ANOLISA) |
| [OWASP Agentic Top 10 (2026)](docs/user-guide/zh/agent-security/owasp-agentic-top10.md) | 全部 10 类 ASI 风险均已映射至安全控制 |

## 文档

[快速开始](https://agentic-os.sh/zh/docs/quickstart/) ·
[安装指南](https://agentic-os.sh/zh/docs/user-guide/installation/) ·
[用户指南](https://agentic-os.sh/zh/docs/user-guide/) ·
[故障排查](https://agentic-os.sh/zh/docs/user-guide/troubleshooting/) ·
[源码构建](https://agentic-os.sh/zh/docs/building/) ·
[变更日志](https://agentic-os.sh/zh/changelog/)

## 社区

<div align="center">

<img src="docs/images/readme/dingtalk-qr.png" alt="ANOLISA 钉钉社区二维码" width="180"/>

使用钉钉扫码加入 ANOLISA 社区。

</div>

- 遇到问题或有新的 Agent 场景，欢迎[提交 Issue](https://github.com/alibaba/anolisa/issues)。
- 提交 Pull Request 前，请先阅读
  [贡献指南](https://github.com/alibaba/anolisa/blob/main/CONTRIBUTING_zh.md)。
- 安全问题请通过
  [安全策略](https://github.com/alibaba/anolisa/blob/main/SECURITY.md)中的渠道报告。

## 许可证

ANOLISA 基于
[Apache License 2.0](https://github.com/alibaba/anolisa/blob/main/LICENSE) 发布。

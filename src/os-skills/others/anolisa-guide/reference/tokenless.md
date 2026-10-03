> **爬取时间**: 2026-06-17 16:29:22
> **原文链接**: https://help.aliyun.com/zh/alinux/how-to-use-tokenless
> **文档更新**: 2026-05-28T10:06:46+08:00

---

Tokenless 是 ANOLISA 的 Token 优化组件，旨在不侵入业务逻辑的前提下，通过自动压缩工具定义与模型响应内容，显著降低 LLM 推理成本，并支持 cosh 与 OpenClaw 等主流 Agent 场景。本文介绍如何安装配置使用Tokenless组件。

## 如何使用Tokenless

## 产品简介

Tokenless 是 ANOLISA 的 Token 优化组件，在不侵入业务逻辑的前提下，自动压缩工具定义和模型响应内容，有效降低 LLM Token 消耗。

## 核心能力

Tokenless 主要包含以下能力：

| 能力  | 说明  |
| --- | --- |
| 上下文压缩 | *精简 Function Calling工具定义、过滤 CLI 命令响应中的干扰信息、紧凑编码压缩结构化数据* |
| 统计追踪 | 自动记录压缩前后对比，按类型汇总节省量 |
| 透明集成 | 通过插件和 Hook 自动介入，对 Agent 框架零侵入 |

* * *

**使用范围**

本工具适用于 cosh(Copilot Shell) 以及 OpenClaw。

### 安装方式

支持 RPM 包安装（推荐用于 Alinux 4）与源码一键安装两种方式。

**RPM 包安装（推荐）**：

```
# 使用 yum 安装（自动解决依赖）
sudo yum install tokenless
```

**源码一键安装**：

```
git clone --recursive https://github.com/alibaba/anolisa.git
cd src/tokenless
make setup
```

## 配置与集成

### Copilot Shell 集成

Tokenless 通过 Hook 机制集成到 cosh（Copilot Shell）中，通过install.sh脚本自动完成配置使能：

```
/usr/share/tokenless/scripts/install.sh --cosh
```

### OpenClaw 集成

通过install.sh脚本自动完成openclaw插件集成：

```
/usr/share/tokenless/scripts/install.sh --openclaw
```

## 压缩效果

### stats命令查询

可通过 tokenless stats 命令查看tokenless压缩统计效果：

```
tokenless stats list
```

### AgentSight 集成

AgentSight 已经接入 Tokenless 压缩统计数据库，可通过 AgentSight 的 Dashboard 查看压缩效果，详情可参看《[如何使用AgentSight](https://help.aliyun.com/zh/alinux/how-to-use-agentsight)》。

# 配置与 Agent 启动

[English](../../../en/user-entrypoint/aw/configuration.md)

`aw.yaml` 保存 Agent 入口、启用的 Provider 和运行数据位置。
模型账号仍使用 Agent 自己的配置。首次配置先完成[安装](installation.md)。

## 生成配置

沿用安装页的变量。`configure` 不覆盖已有文件；修改配置见下一节。

```bash
aw-package configure --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
```

| 需要的能力 | 在生成命令中增加 |
| --- | --- |
| 只启动 Agent | 不增加 Provider 参数 |
| 工具输出压缩 | `--provider tokenless` |
| 工具安全检查 | `--provider sec-core --socket /run/agent-sec-core/daemon.sock` |
| 安全检查和压缩 | `--provider sec-core+tokenless --socket /run/agent-sec-core/daemon.sock` |

先安装所选 Provider；生成器会填入程序路径和校验信息。

## 修改已有配置

退出使用这份配置的 Agent，用原配置停止服务后再编辑。配置不会自动重载。

```bash
aw stop --config "$AW_HOME/aw.yaml"
cp "$AW_HOME/aw.yaml" "$AW_HOME/aw.yaml.bak"
aw-package config show --config "$AW_HOME/aw.yaml"
```

可以直接编辑 YAML，也可以用 `aw-package config` 增删 Provider 和 Hook。
文件与父目录应属于当前用户，且不能被其他用户写入；配置编辑器要求绝对路径，
不接受符号链接。`show` 会显示配置中的私有值，分享输出前请自行删去敏感内容。

```bash
aw-package config validate --config "$AW_HOME/aw.yaml"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

校验成功会打印 `Valid configuration`。它检查字段和引用，实际程序是否可用会在启动时检查。
YAML 中的路径须写成真实绝对路径，AW 不展开 `$HOME` 等变量。
完整编辑命令和自定义规则示例见[配置工具参考](../../../../developer-guide/zh/aw/packages.md#编辑已有-provider-与-hook-配置)。

## 选择 Agent

一个配置可以保存多个目标，`--agent` 选择 `spec.agents` 中的名称。
生成器目前支持 Qoder 和 OpenClaw；QwenPaw、Hermes 及 SDK 应用使用
[原生接入参考](../../../../developer-guide/zh/aw/native-agents.md)。

| Agent | 当前要求 | 使用入口 |
| --- | --- | --- |
| Qoder | CLI 1.1.64 或更新版本，原生 Hook 可用 | 交互或 print |
| OpenClaw | 2026.9.6，已初始化原生配置和状态目录 | 新启动的 Gateway |
| QwenPaw | 2.2.2b4，AgentScope 2.0.9 | App/API |
| Hermes | 使用[兼容版本及安装步骤](../../../../developer-guide/zh/aw/native-agents.md#启动-hermes) | 本地 chat |
| AgentScope、Pi、Claude SDK | 需要在应用中显式接线 | [SDK 接入参考](../../../../developer-guide/zh/aw/native-agents.md) |

### Qoder

从希望它操作的目录启动。`--` 后面的参数交给 Qoder。

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

已有附加设置文件时，使用 AW 的 `--native-settings /absolute/qoder.settings.json`。
AW 会保留原有设置并加入本次回调。

### OpenClaw

先停止使用同一原生 profile 的 Gateway。将变量改为已有安装和 profile 的绝对路径。

```bash
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw.mjs
export AW_OPENCLAW_CONFIG=/absolute/path/to/openclaw.json
export AW_OPENCLAW_STATE=/absolute/path/to/openclaw-state
aw-package configure --prefix "$AW_PREFIX" \
  --config "$AW_HOME/openclaw.yaml" --state-dir "$AW_HOME/openclaw-aw-state" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
aw run --config "$AW_HOME/openclaw.yaml" --agent openclaw \
  --native-settings "$AW_OPENCLAW_CONFIG" --native-state-dir "$AW_OPENCLAW_STATE"
```

等待 `AW openclaw native hooks ready` 后，通过连接该 Gateway 的客户端使用。
需要压缩或安全检查时，在生成配置时增加上表参数。`agent --local` 会绕过这个 Gateway。

## 添加自己的规则

简单规则可以从 stdin 读取工具事件，向 stdout 输出 JSON `true` 或 `false`。
配置决定命中时观察还是阻断。执行故障由 `on_error` 处理，安全检查通常选 `block`。
按[布尔规则教程](../../../../developer-guide/zh/aw/integration.md)准备脚本并验证允许和阻断两种情况。

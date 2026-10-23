# 配置与 Agent 启动

[English](../../en/aw/configuration.md)

`aw.yaml` 保存 Agent 入口、启用的 Provider 和运行数据位置。
模型账号仍使用 Agent 自己的配置。首次配置先完成[安装](installation.md)。

## 生成配置

沿用安装页的变量。`config init` 不覆盖已有文件；修改配置见下一节。

```bash
aw config init --prefix "$AW_PREFIX" \
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

退出使用这份配置的 Agent，用原配置停止服务后再编辑。配置文件不会被监听；
已显式启用稳定桥接的实例可使用下述空闲更新流程。

```bash
aw stop --config "$AW_HOME/aw.yaml"
cp "$AW_HOME/aw.yaml" "$AW_HOME/aw.yaml.bak"
aw config show --config "$AW_HOME/aw.yaml"
```

可以直接编辑 YAML，也可以用 `aw config` 增删 Provider 和 Hook。
文件与父目录应属于当前用户，且不能被其他用户写入；配置编辑器要求绝对路径，
不接受符号链接。`show` 会显示配置中的私有值，分享输出前请自行删去敏感内容。

```bash
aw config validate --config "$AW_HOME/aw.yaml"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

校验成功会打印 `configuration valid`。它检查字段和引用，实际程序是否可用会在启动时检查。
YAML 中的路径须写成真实绝对路径，AW 不展开 `$HOME` 等变量。
完整编辑命令和自定义规则示例见[配置工具参考](../../../developer-guide/zh/aw/packages.md#编辑已有-provider-与-hook-配置)。

## 比较策略修改

准备独立的候选文件，与现有配置比较：

```bash
aw validate --config next.yaml --against current.yaml
```

两份文件都经过完整校验，只允许 Provider、事件和默认事件预算变化。
Agent 入口、原生调度、服务路径、审计、观测和元数据必须相同。
成功打印 `policy update scope valid; runtime activation is not checked`。
比较不运行 Provider、不修改文件、不检查原生就绪，也不更新运行中的 Agent。
启用稳定桥接的实例可使用下述显式空闲更新；其他实例使用前述停止、编辑、启动流程。
原有 `aw validate --config FILE` 继续打印 `configuration valid`。

`aw config validate` 与 `aw validate` 等价，支持 `--against` 和相对文件路径，
不执行编辑器的所有权检查。独立 `aw-package` 命令及其更严格的编辑器验证已删除。
初始化、查看和编辑继续使用 `aw config init`、`aw config show` 与编辑命令；
这些动作的文件安全检查并不意味着 `aw config validate` 替代了已删除的严格验证器。

## 注册原生 Hook

对于支持持久原生注册的 Agent，使用 `aw agent connect --config FILE --agent TARGET`，
并提供所需的原生选项，例如 Hermes 的 `--native-profile`。它沿用既有 Adapter 注册
和支持性检查，不安装 Agent 或软件包。旧的 `aw install --config FILE --agent TARGET`
语法仍兼容；与 `--prefix` 或 `--bundle` 混用会被拒绝。

## 更新预热实例

需要保留预热宿主并更新 AW Hook 时，在原启动入口显式启用稳定桥接：

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder --reloadable
```

运行时路径固定 Qoder 1.1.73 TUI，或 QwenPaw 2.2.2b4 / AgentScope 2.0.9 App。
QwenPaw 按原生接入参考选择配置目标并设置已初始化的 `QWENPAW_WORKING_DIR`。
普通 `aw run` 保持现有行为；未预装稳定桥接的实例不能通过此命令升级，外部 attach 后续提供。

独立准备 `next.yaml`，在另一个终端通过原启动配置定位现有服务并更新目标：

```bash
aw update --config "$AW_HOME/aw.yaml" --agent qoder --policy next.yaml
```

命令选择该目标唯一的存活实例，并读取当前策略版本。这个版本仍用于乐观并发校验：
若另一个更新先完成，本次返回冲突，不会自动重试。没有匹配实例时报错；多个匹配实例时
要求补充 `--instance ID`。通过 `aw status --config "$AW_HOME/aw.yaml"` 查询实例 ID 和版本。
选择范围包括未就绪和未启用热更新的实例，不会在忙碌实例旁边默默选择一个空闲实例。

需要显式选择实例或接入外部管控时：

```bash
aw update --socket "$AW_SOCKET" --instance "$AW_INSTANCE" \
  --policy next.yaml --expected-revision "$AW_POLICY_REVISION"
```

`--socket` 替代 `--config`；`--instance` 也可用于消除 `--agent` 的多实例歧义。
`--expected-revision` 可选，在调用方需要指定前置版本时覆盖查询到的版本。冲突不自动重试。

成功返回实际激活的 `policy_revision`、前版本、审计 key、`preparation_ms` 和 `activation_ms`。
命令不修改两份文件、不重启原生宿主，也不会启动服务。原启动文件保持不变，用于
`aw update`、`aw status` 和 `aw stop`：自动服务路径由启动摘要决定，不使用候选策略摘要。
新 binding 仍使用服务最初的默认策略；原地修改启动文件不能重新定位运行中的实例。

热更新模式下，生命周期确认缺失或无法取得激活计划时，`tool.before` 会阻断。
成功取得计划后，每个步骤仍使用配置的 report/block 错误策略。

活跃 Agent 请求（含没有工具执行的模型等待）返回 `busy`，旧策略保持生效，更新不排队，
之后也不会自动应用。准备失败、版本冲突和不支持的原生策略同样保留旧策略。
超时无法确定是否已经生效时，先查询实际版本再派发工作。管控侧必须独占分配实例，并将
更新与 Session 派发串行化；成功确认不等于预留下一条请求。同一 App 的多个 Session 共用
实例策略，任一活跃请求都会阻止切换。

在创建或派发首个用户 Session 前完成策略更新。模型账号、原生 Agent 定义由框架自己的
Session 配置处理；通过 AW 修改这些字段仍需另起实例。Qoder 原生工具 Hook 使用顺序执行时，AW 目标也须设置 `qoder.sequential: true`，以保留相同调度。
Qoder 的 `Stop` 不等于空闲，须等
原生 idle_prompt 通知。QwenPaw 在正常耗尽流后释放请求/工具占用；确认完成前提前关闭流、取消或异常退出
无法确认原生清理完成，因此保留 busy。AW 在原生清理完成后交付最终响应及终结错误/中断片段，
因此正常返回的结构化工具错误不会阻止后续更新；无法确定结束时
使用新实例。

本 Preview 已有 CLI 接入回归和固定版本 AgentScope middleware 回归。原生清单还要求
独立校验和固定的 Qoder 1.1.73 TUI 启动及派发前空闲切换用例。必需的 QwenPaw App HTTP 测试
使用确定性本地模型和真实 shell/read_file 工具，验证同一进程在成功及结构化错误结果后的多次策略切换。完整的托管 Qoder TUI
工具轮次采纳仍需独立验收。

## 选择 Agent

一个配置可以保存多个目标，`--agent` 选择 `spec.agents` 中的名称。
生成器目前支持 Qoder 和 OpenClaw；QwenPaw、Hermes 及 SDK 应用使用
[原生接入参考](../../../developer-guide/zh/aw/native-agents.md)。

| Agent | 当前要求 | 使用入口 |
| --- | --- | --- |
| Qoder | CLI 1.1.64 或更新版本，原生 Hook 可用 | 交互或 print |
| OpenClaw | 2026.9.6，已初始化原生配置和状态目录 | 新启动的 Gateway |
| QwenPaw | 2.2.2b4，AgentScope 2.0.9 | App/API |
| Hermes | 使用[兼容版本及安装步骤](../../../developer-guide/zh/aw/native-agents.md#启动-hermes) | 本地 chat |
| AgentScope、Pi、Claude SDK | 需要在应用中显式接线 | [SDK 接入参考](../../../developer-guide/zh/aw/native-agents.md) |

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
aw config init --prefix "$AW_PREFIX" \
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
按[布尔规则教程](../../../developer-guide/zh/aw/integration.md)准备脚本并验证允许和阻断两种情况。

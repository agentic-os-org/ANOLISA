# AW

[English](README.md)

AW 为 Agent 和 SDK 应用提供统一 Hook 编排、受管 MCP 接入和策略热更新。
通过一份配置组合工具执行前的安全检查、执行后的输出压缩和自定义规则。
支持的常驻入口可在任务排空后更新策略，保留宿主进程。
Agent 继续管理自己的模型、界面和权限。当前提供 Linux Preview。

## 通用能力

| 能力 | 用户能做什么 |
| --- | --- |
| Hook 编排 | 按目标和工具选择规则，组合 sec-core、Tokenless 与自定义 Hook，统一步骤顺序、执行期限和审计。 |
| 受管 MCP | 在支持的实际工具调用处执行同一套策略，检查调用身份及获准参数。 |
| 策略更新 | 向支持的空闲实例显式应用新策略，让后续任务使用新版本。 |

Adapter 接通原生事件并采纳支持的效果，Provider 实现具体策略。
core 可独立使用；安全检查与压缩需安装对应 Provider 并通过配置启用。
工具结果替换限于获准的位置，Agent 的原生权限检查始终保留。

## Hook 支持矩阵

✓ 已声明且有契约测试覆盖，× 尚不支持。矩阵从[回归清单](tests/regression-matrix.json)生成。结果替换的原生限制与 MCP、更新范围见[完整能力表](../../docs/user-guide/zh/aw/capabilities.md)。

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.after` / `observe` | `tool.after` / `replace_result` |
| --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ |
| `openclaw` | ✓ | ✓ | ✓ | ✓ |
| `pi` | ✓ | ✓ | ✓ | ✓ |
| `qoder` | ✓ | ✓ | ✓ | ✓ |
| `qoder-sdk` | ✓ | ✓ | ✓ | × |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ |
<!-- aw-regression-matrix:end -->

<a id="安装与使用"></a>

## 安装与首次运行

从[安装指南](../../docs/user-guide/zh/aw/installation.md)获取 Preview 包并安装。
当前使用独立安装包；`anolisa install aw` 和 RPM 安装入口尚未提供。
先安装并登录所选 Agent，再用 `aw config init` 生成配置。

下面以已安装、登录的 Qoder 为例。`AW_PREFIX` 是安装指南中选定的安装目录，
`qodercli` 和 `aw` 应已加入 `PATH`。

```bash
export AW_HOME="$HOME/aw"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$(command -v qodercli)"
aw validate --config "$AW_HOME/aw.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

进入 Qoder 后，请它用工具读取当前目录，并按原生提示处理权限确认。
退出 Agent 后，本地服务仍可复用。使用结束后查询并停止它。

```bash
aw status --config "$AW_HOME/aw.yaml"
aw stop --config "$AW_HOME/aw.yaml"
```

<a id="hook-支持"></a>
<a id="adapter-能力覆盖"></a>

## 支持范围

当前接入 Qoder CLI、Qoder SDK、OpenClaw、Hermes、QwenPaw、Pi SDK、
Claude SDK 和 AgentScope SDK。CLI 使用原生 Hook 或插件，SDK 应用需要显式接入受管工厂。
[Qoder SDK 接入](../../docs/developer-guide/zh/aw/agents/qoder-sdk.md)还支持固定摘要的 Bash 消费者、Worker import 和声明式角色资产。
[能力支持矩阵](../../docs/user-guide/zh/aw/capabilities.md)按 Adapter 和启动模式说明
结果处理、MCP 与策略更新的差异，并链接支持版本与接线步骤。

## 文档导航

| 任务 | 文档 |
| --- | --- |
| 安装并完成首次运行 | [AW 用户手册](../../docs/user-guide/zh/aw/index.md) |
| 选择 Agent、配置规则和更新策略 | [配置与启动](../../docs/user-guide/zh/aw/configuration.md) |
| 启用输出压缩或安全检查 | [Tokenless](../../docs/user-guide/zh/aw/tokenless.md)、[sec-core](../../docs/user-guide/zh/aw/security.md) |
| 查看运行记录 | [AgentSight](../../docs/user-guide/zh/aw/observability.md) |
| 查询状态、升级卸载与排错 | [运行维护](../../docs/user-guide/zh/aw/operations.md) |
| 编写 Provider、接入 SDK 或理解架构 | [开发指南](../../docs/developer-guide/zh/aw/index.md) |

<a id="代码架构"></a>
<a id="开发与验证"></a>

[架构总览](docs/design/zh/architecture.md)说明框架与 Provider 的职责和调用流程。
源码构建、测试与 CI 入口见[贡献指南](CONTRIBUTING_zh.md)。

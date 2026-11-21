# AW

[English](README.md)

AW 为 Agent 和 SDK 应用提供统一 Hook 编排、受管 MCP 接入和策略热更新。
通过一份配置在 CLI Agent、原生框架适配与自研 Python/Node.js 应用之间复用工具前安全检查、工具后输出压缩和自定义规则。
支持的常驻入口可在任务排空后更新策略，保留宿主进程。
Agent 继续管理自己的模型、界面和权限。AW 运行于 Linux。

<a id="安装与使用"></a>
<a id="安装与首次运行"></a>

## 5 分钟快速入门

按[安装指南](../../docs/user-guide/zh/aw/installation.md)安装 AW `0.2.0` core 及可选 Provider 扩展。
当前 AW 通过独立发布包交付，`anolisa install aw` 和 RPM 安装入口尚未提供。
先安装并登录所选 Agent，再用 `0.2.0` CLI 生成配置、校验并启动。

下面以已登录的 Qoder CLI 为例（`AW_PREFIX` 为 AW 安装前缀，`aw` 和 `qodercli` 需在 `PATH` 中）：

```bash
export AW_HOME="$HOME/aw"
install -d -m 700 "$AW_HOME" "$AW_HOME/workspace"
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$(command -v qodercli)"
aw config validate --config "$AW_HOME/aw.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

进入 Qoder 后，请它用工具读取当前目录并按原生提示处理权限确认。
Agent 退出后，本地服务仍保持可复用状态；结束使用时查看并停止服务：

```bash
aw service status --config "$AW_HOME/aw.yaml"
aw service stop --config "$AW_HOME/aw.yaml"
```

## 通用能力

| 能力 | 用户能做什么 |
| --- | --- |
| Hook 编排 | 按目标和工具选择规则，组合 `sec-core`、`Tokenless` 与自定义 Provider，统一步骤顺序、执行期限和审计。 |
| 受管 MCP | 在支持的实际工具调用边界执行同一套策略，检查调用身份及获准参数。 |
| 策略更新 | 通过 `aw update` 向支持的空闲实例显式应用新策略，让后续任务使用新版本。 |

Adapter 接通原生事件并采纳支持的效果，Provider 实现具体策略。
AW core 可独立使用；需要安全检查（`sec-core`）或输出压缩（`Tokenless`）时按需安装并启用对应 Provider。
工具结果替换仅限于获准位置，Agent 的原生权限检查始终保留。

## Hook 支持矩阵

✓ 已声明且有契约测试覆盖，× 尚不支持。矩阵从[回归清单](tests/regression-matrix.json)生成。结果替换限制、Skill 校验、MCP 与策略更新范围见[完整能力表](../../docs/user-guide/zh/aw/capabilities.md)，测试框架版本见[已验证基线](../../docs/developer-guide/zh/aw/native-agents.md)。

<!-- aw-regression-matrix:start -->
| Agent | `tool.before` / `observe` | `tool.before` / `block` | `tool.before` / `replace_input` | `tool.after` / `observe` | `tool.after` / `replace_result` | `input.submit` / `observe` | `input.submit` / `block` | `security.violation` / `observe` | `security.violation` / `block` |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| `agentscope` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `claude-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `codex` | ✓ | ✓ | × | ✓ | ✓ | ✓ | ✓ | × | × |
| `deepseek-harness` | ✓ | ✓ | × | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ |
| `hermes` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `openclaw` | ✓ | ✓ | × | ✓ | ✓ | × | × | × | × |
| `pi` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qoder` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qoder-sdk` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `qwenpaw` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | × | × |
| `sdk` | ✓ | ✓ | × | ✓ | × | ✓ | ✓ | × | × |
<!-- aw-regression-matrix:end -->

<a id="hook-支持"></a>
<a id="adapter-能力覆盖"></a>

## 双语言 Runtime SDK

AW `0.2.0` 提供 6 个独立 SDK 包（2 个核心包 + 4 个可选框架适配包），应用可保留原生启动方式、认证与任务调度：

| 语言 | 核心包 | 可选框架适配包 |
| --- | --- | --- |
| Python（`>=3.11`） | `anolisa-aw-sdk`（`aw_sdk`、`aw_sdk.sources`） | `anolisa-aw-sdk-claude`、`anolisa-aw-sdk-qwenpaw` |
| TypeScript / Node.js（`>=22`） | `@anolisa/aw-sdk`（`@anolisa/aw-sdk`、`@anolisa/aw-sdk/sources`） | `@anolisa/aw-sdk-qoder`、`@anolisa/aw-sdk-pi` |

- **自研 Python / Node.js Agent（零 Rust Adapter）**：只需安装核心包，通过 `LanguageSource`（`aw_sdk.sources` / `@anolisa/aw-sdk/sources`）声明式包装或插桩输入与工具方法。
- **常驻框架应用**：按需安装对应适配包，部署阶段用 `aw config descriptor` 编译绑定配置的启动提示，原生启动应用后以 `aw connect --config aw.yaml --sdk-pid PID` 完成协作挂接。

可运行示例与部署 Descriptor 流程见 [Runtime SDK 指南](../../docs/developer-guide/zh/aw/deployment/native-sdk.md)与 [Python SDK 指南](../../docs/developer-guide/zh/aw/deployment/python-sdk.md)。

## Provider 协议

外部策略、安全检查、输出压缩或状态观测／快照工具可通过 `aw-provider/v1alpha1` stdio JSON 协议接入 AW，无需链接 Rust 库或检出 ANOLISA 仓库：

- 通过 stdin/stdout 实现三个一次性 JSON 方法：`describe`、`validate_config` 与 `invoke`。
- 在 `aw.yaml` 中用 `providers.<id>.transport.argv` 直接指向已安装的可执行文件，或按可选本地扩展包格式安装。
- 一方参考实现位于 `providers/agent-sec-core` 与 `providers/tokenless`。

最小独立 Provider 骨架与 `aw.yaml` 挂接示例见 [Provider 开发与分发](../../docs/developer-guide/zh/aw/providers/index.md)，完整 Schema 与准入规则见 [Provider 协议](../../docs/developer-guide/zh/aw/providers/protocol.md)。

## 文档导航

| 任务 | 文档 |
| --- | --- |
| 安装并完成首次运行 | [快速入门](../../docs/user-guide/zh/aw/index.md) · [安装指南](../../docs/user-guide/zh/aw/installation.md) |
| 查询完整子命令与参数 | [CLI 命令参考](../../docs/user-guide/zh/aw/cli-reference.md) |
| 选择 Agent、配置规则和更新策略 | [配置与启动](../../docs/user-guide/zh/aw/configuration.md) |
| 对比 Hook、Skill、MCP 与策略更新支持 | [能力支持矩阵](../../docs/user-guide/zh/aw/capabilities.md) |
| 启用输出压缩或安全检查 | [Tokenless](../../docs/user-guide/zh/aw/tokenless.md) · [sec-core](../../docs/user-guide/zh/aw/security.md) |
| 查看运行与审计记录 | [AgentSight](../../docs/user-guide/zh/aw/observability.md) · [运行维护](../../docs/user-guide/zh/aw/operations.md) |
| 接入 SDK、独立实例或编写自定义 Provider | [开发者指南](../../docs/developer-guide/zh/aw/index.md) |

<a id="代码架构"></a>
<a id="开发与验证"></a>

[架构总览](docs/design/zh/architecture.md)说明框架与 Provider 的职责和调用流程。
本地验证命令、首批可领取贡献方向与 CI 门禁见[贡献指南](CONTRIBUTING_zh.md)。

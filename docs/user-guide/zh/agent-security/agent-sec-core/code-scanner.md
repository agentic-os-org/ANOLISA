# Code Scanner Hook 配置

Code Scanner hook 会在 shell 或代码工具执行前进行检查，并复用各 Agent 宿主现有的 hook 交互模式。环境变量只选择已有行为，不会为宿主插件新增此前不存在的审批或阻断返回。

## 安装

```bash
# 首选（需要 system mode）
sudo anolisa --install-mode system install sec-core

# 备选（已配置 YUM 源的 Alinux 系统）
sudo yum install agent-sec-core

# 源码构建（仅开发者）
cd src/agent-sec-core
make build-cli
```

按照 [AgentSecCore 快速开始](QUICKSTART.md) 安装或部署所用 Agent 的 adapter。

## V2 CLI 接口

在 V2 RPM 中，`agent-sec-cli scan-code` 是连接已运行 `agent-sec-daemon` 的 Rust client。
部署环境通过 `AGENT_SEC_DAEMON_SOCKET` 提供 daemon endpoint；绝对路径 `--socket`
优先于该变量。CLI 不会启动 daemon，也不会回退到 V1 Python 实现。

```bash
export AGENT_SEC_DAEMON_SOCKET=/run/agent-sec-core/daemon.sock

# 扫描 Bash（默认语言）或 Python 源码。
agent-sec-cli scan-code --code 'rm -rf /tmp/test'
agent-sec-cli --socket /run/agent-sec-core/daemon.sock \
  scan-code --code 'import os; os.system("rm -rf /")' --language python
```

V2 scanner 当前只支持内嵌的 regex 规则。`--mode llm` 为兼容性保留，但会返回可解析的
`LLM model not available` error result，不会连接 Ollama。V2 尚未支持
`--trace-context` 或写入 code-scan telemetry；依赖这些能力的 Agent hook 会在完成迁移前
保持延期状态。

## 环境变量

| Agent 插件 | `CODE_SCANNER_HOOK_ENABLED` | `CODE_SCANNER_MODE` | `CODE_SCANNER_TIMEOUT` |
|---|---|---|---|
| Qoder | `true` / `false` | `observe`、`ask`、`block` | 支持；默认 10 秒 |
| Qwen Code | `true` / `false` | `observe`、`ask`、`block` | 支持；默认 10 秒 |
| Codex | `true` / `false` | `observe`、`block` | 支持；默认 10 秒 |
| Cosh | `true` / `false` | 仅 `ask` | 不支持；固定 10 秒 |
| Hermes | `true` / `false` | `observe`、`block` | 不支持；使用 capability `timeout` |
| OpenClaw | `true` / `false` | `observe`、`ask`、`block` | 不支持；固定 10 秒 |

`CODE_SCANNER_HOOK_ENABLED=false` 会跳过 hook input 处理和 CLI 调用。在 Hermes 和 OpenClaw 中，合法布尔环境变量会覆盖 capability `enabled`；非法值等价于未设置，并回到 capability 配置。

`CODE_SCANNER_MODE` 控制插件如何处理带 findings 的 scanner `warn` 和 `deny` verdict：

- `observe` 执行扫描和审计，但放行工具调用。
- `ask` 使用宿主现有的审批交互。
- `block` 使用宿主现有的 deny 或 block 交互。

兼容别名会先完成归一化，再检查宿主能力：`debug` 映射为 `observe`，`deny` 映射为 `block`。`warn`、非法值以及宿主不支持的模式都等价于未设置；这些配置错配不会进入 stdout、systemMessage 或其他 HookOutput。独立脚本会向 stderr 记录 bounded diagnostic，Hermes/OpenClaw capability 会写宿主 logger。

因此，Cosh 收到 `observe` 或 `block` 时仍保持固定 `ask`；Codex 和 Hermes 忽略 `ask`；OpenClaw 支持 `observe`、`ask` 和 `block`，其中 `deny` 会归一化为 `block`。不受支持的模式会使用未设置 `CODE_SCANNER_MODE` 时相同的默认值或原生配置。

## 原生配置优先级

Hermes 保留 `[capabilities.code-scan]` 配置：

```toml
[capabilities.code-scan]
enabled = true
timeout = 10
enable_block = false
```

受支持的 `CODE_SCANNER_MODE` 优先于 `enable_block`；否则 `enable_block=true` 选择 block，`false` 选择 observe。

OpenClaw 保留 `capabilities["scan-code"].enabled` 和 `codeScanRequireApproval`。受支持的 `CODE_SCANNER_MODE` 优先于 `codeScanRequireApproval`；否则 `true` 选择 ask，`false` 选择 observe。在 `ask` 模式下，普通 findings 返回 `requireApproval`；在 `block` 模式下，普通 findings 返回 `{ block: true, blockReason }`。

## 示例

```bash
# Qoder 或 Qwen Code：请求审批
CODE_SCANNER_MODE=ask qoder
CODE_SCANNER_MODE=ask qwen

# Codex：阻断 scanner warn 和 deny findings
CODE_SCANNER_MODE=block codex

# 完全禁用 hook
CODE_SCANNER_HOOK_ENABLED=false codex
```

对于托管服务，将这些变量注入 Agent 进程环境并重启服务。不要为 Cosh、Hermes 或 OpenClaw 配置 `CODE_SCANNER_TIMEOUT`，这些 adapter 不消费该变量。

## 故障与安全语义

CLI 启动失败、超时、非零退出、非法 JSON 和未知 verdict 都保持 fail-open。非法或不支持的配置等价于未设置环境变量。

Hermes 和 OpenClaw 保留现有 self-protect findings；当工具调用尝试禁用安全插件时会强制 block。这是固定安全例外，不是额外的可配置 MODE。禁用整个 hook 后不会扫描，也不会执行 self-protect 检查。

## Hook MODE 与扫描引擎

`CODE_SCANNER_MODE` 控制宿主 hook 响应，不选择扫描引擎。V2 当前只运行内嵌 regex 规则；`--mode llm` 为兼容性保留，会返回 `LLM model not available`。

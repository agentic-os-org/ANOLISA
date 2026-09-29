# OpenClaw 兼容部署与升级指南

本文说明如何部署、升级、回滚和排查 AgentSecCore OpenClaw plugin。

## 适用范围

AgentSecCore OpenClaw plugin 的 OpenClaw host 兼容边界是 `>=2026.4.14`。
该边界在以下位置保持一致：

- `openclaw-plugin/package.json` 的 `openclaw.install.minHostVersion`
- `openclaw-plugin/package.json` 的 `openclaw.compat.pluginApi`
- `openclaw-plugin/package.json` 的 `peerDependencies.openclaw`

以下矩阵记录此前的 CI 验证：

| OpenClaw host | 验证结果 |
|---------------|----------|
| `2026.4.14` | 通过 |
| `2026.4.23` | 通过 |
| `2026.4.24` | 通过 |
| `2026.4.29` | 通过 |
| `2026.5.7` | 通过 |
| `2026.5.28` | 通过 |
| `2026.6.10` | 通过 |
| `latest` | 通过 |

验证证据：GitHub Actions `OpenClaw Plugin E2E` run `28774739252`。

本地 E2E 也已在 `2026.8.1` 和旧版本 `2026.4.23` 通过，环境为 Node
`22.22.3` 和已安装的 `agent-sec-cli 0.13.0`，覆盖安装、运行时加载、Gateway
流量、全部四个策略用例和 14 个直接 hook 探针。测试使用 mock 模型和确定性的
deny fixture；`2026.8.1` 的审批用例验证了无可用审批路由时阻止执行。

OpenClaw 2.0 对应版本 `2026.8.1`，要求 Node `>=22.22.3 <23`、
`>=24.15.0 <25` 或 `>=25.9.0`。插件安装记录位于宿主管理的 SQLite 状态中；
请使用 `plugins inspect` 校验安装，不要读取已停用的 `plugins.installs` 配置键。

## 前置条件

部署主机需要具备：

- `openclaw`
- `agent-sec-cli`
- `jq`
- 已构建的 OpenClaw plugin `dist/index.js`
- `openclaw-plugin/openclaw.plugin.json`

`deploy.sh` 会在执行前检查这些条件；缺失时会直接失败并打印原因。

## 从源码部署

在 `agent-sec-core` 仓库根目录执行：

```bash
make build-openclaw-plugin
```

该目标会构建 TypeScript，并把 `openclaw.plugin.json`、`package.json`、
`dist/` 和 `scripts/` 放入 `target/openclaw-plugin/`。

然后安装到目标目录。默认源码安装路径由 Makefile 变量
`OPENCLAW_PLUGIN_DIR` 控制，默认值是
`/usr/local/lib/anolisa/sec-core/openclaw-plugin`：

```bash
sudo make install-openclaw-plugin
```

最后运行 deploy 脚本注册插件：

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
```

如果要安装到 RPM profile 使用的路径，显式传入 `OPENCLAW_PLUGIN_DIR`：

```bash
sudo make install-openclaw-plugin OPENCLAW_PLUGIN_DIR=/opt/agent-sec/openclaw-plugin
sudo /opt/agent-sec/openclaw-plugin/scripts/deploy.sh \
    /opt/agent-sec/openclaw-plugin
```

源码开发环境也可以直接在插件目录内构建和部署：

```bash
cd openclaw-plugin
npm install
npm run build
./scripts/deploy.sh "$(pwd)"
```

如果 OpenClaw 使用非默认 state directory，部署时传入 `OPENCLAW_STATE_DIR`：

```bash
OPENCLAW_STATE_DIR=~/.openclaw-dev ./scripts/deploy.sh "$(pwd)"
```

## deploy.sh 做了什么

`deploy.sh` 负责安装期兼容处理：

- 读取 `openclaw --version`，要求 OpenClaw `>=2026.4.14`
- 读取 `openclaw plugins install --help`，确认支持 `--force`
- 支持时传入 `--accept-capabilities`，接受 agent-sec 声明的能力（OpenClaw 2.0 要求）
- 不支持能力确认的旧版本仅在 help 中提供时传入 `--dangerously-force-unsafe-install`；现代版本中该旧参数不再起作用
- 保留运维配置的 `security.installPolicy` 决策；接受能力不会绕过策略阻断
- OpenClaw `>=2026.4.24` 时写入 `plugins.entries.agent-sec.hooks.allowConversationAccess=true`
- OpenClaw `2026.4.14` 到 `2026.4.23` 跳过 `allowConversationAccess`
- 使用 `openclaw plugins inspect agent-sec --json` 校验安装记录
- 当当前 OpenClaw 支持 `plugins inspect --runtime` 时，使用 `openclaw plugins inspect agent-sec --runtime --json` 校验运行时加载
- 校验到插件状态不是 `loaded` 时失败

`deploy.sh` 不会启动、停止或重启 OpenClaw gateway。

## 重启 Gateway

部署或升级插件后，重启 OpenClaw gateway：

```bash
openclaw gateway restart
```

如果环境使用 systemd user service，使用对应服务重启命令：

```bash
systemctl --user restart openclaw-gateway-dev.service
```

## 验证安装

先验证 OpenClaw 安装记录：

```bash
openclaw plugins inspect agent-sec --json | jq -e '.plugin.id == "agent-sec"'
```

如果当前 OpenClaw 支持 runtime inspect，再验证运行时加载：

```bash
openclaw plugins inspect agent-sec --runtime --json | jq -e '.plugin.status == "loaded"'
```

不支持 `--runtime` 的 OpenClaw 版本使用普通 inspect 的 `plugin.status`：

```bash
openclaw plugins inspect agent-sec --json | jq -e '.plugin.status == "loaded"'
```

OpenClaw `>=2026.4.24` 部署后，还应确认配置中存在：

```bash
openclaw config get plugins.entries.agent-sec.hooks.allowConversationAccess
```

期望值为 `true`。

## 默认安全策略

默认配置是观察优先：

- `promptScanBlock=false`：prompt scanner 检测到 `deny` 时记录告警，但不阻断模型调用
- `codeScanRequireApproval=false`：code scanner 检测到风险时记录告警，但不弹审批
- `piiScanUserInput=true`：扫描用户输入中的 PII 和凭据
- `piiIncludeLowConfidence=false`：不包含低置信度 PII findings
- `pii-scan-user-input.enableBlock=false`：PII deny 默认不阻断
- `skill-ledger.policy=ask`：有用户可见消息时优先要求确认
- `observability.enabled=true`：启用观测记录

启用 prompt 阻断：

```bash
openclaw config set plugins.entries.agent-sec.config.promptScanBlock true
```

你也可以通过以下环境变量在部署层影响 prompt scanner 行为。
`PROMPT_SCANNER_HOOK_ENABLED=false` 无论插件配置如何都会跳过 hook 注册：

| 环境变量 | 默认值 | 行为 |
|----------|--------|------|
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | 设为 `false` 时完全跳过 prompt-scan hook 注册 |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | 传给 `scan-prompt` 的扫描强度：`fast` / `standard` / `strict` |

OpenClaw 插件不读取 `PROMPT_SCANNER_MODE` 和 `PROMPT_SCANNER_TIMEOUT`。OpenClaw 上的
prompt 策略由上方的 `promptScanBlock` 决定，scanner 超时固定为 10 秒。

修改这些变量后需重启 OpenClaw gateway。

启用代码扫描审批：

```bash
openclaw config set plugins.entries.agent-sec.config.codeScanRequireApproval true
```

如需部署级覆盖，`CODE_SCANNER_HOOK_ENABLED=true|false` 的优先级高于 `capabilities["scan-code"].enabled`，`CODE_SCANNER_MODE=observe|ask|block` 的优先级高于 `codeScanRequireApproval`。`debug` 是 `observe` 的别名，`deny` 是 `block` 的别名，`warn` 或非法值等价于未设置，并回到插件配置。在 `ask` 模式下，普通 findings 返回 `requireApproval`；在 `block` 模式下，普通 findings 返回 `{ block: true, blockReason }`。已有 self-protect 规则仍是不受模式影响的强制 block 例外。OpenClaw 不消费 `CODE_SCANNER_TIMEOUT`，继续使用固定 10 秒超时。

启用 PII deny 阻断：

```bash
openclaw config set 'plugins.entries.agent-sec.config.capabilities.pii-scan-user-input.enableBlock' true
```

配置 Skill Ledger 为直接阻断：

```bash
openclaw config set 'plugins.entries.agent-sec.config.capabilities.skill-ledger.policy' block
```

## 运行时兼容策略

不要为旧版 OpenClaw 写入持久化 hook disable 配置。

AgentSecCore 的策略是：

- `before_dispatch`、`before_tool_call`、`after_tool_call` 是支持矩阵内的核心安全 hook
- `model_call_started`、`model_call_ended` 属于可选模型调用观测 hook
- `llm_input`、`llm_output`、`agent_end` 需要 `allowConversationAccess`
- 旧版 OpenClaw 缺失可选观测 hook 时，插件允许观测能力降级
- 用户升级 OpenClaw 后，重新运行 `deploy.sh` 并重启 gateway，即可获得新版本支持的 hook 行为

如果把缺失 hook 写成持久化禁用配置，升级 OpenClaw 后这些 hook 可能仍被旧配置禁用，因此不要这么做。

## 升级流程

升级 AgentSecCore OpenClaw plugin：

```bash
make build-openclaw-plugin
sudo make install-openclaw-plugin
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
```

升级 OpenClaw host 后，也要重新运行 `deploy.sh`：

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
```

原因是新版 OpenClaw 可能支持旧版本没有的配置或 hook，例如
`plugins.entries.agent-sec.hooks.allowConversationAccess`。

## 回滚流程

如果新版本插件部署后需要回滚：

1. 将旧版本 plugin 目录恢复到目标路径。
2. 重新运行旧版本目录中的 `deploy.sh`。
3. 重启 OpenClaw gateway。
4. 使用 `openclaw plugins inspect agent-sec --json` 和 runtime inspect 校验状态。

示例：

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
openclaw plugins inspect agent-sec --json | jq -e '.plugin.id == "agent-sec"'
```

## 常见问题

### 插件安装失败

先确认命令可用：

```bash
openclaw --version
agent-sec-cli --help
jq --version
```

再确认 plugin 目录存在：

```bash
test -f /usr/local/lib/anolisa/sec-core/openclaw-plugin/openclaw.plugin.json
test -f /usr/local/lib/anolisa/sec-core/openclaw-plugin/dist/index.js
```

### runtime inspect 不是 loaded

执行：

```bash
openclaw plugins inspect agent-sec --runtime --json
```

查看 `diagnostics`。如果当前 OpenClaw 不支持 `--runtime`，使用：

```bash
openclaw plugins inspect agent-sec --json
```

### 会话观测 hook 被阻止

OpenClaw `>=2026.4.24` 需要：

```bash
openclaw config set plugins.entries.agent-sec.hooks.allowConversationAccess true
openclaw gateway restart
```

OpenClaw `2026.4.14` 到 `2026.4.23` 不支持该配置。核心安全 hook 仍应可用，但会话级观测 hook 会降级。

### 升级 OpenClaw 后观测能力没有变化

重新运行：

```bash
sudo /usr/local/lib/anolisa/sec-core/openclaw-plugin/scripts/deploy.sh \
    /usr/local/lib/anolisa/sec-core/openclaw-plugin
openclaw gateway restart
```

不要手动保留旧版本下写入的 hook disable 配置。

# AgentSecCore 内部命令

[English](../../../en/agent-security/agent-sec-core/internal-commands.md)

`--trace-context` 与 `log-sandbox` 是 hook 和插件使用的隐藏集成点，本文记录其审计与
调试契约。`hidden=True` 只控制帮助可见性，不代表功能是否可用。

## 入口清单

| 入口 | 角色 | 调用结果 |
| --- | --- | --- |
| `--trace-context JSON`（顶层选项） | 隐藏集成点 | 可用 |
| `log-sandbox` | 隐藏集成点 | 可用 |
| `skill-ledger rotate-keys` | 公开，尚未实现 | 明确报错，退出 `1` |

两个隐藏集成点都可显式查看 `--help`。

## `log-sandbox`

把一次沙箱前置决策记录为一条 Security Event。它是 Copilot Shell 沙箱防护的审计侧：
[`sandbox-guard` hook](../../user-entrypoint/copilot-shell/hooks.md) 决定如何处理一条
危险 shell 命令后，spawn `log-sandbox` 把该决策落到本地事件库。

```bash
agent-sec-cli log-sandbox \
  --decision sandbox \
  --command 'rm -rf /tmp/test' \
  --reasons 'recursive-delete' \
  --network-policy restricted \
  --cwd /home/user/project
```

### 参数

所有参数都是自由字符串，默认为空。不做拒绝也不做归一化——传什么就记什么。

| 参数 | 记录含义 |
|------|---------|
| `--decision` | 前置决策结论。`sandbox-guard` 只会产出 `block` 或 `sandbox` |
| `--command` | 被评估的 shell 命令 |
| `--reasons` | 决策理由，取自调用方的规则标签 |
| `--network-policy` | 沙箱执行的网络策略：`restricted` 或 `enabled`。`block` 路径不传此参数，因此落库为空字符串 |
| `--cwd` | 该命令即将执行的工作目录 |

以上是 `sandbox-guard` 实际产出的取值，而不是 CLI 强制约束的取值。CLI 不校验任何
取值范围，非预期取值会被原样存入，因此拼写错误不会报错，而是产生一条标签错误却
看不出来的审计记录。若要按这些字段过滤事件，请按 hook 实际产出的值过滤——不存在
`allow` 记录，也不存在 `unrestricted` 策略。

### 输出与退出码

该命令按设计静默：成功时无 stdout、无 stderr——因为调用方以 detached 方式 spawn 它，
从不读取其输出。

| 退出码 | 条件 |
|--------|------|
| `0` | 仅做记录的 backend 执行完毕。这是正常结果，但它*不*代表事件已落盘 |
| 非 0 | 中间件层抛出了未预期的内部故障 |

事件写入本身是 best-effort：JSONL 或 SQLite writer 任一失败都会被吞掉，退出码仍为
`0`。绝不要把退出码 `0` 当作记录已存在的证据——请按[校验记录是否落库](#校验记录是否落库)
查询事件库来确认。再加上 `sandbox-guard` 以 detached 方式 spawn 且丢弃输出，退出码对
沙箱执行没有任何影响——丢一条审计记录既不会拦住命令，也不会放开命令。

### 它不做什么

`log-sandbox` 与 `linux-sandbox` 很容易混淆，而其中只有一个真正做隔离。

| | `linux-sandbox` | `agent-sec-cli log-sandbox` |
|---|---|---|
| 形态 | 位于 `/usr/local/bin/linux-sandbox` 的独立二进制 | `agent-sec-cli` 的隐藏子命令 |
| 职责 | 真正在文件系统与网络隔离下执行命令 | 记录「做过一次决策」这件事 |
| 对命令的影响 | 包装并执行它 | 无——从不执行、拦截或改写任何东西 |
| `sandbox-guard` 如何使用 | 改写工具调用，使其经由它执行 | detached spawn，fire-and-forget |

因此一条 `--decision block` 记录并不拦截任何东西。拦截早已在 hook 中完成，
`log-sandbox` 只是让它可审计。

### 校验记录是否落库

沙箱决策以 event type `sandbox_prehook`、category `sandbox` 存储：

```bash
agent-sec-cli events --category sandbox --last-hours 1
agent-sec-cli events --event-type sandbox_prehook --output json --limit 5
```

在 JSON 输出中，`details.request` 保存传入的五个参数，决策位于
`details.result.decision`。事件在 `/var/log/agent-sec/` 可写时落在该目录，否则落到
`~/.agent-sec-core/`；`AGENT_SEC_DATA_DIR` 可覆盖两者。

## `--trace-context`

顶层选项，让调用方插件把自己的关联 ID 附加到本次调用产生的每条 Security Event 上，
从而把安全记录与宿主 Agent 的 trace 关联起来。

```bash
agent-sec-cli --trace-context '{"trace_id":"t-1","session_id":"s-1"}' \
  log-sandbox --decision block --command 'rm -rf /'
```

取值是一个 JSON 对象。可识别字段为 `trace_id`、`session_id`、`run_id`、`call_id`、
`tool_call_id`、`agent_name`，每个字段同时接受 camelCase 写法（`traceId`、
`sessionId`……）。未知字段被忽略；超过 256 字符的值会被截断并追加
`...[truncated]` 标记。

前五个作为关联字段落到 Security Event 上。`agent_name` 是例外：它以
可观测元数据（`component.agent_name`）的形式传递，不存入事件本体，因此不要指望
用它过滤事件。

它必须出现在子命令之前——这是进程级选项，在命令之前解析，以保证子命令自身的 flag
保持原有语义。JSON 格式错误会显式失败：CLI 向 stderr 打印
`Error: invalid trace context JSON` 并以 `1` 退出，不执行子命令。空值等同于未提供。

## `skill-ledger rotate-keys`

普通 help 中可见，并注明尚未实现。调用它会失败，且不触碰任何密钥材料。

| | 行为 |
|---|------|
| stdout | 空 |
| stderr | `Error: rotate-keys is not implemented; no keys were changed.` |
| 退出码 | `1` |
| 密钥存储 | `key.enc`、`key.pub` 与 keyring 保持不变 |

`rotate-keys --help` 以 `0` 退出，并说明该命令尚未实现。

`init --force-keys` 强制生成新密钥对，并归档旧公钥供历史签名校验；它不实现
`rotate-keys` 所预留的完整轮转流程。

# Policy CLI 使用指南

[English](../../../en/agent-security/agent-sec-core/policy-cli.md)

使用 `agent-sec-cli` 经 `asc-daemon` 管理带 revision 的 Policy Template 和不可变的
Scope 分配。Scope 保存所选 Policy revision 的完整内容；daemon 发现匹配的进程实例，
自动创建 Binding 并协调下发。Binding 仅支持查询。

这些命令由 V2 CLI 提供，已发布的 Python CLI 尚未包含。Policy、Scope 和 Binding 状态保存在 SQLite 中，daemon 重启后恢复；
Scope 受理不代表保护已生效。CLI 和 daemon 需一起升级：Scope update、Scope revision
参数和手动 Binding mutation 已移除。

## 连接 daemon

使用部署管理员提供的绝对 socket 路径。以下示例假设 `SOCKET` 已设置为该路径，
且当前用户已获得策略管理权限。未授权用户会收到 `permission_denied`；CLI 不负责
启动 daemon 或授予权限。

```bash
agent-sec-cli --socket "$SOCKET" policy list
```

## 关联本地日志

V2 使用原生 OpenTelemetry 关联本地请求日志。`--trace-context` 保留原有扁平 Agent
metadata 输入，应放在命令名及其它选项的非选项值之前。可选的 `--otel-context` 接收
version 1 JSON carrier，包含 `traceparent`、`tracestate`、`baggage` 可选字段。
两者同时提供时，显式扁平 Agent 字段优先。

```bash
RUST_LOG=info agent-sec-cli --trace-context '{"session_id":"session-123","agent_name":"openclaw"}' \
  --otel-context '{"version":1,"traceparent":"00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"}' \
  --socket "$SOCKET" policy list
```

`RUST_LOG=info` 开启 stderr 上的有界 JSON 关联诊断；daemon 需要单独设置其进程环境。
默认 warn 不产生这些记录。背压下允许丢诊断；命令结果、错误及退出码保持原有语义。
当前不提供公开 OTLP exporter 或导出/采样/batch 配置；`OTEL_*` 设置不能开启导出或
改变固定的本地采样策略。`--otel-context` 是入站上下文，不是导出目标。
CLI 要求支持 carrier 的新 daemon，应先升级 daemon。

## 管理 Policy

准备 JSON 模板文件，例如 `policy.json`：

```json
{
  "specVersion": "0.1",
  "rules": [
    {
      "effect": "block",
      "category": "file",
      "action": "write",
      "target": {"type": "file", "path": "/workspace/important/**"},
      "where": {"operation": {"eq": "delete"}},
      "because": "Protect important files from deletion"
    }
  ]
}
```

PolicyTemplate 是可被多个 Scope 按 ID/revision 选用的可复用策略。`rules` 表达动作、
带类型的目标、决策、条件和可选理由。当前资源格式为 `{"type":"file","path":"..."}`，
每条规则一个路径。合法的文件 read/write/exec 规则、逻辑条件、历史条件以及
block/allow/require_confirmation 决策均可保存；Network 和 AgentHook 的目标格式尚未定义，
目前不能保存。

AgentSight Adapter 当前只执行不带历史条件的 block + file/write + operation=delete 规则。
任意规则不支持，整个 Binding 失败，例如返回有界错误码 `RULE_1_UNSUPPORTED_EFFECT`，
不发送受支持的规则子集。目标 DSL 不支持转义语法，because 包含引号、反斜杠或控制字符时
转换失败，普通 Unicode 文本保持原样；省略理由时使用 Adapter 默认值。
创建策略成功不代表执行后端支持这些规则。

生成的 DSL 表达禁止删除；真实内核是否仅阻止删除仍需单独验证，当前 ActPlane 的
unlink/write 共用底层操作映射。旧 kind JSON 和包含该格式的开发期数据库不提供迁移，
也不自动重建；已有部署应先用兼容的旧二进制完成清理，再安排新库。

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "protect files" --file policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "protect files v2" --file policy-v2.json
agent-sec-cli --socket "$SOCKET" policy delete --policy-id "$POLICY_ID" --revision 2
```

ID 和 revision 使用成功命令返回的值。这些是参考示例，并非顺序执行脚本；应在目标
Policy revision 仍为 current 时创建 Scope。create/update 均要求名称和完整模板，
不是部分更新。名称或模板变化才增加 revision，相同内容不增版。只保留 current 完整
记录；get/delete 要求精确的当前 revision，旧版本返回 `not_found`。文件相对路径按
CLI 工作目录解析。

更新或删除 Policy 不改变已有 Scope 和 Binding。撤销分配应删除 Scope。已删除的已知
Policy ID 保留 revision 计数器；update 可按下一 revision 重建内容，未知 ID 不可 update。

## 管理 Scope

创建时提供 Policy ID、精确 revision 和一个 selector：`--process-name` 精确匹配 Linux
进程名（1–15 bytes），`--executable` 精确匹配规范化绝对可执行路径，或 `--pid` 选择
一个进程实例。名称/路径持续发现已有和后续进程；PID 固定第一次观察到的实例，不跟随
PID 复用。目前拒绝 cgroup assignment。

```bash
agent-sec-cli --socket "$SOCKET" scope create --process-name openclaw --policy-id "$POLICY_ID" --policy-revision 1
agent-sec-cli --socket "$SOCKET" scope get --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" scope delete --scope-id "$SCOPE_ID"
agent-sec-cli --socket "$SOCKET" scope retry --scope-id "$SCOPE_ID"
```

Scope 没有 revision，不可 update；`policySnapshots` 保存所选 Policy 的完整内容。
若并发 Policy 更新/删除先于 Scope 准入提交，创建返回 `not_found`，不会静默替换版本。
准入成功后，后续匹配的新实例也使用该快照。CLI 每次分配一个 Policy，RPC 支持每个
Scope 1–32 个不同 Policy。

替换分配时，先创建新 Scope，查询所需实例的 Binding 达到 `READY` 后再删除旧 Scope；
这不是原子切换。

删除先关闭发现准入并停止 worker，再异步清理所属部署。未完成时返回
`{"scopeId":"...","completed":false}`，完成后返回 `completed:true`。
清理期间 Scope 保留 `DELETING` 状态，失败 Binding 仍可查询。重复 delete 不重置重试
预算；排除故障后使用 `scope retry` 重试终态失败，不修改分配或重启正在运行的工作。
已完成的删除及未知 Scope ID 的删除均返回 `completed:true`，daemon 重启后也可重复。
其它 Scope 和来源 Policy 不受影响。

## 查询 Binding

daemon 为每个 Scope、Policy 和具体进程实例创建一条 Binding，包含策略快照、Scope ID
和 selector、固定的进程身份及当前状态。通过 `spec.scope.scopeId` 识别所属分配。

```bash
agent-sec-cli --socket "$SOCKET" binding list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" binding get --binding-id "$BINDING_ID"
```

应用从 `PENDING_APPLY` 经 `APPLYING` 到 `READY`。永久或预算耗尽的失败显示为
`APPLY_FAILED`/`DELETE_FAILED`，原因在 `status.error`；查询命令仍成功退出。
进程退出或 selector 失配与 Scope 删除使用同一清理路径。部署结果不确定时，责任保留到
确认目标不存在。自动重试有界，`scope retry` 重试所属失败工作。目前没有 `--wait`。

## 公共参数

| 参数 | 用途 |
|------|------|
| `--socket PATH` | 必填的 daemon 绝对 socket 路径，可放在子命令前后 |
| `--timeout-ms N` | 正 32 位无符号整数，默认 `5000`；连接、发送、接收共用一次时间预算 |
| `--limit N` | 列表每页条数，`1..=1000`，默认 `100` |
| `--offset N` | 列表偏移量，32 位无符号整数，默认 `0` |
| `--help` | 顶层、命令组和具体操作的帮助，不连接 daemon |
| `--version` | 顶层版本信息，不连接 daemon |

支持 `--key value` 和 `--key=value`。带空格的名称、路径需使用引号；
以 `--` 开头的值使用 `--key=--value`。重复参数会被拒绝。
列表命令每次返回一页 `{items,total}`，`total` 是分页前总数。后续页需显式请求，
offset 按实际返回条数推进；达到 3 MiB 编码条目预算时可能提前结束一页。

## 输出与错误

| 结果 | 输出 | 退出码 |
|------|------|--------|
| 成功 | stdout 输出格式化的结果 JSON，stderr 为空 | `0` |
| daemon 拒绝 | stderr 输出 JSON `{requestId,error:{code,message}}`，stdout 为空 | `1` |
| 文件、连接、响应或输出失败 | stderr 输出错误说明 | `1` |
| 命令行参数错误 | stderr 输出用法错误 | `2` |

模板文件最大为 4 MiB，必须是有效 JSON，不能包含未知或重复字段。
完整编码后的请求和响应各自另有 4 MiB 上限，包含行结束符。文件大小检查通过不保证
组装后的请求符合上限；请求超限时不会发送。daemon 另在保存前拒绝编码后超过
1 MiB 的 Policy 或完整 Scope。

超时预算覆盖 daemon 通信，包括等待 daemon 处理请求；不包含文件读取、请求编码、
响应解码和结果输出。CLI 不自动重试。发送后超时、响应缺失或非法，不代表请求没有
执行；再次提交变更前应先查询当前状态。

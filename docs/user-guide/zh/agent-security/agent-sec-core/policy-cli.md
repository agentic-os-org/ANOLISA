# Policy CLI 使用指南

[English](../../../en/agent-security/agent-sec-core/policy-cli.md)

使用 `agent-sec-cli` 经 `asc-daemon` 创建、查询、更新和删除 Policy、Scope、Binding。
Policy 描述保护策略，Scope 选择目标，Binding 关联指定版本的 Policy 和 Scope。

这些命令由 V2 CLI 提供，已发布的 Python CLI 尚未包含。当前策略记录在 daemon
重启后丢失；Binding 请求被受理不代表保护已经生效。

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
{"kind":"prevent_file_deletion","files":["/workspace/important/**"]}
```

当前支持的模板保护文件或目录项免于删除（`unlink`/`rmdir`），不覆盖重命名、移动或
文件内容修改。

```bash
agent-sec-cli --socket "$SOCKET" policy create --name "protect files" --file policy.json
agent-sec-cli --socket "$SOCKET" policy get --policy-id "$POLICY_ID" --revision 1
agent-sec-cli --socket "$SOCKET" policy list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" policy update --policy-id "$POLICY_ID" --name "protect files v2" --file policy-v2.json
agent-sec-cli --socket "$SOCKET" policy delete --policy-id "$POLICY_ID" --revision 2
```

ID 和 revision 使用成功命令返回的值。这些是命令参考示例，并非顺序执行脚本；
创建 Binding 时应保留其引用的 Policy 和 Scope。
create 和 update 均要求名称及完整模板。update 替换已有 Policy，不是部分更新，
也不会创建不存在的 Policy。文件相对路径按 CLI 工作目录解析。
get/delete 要求精确的当前 revision，旧 revision 返回 `not_found`。

## 管理 Scope

`--pid`（正进程 ID）和 `--cgroup-id`（正 cgroup ID）必须且只能指定一个。

```bash
agent-sec-cli --socket "$SOCKET" scope create --pid 4242
agent-sec-cli --socket "$SOCKET" scope get --scope-id "$SCOPE_ID" --revision 1
agent-sec-cli --socket "$SOCKET" scope list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" scope update --scope-id "$SCOPE_ID" --cgroup-id 99
agent-sec-cli --socket "$SOCKET" scope delete --scope-id "$SCOPE_ID" --revision 2
```

update 替换完整 selector；get/delete 要求精确的当前 revision。
进程 ID 和 revision 为正 32 位无符号整数，cgroup ID 为正 64 位无符号整数。

## 管理 Binding

Binding 引用已经存在的 Policy revision 和 Scope revision。所有 create 命令均由
服务端生成 ID。Binding get/delete 只需要 Binding ID，无需 revision。

```bash
agent-sec-cli --socket "$SOCKET" binding create --policy-id "$POLICY_ID" --policy-revision 1 --scope-id "$SCOPE_ID" --scope-revision 1
agent-sec-cli --socket "$SOCKET" binding get --binding-id "$BINDING_ID"
agent-sec-cli --socket "$SOCKET" binding list --limit 100 --offset 0
agent-sec-cli --socket "$SOCKET" binding update --binding-id "$BINDING_ID" --policy-id "$POLICY_ID" --policy-revision 2 --scope-id "$SCOPE_ID" --scope-revision 2
agent-sec-cli --socket "$SOCKET" binding delete --binding-id "$BINDING_ID"
```

create/update 通常返回 `PENDING_APPLY`，delete 请求 `PENDING_DELETE`。
重复或没有实际变化的操作可能保留原有状态和 revision。这些状态表示已记录变更请求，
不代表保护生效或删除完成。当前尚不支持自动应用策略或 `--wait`。

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
列表命令每次返回一页 `{items,total}`，`total` 是分页前总数；后续页需显式请求。

## 输出与错误

| 结果 | 输出 | 退出码 |
|------|------|--------|
| 成功 | stdout 输出格式化的结果 JSON，stderr 为空 | `0` |
| daemon 拒绝 | stderr 输出 JSON `{requestId,error:{code,message}}`，stdout 为空 | `1` |
| 文件、连接、响应或输出失败 | stderr 输出错误说明 | `1` |
| 命令行参数错误 | stderr 输出用法错误 | `2` |

模板文件最大为 4 MiB，必须是有效 JSON，不能包含未知或重复字段。
完整编码后的请求和响应各自另有 4 MiB 上限，包含行结束符。文件大小检查通过不保证
组装后的请求符合上限；请求超限时不会发送。

超时预算覆盖 daemon 通信，包括等待 daemon 处理请求；不包含文件读取、请求编码、
响应解码和结果输出。CLI 不自动重试。发送后超时、响应缺失或非法，不代表请求没有
执行；再次提交变更前应先查询当前状态。

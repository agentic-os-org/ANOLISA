# CLI 自动化参考

[English](../../../en/aw/reference/cli.md)

完整子命令与参数枚举见用户手册的 [CLI 命令参考](../../../../user-guide/zh/aw/cli-reference.md)。
本页说明脚本与自动化使用的输出约定。

用 `aw commands --json` 获取命令和参数清单，用具体命令的 `--help` 查询条件约束。

## 输出选择

| 入口 | 输出 |
| --- | --- |
| `install`、`uninstall`、`list --installed`、`doctor`、`recover`、`config init/validate/add-*/remove-*` | 可选局部 `--json` |
| `service status/stop`、连接运行中应用的 `connect`、`disconnect`、`instance`、`request`、`config show`、`update` | 固定 JSON，不另加 `--json` |
| 仅指定 `--agent TARGET` 的 `connect` | 持久 Hook 安装以退出码报告结果；Adapter 可输出后续设置提示。 |
| `inspect` | 必须传 `--json` |
| `run` | 保留原生 Agent 输出；`--` 后参数原样传递 |

管理命令的人类输出支持 `--quiet`、`--verbose` 和 `--color auto|always|never`。
quiet 与 verbose 互斥，三者均与 JSON 冲突。quiet 保留错误和清理警告。

## 管理报告

```bash
aw config validate --config "$HOME/aw/aw.yaml" --json
aw doctor --prefix "$AW_PREFIX" --json
```

```json
{"format":1,"command":"config remove-hook","status":"succeeded","result":{"kind":"configuration_edited","changed":true}}
```

| 字段或状态 | 消费方式 |
| --- | --- |
| `format: 1` | 报告版本 |
| `status: succeeded` | 退出 0，读取有类型的 `result` |
| `failed` / `cancelled` / `partial` | 退出 1，读取 `error.code`；partial 表示恢复未完成 |
| `mutation.committed` | 包文件及记录是否已跨过持久提交点 |
| `mutation.recovery` | `not_attempted`、`complete` 或 `incomplete` |
| `warnings` | 检查资源路径并完成剩余清理；已提交卸载可能带警告退出 0 |
| `changed: false` | 编辑未改变配置字节 |
| `policy_update_scope: true` | 候选通过离线修改范围检查；随后仍须执行 `aw update` |

语法错误在 stderr 输出文本，stdout 为空，退出 1。I/O 失败可能发生在文件发布之后，重试前先查询状态。
`config show` 返回完整私有配置；管理报告的固定错误摘要不适用于原生输出或协议流。

## 卸载与恢复

脚本使用 `aw uninstall --prefix ABS_PREFIX --yes`，system scope 同时需要 `sudo` 和 `--scope system`。
交互卸载只接受 `y` 或 `yes`；JSON 和无终端调用必须提供 `--yes`。`--dry-run` 只预览。
包错误 `needs_recovery`、`package_in_use`、`plan_blocked` 及恢复步骤见[包管理](packages.md)。

## 命令发现

`aw commands --json` 返回 `format`、`name`、`version` 和 `commands`，每项列出命令路径和可见参数。
`required` 仅标记无条件必需参数。补全使用同一声明，支持 bash、zsh、fish、powershell、elvish。
帮助和发现命令离线运行。`aw -V` 报告 CLI crate 版本。

## 独立实例 JSON

`aw instance` 使用 `aw-instance/v1` 信封，区分 `result.state`、操作错误和准备审计键。完整字段、确认期限与重复请求规则见[独立实例接入](../deployment/independent-instances.md)。

## 公共审计导出

`aw audit-export` 从已有的同用户服务状态目录读取一页，服务可以已经停止。
使用服务配置的状态路径，勿另建空目录替代。Journal 目录须属于有效用户且权限为 0700，
源描述符和链文件权限为 0600。请求文件须为普通文件，最多 16 KiB。
此命令不接受控制操作。

```bash
AW_STATE=/absolute/private/state
AW_QUERY="$HOME/aw-audit-query.json"
cat > "$AW_QUERY" <<'JSON'
{"api_version":"audit-export.aw/v1alpha1","operation":{"type":"enumerate","cursor":null,"limit":64}}
JSON
aw audit-export --state-dir "$AW_STATE" --request "$AW_QUERY"
```

成功响应包含 `error: null`、持久 `source_id` 与 `result.keys`。
将 `result.next_cursor` 放入下次枚举请求，直到它为 null。
`result.changed` 报告键集合变化，续页仍向后推进。游标保留首轮的 `through` 上限；
下一轮收集更晚及新插入的前部键。保留单链进度，并限制消费者循环。
对每个键，以 `{"type":"read","chain_id":"KEY","cursor":null,"limit":64}`
作为 `operation`，保留同样的 `api_version` 外层。用返回的键替换 `KEY`。
后续单链请求使用响应中的 `result.cursor`。

单链响应包含 ExecutionEvent、公共 AuditRecord、捕获的链尾、`caught_up`、`terminal`
及固定限制。页面按源位置计数，因此没有公共记录的页面也可能推进游标。
仅 `caught_up && terminal` 表示该链读完；开放链须重访，键枚举须定期从头开始。
记录和游标一起原子保存，以生产者身份和 UID 去重，重放时比较对象摘要。
历史生产者代际保持不变，响应不包含私有 Journal 记录。Schema、界限、损坏及历史缺失处理
见[导出合同](../../../../../src/aw/docs/design/zh/runtime/audit-export.md)。

协议失败返回 `result: null`、稳定 `error` 错误码，退出码为 1。
命令语法及请求文件 I/O 错误使用 stderr。取消没有写入副作用。
单次命令退出后无需停止资源；请求文件不再需要时删除 `"$AW_QUERY"`。

`aw instance list`、`aw connect --sdk-instance ID`、`aw disconnect --sdk-instance ID`
操作应用预装的 SDK 监听。`aw connect --sdk-pid PID` 按固定的进程身份选择一个监听；
同一进程有多个监听时使用实例 ID。配合 `--config` 与 `daemon.startup: on_demand`，
该 PID 选择器按需启动或复用 AW；指定 `--socket` 时要求服务已启动。
应用保持原生启动和进程归属。原有 `--pid` 选择器保留原生热加载限制。
在 JSON 中查看应用确认。
参见[协作式 SDK 控制](../deployment/native-sdk.md#协作式-attach-与-detach)。

## 普通 RPC 完成事件审计

同用户集成可以通过 `aw request` 上报工具完成事件。`open_event` 在有文本投影时
使用已有的 `event.tool.result_projection.status`；结果没有文本投影时，可使用操作层
的可选字段 `result_status`。两者均接受 `success`、`error`、`interrupted`、`denied`。
同时提供时必须一致，并须与任何可识别的原生完成状态一致。`result_status` 仅适用于
`tool.after`。服务在确认这类完成事件前校验事件结构及公共工具调用身份。

Qoder、Qoder SDK 和 Claude SDK 事件在 native 中声明
`hook_event_name: PostToolUse` 时，也可通过布尔值 `tool.result.isError: false`
或 `tool.result.is_error: false` 表达成功，无需在 `native.tool_response` 中重复结果。
显式失败或中断具有优先级。字段缺失、字符串 `"false"` 和任意业务字段均不能证明成功。
该原生格式的映射同时适用于普通 RPC 与原生回调。

这些状态属于调用者报告的事实，普通 RPC 的关联仍为 unverified。服务在打开事件时
保存 `resultStatus` 和原始 `tool.result` 的摘要。Provider 成功或替换文本不会改变
工具状态或结果摘要。`tool.result` 保留任意 JSON，包括 null；其中的 `status` 等
业务字段不作为完成状态声明。操作层的显式字段不转发给 Provider。

CLI 和 daemon 均需包含此 RPC 完成事件支持，旧 daemon 会拒绝新增操作字段。
JSON 调用方可省略该字段。Rust 调用方构造 `aw_service::Operation::OpenEvent` 时，
设为 `result_status: None` 即可省略它。省略该字段时，其他类型的事件沿用原有投影行为。
缺少可识别原生状态和适用的结果投影时，仍可能返回 `projection_unavailable`。
已有历史 gap 链保持原状。

集成验证前，启动私有测试服务，配置目标及 `tool.after` Provider 步骤，例如
[Provider 接入指南](../providers/tokenless.md)中的 Tokenless `post_tool`。
以服务所属用户在任意目录执行，PATH 中须有 `aw`，并已安装 Python 3。
将下列三个变量设为该服务的状态目录、目标名和适配器。示例上报合成完成事件来验证
审计接口，不启动 Agent，也不执行事件所描述的命令。

```bash
export AW_STATE=/absolute/private/state
export AW_TARGET=openclaw
export AW_ADAPTER=openclaw
python3 - <<'PYTHON'
import json
import os
from pathlib import Path
import subprocess
import tempfile
import uuid

state = Path(os.environ["AW_STATE"])
target = os.environ["AW_TARGET"]
adapter = os.environ["AW_ADAPTER"]

def rpc(operation):
    reply = subprocess.run(
        ["aw", "request", "--socket", str(state / "aw.sock")],
        input=json.dumps(operation), text=True, capture_output=True,
        check=True, timeout=10,
    )
    return json.loads(reply.stdout)["result"]

binding = rpc({
    "method": "bind", "target": target, "cwd": str(Path.cwd()), "environment": {},
    "capabilities": {
        "adapter": adapter, "version": "audit-example", "entrypoint": "manual-rpc",
        "events": {"tool.before": ["observe", "block"],
                   "tool.after": ["observe", "replace_result"]},
    },
})
instance = binding["instance_id"]
try:
    handle = rpc({
        "method": "open_event", "instance_id": instance, "result_status": "success",
        "event": {
            "name": "tool.after",
            "agent": {"adapter": adapter, "binding_id": target, "instance_id": instance},
            "session_id": "audit-example",
            "tool": {
                "name": "exec", "native_name": "exec", "call_id": str(uuid.uuid4()),
                "input": {"command": "printf AW_AUDIT_EXAMPLE"},
                "result": {"stdout": "AW_AUDIT_EXAMPLE", "exitCode": 0},
                "result_projection": {"text": "AW_AUDIT_EXAMPLE", "status": "success",
                    "content_origin": "command_output", "command": "printf AW_AUDIT_EXAMPLE"},
            },
            "native": {},
        },
    })
    try:
        for step in handle["steps"]:
            print(json.dumps(rpc({"method": "invoke_step", "instance_id": instance,
                "event_id": handle["event_id"], "step_id": step})))
    finally:
        rpc({"method": "close_event", "instance_id": instance, "event_id": handle["event_id"]})
    with tempfile.TemporaryDirectory() as directory:
        request = Path(directory) / "read.json"
        request.write_text(json.dumps({"api_version": "audit-export.aw/v1alpha1",
            "operation": {"type": "read", "chain_id": handle["event_id"],
                          "cursor": None, "limit": 64}}))
        subprocess.run(["aw", "audit-export", "--state-dir", str(state),
                        "--request", str(request)], check=True, timeout=10)
finally:
    rpc({"method": "release_instance", "instance_id": instance})
PYTHON
```

读取结果应包含 `error: null`、带 `details.resultStatus: success` 和
`details.resultDigest` 的 `ExecutionEvent`，以及每个已调用 Provider 的
`provider.completed` 记录。以 `tokenless` 为 Provider ID 配置的 Tokenless 成功调用
应包含 `providerId: tokenless`、`status: ok`。Provider 失败时仍可读取 `status: failed`，
它与工具完成状态分别记录。若 `caught_up` 为 false，使用 `result.cursor` 继续读取。
手动关闭事件不证明原生效果采用，`event.closed` 保持已有的 `incomplete` 结果。
示例会释放绑定并删除临时请求文件，原有服务继续运行。

非法枚举值在请求解码时被拒绝。`result_status_conflict` 表示声明、投影或可识别的
原生状态冲突；`result_status_requires_tool_after` 表示声明用于其他事件类型。
`event_invalid` 表示归一化事件或投影不合法；`public_event_invalid` 表示公共身份
元数据缺失或不合法，例如工具调用 ID 为 null。这些拒绝不会创建事件 journal 链，
也不会执行 Provider。修正上报元数据后再打开新事件。

# 连接业务自管实例

[English](../../../en/aw/deployment/independent-instances.md)

`aw instance` 让应用保留原生启动命令，由 AW 负责策略准备、原生就绪与资源清理。
应用保留进程启停权。Adapter 接通受支持的原生入口并确认回调后，实例才进入 Ready。

## 准备原生启动

服务、原生启动和后续更新共用一份 `aw.yaml`。配置选择已有可执行文件和原生参数；
准备操作不会把任意运行中进程自动变成受管实例。

| 应用 | 首次接入 | 运行时归属 |
| --- | --- | --- |
| Qoder SDK、AgentScope、Pi 和 Claude SDK | 应用直接启动 `aw instance prepare --agent TARGET` 子进程，设置返回的环境，在同一进程进入文档规定的 AW 工厂 | 原生任务或 Session 使用已有子 Scope 合同 |
| QwenPaw App | `aw instance setup` 在专用 venv 安装启动组件，随后重启一次原生 App | 既有 resident 插件与 reply Scope |
| OpenClaw Gateway 和 Hermes 本机 chat | `aw instance setup` 安装启动接点，随后重启一次原生命令 | 根实例的原生桥接覆盖完整 run/轮次及回调清理 |
| Qoder CLI | `aw instance setup` 安装后续 TUI 或 print 启动所用的原生 Hook；已有 TUI 可[在首请求前 attach](process-attach.md) | 每个进程在 `SessionStart` 登记自己的根桥接 |

框架启动代码、设置和参数范围见 [Agent 指南](../native-agents.md)。`setup` 仅写入返回
清单中的文件并提供启动环境，不创建运行中实例。`prepare` 完成 Adapter 校验与登记，
响应包含 `instance_id`、环境变化 `environment`、私有 `binding`、准备后的 `command`
和 `state: pending_ready`。SDK 应用或启动组件使用这些资产，在原生就绪接点确认原 owner
进程。不要将返回的 command 作为无关替代进程另行启动。

就绪后暂停新任务派发，修改 `aw.yaml` 中的策略字段，再执行：

```bash
aw instance update --agent TARGET
aw status --config aw.yaml
```

后续从同一配置路径准备实例时，复用已选定 daemon，并将修改后的策略与服务启动配置
比较。daemon 保留原有代际和启动摘要，各实例拥有自己的活动策略版本。原生输入、
Session、回调和清理排空后才激活更新。各框架边界与限制见[策略更新](policy-updates.md)。

下文 JSON 协议继续供显式集成使用。随附启动代码负责这些调用，普通应用通过框架启动
入口接入，无需自行构造句柄或私有生命周期 RPC。

## 连接并选择服务代际

使用业务配置单独启动服务，选定的 `spec.agents` target 必须声明匹配的 Adapter。使用服务报告的准确 socket：

```bash
AW_SOCKET=/absolute/private/state/aw.sock
aw instance --socket "$AW_SOCKET" --connect
```

发现阶段检查同用户端点、服务协议和独立登记支持，返回 `api_version: aw-instance/v1`、`service_protocol`、`identity` 和 `result.connected: true`。保存完整 `identity`，包括 generation 和启动配置摘要，供后续请求使用。此命令不会启动 daemon、重载配置或执行 target 的 `argv`。

```bash
aw instance --socket "$AW_SOCKET" --timeout-ms 10000 < request.json
```

每次操作从 stdin 读取一个最多 1 MiB 的 JSON 文档。默认总预算为 30,000 ms；`--timeout-ms` 接受 1–60,000 ms，覆盖 stdin、连接和操作。业务应使用参数数组启动 CLI，写完后关闭 stdin。参数语法错误沿用 CLI 的 stderr 与退出码 1；进入处理器的命令按下文 JSON 信封输出，stdout 自身失败除外。

## 请求与响应

登记请求形状如下。将示例 identity、PID 和启动时间替换为实际值；`target` 对应配置中的键。

```json
{
  "api_version": "aw-instance/v1",
  "identity": {"generation": "DISCOVERED_GENERATION", "config_revision": "DISCOVERED_DIGEST"},
  "command": {
    "action": "register",
    "key": {"request_id": "application-instance-1", "owner": {"pid": 1234, "start_ticks": "5678"}},
    "target": "target",
    "capabilities": {
      "adapter": "qoder-sdk", "version": "1.0.56", "entrypoint": "sdk-worker",
      "events": {"tool.before": ["observe", "block"], "tool.after": ["observe"]},
      "failure_actions": {}
    },
    "cwd": "/absolute/application/workspace",
    "environment": {"PATH": "/usr/bin:/bin"}
  }
}
```

Adapter 提供已经验证的原生版本、入口和实际支持的事件/效果；示例中的能力子集需要按真实接线核对。Provider cwd 必须是绝对路径，environment 显式提供业务所需值，daemon 不隐式补充环境。这些值是可信业务提交的执行上下文，不代表 `/proc/environ` 快照或原生拦截证据。即使其他实例的配置摘要相同，Provider 准备仍绑定本实例的 target 和上下文。

`owner.pid` 是业务进程；`owner.start_ticks` 为 `/proc/PID/stat` 第 22 个字段，以字符串传递。daemon 固定 UID、启动时间和 PID namespace，并在读取请求前固定 Unix peer 身份：owner 可以直接调用，也可以直接启动 CLI 子进程。Shell 或代理中转会被拒绝。owner 在准备与确认期间必须存活；仅提交其 PID 无法取得所有权。

所有操作使用相同的外层 `api_version` 和 `identity`。`command` 的字段如下：

| action | 必填字段 |
| --- | --- |
| `register` | `key`、`target`、`capabilities`、`cwd`、`environment`，可选 `config` |
| `register_native` | 相同字段，仅用于已准入的 Qoder/OpenClaw/Hermes 根桥接 |
| `query` | 原请求的 `key` |
| `ready` | 返回的完整 `handle` 和 `policy_revision` |
| `release` | 返回的完整 `handle` |

`register` 将普通 Qoder SDK、AgentScope、Pi、Claude SDK 和 QwenPaw 工厂路由投影到
子 Scope。`register_native` 选择 Qoder/OpenClaw/Hermes 根回调及其稳定请求、工具生命周期。
可选 `config` 是字符串形式的完整 YAML 或 JSON 配置；相较服务启动配置，只允许修改
Provider、事件与默认事件预算。省略时使用服务启动配置。候选决定实例的策略版本，binding
仍保留 daemon 的原有身份。

`key.request_id` 为 1–128 个 ASCII 字母、数字、`-`、`_`、`.` 或 `:`。每个预期的新实例使用新 ID。句柄包含 key 和完整 binding：服务身份、实例 ID、target 与准备审计键。保留这些字段，不修改或拼接两个句柄。

每个响应包含 `api_version`、`service_protocol`、`identity`、`result`、`error` 和 `audit_key`。操作成功时 result 非空、error 为 null、退出码为 0；拒绝时 result 为 null、error 为稳定字符串错误码、退出码为 1。准备失败还可能返回持久审计键。在发现服务前发生输入或连接错误时，identity 为 null。

## Ready 与任务 Scope

登记成功返回 `state: pending_ready`、`handle`、`policy_revision`、`routes`（包含 `events` 和原生 `hooks`）、`confirmation_timeout_ms: 30000` 及剩余确认时间。待就绪的根实例不能执行 Hook 事件。Adapter 在真实原生入口安装返回的路由后，使用同一句柄和 revision 发送 `ready`。Ready 记录经过身份核验的 Adapter 确认；回调是否实际安装、结果是否采纳，由 Adapter 验证，Service 无法从 PID 推断。

普通工厂根实例 Ready 后可使用现有任务 Scope 协议，Hook 执行仍须经过子 Scope。worker 为 owner 本身或其后代；每个任务操作必须由该 worker 或其直接启动的 CLI 发起。使用根 ID 和 worker 上下文调用 `open_scope`，保存任务 binding，并在五秒内确认。`scope_policy` 也可返回固定任务的原生路由。既有 `aw scope` 提供策略、确认、调用登记、原生回调、有效性检查与关闭。CLI HookBinding 使用返回的根 binding、socket、Adapter、cwd 和 `routes.events`，保存在业务拥有、权限为 0600 的文件中。下文 fixture 展示完整调用顺序。

原生根模式使用已准入的稳定桥接：原 owner 确认就绪与进程关联，桥接在工作前保留
整个原生 run 或轮次的准入，清理确认后再释放。原生回调的调用方须属于 owner 进程树，
待就绪根实例与无关调用方不能执行回调。根模式限于已准入的 Qoder/OpenClaw/Hermes 入口，
普通 SDK 根实例继续通过子 Scope 执行。

一个任务 Scope 覆盖整个请求，包括模型等待、原生工具、回调与取消收尾。关闭任务或释放根实例前，先停止准入并等待原始原生工作清理完毕。release 取消并等待 AW Provider 工作及其后代回收；业务进程、原生 Worker、另一实例与 daemon 继续由各自所有者管理。release 超时表示清理尚未确认，查询状态并在原生收尾后重试 release。生命周期请求失败或超时不构成工具执行许可。

可选证据采集在原生就绪或任务 Scope 确认后启动。各 Scope 独立持有有界采集任务与
来源会话关联，关闭时先排空读取，再释放关联。原生启动沿用 Adapter 已有的来源关联
覆盖，包括未核实或不可用的来源。证据配置在实例生命周期内保持固定，修改后须重新
准备并重启。

## 恢复与错误

| 状态 | 含义与操作 |
| --- | --- |
| `preparing` | 原登记仍在准备，继续查询 |
| `pending_ready` | 资源准备完成，在剩余期限内安装路由并确认 |
| `ready` | Ready 已确认，已准入的任务或原生桥接可以接受工作 |
| `closing` | 已停止准入，取消或清理仍在进行 |
| `released` | 实例所属资源已回收 |
| `expired` | 未确认准备已回收，显式重新尝试时使用新 request ID |
| `failed` | 准备失败，检查内层 `error` 和 `audit_key` |

响应丢失时结果未知，使用原 key 与 identity 查询。相同输入的重复登记返回已有状态，不重复准备，也不延长确认期限。同一 key 的输入变化返回 `registration_request_conflict`。成功查询或重复登记可能返回终态 failed，因此退出码为 0 时也要读取 `result.state`。Ready 与 release 支持重复确认。准备完成后 30 秒仍未 Ready 的实例自动过期，owner 退出也会触发回收。

去重记录保留到 daemon 本代结束，容量为 4,096 个 key。用尽后新 key 返回 `registration_limit`，既有 key 仍可查询；达到容量时安排排空并重启 daemon。重启使全部旧句柄失效。发现新 identity 后显式登记新实例，不重放原始工具或自动沿用旧 Ready 状态。

| 错误码 | 处理方式 |
| --- | --- |
| `stale_service`、`protocol_version`、`registration_protocol_version`、`registration_unsupported` | 核对选定服务与客户端组合，显式重新连接 |
| `registration_input_invalid`、`registration_input_unavailable` | 修正 JSON，或在期限内完成 stdin |
| `registration_request_id`、`registration_request_conflict` | 修正 key；有意创建不同实例时使用新 ID |
| `registration_owner_mismatch`、`registration_peer_mismatch`、`stale_process`、`bridge_process_mismatch` | 修正进程启动身份、直接调用关系或 worker 归属 |
| `registration_handle_mismatch`、`registration_unknown` | 使用完整返回句柄或原 key；unknown 不代表 Ready |
| `registration_not_ready`、`registration_confirmation_expired`、`registration_unavailable` | 查询并完成确认，或在清理后显式重新登记 |
| `registration_policy_mismatch` | 安装返回的 revision 后再 Ready |
| `registration_requires_scope` | 普通工厂根实例通过任务 Scope 执行，使用登记协议释放 |
| `binding_preflight`、`registration_adapter_unsupported`、`registration_cwd` | 修正 target、能力、原生路由约束或绝对 cwd |
| `binding_limit`、`registration_limit`、`connection_limit` | 释放/排空资源，或按上述去重容量流程处理 |
| `instance_release_timeout`、`transport_unavailable` | 按结果未知或清理未完成处理，查询后再继续 |
| `audit_unavailable`、`service_failure` | 停止派发并排查服务或审计故障 |

Provider 失败保留原有准备审计与运行时失败动作；审计故障沿用服务终止派发的语义。

## 可运行合同 fixture

在已安装 Rust、Python 和 Node 前提的 Linux 检出中执行：

```bash
cd src/aw
cargo test --locked -p aw-service --lib runtime::registration
cargo test --locked -p aw-cli --test service registration::
```

[业务进程 fixture](../../../../../src/aw/crates/aw-cli/tests/fixtures/independent_owner.py) 作为普通 Python 子进程运行，通过真实 CLI 验证登记、Ready 门禁、任务回调、fixture 实际副作用、同 PID 双实例、双 owner、独立释放与 owner 退出回收。补充 socket 测试覆盖响应丢失、Provider 准备失败和在途 Provider 后代取消；Service 单测确定性推进确认期限并检查审计故障。

这些 fixture 使用模拟原生边界，证明独立生命周期合同。各框架的启动与真实原生验收
记录在[原生清单](../../../../../src/aw/tests/native-matrix.json)，版本与覆盖范围独立于登记合同。

## 清理与回退

应用停止派发、关闭原生工厂和任务后，在 owner 仍存活时直接调用
`aw instance release --binding FILE`。成功释放排空本实例的 AW 资源，并删除准备时
返回的准确私有 `native-*` 目录。owner 与直接子进程核验继续生效，其他控制端不能通过
复制 binding 路径取得释放权限。

崩溃后，Service 按 owner 退出回收运行时资源，本地临时文件可能仍保留。确认该 owner
身份已退出、所属运行时资源已排空后，仅删除该次准备记录的目录。已退出 owner 的 binding
不能用于认证 release 或新进程。服务重启同样使旧句柄失效。

移除持久启动接点时，先停止受影响的原生应用，仅删除 `setup` 返回的文件，恢复应用原有
启动环境。保留其他 `NODE_OPTIONS`、Python 启动文件与原生设置。单独连接的插件按 Agent
指南清理，再正常重启。SDK 应用须在排空工作后同时移除启动代码和 AW 工厂接入。
回退集成版本时，配套恢复 AW 二进制与业务入口。

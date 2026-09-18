# Telemetry 安全事件同步设计规格

## 1. 背景与目标

`security_middleware.lifecycle` 在每次 action 完成或抛出异常时生成一条
`SecurityEvent`。本地安全审计仍通过 `security_events.log_event()` 写入 JSONL 和
SQLite；同一条事件还会被显式投影为一条隐私安全的 Telemetry 记录，追加到：

```text
/var/log/anolisa/sls/ops/agent-sec-core.jsonl
```

Telemetry 只用于聚合分析组件使用量、成功率、版本分布和结构化错误类型。它不用于
远程复现单次客户问题，也不上传请求内容、模型内容或原始错误。

本设计的目标是：

1. 使用每次写前检查的 sentinel 实现即时停写。
2. 公共 L1 envelope 默认适用于所有 middleware action，仅特殊字段按 action 投影。
3. 默认丢弃开放式容器和 backend 未来新增字段。
4. 保留不含错误消息的 `error_type` 和 `exit_code`，用于产品质量分析。
5. Telemetry 失败不影响安全能力和本地安全审计。

不在本阶段处理：

- 本地 `SecurityEvent` 中的原始 request/result/error 治理。
- L3 字段采集。
- 远端上传、重试队列或 OpenTelemetry exporter。
- 组件文件创建、权限管理和 logrotate 配置。
- 用户主动授权的详细诊断支持包。

## 1.1 V2 scan 共同实现

V2 的等价职责由 `asc-action-runtime` 的 Runtime/Finalizer 承担，所有注册 scan 共用
`asc-telemetry` allowlist 和 `asc-event-sink::telemetry::TelemetryWriter`。
不要求各 capability/handler 手动记录 telemetry。V1 的以下字段和门控规则仍是 oracle；
当前只实现 code-scan 的 telemetry 投影和 fixture；其它扫描能力随各自后续提交加入。
详见 [V2 共享生命周期与验收](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md#54-已实现的共享生命周期)。

## 2. 数据流与故障隔离

实际数据流为：

```text
security_middleware.lifecycle
  -> SecurityEvent(...)
  -> security_events.log_event(event)              # 本地 JSONL / SQLite
  -> telemetry.record_security_event_telemetry(event, ctx)
       -> lstat /etc/anolisa/.telemetry_disabled
       -> 检查预创建的目标 JSONL
       -> build_telemetry_security_event(event, ctx)
       -> TelemetryWriter.write(record)
```

本地安全事件写入和 Telemetry 写入分别隔离异常：任一侧失败都不阻断另一侧，也不改变
action 的业务返回。

## 3. 写前门控

### 3.1 Sentinel

```text
/etc/anolisa/.telemetry_disabled
/etc/anolisa/.telemetry_linked
```

L1 门控规则：

- `.telemetry_disabled` 存在：不构造、不序列化、不写入任何 Telemetry。
- disabled 不存在（`ENOENT`）：允许构造 L1。
- 检查 disabled 出现 `PermissionError` 或其他非 `ENOENT` 的 `OSError`：按禁用处理。
- 每次写入前重新 `lstat`，不缓存结果。

L3 门控规则：

- disabled 优先于 linked。
- 只有 disabled 不存在且 `.telemetry_linked` 存在时，未来获批的 L3 mapper 才可运行。
- linked 不存在或检查失败时只允许 L1。
- 当前 L3 mapper 集合为空，因此 linked 是否存在不会改变当前输出。

创建 disabled 后下一次事件立即停止写入；移除后下一次事件恢复 L1，无需重启进程。

`AGENT_SEC_TELEMETRY_LOG_PATH` 仅用于覆盖目标日志路径，不能绕过 sentinel。组件不实现
`.telemetry_enabled` 或环境变量 opt-out。

### 3.2 目标文件

目标目录、空文件及 logrotate 由注册授权模块管理。writer 的约束：

- 目标文件必须已经存在。
- 不使用 `O_CREAT`，不创建目录、锁文件或轮转文件。
- 每次写入 fresh open，完成后关闭，以兼容 rename rotation。
- 使用进程内锁和目标 fd 的非阻塞 `flock`。
- 写入失败 best-effort 丢弃，不重试。

目标文件存在仅代表运输通道就绪，不代表可以绕过 disabled 门控。

## 4. 数据分级

### 4.1 L1

本版本只输出匿名、结构化的产品运行数据：

- 固定组件名和版本。
- Agent 产品/运行时名称。
- 固定 action、category、执行结果和时间。
- 多个扫描能力复用的 verdict 和耗时。
- 必要的纯数值验证/基线计数。
- 结构化错误类型和退出码。

`component.agent_name` 只接受 `AgentName` 枚举中的产品名称：`codex`、`cosh`、
`hermes`、`openclaw`、`qoder`、`qwencode`。Telemetry 去除首尾空白后执行精确匹配；
缺失或未知值统一输出空字符串。该字段不得用于用户名、邮箱、账号、实例 ID 或客户自定义
Agent 昵称。新增 Agent 产品必须先扩展枚举并补充测试，避免把调用方传入的任意标识写入
Telemetry。

### 4.2 L3

当前没有 L3 输出字段。未来任何 L3 字段必须同时满足：

1. 通过字段级合规评审。
2. 注册到具体 action 的 L3 mapper。
3. disabled 不存在。
4. linked 存在。
5. 通过固定类型和取值校验。

### 4.3 严格丢弃

以下数据无论位于 `request`、`result`、`summary`、`details`、`error` 或未知字段，均不
进入 Telemetry：

- Prompt、query、对话历史、上下文和 assistant response。
- 模型输入、输出、reasoning 和原始模型错误。
- 代码、脚本、文件内容、扫描 evidence 和命中片段。
- 命令、argv、cwd、stdout 和 stderr。
- 文件路径、Skill 路径、工具路径和配置内容。
- 用户、账号、设备、进程和实例标识。
- token、密钥、passphrase、凭据和原始内容 hash。
- 原始错误消息、error details 和 traceback。
- event/trace/session/run/call/tool-call ID。
- backend 未来新增但未进入特殊字段投影的任何值。

被丢弃或无效的字段完全省略，不输出 `null`。

## 5. Schema

### 5.1 公共字段

所有记录包含：

```text
component.name
component.version
component.agent_name
```

除 `harden` 外的记录包含：

```text
seccore.event_type
seccore.category
seccore.result
seccore.timestamp
```

`event_type` 和 `category` 由 security middleware lifecycle 写入 `SecurityEvent`，Telemetry
直接使用该受信 envelope，不再维护完整 action/category 映射。新增 action 默认只输出公共
L1 字段。

### 5.2 Action 字段矩阵

| action | 公共字段外允许的字段 |
| --- | --- |
| `sandbox_prehook` | 结构化产品错误发生时的 `error_type` / `exit_code` |
| `summary` | 结构化产品错误发生时的 `error_type` / `exit_code` |
| `skill_ledger` | 结构化产品错误发生时的 `error_type` / `exit_code` |
| `code_scan` | `seccore.verdict`、`seccore.elapsed_ms`、可选错误字段 |
| `prompt_scan` | `seccore.verdict`、`seccore.elapsed_ms`、可选错误字段 |
| `pii_scan` | `seccore.verdict`、`seccore.elapsed_ms`、可选错误字段 |
| `verify` | `seccore.asset_outcome`、`seccore.asset_passed_count`、`seccore.asset_failed_count`、可选错误字段 |
| `harden` | baseline result/timestamp/counts、可选错误字段 |
| 其他或新增 action | 无额外字段，仅公共 L1 envelope 和可选错误字段 |

`pii_scan` 不输出专属 request、summary、PII 类型分布或扫描文本统计。

扫描 verdict 仅接受：

```text
pass / warn / deny / error
```

耗时必须是非负有限数值。计数必须是非负整数；布尔值不能作为整数接受。

`seccore.asset_outcome` 仅接受：

```text
verified / failed / no_candidates
```

- `verified`：至少检查了一个候选 Skill，且所有候选均通过。
- `failed`：至少一个候选 Skill 验证失败。
- `no_candidates`：可选发现根目录的扫描正常完成，但没有找到候选 Skill。缺失或为空的
  `/usr/share/anolisa/skills` 与 `/usr/local/share/anolisa/skills` 不属于产品失败。

配置、受信密钥、canonicalization 或根目录枚举异常属于操作错误；此时 backend 没有稳定
的资产结果，允许省略 `seccore.asset_outcome` 和资产计数。Telemetry 不投影配置根目录、
候选 Skill 路径或其他文件系统路径。

### 5.3 示例

Code Scanner 成功记录：

```json
{
  "component.name": "agent-sec-core",
  "component.version": "<package-version>",
  "component.agent_name": "qwencode",
  "seccore.event_type": "code_scan",
  "seccore.category": "code_scan",
  "seccore.result": "succeeded",
  "seccore.timestamp": "2026-07-16T08:00:00+00:00",
  "seccore.verdict": "pass",
  "seccore.elapsed_ms": 3
}
```

Asset Verification 未发现候选的成功记录：

```json
{
  "component.name": "agent-sec-core",
  "component.version": "<package-version>",
  "component.agent_name": "codex",
  "seccore.event_type": "verify",
  "seccore.category": "asset_verify",
  "seccore.result": "succeeded",
  "seccore.timestamp": "2026-07-16T08:00:30+00:00",
  "seccore.asset_outcome": "no_candidates",
  "seccore.asset_passed_count": 0,
  "seccore.asset_failed_count": 0
}
```

Harden 产品错误记录：

```json
{
  "component.name": "agent-sec-core",
  "component.version": "<package-version>",
  "component.agent_name": "codex",
  "baseline.result": "failed",
  "baseline.timestamp": "2026-07-16T08:01:00+00:00",
  "baseline.error_type": "FileNotFoundError",
  "baseline.exit_code": 127
}
```

## 6. 错误质量指标

允许上传：

```text
seccore.error_type
seccore.exit_code
baseline.error_type
baseline.exit_code
```

`error_type` 只能来自实际异常类名或 backend 明确设置的稳定产品错误分类。它必须是最长
128 字符的简单结构化名称，只包含字母、数字、下划线和点。不得从错误文案中正则猜测，
不得读取开放式 summary，也不得拼接异常消息或客户输入。

`exit_code` 仅在有效 `error_type` 存在时输出：

- 接受整数并拒绝布尔值。
- 允许负数以表达子进程被 signal 终止。
- 没有实际退出码的 Python 异常不虚构退出码。
- 成功事件不输出错误字段。
- 安全检测的 warn/deny/tampered 不自动归类为产品错误。

原始 `error`、截断/脱敏 error、error hash、error details、traceback、stdout 和 stderr 均不
上传。详细根因留在本地诊断数据中。

`ActionResult` 提供可选的 `error_type` 字段。lifecycle 只在失败结果同时提供结构化类型时，
把 `error_type` 和 `exit_code` 放入本地 event 顶层，供 Telemetry mapper 精确读取；原始
`error` 仍只服务本地 CLI 和审计。

## 7. Mapper 与 sanitizer

builder 的内部接口为：

```python
def build_telemetry_security_event(
    event: SecurityEvent,
    ctx: TelemetryContext,
) -> dict[str, Any]:
    ...
```

Telemetry 直接消费 middleware 已写入的 `event_type` 和 `category`，只用 `_SCAN_ACTIONS`、
`_BASELINE_ACTION` 和 `_ASSET_VERIFY_ACTION` 表达特殊字段投影。公共 envelope、扫描指标、
资产 outcome 与计数、baseline 计数和错误字段分别复用小型 scalar projector。

Telemetry sanitizer 不提供通用递归 JSON 转换，不调用未知对象的 `model_dump()` 或
`str()`，也不自动展开 dict/list。新增字段必须修改显式映射和精确字段测试，不能仅由 backend
增加 result key 后自动进入上传日志。

## 8. 测试与验收

必须覆盖：

- disabled 存在、缺失、权限错误、其他 I/O 错误和状态即时变化。
- linked 存在、缺失和 fail-close；disabled 始终优先。
- 8 个 action 的精确 key set。
- 新增 action 无需修改 Telemetry 即可输出公共 L1 envelope，且不会获得特殊字段。
- `AgentName` 枚举中的 6 个产品名可输出，未知值和客户标识统一降为空字符串。
- PII request/summary 完全缺席。
- Asset Verification 三种 outcome、对应计数组合及操作错误省略 outcome 的契约。
- 原始 Prompt、代码、命令、路径、错误消息和所有 ID 的 canary 无法进入 JSON。
- 未知对象不会被字符串化。
- error type 格式、失败状态和 exit code 的组合约束。
- warn/deny/tampered 不会被误报为产品错误。
- 目标文件缺失时不创建文件。
- Code/Prompt E2E 按调用前 byte offset 查找新增记录，不依赖已删除的 trace ID。

正向 E2E 若检测到系统 disabled sentinel 已存在或不可读取，应跳过；测试不得创建、删除或
覆盖真实 `/etc/anolisa` sentinel。

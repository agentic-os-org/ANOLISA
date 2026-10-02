---
name: security-observability
description: 只读查询 agent-sec-cli 已落盘的历史安全事件记录，并据此生成会话级安全复盘。仅当用户显式要求查看或审计已发生的安全事件、安全告警、安全审计记录，或要求按 session/run/trace/时间/类别筛选与统计已有安全事件，或要求复盘某次会话的安全判定时使用。不用于扫描新内容：检查代码安全性用 code-scanner，检测 prompt 注入用 prompt-scanner，审查 Skill 安全状态用 skill-ledger。不要因为对话中出现“安全”“工具调用”等字样、或为了主动自查而触发。
---

# Security Observability

通过 `agent-sec-cli` 查询本地 SQLite 中的安全事件，并将事件与 Agent 会话关联起来。此 Skill 只执行只读查询，不负责写入 observability 数据。

## 查询流程

1. 先用 `events --summary` 或 `events --count-by` 获取概览。
2. 根据 `event_type`、`category`、关联 ID 和时间范围缩小查询。
3. 需要程序解析时使用 `--output json` 或 `--output jsonl`，不要解析 table 或 summary 文本。
4. 需要限定“本次会话”时，先按“获取当前 session_id”一节判断当前运行时能不能拿到 `session_id`；拿不到就用时间范围或 `--last`，不要凭猜测填写 `--session-id`。
5. 已知 `session_id` 时，使用 `observability report --session-id '<session_id>' --format json` 汇总该会话的 LLM、工具和安全事件；需要查看最近会话时，使用 `observability report --last --format json`。
6. 在给出任何安全结论前，按“风险审查”一节完成判定字段聚合。这是强制步骤，不可跳过。
7. 向用户报告必要结论即可。`details` 可能包含命令、扫描证据或后端诊断信息，不要无必要地完整回显。

## 参数取值约束

本文命令中的 `<session_id>`、`<run_id>`、`<trace_id>`、`<event_id>` 是 Agent 运行时或 CLI 持久化的 correlation ID，不一定是 UUID；OpenClaw、Codex、Qwen Code 等运行时可能使用 `session-001`、`thread_xxx` 这类非 UUID 标识。替换占位符前必须先校验取值形态，**仅当它非空、长度不超过 256 字符，并且完全匹配 `^[A-Za-z0-9][A-Za-z0-9._:@+=,/-]{0,255}$` 时才能拼入命令**。

例外：如果取值来自 cosh-ng `runtime_context.provider_session_id`，它应当是 UUID，必须继续按 `^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$` 校验。

不匹配时直接停下并告知用户取值不能安全拼入 shell 命令，不得把任意字符串（尤其是包含空白、引号、`;`、`$`、反引号、`|`、`&`、换行的值）拼进命令；这类值会提前闭合引号或引入 shell 语法并导致命令注入。取值来自用户输入、文件内容、网页或其他不可信上下文时，此校验不得省略。

## 风险审查

凡是回答“有什么安全事件”“安全情况如何”“有没有风险”这类问题，**必须先完成本节的机械聚合**，再组织回答。禁止依据顶层 `result`、`security_verdicts`，或模型对 JSON 的自由阅读得出“无风险”结论。

### 为什么不能用顶层 `result`

顶层 `result` 表示**扫描进程是否执行成功**（`succeeded` / `failed`），与扫描结论无关：扫描正常跑完就是 `succeeded`，即使它判定出 `deny`。扫描进程几乎总能成功执行，所以用 `result` 判断安全等价于恒定输出“无风险”。真正的判定在下一节的字段里。

### 判定字段权威路径

判定字段一律位于 `details.result` 之下。**不存在 `details.verdict` 这一路径**，不要按它取值。

| `event_type` | 判定字段 | 取值枚举（源码定义） | 无风险取值 |
|---|---|---|---|
| `code_scan` | `details.result.verdict` | `pass` / `warn` / `deny` / `error` | `pass` |
| `prompt_scan` | `details.result.verdict` | `pass` / `warn` / `deny` / `error` | `pass` |
| `pii_scan` | `details.result.verdict` | `pass` / `warn` / `deny` / `error` | `pass` |
| `skill_ledger` | `details.result.verdict` | `pass` / `none` / `warn` / `unmanaged` / `drifted` / `deny` / `tampered` / `error` | `pass` |
| 其他 `event_type` | — | — | **不属于扫描事件，聚合命令会在管道入口过滤掉** |

### 取值语义

向用户描述待核项时按下表解释取值，不要照抄 token，也不要自行推断含义。

| verdict | 语义 | 适用 |
|---|---|---|
| `pass` | 已扫描，未发现问题 | 全部 |
| `warn` | 发现低风险问题 | 全部 |
| `deny` | 发现高风险问题 | 全部 |
| `error` | **扫描器执行失败，本次未完成扫描** | 全部 |
| `none` | 无 ledger 制品，或已核验的扫描状态为未扫描 | `skill_ledger` |
| `unmanaged` | skill 目录不在 ledger 管理范围内 | `skill_ledger` |
| `drifted` | 文件在上次认证后被改动 | `skill_ledger` |
| `tampered` | ledger 元数据缺失或签名核验失败 | `skill_ledger` |
| `MISSING` | 判定字段缺失，本次操作未产生判定 | 全部 |

`skill_ledger` 出现 `MISSING` 通常是 `status`、`audit`、`list-scanners` 这类非判定命令留下的审计记录（查 `details.result.command` 可确认是哪个命令）。这类条目仍会出现在聚合输出的待核项里，但描述时应说明为“非判定操作”，不得断言为风险；`code_scan`、`prompt_scan`、`pii_scan` 出现 `MISSING` 才属异常，必须作为真实待核项呈现。

`error`、`none`、`unmanaged` 表示**未取得有效判定**，既不能表述为“安全”“无风险”，也不能表述为“检测到高危”。正确表述是本次未完成扫描或该目标不在扫描范围内。

### 分类规则：用允许列表，不用拒绝列表

本节只处理**安全扫描事件**（上表四种）。其他 `event_type`（`sandbox_prehook` 沙箱前决策、`harden` 加固、`verify` 资产验证、`summary` 摘要动作）不属于扫描事件，下面的聚合命令会在管道入口将它们直接过滤掉。

允许列表同时作用于两个维度：`event_type` 必须在上表四种中（其余一律过滤），**并且**判定字段显式等于对应的无风险取值。两个条件同时成立才可计入无风险。

- 判定字段缺失、不是字符串、`details` 或 `details.result` 不是对象时，记为 `MISSING` 并列为待核项，不得当作 `pass`。
- 已知类型但判定值不等于无风险取值时，一律列为待核项，包括上表未列出的新增取值。
- 不得因为某个取值“看起来不严重”而省略。`warn` 与 `none` 必须出现在待核项清单里。

### 聚合命令

不要靠阅读 JSON 归纳，必须执行聚合命令。聚合命令的输出只包含**计数表 + 仅 RISK 行**，`pass`/`allow` 事件不逐行列出。对于 `verdict == "pass"` 的普通事件，不要查询其具体 `details` 内容——它们的行为符合预期，打出来只会占上下文。

输出中以 `RISK` 开头的行即待核项。

```bash
agent-sec-cli events --session-id '<session_id>' --count
agent-sec-cli events --session-id '<session_id>' --output json --limit '<matching_count_or_safe_page_size>' | jq -r '{code_scan:"pass",prompt_scan:"pass",pii_scan:"pass",skill_ledger:"pass"} as $ok | [.[] | select($ok[.event_type // ""] != null) | {t: .event_type, j: (.details | if type=="object" then .result else null end | if type=="object" then .verdict else null end | if type=="string" then . else "MISSING" end), ts: .timestamp, id: .event_id}] as $rows | "total=\($rows|length)  risk_items=\([$rows[]|select(.j != $ok[.t])]|length)", ($rows | group_by([.t,.j])[] | "  \(.[0].t) \(.[0].j) \(length)"), ($rows[] | select(.j != $ok[.t]) | "RISK \(.ts) \(.t) \(.j) \(.id)")'
```

环境没有 `jq` 时用等价的 python3（`python3` 是 `agent-sec-cli` 的运行依赖，一定可用）：

```bash
agent-sec-cli events --session-id '<session_id>' --count
agent-sec-cli events --session-id '<session_id>' --output json --limit '<matching_count_or_safe_page_size>' | python3 -c 'import sys,json,collections;OK={"code_scan":"pass","prompt_scan":"pass","pii_scan":"pass","skill_ledger":"pass"};V=lambda d:(lambda r:r["verdict"] if isinstance(r,dict) and isinstance(r.get("verdict"),str) else "MISSING")(d.get("result") if isinstance(d,dict) else None);R=[(e["event_type"],V(e.get("details")),e.get("timestamp"),e.get("event_id")) for e in json.load(sys.stdin) if e.get("event_type") in OK];K=[x for x in R if x[1]!=OK[x[0]]];C=collections.Counter((x[0],x[1]) for x in R);print("total=%d  risk_items=%d"%(len(R),len(K))+"".join("\n  %-14s %-13s %d"%(t,j,n) for (t,j),n in sorted(C.items()))+"".join("\nRISK %s %s %s %s"%(x[2],x[0],x[1],x[3]) for x in K))'
```

按时间范围查询时，把 `--session-id '<session_id>'` 换成 `--last-hours N` 等筛选条件，并保留相同的 `--count` 与 `--limit` 策略。注意 `--limit` 默认 100：聚合前必须先读取匹配总数，再调大 `--limit` 或分页，否则待核项会被截断而漏报。

### 上下文开销控制

安全报告不应挤占 Agent 对话的有效上下文，遵循以下原则：

1. **先查总数，再覆盖全量聚合**：使用 `--count` 取得匹配事件数量。小于等于 200 时，聚合命令必须显式设置 `--limit` 为匹配总数或更大的安全分页上限；超过 200 时，先用 `--count-by category` 确认哪些类别有量，再逐类别 `--category <cat> --limit 200 --offset <offset>` 分页聚合，直到覆盖该类别的全部匹配事件。
2. **只展开待核项**：`pass`/`allow` 事件只出现在计数表的数字里，不逐行列出，更不要查询其 `details`。只有 RISK 行才需要向用户呈现。
3. **仅按需深钻**：如果用户要求了解某条待核项的具体原因，再按“获取单条事件细节”一节取它的 `details`（一条）。不要默认批量展开全部 `details`。
4. **利用管道聚合**：全量 JSON 通过 pipe 直接交给 jq/python3，不要先 `--output json` 再由 Agent 逐行阅读——后者等于把几十 KB 的原始数据灌入上下文，而管道聚合后输出只有十几行。

### 报告输出契约

按顺序输出，前两项不得省略：

1. **查询范围**：实际使用的筛选条件（`session_id` 或时间范围）、`--limit` 取值与匹配总数。
2. **待核项清单**：逐条列出每个待核事件的 `timestamp`、`event_type`、判定值。一条都没有时，写“按判定字段聚合后待核项为 0”，而不是“没有安全事件”。
3. **按 `event_type` × 判定值的计数表**。
4. 需要时再补充结论与建议。

禁止表述：在未完成本节聚合的前提下输出“无任何安全事件”“未发现风险”“一切正常”。待核项非 0 时，结论段必须包含这些项，不得只体现在计数表里。

### 获取单条事件细节

所有细节都在统一的 `details` 字段下，形状固定为 `{request, result}`：

- `details.request` —— 本次扫描的输入侧信息。
- `details.result.summary` —— 判定摘要（一句话或分类计数）。
- `details.result.findings[]` —— **命中明细，要回答“为何被判定”就看这里**。

`events` **没有 `--event-id` 过滤参数**，按 `event_id` 取单条需要在客户端过滤：

```bash
agent-sec-cli events --session-id '<session_id>' --output json | jq -r '.[] | select(.event_id == "<event_id>") | .details'
```

只看判定依据，不取整个 `details`（更省上下文）：

```bash
agent-sec-cli events --session-id '<session_id>' --output json | jq -r '.[] | select(.event_id == "<event_id>") | {summary: .details.result.summary, findings: .details.result.findings}'
```

`findings[]` 内部字段因扫描器而异（规则类扫描带规则标识与描述，PII 类带类型与脱敏证据）。**按实际返回的字段陈述，不要假设字段名，也不要把某一种扫描器的字段套到另一种上。**

注意：不同扫描器对 `details.request` 的处理强度不同——部分扫描器只存长度与哈希，部分会存被扫描的原始内容。引用 `details.request` 时适用下一节的敏感值约束。

### 报告不得重新引入敏感值

安全事件入库时已做脱敏：`findings[].evidence_redacted` 存的是按类型脱敏后的值（如 `phone_cn` 为 `NNN****NNNN`、`credit_card` 为 `[REDACTED_CARD:后四位]`、凭据类为 `[REDACTED_*]`），`request` 侧只存 `text_length` 与 `text_sha256`，**不存原文**。

描述事件时只能使用事件自带的脱敏字段（`type`、`category`、`severity`、`confidence`、`evidence_redacted`、`span`）。**禁止为了“解释更清楚”而从对话历史、用户输入、工具参数或其他上下文里找回并复述原始敏感值**（手机号、卡号、身份证号、密钥、token 等）。

原因：模型输出会被 PII 扫描（`source=model_output`）。在报告里复述原始敏感值会当场触发新的 `pii_scan` 告警，把一次只读查询变成一次新的泄露事件。

- 正确：直接引用事件字段，如“`pii_scan` warn × 3，`type` 为 `phone_cn` / `credit_card`，`evidence_redacted` 已脱敏”。
- 错误：为了说明触发原因而把用户当时输入的手机号、卡号原文重新写进回复。

## 获取当前 session_id

`session_id` 由 Agent 运行时提供，`agent-sec-cli` 自身无法推断“本次会话”。能不能按 `session_id` 查询取决于当前运行时。

### cosh-ng 特别用法：`runtime_context` 工具

> 本节仅适用于 **cosh-ng** Agent。截至当前版本，其他 Agent 运行时（cosh/copilot-shell、OpenClaw、Codex、Qwen Code、Hermes、Qoder CLI 等）没有这个工具，直接跳到“其他 Agent 运行时”一节。判断方式是看当前可用工具列表里有没有 `runtime_context`，不要靠版本号猜。

cosh-ng 提供只读工具 `runtime_context`（无参数），返回当前运行时元数据，其中 `provider_session_id` 就是本次会话的 `session_id`：

```json
{
  "provider_session_id": "<session_id>",
  "runtime": { "name": "cosh-ng", "version": "<version>" },
  "model": "<model>",
  "approval_mode": "<approval_mode>",
  "workspace": { "cwd": "<cwd>", "project_root": "<project_root>" },
  "session": { "resumed": false },
  "compaction": {
    "revision": 0,
    "active_projection": false,
    "compacted_through": null
  },
  "capabilities": { "tools": [], "active_extensions": [] }
}
```

用法是先调用 `runtime_context` 工具取得 `provider_session_id`，再把该值作为 `--session-id` 的参数：

```bash
# 本次会话的安全事件
agent-sec-cli events --session-id '<provider_session_id>' --output json

# 本次会话的会话级报告
agent-sec-cli observability report --session-id '<provider_session_id>' --format json
```

`provider_session_id` 与 cosh-ng hook 上报、并最终写入安全事件与 observability 记录的 `session_id` 是同一个值，且应当是 UUID，因此可以在通过 UUID 校验后直接用于上面两个查询，无需再做映射或截断。

约束：

- **不要用环境变量 `$COSH_SESSION_ID` 代替。** 在 cosh-ng 中它表示 shell/终端会话（审计记录里的 `shell_session_id`），与 Agent 会话的 `session_id` 属于不同命名空间，且在缺省时会退化成进程级的兜底值。用它查询通常会静默返回 0 条事件，看起来像“本次会话没有安全事件”，属于错误结论。
- `runtime_context` **不返回 `run_id`**。需要按 run/turn 缩小范围时，`run_id` 仍必须来自当前上下文或用户提供。
- `runtime_context` 是只读工具，只读取运行时元数据，不写入 observability 数据。
- 该工具无参数；不要尝试传入 `session_id` 之类的字段去“查询指定会话”。

### 其他 Agent 运行时：不使用 `--session-id`

截至当前版本，只有 cosh-ng 能让 Agent 取得自己的 `session_id`。其他 Agent 运行时（cosh/copilot-shell、OpenClaw、Codex、Qwen Code、Hermes、Qoder CLI 等）没有对应能力，Agent 无法识别“本次会话”，因此：

- 默认改用**时间范围查询**（`--last-hours`，或 `--since` / `--until`），或用 `observability report --last` 查询最近记录的会话。不要因为拿不到 `session_id` 而停下来反复询问用户。
- **必须在报告中说明实际查询范围**，例如“最近 1 小时的安全事件”或“最近记录的一次会话，不一定是当前会话”。不要把这类结果表述成“本次会话”的结论。
- 只有用户主动提供 `session_id` 时，才使用 `--session-id` 精确查询；拼入前先按“参数取值约束”一节校验取值形态。
- 同样不要用 shell 环境变量（包括 `$COSH_SESSION_ID`）凑一个 `session_id` 出来。

## Few-shot 场景

### 查询最近一小时的安全事件

**用户：** 帮我查询最近一个小时出现的安全事件。

**执行：**

```bash
agent-sec-cli events --last-hours 1 --output json
```

**回答：** 先按“风险审查”一节对结果做判定字段聚合（把 `--session-id` 换成 `--last-hours 1`），再按报告输出契约作答：查询范围为最近一小时、匹配总数、待核项清单、计数表。不要按顶层 `result` 概括，也不要把 `succeeded` / `failed` 解释为扫描 verdict。

### 查询本次会话的安全事件

**用户：** 帮我查询本次会话出现的安全事件。

**执行：** 在 cosh-ng 上，先调用 `runtime_context` 工具取得 `provider_session_id`，再精确查询：

```bash
agent-sec-cli events --session-id '<current_session_id>' --output json
```

其他 Agent 运行时拿不到当前 `session_id`，改用时间范围查询：

```bash
agent-sec-cli events --last-hours 1 --output json
```

**回答：** 先按“风险审查”一节完成判定字段聚合，再按报告输出契约作答。cosh-ng 上说明使用的 `session_id`、匹配数量、待核项清单和计数表，不要退而使用 `$COSH_SESSION_ID`。其他运行时必须说明实际查询的是最近 N 小时而不是“本次会话”，不要把结果表述成本次会话的结论。

### 复盘本次会话的安全情况（cosh-ng）

**用户：** 帮我复盘本次会话的安全情况。

**执行：** 调用 `runtime_context` 工具取得 `provider_session_id`，再生成该会话的报告：

```bash
agent-sec-cli observability report --session-id '<provider_session_id>' --format json
```

**回答：** `observability report` 只提供会话范围、`tool_breakdown` 和按顶层 `result` 聚合的 `security_verdicts`，**不足以支撑安全结论**。必须再用同一个 `session_id` 执行“风险审查”一节的聚合命令，并按报告输出契约给出待核项清单。不要改用 `--last`：它查询最近记录的会话，在并发或嵌套会话下可能不是本次会话。

## 安全事件查询

### 概览

```bash
# 最近 24 小时的人类可读安全态势摘要
agent-sec-cli events --summary

# 最近 24 小时按类别统计
agent-sec-cli events --last-hours 24 --count-by category

# 最近 8 小时 code_scan 事件数量
agent-sec-cli events --last-hours 8 --category code_scan --count
```

`--summary` 在未指定时间范围时默认查询最近 24 小时。它输出人类可读文本，只适合展示，不适合作为稳定的数据接口。

### 筛选并获取结构化数据

```bash
# 查询最近一小时的代码扫描事件
agent-sec-cli events --last-hours 1 --category code_scan --output json

# 按 session 和 run 精确关联，并以 JSONL 输出
agent-sec-cli events --session-id '<session_id>' --run-id '<run_id>' --output jsonl

# 查询 ISO-8601 时间区间；since 包含边界，until 不包含边界
agent-sec-cli events --since '2026-08-05T00:00:00Z' --until '2026-08-06T00:00:00Z' --limit 100 --offset 0 --output json
```

### 参数

| 参数 | 默认值 | 说明 |
|------|--------|------|
| `--event-type` | 无 | 按事件类型筛选 |
| `--category` | 无 | 按安全能力类别筛选 |
| `--trace-id` | 无 | 按一次 CLI/调用链 trace 筛选 |
| `--session-id` | 无 | 按 Agent 会话筛选 |
| `--run-id` | 无 | 按 Agent run/turn 筛选 |
| `--since` | 无 | ISO-8601 起始时间，包含该边界 |
| `--until` | 无 | ISO-8601 结束时间，不包含该边界 |
| `--last-hours` | 无 | 查询最近 N 小时，可使用小数 |
| `--limit` | `100` | 最多返回的事件数 |
| `--offset` | `0` | 跳过的事件数 |
| `--count` | `false` | 只输出匹配事件数量 |
| `--count-by` | 无 | 分组计数，仅支持 `category`、`event_type`、`trace_id` |
| `--output`, `-o` | `table` | 列表输出格式：`table`、`json`、`jsonl` |
| `--summary` | `false` | 输出人类可读安全态势摘要 |

### 参数约束

- `--last-hours` 与 `--since` / `--until` 互斥。
- `--count` 与 `--count-by` 互斥。
- `--summary` 与 `--count`、`--count-by`、任何显式 `--output` 互斥。
- `--summary` 会读取最多 10000 条匹配事件；普通列表使用 `--limit` 和 `--offset`。
- 未知 `event_type` 或 `category` 会产生 warning，但查询仍会执行，以兼容未来新增类型。

## 事件类型与类别

| `event_type` | `category` | 含义 |
|--------------|------------|------|
| `sandbox_prehook` | `sandbox` | 沙箱执行前决策 |
| `harden` | `hardening` | Security Baseline 检查或加固 |
| `verify` | `asset_verify` | 资产完整性验证 |
| `summary` | `summary` | 安全摘要动作 |
| `code_scan` | `code_scan` | 代码安全扫描 |
| `prompt_scan` | `prompt_scan` | Prompt 安全扫描 |
| `pii_scan` | `pii_scan` | PII/凭据检测 |
| `skill_ledger` | `skill_ledger` | Skill Ledger 检查 |

成功或失败由顶层 `result` 表示，不要假设失败事件的 `event_type` 带 `_error` 后缀。

## `events` 输出结构

### JSON 列表与 JSONL

`--output json` 返回事件对象数组。`--output jsonl` 每行返回一个相同结构的事件对象。事件 envelope 为：

```json
{
  "event_id": "<uuid>",
  "event_type": "code_scan",
  "category": "code_scan",
  "result": "succeeded",
  "timestamp": "<ISO-8601 UTC>",
  "trace_id": "<trace_id>",
  "pid": 1234,
  "uid": 1000,
  "session_id": "<session_id-or-null>",
  "run_id": "<run_id-or-null>",
  "call_id": "<call_id-or-null>",
  "tool_call_id": "<tool_call_id-or-null>",
  "details": {}
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `event_id` | string | 事件 UUID |
| `event_type` | string | 事件类型 |
| `category` | string | 聚合类别 |
| `result` | string | `succeeded` 或 `failed` |
| `timestamp` | string | ISO-8601 UTC 时间 |
| `trace_id` | string | 调用链标识，可能为空字符串 |
| `pid`, `uid` | integer | 记录事件的进程与用户 ID |
| `session_id`, `run_id` | string \| null | Agent 会话和 run/turn 关联标识 |
| `call_id`, `tool_call_id` | string \| null | LLM 与工具调用关联标识 |
| `details` | object | 后端专属结构化数据 |

`details` 没有跨事件类型的固定 schema。只读取当前任务需要且实际存在的字段，不要根据其他类别的事件臆测字段。

扫描判定固定位于 `details.result` 之下：扫描类事件用 `details.result.verdict`，`sandbox_prehook` 用 `details.result.decision`。**`details.verdict` 不是有效路径**，不要按它取值。取值前先检查类型与存在性，缺失时按“风险审查”一节记为 `MISSING` 并列为待核项。

### 计数输出

`--count` 输出一个 JSON 整数：

```json
12
```

`--count-by` 输出一个 JSON 对象，键是分组值，值是数量：

```json
{
  "code_scan": 8,
  "prompt_scan": 4
}
```

## 会话级报告

### 调用方式

```bash
# 最近记录的会话
agent-sec-cli observability report --last --format json

# 指定会话
agent-sec-cli observability report --session-id '<session_id>' --format json
```

选择 `--last` 或 `--session-id` 之一：`--last` 查询最近记录的会话，`--session-id '<session_id>'` 查询指定会话。命令没有默认目标；两者都不提供时返回错误。`--format` 支持 `text` 和 `json`，供 Agent 解析时必须使用 `json`。

### JSON 结构

```json
{
  "session_id": "<session_id>",
  "first_seen": "2026-08-05 10:00:00",
  "last_seen": "2026-08-05 10:05:00",
  "duration_seconds": 300.0,
  "turn_count": 3,
  "llm_calls": 4,
  "request_bytes": 1200,
  "response_bytes": 2400,
  "tool_breakdown": {
    "shell": 2
  },
  "security_verdicts": {
    "code_scan": {
      "succeeded": 2
    }
  },
  "security_hint": null
}
```

| 字段 | 类型 | 说明 |
|------|------|------|
| `session_id` | string | 会话 ID |
| `first_seen`, `last_seen` | string | 会话首末事件的 UTC 时间文本 |
| `duration_seconds` | number | 会话持续秒数，保留一位小数 |
| `turn_count` | integer | 记录的 Agent turn 数 |
| `llm_calls` | integer | `after_llm_call` 事件数 |
| `request_bytes`, `response_bytes` | integer | 模型请求与响应累计字节数 |
| `tool_breakdown` | object<string, integer> | 工具名称到调用次数的映射 |
| `security_verdicts` | object<string, object<string, integer>> | 安全类别到 `result` 计数的映射 |
| `security_hint` | string \| null | 安全事件不可用、未关联或查询失败时的说明 |

`security_verdicts` 当前按安全事件顶层 `result`（如 `succeeded` / `failed`）聚合；不要把它解释为扫描器的 `pass` / `warn` / `deny`。顶层 `result` 只反映扫描进程是否执行成功，几乎恒为 `succeeded`，因此 `security_verdicts` **全 `succeeded` 不代表无风险**，不能作为安全结论的依据。得出任何安全结论前，必须用相同 `session_id` 执行“风险审查”一节的聚合命令。

## 关联与报告规则

- 优先使用 `session_id` 关联会话，使用 `run_id` 缩小到某个 run/turn；`trace_id` 用于追踪一次具体调用链。`run_id` 和 `trace_id` 只能来自用户或已有查询结果，Agent 无法自行获取。
- 只有 cosh-ng 能把“本次会话”落实为真实的 `--session-id` 查询（通过 `runtime_context` 的 `provider_session_id`）。其他运行时用时间范围或 `--last`，并在报告里写清实际范围。不要用 shell 环境变量推测会话身份。
- 会话报告没有安全事件时，检查 `security_hint`，不要直接断言“没有发生安全检测”。查询返回 0 条时，先确认传入的 `session_id` 确实是当前会话的 ID，再下结论。
- table 与 summary 是人类展示格式，不承诺稳定列结构；自动化处理必须选择 JSON/JSONL。
- 报告事件时给出查询范围、筛选条件、匹配数量和必要结论。除非用户明确要求，不完整输出 `details` 中的命令、输入、证据或诊断信息。
- **安全结论必须来自“风险审查”一节的聚合输出，不得来自对 JSON 的自由阅读。** 顶层 `result` 与 `security_verdicts` 都不是判定依据。未完成聚合时，不得输出“无风险”“无安全事件”“一切正常”一类表述；待核项非 0 时，必须逐条列出。
- 事件总数接近或超过 `--limit`（默认 100）时，先调大 `--limit` 或分页取全量，再做聚合。基于被截断的结果得出的“无风险”结论是错误的。即使匹配总数小于等于 200，也不能省略显式 `--limit`；默认 100 会截断 101–200 条事件。

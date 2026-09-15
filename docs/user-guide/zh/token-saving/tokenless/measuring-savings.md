# Tokenless 效果度量

[English](../../../en/token-saving/tokenless/measuring-savings.md)

Tokenless 记录自己处理的 Payload 在压缩前后的大小和估算 Token 数。它回答的是“压缩候选内容缩小了多少”，不是“模型请求或账单减少了多少”。

数据库和 CLI 把大小字段称为“字符”，但当前写入器实际保存 UTF-8 字节长度。统计中的 Token 数使用近似 `ceil(bytes / 4)` 规则，并没有调用模型 Tokenizer。二者都应只作为对比指标。

## 先理解统计范围

Tokenless 可以度量：

- Schema 压缩前后的大小。
- 工具/API 响应压缩前后的大小。
- TOON 编码前后的大小。
- 改写后的 RTK 命令真正执行时，RTK 过滤输出前后的大小。
- active 与 dry-run 模式。
- Session、Agent 和 Tool Use 标识。

Tokenless 不能直接度量：

- 模型生成的输出 Token。
- system prompt 和未经过 Tokenless 的对话历史。
- 提供商最终计费 Token。
- 压缩是否改变了任务质量。
- 追加型 Adapter 是否从最终模型请求中移除了原始结果。

因此上线评估应同时比较统计数据和任务结果质量。

## 查看累计摘要

```bash
tokenless stats summary
```

当前文本输出包含以下结构：

```text
Tokenless Statistics Summary
============================================================
Total Records: ...

Character Savings:
  Before: ...
  After:  ...
  Saved:  ...

Token Savings:
  Before: ...
  After:  ...
  Saved:  ...

Breakdown by Operation:
----------------------------------------
  compress-response: ...
```

输出中的 `Character Savings` 和 `Chars` 是上文说明的、基于字节的兼容字段名。

机器读取使用：

```bash
tokenless stats summary --json
```

摘要默认读取最近最多 10,000 条记录。可限制查询数量：

```bash
tokenless stats summary --limit 1000
```

`--limit` 必须为正整数。`--limit 0` 会在解析阶段以非零退出码被拒绝，行为与 `stats diff --limit` 一致。

## 节省率字段定义

Tokenless 统一把“节省率”表达为“节省 Token 数占原始未压缩 Token 数的比例”，仅统计口径不同。`tokenless stats` 输出的全部百分比字段定义如下：

| 字段 | 来源 | 计算公式 | 含义 |
|------|------|----------|------|
| `chars_saved_percent` | `stats summary --json` | (before_chars − after_chars) ÷ before_chars × 100% | Tokenless 经手 Payload 的字符级压缩率 |
| `tokens_saved_percent` | `stats summary --json` | (before_tokens − after_tokens) ÷ before_tokens × 100% | Tokenless 经手 Payload 的 Token 级压缩率 |
| `saved_percent` | `stats summary --compare --json` | (baseline_tokens − tokenless_tokens) ÷ baseline_tokens × 100% | 双跑对比中“开启压缩”一侧的节省率 |
| `saved_percent` | `stats diff --json`（每个 chain 和 stage） | (before_tokens − after_tokens) ÷ before_tokens × 100% | Diff 报告中单个 chain 或 stage 的节省率；使用该对象自身的 `before_tokens`、`after_tokens` 计算，而非 `--compare` 的运行级总量 |

`saved_percent` 在两种输出中公式相同、口径不同：`--compare` 的值按两次运行的总量（`baseline_tokens` 与 `tokenless_tokens`）计算；`stats diff --json` 则为每个 chain 和每个 stage 各输出一个 `saved_percent`，均按该对象自身的 `before_tokens`、`after_tokens` 计算。

文本输出中的 `Saved: N tokens (X%)` 对应 `tokens_saved_percent`：分母是同一批记录的 `before_tokens` 之和，即原始未压缩大小，既不是会话总消耗，也不是任何提供商侧缓存指标。也就是说，Tokenless 的节省率就是“节省 Token 数占原始未压缩总 Token 数的比例”这一定义：分子是 `before − after`（实际节省的 Token），分母是 `before`（原始未压缩 Token）。

> `tokenless-stats` **不输出** `savings_rate`、`cached_tokens`、`total_cached_tokens` 字段，也不采集模型提供商的提示词缓存（Prompt Cache）命中数据。如果在其他工具的报告里看到按 `cached_tokens ÷ total_tokens` 计算的 `savings_rate`，它反映的是提供商提示词缓存命中占比，不代表 Tokenless 的压缩节省，也不是 `tokenless-stats` 的输出。

## 查看单条记录

列出最近记录：

```bash
tokenless stats list
tokenless stats list --limit 50
```

输出中的 `[ID:<n>]` 是记录 ID。查看某次压缩的完整前后文本：

```bash
tokenless stats show <record-id>
```

解释该记录的估算节省和内容变化：

```bash
tokenless stats diff <record-id>
tokenless stats diff <record-id> -U 5
tokenless stats diff <record-id> --json
```

当两端都是合法 JSON 时，`diff` 会在展示前对对象 key 排序，因此只改变 key 顺序的差异不会显示；存储内容本身不会被修改。需要查看原始 Payload，或 diff 提示内容缺失、过大时，使用 `stats show`。

分析一个 Session 内可确认衔接的端到端阶段：

```bash
tokenless stats diff --session <session-id>
tokenless stats diff --session <session-id> --sort time
tokenless stats diff --session <session-id> \
  --tool-use-id <tool-use-id>
```

Session 总览只包含指标。tool-use 报告会显示内容差异；只有 session/tool-use ID 相同，并且上一阶段存储的输出与下一阶段输入完全一致时，连续的 active 阶段才会串联。断开的阶段、dry-run 记录及缺少 tool-use ID 的记录保持独立，避免重复计算中间输入。

对于 dry-run 记录，`after` 表示预测压缩大小，`emitted` 仍是原始 `before` 大小。无估算节省的操作不会入库，因此 Session 报告只覆盖节省记录。

> 本地统计包含完整工具文本。不要把 `stats show` 输出粘贴到公开 Issue、共享日志或不受信任的聊天中。详见[配置与数据隐私](configuration-and-privacy.md)。

## 为什么没有记录

以下情况不会新增统计记录：

- 压缩后估算 Token 数没有下降。
- `stats_enabled=false` 或 `TOKENLESS_STATS_ENABLED=0`。
- Adapter 没有启用或旧 Agent 会话尚未重启。
- Hook/Plugin 无法找到 `tokenless`。
- 输入没有经过 Tokenless 支持的 Hook。

追加型 Adapter 即使让宿主保留了原始结果，也可能产生统计记录。Codex 避免了这种歧义：宿主无法替换原始输出，因此其 PostToolUse Hook 不执行压缩，也不记录响应候选。Codex 的节省应通过 RTK 重写记录衡量。

先运行：

```bash
tokenless stats status
anolisa adapter status tokenless
```

继续参阅[启用后没有产生统计记录](troubleshooting.md#启用后没有产生统计记录)。

## 运行仓库参考负载

源码树提供了确定性的 fixture，用于比较不同 Tokenless 版本的压缩行为。在 Linux 上，
进入 ANOLISA 源码中的 `src/tokenless/benchmark/l1-compressor` 后运行：

```bash
cargo run --release --bin compression_rate -- --json
```

报告使用仓库内置的
`src/tokenless/benchmark/l1-compressor/fixtures/tool_response.json` 和
`src/tokenless/benchmark/l1-compressor/fixtures/schema_search.json`，并应用当前检出源码的
默认压缩配置。Tokenless 0.7.11 的参考结果如下：

| JSON 字段 | 独立测试阶段与输入 | 节省率 |
|---|---|---:|
| `canonical.response.savings_pct` | 对 canonical 响应执行响应压缩 | 65.8% |
| `canonical.schema.savings_pct` | 对 canonical Schema 执行 Schema 压缩 | 47.3% |
| `canonical.response.toon_only_savings_pct` | 对未压缩的 canonical 响应执行 TOON 编码 | 17.0% |
| `canonical.schema.toon_only_savings_pct` | 对未压缩的 canonical Schema 执行 TOON 编码 | -2.3% |

TOON 独立测试出现负数，表示编码后反而变大。Active 模式下，Runtime 会在候选结果
没有减少估算 Token 数时输出原始 JSON。

这是一组回归参考负载，不是承诺的生产压缩率范围。响应 fixture 是特意构造的、易于
压缩的合成数据；测试只使用一个响应和一个 Schema，并以近似 `ceil(bytes / 4)` 规则
估算 Token，而不调用模型 Tokenizer。测试也不包含 Adapter 行为以及工具数据在完整
会话中的占比。因此，这组结果只用于确认同一源码版本的行为是否相近；评估真实工作
负载时，应使用有代表性的自有 Payload，并执行下文的 dry-run 双跑。详细口径见
[benchmark 方法与限制](../../../../../src/tokenless/benchmark/l1-compressor/README.md#methodology)。

## 用 dry-run 做双跑对比

dry-run 会计算压缩结果和预测节省，但向调用方返回原文。要对同一输入做最小可重复对比：

```bash
TOKENLESS_COMPRESSION_ENABLED=0 \
  tokenless compress-response -f response.json \
  --session-id baseline-run

TOKENLESS_COMPRESSION_ENABLED=1 \
  tokenless compress-response -f response.json \
  --session-id active-run

tokenless stats summary --compare baseline-run active-run
```

机器读取：

```bash
tokenless stats summary \
  --compare baseline-run active-run \
  --json
```

注意：

- `--compare` 必须提供恰好两个 Session ID，顺序为 baseline、active。
- 任一 Session 没有记录时，命令以错误退出，而不是报告 0% 节省。
- `--limit` 必须为正整数。`--limit 0` 会在解析阶段被拒绝，而不会被误报为 Session 缺失。
- baseline 应为 dry-run，active 应为真实压缩；模式不匹配时 CLI 会告警。
- 对真实 Agent 任务做对比时，应尽量使用相同输入、工具版本和环境。
- dry-run 仍会把压缩前后文本写入本地统计数据库。
- dry-run 不创建 Stash 条目，也不会关闭 RTK 重写。RTK 写入的记录没有显式 mode，读取时按 active 处理，因此可能触发基线模式警告。

## 正确解释节省率

各百分比字段的定义见上文[节省率字段定义](#节省率字段定义)。`stats summary` 中的压缩率只针对 Tokenless 经手的 Payload。估算会话总体收益时，可以使用：

```text
总体估算节省率
= Tokenless Payload 压缩率 × 工具 Payload 占会话总 Token 的比例
```

例如，Payload 压缩率为 60%，但工具 Payload 只占会话总 Token 的 20%，则总体估算收益约为 12%。这个结果仍不是提供商账单保证值。

## AgentSight 本地展示

AgentSight 的 Token savings 页面可以只读聚合 `~/.tokenless/stats.db`。两者由同一用户运行，且 AgentSight 能访问该数据库时，不需要通过 SLS 才能看到本地 Tokenless 统计。

安装后可先确认：

```bash
test -r ~/.tokenless/stats.db
```

AgentSight 的安装和 Dashboard 使用方式见[AgentSight 用户指南](../../agent-observability/agentsight/README.md)。

## SLS JSONL

SLS 是独立的外部采集通道，不是 AgentSight 读取本地统计的前置条件。

默认行为：

- `sls_enabled=true`。
- 默认目标为 `/var/log/anolisa/sls/ops/tokenless.jsonl`。
- Tokenless 只在目标文件已经存在时追加；不存在时静默跳过。
- 文件由 ANOLISA SLS/Logtail 设施创建、轮转和删除。
- SLS 记录只包含度量与标识，不包含压缩前后的原文。
- 当 Agent 宿主或 Adapter 向 Tokenless 的运行环境注入 W3C `traceparent` 时，记录还会带上 `tokenless.trace_id` 和 `tokenless.span_id`，AgentLoop 这类可观测后端因此可以把节省量归因到产生它的 trace。该变量不会被自动写入 —— OpenTelemetry 只在进程内 carrier 中保存 active span —— 因此启动方没有注入可用 trace context 时这两个字段不会出现。
- 随包提供的 RTK 统计写入器只把 `rewrite-command` 记录写入本地 SQLite，不会调用 SLS Writer。

自定义测试文件：

```bash
touch /tmp/tokenless-sls.jsonl
TOKENLESS_SLS_ENABLED=1 \
TOKENLESS_SLS_PATH=/tmp/tokenless-sls.jsonl \
  tokenless compress-response -f response.json

tail -n 1 /tmp/tokenless-sls.jsonl | jq .
```

把一次运行与启动方所在的 trace 关联起来。示例直接在命令行写入该变量，宿主或 Adapter 在启动 Tokenless 前需要做的正是这件事：

```bash
touch /tmp/tokenless-sls.jsonl
TRACEPARENT=00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01 \
TOKENLESS_SLS_ENABLED=1 \
TOKENLESS_SLS_PATH=/tmp/tokenless-sls.jsonl \
  tokenless compress-response -f response.json

tail -n 1 /tmp/tokenless-sls.jsonl | jq '."tokenless.trace_id", ."tokenless.span_id"'
```

`TOKENLESS_SLS_PATH` 必须位于 `/var/log/` 或 `/tmp/` 下。生产 SLS endpoint、认证和 Logtail 配置属于平台运维配置，不在 Tokenless 用户指南中展开。

## 清理统计

先确认不再需要历史对比：

```bash
tokenless stats clear --yes
```

这会清空统计记录，但不会禁用后续记录。停止新增记录：

```bash
tokenless stats disable
```

`stats disable` 只关闭本地 SQLite 统计，不会关闭 SLS。完整开关关系见[配置与数据隐私](configuration-and-privacy.md)。

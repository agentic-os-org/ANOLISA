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

`stats summary` 中的压缩率只针对 Tokenless 经手的 Payload。估算会话总体收益时，可以使用：

```text
总体估算节省率
= Tokenless Payload 压缩率 × 工具 Payload 占会话总 Token 的比例
```

例如，Payload 压缩率为 60%，但工具 Payload 只占会话总 Token 的 20%，则总体估算收益约为 12%。这个结果仍不是提供商账单保证值。

## 压缩率的适用场景

Tokenless 的压缩率取决于 Payload 中可精简成分的多少，不同场景差异很大。使用下文的标准测试负载测得的参考值（测量于 commit `2e7d69f1`，升级后请在本地重新运行确认）：

| 场景 | 参考节省率（估算 Token） | 说明 |
|------|--------------------------|------|
| 结构化 JSON 响应（统一记录 + 冗余字段），单路响应压缩 | 约 66% | 黑名单字段、空值被移除，超长字符串/数组被截断 |
| 函数调用 Schema（长描述），单路 Schema 压缩 | 约 47% | 描述截断，移除 `title`/`examples` 与代码块 |
| 混合负载（响应 + Schema）：仅响应压缩 | 约 62% | 响应是主要收益来源 |
| 混合负载：Schema + 响应压缩叠加 | 约 65% | 两类 Payload 同时精简 |
| 混合负载：全栈叠加（Schema + 响应 + TOON，部署门控） | 约 65% | 部署门控生效，结果与上一行相同（见表后说明） |
| 仅 TOON 编码 | 约 16% | 表格化、规整的 JSON 才有明显收益 |

> 全栈叠加（部署门控）：部署路径的 `compress-toon` 尺寸保护会在 TOON 不能减少估算 Token 时保留原输入；本负载下响应/Schema 压缩后再做 TOON 反而膨胀，因此部署后的节省率与「Schema + 响应压缩叠加」行相同。基准报告中未加门控的 `full_stack` 组合测得约 63%，但部署路径不会输出该结果。

按场景归纳：

- **收益高**：工具返回大量统一结构的记录（列表、表格、搜索结果），或携带 `debug`/`trace`/`logs` 等冗余字段，或 Schema 描述冗长。
- **收益中等**：Shell 输出中超过 Layer 2 阈值（字符串 65,536 字符、数组 128 项、深度 8）的部分会被截断；未超阈值的输出基本保持原样。
- **收益接近零**：短于最小触发长度的响应（共享 Adapter 200 字符、Codex 500 字符）；本身已经紧凑、无冗余的 JSON；任何压缩后不比原文更小的输入（尺寸保护会保留原文）。
- **不参与压缩**：内容读取类工具（Read/Glob/Grep 等）的输出、非 JSON 文本、带 YAML frontmatter 的 Skill 文本。触发条件详见[用户手册 · 压缩的触发条件与阈值](user-manual.md#压缩的触发条件与阈值)。

实际会话收益还要乘以工具 Payload 在会话总 Token 中的占比，见上文[正确解释节省率](#正确解释节省率)。

## 标准测试负载

仓库内置了确定性的标准测试负载，位于 `src/tokenless/benchmark/l1-compressor`（独立 Cargo workspace；仅支持 Linux，不支持 macOS/Windows）。负载由 `python/gen_fixtures.py` 生成，不含随机数，字节级可复现，并已提交在仓库中：

| 负载文件 | 内容 |
|----------|------|
| `fixtures/records.json` | 1,000 条统一结构记录 |
| `fixtures/tool_response.json` | 典型工具响应（外层信封 + 60 条记录 + `trace`/`logs` 冗余字段） |
| `fixtures/schema_search.json` | 典型函数调用 Schema |

快速运行压缩率报告（需要先构建，输出包括单路压缩率、各压缩组合的叠加结果与成本估算）：

```bash
cd src/tokenless/benchmark/l1-compressor
cargo run --release --bin compression_rate            # 人类可读报告
cargo run --release --bin compression_rate -- --json  # 机器可读 JSON；二进制参数必须放在 `--` 之后
```

运行完整质量/对抗测试 + 压缩率报告（跳过 criterion 性能基准，耗时数分钟）：

```bash
cd src/tokenless/benchmark/l1-compressor
./run-benchmarks.sh --quick
```

使用标准负载时请注意：

- Token 数使用字节/4 启发式估算，适合版本间相对比较，绝对值不代表真实计费 Token。
- 压缩率随版本演进可能变化，引用数字时请注明对应的 commit 或版本号。
- 标准负载用于横向对比，不代表你的业务数据；评估真实收益仍应使用[双跑对比](#用-dry-run-做双跑对比)在自己的工作负载上测量。

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

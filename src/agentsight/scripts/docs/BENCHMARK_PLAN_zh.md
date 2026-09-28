# AgentSight 压测与稳定性验证计划

[English](BENCHMARK_PLAN.md)

本文定义 AgentSight 内存治理、持续高负载稳定性、异常输入容错和资源清理的
验证方案。最终报告必须回答：相同 QPS 下资源占用降低了多少、最大可持续 QPS
提高了多少、采集完整性是否回退，以及长时间运行和异常输入下是否仍然稳定。

本地编排与报告生成工具已经补齐；正式 before/after 数据仍需在冻结配置的 Linux
环境中，使用选定的两个 AgentSight 构建版本按本文执行多小时测试后才能获得。报告
会记录实际架构，结论只适用于该环境。Campaign runner 将两个二进制和原始配置视为
不可变输入，并在运行前后校验 SHA-256。

## 审阅摘要

### 项目主线

本项目以**内存治理**为主要目标，以吞吐、延迟、丢弃率和 Trace 完整率作为
约束条件，并补充稳定性、异常输入和资源泄漏验证。核心不是单纯追求更高 QPS，
而是证明 AgentSight 在持续高负载下内存有明确上界，同时不因限流、背压或淘汰
机制损失采集完整性。

最终报告必须直接回答以下五个问题：

1. 在相同 QPS 下，optimized 相比 baseline 的 RSS 降低了多少；
2. baseline 和 optimized 分别最多能够稳定处理多少 QPS；
3. 连续运行 4 小时后，RSS、FD、线程和 Socket 是否持续增长；
4. 内存优化是否导致 CPU、P99 延迟、丢弃率或 Trace 完整率退化；
5. 发生过载或收到异常输入时，系统是否会 OOM、panic、退出或无法恢复。

### 核心实验和证据

| 要回答的问题 | 核心测试 | 主要指标 | 对比方式 |
| --- | --- | --- | --- |
| 内存降低多少 | 共同上限的 20%/40%/60%/80%/100% QPS 矩阵 | RSS Avg/P99/Max、RSS slope | 三次重复 before/after |
| 最多能扛多少流量 | 倍增升压、区间细化和正式确认 | 最大可持续 QPS、Effective QPS、P99 | 三次重复 before/after |
| 内存是否有上界 | 同一高 QPS 的 4 小时长稳 | RSS 峰值/斜率；可导出时记录队列/缓存字节 | 同 QPS before/after |
| 是否存在资源泄漏 | 4 小时长稳和连接 churn | FD、线程、Socket 首末差值和斜率 | 同 QPS before/after |
| 过载后能否恢复 | 稳定 10 分钟→过载 5 分钟→恢复 15 分钟 | RSS/P99 恢复；可导出时记录队列恢复 | 三次重复 before/after |
| 异常输入是否安全 | 正常流量中注入畸形、截断和超大输入 | panic/OOM、正常请求成功率、资源回落 | baseline 失败证据与 optimized 通过结果 |
| 采集质量是否回退 | 所有性能与稳定性测试 | 丢弃率、Trace 完整率、Token 正确率 | before/after 门禁 |

先分别测出 baseline 和 optimized 的最大可持续 QPS，再取两者较低值作为共同
上限。相同 QPS 主矩阵使用共同上限的 20%、40%、60%、80% 和 100%，按两个版本
× 五档 QPS × 三次重复执行。每次预热 3 分钟、正式测量 15 分钟，共 30 次正式
运行，至少需要 9 小时。之后再执行两个版本各一次 4 小时长稳和各三次过载恢复。

### 主要测试工具

- k6：生成 HTTP/1.1 SSE/JSON 定 QPS 负载并统计吞吐、成功率和延迟；
- h2load：补充 HTTP/2 吞吐和延迟结果；
- mock LLM server：提供可重复的 HTTPS/SSE/JSON 响应；
- `single_run/collect_metrics.py` + Linux `/proc`：采集 CPU、RSS、线程、FD 和 Socket；
- SQLite validator：按 request ID 对账 Trace 完整率、端到端采集丢失率和 Token 正确率；
- AgentSight 内部指标：通过原子快照导出真实队列、缓存字节、完成数、淘汰数和
  丢弃计数；
- Cargo test、pytest、coverage.py、diff-cover：执行回归测试和覆盖率门禁；
- Campaign runner、Aggregate reporter、Fault injector：完成自动编排、严格证据
  审计、before/after 汇总和运行态异常输入注入。

### 实验约束与门槛确定方式

1. 正式测试使用满足内核 BTF 和工具要求的 Linux 主机，所有结论限定在
   该冻结实验环境；
2. 容量测试先执行短预试验，从默认 10 QPS 倍增定位首个失败区间；正式搜索从
   估算值的 80% 开始并按预设精度二分细化，1000 QPS 不是测试上限；
3. baseline 和 optimized 分别执行一次 4 小时长稳测试，用于证明在该观测窗口
   内内存和资源保持有界；
4. Trace 完整率最低门槛固定为 99.9%；
5. 正式测试前先使用未优化版 AgentSight 执行独立预试验，根据实际硬件资源、
   基线表现和项目目标确定最大 P99、最大丢弃率、最大 RSS/RSS 斜率、FD/线程/
   Socket 斜率和最长恢复时间。预试验结果不进入正式 before/after 报告；门槛写入
   `campaign.json` 并冻结后，再重新执行正式 baseline 和 optimized 测试。

本文的正式 `drop_rate` 门禁取可观测值中的最差值：
`max(internal_drop_rate, 1 - trace_completeness)`。原始二进制没有内部计数器时，
request ID 精确对账仍能提供端到端采集丢失率；缺失计数器不会被填成 0。

## 1. 验证目标

| 编号 | 必须证明的结论 | 主要证据 |
| --- | --- | --- |
| G1 | 相同负载下内存开销降低 | 五档 QPS 的 RSS before/after |
| G2 | 持续高负载下内存有上界 | 4 小时 RSS 峰值、斜率和滚动窗口趋势 |
| G3 | 系统可承载流量提高或不回退 | 最大可持续 QPS、有效 QPS 和 P99 延迟 |
| G4 | 内存治理没有牺牲采集质量 | 丢弃率、Trace 完整率和 Token 正确率 |
| G5 | 过载后能够恢复 | RSS 和延迟恢复时间；可用时记录队列和连接诊断项 |
| G6 | 异常输入不会拖垮管线 | 无 panic/OOM，正常流量继续成功 |
| G7 | 长时间运行没有资源泄漏 | FD、线程、Socket 数量及其增长斜率 |

## 2. 测试工具

### 2.1 现有工具

| 工具 | 用途 | 产物 |
| --- | --- | --- |
| `single_run/load/k6.js` + k6 | HTTP/1.1 SSE/JSON 定 QPS 负载、请求 ID、延迟和成功率 | `k6.jsonl.gz`、`k6.log` |
| `single_run/load/h2load.sh` + h2load | HTTP/2 补充吞吐和延迟测试 | `h2load.txt` |
| `single_run/mock_llm_server.py` | 提供确定性的 HTTPS SSE/JSON/HTTP/2 LLM 响应 | `mock-server.log` |
| `single_run/collect_metrics.py` + Linux `/proc` | 每秒采集 AgentSight CPU、RSS、线程、FD 和 Socket | `metrics.csv` |
| AgentSight Prometheus 指标快照 | 当前二进制设置 `AGENTSIGHT_METRICS_FILE` 后导出真实连接缓存、队列、阶段耗时、完成、淘汰和丢弃指标 | 合并进 `metrics.csv`；原始快照保留带标签的阶段指标 |
| `single_run/validate_results.py` + Python `sqlite3` | 按 request ID 对账 SQLite，计算完整率和 Token 正确率 | `report.json` |
| `single_run/render_report.py` | 把单次 JSONL/CSV 转成人工可读 Markdown | `benchmark-report.md` |
| `single_run/run.sh` | 执行单次负载、资源采集、校验和报告 | 单次运行目录 |
| Cargo test | 内存预算、解析容错和资源释放的 Rust 回归测试 | 测试日志 |
| pytest | benchmark 脚本的行为和统计公式测试 | 测试日志 |
| coverage.py + diff-cover | 验证新增 Python 行的增量覆盖率至少为 85% | coverage XML、diff coverage 结果 |

`openssl` 用于生成临时测试证书，`curl` 用于 mock 健康检查；`git`、`sha256sum`、
`uname` 和 `lscpu` 用于记录版本、二进制校验和及主机信息。CPU/RSS 的正式数据
来自 `single_run/collect_metrics.py`，`pidstat` 只可作为人工交叉检查，不能混入主统计口径。

### 2.2 Campaign 自动化工具

| 工具 | 已实现能力 |
| --- | --- |
| `campaign/reproduce_campaign.sh` | 从两个 Git ref 创建临时 worktree，以相同工具链构建并冻结输入，然后一键执行 campaign |
| `campaign/run_campaign.py` | 预检输入、生成仅发现 mock server 的隔离运行配置副本、每次启动一个不可变二进制、断点续跑并把全部产物集中到一个目录 |
| `campaign/campaign.py` | 编排 baseline/optimized、自适应 QPS、预热、重复、长稳、恢复和异常输入阶段 |
| `campaign/aggregate_report.py` | 生成 median/min/max、before/after delta、CSV/Markdown 和严格证据判定 |
| `single_run/fault_injector.py` | 通过 Python `ssl`/`socket` 发送畸形、截断、二进制、超大输入和 TLS churn |
| `campaign/campaign_manifest.py` | 在运行前冻结门槛、配置、二进制、日志/指标路径和主机元数据 |

优先扩展现有 `single_run/run.sh`、`single_run/render_report.py` 和测试模块；只有职责无法合理容纳时才
新增脚本。Fault injector 的 HTTP/2 非法帧先由 Rust fixture 回归测试覆盖，不能
把普通 h2load 流量误写成异常 HTTP/2 测试。

## 3. 哪些内容做 before/after

| 测试 | 对比方式 | 原因 |
| --- | --- | --- |
| 相同 QPS 性能矩阵 | 完整 before/after，三次重复 | 量化相同流量下资源和性能变化 |
| 最大可持续 QPS | 完整 before/after | 量化稳定承载能力变化 |
| 4 小时长稳 | 同 QPS before/after | 证明内存和资源不会持续增长 |
| 过载恢复 | 完整 before/after，三次重复 | 比较恢复速度和峰值 |
| 运行态异常输入 | 记录 baseline/optimized 结果，不强求性能百分比 | 重点是从崩溃或泄漏变为安全降级 |
| Rust/Python 回归测试 | optimized 必须通过；可记录 baseline 失败证据 | 证明具体缺陷不可复现，不用于性能结论 |

可选增加 AgentSight 关闭的 control 运行，用于估算观测本身的开销。control 不能
替代 baseline，也不能参与“优化了多少”的主要 delta。

## 4. 统一测试条件

- 主结果必须来自同一台冻结的 Linux 物理主机并记录实际架构，测试结论只适用于
  该实验环境，不能外推到其他架构或不同硬件；
- baseline 和 optimized 固定 CPU/内存配额、内核、AgentSight 配置、数据库配置、
  payload、SSE chunk 和 mock 延迟；
- 两个版本尽量交替运行，并在运行间冷却，降低温度和时段偏差；
- 物理主机休眠后先重启，并在 campaign 期间关闭自动休眠，保证 BPF 与用户态时间戳
  使用的时钟域保持对齐；
- 记录 git commit、二进制 checksum、配置 checksum、CPU、内存、内核、cgroup、
  CPU governor、工具版本以及 UTC 开始/结束时间；
- 每个正式测试窗口前单独预热，预热样本不得进入正式统计；
- 采集完整率和内部指标门禁只判断正式测量，避免过载预热中断容量边界搜索；预热和
  正式测量均保持磁盘、内存及 RSS 安全保护；
- baseline 崩溃时仍保留已有产物，不能删除失败运行后只报告成功样本；
- 数值门槛可通过独立 baseline 预试验校准，但必须在正式 baseline/optimized
  campaign 前写入 `campaign.json` 并冻结，不能根据正式结果反向调整。

## 5. 测试场景

### 5.1 Smoke 测试

baseline 和 optimized 各执行一次短运行，验证 mock、AgentSight、负载生成器、
采集器、SQLite 校验和报告链路能够启动并退出，默认负载为 10 QPS、持续 30 秒。
Smoke 只验证 harness，不进入性能结论。一键 runner 会在私有 mount namespace 中为
两个版本分别提供空 SQLite 目录，避免宿主机旧数据或另一个版本影响检查。

### 5.2 最大可持续 QPS

对 baseline 和 optimized 分别执行独立的自适应容量搜索，不设置 1000 QPS
硬上限：

1. **自动预试验**：从 `qps_start` 开始，默认 10 QPS；通过后倍增，失败后减半，
   以短测试自动取得大致的通过/失败区间，区间中点作为容量估算值；
2. **从 80% 开始**：将估算值的 80% 按 `qps_resolution` 向下取整，默认精度为
   1 QPS，然后建立正式通过下界和失败上界；若预试验低估本机能力则自动向上扩展；
   若第一个正式档位失败，先正式验证预试验最后通过档位，再继续向下降档；
3. **二分搜索**：反复二分正式通过/失败区间，直到两个边界只相差一个
   `qps_resolution`；
4. **正式确认**：候选最高通过档位及与其相邻的首个失败档位均正式测量 15 分钟，
   并重复三次；若候选正式确认失败，则利用此前通过的搜索档位建立新区间并自适应二分，
   不逐个 QPS 向下扫描，也不再确认已经失败下界之上的档位；
5. **结果判定**：某档至少两次重复通过全部门槛才算通过，最高通过档位就是该版本
   的最大可持续 QPS。非单调结果需要排查主机干扰并使用新 ID 重跑。

runner 会在每个容量探针前重启 AgentSight，同时保留该版本隔离的 SQLite 目录，避免较高
QPS 探针留下的内存积压污染随后的降档结果。若最低搜索精度对应的正式探针仍失败，阶段会
记录未确认容量并直接停止，不再执行没有意义的重复确认。

自动预试验默认预热 30 秒、测量 60 秒，正式二分探针使用 3 分钟预热加 5 分钟测量。
RSS 斜率不随 QPS 单调变化，短窗口也无法区分分配器波动和长期泄漏，因此全部容量探针
和重复确认都将该门槛延期到 4 小时长稳测试；其他冻结的容量门槛仍然生效。两类搜索
探针都不能直接作为最大可持续 QPS 结论。正式确认运行必须同时满足：

- `effective_qps / input_qps` 达到预设最低比例；
- HTTP 成功率、Trace 完整率和 Token 正确率达到预设门槛；
- P99 延迟、丢弃率和 RSS 上限不超过预设门槛；
- 无 OOM、panic、意外退出或数据库写入错误。

两个版本的 4 小时同 QPS 长稳测试负责判断冻结的 RSS 斜率门槛，并且仍是正式 campaign
总体结论的必要条件。

完整率门槛为 99.9%，`min_token_accuracy` 为 1.0。`min_throughput_ratio`、
`max_p99_ms`、`max_drop_rate`、
`max_rss_mb` 和 `max_rss_slope_mb_per_hour` 通过独立预试验校准，并在正式
campaign 执行前填写和冻结；缺少任一门槛时只能报告观测值，不能宣称最大可持续
QPS。这里用于校准阈值的独立预试验，与上述脚本自动执行的容量预试验不是同一件事。

### 5.3 相同 QPS 性能矩阵

容量确认结束后，先计算：

```text
common_max_qps = min(baseline_max_qps, optimized_max_qps)
```

两个版本使用完全相同的五档绝对 QPS。各档取共同上限的 20%、40%、60%、80%
和 100%，按 `qps_resolution` 向下取整；取整后必须保持五档互不重复。runner 将
最终绝对数值写入 `campaign-resolution.json`，避免改写已经冻结的 `campaign.json`。

| 维度 | 要求 |
| --- | --- |
| 版本 | baseline、optimized |
| QPS | `common_max_qps` 的 20%、40%、60%、80%、100% |
| 重复 | 每个版本/QPS 三次 |
| 预热 | 3 分钟，不计入统计 |
| 正式测量 | 15 分钟 |
| 资源采样 | 每秒一次 |
| 主协议 | HTTP/1.1 SSE |
| 补充协议 | JSON、HTTP/2 单独报告 |

该矩阵共 30 次正式运行，含预热至少 9 小时。每个 QPS 必须比较：

- 输入 QPS、有效 QPS、成功完成 QPS；
- CPU 平均值/P95/P99/最大值；
- RSS 平均值/P95/P99/最大值、首值、末值和斜率；
- 延迟 P50/P95/P99/最大值；
- HTTP 错误率、超时率、ring-buffer/channel 丢弃率；
- Trace match rate、Trace 完整率和 Token 正确率。

### 5.4 4 小时长稳测试

容量测试完成后，使用 `common_max_qps` 的 80% 作为两个版本相同的长稳负载。
baseline 和 optimized 各执行一次 10 分钟预热加 4 小时正式测量；主机受干扰或
结果异常时以新 ID 重跑。

记录 RSS 首值/末值/平均值/P99/峰值、RSS 每小时线性斜率、五分钟滚动最大增幅、
FD/线程/Socket 的首末差值和每小时斜率、端到端采集丢失率、完整率，以及
OOM/panic/退出次数。通过原子运行时快照记录队列/缓存字节、淘汰数和内核/用户态
丢弃数；不支持导出器的历史 baseline 保持字段缺失，不能用 0 替代。

通过条件是进程存活、RSS 不超过预设上限、RSS/FD/线程/Socket 斜率均不超过
campaign 门槛、采集完整率不回退。该结果证明“在四小时观测窗口内有界”，不能
外推为无限时间的数学证明。

### 5.5 过载与恢复测试

每个版本执行三次以下阶段：

1. 10 分钟稳定阶段：使用长稳测试 QPS；
2. 5 分钟过载阶段：两个版本都使用较低容量版本的首个失败档位；
3. 15 分钟恢复阶段：恢复到稳定 QPS。

比较 RSS 峰值、稳定吞吐恢复时间、P99 恢复时间、RSS 回落时间和恢复阶段 Trace
完整率。被测二进制能够导出真实队列/连接指标时，再报告其峰值和排空时间。
恢复判定窗口、允许偏差和最长恢复时间在
campaign 中预先声明；默认建议要求恢复指标连续五分钟回到稳定阶段的 10% 范围内。

### 5.6 运行态异常输入

在正常流量持续运行时，Fault injector 分别注入：

- 非法 JSON、非法 chunk size 和不一致的 Content-Length；
- 截断 HTTP body、截断 SSE、非法 UTF-8 和二进制 body；
- 超过 HTTP body/SSE continuation 限制的输入；
- TLS 建连后立即断开、连接重复建立和关闭。

记录每类输入数量、被拒绝/安全降级数量、panic/OOM/退出次数、异常前后正常请求
成功率、Trace 完整率，以及 RSS/FD/Socket 是否恢复。optimized 必须不崩溃且正常
流量持续工作；baseline 只用于保留问题证据，不要求计算性能改善百分比。

### 5.7 代码级回归测试

| 目标 | 必须覆盖的测试 |
| --- | --- |
| 内存有界 | event-channel 字节预算、超大事件拒绝、pending-GenAI 数量/字节淘汰、HTTP body/SSE continuation 上限 |
| 异常输入 | 非法 JSON/chunked body、二进制、非法 UTF-8、截断 SSE/HTTP/2 fixture 不得 panic |
| 资源清理 | reservation release/refund、并发 drain 无 phantom reservation、idle 连接淘汰清理 side map、子进程和 FD 清理 |
| 报告正确性 | P99、QPS 分组、计数器 reset、丢弃率分母、斜率、恢复时间、缺失值和 delta |

每个代码变更必须增加或更新测试；提交前执行适用的 Cargo/pytest 检查，并使用
`diff-cover --fail-under=85` 验证增量覆盖率。

## 6. 指标定义

```text
effective_qps = completed_load_requests / measured_seconds
http_success_rate = successful_http_requests / sent_requests
trace_match_rate = matched_request_ids / sent_request_ids
trace_completeness = complete_captured_calls / sent_request_ids
drop_rate = (ring_buffer_delta + channel_delta)
            / (completed_delta + ring_buffer_delta + channel_delta)
delta_pct = (optimized_median - baseline_median)
            / baseline_median * 100
```

RSS、FD、线程和 Socket 斜率使用正式窗口内样本对时间的一元线性回归。累计计数器
按窗口增量计算并容忍 reset；分母为零或字段缺失时报告 `—`，不能当作零或通过。
所有百分位统一使用 nearest-rank。固定矩阵的三次重复按 median/min/max 汇总。

CPU、RSS、延迟、错误和丢弃越低越好，负 delta 表示改善；吞吐和完整率越高越好，
正 delta 表示改善。baseline median 为零时只报告绝对差。

## 7. 优化手段与证据对应关系

| 优化手段 | 主要指标 | 必须同时检查的副作用 |
| --- | --- | --- |
| event-channel 字节预算与背压 | RSS 峰值、RSS 斜率、队列字节 | 丢弃率、P99、有效 QPS |
| pending 连接数量/字节/TTL 限制 | RSS、缓存字节、淘汰数 | Trace match/完整率 |
| HTTP body/SSE continuation 上限 | 超大输入内存峰值、拒绝结果 | 正常大请求完整率 |
| 资源释放和 idle 淘汰 | FD/Socket/线程斜率、恢复时间 | 连接误淘汰、关联失败 |
| 异常输入安全降级 | panic/OOM 数、正常请求成功率 | CPU、错误日志量 |

最终报告必须按此表说明“采用了什么手段、改善了哪个指标、改善多少、有什么取舍”，
不能只给一个总的内存百分比。

## 8. 报告和产物

每次运行保留 manifest、配置、日志、`metrics.csv`、负载原始输出、SQLite 校验和
单次 Markdown 报告。campaign 至少生成：

- `performance-comparison.md/.csv`：五档 QPS 的 before/after；
- `capacity-report.md`：最大可持续 QPS 及失败门槛；
- `soak-report.md`：4 小时内存与泄漏趋势；
- `recovery-report.md`：过载峰值和恢复时间；
- `fault-report.md`：每类异常输入和正常流量影响；
- `regression-report.md`：Cargo、pytest 和 diff-cover 结果；
- `final-summary.json`：机器可读的 campaign 判定和问题列表；
- `final-report.md`：优化手段、量化收益、完整性、限制和结论。

核心性能表格式为：

| QPS | 版本 | Effective QPS | CPU Avg/P95 | RSS Avg/P99/Max | RSS slope | Latency P99 | Drop rate | Trace completeness |
| ---: | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 500 | baseline/optimized | … | … | … | … | … | … | … |

原始 JSONL/CSV 是不可变输入，Markdown/汇总 CSV 是可重建输出。失败运行和缺失
指标必须显示原因，不能覆盖或静默排除。

## 9. 实施和执行顺序

1. 完成 matrix runner、正式窗口、manifest 和阶段编排；
2. 完成 procfs 进程资源斜率字段，以及真实 AgentSight 内部计数器采集；
3. 完成 aggregate reporter、P99、delta、恢复时间和各类报告；
4. 完成 Fault injector 及其自动化测试；
5. 运行 Rust/Python 回归和双版本 Smoke；
6. 冻结主机、配置、测试门槛和工具版本；
7. 自适应搜索并正式确认两个版本的最大可持续 QPS；
8. 根据 `common_max_qps` 执行 30 次相同 QPS 测试并生成性能对比；
9. 依次执行 4 小时长稳、过载恢复和运行态异常输入；
10. 生成最终 Markdown/CSV，人工复核异常值和原始产物。

## 10. 完成定义

Harness 已覆盖步骤 1–4；只有在 Linux 环境继续执行正式 campaign，并满足以下条件，
项目测试交付才算完成：

- 两个版本的最大可持续 QPS 均通过倍增定位、区间细化和三次正式确认；
- 基于 `common_max_qps` 生成的五档相同 QPS，其 baseline/optimized 三次重复均有
  可追溯产物；
- 报告给出各 QPS 的 CPU、RSS、有效 QPS、P99、丢弃率和 Trace 完整率；
- 报告给出两个版本的最大可持续 QPS 及每项失败门槛；
- 两个版本在相同 QPS 下完成 4 小时长稳和三次过载恢复测试；
- 异常输入在正常流量中执行，optimized 全程无 OOM/panic/意外退出；
- FD、线程、Socket 和 RSS 都有首末值、峰值、斜率及预设门槛结论；
- 内存、吞吐、延迟、丢弃率和完整性的 before/after delta 可重新生成；
- 每项优化手段都能关联到量化收益和副作用；
- 回归测试全部通过，新增代码的 diff coverage 不低于 85%；
- 报告明确环境、原始产物、失败运行、限制和复现方式。

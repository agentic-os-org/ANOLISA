# plot_metrics.py 使用说明

> 本文档目前只有中文版。

`plot_metrics.py` 把 benchmark 生成的 `metrics.csv` 渲染成一份自包含的 HTML 评估报告。
脚本只读取输入文件，不会修改它们；输出是单个 HTML 文件，内嵌 SVG 图表、悬浮读数、汇总表和主题切换，
不依赖任何第三方 Python 包。

适合在 `run.sh` 或 `resource_run.sh` 跑完后，快速查看 AgentSight 的内存、背压、CPU、事件丢失和采集完整性。
`resource_run.sh` 收尾时会自动调用本脚本，按运行目录生成报告；手动运行主要用于已有产物或需要重新渲染的场景。

## 快速开始

在 `src/agentsight` 目录执行：

```bash
python3 scripts/benchmark/single_run/plot_metrics.py \
  /tmp/agentsight-resource-single/run/metrics.csv
```

脚本会自动读取 CSV 同目录下的 `run-summary.json` 和 `report.json`（存在时），
把报告写到 `<CSV 目录>/metrics-report.html`，并在标准输出打印该路径。用浏览器直接打开即可，无需启动服务。

配合 `resource_run.sh` 的完整流程：

```bash
bash scripts/benchmark/single_run/resource_run.sh \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 300 \
  --output-dir /tmp/agentsight-resource-single

# 收尾时已自动生成 /tmp/agentsight-resource-single/run/metrics-report.html；
# 下面这步仅用于重新渲染：
python3 scripts/benchmark/single_run/plot_metrics.py \
  /tmp/agentsight-resource-single/run/metrics.csv
```

## 输入文件

| 文件 | 是否必需 | 说明 |
| --- | --- | --- |
| `metrics.csv` | 必需 | 由 `collect_metrics.py` 采样生成；`run.sh` 启用资源采集时输出到运行目录 |
| `run-summary.json` | 可选 | 运行汇总；提供测量时长、进程存活状态、完整率、Token 准确率、QPS/TPS、延迟和资源斜率 |
| `report.json` | 可选 | 采集校验结果；提供完整率、有效完整率和 Token 准确率 |
| `config.json` | 可选 | 本次运行的 `runtime_limits` / storage 配置；存在时渲染“运行限制”折叠表 |
| `worker-result.json` | 可选 | 进程状态、退出码和采集进程存活信息，用于稳定性区 |
| `safety-stop.json` | 可选 | 资源 guard 是否触发 |
| `fault-results.json` | 可选 | fault_agent / fault_injector 的结果；不存在时 Fault 区显示“未运行” |
| `trace.log` / `runner.log` | 可选 | 扫描 panic、abort 和 OOM 日志 |

`--run-summary` 和 `--report-json` 不传时，脚本只在 CSV 同目录查找同名文件；文件不存在或内容无效时按缺失处理，不会报错。

## 参数

| 参数 | 默认值 | 说明 |
| --- | --- | --- |
| `metrics` | 必填 | `metrics.csv` 路径 |
| `--run-summary` | 自动探测 | `run-summary.json` 路径 |
| `--report-json` | 自动探测 | `report.json` 路径 |
| `--output` | `<CSV 目录>/metrics-report.html` | 输出 HTML 路径；父目录不存在时自动创建 |
| `--title` | `AgentSight benchmark · <CSV 目录名>` | 报告标题 |
| `--rss-target-mb` | `350` | RSS 验收目标线，单位为 MiB |
| `--max-rss-mb` | 兼容别名 | 仅在未传 `--rss-target-mb` 时作为目标值；建议改用 `--rss-target-mb` |
| `--warmup-seconds` | `30` | 计算 RSS 斜率前跳过的启动时间；4h 长稳建议 300–600 |
| `--slope-window-seconds` | `15` | RSS 滚动斜率窗口；4h 长稳建议 1800+ |
| `--connection-capacity` | 不画参考线 | 在 In-flight connections 图上画连接容量上限 |
| `--max-points` | `700` | 每条曲线保留的 SVG 点数；超出时降采样 |

参考线只是视觉标记，不参与 PASS/FAIL 判定。`--connection-capacity` 的取值应与被测运行的实际配置一致；
可在运行产物的 `settings.json` 中找到。全窗口 RSS 斜率不再展示，报告只保留 warmup 后的拟合斜率。

完整参数示例：

```bash
python3 scripts/benchmark/single_run/plot_metrics.py \
  /tmp/agentsight-resource-single/run/metrics.csv \
  --run-summary /tmp/agentsight-resource-single/run/run-summary.json \
  --report-json /tmp/agentsight-resource-single/run/report.json \
  --rss-target-mb 350 \
  --warmup-seconds 30 \
  --slope-window-seconds 15 \
  --connection-capacity 128 \
  --output /tmp/agentsight-resource-single/metrics-report.html \
  --title "AgentSight resource run"
```

## 报告内容

### KPI 卡片

| 指标 | 来源与含义 |
| --- | --- |
| Window | `run-summary.json` 的 `measured_seconds`，缺失时用最后一个相对时间；同时显示样本数 |
| Peak RSS | RSS 峰值，以及首样本到末样本的变化 |
| RSS slope | RSS 对时间的最小二乘斜率，单位 MiB/h；正值表示仍在增长 |
| Peak CPU | CPU 峰值和平均值 |
| Peak in-flight | `pending_connection_count` 峰值 |
| Evictions | `eviction_count` 在窗口内的增量；大于 0 标为警告 |
| Drops | `ring_buffer_dropped`、`channel_dropped`、`ffi_dropped` 的增量之和；大于 0 标为严重 |
| Process | 进程是否存活。优先用 `run-summary.json` 的 `process_survived`，否则用 `process_alive` 列；存在 `safety-stop.json` 时判为未存活 |
| Effective QPS / TPS | `run-summary.json` 的 `effective_qps` 和 `tokens_per_second` |
| Latency P50/P95/P99 | `run-summary.json` 的 `latency_ms` |
| RSS average / warmup slope | RSS 平均值，以及跳过 `--warmup-seconds` 后的线性斜率 |
| CPU average | `cpu_pct` 平均值和 P50/P95 |
| Effective completeness | 优先 `report.json` 的 `admitted_completeness`，否则按 `complete/(sent-rejected)` 计算 |
| Drop rate | `run-summary.json` 的 `drop_rate` 和 `capture_loss_rate` |
| Stability | 扫描日志/`worker-result.json`/`safety-stop.json`，展示 Panic、OOM、重启、exit code、cgroup OOM |

资源门禁只检查平均 CPU（25%）与平均/峰值 RSS（265/300 MiB）；P95/峰值 CPU 为诊断指标，不参与门禁判定。

`safety-stop.json` 会检查 CSV 所在目录及其上一级目录。资源安全保护停止负载时，`resource_run.sh` 会把该文件写在单次运行的 `run/` 目录下。

### 图表分区

| 分区 | 图表 | 说明 |
| --- | --- | --- |
| Memory | Resident memory | `rss_mb`，额外画 RSS 斜率趋势线 |
| Memory | Connection cache | `connection_cache_bytes`，字节换算为 MiB |
| Memory | In-flight connection bytes | `pending_connection_bytes`，换算为 MiB |
| Memory | Pending GenAI bytes | `pending_genai_bytes`，换算为 MiB |
| Backpressure | Entry event queue | `event_channel_bytes`；存在 `event_channel_budget_bytes` 时自动画预算参考线 |
| Backpressure | Entry queue slots | `channel_length` |
| Backpressure | In-flight connections | `pending_connection_count`；传入 `--connection-capacity` 时画上限参考线 |
| Backpressure | Completion queue | `completion_queue_bytes`，换算为 MiB |
| Throughput and latency | 表格 | 目标/实际 QPS、生成/采集 TPS、P50/P95/P99/max 延迟 |
| Stability | 表格 | Panic、abort、OOM、重启、exit code、trace survived、安全停止、cgroup OOM |
| Fault injection | 表格 | fault 类型、次数、期望分类、实际结果；缺少 `fault-results.json` 时显示未运行 |
| CPU and resources | CPU / Threads / File descriptors | `cpu_pct`、`threads`、`file_descriptors` |
| Resource leak | 表格 | 线程/FD/Socket 首末值、最大值和斜率 |
| Loss | Cumulative loss counters | `eviction_count`、`ring_buffer_dropped`、`channel_dropped`、`ffi_dropped`；全部为 0 时显示绿色的“无丢失”空状态 |
| Progress | Completed events per second | 对累计 `completed` 计数器求导，再做 5 点滑动平均 |

报告末尾的可折叠汇总表列出每个指标的 First、Last、Min、Max、Avg、P95 和每小时斜率；
RSS 行的斜率显示为 `—`，因为全窗口斜率受启动阶段影响，改由“RSS 斜率分析”展示 warmup 后拟合和滚动斜率。
存在 `config.json` 时，报告还会生成“运行限制”折叠表。
所有图表支持鼠标悬浮显示十字线和各系列数值；右上角按钮可切换浅色/深色主题。

## A/B 对比报告

`resource_run.sh --ab` 在 before/after 两个 run 完成后会额外调用
`plot_ab_comparison.py`，生成 `<output>/ab-comparison.html`，包含：

- baseline/optimized 的 RSS 峰值/平均、CPU、QPS、TPS、P99、丢弃率、有效完整率、Panic/OOM；
- RSS 和 CPU 的双曲线图；
- 关键指标对比表（baseline / optimized / Δ / 判定）；
- 结论区。

A/B 报告优先读取同目录 `resource-summary.json` 的权威 RSS/CPU 统计（正式负载窗口、
CPU 区间重叠加权），并用其 `valid` 字段决定页面结论。结果无效或缺少该文件时，页面会
标记为 INVALID/未验证，Δ 与判定显示 `N/A (invalid)`，不能据此推断改善。独立运行
`plot_ab_comparison.py` 时可用 `--resource-summary` 显式指定该文件。

A/B 报告的“改善/退化”只是展示分类，最终 PASS/FAIL 仍由冻结门槛决定。

## 数据要求与行为

- CSV 必须非空且包含表头；文件无法读取或没有任何样本时，脚本以非零状态退出。
- 时间列优先使用 `monotonic_timestamp`（只要有一行非空），否则使用 `timestamp`；所有时间换算成相对首个有效样本的秒数。
- 数值按浮点数解析；空值、非数字、`NaN` 和 `Inf` 一律视为缺失，折线在缺失处断开。
- 只有上表列出的列会参与绘图；`event_channel_budget_bytes` 和 `process_alive` 分别只用于参考线和进程存活判定，其他未知列会被忽略。
- 字节类指标统一除以 1,048,576 换算为 MiB。
- 每条序列超过 `--max-points` 时按桶降采样，并保留每桶的最小值和最大值，避免尖峰被抹掉。
- 脚本对输入只读，只写出一个 HTML 文件（以及按需创建输出目录）。

## 与其他 benchmark 脚本的关系

```text
run.sh / resource_run.sh
        │
        ├── collect_metrics.py  ──> metrics.csv
        ├── mock_llm_server.py
        └── validate_results.py ──> report.json

plot_metrics.py ── 读取 metrics.csv + 可选 run-summary.json / report.json ──> metrics-report.html
                  （resource_run.sh 收尾时自动执行）
```

- `collect_metrics.py` 负责采样进程资源和 AgentSight 内部指标，写出 `metrics.csv`。
- `run.sh` 在启用资源采集时把 `metrics.csv` 写到运行目录；`resource_run.sh` 把单次运行产物放在 `<输出目录>/run/` 下。
- `resource_run.sh` 结束时会对每个存在 `metrics.csv` 的运行目录自动调用 `plot_metrics.py`，生成 `metrics-report.html`；A/B 模式分别生成在 `before/` 和 `after/` 下。
- 也可以按上面的命令手动运行，对已有目录重新生成报告。`run.sh` 不会自动调用本脚本。

## 退出码

| 退出码 | 含义 |
| --- | --- |
| `0` | 报告生成成功，标准输出为 HTML 路径 |
| `1` | CSV 无法读取、CSV 没有样本，或 HTML 无法写入 |
| `2` | 命令行参数非法（`argparse` 标准行为） |

## 维护注意

`resource_run.py` 会把整个 `single_run/` 目录复制到运行产物的 `harness/` 下，因此本目录也会被一并复制。
请只在 `docs/` 中放体积很小的说明文件。新增图表或 KPI 时，同步更新 `plot_metrics.py` 中的定义和本文档。

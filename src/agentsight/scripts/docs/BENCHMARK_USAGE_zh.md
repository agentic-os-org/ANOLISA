# AgentSight benchmark 使用说明

[English](BENCHMARK_USAGE.md)

此 benchmark 生成可复现的独立模拟 Agent 负载，并控制每个 Agent 每秒接收的输出 Token 数。入口是 [`single_run/run.sh`](../benchmark/single_run/run.sh)，会启动本地 HTTPS/SSE mock 服务端、客户端进程，并生成报告。

自动启动采集、测量 RSS/CPU 和进行 Git A/B 对比时，使用 [`single_run/resource_run.sh`](../benchmark/single_run/resource_run.sh)，见第 7 节。

## 1. 准备环境

使用 Linux、Python 3.10 或以上版本，以及 `openssl` 和 `curl`。从仓库根目录执行：

```bash
cd src/agentsight
python3 -m venv /tmp/agentsight-bench-venv
source /tmp/agentsight-bench-venv/bin/activate
python -m pip install -r scripts/benchmark/requirements.txt
```

Agent 模式无需 k6。`tiktoken` 用于确认重复的 ` x` 文本每个片段对应一个 `cl100k_base` Token；首次使用会在测量窗口开始前下载并缓存编码数据。mock 的输入 usage 是每个请求固定 12 Token 的模拟值。

## 2. 启动 32 个 Agent，每路 100 tok/s

在同一个终端中，从 `src/agentsight` 目录执行：

```bash
bash scripts/benchmark/single_run/run.sh \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60
```

此命令创建 32 个独立客户端进程，目标总输出速率为 3,200 tok/s。每个客户端维持一路 SSE 请求，响应结束后立即开始下一轮。默认每个响应输出 1,000 Token，在 100 tok/s 下约持续 10 秒。

60 秒是测量窗口，不包含准备阶段和探针发现等待时间。退出前会收完在途响应，因此总运行时间会更长。至少指定 `--agents` 或 `--tokens-per-second` 来启用 Agent 模式；否则脚本使用原有的 QPS 模式。

## 3. 调整负载和产物目录

例如，改为 8 个 Agent、每路 50 tok/s，每个响应输出 250 Token：

```bash
bench_output_dir="/tmp/agentsight-bench-$(date -u +%Y%m%dT%H%M%SZ)"
bash scripts/benchmark/single_run/run.sh \
  --agents 8 \
  --tokens-per-second 50 \
  --tokens-per-response 250 \
  --duration 10 \
  --port 8444 \
  --output-dir "$bench_output_dir"
```

未指定 `--output-dir` 时，产物保存在 `src/agentsight/scripts/benchmark/results/<UTC 时间戳>/`，时间戳格式为 `YYYYMMDDTHHMMSSZ`。脚本结束时会打印最终目录。每次使用新的目录：Agent 模式发现已有 `agents/` 路径时会拒绝复用。

也可通过 `OUTPUT_DIR` 环境变量指定位置，显式传入的 `--output-dir` 优先。

## 4. 常用参数

| 参数 | 默认值 | 用途 |
| --- | --- | --- |
| `--agents N` | Agent 模式下为 32 | 独立客户端进程数量，必须为正整数。 |
| `--tokens-per-second RATE` | Agent 模式下为 100 | 每个 Agent 的目标输出 tok/s，必须为有限正数。 |
| `--duration SEC` | 900 | 测量时长，单位为秒；短测时显式指定。 |
| `--tokens-per-response N` | 1000 | 每个请求输出的 Token 数，影响流持续时间和请求轮转频率。 |
| `--payload-kb N` | 4 | 请求填充的近似大小，单位为 KiB；完整请求还包含 JSON 等开销。 |
| `--startup-delay SEC` | 5 | 所有客户端就绪后的等待时间，用于探针发现；允许为 0。 |
| `--rate-tolerance FRACTION` | 0.05 | 允许的相对速率偏差；0.05 表示 ±5%，要求 `0 < 值 < 1`。 |
| `--output-dir DIR` | UTC 时间戳目录 | 产物位置，每次使用新的路径。 |
| `--host HOST` | `127.0.0.1` | mock 服务端监听地址，也是客户端目标地址。 |
| `--port PORT` | 8443 | mock HTTPS 端口；默认端口被占用时选择空闲端口。 |
| `--agentsight-pid PID` | 未指定 | 采集已经运行的采集进程的资源占用。 |
| `--metrics-file PATH` | 未指定 | 读取已有的内部 Prometheus 指标快照。 |
| `--db PATH` | 未指定 | 与已有的 standalone AgentSight SQLite 数据库按 request ID 对账。 |
| `--ffi-events PATH` | 未指定 | 对账 FFI 回调凭证；与 `--db` 互斥。 |
| `--validation-wait-seconds SEC` | 30 | 负载结束后额外等待异步数据库写入的时间。 |
| `--help` | — | 打印脚本参数和 Agent 模式默认值。 |

指定两个负载参数中的任意一个后，另一项默认采用 32 个 Agent 或 100 tok/s。此模式支持 HTTP/1.1 SSE，`--qps` 不控制其速率：脚本按 Agent 数量、输出速率和每个响应的 Token 数计算名义请求吞吐。

对比内存优化前后时，保持请求填充大小和每个响应的 Token 数一致；它们都会影响驻留数据量和流的存活时间。

## 5. 查看结果

| 产物 | 内容 |
| --- | --- |
| `benchmark-report.md` | 可读的负载、资源和可选采集校验报告。 |
| `agent-summary.json` | 独立 PID、每路接收速率、并发流重叠情况和 `passed`。 |
| `run-summary.json` | 整次运行的机器可读汇总，包含 `agent_load`。 |
| `agents.jsonl.gz` | request ID、成功计数和请求延迟记录。 |
| `agents/` | 每个客户端的结果、请求日志和统一起跑文件。 |
| `agents.log` | 客户端协调进程的输出和失败详情。 |
| `mock-server.log` | 本地 mock 服务端的启动信息和错误。 |
| `request-logs/` | mock 服务端接受请求的流水记录。 |
| `metrics.csv` | 通过 `--agentsight-pid` 启用采样后生成的进程资源数据。 |
| `report.json` | 根据 SQLite 或 FFI 回调凭证生成的 request ID 和 Token 对账结果。 |

在 `agent-summary.json` 中查看 `distinct_agent_pids`、`peak_active_streams`、`average_active_streams`、`agents[].tokens_per_second` 和 `passed`。每路输出速率都必须位于配置的偏差范围内；活跃流峰值必须达到 Agent 数量，均值必须达到 `N × (1 - tolerance)`。实测速率仅统计测量窗口内接收的输出 Token，不包含模拟输入 usage 和尾部收流阶段接收的 Token。

执行成功返回 0；负载验证、可选资源采样或采集校验失败时返回非零。资源安全保护停止负载时，脚本返回 75 并生成 `safety-stop.json`。仅负载 PASS 不能证明采集完整，也不能证明已达到 275 MB 内存目标。

## 6. 采集 AgentSight 资源

先按 [CLI 参考](../../../../docs/user-guide/zh/agent-observability/agentsight/cli-reference.md) 启动采集程序。eBPF tracing 需要 root 或所需 capabilities。在生产配置的隔离副本中，将下面条目加入 `cmdline.allow` 以匹配客户端，并排除发现 mock 服务端的规则：

```json
{
  "rule": ["*python*", "*agents.py", "--worker"],
  "agent_name": "BenchmarkAgent"
}
```

保留待评估的功能、资源限制和存储配置。在负载命令中添加 `--agentsight-pid <COLLECTOR_PID>`，填写真实采集进程 PID；已配置指标导出器时可添加 `--metrics-file <METRICS_SNAPSHOT_PATH>`，使用 standalone SQLite 存储时可添加 `--db <GENAI_DATABASE_PATH>`。这些占位值必须替换为已有的实际值。脚本不会启动 AgentSight，也不会配置其指标导出器。

资源采样从统一测量起点开始，并覆盖尾部收流阶段。`metrics.csv` 中的 RSS 单位实际为 MiB，现有报告可能标为 MB。FFI 集成时传入宿主进程 PID，采集的是宿主的 RSS；LoongCollector WSS 需要按原实验的统计范围单独测量。

## 7. 一键测资源与 A/B 对比

脚本先让全部客户端建立 TLS 连接，再启动 AgentSight，使启动扫描能看到已加载的 TLS 库。
每路客户端发送一条小预检请求；只有全部客户端都有完整且 Token 用量正确的 SQLite 记录（CLI）或 FFI 回调凭证后，
才放行测量 barrier。预检请求使用独立 ID 前缀，不计入正式负载、采集记录数和 RSS 采样。

`--ready-timeout` 默认 30 秒，分别限制采集初始化和捕获就绪的等待时间。预检失败会在测量开始前退出，
并把每路结果写入 `capture-readiness.json`；可结合 `trace.log` 与 `agents.log` 排查。
正常退出、失败、Ctrl+C 和 SIGTERM 都会回收本轮启动的进程。A/B 两轮使用相同的准备流程与就绪检查。

从 `src/agentsight` 目录执行：

```bash
bash scripts/benchmark/single_run/resource_run.sh \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60 \
  --output-dir /tmp/agentsight-resource-single
```

脚本会在独立缓存的 venv 中准备缺失的 Python 依赖，冻结当前工作区，并使用 Cargo lock 文件编译 release 二进制。Git/源码模式需要项目的 Rust 和 Linux eBPF 构建依赖；已有二进制时可通过 `--agentsight-bin PATH` 跳过编译。构建和依赖缓存在 `${XDG_CACHE_HOME:-$HOME/.cache}/agentsight-benchmark/` 下。采集和数据库校验会按需请求 `sudo`，可能需要输入密码；仅当当前用户已有 eBPF 权限时使用 `--no-sudo`。

脚本生成隔离配置、数据库和日志，等待采集初始化成功，然后执行已有负载和采集校验。正常结束、失败、Ctrl+C 或 SIGTERM 都会回收本次启动的进程。

隔离配置中的 `runtime_limits.connection_capacity` 默认等于 Agent 数量，因此 32 个 Agent 使用 32 个连接槽位。可用 `--connection-capacity N` 覆盖，报告会记录实际设置值。A/B 两轮使用相同容量。比较的二进制需支持此配置字段；忽略该字段的旧版本仍沿用自身上限，采集失败时比较结果继续为 INVALID。

`--ring-buffer-mb N` 覆盖 eBPF Ring Buffer 大小，单位为 MiB；`N` 必须是正的 2 的幂，例如 32 或 64。未传此参数时保留 `--config` 中的设置，默认配置为 32 MiB。A/B 两轮需使用相同大小，增大缓冲区会改变内存基线。并发发送大请求时，可以补跑 64 MiB 档，检查默认缓冲区是否偏小；报告会打印所选大小。

对比最近一次提交与当前工作区，包含暂存、未暂存和未被 ignore 的未跟踪文件：

```bash
bash scripts/benchmark/single_run/resource_run.sh \
  --ab \
  --baseline HEAD \
  --candidate working-tree \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60 \
  --output-dir /tmp/agentsight-resource-ab
```

`--baseline` 和 `--candidate` 也接受 commit hash、本地分支和 tag。候选版本默认是 `working-tree`；`HEAD` 不包含未提交改动。源码在临时 detached worktree 中编译，工作区快照使用独立 index，保留当前分支和暂存区。两个二进制都在测量前编译完成，随后使用同一份冻结的负载脚本、配置和负载参数依次运行，每轮重新启动采集，并使用独立数据库。已有两个二进制时，使用 `--ab --baseline-bin PATH --candidate-bin PATH`。

终端和 `resource-report.txt` 会显示平均/峰值 RSS、对应整机内存占比、平均/P95/峰值 CPU、Load/Capture 的 PASS 或 FAIL，以及运行状态。CPU 统计 standalone AgentSight 进程的全部线程，100% 表示一个逻辑核，允许超过 100%；报告记录主机逻辑核数，不包含 mock 服务端和客户端进程。

A/B 显示 RSS 减少的 MiB 和相对百分比；CPU 减少量使用百分点 `before - after` 和相对百分比，负数表示资源占用增加。CPU 基线为零时不计算相对百分比。整机内存占比的变化使用百分点。`resource-summary.json` 保存测量值、版本/tree ID 和二进制 hash。每轮的 CSV、采集报告和日志位于 `run/`，A/B 则位于 `before/`、`after/`。

只有负载通过、采集完整率和 Token 正确率均达到 99.9%、采集进程存活并有有效资源样本，且可用的探针计数未发现丢事件时，结果才是 VALID。报告分别打印 Ring Buffer 和用户态通道丢事件数；计数覆盖全部探针类型，不能据此确定丢失事件的类型。即使全部请求的 usage 正确，发现丢事件仍判为 INVALID，不计算优化比例。计数不可用时显示 N/A，旧二进制继续使用请求级校验。CPU 样本缺失、无效或覆盖不足正式窗口的 95% 时，同样判为 INVALID，不计算任何优化比例。成功退出 0，无效或失败退出非零；Ctrl+C 退出 130 并保留部分产物。每次使用新的输出目录，默认位置是 `scripts/benchmark/results/resource-<UTC 时间戳>-<PID>/`。

此一键脚本默认 32 个 Agent、每路 100 tok/s、60 秒；仅负载的 `run.sh` 仍默认 900 秒。支持上面的负载、探针发现、端口和安全限制参数，完整列表见 `resource_run.sh --help`。RSS 和 CPU 约每秒采样一次，覆盖尾部收流。主统计使用 `agents/start.json` 中的单调时钟起止窗口，尾部收流单独保存在每轮的 `tail` 对象和文本报告中。

平均 RSS 是窗口内样本的算术平均，峰值 RSS 是采样最大值，并非进程运行以来的 `VmHWM`；单位为 MiB，并非十进制 MB。首条样本只建立 CPU 基准，CPU 字段留空。平均 CPU 按实际采样间隔加权，同时报告累计 CPU 秒；P95 使用区间样本的最近秩百分位数，CPU 峰值是区间平均占用率的最大值。跨越窗口边界的区间按重叠时长分摊。`metrics.csv` 保留原时间戳，并新增 `monotonic_timestamp` 和 `cpu_interval_seconds` 支持这些计算。原 LoongCollector WSS 目标需要按原实验范围另行测量。

### 7.1 使用最小 C 宿主测量 FFI 动态库

在资源测试命令中添加 `--ffi`，脚本会编译 `libagentsight.so` 和
[最小 C consumer](../benchmark/single_run/ffi_runner.c)，复用现有负载、配置和 Git A/B 参数。
需要 C 编译器（`cc`，也可用 `CC` 指定）。eBPF 仍需要 root 或相应 capabilities，默认请求 sudo。

```bash
bash scripts/benchmark/single_run/resource_run.sh --ffi \
  --agents 32 --tokens-per-second 100 --duration 60
```

C 宿主通过 epoll 等待 eventfd，并使用非 NULL 回调持续消费事件。
默认的 `--ffi-api typed` 测量 `agentsight_read` typed 接口；`--ffi-api v2` 则通过
`agentsight_read_v2` 仅接收 JSON envelope 回调。请选择实际集成使用的接口。
不逐事件打印或执行业务处理。CPU/RSS 包含全部 AgentSight 线程、FFI 转换、凭证转发及少量 C 宿主基础开销，
并非对生产宿主内部内存的精确归因。FFI 校验直接使用回调，不要求 AgentSight SQLite 存储；
保留 `--config` 中的存储开关，包括关闭 SQLite 的配置。

请求在用户内容中携带测试 ID 标记，使精简的 FFI 事件即使使用库自行生成的 response ID，也能准确关联测试请求。
Typed 回调仅将 ID、HTTP 状态和 Token 用量写入 `ffi-events.log`；v2 回调转发已有 JSON，不重复解析或序列化，
大请求可能产生较大的凭证文件。独立校验进程增量读取凭证，复用现有就绪、完整性和 Token 正确率校验。
宿主测量包含凭证写入开销；对账和 JSON 解析在其进程之外完成。

已有动态库可指定 `--ffi --candidate-lib /path/to/libagentsight.so`。
两个已有动态库使用 `--ffi --ab --baseline-lib PATH --candidate-lib PATH`；
两个 Git 版本使用 `--ffi --ab --baseline REF --candidate REF`。
`--ffi` 不能与可执行文件输入参数 `--agentsight-bin`、`--candidate-bin`、`--baseline-bin` 混用。
已有动态库需要兼容当前工作区的 `include/agentsight.h`，并安装其运行时依赖。

每轮将动态库保存在 runner 旁边，通过相对库搜索路径加载。
`inputs.json` 和 `resource-summary.json` 记录动态库及 runner 的 SHA-256。
`ffi-consumer.json` 记录包含预检请求的回调计数；回调凭证缺失或不正确、consumer 异常退出、
最终流水线指标缺失或 FFI 队列丢事件仍会将结果判为 INVALID。
报告会明确标注最小 C FFI 宿主的测量范围。

完整的优化前后对比设计见 [benchmark 计划](BENCHMARK_PLAN_zh.md)。


压测脚本需要传入实际 AgentSight 进程的 PID，才会生成内存数据。我确认了当前目录还没有编译好的二进制，下面会把编译、启动采集和压测命令一起给你。
```
• Explored
  └ Read run.sh, collect_metrics.py
    Search genai_path|genai\.db|genai_records\.db|genai_events|llm_calls\.db in config
    Read BENCHMARK_USAGE_zh.md
```
需要先启动 **AgentSight 采集进程**，再给 bench 传 `--agentsight-pid`。下面可以直接跑。

终端 1：编译，并用临时配置观测 benchmark 的 Agent：

```
cd /home/peanut/myCode/anolisa-next/src/agentsight
make build

python3 - <<'PY'
import json
from pathlib import Path

config = json.loads(Path("agentsight.json").read_text())
config["cmdline"] = {
    "allow": [{
        "rule": ["*python*", "*agents.py", "--worker"],
        "agent_name": "BenchmarkAgent"
    }],
    "deny": []
}
config["https"] = []
config["http"] = []
config["storage"]["base_path"] = "/tmp/agentsight-memory-db"
Path("/tmp/agentsight-memory.json").write_text(json.dumps(config, indent=2))
PY

sudo ./target/release/agentsight trace \
  --config /tmp/agentsight-memory.json
```

等采集初始化成功，保持这个终端运行。终端 2：跑 32 × 100 tok/s，同时记录内存并校验采集结果：

```
cd /home/peanut/myCode/anolisa-next/src/agentsight
source /tmp/agentsight-bench-venv/bin/activate

AGENTSIGHT_PID="$(pgrep -n -x agentsight)"
OUT_DIR="/tmp/agentsight-bench-32x100-memory-$(date +%Y%m%d-%H%M%S)"

sudo env "PATH=$PATH" bash scripts/benchmark/single_run/run.sh \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60 \
  --agentsight-pid "$AGENTSIGHT_PID" \
  --db /tmp/agentsight-memory-db/genai_events.db \
  --output-dir "$OUT_DIR"
```

跑完后，**在终端 2** 查看平均、峰值内存，以及占整机内存的比例：

```
python3 - "$OUT_DIR/metrics.csv" <<'PY'
import csv
import sys
from pathlib import Path

with open(sys.argv[1]) as f:
    rss = [float(r["rss_mb"]) for r in csv.DictReader(f) if r["rss_mb"]]
assert rss, "No memory samples collected"

total = next(line for line in Path("/proc/meminfo").read_text().splitlines()
             if line.startswith("MemTotal:"))
total_mib = int(total.split()[1]) / 1024

for label, value in [("平均", sum(rss) / len(rss)), ("峰值", max(rss))]:
    print(f"{label}: {value:.2f} MiB / {value * 1.048576:.2f} MB"
          f"，占整机内存 {value / total_mib * 100:.2f}%")
PY
```

这里测的是 **AgentSight 进程 RSS**。完整报告在 `$OUT_DIR/benchmark-report.md`；采集完整性也要看报告，避免漏采导致内存看起来偏低。结束后在终端 1 按 `Ctrl+C` 停止采集。


单次测量：

```
cd /home/peanut/myCode/anolisa-next/src/agentsight

bash scripts/benchmark/single_run/resource_run.sh \
  --agents 32 \
  --tokens-per-second 100 \
  --duration 60 \
  --output-dir /tmp/agentsight-resource-single
```

A/B 对比上次提交与当前工作区，默认同样是 **32 × 100 tok/s、60 秒**：

```
bash scripts/benchmark/single_run/resource_run.sh \
  --ab \
  --baseline HEAD \
  --candidate working-tree \
  --output-dir /tmp/agentsight-resource-ab
```


`--cpu-budget-percent 25` 为采集进程设置 `runtime_limits.cpu_budget_percent`，省略时沿用配置，
配置未设置时默认 25%，`0` 关闭。
该协作式目标调度采集落库及 primary/GenAI 清理，必要时会增加投递延迟；负载生成、CPU 采样和有效性
判断保持原口径。评估时同时查看 `metrics.csv` 的 `completion_queue_count` / `completion_queue_bytes`、
探针丢事件和尾部排空；队列持续增长不能视为 CPU 优化成功。

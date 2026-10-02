# SWE-bench Runner

[![Python 3.12+](https://img.shields.io/badge/python-3.12+-blue.svg)](https://www.python.org/downloads/)

[English](README.md)

SWE-bench Runner 是一个本地 SWE-bench 批量运行工具。它负责加载 SWE-bench
数据集，准备每个实例的工作区，调用外部 agent 生成 patch，写出
`preds.json`，并封装 SWE-bench 官方 evaluation 流程。它还可以从 OpenClaw
local session JSONL 中补录 trace，并导出 CSV 指标。

当前内置两个 agent adapter：

- `cosh`：通过本机 `cosh` CLI 在 Docker 工作区中运行。
- `openclaw`：通过 OpenClaw local 模式为每个实例创建独立 profile 和 sandbox。

## 功能范围

- 运行 SWE-bench Lite、Verified、Full、Multilingual，或自定义 HuggingFace 数据集。
- 按实例 ID、正则和切片过滤任务。
- 串行或并发执行多个实例。
- 自动跳过已尝试实例，支持 `--redo` 强制重跑。
- 生成 SWE-bench evaluation 兼容的 `preds.json`。
- 为每个实例记录 result、run metadata 和 input manifest。
- 调用 SWE-bench 官方 evaluator 评估 patch。
- 从 OpenClaw local session JSONL 补录 trace，并导出 CSV。
- 支持用户提供 skill 目录和 per-case prompt 目录。

## 前置要求

基础依赖：

- Python 3.12+
- Docker，并确保当前用户可以访问 Docker daemon
- [uv](https://github.com/astral-sh/uv)
- 可访问 HuggingFace 数据集
- 可访问 SWE-bench evaluation 所需 Docker 镜像，或允许本地构建镜像

按 agent 额外准备：

- `cosh`：`cosh` 可执行文件在 `PATH` 中。
- `openclaw`：`openclaw` 可执行文件在 `PATH` 中。

运行前可以先检查：

```bash
docker info
which cosh
which openclaw
```

## 安装

克隆仓库后同步运行依赖：

```bash
git clone <repository-url>
cd swebench-runner
uv sync
uv run swe-runner --help
```

开发环境需要同步 dev 依赖：

```bash
uv sync --group dev
uv run --group dev pytest
```

构建 wheel：

```bash
uv build --wheel
```

## 快速开始

运行一个 SWE-bench Lite 实例：

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --split test \
  --instance-id django__django-11999
```

运行 Lite 的前 10 个实例：

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --slice 0:10
```

并发运行：

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --workers 4
```

评估本次 run 生成的 patch：

```bash
swe-runner evaluate \
  --predictions ./output/run/preds.json \
  --subset lite
```

根据 run metadata 从 OpenClaw profiles 补录并分析 trace：

```bash
swe-runner analyze-traces \
  --run-metadata ./output/run/run_metadata.json
```

## 命令概览

```text
swe-runner
├── run             运行实例并生成 patch
├── evaluate        调用 SWE-bench 官方 evaluator
└── analyze-traces  补录 trace 并导出 CSV
```

## `swe-runner run`

`run` 是主流程命令。它会：

1. 检查 Docker、磁盘空间和目标 agent 可执行文件。
2. 加载数据集并应用过滤条件。
3. 为每个实例准备工作区。
4. 调用对应 agent adapter。
5. 从工作区提取 git diff 作为 `model_patch`。
6. 写入 per-instance result、`preds.json`、input manifest 和 run metadata。

### 数据集

`--subset` 支持四个内置别名：

| 别名 | HuggingFace 数据集 |
|---|---|
| `lite` | `princeton-nlp/SWE-bench_Lite` |
| `verified` | `princeton-nlp/SWE-bench_Verified` |
| `full` | `princeton-nlp/SWE-bench` |
| `multilingual` | `SWE-bench/SWE-bench_Multilingual` |

也可以传入自定义 HuggingFace dataset path：

```bash
swe-runner run \
  --agent cosh \
  --subset your-org/your-swe-dataset \
  --split test
```

自定义数据集需要提供 SWE-bench 兼容字段：`instance_id`、`repo`、
`version`、`base_commit`、`problem_statement`、`patch`、`test_patch`。
`image_name` 和 `docker_image` 为可选字段。

### 过滤顺序

过滤按以下顺序执行：

1. `--instance-id`
2. `--filter`
3. `--slice`

示例：

```bash
swe-runner run --agent cosh --instance-id django__django-11999
swe-runner run --agent cosh --filter "django__.*"
swe-runner run --agent cosh --slice 10:20
```

`--instance-id` 支持逗号分隔的多个 ID：

```bash
swe-runner run \
  --agent cosh \
  --instance-id django__django-11999,matplotlib__matplotlib-18869
```

### 续跑与重跑

默认情况下，runner 会检查 `<output>/run/results/*.json`，跳过已经有
result 文件的实例。需要重新运行时使用：

```bash
swe-runner run --agent cosh --subset lite --redo
```

### Docker registry

如果需要从指定 registry 拉取 SWE-bench 镜像，可以使用
`--docker-pull-registry`。runner 会先从该 registry 拉取镜像，再 tag
回原始镜像名供后续流程使用。

```bash
swe-runner run \
  --agent cosh \
  --subset lite \
  --docker-pull-registry registry.example.com
```

### Prompt 资源目录

`--use-skill` 和 `--per-case-prompt` 互斥。两者都通过用户指定目录匹配资源。

#### Skill

使用 `--use-skill` 时，runner 会查找：

```text
<skills-dir>/swe-bench-patch-generation/SKILL.md
```

示例：

```bash
swe-runner run \
  --agent openclaw \
  --subset lite \
  --use-skill \
  --skills-dir ./resources/skills
```

行为说明：

- `openclaw` 会把 skill 文本合并到每个实例工作区的 `AGENTS.md`。
- `cosh` 会在 prompt 中加入使用该 skill 的指令。
- 如果文件不存在，runner 会跳过 skill 注入并继续运行。

#### Per-case prompt

使用 `--per-case-prompt` 时，runner 会按实例 ID 查找：

```text
<prompts-dir>/<instance_id>
```

示例：

```bash
swe-runner run \
  --agent openclaw \
  --subset lite \
  --per-case-prompt \
  --prompts-dir ./resources/prompts
```

行为说明：

- `openclaw` 会把匹配到的文本追加到该实例 user prompt 的
  `<task_guidance>` 中。
- `cosh` 会把匹配到的文本放入 prompt 的 `<custom_instructions>` 中。
- 如果文件不存在或为空，runner 会跳过该实例的 per-case prompt 并继续运行。

### Tokenless

`--tokenless` 只适用于声明支持该选项的 agent。当前内置 adapter 中只有
`openclaw` 支持它。

```bash
swe-runner run \
  --agent openclaw \
  --subset lite \
  --tokenless
```

开启后，runner 会：

- 在每个 OpenClaw per-case profile 中启用 `plugins.entries.tokenless.enabled`。
- 将 host 上的 tokenless OpenClaw extension 暴露到该 profile。
- 查找并复制 `rtk`、`tokenless` 二进制到实例 workspace 的
  `.runner/tokenless/bin/`。
- 将 sandbox 内的 `/workspace/.runner/tokenless/bin` 放到 `PATH` 前部。
- 在运行结果中写出 tokenless evidence，用于确认配置和运行期 hook 状态。

### Run 参数速查

| 参数 | 默认值 | 说明 |
|---|---:|---|
| `--agent, -a` | 必填 | agent 名称：`cosh` 或 `openclaw` |
| `--subset, -s` | `lite` | 数据集别名或 HuggingFace dataset path |
| `--split` | `test` | 数据集 split |
| `--output, -o` | `./output` | 输出根目录，run 结果写入其 `run/` 子目录 |
| `--timeout` | `1200` | 单实例 agent 超时时间，单位秒 |
| `--step-limit` | `0` | 最大步骤数；`0` 表示不限制。当前 `cosh` 会映射为 CLI turn limit |
| `--slice` | 无 | 实例切片，如 `0:10`、`10:`、`:5` |
| `--filter` | 无 | 实例 ID 正则过滤 |
| `--instance-id, -i` | 无 | 指定实例 ID，多个用逗号分隔 |
| `--workers, -w` | `1` | 并发 worker 数 |
| `--docker-pull-registry` | 无 | Docker pull 时使用的 registry host |
| `--use-skill` | `false` | 从 `--skills-dir` 读取 skill 资源 |
| `--skills-dir` | 无 | skill 资源根目录 |
| `--per-case-prompt` | `false` | 从 `--prompts-dir` 读取 per-instance prompt |
| `--prompts-dir` | 无 | per-instance prompt 资源根目录 |
| `--tokenless` | `false` | 启用 tokenless/rtk 注入，当前仅 `openclaw` 支持 |
| `--redo` | `false` | 重新运行已经有 result 文件的实例 |
| `--verbose, -v` | `false` | 写入 DEBUG 级别日志 |

## Agent adapter

### `cosh`

`cosh` adapter 会为每个实例启动 SWE-bench Docker 工作区，并构造包含问题描述、
工作区路径、容器名和操作约束的 prompt。运行命令形态为：

```text
cosh --yolo <prompt>
```

如果设置 `--step-limit N`，会额外传入：

```text
--max-session-turns N
```

agent 结束后，runner 从工作区提取 git diff 并写入 `preds.json`。

### `openclaw`

`openclaw` adapter 使用 OpenClaw local 模式。每个实例都会创建独立的
OpenClaw profile、session 和 sandbox agent。

主要行为：

- 将 SWE-bench repo 准备到临时工作区。
- 将 repo 挂载到 OpenClaw sandbox 的 `/testbed`。
- 将 OpenClaw workspace 放在单独的 `/workspace`。
- 为每个实例生成独立 profile，写入 `<output>/run/openclaw-profiles/<instance_id>/`。
- 复制基础 OpenClaw 配置后，只修改该实例 profile 的配置。
- 通过 `openclaw --profile <profile> agent --local --json ...` 执行。
- 运行结束后清理 profile symlink、临时 workspace 和对应 sandbox 容器。

基础 OpenClaw 配置路径解析顺序：

1. adapter 显式传入的配置路径
2. `OPENCLAW_CONFIG_PATH`
3. `~/.openclaw/openclaw.json`

如果基础配置文件不存在，runner 会为该实例 profile 写入空配置对象，然后继续
按当前实例补充 sandbox 配置。

## `swe-runner evaluate`

`evaluate` 封装 SWE-bench 官方 evaluation 流程。它读取 `preds.json` 中
非空的 `model_patch`，并只评估这些实例。

默认评估：

```bash
swe-runner evaluate
```

指定预测文件和输出目录：

```bash
swe-runner evaluate \
  --predictions ./output/run/preds.json \
  --output ./output \
  --subset lite \
  --workers 4
```

`evaluate` 的 `--subset` 只接受 `lite`、`verified`、`full`、`multilingual`
四个内置别名。

如果要本地构建 evaluation 镜像，使用：

```bash
swe-runner evaluate --namespace none
```

### Evaluate 参数速查

| 参数 | 默认值 | 说明 |
|---|---:|---|
| `--predictions, -p` | `./output/run/preds.json` | 预测文件 |
| `--subset, -s` | `lite` | 评估数据集：`lite` / `verified` / `full` / `multilingual` |
| `--split` | `test` | 数据集 split |
| `--output, -o` | `./output` | 输出根目录，评估结果写入其 `evaluate/` 子目录 |
| `--workers, -w` | `4` | SWE-bench evaluation 并发数 |
| `--timeout` | `1800` | 单实例评估超时，单位秒 |
| `--run-id` | 自动生成 | SWE-bench evaluation run id |
| `--cache-level` | `env` | Docker 缓存级别：`none` / `base` / `env` / `instance` |
| `--namespace` | `swebench` | evaluation 镜像命名空间；`none` 表示本地构建 |
| `--verbose, -v` | `false` | 写入 DEBUG 级别日志 |

## `swe-runner analyze-traces`

`analyze-traces` 会读取 trace JSON，导出 per-trace 明细、per-case 汇总和
详细指标 CSV。它也可以根据 `run_metadata.json` 和 OpenClaw profiles 自动
从 session JSONL 补录 trace。

分析已有 trace：

```bash
swe-runner analyze-traces \
  --trace-root ./traces \
  --output ./output
```

从 run metadata 推导时间窗口和 OpenClaw profiles：

```bash
swe-runner analyze-traces \
  --run-metadata ./output/run/run_metadata.json \
  --output ./output
```

显式指定 OpenClaw profiles：

```bash
swe-runner analyze-traces \
  --run-metadata ./output/run/run_metadata.json \
  --openclaw-profiles-dir ./output/run/openclaw-profiles
```

### Analyze 参数速查

| 参数 | 默认值 | 说明 |
|---|---:|---|
| `--trace-root` | `<output>/analyze-traces/traces` | trace JSON 根目录 |
| `--output, -o` | `./output` | 输出根目录 |
| `--trim-ratio` | `0.1` | 截尾平均比例，范围 `[0, 0.5)` |
| `--openclaw-profiles-dir` | 根据 `--run-metadata` 推导 | OpenClaw local profiles 目录 |
| `--start` | 无 | 补录窗口开始时间，ISO-8601 或 epoch |
| `--end` | `now` | 补录窗口结束时间 |
| `--run-metadata` | 无 | `run` 命令生成的 `run_metadata.json` |

## 输出目录

默认输出根目录是 `./output`。每个命令会写入自己的子目录：

```text
output/
├── run/
│   ├── preds.json
│   ├── run_metadata.json
│   ├── swe-runner.run.log
│   ├── results/
│   │   └── <instance_id>.json
│   ├── input-manifests/
│   │   └── <instance_id>/input_manifest.json
│   ├── openclaw-profiles/
│   │   └── <instance_id>/
│   ├── openclaw-errors/
│   │   └── <instance_id>.log
│   └── openclaw-tokenless-evidence/
│       └── <instance_id>.json
├── evaluate/
│   ├── swe-runner.evaluate.log
│   ├── <model>.<run_id>.json
│   └── logs/run_evaluation/<run_id>/<model>/<instance_id>/
└── analyze-traces/
    ├── swe-runner.analyze-traces.log
    ├── traces/
    │   └── <instance_id>/trace*.json
    ├── trace_details/
    │   └── <instance_id>.csv
    ├── trace_metrics/
    │   └── trace_metrics.csv
    └── trace_summary.csv
```

重要文件：

| 文件 | 说明 |
|---|---|
| `run/preds.json` | SWE-bench evaluation 可读取的预测文件 |
| `run/results/*.json` | 单实例运行摘要，成功失败都会写入 |
| `run/run_metadata.json` | 本次 run 的时间、agent、worker、实例和 metadata mapping |
| `run/input-manifests/*/input_manifest.json` | 单实例输入快照，包含数据集行、settings、prompt hash、资源文件记录和 runner 信息 |
| `run/openclaw-profiles/` | OpenClaw local 每实例 profile 和 session 产物 |
| `run/openclaw-errors/*.log` | OpenClaw local 非零退出时的 stdout/stderr |
| `run/openclaw-tokenless-evidence/*.json` | `--tokenless` 运行的配置和运行期证据摘要 |
| `evaluate/*.json` | SWE-bench evaluation 汇总报告 |
| `analyze-traces/trace_details/*.csv` | 每条 trace 的基础指标 |
| `analyze-traces/trace_summary.csv` | 每个实例的汇总指标 |
| `analyze-traces/trace_metrics/trace_metrics.csv` | 更细的 trace、工具调用和 token 指标 |

## 项目结构

```text
src/swe_runner/
├── cli.py                  # Typer CLI 入口
├── cli_commands.py         # CLI 命令处理和 settings 组装
├── agents/                 # Agent adapter 抽象、注册表和内置 adapter
│   ├── cosh/
│   └── openclaw/
├── common/                 # Pydantic 模型、日志和数据集别名
├── run/                    # run 流程
│   ├── dataset.py          # 数据集加载和过滤
│   ├── session.py          # run 会话入口
│   ├── execution/          # 并发调度和单实例生命周期
│   ├── io/                 # result、metadata、manifest 和 report 输出
│   ├── prompting/          # prompt 和外部资源加载
│   └── workspace/          # Docker、git、patch 和工作区规则
├── evaluation/             # SWE-bench evaluation 封装
└── trace_extraction/       # OpenClaw trace 补录、重建、分析和导出
```

## 开发

运行测试：

```bash
uv run --group dev pytest
uv run --group dev pytest tests/unit/cli/test_cli.py
```

静态检查：

```bash
uv run --group dev ruff check src tests
uv run --group dev mypy src/swe_runner
```

格式化：

```bash
uv run --group dev ruff format src tests
```

## 故障排查

### Docker 不可用

```bash
docker info
```

确认 Docker daemon 正在运行，并且当前用户有权限访问。

### 磁盘空间不足

SWE-bench 镜像、容器和工作区会占用较多空间。检查空间：

```bash
df -h
```

清理 Docker 缓存：

```bash
docker system prune -a
```

### Agent 可执行文件找不到

```bash
which cosh
which openclaw
```

确认对应 CLI 已安装，并且当前 shell 的 `PATH` 能找到它。

### HuggingFace 或 Docker 拉取慢

可以配置 HuggingFace 缓存、Docker registry mirror，或使用
`--docker-pull-registry` 指定镜像拉取来源。

### OpenClaw local 失败

优先检查：

- `which openclaw`
- `OPENCLAW_CONFIG_PATH`
- `~/.openclaw/openclaw.json`
- `output/run/openclaw-profiles/<instance_id>/openclaw.json`
- `output/run/openclaw-errors/<instance_id>.log`

OpenClaw adapter 不会改写基础配置文件；它只复制基础配置并修改每个实例自己的
profile 配置。

## 许可证

本项目使用 Apache License 2.0，详见 `LICENSE`。

## 致谢

- [SWE-bench](https://www.swebench.com/)
- [SWE-bench 官方 evaluation harness](https://github.com/SWE-bench/SWE-bench)

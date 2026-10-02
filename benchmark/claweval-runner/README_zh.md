# ce-runner

[English](README.md)

通过 openclaw agent 编排 [claw-eval](https://github.com/claw-eval/claw-eval) 评测任务。负责 agent 执行、session 到 trace 的转换,以及 LLM 裁判打分。

## 快速开始

```bash
# 1. 克隆 anolisa 单仓并进入本 runner
git clone <anolisa-repo-url>
cd anolisa/benchmark/claweval-runner

# 2. 一键环境搭建(下文说明它做了什么)。
#    该步骤会把锁定版本的 claw-eval 源码克隆到 ./claw-eval。
MODEL_API_KEY=sk-xxx python scripts/setup_env.py

# 3. 通过 `uv run` 运行（使用 setup 创建的 .venv，无需激活）。
#    传 --config 让运行时读取 claw-eval/config.yaml（不会自动加载）。
uv run ce-runner run claw-eval/tasks/T001zh_email_triage --config claw-eval/config.yaml
uv run ce-runner batch --prefix T --range 1-5 --parallel 4 --config claw-eval/config.yaml
```

> **所有命令都用 `uv run <cmd>` 调用。** 它会自动解析项目的 `.venv`、确保正确的解释器/依赖,并避免陈旧的 shell PATH 问题。`source .venv/bin/activate` 虽受支持但不推荐。

## 前置依赖

以下必须在运行 `setup_env.py` **之前**安装好:

- Docker >= 24.0(daemon 运行中)
- Node.js >= 18(用于 `openclaw` CLI)
- openclaw **= 2026.4.22**(`openclaw --version`)—— **只有 4.22 经过完整测试,其他版本可能存在兼容性问题。强烈建议使用 4.22。**
- Python >= 3.11

应导出 `MODEL_API_KEY`(可选 `MODEL_BASE_URL`、`MODEL_ID`);否则模型配置步骤会被跳过。

## 环境搭建

`scripts/setup_env.py` 是幂等且端到端的。它执行以下步骤:

| 步骤 | 动作 |
|------|--------|
| 1 | 检查前置依赖:Python >= 3.11、git、curl、Docker、Node.js >= 18、npm、openclaw = 2026.4.22 |
| 2 | 将锁定版本的 `claw-eval` 源码克隆到 `claw-eval/`(仓库地址与 revision 在 `scripts/setup_env.py` 中锁定) |
| 3 | 确保 `uv` 包管理器(通过 curl 安装独立二进制) |
| 4 | 通过 `uv venv` 创建 `./.venv`;注入 `VIRTUAL_ENV` + `PATH` |
| 5 | 通过 `uv pip` 安装 ce-runner `[dev]` + claw-eval `[mock,sandbox]` + sandbox-server 依赖 |
| 6 | 运行 `scripts/configure_openclaw.py` 设置 openclaw 配置(含前后 gateway 健康检查) |
| 7 | 确保 openclaw gateway 服务:缺失/禁用则 `install`,停止则 `start` |
| 8 | 运行 `scripts/configure_model.py --api-key $MODEL_API_KEY`(未设置环境变量则跳过) |
| 9 | 通过 npm 安装 `mcporter`(MCP 工具分发所需) |
| 10 | 修补 `claw-eval/Dockerfile.agent` —— 备份为 `.bak` 并移除 TUNA 镜像行 |
| 11 | `docker build -t claw-eval-agent:latest -f claw-eval/Dockerfile.agent claw-eval/` |
| 12 | 从 Hugging Face 下载并解压任务 fixtures(视频等;`--skip-fixtures` 可跳过) |
| 13 | 校验:`ce-runner --help`、`claw-eval --help`、`openclaw gateway status`、Docker、环境清洁度 |

搭建完成后,给每条命令都加上 `uv run` 前缀(如 `uv run ce-runner --help`、`uv run pytest`、`uv run python scripts/list_tasks.py`)。`ce-runner` 与 `claw-eval` 作为 console script 暴露在 venv 内,由 `uv run` 自动解析。

## 模型配置

```bash
# 任何时候都可用新 key 重新运行
uv run python scripts/configure_model.py --api-key sk-xxx \
  --base-url https://api.provider.com/v1 --model-id your-model

# 或通过环境变量
export MODEL_API_KEY=sk-xxx MODEL_BASE_URL=https://api.example.com/v1 MODEL_ID=gpt-4o
```

## 用法

以下所有命令均假设使用 `uv run`（项目 venv 自动解析）。

> **除非已 export model/judge 环境变量，否则必须传 `--config`。** `run`/`batch` **不会**自动加载 `claw-eval/config.yaml`；请传 `--config claw-eval/config.yaml`（由 `scripts/configure_model.py` 写入），或 export `MODEL_API_KEY`/`MODEL_BASE_URL`/`MODEL_ID` 与 `JUDGE_API_KEY`/`JUDGE_BASE_URL`/`JUDGE_MODEL_ID`。

```bash
# 单个任务
uv run ce-runner run claw-eval/tasks/T001zh_email_triage --config claw-eval/config.yaml

# 批量
uv run ce-runner batch --prefix T --range 1-10 --parallel 5 --config claw-eval/config.yaml
uv run ce-runner batch --prefix T --range 1-5 --trials 3 --parallel 5 --config claw-eval/config.yaml
uv run ce-runner batch --tag general --parallel 4 --config claw-eval/config.yaml
uv run ce-runner batch --tasks-file tasks.txt --parallel 4 --config claw-eval/config.yaml
uv run ce-runner batch --tasks-string T001zh_email_triage,T002zh_calendar --parallel 4 --config claw-eval/config.yaml
uv run ce-runner batch --prefix M --range 1-10:2 --parallel 3 --config claw-eval/config.yaml   # 每隔一个 M 任务
```

### 任务类型

ce-runner 支持三种任务前缀,对应不同执行模式:

| 前缀 | 模式 | 描述 |
|--------|------|-------------|
| T | 通用 | Mock 服务 + sandbox 容器;标准工具调用评测 |
| M | 多模态 | 首轮 HTTP API(图片/视频附件)+ sandbox 容器 |
| C | 用户 Agent | 多轮 agent ↔ UserAgent 对话循环 |

### 关键选项

| 选项 | 默认值 | 描述 |
|---|---|---|
| `--config` | — | 配置 YAML 路径（如 `claw-eval/config.yaml`），含 `model`/`judge` 段。除非已 export model/judge 环境变量，否则必传。 |
| `--parallel N` | 4 | 并行 agent worker 数 |
| `--trials N` | 1 | 每任务试验次数(用于 pass@k) |
| `--timeout N` | 600 | agent 超时(秒) |
| `--sandbox-image` | `claw-eval-agent:latest` | sandbox 的 Docker 镜像 |
| `--tasks-file` | — | 任务 ID 文件(每行一个) |
| `--tasks-string` | — | 逗号分隔的精确任务名(与 --tasks-file 互斥) |
| `--filter` | — | 对任务目录名做子串匹配 |
| `--grade-parallel` | min(parallel,2) | LLM 裁判打分的并行 worker 数 |
| `--chunk-size` | 4 | 每 chunk 的任务数(控制峰值内存;会自动提升到 --parallel) |
| `--trace-prefix` | `openclaw` | trace 目录名前缀 |
| `--skip-preflight` | false | 跳过 openclaw 插件 + docker 预检 |

## 输出

```
claw-eval/traces/openclaw_<YY-MM-DD-HH-MM>/
├── <task_id>_xxxx.jsonl   # 转换后的 trace
├── batch_results.json     # 每次试验结果
└── batch_summary.json     # 聚合汇总
```

## 架构:MCP 工具注入与防作弊隔离

ce-runner 使用 openclaw 原生的 MCP 运行时(stdio)向 agent 暴露任务专属工具。核心挑战在于确保 agent **只能**访问自己任务的工具,无法读取宿主机文件(如 `grader.py`)作弊。

### 核心机制

每个任务 agent 都配置了三条协同工作的工具策略:

```json
{
  "id": "claweval-t001zh_email_triage",
  "tools": {
    "allow": [
      "exec",
      "claw-eval-mock-T001zh_email_triage__gmail_list_messages",
      "claw-eval-mock-T001zh_email_triage__gmail_send_message",
      "claw-eval-sandbox-T001zh_email_triage__Bash",
      "claw-eval-sandbox-T001zh_email_triage__Read",
      "..."
    ],
    "deny": [
      "exec", "read", "write", "edit", "process", "browser",
      "canvas", "nodes", "cron", "sessions_list", "sessions_history",
      "sessions_send", "sessions_spawn", "sessions_yield", "subagents",
      "web_fetch", "session_status", "memory_get", "memory_search",
      "other-mcp-server__*"
    ],
    "exec": { "security": "full", "ask": "off" }
  }
}
```

| 策略 | 作用 |
|--------|------|
| `tools.allow` | 显式白名单:`exec`(启用工具执行)+ `serverKey__toolName` 格式(双下划线)的任务 MCP 工具。只有列出的工具对模型可用。 |
| `tools.deny` | 屏蔽所有内置 gateway 工具(宿主 exec/read/write/browser 等)以及其他 MCP 服务器的工具。防止访问宿主文件系统和跨任务泄漏。 |
| `tools.exec` | 设置 MCP 工具执行的安全策略(工具调用成功所必需)。 |

### 每个任务两个 MCP 服务器

| 服务器 | 用途 | 工具示例 |
|--------|---------|---------------|
| `claw-eval-mock-<task_id>` | 运行在宿主机上的 Mock 服务(邮件、日历等) | `gmail_list_messages`、`calendar_create_event` |
| `claw-eval-sandbox-<task_id>` | 路由到 Docker 容器的 sandbox 桥接 | `Bash`、`Read`、`Write`、`Edit`、`Glob`、`Grep` |

两者都通过 `openclaw mcp set` 注册(持久化,stdio 传输)。sandbox 桥接把所有文件/shell 操作路由到隔离的 Docker 容器 —— 即使 agent 调用 `Bash`,也是在容器内执行,而非宿主机。

### 批量模式的跨任务隔离

在批量模式下,每个任务的 deny 列表还会包含其他任务的 MCP 服务器名(`claw-eval-mock-<other_task>__*`),防止一个 agent 调用另一个任务的 mock 服务。

### 实现

核心逻辑:`src/ce_runner/tool_injector.py`
- `_build_allowlist()` —— 根据 task.yaml 工具 + sandbox 工具构造 `alsoAllow`
- `_build_deny_list()` —— 根据内置工具 + 其他 MCP 服务器构造 `deny`
- `_DENY_BUILTIN_TOOLS` —— 需屏蔽的 gateway 内置工具静态列表

## 脚本

| 脚本 | 描述 |
|---|---|
| `scripts/setup_env.py` | 一键环境搭建(见上表) |
| `scripts/configure_model.py` | 配置模型 API key / base-url / model-id |
| `scripts/configure_openclaw.py` | 为 ce-runner 配置 openclaw 设置 |
| `scripts/run_integration_test.py` | 端到端集成测试,含时间戳与分数校验 |
| `scripts/run_task_compare.py` | 以原生 + ce-runner 两种模式运行任务做对比 |
| `scripts/list_tasks.py` | 按前缀(T/M/C)和难度列出任务 |
| `scripts/debug_task.py` | 单任务交互式调试,输出详细信息 |
| `scripts/analyze.py` | 分析批量 trace 产物 |
| `scripts/summarize_results.py` | 汇总多次运行的批量结果 |
| `scripts/generate_trial_reports.py` | 生成每次试验的详细报告 |
| `scripts/prompt_task.py` | 显示指定任务的 system prompt |
| `scripts/check_api_key.py` | 测试 API key 连通性 |
| `scripts/check_openclaw_env.py` | 检查 openclaw 环境(`--fix` 可清理) |

## 故障排查

| 问题 | 修复 |
|---|---|
| setup 后 `uv` 命令找不到 | `python -m pip install --upgrade uv`(重跑 setup) |
| `command not found: ce-runner` | 使用 `uv run ce-runner ...` 而非裸 `ce-runner` |
| Gateway 服务被禁用 / 未安装 | 重跑 `python scripts/setup_env.py`(自动安装并启动) |
| Gateway 运行中但不可达 | `openclaw gateway restart` |
| Docker permission denied | `sudo usermod -aG docker $USER && newgrp docker` |
| Sandbox 镜像构建慢 / 在 TUNA 上失败 | 已处理 —— setup 会把 Dockerfile 修补到官方 PyPI;需要时通过 `Dockerfile.agent.bak` 恢复 |
| 环境被污染 | `uv run python scripts/check_openclaw_env.py --fix` |
| Mock 服务端口冲突(9100–9116) | `ss -tlnp \| grep 91` 定位后 kill |

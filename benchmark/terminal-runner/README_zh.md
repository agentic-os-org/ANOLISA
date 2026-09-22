# terminal-runner

[English](README.md)

本文介绍如何通过 [harbor](https://github.com/harbor-framework/harbor) 平台测试 terminal-bench 任务，重点对比两种实现方式 — **Installed mode** 和 **External mode** — 的差异、各自优缺点及使用方法。

---

## 两种模式，两条支撑路径

[harbor](https://github.com/harbor-framework/harbor) 在 Docker 容器内运行 agent 评测。它支持两种提供 agent 的方式：

### Installed mode — harbor 原生支持

内建于 harbor。agent 实现 `install()` 方法 — harbor 在 `setup()` 阶段自动调用，在容器内动态安装 agent 运行时（如通过 nvm 安装 Node.js + openclaw）。Docker 镜像保持干净，安装过程每次 trial 都会执行。

```bash
harbor run --agent openclaw ...
```

- **谁来支撑：** Harbor 通过 `--agent <name>` 原生支持。
- **代价：** 每次 trial 重新安装 agent（增加启动耗时）。

### External mode — harbor 的可扩展接口

Harbor 提供 `BaseAgent` + `--agent`，这是一个允许你接入任意运行在容器外部的 agent 的接口 — 宿主机 CLI、HTTP 服务背后的远端 agent，任何形式。外部 agent 通过 `environment.exec()` 与容器通信。

```bash
harbor run --agent-import-path external_agent.openclaw_external_agent:OpenClawExternalAgent ...
```

- **谁来支撑：** Harbor 的 `BaseAgent` API。任何人都可以实现外部 agent。
- **优势：** agent 在宿主机安装一次，跳过每次 trial 的安装开销。容器内无需运行 agent。

### 本仓库

External mode 的一个**可运行示例**，以 OpenClaw 作为宿主机端 agent：

```
OpenClaw (宿主机 LLM) <-> OpenClawExternalAgent (路由器) <-> Harbor Docker 容器
```

同样的模式适用于 Claude Code、Codex 或任何 agent CLI。

---

## 在宿主机安装 OpenClaw

External mode 在宿主机运行 agent CLI。对于本仓库的 `openclaw_external_agent` 来说，需要 `openclaw` CLI 在宿主机 PATH 中可用。如果你自己实现 `xxx_external_agent.py`，则安装对应的 agent CLI 即可。

```bash
# 需要 Node.js >= 22.19（推荐 Node 24）
npm install -g openclaw
openclaw --version
```

API key 和模型配置请参考 [OpenClaw 文档](https://github.com/openclaw/openclaw)。

---

## 仓库内容

```
terminal-runner/
├── external_agent/
│   ├── __init__.py                  # 包标记文件（无导入，懒加载）
│   └── openclaw_external_agent.py   # 外部 agent 适配器（继承 harbor BaseAgent）
├── scripts/
│   └── setup.sh                     # 克隆 harbor 上游 + 数据集，安装 harbor
├── .gitignore
├── LICENSE
├── README.md
└── README_zh.md
```

### 不在仓库内（由 `setup.sh` 克隆）

- `harbor/` — harbor 上游框架
- `dataset/` — terminal-bench 任务数据集

---

## 快速开始

### 前置条件

- Python ≥ 3.12
- Docker ≥ 24，需 **compose v2 插件**（`docker compose version` 必须可用）
- Node.js ≥ 22.19 + `openclaw` CLI 在宿主机 PATH 中（仅 external mode 需要）
- `git`、`git-lfs`

### 安装

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/benchmark/terminal-runner
./scripts/setup.sh
```

`setup.sh` 做了什么：
1. 检查 Python 版本 — 若 < 3.12，自动检测 conda（Miniconda/Anaconda）并创建 3.12 环境（环境名可通过 `CONDA_ENV_NAME` 配置，默认 `terminal-runner-py312`）
2. 创建 Python 虚拟环境 `.venv/`
3. `git clone` harbor 上游 → `harbor/`，**checkout 到锁定的 release tag**（`v0.15.0`），然后在 venv 中 `pip install -e`
4. 通过 Git LFS 从 HuggingFace 克隆数据集 → `dataset/`

> **注意：** Harbor 锁定到特定 release（`v0.15.0`）以保证稳定性。如有需要可通过 `HARBOR_REF` 覆盖。

覆盖源地址：

```bash
HARBOR_URL=git@github.com:your-fork/harbor.git \
HARBOR_REF=v0.15.0 \
DATASET_URL=https://huggingface.co/datasets/<org>/<dataset> \
  ./scripts/setup.sh
```

---

## 使用方法

### External mode（本仓库）

```bash
source .venv/bin/activate
export PYTHONPATH=$(pwd)

harbor run \
  --agent-import-path external_agent.openclaw_external_agent:OpenClawExternalAgent \
  -p dataset/nginx-request-logging \
  -m openai/qwen3.6-plus \
  --agent-env OPENAI_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1 \
  --agent-env OPENAI_API_KEY=sk-xxxxx
```

### Installed mode（harbor 内置）

Installed 模式下，harbor 内置的 `openclaw` agent 在**容器内**运行 OpenClaw（通过 nvm + npm 自动安装）。内置 agent 仅支持固定的一组 provider（`openai`、`anthropic`、`nvidia`）；对于任何 OpenAI 兼容端点（如 DashScope），请使用 `openai` provider 配合 `OPENAI_BASE_URL` / `OPENAI_API_KEY`。Qwen 模型不支持 thinking 模式，需传 `--agent-kwarg thinking=off`。

```bash
source .venv/bin/activate

harbor run \
  --agent openclaw \
  -p dataset/nginx-request-logging \
  -m openai/qwen3.6-plus \
  --agent-env OPENAI_BASE_URL=https://dashscope.aliyuncs.com/compatible-mode/v1 \
  --agent-env OPENAI_API_KEY=sk-xxxxx \
  --agent-kwarg thinking=off
```

### 工作原理（单次迭代）

1. 在宿主机启动 `openclaw --profile <trial-profile> agent --local --json --message ...`
2. 流式读取 stdout/stderr，附带心跳 + 三重安全防护（总超时、无输出超时、stdout 上限）
3. 从 OpenClaw 响应中提取 bash 命令（toolCall 或 `bash` 代码块）
4. 通过 `environment.exec()` 在 harbor 容器中执行每条命令
5. 将结果反馈给 OpenClaw 进入下一轮迭代
6. 当 OpenClaw 输出 `TASK_COMPLETE` 或达到 `OPENCLAW_MAX_ITERATIONS` 时停止

### 每次试验隔离

每次试验获得独立的 `~/.openclaw-<profile>` 目录（profile 名称源自 harbor session id），并发试验互不冲突。

### 自动 skill 提示

Skill 提示按优先级从高到低加载：

1. **`dataset/<task>/skill.md`** — 手动覆盖文件。若存在，内容原样注入（截断至 3000 字符）。始终生效。
2. **`dataset/<task>/solution/solve.sh`** — 自动解析为分步提示。**默认禁用**，设置 `SKILL_FROM_SOLUTION=1` 启用。

数据集目录通过 `DATASET_DIR` 环境变量解析（默认 `dataset`，相对于 `harbor run` 的工作目录）。

---

## 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `OPENCLAW_VERSION` | `2026.4.14` | `version()` 返回的版本号。 |
| `OPENCLAW_AGENT_ID` | `main` | 传给 `--agent` 的 OpenClaw agent-id。 |
| `OPENCLAW_TIMEOUT` | `600` | 单次调用超时（秒），传给 OpenClaw 的 `--timeout`。同时也是子进程总超时守卫的下限。 |
| `OPENCLAW_NO_OUTPUT_TIMEOUT` | `500` | OpenClaw 子进程在无 stdout 输出超过该秒数后被杀死（API 挂起守卫）。 |
| `OPENCLAW_THINKING` | `off` | OpenClaw 思考模式（`off` / `low` / `high`）。 |
| `OPENCLAW_MAX_ITERATIONS` | `0` | 最大 agent 循环迭代次数（0 = 不限）。 |
| `OPENCLAW_MAX_STDOUT_BYTES` | `102400` | stdout 超过该字节数时杀死 OpenClaw 子进程（无限生成守卫）。 |
| `DOCKER_EXEC_TIMEOUT` | `600` | 单条命令 `environment.exec()` 超时（秒）。 |
| `DATASET_DIR` | `dataset` | 任务数据集目录路径（用于定位 `skill.md` / `solution/solve.sh`）。 |
| `SKILL_FROM_SOLUTION` | `0` | 设为 `1` 启用从 `solution/solve.sh` 自动生成 skill 提示。 |

---

## 许可证

Apache License 2.0，与 harbor 许可证一致。

---

## 致谢

- [harbor](https://github.com/harbor-framework/harbor) — agent 评测框架
- [terminal-bench](https://github.com/laude-institute/terminal-bench) — 任务数据集格式
- [OpenClaw](https://github.com/openclaw/openclaw) — 宿主机端 agent CLI

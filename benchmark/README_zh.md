# Benchmark

[English](README.md)

本目录包含面向 AI 基准测试(Bench)的执行 runner。组件专属的基准测试
随组件存放——Tokenless 压缩套件(L1 compressor、L2 module、L3 scenario)
位于 `src/tokenless/benchmark/`。

## Runner 一览

| 目录 | 基准 | 说明 |
|---|---|---|
| `claweval-runner/` | ClawEval(OpenClaw 平台评测套件) | 任务执行与结果工具;固定 openclaw 2026.4.22,需要 Docker 与 Python ≥ 3.11 |
| `swe-runner/` | SWEBench | 面向真实软件工程任务的 Agent 与评测框架(cosh/openclaw agent、trace 提取) |
| `terminal-runner/` | TerminalBench | 基于固定 harbor 检出的适配器;`scripts/setup.sh` 负责克隆 harbor 与 terminal-bench 数据集 |

## AI Bench Agent

上述 runner 为主流 AI 基准测试提供执行 Agent 实现。所有 Agent 均已适配
OpenClaw 平台,并针对各 Bench 中的任务类型做了专项优化,以提升 instance
运行的稳定度与通过率。

目前已覆盖的 Bench 包括:

- **SWEBench** — 面向真实软件工程任务的基准测试,Agent 针对代码理解、定位、修复等流程做了流程编排与容错优化。
- **TerminalBench** — 面向终端交互类任务的基准测试,Agent 对命令执行、输出解析、多步交互场景进行了适配与稳定性加固。
- **ClawEval** — OpenClaw 平台自有评测套件,Agent 以定制化提示词策略与执行逻辑调优覆盖其多样任务类型。

后续将持续接入更多主流基准测试,新的 Agent 实现会沿用同一套适配与
优化范式。

各 runner 的目录中均有独立 README,说明环境搭建与使用方法。

# 测试 ANOLISA

[English](../../en/getting-started/testing.md)

按改动涉及的组件选择检查。先通过[从源码构建](building.md)准备工具链和构建依赖，
再运行下列组件命令。聚合脚本只覆盖部分组件，还可能在前置条件缺失时跳过测试。

## 1. 组件测试和质量门禁

除非命令中带有 `cd`，否则都从仓库根目录执行。以下命令是最小的本地门禁；修改
组件内部实现前，请阅读对应 README 和开发指南。

| 组件 | 测试和质量门禁 |
| ------ | ---------------- |
| [AW](../../../../contribute/zh/aw/building.md) | `python3 src/aw/scripts/check.py`；仅 Linux，使用固定 Rust 工具链、Python 3 和 Node.js。 |
| [os-skills](../../../../src/os-skills/README_zh.md) | 没有编译目标。检查变更过的 `SKILL.md`，并按文件说明的解释器运行变更脚本。 |
| [agent-sec-core](../../../../src/agent-sec-core/README_zh.md) | `cd src/agent-sec-core && make test` 会运行 Python、Rust sandbox 和 OpenClaw plugin 测试。Python 使用 uv 与 Python 3.11.6。 |
| [agentsight](../../../../src/agentsight/README_zh.md) | Linux 使用 `cd src/agentsight && make lint && make test`；macOS 运行 local viewer 和 trajectory collector 相关测试。 |
| [tokenless](../../../../src/tokenless/README_zh.md) | `cd src/tokenless && make lint && make test` |
| [agent-memory](../../../../src/agent-memory/README_zh.md) | `cd src/agent-memory && make fmt-check && make lint && make test`；`cd src/agent-memory && make smoke` 覆盖 MCP stdio 路径。仅 Linux。 |
| [ws-ckpt](../../../../src/ws-ckpt/README_zh.md) | `cd src/ws-ckpt && make test`；安装和 service 检查需要 Linux system mode。 |
| [cosh-ng](../../../../src/cosh-ng/README_zh.md) | `cd src/cosh-ng && cargo fmt --all -- --check`，随后按[贡献指南](../../../../src/cosh-ng/CONTRIBUTING_zh.md)选择最接近改动的测试。只有明确要求对大型或跨模块改动执行完整验证时，才运行全量本地门禁。 |
| [anolisa](../../../../distribution/anolisa/README_zh.md) | `cd distribution/anolisa && cargo fmt --all --check && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked` |
| [SkillFS](../../../../src/skillfs/README_zh.md) | `cd src/skillfs && cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`；Linux 上再运行 `cd src/skillfs && scripts/test.sh` 执行 FUSE smoke test。 |
| [ktuner](../../../../src/ktuner/README.md) | `cd src/ktuner && cargo fmt --all --check && cargo clippy --all-targets -- -D warnings && cargo test` |

Agent Memory 的 `make smoke` 需要 Python 3.8+，在临时 userland 命名空间中检查
release 二进制。它校验 MCP 响应和回读数据，在服务出错或超过协议检查期限时失败，
并在停止子进程后清理临时数据。它不验证特权挂载隔离。
运行 `python3 src/agent-memory/tests/test_smoke.py -v` 可以在不构建组件的情况下
验证 smoke 入口的故障检测。

Agent Memory 可使用 `make smoke SMOKE_TIMEOUT=10` 将协议检查期限设为 10 秒（默认 30 秒）；强制清理进程另有 2 秒上限。沙箱检查会尝试读取临时命名空间外已存在的文件，必须被拒绝。

修改公共 API 或 rustdoc 时，在受影响的 Rust 组件门禁中追加
`cargo doc --workspace --no-deps`。`ktuner tune`、FUSE 挂载、
eBPF tracing 和系统 daemon 需要普通单元测试之外的主机权限或内核能力。

[CI workflow](https://github.com/agentic-os-org/ANOLISA/blob/main/.github/workflows/ci.yaml) 还会为部分组件执行 coverage、打包、前端、
adapter 和集成检查。这些任务比矩阵中的最小本地命令更严格，涉及生成制品或框架
adapter 时应再查看 workflow。

## 2. 聚合测试与 PR 门禁

`tests/run-all-tests.sh` 只是部分组件的便捷测试脚本。请通过 `--filter` 选择
agent-sec-core、agentsight、tokenless 或 agent-memory。它不会测试 cosh-ng、os-skills、
ws-ckpt、anolisa、SkillFS 或 ktuner。

```bash
./tests/run-all-tests.sh --filter sec
./tests/run-all-tests.sh --filter sight
./tests/run-all-tests.sh --filter tokenless
./tests/run-all-tests.sh --filter memory
```

当前脚本在前置条件缺失时会跳过测试。没有 `uv` 时会跳过 agent-sec-core 的 Python
测试，没有 `/usr/local/bin/linux-sandbox` 时会跳过其 sandbox e2e，没有 `cargo` 时
会跳过 AgentSight。只有 `make` 和 `cargo` 都不存在时才会跳过 tokenless。在非 Linux
系统或没有 `cargo` 时会跳过 agent-memory。即使出现这些跳过，脚本仍会打印成功
信息，因此退出码为 0 不能证明
所有测试都已运行。agent-sec-core 的 e2e 调用还依赖当前工作目录布局，可靠的本地
门禁应使用组件 Makefile。

PR 应根据变更文件选择组件矩阵中的对应行，运行相应的构建、lint 和测试命令。变更
涉及的平台、集成、smoke、前端或文档时，还要补充相应检查。聚合脚本适合作为快速
信号，不应作为 PR 的唯一验收依据。

## 3. 延伸阅读

- [从源码构建](building.md)
- [故障排查](troubleshooting.md)
- [贡献指南](../../../../CONTRIBUTING_zh.md)
- [开发指南索引](../../../developer-guide/zh/index.md)

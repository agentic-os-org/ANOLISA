# 安装产物发布前回归

[English](README.md)

这套需手动运行的 Linux x86-64 测试在已有 Claude Code 和 OpenCode 镜像内安装本地
Tokenless npm 包，并在 AgentScope 2.x 镜像内安装本地 Python Wheel，再验证安装后的 Core
与 Integration。镜像只包含 Agent 及其依赖，不预装
Tokenless 或 RTK。运行不会发布包，也不向容器挂载 Tokenless 源码。

## 准备输入

使用现有镜像 `tokenless-test-agent-claude-code:2.1.259` 和
`tokenless-test-agent-opencode:1.18.27`。两者需要 Node.js、npm、Python 3.13、Bash，
以及与输入二进制兼容的 glibc。结果记录 Image ID；这些 Tag 是本地测试资产，不是公开镜像。
AgentScope 使用 `tokenless-test-agent-agentscope2:2.0.7.post1`，预装 Python 3.13 和 Agent
依赖，不需要 Node.js 或 Shell Tool。

构建当前原生二进制，并使用正常打包入口：

```bash
make build
make npm-package
make python-wheel agentscope-wheel
```

准备 `pillarjs/path-to-regexp` 在
`8877f41873e37a30258d3935feaf1d2679321735` 的干净 Checkout，复用 BuildLog 开发期间的
真实项目。用 `npm install --package-lock-only --ignore-scripts` 一次性生成依赖 Lock，
或复用此前运行保留的 Lock。测试会打包已跟踪文件、复制 Lock，再在每个容器内执行
`npm ci`。结果记录 Lock 的 SHA-256；不同 Lock 代表不同的工作负载环境。

## 运行

在 Tokenless 组件根目录执行：

```bash
# Package installation and deterministic Core checks; no model calls.
python3 tests/release_regression/run.py --project /path/to/path-to-regexp

# Also run live Agent tasks in each selected image.
python3 tests/release_regression/run.py \
  --project /path/to/path-to-regexp \
  --api-key-file /tmp/tokenless-openclaw-api-key
```

用 `--agents claude-code`、`--agents opencode` 或 `--agents agentscope2` 做单独迭代。
`--model` 默认为 `deepseek-v4-flash-0731`。真实任务使用
[百炼 TokenPlan 端点](https://help.aliyun.com/zh/model-studio/base-url)。Key 以只读文件挂载，
通过进程环境或 AgentScope 内存中的 Credential 传给 Agent，不复制进包、配置文件或报告。
真实运行会消耗所提供账号的模型额度。

脚本新建 `/tmp/tokenless-release-regression.*` 目录并打印路径。每个 Agent 使用独立的
安装、项目、状态与报告。非零退出表示检查失败。不提供 Key 时真实任务明确标为
`not_run`，不能作为完整的发布验收。

## 证据与验收

- 本套件按需运行，不能替代构建检查。修改恢复 API 时，还必须检查独立的
  `benchmark/l1-compressor` 和 `benchmark/l2-module` 工作区：主工作区的
  `cargo test --workspace` 不会编译它们。按 Tokenless CI 的方式，在每个工作区运行
  `cargo fmt --all -- --check`、`cargo clippy --all-targets --locked -- -D warnings` 和
  `RUST_MIN_STACK=16777216 cargo test --release --locked`。
- 离线安装两个本地 npm 包。实际解析出的可执行文件必须与 `target/release/tokenless`
  的 SHA-256 一致。Plugin 通过安装后的脚本启用，不使用测试替身 Hook。
- AgentScope 在复用镜像已有 Agent 依赖的隔离环境中，使用 `--no-index --no-deps` 安装两份
  输入 Wheel，并记录 Wheel 的 SHA-256。
- 真实工作负载：原样执行 `npm test`，必须报告 484 个测试通过。另用刻意指定的缺失
  Vitest 配置构造失败用例；不声称这个注入的配置错误是自然发生的。
- Core 检查覆盖 BuildLog 缩减、完整数据 TOON 优先、Record Reduction、Tool Error 与
  RTK 旁路、文件及普通文本透传、无收益、Dry-run、未授权 Retrieve 和 CLI 字节级恢复。
- 原有 L1 Records Fixture 验证完整 TOON。派生 Fixture 将 Message 重复一次，使 TOON
  独自节省低于无损候选选择门槛。这个 Fixture 只验证恢复契约，其节省不代表真实工作负载
  表现。先从 Core 确实遗漏的记录中选择目标，再要求模型恢复。
- 每个真实 Shell 任务必须观察到一次生效压缩、一个发出的 Stash Entry、一次模型主动执行的
  独立 Retrieve 命令，以及一次 CLI Retrieve Hit。恢复输出必须旁路压缩。最终回答需要
  引用遗漏的测试或目标记录。
- AgentScope 使用自定义静态 Retrieve Tool 执行合成记录任务，不提供 Shell Tool。记录实际
  模型输入输出，验证工具列表不变、BeforeModel 可见性授权、字节一致恢复、一次 `embedded`
  Retrieve Hit 和不二次压缩。`embedded` 与 CLI 事件分开统计。记录器失败属于测试脚本问题，
  不代表产品恢复失败。
- 新提示必须包含完整 `If needed` 指令和 24 位裸 Hash，不再生成尖括号 Marker。失败尝试和
  超时前的部分输出也保留在日志中。
- 报告分开保留 Provider Usage 和工具输出 Token 估算。
  `saved_minus_retrieved_tokens` 是单次输出节省减去恢复 Payload Token，不是整场会话
  节省或账单估算。宿主末尾换行规范化与 CLI Payload 字节级恢复分开报告。这套测试不充当
  随机化 Agent A/B 实验。

报告、依赖 Lock、日志与 SQLite 证据保留在 Git 之外。本轮不验证 macOS、旧 glibc 兼容性、
其他安装方式或全部 Agent。发布前需要依据记录的证据，分别判断 Provider 失败、宿主交付
限制和 Core 失败。

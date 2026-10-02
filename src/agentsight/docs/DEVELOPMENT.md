# Development Guide — AgentSight

## Prerequisites

| 依赖 | 最低版本 | 用途 |
|------|----------|------|
| Linux kernel | >= 5.8 | BTF 支持，eBPF 运行时 |
| Rust | >= 1.80 | 编译 Rust 代码 |
| clang/llvm | >= 15 | 编译 eBPF C 程序（14 及以下会优化掉 sslsniff/tcpsniff 的长度钳制，导致 verifier 拒绝加载）|
| libbpf | >= 0.8 | eBPF 用户态库 |
| Node.js | >= 16 | 前端构建 |
| npm | >= 8 | 前端依赖管理 |

### 安装构建依赖（CentOS/Alinux）

```bash
yum install -y clang llvm elfutils-libelf-devel libbpf-devel zlib-devel
```

## Build Commands

### 仅构建 Rust 二进制（release）

```bash
cargo build --release
# 产物：target/release/agentsight
```

### 构建前端并嵌入

```bash
cd dashboard
npm install
npm run build:embed    # 产物输出到 frontend-dist/
```

### 完整构建（前端 + Rust）

```bash
make build-all
# 等效于: make build-frontend && make build
```

### 安装到系统

```bash
make install
# 安装到 /usr/local/bin/agentsight 并设置 BPF capabilities
# 可自定义: make install PREFIX=/opt/agentsight
```

### RPM 打包

```bash
make rpm    # 或使用 scripts/rpm-build.sh
```

## macOS 构建

macOS 上 AgentSight 编译为轻量二进制：仅包含 `agentsight serve`（Agent 看板 + 本地会话查看器），不含 eBPF 追踪功能。通过 `#[cfg(target_os = "linux")]` 条件编译实现，同一份源码在 Linux 上编译出完整功能。

### macOS 依赖

| 依赖 | 最低版本 | 用途 |
|------|----------|------|
| Rust | >= 1.80 | 编译 Rust 代码 |
| Node.js | >= 16 | 前端构建 |
| npm | >= 8 | 前端依赖管理 |

> macOS 不需要 libbpf、clang/llvm、内核头文件等 eBPF 相关依赖。

### macOS 构建步骤

```bash
cd src/agentsight

# 构建 agentsight-local 前端和 macOS serve-only 二进制
make build-mac
# 产物：target/release/agentsight
```

`make build-mac` 会先在 `crates/agentsight-local/dashboard` 中构建本地查看器前端，再执行 `cargo build --release --bin agentsight`。macOS 产物只包含 `serve` 子命令，不会构建或加载 eBPF 代码。

### macOS 使用

```bash
# 启动 Agent 看板 + 本地会话查看器
target/release/agentsight serve
# 打开 http://127.0.0.1:7396

# 自定义 host/port
target/release/agentsight serve --host 0.0.0.0 --port 8080
```

### macOS 限制

| 不可用命令 | 原因 |
|-----------|------|
| `trace` | 需要 eBPF 内核探针 |
| `token` / `audit` / `metrics` | 依赖 eBPF 采集的 SQLite 数据 |
| `discover` / `interruption` / `skill-metrics` / `summary` | 依赖 eBPF 追踪数据 |
| `--db` / `--config` 参数 | `serve` 在 macOS 上使用轻量服务器，无 SQLite |

macOS 上 `serve` 提供的功能：
- **Agent 看板** — 扫描本机运行的 AI Agent 进程
- **本地会话查看器** — 读取 `~/.agentsight/sessions/` 下的 ATIF 轨迹文件

## Development Workflow

### 启动追踪 + 服务器

```bash
# Terminal 1: eBPF 追踪（需要 root）
sudo agentsight trace

# Terminal 2: API 服务器 + Dashboard
agentsight serve
# 打开 http://127.0.0.1:7396
```

### 前端开发（热重载）

```bash
cd dashboard
npm install
npm run dev    # http://localhost:3004，代理 API 到 localhost:7396
```

前端开发完成后重新构建嵌入：
```bash
cd dashboard && npm run build:embed
make build    # 重新编译 Rust 以嵌入更新后的前端
```

## Project Structure

```
agentsight/
├── src/           # Rust 源码
│   ├── bpf/       # eBPF C 程序
│   ├── probes/    # 探针管理
│   ├── parser/    # 协议解析
│   ├── aggregator/# 事件聚合
│   ├── analyzer/  # 数据分析
│   ├── genai/     # GenAI 语义层
│   ├── storage/   # SQLite 持久化
│   ├── discovery/ # Agent 发现
│   ├── health/    # 健康检查
│   ├── tokenizer/ # Token 计数
│   ├── atif/      # ATIF 轨迹导出
│   ├── server/    # HTTP API 服务器
│   └── bin/       # CLI 入口
├── dashboard/     # React 前端
├── scripts/       # 部署脚本
└── rpm-sources/   # RPM 打包源
```

详见 → [ARCHITECTURE.md](ARCHITECTURE.md)

## Testing

### Rust 单元测试

```bash
cargo test                    # 运行所有测试
cargo test --lib              # 仅库测试
cargo test -p agentsight -- <test_name>  # 运行特定测试
make test                     # 等效于 cargo test
```

### 代码覆盖率

```bash
make coverage                 # 生成 HTML 覆盖率报告并打开浏览器
make coverage-xml             # 生成 Cobertura XML 报告 (coverage.xml)
```

需要安装 `cargo-llvm-cov`：
```bash
cargo install cargo-llvm-cov
rustup component add llvm-tools-preview
```

### CI 质量门禁

CI (`ci.yaml` 的 `test-agentsight` job) 对每次 PR 执行以下检查：

| 检查项 | 命令 | 失败条件 |
|--------|------|----------|
| 格式化 | `cargo fmt --all --check` | 代码未格式化 |
| 架构边界 | `python3 scripts/check-arch-boundaries.py` | 存在未声明的跨模块依赖 |
| Lint | `cargo clippy --all-targets -- -D warnings` | 存在 clippy 警告 |
| 单元测试 | `cargo test`（通过 `cargo llvm-cov`） | 任何测试失败 |
| 增量覆盖率 | `diff-cover --fail-under=80` | 新增/修改代码行覆盖率 < 80% |

**架构边界检查**：脚本验证 `use crate::` 跨模块引用是否符合 `ARCHITECTURE.md` 声明的层级依赖。若需要新增跨模块依赖，先确认是否合理，然后更新脚本中的 `ALLOWED_DEPS` 和 `ARCHITECTURE.md` 依赖图。若为暂时无法修复的历史违规，添加到 `KNOWN_VIOLATIONS` 并附 issue 链接。

本地运行：
```bash
python3 scripts/check-arch-boundaries.py
```

覆盖率报告在 CI 运行结果页的 **Step Summary** 中可见，完整的 Cobertura XML 可从 **Artifacts** 下载。

本地提交前建议运行：
```bash
make test                     # 快速单元测试（无覆盖率）
make test-coverage            # 带覆盖率的测试（与 CI 一致，生成 coverage.xml）
make coverage                 # 生成 HTML 覆盖率报告并打开浏览器
```

如需本地复现 diff-cover 增量门禁：
```bash
make test-coverage
pip install diff-cover
diff-cover coverage.xml --compare-branch=origin/main --fail-under=80
```

### 前端类型检查

```bash
cd dashboard && npm run typecheck
```

### 手动集成测试

```bash
# 1. 启动追踪
sudo agentsight trace &

# 2. 发起一个 LLM API 调用（使用任何 AI Agent）

# 3. 查询结果
agentsight token --last 1h
agentsight audit --last 1h

# 4. 启动服务器查看 Dashboard
agentsight serve
```

## Code Style

### Rust

- 遵循标准 `rustfmt` 格式：`cargo fmt`
- 使用 `clippy` 检查：`cargo clippy -- -D warnings`
- 错误处理使用 `anyhow::Result`
- 模块组织：每个模块有 `mod.rs`（声明）+ `unified.rs`（统一入口）+ 子模块

### TypeScript (Frontend)

- 使用 TypeScript strict 模式
- Tailwind CSS 类名样式
- 组件放在 `dashboard/src/components/`，页面放在 `dashboard/src/pages/`

## Configuration

运行时配置通过 `AgentsightConfig`（`src/config.rs`），支持以下方式：

1. **默认值**: 内置默认配置
2. **配置文件**: `/etc/agentsight/config.json`（通过 `--config` 指定），**完全替换**内嵌默认规则，详见 AGENTS.md §10
3. **环境变量**: SLS 相关变量、`AGENTSIGHT_TOKENIZER_PATH` 等
4. **CLI 参数**: 各子命令支持 `--verbose`, `--storage-path` 等

### 关键配置项

| 配置 | 默认值 | 说明 |
|------|--------|------|
| `storage_base_path` | `/var/log/sysak/.agentsight` | SQLite 数据库目录 |
| `db_name` | `agentsight.db` | 数据库文件名 |
| `data_retention_days` | 30 | 数据保留天数（0=不限） |
| `connection_cache_capacity` | 24 | HTTP 连接 LRU 缓存大小 |
| `tokenizer_url` | Qwen3.5-27B tokenizer | 默认 tokenizer 下载 URL |

## Debugging

### 启用详细日志

```bash
sudo agentsight trace --verbose
# 或
RUST_LOG=debug sudo agentsight trace
```

### Chrome Trace 导出

```bash
AGENTSIGHT_CHROME_TRACE=1 sudo agentsight trace
# 产物：trace.json，可在 chrome://tracing 查看
```

### 常见问题

| 问题 | 原因 | 解决方案 |
|------|------|----------|
| `Failed to create probes` | 内核不支持 BTF | 检查 `/sys/kernel/btf/vmlinux` 是否存在 |
| `Failed to attach probes` | 权限不足 | 使用 `sudo` 或 `setcap cap_bpf,cap_perfmon=ep` |
| `Frontend not embedded` | 未构建对应前端 | Linux: `make build-all`；macOS: `make build-mac` |
| `Tokenizer not found` | 未下载 tokenizer | 设置 `AGENTSIGHT_TOKENIZER_PATH` 或自动下载 |

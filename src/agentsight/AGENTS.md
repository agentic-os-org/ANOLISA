# AGENTS.md — AgentSight Navigation Map

> AI Agent 可观测性工具，基于 eBPF 捕获 LLM API 调用、Token 消耗和进程行为，无需修改 Agent 代码。

## 0. 硬性规则

### 代码
- 提交前必须运行 `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + `cargo test`
- 禁止在非测试代码中使用 `unwrap()` 或 `expect()` — 用 `?`、`match` 或 `unwrap_or` 替代
- 禁止使用 `dbg!()` — 用 `log::debug!()` 或 `tracing::debug!()` 替代
- 禁止添加 `#[allow(clippy::...)]` 除非附带注释说明原因
- PR diff 不应超过 800 行；复杂逻辑变更应控制在 500 行以内，超出则拆分为多个可 review 的阶段

### 架构
- 禁止高层模块直接 import 低层模块（如 `server/` → `probes/`），遵循 [ARCHITECTURE.md](docs/ARCHITECTURE.md) 中 L0–L8 层级约束
- 优先扩展现有模块，而非创建新文件
- 单模块目标 < 500 行（不含测试）；超过 2,000 行的文件在增加代码前必须先有拆分计划

### 测试
- 流水线逻辑变更（probes → parser → aggregator → analyzer → genai → storage）必须包含集成测试
- 跨模块行为优先写集成测试，而非单元测试
- 测试代码放在独立的 `*_tests.rs` 文件或 `#[cfg(test)] mod tests` 中，避免在主实现中添加仅测试用的函数

### FFI
- 修改 FFI 函数签名时必须同步更新 `cbindgen.toml` 并确认 `build.rs` drift guard 通过
- FFI 类型必须使用 `#[repr(C)]`
- 禁止 panic 穿越 FFI 边界 — 使用 `std::panic::catch_unwind`

### eBPF
- 禁止修改 BPF 程序而不验证 kernel >= 5.8 兼容性
- BPF 变更必须在真实内核上测试，仅编译通过不够

### 配置文件
- 修改 `agentsight.json` 结构（新增/删除/重命名字段、改变语义）时，必须同步 bump `schema_version` 并更新 `config.rs` 中的 `CURRENT_SCHEMA_VERSION` 常量
- 纯新增可选字段且旧配置完全兼容时，不需要 bump `schema_version`

### 用户文档
用户指南位于 `docs/user-guide/{en,zh}/agent-observability/agentsight/`，中英双语必须同步改，只改一侧视为未完成。

改动落在下面这些面上时，必须在同一个 PR 里更新对应页面：

| 改动内容 | 必须同步的页面 |
|----------|----------------|
| 新增/删除子命令，或改动参数、默认值、`possible_values` | `cli-reference.md` |
| `agentsight.json` 字段、默认值、`features` 开关、`runtime_limits` | `configuration.md` |
| `InterruptionType` 枚举、severity 默认值、检测阈值、DeadLoop 处置 | `interruption-detection.md` |
| API 路由增删（`/api/docs` 清单变化）、数据库文件、保留与容量策略 | `data-and-storage.md` |
| Dashboard 页面增删、导航能力探测（`capabilities`）、认证行为 | `dashboard.md` |
| systemd unit、安装路径、容器/Sidecar 要求、macOS 能力边界 | `deployment.md` |
| 伴生组件集成（tokenless / agent-sec-core / enforcer）表现变化 | `integrations.md` |

硬性要求：

- 文档中的命令与输出必须在真实环境验证过，禁止凭源码推测排版；示例中的会话/对话/中断 ID、Token 数、主机名与 IP 必须是一眼可辨的占位值（如 `00000000-0000-0000-0000-000000000001`、`203.0.113.10`），禁止粘贴真实采集数据
- 不要在正文中钉未发布的补丁版本号，用 `0.11`、`0.11.x` 这类版本序列表述
- 已知缺陷（CLI 与界面提示不一致、参数覆盖不全等）以显式 caveat 写清楚，不要静默省略
- 截图放在 `docs/images/agentsight/{en,zh}/`，用相对路径引用；网站构建由 `website/scripts/prepare-docs.mjs` 自动改写路径并拷贝到静态目录，新增图片必须被某个页面引用，否则不会进入站点
- 文档变更后本地至少跑通：`bash scripts/docs-lint.sh`、`python3 scripts/docs-link-check.py`、`npm run build --prefix website`

## 1. Quick Start

```bash
make build-all          # 构建前端+Rust二进制
sudo agentsight trace   # 启动 eBPF 追踪
agentsight serve        # 启动 API 服务器 + Dashboard UI（http://127.0.0.1:7396）
```

详见 → [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)

## 2. Architecture

数据流水线：`Probes → Parser → Aggregator → Analyzer → GenAI → Storage`

```
eBPF Probes → Event → Parser → ParsedMessage → Aggregator → AggregatedResult
                 ↓                                                  ↓
           ProcMon/SSL                                     Analyzer → AnalysisResult
                                                                    ↓
                                              GenAIBuilder → GenAISemanticEvent → Exporter
                                                                    ↓
                                                              Storage (SQLite/SLS)
```

详见 → [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)

## 3. 代码表面增长控制（Footprint Ladder）

新功能必须选择能解决问题的**最高级别**（最少代码表面）。只有当高级别确实无法实现时，才降级到下一级。

| 级别 | 手段 | 新增代码表面 | 何时使用 | 示例 |
|------|------|------------|---------|------|
| **1** | 扩展现有函数/方法 | 最少 | 功能可通过修改现有逻辑实现 | 在 `AgentScanner` 中新增一个 Agent 匹配规则 |
| **2** | 模块内新增 helper 函数 | 少 | 需要复用逻辑但不改变模块接口 | 在 `parser/sse/` 中提取内部解析 helper |
| **3** | 新增模块文件 | 中 | 职责明确独立，现有模块无法容纳 | 新建 `src/interruption/` 模块 |
| **4** | 新增 eBPF 探针 | 大 | 需要捕获新的内核/用户态事件 | 新增 `src/bpf/gotls.bpf.c` + Rust wrapper |
| **5** | 新增 `extern "C"` FFI 导出 | 最大 | 需要暴露新能力给 C 调用方 | 新增 FFI 函数（须同步 cbindgen.toml + drift guard） |

**规则**：
- 从级别 1 开始评估，逐级下降，在 PR 描述中说明为什么当前级别不够
- 禁止直接跳到级别 3-5 而不先考虑是否可以扩展现有代码
- 级别 4-5 的变更必须在 PR 中附带架构影响说明

## 4. Module Map

| 模块 | 位置 | 职责 | 关键类型 |
|------|------|------|----------|
| **Probes** | `src/probes/` | eBPF 探针管理 | `Probes`, `ProbesPoller`, `SslSniff`, `ProcMon`, `FileWatch`, `FileWriteProbe`, `UdpDns`, `TcpSniff`, `codex_offsets`, `elf_buildid` |
| **Event** | `src/event.rs` | 统一事件枚举 | `Event::{Ssl, Proc, ProcMon, FileWatch, FileWrite, UdpDns}` |
| **Parser** | `src/parser/` | 协议解析（HTTP/1.x, HTTP/2, SSE, ProcTrace） | `Parser`, `ParsedMessage` |
| **Aggregator** | `src/aggregator/` | 请求-响应关联 + SSE continuation buffer | `Aggregator`, `AggregatedResult` |
| **Analyzer** | `src/analyzer/` | Token/审计/消息分析 | `Analyzer`, `AnalysisResult` |
| **GenAI** | `src/genai/` | 语义事件构建+导出 | `GenAIBuilder`, `GenAISemanticEvent`, `GenAIExporter` |
| **Storage** | `src/storage/` | SQLite 持久化 | `Storage`, `SqliteStore`, `AuditStore`, `TokenStore` |
| **Discovery** | `src/discovery/` | Agent 进程发现 | `AgentScanner`, `AgentMatcher`, `known_agents` |
| **Health** | `src/health/` | Agent 健康检查 | `HealthChecker`, `HealthStore` |
| **Tokenizer** | `src/tokenizer/` | LLM Token 计数 | `LlmTokenizer`, `MultiModelTokenizer` |
| **ATIF** | `src/atif/` | 轨迹格式导出（转换逻辑；数据模型来自 `agentsight-atif`） | `convert_trace_to_atif`, `convert_session_to_atif` |
| **Server** | `src/server/` | HTTP API + 嵌入式前端 | `AppState`, `run_server` |
| **Container** | `src/container.rs` | 容器 ID 提取（/proc/pid/cgroup） | `extract_container_id`, `parse_container_id_from_cgroup` |
| **Config** | `src/config.rs` | 统一配置 | `AgentsightConfig` |
| **Unified** | `src/unified.rs` | 主编排器 | `AgentSight` |
| **Opt** | `crates/agentsight-opt/` | 三维优化分析（准确性/性能/成本），workspace 成员 crate | `AnalyzePipeline`, `LlmClient`, `Trajectory` |
| **OptStore** | `crates/agentsight-opt-store/` | 优化结果 SQLite 持久化（optimization.db） | `OptimizationStore`, `Dimension` |
| **SQLiteLifecycle** | `crates/agentsight-sqlite-lifecycle/` | 无业务模型依赖的 SQLite 连接、容量计量、checkpoint 与清理策略驱动 | `ConnectionOptions`, `SizeSnapshot`, `SizePolicy` |
| **Atif (v1.7)** | `crates/agentsight-atif/` | ATIF v1.7 公共 schema 叶子 crate，唯一的 ATIF 数据模型（采集链路 + 主 crate 导出链路共用） | `AtifTrajectory`, `Step`, `ATIF_SCHEMA_VERSION` |
| **TrajectoryCollector** | `crates/agentsight-trajectory-collector/` | 定时扫描 Qoder/QoderWork 会话目录，JSONL → ATIF v1.7 入库（trajectories.db，仅 trace 模式，默认关闭）；serve 侧经 `/api/trajectories` 只读查询 | `CollectorConfig`, `run_collector_loop`, `TrajectoryStore` |

## 5. Critical Code Paths

1. **SSL 捕获流程**: `sslsniff.bpf.c` → `Probes::run()` → `Event::Ssl` → `Parser::parse_ssl_event()` → `HttpConnectionAggregator` → `Analyzer::analyze_aggregated()` → `Storage::store()`
2. **Agent 自动发现**: `procmon.bpf.c` → `Event::ProcMon::Exec` → `AgentSight::handle_procmon_event()` → `AgentScanner::on_process_create()` → `Probes::attach_process()`
3. **Token 提取**: `SSE Parser` → `TokenParser::parse_event()` → `TokenRecord` → `TokenStore::add()`
4. **GenAI 语义构建**: `AnalysisResult` → `GenAIBuilder::build()` → `GenAISemanticEvent::LLMCall` → `GenAIExporter::export()`

## 6. eBPF Probes

| 探针 | BPF 程序 | 功能 |
|------|----------|------|
| sslsniff | `src/bpf/sslsniff.bpf.c` | uprobe on SSL_read/SSL_write 捕获加密流量明文 |
| proctrace | `src/bpf/proctrace.bpf.c` | tracepoint on execve 捕获命令行参数 |
| procmon | `src/bpf/procmon.bpf.c` | 进程创建/退出事件（Agent 发现） |
| filewatch | `src/bpf/filewatch.bpf.c` | 监控 .jsonl 文件打开事件 |
| filewrite | `src/bpf/filewrite.bpf.c` | fentry on vfs_write 捕获 .jsonl 写入内容 |
| udpdns | `src/bpf/udpdns.bpf.c` | fentry on udp_sendmsg 捕获 DNS 查询（域名→IP）|
| tcpsniff | `src/bpf/tcpsniff.bpf.c` | fentry on tcp_recvmsg/sendmsg 捕获明文 HTTP 流量 |

构建时 `build.rs` 通过 `libbpf-cargo` 自动生成 eBPF skeleton。

### Codex CLI 适配（三级回退）

Codex CLI 静态链接 aws-lc/BoringSSL，无导出符号。`attach_process` 使用三级回退策略：

1. **符号表查找**（Tier 1）：`nm` 读取 `.symtab` / `.dynsym` 中的 `SSL_write_ex` / `SSL_read_ex`
2. **字节模式匹配**（Tier 2）：扫描 `.text` 段中的 BoringSSL 函数 prologue 模式
3. **Offset 表查找**（Tier 3）：`agentsight.json → codex_offsets.entries` 按 fingerprint（file_size + head_64k_sha256 + BuildID）匹配

使用 `scripts/extract-codex-offsets.py` 提取新版本 offset 并更新 `agentsight.json`。
详见 [Codex CLI 适配文档](docs/codex-adaptation.md)。

### SSE Continuation Buffer

OpenAI Responses API 的 `response.completed` 事件可能跨多个 TLS record。
`HttpConnectionAggregator` 在 SseActive 状态下为 `/v1/responses` 路径缓冲原始 SSL 字节，
供下游 `Analyzer::extract_token_from_sse` 在标准 SSE 解析失败时回退扫描。

## 7. CLI Subcommands

| 命令 | 入口 | 功能 |
|------|------|------|
| `agentsight trace` | `src/bin/cli/trace.rs` | eBPF 追踪（需 root）；`--no-ebpf` 仅跑轨迹采集，无需特权 |
| `agentsight serve` | `src/bin/cli/serve.rs` | API + Dashboard 服务器 |
| `agentsight token` | `src/bin/cli/token.rs` | 查询 Token 消耗 |
| `agentsight audit` | `src/bin/cli/audit.rs` | 查询审计事件 |
| `agentsight discover` | `src/bin/cli/discover.rs` | 发现运行中的 AI Agent |
| `agentsight metrics` | `src/bin/cli/metrics.rs` | Prometheus 格式指标 |
| `agentsight interruption` | `src/bin/cli/interruption.rs` | 查询/管理会话中断事件 |
| `agentsight dashboard` | `src/bin/cli/dashboard.rs` | 查看 Dashboard 认证状态与 Token |

### 6.1 Interruption CLI 详细用法

查询和管理 AI Agent 会话中断事件。数据存储于 SQLite 数据库。

**数据库路径**: `/var/log/sysak/.agentsight/interruption_events.db`（可通过 `--db` 覆盖）

**中断类型**:

| 类型 | 含义 | 默认严重级别 |
|------|------|-------------|
| `llm_error` | HTTP 状态码 >= 400 或 SSE body 包含 `{"error":...}` | high |
| `sse_truncated` | SSE 流未收到 `finish_reason=stop` 即终止 | high |
| `context_overflow` | 上下文长度超限（`context_length_exceeded`） | high |
| `agent_crash` | Agent 进程在会话中途消失（OOM/signal） | critical |
| `token_limit` | `finish_reason=length` 且 `output_tokens >= max_tokens * 0.95` | medium |

**严重级别**: `critical` > `high` > `medium` > `low`

**子命令**:

```bash
# 列出中断事件（默认最近24小时，最多100条）
agentsight interruption list [--last <HOURS>] [--type <TYPE>] [--severity <LEVEL>] [--agent <NAME>] [--unresolved|--resolved] [--limit <N>] [--json]

# 按类型统计中断数量
agentsight interruption stats [--last <HOURS>] [--json]

# 按严重级别统计未解决的中断数量
agentsight interruption count [--last <HOURS>] [--json]

# 获取单个中断事件详情
agentsight interruption get <INTERRUPTION_ID> [--json]

# 列出指定 session 的所有中断
agentsight interruption session <SESSION_ID> [--json]

# 列出指定 conversation 的所有中断
agentsight interruption conversation <CONVERSATION_ID> [--json]

# 标记中断为已解决
agentsight interruption resolve <INTERRUPTION_ID>

# 使用自定义数据库路径
agentsight interruption --db /path/to/interruption_events.db list --last 48
```

## 8. API Endpoints

| 路径 | 方法 | 功能 |
|------|------|------|
| `/health` | GET | 健康检查 |
| `/metrics` | GET | Prometheus token 指标 |
| `/api/sessions` | GET | 会话列表 |
| `/api/sessions/search` | POST | 语义会话搜索（复用优化 LLM，Body: `{"query","candidates":[{session_id,first_message,last_message,project}]}`，候选 ≤200、≤5 跳过 LLM）；排除有效标签为 `useless` 的会话，标签库不可用时退化为不过滤 |
| `/api/sessions/{id}/traces` | GET | 会话下的 trace |
| `/api/traces/{id}` | GET | 单次调用详情（按 per-call response_id 查询，非 conversation_id） |
| `/api/conversations/{id}` | GET | conversation 事件详情 |
| `/api/agent-names` | GET | Agent 名称列表 |
| `/api/timeseries` | GET | 时序 Token 统计 |
| `/api/metrics/latency` | GET | LLM latency and throughput percentile metrics |
| `/api/agent-health` | GET | 历史 Agent 活动（SQLite 聚合） |
| `/api/agent-process-health` | GET | 当前 Agent 进程健康状态 |
| `/api/agent-health/{pid}` | DELETE | 删除健康条目 |
| `/api/agent-health/{pid}/restart` | POST | 重启 Agent |
| `/api/export/atif/trace/{id}` | GET | ATIF trace 导出 |
| `/api/export/atif/session/{id}` | GET | ATIF session 导出 |
| `/api/export/atif/conversation/{id}` | GET | ATIF conversation 导出 |
| `/api/token-savings` | GET | Token 节省统计（`start_ns`, `end_ns`, `agent_name`） |
| `/api/interruptions` | GET | 中断事件列表（`start_ns`, `end_ns`, `agent_name`, `type`, `severity`, `resolved`, `limit`） |
| `/api/interruptions/count` | GET | 中断计数按严重级别（`start_ns`, `end_ns`, `agent_name`）— 固定只统计 `resolved=false`，不接受 `resolved` 参数 |
| `/api/interruptions/stats` | GET | 中断按（类型, 严重级别）统计（`start_ns`, `end_ns`）— 同样固定只统计 `resolved=false` |
| `/api/interruptions/session-counts` | GET | 按 session 分组的未解决中断计数（`start_ns`, `end_ns`, `agent_name`），NULL session_id 归入 `__unassigned__` |
| `/api/interruptions/conversation-counts` | GET | 按 session + conversation 分组的未解决中断计数（`start_ns`, `end_ns`, `agent_name`），NULL session_id / conversation_id 归入 `__unassigned__`；session 维度不可省略，否则会话未识别的中断会被计入拥有该 conversation 的 session |
| `/api/interruptions/{id}` | GET | 单个中断事件详情 |
| `/api/interruptions/{id}/resolve` | POST | 标记中断为已解决 |
| `/api/sessions/{id}/interruptions` | GET | 指定 session 的所有中断 |
| `/api/conversations/{id}/interruptions` | GET | 指定 conversation 的所有中断 |
| `/api/auth/login` | POST | Dashboard 登录（Body: `{"token":"..."}` ），成功设置 httpOnly cookie |
| `/api/auth/status` | GET | 返回 `{"auth_enabled": bool, "capabilities": [...]}`（免认证）；`capabilities` 为动态探测结果，取决于宿主机上安装的伴随组件（agent-sec / enforcer / tokenless），供前端 NavBar 过滤不可用页面 |
| `/api/auth/verify` | GET | 校验当前 session cookie/token 是否有效，返回 `{"authenticated": bool}` |
| `/api/optimize/sessions/{id}/{dim}` | POST | 运行单维度优化分析，`dim` ∈ `perf` / `perf-issues` / `cost` / `cost-waste` / `accuracy` / `summary`（后四者需 LLM 配置，10–60s；`summary` 为单次调用叙事摘要） |
| `/api/optimize/sessions/{id}/results` | GET | 读取已持久化的优化分析结果 |
| `/api/optimize/results` | GET | 分析历史列表（`start_ns`, `end_ns`, `limit` ≤ 200；默认最近 30 天，仅返回各维度存在标记，不含 payload） |
| `/api/optimize/config` | GET/POST | 优化 LLM 配置（api_key 脱敏；持久化到 `optimization_config.json`） |
| `/api/reuse/triage` | POST | 用确定性规则为已采集轨迹生成自动标签（可选 `session_id`, `limit`） |
| `/api/reuse/sessions` | GET | 列出有效标签已解析的轨迹标签（支持 `label`, `confirm_state`, `changed_since_decision`, `limit`） |
| `/api/reuse/sessions/{session_id}/label` | POST | 人工确认或覆盖单条轨迹标签；只有人工或 LLM 可以标为 `bad` |
| `/api/reuse/sessions/labels:batch-confirm` | POST | 批量确认自动标签 |
| `/api/reuse/label-stats` | GET | 按规则统计人工接受与覆盖情况 |
| `/api/reuse/judge` | POST | 用已配置 LLM 判定规则无法归类的轨迹；仅在 `features.reuse_llm_judge=true` 时可用，调用会产生费用 |
| `/api/preferences` | GET | 用户偏好分析（规则 + 可选 LLM） |
| `/api/preferences/export` | GET | 以 Markdown 导出用户偏好 |
| `/api/preferences/turns` | GET | 供 Agent 侧 LLM 推理使用的原始用户轮次 |
| `/api/trajectories` | GET | 采集轨迹列表（`project`, `source`, `agent_name`, `limit`, `label`, `exclude_label`, `human_backed`；不含 `atif_json`，按采集时间倒序） |
| `/api/trajectories/filters` | GET | 轨迹过滤下拉选项（distinct project/source/agent_name） |
| `/api/trajectories/steps` | GET | 按步骤分类检索（`category` 逗号分隔多值 OR：`user_input`/`system`/`agent_message`/`thinking`/`tool_call`/`tool_result`；另支持 `agent_name`, `project`, `source`, `session_id`, `limit`, `context`, `max_scan`）。每条命中附带同会话前后各 `context` 条步骤；分类为多标签，非法 `category` 返回 400 |
| `/api/trajectories/{session_id}` | GET | 单条轨迹的原始 ATIF v1.7 JSON（store 不可用或 session 不存在均返回 404，消息不同；列表/过滤/步骤端点则降级为空 + 200） |
| `/api/storage/status` | GET | 各 SQLite store 的生效策略、物理/逻辑占用和治理覆盖状态，不返回文件路径 |

## 9. Frontend

React + TypeScript + Webpack + Tailwind CSS，位于 `dashboard/`。开发: `npm run dev`(localhost:3004)，嵌入构建: `npm run build:embed`。

## 10. Configuration

`AgentsightConfig`（`src/config.rs`），关键环境变量：SLS_*（阿里云日志服务导出）、`AGENTSIGHT_TOKENIZER_PATH`、`AGENTSIGHT_CHROME_TRACE`、`RUST_LOG`、`AGENTSIGHT_SSL_REATTACH_TTL_SECS`（SSL uprobe 陈旧重挂载 TTL，默认 30 秒；用于内核静默注销 uprobe consumer 的 serverless/overlayfs 场景，如 ACS；设为 `0` 表示每次匹配进程都强制重挂载，仅供测试）。

### 配置文件加载语义

Agent 规则配置文件路径：`/etc/agentsight/config.json`（可通过 `--config` 覆盖），格式参见项目根目录 `agentsight.json`。

**重要：用户配置文件会完全替换（replace）内嵌的默认规则，而非追加（extend）。** 如果配置文件中缺少某个 Agent 的规则（如 `*claude*`），该 Agent 将不会被发现。修改配置前请确保包含所有需要监控的 Agent 规则。

### schema_version 配置升级机制

`agentsight.json` 顶层包含 `schema_version` 字段，标记当前配置格式的版本。程序启动时通过 `ensure_default_agents_config` 检查磁盘上配置文件的 `schema_version`：

- **版本缺失或过旧**：先把旧文件复制为 `config.json.bak.<unix秒>`，再用当前内嵌默认配置整体替换；旧字段不迁移
- **版本一致或更新**：保留用户自定义配置不动
- **RPM 安装**：使用 `%config(noreplace)`，RPM 升级不覆盖磁盘文件，由程序自身的 schema_version 检查处理升级

**修改 `agentsight.json` 时的检查清单**：
1. 是否新增/删除/重命名了字段？→ bump `schema_version` + 更新 `CURRENT_SCHEMA_VERSION`
2. 是否改变了字段语义（如默认值翻转）？→ bump `schema_version` + 更新 `CURRENT_SCHEMA_VERSION`
3. 是否纯新增可选字段（旧配置完全兼容）？→ 无需 bump

### 功能开关（`features`）

通过 `agentsight.json` 的 `features` 区块独立控制各可选功能的启停。关闭后对应模块不实例化（`Storage::noop()` / `InterruptionDetector::disabled()` / `ResponseSessionMapper::disabled()`），减少内存和 I/O 开销。

| 功能 | JSON 路径 | 默认值 | 说明 |
|------|-----------|--------|------|
| Token 统计 | `features.token_stats` | `true` | 核心功能 |
| 本地 Tokenizer | `features.tokenizer.enabled` | `false` | HuggingFace 模型 fallback |
| Session 映射 | `features.session_mapping.enabled` | `true` | responseId → sessionId |
| SQLite 存储 | `features.sqlite_storage.enabled` | `true` | 关闭后用内存 noop store |
| 中断检测 | `features.interruption_detection.enabled` | `true` | 死循环/崩溃检测 |
| 审计 | `features.audit` | `true` | LLM 调用审计持久化 |
| Token 消费 | `features.token_consumption` | `false` | 聚合消费记录 |
| SLS Logtail | `features.sls_logtail` | `false` | SLS 日志文件导出 |
| 轨迹采集 | `features.trajectory_collection.enabled` | `false` | 定时扫描 Qoder/QoderWork 会话目录，JSONL 转 ATIF v1.7 存入 trajectories.db（仅 trace 模式；`scan_interval_secs` 默认 30，`scan_dirs` 可覆盖扫描目录） |
| 轨迹 LLM 判定 | `features.reuse_llm_judge` | `false` | 允许 `POST /api/reuse/judge` 调用已配置 LLM 判定规则无法归类的轨迹；每次调用会产生费用 |

### 运行时资源上限（`runtime_limits`）

通过 `runtime_limits` 配置有界缓冲区上限，防止内存无限增长。

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `event_channel_capacity` | 10,000 | Probe 事件有界通道容量 |
| `event_channel_policy` | `backpressure` | 满载策略：backpressure / drop_newest / sample |
| `event_channel_max_bytes_mb` | 64 | 排队事件的字节预算（单条 SSL 记录可达 4 MiB，槽位数无法限定内存）|
| `pending_genai_max_count` | 1,000 | 等待 session_id 的最大事件数 |
| `pending_genai_max_bytes_mb` | 64 | 等待 session_id 的最大字节数 |
| `pid_cache_size` | 1,024 | PID → agent_name LRU 缓存 |
| `max_connection_body_mb` | 8 | 单 HTTP 连接 body 缓冲上限 |
| `connection_idle_timeout_secs` | 60 | HTTP 连接 idle 超时 |
| `ring_buffer_mb` | 32 | eBPF Ring Buffer 大小（必须为 2 的幂） |

## 11. Design Docs

- [eBPF Probes 设计](docs/design-docs/ebpf-probes.md)
- [数据流水线设计](docs/design-docs/data-pipeline.md)
- [GenAI 语义层设计](docs/design-docs/genai-semantic.md)
- [Codex CLI 适配](docs/codex-adaptation.md) — 三级回退 offset 查找 + SSE continuation buffer
- [常见踩坑记录](docs/PITFALLS.md) — AI agent 和新贡献者最容易踩的坑
- [架构决策记录（ADR）](docs/adr/) — 关键架构选型的背景和理由

## 12. Scoped Rules

高风险模块有独立的边界约束文件：

| 模块 | 规则文件 | 关注点 |
|------|----------|--------|
| FFI 导出层 | [src/FFI_AGENTS.md](src/FFI_AGENTS.md) | ABI 安全、cbindgen 同步、panic 隔离 |
| 主编排器 | [src/UNIFIED_AGENTS.md](src/UNIFIED_AGENTS.md) | 禁止业务逻辑、保持委托模式 |
| 存储层 | [src/storage/AGENTS.md](src/storage/AGENTS.md) | SQL 注入防护、schema 兼容、mutex 处理 |

## 13. Prerequisites

- Linux kernel >= 5.8（BTF 支持）
- Rust >= 1.80
- clang/llvm >= 15（eBPF 编译；14 及以下会优化掉长度钳制，sslsniff/tcpsniff 无法通过 verifier）
- libbpf >= 0.8

## 14. User-Facing Documentation Guidelines

### Value Proposition
- Lead with "zero-instrumentation eBPF observability" — the user installs AgentSight and gets full tracing without touching their Agent code. This is the single most important differentiator.

### Gotchas to Warn About
- root/CAP_BPF is required for `agentsight trace` — users who forget get silent "no events" behavior. Every usage section must note this. `--no-ebpf` is the unprivileged fallback and runs the trajectory collector alone; never present it as a substitute for eBPF-derived data (token metering, audit, interruption detection).
- Config file semantics: user-provided `config.json` **replaces** the built-in defaults entirely (no merge). Users who partially customize lose Agent discovery rules for unmentioned Agents.

### Content Decisions
- SLS export: mention the capability exists ("supports external log service export"), never include SLS-specific configuration (endpoints, access keys)
- Interruption detection: this is a unique capability competitors lack — deserves its own section, not a buried bullet point
- Token savings page (Tokenless integration): document as cross-component feature — "install both, savings appear in Dashboard automatically"

### Terminology
- Use "interruption" not "failure" or "error" for the detection feature (the enum is `InterruptionType`, not `ErrorType`)
- Use "Agent auto-discovery" not "process scanning" (user-facing language)

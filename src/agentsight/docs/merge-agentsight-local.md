# agentsight-local 合并改造方案

## 一、背景与问题

当前 `agentsight` 工作区包含 6 个 crate:

| Crate | 作用 | 平台 |
|---|---|---|
| `agentsight` (根) | 主 crate,eBPF 管线 + Linux server + CLI | Linux |
| `agentsight-local` | macOS 本地轨迹查看器 server | 非 Linux |
| `agentsight-trajectory-collector` | JSONL 文件扫描 → ATIF 转换 → SQLite 存储 | 跨平台 |
| `agentsight-atif` | ATIF v1.7 数据模型 | 跨平台 |
| `agentsight-opt` | LLM 优化分析管线 | 跨平台 |
| `agentsight-opt-store` | 优化结果存储 | 跨平台 |

**问题:**

1. `agentsight-local` 是独立 crate,但与主 crate 耦合极强 — `serve` 在 macOS 上直接调用 `agentsight_local::server::run_server()`
2. `agentsight-local` 描述写着 "No eBPF, no SQLite",但实际已使用 `TrajectoryStore`(SQLite)和 collector 扫描逻辑 — 描述过时
3. macOS 架构与 Linux 不一致 — macOS 的 `serve` 既采集又展示,Linux 的 `trace` 采集、`serve` 展示
4. `trace` 命令被 `#[cfg(target_os = "linux")]` 门控,macOS 完全不可用,但 `trace` 里的轨迹收集器是纯用户态的

## 二、关键发现

### trace 有两条路径

`unified.rs` 的 `AgentSight::new()` 中,有两条独立的数据路径:

1. **eBPF 管线**(Linux 专有):probes → parser → aggregator → analyzer → genai → storage → `agentsight.db`
2. **轨迹收集器**(纯用户态):`run_collector_loop()` → `discover_sessions()` → `convert_qoder_events()` → `TrajectoryStore.upsert()` → `trajectories.db`

路径 2 **不依赖 eBPF**,只读 JSONL 文件 + 写 SQLite。当前它被包在 `#[cfg(target_os = "linux")]` 的 `unified.rs` 里,所以 macOS 完全用不到。

### macOS serve 已在内部启动 collector

`agentsight-local` 的 `run_server()` 在启动时做了两件事:
1. `scan_once()` + 后台 `run_collector_loop()`(300s)— 采集轨迹到 `trajectories.db`
2. 注册 HTTP 路由,从 `trajectories.db` 读数据展示

这导致 macOS 的 `serve` 既采集又展示,与 Linux 的 "trace 采集 / serve 展示" 架构不一致。

## 三、改造思路:统一 trace/serve 架构

### 核心改动:macOS 也启用 trace,只走收集器路径

```
改造前:
  Linux:   trace (eBPF + collector) → serve (读 DB)
  macOS:   serve (采集 + 展示)        ← 架构不一致

改造后:
  Linux:   trace (eBPF + collector) → serve (读 DB)
  macOS:   trace (collector only)   → serve (读 DB)  ← 统一
```

macOS 的 `agentsight trace` 只运行轨迹收集器(不启动 eBPF probes),`agentsight serve` 只从 DB 读数据。两个平台架构一致。

### 改造后数据流

```
macOS:
  agentsight trace
    │
    └─→ run_collector_loop()         ← 仅此一条路径
           ├─→ discover_sessions()       (agentsight-trajectory-collector)
           ├─→ convert_qoder_events()   (agentsight-trajectory-collector)
           └─→ TrajectoryStore.upsert() → trajectories.db

  agentsight serve
    │
    ├─→ [HTTP API] /api/trajectories       → 读取 trajectories.db
    ├─→ [HTTP API] /api/local-sessions     → 本地 discover_local_sessions()(富元数据)
    ├─→ [HTTP API] /api/local-session/atif → 本地 convert_jsonl_to_atif()(按需转换)
    ├─→ [HTTP API] /api/agents             → sysinfo 进程扫描
    └─→ [HTTP API] /api/optimize/...       → 读取 trajectories.db + agentsight-opt
```

### 具体改动

#### 1. macOS trace 命令(新增)

在 `src/bin/cli/trace.rs` 中增加 `#[cfg(not(target_os = "linux"))]` 分支:

```rust
#[cfg(not(target_os = "linux"))]
impl TraceCommand {
    fn run_tracing(&self) {
        // 只跑轨迹收集器,不启动 eBPF
        let db_path = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("agentsight/trajectories.db");
        let config = CollectorConfig {
            scan_interval_secs: 300,
            scan_dirs: local_trajectory_scan_dirs(),
            db_path: db_path.clone(),
        };
        let store = TrajectoryStore::new_with_path(&db_path).unwrap();
        agentsight_trajectory_collector::scan_once(&store, &config);
        // Ctrl+C → stop
        let stop = AtomicBool::new(true);
        run_collector_loop(&config, &stop);  // 前台阻塞
    }
}
```

macOS 的 `trace` 命令简化参数 — 不需要 `--enable-filewatch`、`--daemon` 等 eBPF 专用选项。

#### 2. macOS serve 命令(简化)

`local/server.rs` 的 `run_server()` 移除 collector 启动逻辑:
- 删除 `scan_once()` 调用
- 删除 `run_collector_loop()` 后台线程
- 只打开 `TrajectoryStore` 用于读取(如果 DB 文件不存在则返回空)

这与 Linux 的 `serve` 行为一致 — `serve` 不采集,只展示。

#### 3. 合并 agentsight-local 到 src/local/

将 `agentsight-local` 的代码移入 `src/local/`,用 `#[cfg(all(feature = "server", not(target_os = "linux")))]` 控制:

```
src/agentsight/src/
├── lib.rs                           # cfg gate
├── local/                           # ← 从 agentsight-local 移入
│   ├── mod.rs                       #   pub mod collector; pub mod server;
│   ├── server.rs                    #   run_server() — 移除 collector 启动
│   ├── server/
│   │   ├── agents.rs                #   sysinfo 进程扫描
│   │   ├── local_sessions.rs        #   文件浏览 API
│   │   ├── optimize.rs              #   优化分析 API
│   │   └── trajectories.rs          #   轨迹查询 API(只读)
│   ├── collector.rs                 #   模块聚合
│   └── collector/
│       ├── converter.rs             #   JSONL → ATIF(按需)
│       └── discovery.rs             #   本地文件发现(富元数据)
├── server/                          # Linux-only server
├── bin/
│   ├── agentsight.rs                # CLI 入口(serve + trace 均跨平台)
│   └── cli/
│       ├── serve.rs                 # serve 命令(cfg 分支)
│       └── trace.rs                 # trace 命令(cfg 分支:eBPF vs collector)
└── ...
```

#### 4. CLI 命令门控调整

| 命令 | 改造前 | 改造后 |
|---|---|---|
| `serve` | `#[cfg(feature = "server")]` (跨平台) | 不变 |
| `trace` | `#[cfg(target_os = "linux")]` (Linux only) | `#[cfg(feature = "server")]` (跨平台) |
| `token` | `#[cfg(target_os = "linux")]` | 不变 |
| `audit` | `#[cfg(target_os = "linux")]` | 不变 |
| 其他 | `#[cfg(target_os = "linux")]` | 不变 |

`trace.rs` 内部用 cfg 分支:
- Linux:走 `AgentSight::new()` → eBPF 管线 + collector
- macOS:走 `run_collector_loop()` → 仅 collector

#### 5. Cargo.toml 调整

- 移除 `agentsight-local` 作为 workspace member 和依赖
- 添加 `sysinfo = "0.33"` 和 `dirs = "6"` 为 `cfg(not(target_os = "linux"))` 目标依赖
- 添加 `actix-web` macros 为 dev-dependency(测试用)

#### 6. collector 跨平台改进

`default_scan_roots()` 改用 `dirs::home_dir()` 代替硬编码 `/root` + `/home/*`,让 collector 在 macOS 上也能独立运行。

### 为何保留本地 discovery + converter

| | collector 的 `discover_sessions()` | local 的 `discover_local_sessions()` |
|---|---|---|
| **用途** | 后台增量扫描 → 写入 SQLite | 按需文件浏览 → 返回 HTTP 响应 |
| **返回数据** | 最小:path, project, session_id, is_subagent, source | 富元数据:message_count, first_message, file_size_kb, modified_ts, agent_name, agent_icon |
| **文件读取** | 不读取(仅路径);后续 `process_session` 才读取 | 立即读取(快速字符串匹配统计消息数) |
| **增量跳过** | 是(size + mtime 对比) | 否(每次请求都扫描) |
| **子代理发现** | 是(`<uuid>/subagents/agent-*.jsonl`) | 否 |
| **调用频率** | trace 后台每 300s 一次 | 用户每次打开文件浏览器 |

两套逻辑服务不同场景,合并后都保留。

## 四、改造步骤

1. **collector 跨平台**:`default_scan_roots()` 使用 `dirs::home_dir()`,添加 `dirs` 依赖
2. **移动文件**:`crates/agentsight-local/src/*` → `src/local/*`
3. **修改路径引用**:
   - `include_dir!` 路径: `$CARGO_MANIFEST_DIR/../../frontend-dist` → `$CARGO_MANIFEST_DIR/frontend-dist`
   - 模块路径: `crate::collector` → `crate::local::collector`
   - 二进制入口: `agentsight_local::server::run_server` → `crate::local::server::run_server`
   - 自排除字符串: `"agentsight-local"` → `"agentsight"`
4. **简化 serve**:从 `local/server.rs` 的 `run_server()` 移除 `scan_once` + `run_collector_loop`
5. **新增 macOS trace**:`trace.rs` 增加 `#[cfg(not(target_os = "linux"))]` 分支,只跑 collector
6. **更新 CLI 门控**:`trace` 命令从 `#[cfg(target_os = "linux")]` 改为 `#[cfg(feature = "server")]`
7. **更新 Cargo.toml**:移除 agentsight-local,添加非 Linux 依赖
8. **cfg gate**: `#[cfg(all(feature = "server", not(target_os = "linux")))] pub mod local;`
9. **删除** `crates/agentsight-local/` 目录
10. **验证**: `cargo fmt`, `cargo clippy`, `cargo test`, 推送到 PR #1609

## 五、不改变什么

- **API 端点**:所有路由和响应格式不变
- **前端**:同样的静态资源
- **测试**:所有测试跟随代码迁移
- **Linux 行为**:`trace` 和 `serve` 在 Linux 上的行为完全不变
- **数据存储**:`trajectories.db` schema 不变

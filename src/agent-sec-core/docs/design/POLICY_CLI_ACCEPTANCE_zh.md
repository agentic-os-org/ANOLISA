# V2 Policy CLI 工作包验收记录

| 属性 | 值 |
| --- | --- |
| 源码基线 | `main@8bf150c24482de42cf5f4be580b56d8c6bf3a376` |
| 验收日期 | 2026-09-07 |
| 当前状态 | CLI/PAP Rust 集成测试与非 root daemon binary CRUD 通过；CLI＋daemon 双进程 E2E 暂缓；不是 release/distribution ready |
| 产品范围 | Policy、Scope、Binding 各 create/get/list/update/delete，共 15 条命令 |
| 权威输入 | 当前 `asc-daemon-protocol` 的 `pap-methods.json`、`pap-crud-e2e.json` 和领域类型 |

## 1. 实现边界与直接依赖

`agent-sec-cli` 是对外 binary 名；内部 Cargo package 为 `asc-cli`，源码目录
仍是 `v2/apps/asc-cli`。`asc-cli` crate 包含命令解析、Policy 输入适配和 Policy 输出层；`asc-daemon-client` 只负责
一次 UDS 请求、字节上限、deadline、响应解析和 transport 错误。客户端返回完整
`DaemonResponse`，不将其统一降成 JSON result 或决定 CLI 退出码。未来其它命令可增加
输入和输出适配，当前不提前实现 Action、Query、TUI、V1 wire adapter 或通用 action RPC。

运行时依赖方向：

```text
asc-cli -> asc-daemon-client -> asc-daemon-protocol
        -> asc-daemon-protocol / asc-policy-types / asc-foundation-types
```

CLI 与 client 不在运行时依赖 PAP、Repository、Compiler、daemon handler/service 或
Reconciler。CLI 测试依赖服务端组件，仅用于真实 CLI 子进程与 UDS 集成。当前协议 crate
已经与服务端实现分离，本变更不复制 domain DTO、不修改 PAP 协议。

客户端同步阻塞调用者线程，不创建 Tokio runtime 或后台线程。连接使用 `socket2`
的有界连接，发送和接收使用标准库 `UnixStream`，每次 I/O 前根据单调时钟计算同一
deadline 的剩余预算。请求编码及大小检查先于 I/O 计时，收到完整 frame 后再解码。
`socket2` 复用 lockfile 中已有版本；CLI 的 Tokio 依赖仅用于测试服务端，普通运行时
依赖树不包含 Tokio。daemon 的异步运行方式不变。

命令入口 `apps/asc-cli/src/commands.rs` 仅声明顶层命令并分派请求构造；
`commands/policy.rs`、`commands/scope.rs`、`commands/binding.rs` 分别拥有各自
参数和正式 DTO 映射。`commands/common.rs` 只共享分页、ID/revision 校验和请求编码。
新增命令族时新增模块并注册到顶层，不在入口堆叠业务参数，也不引入动态插件或通用 CRUD 框架。

直接依赖 revision 是上述基线中的 protocol/types；新增 CLI 与 client 同一变更验收。
直接消费者证据是 CLI 的 15 method serialized mapping、CLI 子进程经过 daemon
Dispatcher/PapHandler/PapService 的完整 frozen scenario，以及独立 mock UDS client 测试。

## 2. V1 relationship 与兼容分类

| 项目 | 分类 | 验收方式 |
| --- | --- | --- |
| `agent-sec-cli` Policy 命令面 | `GREENFIELD_CONTRACT` | 新增 V2 PAP command mapping 与输出/退出码 fixtures |
| `asc-daemon-client` | 当前 V2 协议的 `ADAPTER_CONFORMANCE` | 实际 socket bytes、LF/EOF、deadline、limits、typed response |
| V1 Python CLI | 无修改、未替代 | 不宣称全量命令或 wire 等价；不依赖 Python/PyO3 runtime |
| POC `poc.*` | 历史参考，不作为产品兼容承诺 | 顶层保留 policy/scope/binding；正式 wire 采用当前 `policy.*` |

服务端生成所有 CREATE ID，不引入 POC 的可选 Binding ID。Policy/Scope get/delete
要求 exact current revision，Binding get/delete 只需要 ID。UPDATE 是完整更新，不做
upsert 或 CLI 侧读改写。LIST 默认 100/0、只取一页；返回 `{items,total}`。Scope create/update
支持正 PID 或 cgroup ID，互斥。Binding intent 不等于实际 enforcement。

V1 支持面今后的迁移仍需逐项兼容分类和对应 oracle；本工作包的扩展能力以依赖/输入/
transport/输出边界验收，不用尚未实现的 V1 stub 或假设的通用响应模型充数。

## 3. Internal contract change record

| ID | 变更 | 影响 |
| --- | --- | --- |
| CCLI-CR-001 | 增加 `asc-cli` 与独立 `asc-daemon-client` | CLI 参数解析采用 clap，版本统一写在 workspace，更新 Cargo.lock；无新增服务端 RPC |
| CCLI-CR-002 | 固定当前 Policy 输出/退出码 | result JSON/stdout/0；Binding CREATE/UPDATE/DELETE 返回 APPLY_FAILED/DELETE_FAILED 时保持完整 result JSON/stdout，但退出 1；GET/LIST 查询 Failed 记录仍退出 0；daemon error envelope JSON/stderr/1；本地执行失败 1；用法错误 2 |
| CCLI-CR-003 | 一次调用 deadline、LF/EOF、4 MiB wire bounds | 不重试未知结果、不增加 wire timeout 字段；默认 5000 ms，可用正 u32 覆盖 |
| CCLI-CR-004 | 输入文件读取与完整模板解码属于 CLI | 文件相对 CLI cwd 解析；保留空格和 OS-native 路径；重复键在 Value 之前拒绝；领域编译仍在 PAP |
| CCLI-CR-005 | daemon 增加可重复 `--policy-admin-uid <UID>` 启动配置 | 默认 root-only 保持；内核 peer UID 匹配部署配置，不跳过授权；运行时委派仍需 root；每次启动重新配置 |
| CCLI-CR-006 | `asc-daemon-client::call` 从 async API 改为同步 `Result` API | 唯一产品消费者 CLI 直接调用，移除 runtime；库调用者须移除 await；参数、wire、退出码、默认 5000 ms 总 deadline、未知结果与不重试语义保持 |
| CCLI-CR-007 | 按命令族拆分参数和请求构造模块 | 顶层仅保留注册与分派；现有 15 method fixture、参数验证及进程场景复验，外部命令语法不变 |
| CCLI-CR-008 | Rust CLI binary 命名为 `agent-sec-cli`，crate 名保留 `asc-cli` | Cargo bin target、help/version、错误前缀与进程测试同步；只改变 V2 产品命令名，不安装或覆盖现有 V1 Python CLI |

## 4. 可执行 pass/fail matrix

下列 Rust 路径相对于 `v2/`；测试函数是可执行门禁，不以文档 ID 代替测试。

| ID | 验收项 | executable evidence | 结果 |
| --- | --- | --- | --- |
| CCLI-001 | 全部 15 条命令、默认值与正式 params 一致 | `apps/asc-cli/tests/commands.rs::all_fifteen_commands_match_frozen_wire_parameters`，消费 `pap-methods.json` 并比对 PAP_METHODS 集合 | PASS |
| CCLI-002 | 参数边界、重复/互斥/缺失、空格与 OS-native 路径、文件/JSON 错误 | `commands.rs` 的参数与文件测试 | PASS |
| CCLI-003 | 帮助、版本、输出、退出码与 daemon 缺失；DPROC-011 partial | `commands.rs::binary_help_version_and_failures_have_stable_exit_codes`、`result_rendering_keeps_domains_and_errors_separate` | PASS |
| CCLI-004 | 真实 CLI 子进程执行完整 15 步 CRUD，与 frozen 完整结果及 socket 请求比较 | `apps/asc-cli/tests/pap_process.rs::real_cli_processes_execute_the_complete_frozen_pap_crud_scenario` | PASS；测试服务端 PrincipalPolicy，真实 UDS，process-local Repository |
| CCLI-005 | 15 method 均经服务端授权拒绝 | `pap_process.rs::unauthorized_cli_cannot_read_or_modify_any_pap_resource` | PASS |
| CCLI-006 | 领域错误不被 CLI 改写、total 与单页、不隐式读取 | `pap_process.rs::domain_validation_and_pagination_are_owned_by_the_daemon` | PASS |
| CCLI-007 | 分段 LF、不等待 EOF、EOF frame、typed daemon error、空/非法响应 | `crates/asc-daemon-client/tests/transport.rs` | PASS |
| CCLI-008 | 总 deadline 覆盖 blocked write/分段 read、精确 LF-inclusive 边界、不自动重试 | `transport.rs` 的 deadline、frame limit、no replay assertions | PASS |
| CCLI-009 | 真实 CLI＋daemon 非 root 授权与配置生命周期 | 暂无自动化入口 | DEFERRED：双进程 E2E 暂缓 |
| CCLI-010 | 真实 CLI＋daemon 非 root 完整 CRUD | 暂无自动化入口 | DEFERRED：双进程 E2E 暂缓 |
| CCLI-012 | 真实 daemon binary 启动和非 root 只读授权 | `asc-daemon/tests/bootstrap.rs::dproc_configured_administrator_can_query_without_root` | 默认 Client 凭据不参与启动；验证查询及退出；完整 CRUD 进程 E2E 单独验收 |
| CCLI-013 | startup UID 校验及管理员不可继续委派 | daemon CLI `administrator_uids_are_explicit_repeatable_and_bounded`、core `startup_administrators_are_explicit_and_cannot_delegate` | PASS |
| CCLI-011 | workspace test、Clippy、fmt、Rustdoc、lockfile 和 diff | 以下命令 | PASS |
| CCLI-014 | 无 runtime 的同步 UDS 与共享 deadline | `transport.rs` 普通 `#[test]`：LF/EOF、精确 frame 边界、分段读、阻塞写、写后读取共享预算、连接前超时、Linux 满连接队列、不重放；`cargo tree -p asc-cli --edges normal` | PASS：13 个同步 transport 测试；普通依赖树无 Tokio |
| CCLI-015 | 独立构建对外 binary，不依赖 workspace feature 合并 | `cargo build -p asc-cli --bin agent-sec-cli --locked`；CLI 进程测试通过 `CARGO_BIN_EXE_agent-sec-cli` 启动 | 本地 PASS；未接入 CI 独立构建步骤 |
| CCLI-016 | 非 UTF-8 内联 socket 路径不能绕过跨层级重复选项检查 | `commands.rs::repeated_socket_options_reject_non_utf8_inline_paths_at_every_level`；覆盖两种选项语法、路径顺序及子命令层级，单一路径保持原始字节 | PASS：解析为 ArgumentConflict，真实 CLI 退出 2 |

## 5. 复现命令

### 开发构建与手动连接

从 `src/agent-sec-core/v2` 构建；package 名与 binary 名有意区分：

```bash
cargo build -p asc-cli -p asc-daemon --locked
./target/debug/agent-sec-cli --help
./target/debug/agent-sec-cli policy update --help
./target/debug/agent-sec-cli --version
```

CLI 独立构建也必须通过 `cargo build -p asc-cli --bin agent-sec-cli --locked`，
不能仅靠 workspace feature 合并通过构建。同步 socket 经 `OwnedFd` 转换为标准库
`UnixStream`，不依赖 daemon 间接启用的 `socket2/all`。

开发环境可由 daemon 启动者显式配置管理员 UID。`SOCKET` 应设为该环境可用的绝对
路径；下面两条命令分别在服务端和客户端终端执行：

```bash
./target/debug/asc-daemon serve --socket "$SOCKET" --policy-admin-uid "$(id -u)"
./target/debug/agent-sec-cli --socket "$SOCKET" policy list
```

`--policy-admin-uid` 属于 daemon 启动配置，可重复并支持 `--policy-admin-uid=<UID>`；
UID 为十进制 u32，重复值去重，非法值在 socket bind 前拒绝。省略时只授权 root，
root 始终获授权。配置的管理员不能在运行时继续委派，重启时需重新提供名单。
该配置按内核 peer credentials 授权，不修改 socket 权限或 system-level 部署形态。

构建、启动和 Rust 集成测试步骤集中维护于本节；中英文 Policy 用户指南只维护命令用法、
参数、输出与当前能力限制。

### 自动化验证

从 `src/agent-sec-core/v2` 执行：

```bash
cargo build -p asc-cli --bin agent-sec-cli --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
cargo doc --workspace --no-deps --locked
cargo build --workspace --bins --locked
./target/debug/agent-sec-cli --version
cargo tree -p asc-cli --edges normal
cargo +1.88.0 check --workspace --all-targets --locked
git diff --check
```

同步客户端与命令拆分复验：workspace tests、Clippy、fmt、Rustdoc、binary build 和
Rust 1.88.0 `check --workspace --all-targets --locked` 全部通过。
此结果不包含性能基准，不据此宣称 CPU、RSS 或启动时间改善幅度。

## 6. 范围限制与回滚

当前保留两条 Rust 集成路径：真实 CLI 子进程连接测试进程内的 daemon service，以及
真实 daemon binary 接受测试客户端的 UDS 请求。CLI 与 daemon binary 共同运行的
双进程 E2E 暂缓，不计为现有自动化门禁。存储仍为 process-local，未验证 durable
persistence、重启恢复、Reconciler、AgentSight/ActPlane 或内核 enforcement。现有
`TODO(policy-response-bounds)` 保留：提交后的超大 mutation response 可能失败，客户端
只报告错误与未知结果，不提高上限也不重试。

回滚时停止使用V2 `agent-sec-cli` binary，回退本变更引入的两个 crate、workspace/lockfile
条目及配套文档/测试。同时撤回新增 daemon 管理员 UID 启动参数及对应 constructor，并从启动配置中移除该参数。
不需要数据迁移，daemon wire、状态模型和 V1 Python CLI 未被修改。已通过 CLI 提交的 PAP mutation 不随 CLI 回滚自动撤销，需按 PAP 当前状态显式处理。

仅回滚 CCLI-CR-006 时，须同时恢复客户端 async API、CLI runtime 调用及对应 transport
测试和 Cargo 依赖；无需更改 daemon、协议、启动参数或数据。

### OTel tracing 接线后的兼容证据

Rust CLI 增加 legacy `--trace-context` 和 native `--otel-context` 输入；client 在 child span 内
注入可选 version 1 carrier。原有 CLI parsing、stdout/默认 stderr、exit code 与 PAP CRUD fixtures
继续验收；wire golden 比较前单独校验新增 carrier，原 method/params 保持精确比较。
默认无 exporter，CLI 不启动应用级 Tokio runtime；独立 worker 的退出等待上限 50 ms。
对应代码、测试、未迁移实际 hook 的范围和部署回滚见 [V2 OTel 验收](V2_OTEL_ACCEPTANCE_zh.md)。

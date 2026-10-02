# AgentSec daemon 当前控制面行为规范

| 属性 | 值 |
| --- | --- |
| 状态 | V1 Python 行为基线、兼容语料及仓库内 V2 目标 |
| 实现核对日期 | 2026-08-19 |
| 实现核对提交 | `fe58ed4b23b8` |
| 适用实现 | 当前 Python daemon oracle；目标 Rust asc-daemon |

## 1. 文档地位

本文固化 AgentSec daemon 控制面的 V1 外部可观察行为，为 Rust V2 提供 discovery、
regression oracle 和 compatibility fixtures。V2 产品架构、进程拓扑和迁移策略由仓库内
迁移总计划定义；本文不得以 V1 当前形态覆盖 V2 决策；权威关系见
[`AGENT_SEC_RUST_MIGRATION_zh.md`](AGENT_SEC_RUST_MIGRATION_zh.md#1-文档状态与仓库内权威关系)。
本文不规定 async runtime、线程模型、crate、Python module 或内部类名；
compatibility adapter 必须满足本文、
[`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md) 和相同的 conformance tests，
即可作为兼容实现。

本文使用以下规范用语：

- **必须**：兼容实现不可改变的行为。
- **应当**：默认要求；偏离时必须有明确理由、测试和评审。
- **可以**：不构成兼容边界的实现选择。

本文使用四类范围标记：

- **[CURRENT]**：当前 Python 实现已经具备的可观察行为；
- **[PRESERVE V1]**：supported V1 兼容接口在兼容期必须保持的外部行为；
- **[TARGET V2]**：与仓库内迁移总计划一致、当前 Python daemon 尚未实现的目标；
- **[SUPERSEDED]**：已经被当前仓库 V2 架构取代的旧目标；
- **[HISTORICAL]**：仅作为设计依据，不构成当前或目标协议。

本文未显式标记的控制面行为只属于 **[CURRENT]**。只有进入 supported compatibility
inventory 的语义才能标记为 **[PRESERVE V1]**；Python worker、Python class/module、GIL、
per-user 路径和 user service 不因当前存在而自动成为 V2 要求。

当前 Python 实现是迁移期的行为 oracle；Rust 实现是替代实现，不得把 Python 的类结构
翻译成 Rust API。若源码、测试和本文发生冲突，必须先确认是实现缺陷还是文档漂移，
在同一个变更中更新文档与双实现测试，不能静默选择其中一方。后台 Job 的完整生命周期、
调度、日志、trace、失败、恢复和 Rust 目标机制由
[`DAEMON_JOB_CONTRACT_zh.md`](DAEMON_JOB_CONTRACT_zh.md) 统一定义；本文只保留控制面集成
摘要。进程入口、安装布局、systemd、重启、数据目录和日志交付由
[`DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md`](DAEMON_PROCESS_DEPLOYMENT_CONTRACT_zh.md)
统一定义。

## 2. 范围与职责边界

**[CURRENT]** daemon 是本机、当前用户范围内的可选控制面，当前承担：

1. Unix domain socket 的创建、监听和清理。
2. 单实例、连接上限、请求读取超时、响应大小限制和优雅退出。
3. daemon method allowlist、请求上下文归一化、方法超时和结构化错误。
4. health、安全事件查询和可观测性查询。
5. SkillFS 变更通知的可选双向认证。
6. Skill Ledger activation 后台任务、debounce、启动 reconcile 和 worker 生命周期。
7. daemon 生命周期、请求和后台任务的结构化诊断日志。

daemon 当前不承担：

- 通用 TCP、HTTP 或远程网络监听；
- 八个 security middleware action 的执行入口；
- `prompt_scan` 模型状态管理或扫描执行；
- 所有普通 daemon 方法的通用身份认证；
- 动态 method/plugin 注册；
- 收到 SIGHUP 后重载配置；
- 以 daemon 内存状态作为安全事件、Skill Ledger 或 observability 的唯一事实来源。

**[TARGET V2]** asc-daemon 是 one daemon per host 的 system-level service；agent-sec-cli 是 Rust
daemon client，不拥有 daemon 生命周期，也不提供 PyO3 NativeExecutor 或通用本地 fallback。
同一 daemon 服务多个 UID/Agent，并通过 trusted Principal、authorization 和 QueryScope
隔离访问。上述 V2 目标不改写本节记录的 V1 当前事实。

## 3. 逻辑组件

```text
local client
    |
    | Unix stream, one request per connection
    v
transport -> request parser -> gateway -> method allowlist -> handler
                 |              |              |             |
                 |              |              |             +-- SQLite readers
                 |              |              |             +-- Skill Ledger enqueue
                 |              |              +-- per-method timeout/access metadata
                 |              +-- trace context, validation hook, access log
                 +-- byte/type validation and daemon-owned request ID

daemon lifecycle -> runtime paths/lock/socket -> background jobs -> graceful drain
```

组件边界是逻辑边界。Python 可以使用 module/class，Rust 可以使用 crate/trait/task；
外部调用方不得依赖内部组织方式。

## 4. 进程生命周期

### 4.1 启动

兼容实现必须按下列可观察顺序建立服务：

1. 解析 socket path。
2. 若配置了 SkillFS notify 认证 key，先安全加载并校验 key；失败时不得启动 job、
   bind socket 或留下 socket 文件。
3. 将 daemon 创建文件的进程 umask 设为 `0077`。
4. 创建或校验 runtime directory。
5. 获取非阻塞单实例文件锁，并把当前 PID 写入 lock file。
6. 若 socket path 已存在，先探测 `daemon.health`：可达时拒绝启动第二实例；不可达时
   只允许删除一个真实 Unix socket，普通文件、目录或其它文件类型必须报错。
7. 启动已注册后台 job。
8. bind Unix socket，并把 socket mode 设为 `0600`。
9. 服务可接受请求后记录 `daemon_started`。

后台 job 当前先于 socket bind 启动。这一顺序意味着 bind 失败时必须停止已启动 job，
释放 lock，删除本实例拥有的 socket，并恢复 umask。

### 4.2 运行状态

runtime `status` 的当前值域为：

| 值 | 含义 |
| --- | --- |
| `ok` | 正常接收请求 |
| `stopping` | 已开始退出，不再接收新工作 |

`daemon.health` 必须读取轻量 runtime 状态，不得为了 health 初始化 scanner、模型或其它
重型 capability。

### 4.3 信号

- SIGTERM 和 SIGINT 必须触发正常停止。
- SIGHUP 当前必须被忽略，并记录 `daemon_sighup_noop`；不得重载配置。
- task cancellation 必须进入与信号停止相同的资源清理路径。

### 4.4 停止

停止顺序必须满足：

1. 将 runtime 标记为 `stopping`。
2. 停止 accept 新连接。
3. 等待现有连接完成，当前默认 drain 上限为 2 秒。
4. 超过 drain 上限仍未结束的连接任务必须被取消，并产生失败 completion log。
5. 停止所有后台 job 和其 worker 子进程。
6. 只删除本实例 bind 的同一 socket inode；不得删除后来被其它进程替换的路径。
7. 释放单实例 lock，并恢复进程原 umask。
8. 记录 `daemon_stopped`。

## 5. Runtime path、所有权和权限

socket path 按以下优先级解析：

1. 进程参数显式指定的路径；
2. `AGENT_SEC_DAEMON_SOCKET`；
3. `$XDG_RUNTIME_DIR/agent-sec-core/daemon.sock`；
4. 当 `XDG_RUNTIME_DIR` 缺失且 `/run/user/<uid>` 已存在时，使用
   `/run/user/<uid>/agent-sec-core/daemon.sock`；
5. 否则启动失败，错误分类为 `unavailable`。

runtime directory 必须：

- 是真实目录而不是 symlink；
- 由当前用户拥有；
- mode 精确为 `0700`；
- 若由 daemon 新建，创建后显式设为 `0700`；
- 若已存在但权限不是 `0700`，必须拒绝使用，不能自动放宽或收紧用户已有目录。

默认 lock file 名为 `daemon.lock`，创建 mode 为 `0600`；socket mode 为 `0600`。
锁的作用域是 socket 所在 runtime directory，每个 scope 只允许一个 daemon 实例。

`daemon.lock` 正常停止后保留。再次启动时，若文件存在但没有进程持有 `flock`，当前实现
复用该 inode、truncate 并写入新 PID；若锁仍被持有则拒绝第二实例。当前 Python 实现没有
对已存在 lock path 验证 symlink、regular file、owner 和现有 mode；该行为只属于
**[CURRENT]** 已知安全缺口，不属于 **[PRESERVE V1]**。目标安全要求见进程与部署契约。

## 6. 连接、并发和背压

### 6.1 连接模型

- transport 是 Unix `SOCK_STREAM`。
- 每个连接只处理一个业务请求并返回一个业务响应，然后关闭。
- daemon 不支持同一连接上的 pipeline、multiplex 或长连接复用。
- 普通未认证请求允许以换行或 EOF 结束第一帧；认证帧和认证业务帧必须以换行结束。

### 6.2 上限

当前默认值：

| 项目 | 默认值 |
| --- | ---: |
| 最大活动连接 | 64 |
| 请求 frame | 4 MiB |
| 响应 frame | 4 MiB |
| 首个请求读取超时 | 5000 ms |
| 默认方法执行超时 | 5000 ms |
| 调用方可请求的最大执行超时 | 300000 ms |
| 优雅退出 drain | 2 s |

达到活动连接上限时，新连接必须收到 `busy` 错误后关闭。当前没有等待队列；health 中的
`queues.queued` 当前保持 `0`。method metadata 中的 `queue` 只用于分类，尚不构成独立
queue、优先级或每类并发门禁。Rust 实现不能在兼容迁移中把它解释成新的排队语义。

`queues.inflight` 只统计已经通过 parse、prepare 和 validator，并进入 dispatch 的请求。
非法请求不得增加或窃取 inflight 计数。

## 7. Gateway 与方法调度

请求进入 gateway 后依次执行：

1. 归一化 caller 提供的 trace context。
2. 根据 method metadata 判断是否写 access log。
3. 在 request-local scope 中安装 trace context 和 daemon request ID。
4. 写 `daemon_request_started`（该 method 启用 access log 时）。
5. 执行统一 validator。
6. 增加 inflight。
7. 从静态 allowlist 查找 method，并按调用方 timeout 或 method 默认 timeout 执行。
8. 把 handler result 归一化为 daemon response。
9. 在所有正常、错误和取消路径减少 inflight。
10. transport 写出响应后写 completion log。

当前 validator 是 no-op 扩展点。除协议字段校验和 method handler 自身校验外，不能宣称
当前 daemon 已实施额外的通用 authorization、policy 或 schema validation。

当前七个 dashboard query handler 是同步函数，并在 asyncio event-loop task 内直接执行
SQLite 读取。同步调用返回前没有 await/yield，因此慢查询会阻塞 accept、其它连接和 timeout
推进；包在 `asyncio.wait_for` 中不能抢占正在执行的同步 SQLite 调用。这是 **[CURRENT]**
实现缺口而不是必须保持的协议行为。**[TARGET V2]** Rust daemon 必须把 SQLite、文件、模型、
CPU 和 subprocess 工作放入明确、有界的 blocking resource class，并冻结排队、timeout、
cancellation 和 shutdown 语义。

handler 可以同步或异步实现，但逻辑结果只能是：

- 结构化 handler result；或
- JSON object，自动作为 response `data`。

其它返回类型必须转成 `internal_error`，不得向调用方暴露任意异常细节。

## 8. 当前方法清单

当前默认 allowlist 恰好包含 9 个方法：

| 方法 | 生命周期分类 | queue 标签 | 默认超时 | access log |
| --- | --- | --- | ---: | --- |
| `daemon.health` | `admin` | `admin` | 1000 ms | 否 |
| `skill_ledger.skillfs_notify_change` | `skill_ledger` | `skill_ledger` | 1000 ms | 是 |
| `sec.summary` | `dashboard query` | `dashboard` | 5000 ms | 是 |
| `sec.events.list` | `dashboard query` | `dashboard` | 5000 ms | 是 |
| `sec.events.get` | `dashboard query` | `dashboard` | 5000 ms | 是 |
| `sec.events.count_by` | `dashboard query` | `dashboard` | 5000 ms | 是 |
| `obs.sessions.list` | `dashboard query` | `dashboard` | 5000 ms | 是 |
| `obs.runs.list` | `dashboard query` | `dashboard` | 5000 ms | 是 |
| `obs.timeline.get` | `dashboard query` | `dashboard` | 5000 ms | 是 |

method 的 wire contract 由
[`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md) 定义。新增、重命名或删除方法
必须同时更新该文档、registry inventory test 和 Python/Rust protocol fixtures。

历史 `scan-prompt`/`scan_prompt` 不是当前注册方法。源码树中的
`daemon/handlers/prompt_scan_protocol.md` 只作为历史资料，不具有规范地位。

## 9. Skill Ledger 后台任务

本节是当前控制面摘要；完整 Job 机制及验收 ID 见
[`DAEMON_JOB_CONTRACT_zh.md`](DAEMON_JOB_CONTRACT_zh.md)。

### 9.1 注册和启动

当前默认只注册一个 job：`skill-ledger-activation`。job 启动后：

1. 状态变为 `running`；
2. 创建循环任务；
3. 枚举显式 managed Skill 目录；
4. 为每个目录加入 `reconcile` 事件；
5. 默认 debounce 500 ms 后逐个处理合并后的变更。

默认目录发现项不等同于 managed 目录；启动 reconcile 只处理 resolver 返回的 managed
Skill 目录。

### 9.2 合并规则

pending 以 canonical Skill directory 为 key：

- 同一目录的多个通知合并 `eventKinds` 和 `paths`；
- 后到且非空的 reported Skill ID 覆盖前值；
- `enqueue` 第一次加入返回 newly queued，已存在时返回 coalesced；
- job 被取消时，尚未处理的本批变更必须重新放回 pending。

### 9.3 当前 worker 边界

当前 Python daemon 延迟启动一个串行、持久 Python worker 子进程。每次只处理一个
Skill 变更，单次默认超时 300 秒。transport 失败、EOF、协议错误或超时后，当前请求
最多重启 worker 并重试一次；业务执行错误不得触发 worker 重启。stop 时先关闭 stdin，
2 秒内未退出则 terminate，再等待 5 秒，仍未退出则 kill。

该 Python worker 是 **[CURRENT]** 实现细节，不是长期架构约束。**[TARGET V2]** Rust daemon
通过 JobSupervisor 调用 Rust Skill Ledger application service，不保留永久 Python worker；
debounce、合并、重试边界、状态、持久化副作用和取消行为按 compatibility inventory 验收。

## 10. SkillFS notify 认证

`AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` 只保护
`skill_ledger.skillfs_notify_change`，不是通用 daemon 认证。

- 未配置 key：普通明文 notify 保持兼容。
- 配置 key：notify 必须完成 version 1 的双向 HMAC 握手，并对请求、响应业务 frame
  分别认证；普通 health/query 仍使用明文协议。
- 配置 key 后收到明文 notify、错误 key、伪造 auth frame、超时、截断或超大 auth
  frame 时，必须在 dispatch 前静默关闭连接，不返回可供探测的错误内容。
- 已认证连接只允许 notify 方法；请求其它方法必须返回经过认证的 `bad_request`。
- key 必须是当前有效用户拥有的普通文件、最终路径不得是 symlink、不得授予 group/other
  权限，长度必须为 32 至 4096 bytes。

完整 wire sequence 见协议文档；SkillFS 的业务集成与密钥部署见
[`SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md`](SKILL_LEDGER_SKILLFS_INTEGRATION_zh.md)。

## 11. Health、日志和可观测性

### 11.1 Health

health 返回：

- `status`、`pid`、`uptime_seconds`、`socket`；
- `jobs` 数组；
- `queues.inflight` 和 `queues.queued`；
- 兼容字段 `prompt_scan`。

`prompt_scan` 当前是固定兼容 stub：`status=ready`、`model=native`、`loaded=true`，
其它时间和错误字段为 `null`。它不证明 daemon 托管了 scanner。

### 11.2 诊断日志

daemon 诊断日志是 JSONL，默认记录 daemon 自身和进程内依赖的 Python/Rust log records。
当前控制面必须保留以下逻辑事件：

- `daemon_started`、`daemon_stopped`、`daemon_sighup_noop`；
- `daemon_request_started`、`daemon_request_completed`。

通用 one-shot/periodic job framework 还定义 `daemon_job_started`、
`daemon_job_completed`、`daemon_job_failed` 和 `daemon_job_cancelled`；当前默认
`skill-ledger-activation` 使用自定义循环，没有承诺逐次产生这些通用 job event。Rust
迁移若统一 job lifecycle，可以补齐事件，但这属于可观测性增强，不能把新增事件计入
当前 golden 数量。事件字段、trace 和目标 Job run 模型见
[`DAEMON_JOB_CONTRACT_zh.md`](DAEMON_JOB_CONTRACT_zh.md)。

completion 记录至少包含 `request_id`、`method`、`caller`、`ok`、`exit_code`、
`error_code`、`latency_ms`、`queue_ms`、`bytes_in` 和 `bytes_out`。当前
`queue_ms` 固定为 `0`。

`AGENT_SEC_DAEMON_LOG_LEVEL` 控制 daemon 日志级别；日志文件当前滚动上限为 10 MiB、
保留 5 个备份。值 `off` 禁用 daemon JSONL；默认值为 INFO。日志写入失败不得改变业务
响应。daemon 日志路径为 `<AGENT_SEC_DATA_DIR>/daemon.jsonl`；未设置数据目录时使用共享的
system/user/per-user temporary fallback。完整数据与日志交付语义见进程与部署契约。

## 12. 配置面

| 名称 | 作用 | 当前默认 |
| --- | --- | --- |
| `AGENT_SEC_DAEMON_SOCKET` | 覆盖 socket path | XDG runtime path |
| `AGENT_SEC_DAEMON_DISABLED` | CLI 是否禁用 daemon 路径 | false；真值为 `1/true/yes/on`，忽略大小写和首尾空白 |
| `AGENT_SEC_DATA_DIR` | CLI writer、daemon query SQLite 与 daemon JSONL 的共享数据根目录 | system/user/per-user temporary fallback |
| `AGENT_SEC_DAEMON_LOG_LEVEL` | daemon 诊断日志级别；`off` 禁用 | INFO |
| `AGENT_SEC_SKILLFS_NOTIFY_AUTH_KEY_FILE` | notify 双向认证 key | 未配置，明文 notify |
| `--max-request-bytes` | 请求 frame 上限 | 4194304 |
| `--max-response-bytes` | 响应 frame 上限 | 4194304 |
| `--max-connections` | 活动连接上限 | 64 |
| `--request-read-timeout-ms` | 首帧/认证业务读取上限 | 5000 |

`--socket`、`--max-response-bytes` 和 `--request-read-timeout-ms` 当前可用但在普通 help
中隐藏。Rust binary 可以改变参数解析库，但必须保留这些调用形式和默认值。

## 13. **[TARGET V2]** Rust 责任映射

| 逻辑层 | Python V1 参考 | Rust V2 责任 |
| --- | --- | --- |
| process/bootstrap | `agent_sec_cli.daemon.server` | `apps/asc-daemon` composition root |
| request/response | `agent_sec_cli.daemon.protocol` | `asc-daemon-protocol` versioned wire contract |
| identity/authorization/query | gateway、socket ownership、query handlers | `asc-daemon-core` trusted Principal、authorization、QueryScope |
| action execution | 当前不由 daemon 执行 | `asc-action-runtime` + typed `CapabilityExecutor` |
| jobs | `daemon/jobs/` | `asc-daemon-core` JobSupervisor + domain application service |
| data | CLI/daemon 直接使用 JSONL/SQLite | Repository ports + injected persistence；CLI/TUI 不直读 SQLite |
| state migration | 当前无 system-wide migrator | `asc-state-migrator` 显式迁移 V1 per-user state |

### 13.1 **[TARGET V2]** Security action handler 边界

Rust daemon 通过默认 registry 中显式注册的 action handler 调用 asc-daemon-core action use
case，再进入 asc-action-runtime 和唯一 CapabilityExecutor。
该结构沿用 commit `ef0d75f27c389434cf6f4361f5dbcdeaff42ab72` 中
`register_prompt_scan_methods()`、`prompt_scan_handler()` 和 `asyncio.to_thread()` 已验证的
边界，但不恢复已经退役的 Python模型 preload 实现。

每个 action handler 必须完成 method-specific wire-shape 校验、daemon context 到
`ActionContext` 的映射、blocking work 隔离和 `ActionResult` projection；领域输入校验仍由
共享 core 按 action contract 执行，handler 不得复制 backend router、lifecycle、event、
redaction 或改变错误所在的 response layer。canonical method 清单和三层响应见
[`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md#daemon-security-action-handler-contract)。

当前 9 个 method 必须进入 compatibility inventory。拟新增的 8 个 canonical `action.*`
method 与显式 allowlist 目标一致，但 method 名、schema、授权、timeout 和 compatibility
version 仍须由 asc-daemon-protocol 的 Definition Review 固化；在该门禁完成前，17 只能是
候选 canonical 总数，不能当作已经冻结的 V2 接口。

`caller` 只是调用方提供的 attribution metadata，不能作为授权依据。V2 由内核 peer
UID/GID/PID、token binding 和服务端配置构造 Principal；客户端自报的 UID、role、scope
不可信。handler 使用服务端生成的 authorization 和 QueryScope。

测试必须从语言无关的 bytes、JSON、文件权限、SQLite fixture、signal 和进程副作用出发，
不得通过 mock Python 类或检查 Rust 内部类型来宣称兼容。

## 14. 验收清单

### 14.1 **[CURRENT]** V1 行为 oracle

以下 ID 全部必须有 V1 fixture；是否在 V2 原样保留由 compatibility inventory 决定。尤其
per-user path、user service 和用户级 singleton 不能直接升级为 V2 PRESERVE 要求。

| ID | 必须验证的行为 |
| --- | --- |
| DCB-001 | path 优先级、0700 runtime directory、0600 socket/lock |
| DCB-002 | 单实例、健康实例拒绝、不可达 stale socket 安全清理 |
| DCB-003 | 一连接一请求、64 连接上限和 `busy` |
| DCB-004 | 读取超时、方法超时、4 MiB request/response 上限 |
| DCB-005 | 当前 9 个 method 的名称、语义及 metadata 在新增 method 后仍不漂移 |
| DCB-006 | trace context、request ID、started/completed access log |
| DCB-007 | SIGTERM/SIGINT drain、job stop、socket/lock 清理 |
| DCB-008 | SIGHUP no-op |
| DCB-009 | Skill Ledger 启动 reconcile、500 ms debounce、按目录合并 |
| DCB-010 | worker 业务错误不重启；transport 错误最多重启重试一次 |
| DCB-011 | notify 无 key 明文兼容、有 key 时 fail-closed 且 health/query 不受影响 |
| DCB-012 | health 不初始化重型模块并保留固定 prompt compatibility stub |

### 14.2 **[TARGET V2]** Rust 迁移验收

| ID | 必须验证的行为 |
| --- | --- |
| DCB-013 | Rust daemon 对当前 Python oracle 的同一 fixture 输出规范化后等价 |
| DCB-014 | agent-sec-cli 只通过 daemon 执行；daemon 不可用时返回受控 unavailable/version error，不启动用户 daemon、不使用 PyO3 或本地业务 fallback |
| DCB-015 | 当前 9 个 method 均有 compatibility classification；八个 action method 通过 protocol Definition Review 后才进入 canonical registry |
| DCB-016 | daemon error 与失败 ActionResult 保持三层分离，并兼容历史 action response projection |
| DCB-017 | Host 第二实例被拒绝；system-scope service 和每 Node 单 DaemonSet 形态通过验收 |
| DCB-018 | 多 UID/Agent 使用同一 socket，trusted Principal 和 owner QueryScope 阻止越权查询 |
| DCB-019 | CLI/TUI 不直读 SQLite、Compiler 或 PCP；所有查询经过 daemon-core authorization |
| DCB-020 | V1 per-user state 由 state migrator 安全映射到 system-owned persistence，并支持失败恢复与回滚 |

时间、PID、UUID、latency、socket 临时路径等非确定字段在比较前允许规范化；方法名、
错误码、权限、状态转换、结果 schema、退出码和持久化副作用不得被规范化掉。

Job 相关分类和验收必须遵循 Job 契约：`DJOB-001` 至 `DJOB-004`、`DJOB-007` 至
`DJOB-012` 是 production V1 基线，`DJOB-005/006` 仅为 Python 通用框架历史表征，
`DJOB-013` 至 `DJOB-023` 是 Rust 首版目标。不能以 `DCB-007/DCB-009/DCB-010` 的摘要替代
适用的 Job subsystem fixtures。

进程、安装、systemd、singleton hardening、数据目录和日志交付还必须满足进程与部署契约的
`DPROC-001` 至 `DPROC-019`。

## 15. 当前实现证据

- 控制面：`agent-sec-cli/src/agent_sec_cli/daemon/server.py`、`runtime.py`、
  `gateway.py`、`registry.py`。
- 协议与错误：`daemon/protocol.py`、`daemon/errors.py`。
- method：`daemon/health.py`、`daemon/handlers/security_query.py`、
  `daemon/handlers/skill_ledger.py`。
- job：`daemon/jobs/`，尤其 `jobs/skill_ledger/`。
- auth：`agent_sec_cli/skill_ledger/skillfs_peer_auth.py`。
- 进程与部署：`agent-sec-cli/pyproject.toml`、`scripts/agent-sec-daemon-wrapper.sh`、
  `packaging/raw/`、`packaging/systemd/`、`Makefile` 和 `agent-sec-core.spec.in`。
- characterization：`tests/unit-test/daemon/`、`tests/e2e/daemon/`、
  `tests/integration-test/skill-ledger/test_skill_ledger_daemon_integration.py`。

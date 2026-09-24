# AgentSec daemon 进程与部署契约

| 属性 | 值 |
| --- | --- |
| 状态 | V1 Python 交付基线、兼容语料及仓库内 V2 部署目标 |
| 实现核对日期 | 2026-09-04 |
| 当前行为基线 | fe58ed4b23b8；与 main 中已有 systemd/RPM 行为交叉核对 |
| 适用实现 | V1 Python daemon oracle；V2 Rust asc-daemon；安装器、migrator 和进程管理器 |

## 1. 文档地位与范围

本文冻结 V1 agent-sec-daemon 的进程入口、安装布局、systemd user service、启动/停止、
重启、runtime/data path 和诊断日志事实，并定义这些事实如何进入 V2 compatibility 和迁移
验收。V2 产品形态以仓库内
[`AGENT_SEC_RUST_MIGRATION_zh.md`](AGENT_SEC_RUST_MIGRATION_zh.md#1-文档状态与仓库内权威关系)
为准：

- one daemon per host；
- Linux system-scope systemd；
- Kubernetes 每 Node 一个 DaemonSet；
- system-owned runtime/state；
- Rust agent-sec-cli 仅作为 daemon client；
- 不保留 Python CLI、PyO3 local fallback 或 per-user daemon。

socket 生命周期见
[DAEMON_CURRENT_BEHAVIOR_zh.md](DAEMON_CURRENT_BEHAVIOR_zh.md)，wire protocol 见
[DAEMON_PROTOCOL_V1_zh.md](DAEMON_PROTOCOL_V1_zh.md)，后台任务见
[DAEMON_JOB_CONTRACT_zh.md](DAEMON_JOB_CONTRACT_zh.md)。

标签含义：

- **[CURRENT]**：V1 当前事实；
- **[PRESERVE V1]**：supported V1 接口在兼容期保持的语义；
- **[TARGET V2]**：与仓库内迁移总计划一致的 V2 目标；
- **[SUPERSEDED]**：已被当前仓库 V2 架构取代的旧目标。

未标记行为只属于 **[CURRENT]**，不能自动升级为 V2 要求。外部兼容可以由 Rust binary、
命令 alias、protocol adapter 或 state migrator 提供，不要求保留 Python runtime。

deploy/sidecar/healthcheck.py 不在 main 的受支持交付基线中，不得作为 CURRENT/PRESERVE V1
证据。sidecar、本地 probe 或实验 chart 不能反向定义 readiness。

## 2. **[CURRENT]** V1 进程入口与信号

受支持 V1 安装当前提供 agent-sec-daemon：

- agent-sec-daemon serve 启动前台 daemon；
- 无子命令当前等价于 serve；
- --help 成功并输出 usage/help；
- daemon 不 fork、double-fork、写 pidfile 或自行转入后台；
- SIGTERM、SIGINT 进入 drain/cleanup；
- SIGHUP 记录 no-op；
- 启动配置、runtime path、lock、Job start 或 bind 失败非零退出；
- SIGKILL 和不可恢复 crash 由进程管理器与下次启动恢复；
- Python traceback 文本不是稳定接口，稳定面是 exit status、结构化日志和 health RPC。

当前 wheel 使用 Python console script；RPM/raw wrapper 启动私有 Python runtime。这些是
V1 packaging 事实，不是 V2 实现约束。

agent-sec-daemon 命令名、serve、参数、默认值和退出语义必须进入 compatibility inventory。
如果继续标记为 supported，V2 应由 Rust asc-daemon binary、Rust 命令 alias 或明确版本化
入口承接；不能静默删除，也不要求保留 Python wrapper。

## 3. **[CURRENT]** V1 per-user 部署

### 3.1 安装产物

V1 RPM 当前提供：

- /usr/bin/agent-sec-daemon，mode 0755；
- /usr/lib/systemd/user/agent-sec-core.service，mode 0644；
- ExecStart 指向 agent-sec-daemon serve。

raw package 保存可重定位 wrapper 和带 bindir/datadir 占位符的 user unit template；
source/venv 使用 console-script symlink。安装器渲染后不得残留占位符。

### 3.2 systemd user unit

V1 agent-sec-core.service 当前是 per-user service：

| 项目 | 当前值/语义 |
| --- | --- |
| service type | Type=simple；被 exec 的 daemon 是主进程 |
| runtime env | XDG_RUNTIME_DIR=/run/user/%U |
| runtime directory | RuntimeDirectory=agent-sec-core、mode 0700 |
| restart | Restart=on-failure、RestartSec=2 |
| crash-loop gate | 300 秒内最多 5 次启动失败 |
| install target | default.target |
| privilege | 不要求 root，不允许 privilege uplift |

当前 hardening 包括 NoNewPrivileges、PrivateTmp、ProtectSystem、受控 ReadWritePaths、
kernel/control-group protection、RestrictSUIDSGID 和 LockPersonality。V2 应保留等价或更强
hardening；需要放宽时提供 syscall/filesystem 证据、最小例外和测试。

上述 user unit、XDG path 和用户级 singleton 只属于 V1。V2 交付物不得继续安装 user-scope
unit，也不得让 agent-sec-cli 自动创建用户 daemon。

### 3.3 active 不等于 ready

V1 Type=simple 的 active 不证明 socket 已 bind、Job 已启动或 daemon.health 可返回。此
可观察区别必须保留到 V2 readiness 设计：

- process active 与 application READY 分开；
- 将来交付 readiness 时，必须通过受支持 health RPC/probe 验证；本阶段不交付该接口；
- prompt compatibility stub 不代表 capability readiness；
- 单个 Job error 不自动等于顶层 daemon 不可用；
- 引入 sd_notify、socket activation 或 container probe 时必须冻结 timeout、failure 和
  restart 语义。

## 4. **[TARGET V2]** system-level 部署

### 4.1 Host

- 安装 system-scope systemd unit，不安装或启用 user-scope unit；
- 默认一个 Host 一个 asc-daemon；第二实例必须因 Host 级 singleton 失败；
- 当前 Linux unit 使用 root:root，不创建专用账户；保留现有 capability 和沙箱限制，
  root 身份不等于开放全部特权；
- systemd 负责 start/stop/restart、资源限制、目录准备和故障拉起；
- daemon 不自行 daemonize，不在启动路径隐式执行不可逆 migration；
- runtime 目录由 systemd RuntimeDirectory 创建；state/log 路径按各自存储契约管理；
- 两个不同 UID/Agent 通过同一 system socket 访问并保持 owner-scope 隔离。

### 4.2 Kubernetes

- 默认 DaemonSet，每个目标 Node 恰有一个 Ready 实例；
- Helm/manifest 明确 service account、security context、volume、resource 和 probe；
- init job 或安装流程显式调用 asc-state-migrator；daemon 启动不偷偷升级状态；
- rollout、rollback、drain 和 Node replacement 分别留存证据。

### 4.3 CLI 与进程所有权

agent-sec-cli 是 Rust daemon client：

- 不执行 systemctl start；
- 不创建用户 socket、lock 或 daemon；
- daemon unavailable 时返回稳定错误；
- 不使用 PyO3、Python backend 或通用 local fallback；
- 少数纯函数的 Rust local mode 必须有独立批准合同，不能继承旧 fallback 语义。

## 5. Runtime、singleton 和 lock

### 5.1 **[CURRENT]** V1 lock

V1 daemon.lock 正常 stop 后保留。下次启动当前会：

1. read/write 打开或创建；
2. 尝试 non-blocking exclusive flock；
3. 持锁时报告 already running；
4. 无持锁者时复用 inode、truncate、写 PID；
5. 继续 stale-socket probe。

V1 未对已有 lock path 完整执行 no-follow、regular-file、owner 和 mode 验证。这是安全缺口，
不是 PRESERVE V1 行为。

### 5.2 **[TARGET V2]** Host 级 singleton hardening

- lock、socket、runtime 和 state 是 Host 级 system-owned 资源；
- 最终 path component 不跟随 symlink；
- 在同一已打开 fd 上验证 regular file、owner、mode 并获取 flock，避免 reopen
  TOCTOU；V2 不要求锁文件保存 PID；
- 无持锁者时可复用安全遗留 lock；持锁实例阻止第二实例；
- cleanup 只删除本实例绑定的同一 socket inode；
- held lock、unsafe path、permission 和普通 I/O failure 使用稳定分类；
- 多 UID client 不能通过替换 runtime path 或客户端自报身份影响 singleton。

## 6. 数据目录与状态迁移

### 6.1 **[CURRENT]** AGENT_SEC_DATA_DIR

V1 AGENT_SEC_DATA_DIR 是 Python CLI writer 与 daemon query/log 共用的数据根，不只是日志
目录。设置后承载：

- security-events.db/jsonl；
- observability.db/jsonl；
- daemon.jsonl；
- 其它复用 security-event path resolver 的本地流。

未设置时，V1 resolver 依次尝试 /var/log/agent-sec、~/.agent-sec-core 和 per-user 临时目录，
并创建 mode 0700 目录。这些 fallback 是 V1 discovery 输入，不是 V2 默认布局。

### 6.2 **[TARGET V2]** system-owned persistence

- asc-daemon composition root 装配 persistence adapter；
- CLI/TUI 不直读 SQLite，所有查询经过 daemon-core authorization 和 server QueryScope；
- state 以 owner principal 隔离，不以客户端传入 UID/role/scope 决定访问；
- AGENT_SEC_DATA_DIR 是否继续作为 operator override 必须在 config contract 中版本化定义；
- V2 不让每个用户和 daemon 各自推导不同数据库路径。

asc-state-migrator 必须验证 V1 path discovery、显式 source、owner mapping、schema migration、
重复运行、事务、失败恢复、回滚、mixed-read、权限、symlink/hardlink 和多用户数据冲突。
Credential、token、passphrase 和 key material 不进入通用 persistence。

## 7. 诊断日志

V1 当前 daemon 日志语义进入兼容语料：

- 主流为 data-dir/daemon.jsonl；
- 默认 INFO，AGENT_SEC_DAEMON_LOG_LEVEL=off 禁用；
- debug/info/warning/error/critical 大小写和空白不敏感；
- 单文件 10 MiB，保留 5 个备份；
- 写入失败 best-effort，不改变业务 response。

V2 logging/OTel 可以更换 sink 和 delivery，但结构化字段、脱敏、failure isolation 和
operator-visible semantics 必须进入 compatibility/change record。journald 文本不能替代稳定
机器可读日志或 SecurityEvent。

## 8. **[TARGET V2]** Rust 交付要求

1. asc-daemon、agent-sec-cli 和 asc-state-migrator 都是 Rust binary；
2. V2 runtime 不依赖 Python interpreter、site-packages、PyO3 extension 或 wheel；
3. raw/RPM/container/systemd/Helm 安装相互一致；
4. Linux 只交付 system-scope unit；Kubernetes 交付每 Node 一个 DaemonSet；
5. supported V1 命令/RPC/config/state 由兼容 adapter 或版本化迁移承接；
6. restart、signal、readiness、runtime/state path、权限、日志和 exit semantics 使用黑盒
   fixture；
7. 安装、升级和不可逆 migration 由 packaging/deploy/state-migrator 所有；
8. 不把未进入 main 的 helper、probe 或本地 chart 当作 V1 事实。

### 8.1 **[TARGET V2][PARTIAL]** 当前 Rust transport bring-up

`v2/apps/asc-daemon` 当前提供对外名为 `agent-sec-daemon` 的前台 Rust binary 和
composition bootstrap。它接受无子命令或显式 `serve` 两种形式；`--socket` 可提供显式绝对
路径，省略时读取非空 `AGENT_SEC_DAEMON_SOCKET`，再回退到 `/run/agent-sec-core/daemon.sock`；不读取 HOME 或 XDG_RUNTIME_DIR。进程安装 SIGTERM/SIGINT cooperative shutdown，并消费 SIGHUP 而不 reload。
bootstrap 使用 `asc-daemon-service` 完成真实 UDS bind、bounded admission、单请求 frame
读取、drain 和同 inode socket cleanup。

transport 对 frame read、application dispatch、transport rejection encode、response
write 和 drain 分别设置显式 deadline。dispatch deadline 到期会释放 connection admission
并向 handler 发出 cooperative cancellation，但 Rust 不能强制终止已经运行且忽略取消信号的
blocking call。`asc-daemon` 因此显式拥有 Tokio runtime，并在 service drain 后使用额外的
runtime shutdown timeout，避免残留 `spawn_blocking` 让前台进程永久不能退出。该 bounded drain
保持 V1 语义：deadline 后仍未完成的 admitted task 可以被 abort，终态 audit 在该进程退出边界
是 best-effort，不构成持久化交付保证；这不改变 daemon 正常运行时 caller timeout 不使 work
无主的规则。

security-event 存储使用 system-owned 目录：daemon 只接受 systemd/DaemonSet 显式设置的
`AGENT_SEC_DATA_DIR`，未设置时固定为 `/var/log/agent-sec`，不回退到 `HOME` 或 `/tmp`。
目录必须由 daemon 有效用户拥有且为 `0700`，主 JSONL/SQLite 文件为 `0600`；SQLite
WAL/SHM sidecar 受私有目录保护。绑定 UDS 前必须实际打开 JSONL、打开并初始化 SQLite：SQLite
失败即非零退出；JSONL 失败只输出告警，daemon 仍启动并对该副本保持 best-effort 写入。成功
启动后单侧瞬时写失败仍保持独立 fail-open，不改变 capability 的业务结果。

scan composition root 还注入共享 telemetry sink。`AGENT_SEC_TELEMETRY_LOG_PATH` 沿用 V1，
默认 uploader-owned JSONL。路径中的 `~` 优先使用 `HOME`，未设置时查询当前 UID 的系统
用户数据库；`~user` 查询指定用户的 home，保持 V1 `Path.expanduser()` 对已有用户的展开行为。
缺失/禁用 telemetry 不影响 daemon 启动或 audit。telemetry writer
不创建目标/目录，不改变现有 system-owned audit 路径。handler 接收 Action application，
不持有 Finalizer；生产 startup 显式注入 outputs。DPROC-SCAN-001–004 的真实进程、restart、
独立 sink fault 和 UDS lifetime fixtures 见 [共享 scan lifecycle](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md#54-已实现的共享生命周期)。
这些 fixtures 不替代 RPM/systemd 或完整 DPROC 验收。

Telemetry 目标必须是已有 regular file；V2 的版本化加固例外是拒绝最终路径分量为
symlink，替代 V1 跟随目标 symlink 的行为。预检查使用 `symlink_metadata`，实际 open
使用 `O_NOFOLLOW` 防止检查后被替换为 symlink；不新增祖先目录校验或文件管理职责。
`asc-event-sink/tests/telemetry.rs` 覆盖目标 symlink、dangling symlink、检查后替换及恢复写入。

OTel 初始化前的进程 panic hook 向 stderr/journald 输出 `agent-sec-daemon: internal panic`，并在 location
可用时追加 ` at <file>:<line>:<column>`，通过临时有界 writer 最多等待 50 ms，
不输出 panic payload。源码位置用于诊断，不进入
RPC error、SecurityEvent 或 telemetry。`agent-sec-daemon` binary 单元测试
`panic_hook_reports_location_without_payload` 通过独立子进程验证位置输出与 payload 脱敏。
OTel 初始化后由 §11 的有界 hook 接管，仅输出固定诊断且不等待 stderr。

该 slice 已由唯一的 concrete `DaemonDispatcher` 注册 first-version PAP daemon protocol，
但尚未注册 `daemon.health`。dispatcher 完成 envelope decode、request ID、kernel peer
credentials 到 trusted Principal 的绑定、method allowlist、authorization 和 response
encode；PAP 是其中一组显式注册的方法，不增加第二个 service dispatch 层。当前 composition
root 使用 `RootManagedPrincipalPolicy`：UID 0 始终具有 PAP 管理权限。部署者可用
可重复的 `--policy-admin-uid <UID>` 在启动时配置额外管理员；省略时其它 UID 调用 PAP 方法返回
`permission_denied`。值为十进制 u32，非法值启动失败；重复 UID 去重。启动配置由服务端
部署者控制，匹配的是内核 peer UID，caller-supplied identity 不能覆盖该判断。名单每次
启动重新构造，不带参数重启恢复 root-only。被配置的管理员没有继续委派权限；运行中的
`allow_uid` API 仍要求 root。该选项不改变 OS 权限、socket mode 或 system-level 部署形态。

**[TARGET V2][IMPLEMENTED] DPROC-UDS-001**：复用已合入的 system-service 接入权限：
CLI 装配选择 `0666`，可复用 bootstrap 保持私有 `0600` 默认值。任何能连接 UDS 的调用方
均可调用 code scan；其 `LocalUser` policy 不增加管理员或 UID allowlist 检查，peer credentials
用于生成审计归属。PAP 仍由内核 peer credentials 和管理员名单授权。
此 lifecycle 工作包不再修改 socket mode。
`asc-daemon/tests/bootstrap.rs` 验证真实进程 scan 成功及 PAP 授权；系统运行目录、默认
socket 权限和跨 UID 接入的部署测试沿用 `tests/v2/e2e/test_daemon_process_e2e.py`。

当前 PAP 由 `PolicyTemplateCompiler` 和过渡性的 process-local Repository 组成。Policy、Scope
和 Binding CRUD 可在同一 daemon 生命周期内经真实 UDS 执行，但所有状态在进程重启后丢失，
进程启动时会 best-effort 输出该限制（诊断背压规则见 §11）。这些结果只证明 protocol、identity、authorization 和应用装配的
integration slice，不表示 durable persistence、target enforcement 或 application READY。
Busy、timeout、shutdown 等 transport failure 由独立且有短 deadline 的
`RejectionEncoder` 投影，正常依赖图不包含 PAP、Repository 或 Compiler。
framework 不能证明具体 PAP/Repository 内部没有全局 mutex、长 transaction 或其它共享阻塞
点；该项必须由 PAP direct-consumer concurrency fixture 在集成时验收。

本阶段交付 V2 RPM 的 system-scope unit（`packaging/systemd/agent-sec-core-v2.service.in`）。
它以 `root:root` 运行，不创建专用 UID，systemd 创建 `/run/agent-sec-core`（0755），socket
为 0666。普通用户无需加入服务组即可连接；PAP 方法授权仍检查内核 peer UID，
连接权限不授予 PAP 管理权限。
**TODO（独立入口流量控制任务）**：当前全局 64 个连接名额可被单个普通 UID 占满，
管理员请求也会被拒绝；方法授权不能解决该可用性问题。后续实现按身份隔离/管理员
保留容量，并用跨 UID 饱和测试验收。本次仅记录缺口，不宣称已实现隔离。
`BootstrapConfig::new()` 保持私有 socket 默认值 0600，daemon CLI 装配显式改为 0666；
使用固定授权策略的库测试将运行目录限制为 0700。
目录不可由 group/other 写入，锁保持 0600。
`BoundUnixSocket` 允许 other 读写位，继续拒绝执行位、特殊位和缺少 owner 读写的 mode。
跨 UID fixture 检查普通 UID 可连接且 PAP 管理仍被拒绝；该 fixture 需要 root，
缺少权限时跳过，不计为验收通过。SkillSec 扫描与业务接口见第 11 节。
进程设置 umask 0077，最终 socket mode 显式设置为 0666。unit 启用 NoNewPrivileges，
stdout/stderr 进入 journal；SkillSec 目录访问与最小 capabilities 见第 11 节。
**DPROC-V2-HARDENING-1（2026-09-10，获准实施，跟踪 issue #2861）**：V2 默认启用
`SystemCallFilter=@system-service`、`SystemCallArchitectures=native` 和
`MemoryDenyWriteExecute=true`；SkillSec 将 CapabilityBoundingSet 限定为
`CAP_DAC_OVERRIDE CAP_CHOWN CAP_FOWNER`，AmbientCapabilities 仍为空。
打包 fixture 锁定这些生效指令；实际 syscall/W^X 兼容性需在目标发行版的 systemd
服务下验收当前业务路径，不能用 unit 语法检查或普通进程测试替代。
SkillSec 核心 CLI 子包不再依赖 Python、GPG/PGPy 或 loongshield；未迁移 Hook
子包仍保留自身依赖。自动依赖排除设置保持不变。
不再打包 sysusers 文件或执行账户创建脚本。V1/V2 源包分别收录各自的 unit
模板；构建和安装继续复制共享的 `.anolisa/component.toml`。该 manifest 仍声明
V1 user scope，尚未适配 V2 的 system-service 编排，不能作为 V2 服务管理的验收证据。
V2 安装态 CI 检查 system unit 的 root 身份配置，并拒绝遗留 user unit。

启动先逐级以 nofollow 打开并验证 runtime 目录：祖先属于 root 或服务 UID，不允许
非 sticky 的 group/world 可写祖先；最终目录必须服务 UID 所有，普通权限位
（`mode & 0777`）为 0700/0750/0755。目录特殊位不单独构成拒绝条件；锁文件和
socket 的完整模式检查不变。CI 私有实例在 `/tmp` 下创建独立 0700 目录，避免依赖
runner 工作目录的 owner、权限和 umask。
同一目录的 `daemon.lock` 以 nofollow 打开，在同一 fd 上验证 regular/owner/0600/nlink=1、
获取非阻塞 flock；V2 不读写 PID 文件内容，持锁状态是单实例判定依据。
锁贯穿 Tokio shutdown，不 unlink；
`RuntimeDirectoryPreserve=yes` 保留同一锁 inode，/run 在主机重启后仍为临时存储。
已有 socket 只有 owner/mode/link 合法且 connect 明确返回 ECONNREFUSED 时才按 inode
复核并删除；live listener、符号链接、普通文件和模糊错误均拒绝启动。
服务 UID 与 root 是可信边界；客户端不得拥有目录写权限。显式 `--socket` 允许在另一
安全目录建立隔离开发实例，不代表防止特权操作者故意创建第二个 namespace。

当前正常退出包含 UDS drain 2s、SkillFS worker join 30s、reconciliation join 30s
和 Tokio shutdown 1s 的上限；unit 另设 `TimeoutStopSec=75`，超时由 systemd 对 control group 发 SIGKILL。
SIGTERM/SIGINT 正常退出为 0，启动运行错误为 1，参数错误为 2；SIGHUP 消费但不 reload。
`Restart=on-failure`、`RestartSec=2`、300s 内最多 5 次启动限制异常退出重启循环。
Type=simple 不要求 READY 通知或 watchdog；systemd active 不作为应用 readiness 证据。

版本化变更记录 **DPROC-V2-SYSTEM-1（2026-09-10）**：本次搭建独立 V2 system-service
基础，不执行 V1 到 V2 的迁移。V1 raw/RPM 保留 user unit 和原有测试入口；V2 RPM
使用 root 身份的 system unit 和独立安装检查。V2 不再沿用此前的 XDG socket 默认值。服务环境由 systemd 配置，终端变量不会自动
传给服务；自定义路径时，部署者须同时配置服务和客户端。
回退本次变更时停止测试用 V2 服务并恢复此前 V2 构建，不操作 V1 服务或迁移数据。
PAP 持久化、readiness/持续健康检查和 OTel 不在本次交付范围；
SkillSec 持久化与激活恢复见第 11 节。

RuntimeLease 是 binary 私有实现；目录 fd 仅用于验证/openat，锁 fd 保留至进程退出。
V2 不继承 V1 的 PID 文件内容契约，单实例判断依赖非阻塞 flock。

本阶段 fixture 和执行命令见 [V2 systemd 验收说明](../../tests/v2/systemd/README.md)。
V2 测试统一由 pytest 收集；system manager 环境不足时明确 skip，
专用主机通过 `--require-systemd` 禁止跳过。收集成功或 skip 不计为 DPROC-013 验收通过。
DPROC-012/014 由进程与 staging fixture 覆盖当前 namespace 边界；DPROC-013 的
system-manager 生命周期 pytest 必须在具有 root 权限的 systemd 主机上运行。
只有完成真实 system-manager gate 才能验收该部署生命周期；普通进程测试与 unit
语法验证不能替代它。完整 DPROC-013（含 readiness）和整个 production gate 仍为 PARTIAL。

OTel 初始化、诊断与关闭已接入，见 §11。

### 8.2 **[TARGET V2][PARTIAL]** Rust Policy CLI

`v2/apps/asc-cli` 构建产物为 `agent-sec-cli`（crate 名仍为 `asc-cli`），提供 `policy`、`scope`、`binding` 三组各五条 CRUD 命令，通过
`asc-daemon-client` 调用现有 15 个 PAP method。**DPROC-V2-CLIENT-ENDPOINT-1（2026-09-10）**：CLI 省略 `--socket` 时读取非空 `AGENT_SEC_DAEMON_SOCKET`，再回退到
`/run/agent-sec-core/daemon.sock`；两端均要求解析后的路径为绝对路径。
显式参数优先于环境变量，空环境变量忽略，与 V1 的覆盖顺序一致；不再将缺少该参数视为用法错误。
`asc-daemon-client` 库继续接收调用方传入的路径，不自行解析部署环境。CLI 不启动或
重启 daemon，不解析 HOME socket，不读取 Repository，也不执行本地业务 fallback。
`--help` 和 `--version` 不连接 daemon。

`--timeout-ms` 为正 u32，默认 5000；一次客户端 deadline 覆盖 connect/write/read，
不向现有 wire envelope 添加 timeout 字段。请求业务预算为 4,194,304 字节，传播成员
另有 32,768 字节，总上限 4,227,072 字节；响应仍为 4,194,304 字节，均包含 LF。
分项超限为 `invalid_request`，transport 总上限超限为 `resource_exhausted`；详见协议 §13。
完整 LF response 立即完成读取，也接受非空 EOF frame。客户端保留完整
`DaemonResponse`；Policy 输出层将 success 的领域 result 输出到 stdout、退出 0，
daemon error 的 `{requestId,error}` 输出到 stderr、退出 1。本地文件、transport、
response 和 output failure 退出 1，参数用法错误退出 2。OTel subscriber 初始化冲突或
无效 SDK 身份同样在发送请求前退出 1，stderr 的 `otel: subscriber_conflict` 或
`otel: invalid_sdk_identity` 区分启动观测失败；daemon 也在接受请求前按此规则失败。

请求发送后的超时或协议失败不证明业务未执行；CLI 不自动重试，也不把 Binding
`PENDING_APPLY`/`PENDING_DELETE` 表述为目标生效或删除完成。CREATE identity、current
revision、授权和领域语义继续由 daemon/PAP 所有。该 Rust binary 与 V1 Python CLI 同名；
本节仅冻结 PAP 子集。现有 Code Scan 和新增 SkillSec 命令分别依照各自合同，不表示
Rust binary 已替代 V1 全量能力或提供通用 V1 wire adapter。

DPROC-011 和 DPROC-018 的 focused evidence 为 `asc-cli/tests/commands.rs` 的 binary
失败测试、`asc-cli/tests/pap_process.rs` 的真实 CLI 进程和 UDS 授权测试，以及客户端
依赖图。CLI 进程测试使用测试进程内的 daemon service；真实 CLI 与 daemon binary
共同运行的双进程 E2E 位于 `tests/v2/e2e/`。
`asc-daemon/tests/bootstrap.rs::dproc_configured_administrator_can_query_without_root`
在 root 环境验证真实 daemon binary 的管理员配置与信号退出，在非 root 环境验证系统 daemon
明确拒绝启动。普通用户 CLI 调用 root daemon 的证据见 DPROC-SG-001。
测试注入独立配置、状态及审计目录，不创建或覆盖宿主凭据，不向宿主 AgentSight 下发策略。
完整 PAP CRUD 保留在 `asc-daemon/tests/pap_protocol.rs` 的进程内 UDS fixture 中；后台下发
装配由 DPROC-021 验证，CLI/daemon 进程链路由上述 pytest E2E 验证。完整范围与命令见
[`POLICY_CLI_ACCEPTANCE_zh.md`](POLICY_CLI_ACCEPTANCE_zh.md)，不扩大其它 DPROC gate。

## 9. 验收矩阵

### 9.1 **[CURRENT]** V1 oracle

| ID | 必须固定的 V1 事实 |
| --- | --- |
| DPROC-001 | wheel/source、RPM、raw 当前命令和 --help 行为 |
| DPROC-002 | 无子命令/serve、前台主进程和不 daemonize |
| DPROC-003 | SIGTERM/SIGINT、启动失败、SIGKILL 与 cleanup |
| DPROC-004 | user unit、RuntimeDirectory 0700、restart 和 crash-loop 当前值 |
| DPROC-005 | 当前 hardening 与 privilege 行为 |
| DPROC-006 | systemd active 与 UDS health 分离 |
| DPROC-007 | AGENT_SEC_DATA_DIR 的 V1 CLI/daemon path 解析 |
| DPROC-008 | log level、rotation 和 best-effort failure |
| DPROC-009 | V1 lock reuse、held lock 和 stale socket |

以上 ID 都必须有 fixture，但 DPROC-004、DPROC-007 的 per-user 形态不自动成为 V2 PRESERVE。

### 9.2 **[TARGET V2]**

| ID | 必须验证的 V2 行为 |
| --- | --- |
| DPROC-010 | Rust binaries 不装载 Python/PyO3，supported V1 命令具有兼容或版本化路径 |
| DPROC-011 | daemon unavailable 时 agent-sec-cli 返回稳定错误，不启动 user daemon、不 local fallback |
| DPROC-012 | Host lock/socket 拒绝 symlink、非 regular、错误 owner/mode 和 reopen TOCTOU |
| DPROC-013 | system-scope restart、signal、readiness、permission 和 log 黑盒测试通过 |
| DPROC-014 | 不安装 user unit；Host 第二实例被拒绝 |
| DPROC-015 | 两个 UID/Agent 经同一 socket 访问，owner scope 隔离且自报身份不能越权 |
| DPROC-016 | 每个目标 Kubernetes Node 恰有一个 Ready DaemonSet 实例 |
| DPROC-017 | state migrator 完成 V1 per-user 到 system-owned state 的 owner-safe 迁移和回滚 |
| DPROC-018 | CLI/TUI 不直读 SQLite；query 必须经过 daemon authorization |
| DPROC-019 | raw/RPM/container/systemd/Helm 生成 checksum、SBOM 和 build metadata |

每个 DPROC ID 必须映射到机器可执行 fixture 或真实部署证据。Rust unit test 不能代替安装后
service/package、server-side admission 或真实 Kubernetes rollout 验证。

### 9.3 **[TARGET V2]** Policy 下发配置与生命周期

daemon 的 `main.rs` 调用策略下发服务初始化入口；`reconciliation.rs` 内部通过
`AgentSightClientFactory::default()` 注册首版 PEP，并启动 Binding 后台下发；注册没有凭据或网络 I/O。
具体 PEP 的选择和装配由该初始化模块所有；未来的环境变量选择尚未实现。
目标地址、默认 token 文件路径及凭据读取由 Client 封装，daemon/CLI 不暴露对应参数，
CRUD request 不传递目标凭据。每次 reconcile 尝试创建 Client 并读取最新凭据；
缺失或无效凭据进入该 Binding 的有界重试，不阻止 UDS 启动，错误不回显文件内容。Client 对非 literal loopback 的 HTTP 拒绝凭据传输，HTTPS 保留证书验证。
授权仍来自 UDS peer credentials 与既有管理员配置。

启动顺序是构造 Repository、Client factory/核心并尝试启动 Runtime，再开放 UDS 请求。
Runtime 初始化失败时记录安全错误并注入不可用通知入口，Binding mutation 返回既有准入错误；
Policy/Scope CRUD、读查询及其它 daemon 服务继续工作。不能以不注入通知入口的方式静默接受 Binding 写请求。目标尚未
READY 或暂时不可连接也不阻止 daemon 启动。shutdown 先停止 UDS 新准入并 drain 已准入请求，再停止 Runtime
领取和扫描，最多等待 30s join 活跃调用；随后沿用进程外层 1s Tokio shutdown 上限。超时
不会伪装成同步调用已取消或清理成功。单次 reconcile panic 在 worker 调用边界隔离：
核心收尾后保留已提交状态，未确认结果停止该 ID 自动执行，worker 继续处理其它 Binding，
不关闭写准入。timer/scanner 或 worker 调度代码自身异常才使 reconciliation 服务失败并停止领取，关闭 Binding mutation 准入，
但不会主动关闭 daemon。首版不自动重建失败 Runtime，需要进程重启；普通 Binding
重试或终态失败不影响服务健康。单 Binding 存储/数据错误及 CAS 竞争耗尽只安排该 ID 重试；
存储/数据错误输出安全诊断，不能伪造已落库的失败状态。补扫失败影响 health，但不关闭写准入。
实际 Repository 错误由每次 CRUD 操作返回。Memory Repository 无跨重启恢复保证。

| ID | 必须验证 | 可执行 fixture |
|---|---|---|
| DPROC-020 | 默认凭据不参与 daemon 启动；PAP 读查询和信号退出可用；reconciliation 不可用时仅拒绝 Binding 写入，Policy/Scope CRUD 仍可完成 | `v2/apps/asc-daemon/tests/bootstrap.rs`；`tests/reconciliation.rs::unavailable_reconciliation_only_rejects_binding_writes` |
| DPROC-021 | daemon 注入真实 Adapter/核心/Runtime，PAP 接受后下发，Delete 清理及 owned shutdown | `v2/apps/asc-daemon/tests/reconciliation.rs::configured_composition_delivers_pap_intent_and_joins_its_workers` |

DPROC-021 是进程内装配验收，Client 使用 scripted port；完整 CLI→daemon 进程 E2E 是单独 PR，
不能由此宣称真实 AgentSight/kernel 生效或持久化恢复通过。

## 10. 当前实现证据

- daemon entry/process/signal：agent-sec-cli/src/agent_sec_cli/daemon/server.py；
- wheel console script：agent-sec-cli/pyproject.toml；
- RPM wrapper：scripts/agent-sec-daemon-wrapper.sh；
- raw wrapper：packaging/raw/assets/bin/agent-sec-daemon；
- V1 systemd template：packaging/systemd/agent-sec-core.service.in；
- install layout：Makefile、agent-sec-core.spec.in、packaging/raw/package.sh；
- data/log path：security_events/config.py、daemon/logging.py；
- service tests：tests/e2e/daemon/test_daemon_systemd_e2e.py；
- process/signal tests：tests/e2e/daemon/test_daemon_e2e.py；
- package layout tests：tests/packaging/test-package-raw.sh；
- Rust DPROC-002/DPROC-003 与部分 DPROC-013 process fixture：
  v2/apps/asc-daemon/tests/bootstrap.rs；
- Rust PAP 完整 serialized UDS scenario：
  v2/crates/asc-daemon-protocol/tests/fixtures/pap-crud-e2e.json。

## 11. **[TARGET V2]** Tracing 生命周期补充（OTEL-CR-004）

两个 Rust 产品 main 在 help/usage 处理后、业务启动前初始化真实 OTel SDK，固定
AlwaysOff 但仍提供有效 TraceId/SpanId 和 Context/Baggage。main 调用一次 `init_runtime`；
启用 runtime feature 本身不会初始化全局状态。本期没有公开 exporter 或 OTLP 配置，
OTEL export/sampler/batch 环境设置不能开启导出或改变固定采样策略。
初始化冲突在接受请求前退出 1；`otel: <reason>` 通过临时有界 worker best-effort 输出，
最多等 50 ms；stderr 堵塞或 worker 创建失败不能阻止退出，也不保证诊断一定到达。

停止顺序：service drain → reconciliation join（最多 30 s）→ 应用 runtime 1 s shutdown
→ event sinks close → provider/诊断排空额外最多 2 s。singleton lease 保留至关闭流程结束。
CLI 业务 span 结束后最多等待 50 ms；失败不改变业务 exit code、不重试业务请求。
仍运行的 blocking work 不能被 tracing 强停。单请求 scope 覆盖解码后授权/PAP/响应编码，
不声称覆盖 socket 读写。

每个正常 runtime 启动一个独立诊断线程写 stderr；JSON 关联诊断、daemon PAP 启动警告、
signal/runtime/bind/serve 错误及异常链共用此 writer。reconciliation、JSONL、SQLite schema/
corruption/drop/read 的库诊断以 `tracing` target `asc_process_diagnostic` 接入同一 writer；
它们保留原消息且不受 RUST_LOG 关联日志过滤影响，无同步 stderr fallback。库自身不启动
线程、不初始化 SDK；其它库宿主须安装 subscriber，未安装时诊断不输出。队列最多 64 条，每条最多 32 KiB，
排队 payload 最多 2 MiB。producer 不等待 sink I/O；满队列、超长、写失败或关闭预算
用尽允许丢诊断，创建 worker 失败直接禁用诊断。RUST_LOG 默认 warn，仅过滤 JSON 关联
记录；不抑制进程警告/错误。无按秒限速，接收方管理持续存储和 rotation。
CLI help/usage/业务结果及错误、daemon 参数错误/help 仍同步输出，可能等待消费者；
这些输出不能以丢弃诊断队列替代。SecurityEvent 持久化也不使用此队列。

生产初始化还将 Rust 默认的同步 panic hook 替换为同一有界 writer，仅输出固定
`runtime: panic`，不记录 panic payload，也不改变 unwind/abort 或业务错误映射。
内部 runtime 子进程测试验证 caught panic 的固定诊断及 payload 隔离。

DPROC tracing 扩展以 `v2/apps/asc-daemon/tests/tracing.rs` 和 `tests/v2/e2e/test_otel_e2e.py`
作证据：真实 UDS timeout 后 span 不提前关闭；启动前填满 stderr 后仍能启动、响应及退出，
重复 daemon 启动失败也能退出。新增 storage fault 用例覆盖 stderr 已满时的 JSONL 写失败、
SQLite 插入失败、高版本 schema 警告和独立 audit sink 仍写入；
`v2/apps/asc-daemon/tests/process_diagnostics.rs` 覆盖后台 worker 的 repository 错误、
未确认 terminalization 诊断和 join，RUST_LOG info/off 均执行。原有 DPROC 条款仍由各自 fixtures 验收。
OTel E2E 从 PATH 解析产品 binary，与现有 `make test-e2e-rpm-v2` 的收集和安装态执行方式一致；
本机源码进程测试不替代完整 systemd/RPM 验收。

## 12. [TARGET V2] SkillSec 系统身份与恢复

SkillSec 第一阶段使用 root 系统 daemon。`/run/agent-sec-core/daemon.sock` 为默认公共端点，
CLI/进程均支持显式 --socket 和非空 AGENT_SEC_DAEMON_SOCKET 覆盖；无 HOME/XDG socket 回退。
现有库内 BootstrapConfig::new 仍默认 0600，实际进程使用 0666。单实例锁贯穿已准入请求 drain
与外层 Tokio 退出；残留恢复拒绝普通文件、符号链接、错误所有者和活跃/不确定监听者。

领域状态默认 `/var/lib/agent-sec/skillsec`，root 所有、0700，当前 signing-key.pk8 为 0600。
`/etc/agent-sec/skillsec.json` 或 --skillsec-config 可注入 stateDir、精确 managedSkillDirs、
scanners 和 parsers；配置要求 root 所有、不可被普通用户写入，不接受 HOME 配置覆盖。
配置、状态与 runtime 路径校验不跟随符号链接。客户端有业务访问权限不意味着有密钥或配置权限。

换钥不执行 DPROC-017 的 V1 state migration：本模块明确采用新信任域，不导入历史密钥和记录。
旧部署回退须使用对应 V1 状态，不能混合两个写入者。启动恢复先完成私有换钥 intent，再 reconcile
登记 Skill；通过公共 Action Runtime 留审计。恢复失败保留状态供管理员排查，不伪装成功；普通
方法仍可提供状态与诊断。

第七批交付 `packaging/systemd/agent-sec-core-v2.service.in`，安装到 system unit 目录，
不安装 V1 user unit。服务以 root 运行，`UMask=0077`，runtime 为 0755，state/log 为 0700；
仅保留 `CAP_DAC_OVERRIDE`、`CAP_CHOWN`、`CAP_FOWNER`，不授予 `CAP_SYS_ADMIN`。
`NoNewPrivileges` 和内核保护保持开启；HOME、`/tmp`、系统 Skill 目录及共享挂载必须可访问，
以支持扫描、发布和恢复。`TimeoutStopSec=75` 覆盖连接 drain、SkillFS worker 收尾与运行时退出。
服务 active 仍不等于业务就绪，必须通过 CLI 状态查询和真实业务验证。

`make install-core-v2` 安装核心二进制、system unit 及初始 0600 配置；保留已有配置，
不创建签名密钥或启用 Hook。V2 RPM 的 CLI 子包采用 systemd system macros 和
`%config(noreplace)`，不依赖 Python Ledger。V1/V2 CLI 包名和二进制路径相同，切换前必须停止
旧写入者并备份匹配状态。源码 staging 检查不替代真实 RPM 安装或 systemd 生命周期验收。
第七批已在 Alibaba Cloud Linux 4 x86_64 完成源码安装、正常依赖解析的 RPM 安装、真实 PID 1
systemd 255 下的 45 项操作及 12 项包恢复检查。与新主线对齐后，源码安装套件通过 914 项，
RPM 安装套件通过 914 项，并单独通过修正后的 systemd 生命周期用例（含 75s 超时及限流）；
源码套件跳过 38 项（含无 systemd PID 1），RPM 跳过 37 项规则元数据/清单及 telemetry；
两套均排除 2 项真实模型用例，失败尝试与单项复验证据分开保存。
这些结果不代表 Agent Hook 接入或 V1 降级验收。

| ID | 必须验证 | 可执行证据 |
|---|---|---|
| DPROC-SG-001 | 公共 socket 上普通 UID 调用、换钥拒绝、导出归属 | `v2/apps/asc-cli/tests/skill_sec.rs` |
| DPROC-SG-002 | 启动换钥恢复、旧密钥撤销与重新建立信任 | `asc-capability-skill-sec/src/service/administration.rs` 测试 |
| DPROC-SG-003 | 核心接口保持普通方法原超时和权限 | `asc-daemon-handler/src/skill_sec.rs` 测试与原 PAP/CodeScan 回归 |
| DPROC-SG-004 | 真实二进制 staging、配置权限/保留、V1/V2 unit 分离 | `tests/packaging/test-skillsec-install.sh` |
| DPROC-SG-005 | 安装后 CLI 核心流程、公共审计及进程退出 | `tests/v2/e2e/test_skillsec_cli_e2e.py`、`test_daemon_process_e2e.py` |
| DPROC-SG-006 | 实际 RPM 安装、system unit 启停/重启、普通 UID 与用户/系统 Skill 操作 | 第七批独立 Linux 安装环境执行；staging 和进程 fixture 不替代此证据 |

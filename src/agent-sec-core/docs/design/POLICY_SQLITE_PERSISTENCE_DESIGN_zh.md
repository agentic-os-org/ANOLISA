# Policy SQLite 持久化与崩溃恢复设计

文档类型：`[TARGET V2][IMPLEMENTED]`。daemon 已使用 SQLite 保存 Policy、Scope、Binding
和部署责任；内存后端仅用于契约测试。本地验收证据及未覆盖边界见第 9 节。
本文承接 [Policy/Scope/Binding 契约](POLICY_SCOPE_BINDING_CONTRACT_zh.md) 与
[Reconciler Runtime 设计](BINDING_RECONCILER_RUNTIME_DESIGN_zh.md)，替代后者关于 SQLite
crate 归属、deployment 分表建议及状态 CAS 的早期提案；其余生命周期语义继续适用。
PolicyTemplate 表示可被多个 Scope 引用和复用的策略，其身份、revision 与规则内容独立于
具体主体分配；“禁止删除文件”仅为当前执行后端支持的规则场景。
第 3.4 节说明通用规则内容的首版持久格式。第 9 节保留既有验收记录，
新规则内容的证据见第 3.4 节及对象契约第 7 节。

## 1. 交付目标与边界

一次 PAP 写入只有在 SQLite 提交成功后才能返回成功。daemon 崩溃后，重启能够找回已接受的
Policy、Scope、Binding 意图，以及仍需确认或清理的远端目标；不会因本地状态丢失制造孤儿。
数据库提交与远端调用没有共同事务，采用“先保存责任，再执行；不确定时保留责任并重试”。

本阶段交付单 host、单 daemon、单本地数据库的恢复。继续使用现有 WorkQueue 和单 Binding
单执行者，不增加通用 Job 系统、outbox、ORM、分布式锁或多写者服务。

| 项目 | 决策 |
|---|---|
| Policy Template | 每个 ID 一个 current 完整记录，保留 revision 分配头 |
| Scope | 无 revision 的不可变 Assignment，保存指定 Policy revision 的完整快照 |
| Binding | 自动创建，保存自身单策略 spec、status、内部 `status_version` 和 deployments |
| Deployment | Binding 聚合内部的目标责任，首版存 JSON 数组，不增加公开资源 |
| SQLite | 独立 `policy-state.db`，WAL + `synchronous=FULL` |
| Scope 删除 | 停止 discovery 并完成全部 Binding 清理后删除 Scope；最后一条 Binding 与 Scope 同事务删除 |
| Discovery | 保存恢复所必需的 pin 和停止屏障；线程、扫描缓存不落库 |
| 重试 | WorkQueue 的次数、单调时钟 deadline 不落库，重启重置自动重试预算 |
| CAS | status 单独计数；Deployment 观察合并不能被 status 冲突抹掉 |
| 存储故障 | Repository 分类并核实提交，Runtime 暂停/恢复调度；存储重试不消耗远端重试预算 |
| route | 保留单个 `agentsight` route；cleanup 保存非敏感 endpoint，调用前与当前配置校验 |

Policy、Scope 和 Binding 中的完整 Policy 快照仅保存 ID、名称、revision 和 authored template。
恢复后的 Apply 由 Adapter 将 Binding 中的模板直接编译成目标 DSL。

本阶段不解决：PEP 重启后对所有 Ready Binding 重新审计、跨 daemon 的远端 fencing、
远端请求在 daemon 退出后延迟执行的排序，以及硬件不兑现同步写承诺时的数据可靠性。
客户端仍须满足同身份幂等操作和可靠 absence 分类；仅靠 SQLite 无法提供远端 exactly-once。
真实 AgentSight/kernel enforcement 和物理断电恢复需要各自的验收，不能用 mock 或 SIGKILL 代替。

## 2. 代码归属与依赖

新增 `v2/crates/asc-policy-repository-sqlite`。名字覆盖 Policy 领域：它同时服务 PAP 管理、
PCP reconcile 和 Runtime 扫描，不应叫 PAP 专属后端。

| crate | 职责与此次变化 |
|---|---|
| `asc-policy-types` | Policy、Scope、Binding 业务模型、模板校验及公共 wire；不加入数据库连接或队列状态 |
| `asc-pap` | 管理操作、Scope 准入/删除/重试、`PapRepository`；内部通知携带提交版本回执 |
| `asc-policy-repository` | `BindingStateSnapshot`、Deployment、CAS/写回执与扫描端口；保持 backend-neutral |
| `asc-policy-repository-sqlite` | 实现上述端口，负责 schema、事务、编码、约束、严格打开与存储错误分类 |
| `asc-pap-repository-memory` | 保留测试后端，与 SQLite 执行同一套契约；本次不顺带改名 |
| `asc-pcp` | 状态转换、认领、UNKNOWN 登记、观察合并、中断恢复；不写 SQL |
| `asc-policy-runtime` | 单 Binding 执行所有权、队列补扫、重试和异常收尾；不决定存储布局 |
| `asc-policy-adapter-agentsight` | 从 Binding 的模板快照直接编译 AgentSight DSL；不读取模板库或数据库 |
| `asc-agentsight-client` | 在版本化 cleanup 中保存 endpoint，远端调用前校验配置一致；每次执行重新读取凭据 |
| `asc-daemon` | 连接、数据目录租约、启动恢复与 shutdown 装配；生产使用 SQLite |

新后端依赖 `asc-pap`、`asc-policy-repository`、领域类型及 workspace 已有的 `rusqlite`、
serde、uuid 等。业务层不反向依赖 SQLite；daemon 把同一个后端实例交给 PAP、PCP、Runtime。
daemon reconciliation 装配接受满足三个端口的共享实现，生产注入 SQLite，测试可注入内存后端。

`asc-persistence-sqlite` 当前面向事件/可观测性存储，不把控制状态塞进去。
Policy 与 Security Event、Observability 共用 `asc-sqlite-kernel` 的
`TableSpec`、`ColumnSpec`、`IndexSpec` 及 SQL 生成器；Policy 表额外声明 `STRICT` 和表级约束。
当前 `SqliteStore` 的损坏恢复会删除
DB/WAL/SHM，schema converger 遇到更新的版本会告警后返回，连接默认采用 NORMAL。
这些容错语义不适用于 Policy 权威状态。Policy 使用 rusqlite 实现严格连接和事务迁移，
仅共用 schema 声明及生成逻辑，事件存储保持既有行为。
源代码依据见 [Store](../../v2/crates/asc-sqlite-kernel/src/store.rs)、
[schema](../../v2/crates/asc-sqlite-kernel/src/schema.rs) 与
[connection](../../v2/crates/asc-sqlite-kernel/src/connection.rs)。

## 3. 数据库与运行约束

### 3.1 路径、安全与独占

使用既有 daemon 数据根 `AGENT_SEC_DATA_DIR`，未设置时为 `/var/log/agent-sec`，数据库名
`policy-state.db`；不引入 HOME 或临时目录 fallback。沿用 system daemon 身份和授权，
CLI 通过 PAP 访问，不能直接打开数据库。数据库及其辅助文件包含完整策略和 cleanup 数据，
必须限制为服务身份可访问：数据库/锁文件 0600，数据库所在私有目录 0700。
已有共享数据根如不满足私有目录要求，启动报错并明确指出配置问题，不擅自 chmod 共享目录。

安全打开按目录描述符逐级检查所有者与权限，拒绝 symlink、非普通文件、硬链接别名及不可信父目录。
受控 sticky 临时根只用于隔离测试。整个运行期固定路径/文件身份，不支持运行中替换 DB。

除了 runtime/socket lease，还要持有数据库同目录的 `policy-state.lock` 独占进程锁。
不同 socket 路径也不能让两个 daemon 打开同一 Policy 状态。锁文件不 unlink，不通过替换 inode
恢复锁；进程崩溃由 OS 释放锁。租约必须一直存活到所有 worker 和外层 Tokio blocking drain
结束；超时后由进程退出最终截断，而不是先释放锁让另一个 daemon 进入。

### 3.2 连接与持久性

每个 Repository 使用一个 `Mutex<Connection>`，短事务串行访问；不先引入连接池。
同步 SQL 继续在现有 blocking 执行路径调用，不能阻塞 UDS async accept loop。
编译、procfs 扫描、Client 调用、线程 join 都在事务外执行。

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;
PRAGMA busy_timeout = 200;
```

设置后读回核对实际值；事务内不改变 PRAGMA。FULL 是本阶段明确选择，接受每次提交同步
带来的延迟。SQLite 的 WAL/FULL 在提交时同步 WAL；保证以 OS、文件系统、设备正确兑现同步为
前提，不能从用户态测试推导物理断电保证。见 [SQLite synchronous](https://sqlite.org/pragma.html#pragma_synchronous)。

只支持本地持久文件系统，不把 WAL 放到网络文件系统。WAL 属于数据库持久状态，不能在恢复前
删除或只复制主 DB 文件。活跃数据库备份使用 SQLite backup API 或经验证的一致备份流程；
checkpoint 负责回收 WAL，成功 checkpoint 不是每次业务提交的额外前提。
长读事务、磁盘占用和 checkpoint 失败需要诊断。见 [SQLite WAL](https://sqlite.org/wal.html)。

首次初始化在提交 schema 后，同步新建目录项及必要的父目录，再对外接受成功写入。
重启检查 schema、外键和数据编码；使用真实的可回滚写探针确认可写，`SELECT 1` 不能证明可写。
探针失败使启动失败。schema 初始化失败可以留下待检查的空文件，但不得删除已有权威数据重建。

### 3.3 schema 版本与错误

当前 `PRAGMA user_version=1`。仅全新、空且无业务 schema 的 version 0 数据库可以初始化。
表定义位于 [table.rs](../../v2/crates/asc-policy-repository-sqlite/src/table.rs)，
使用与事件存储一致的声明方式，由共享生成器生成建表和索引 SQL。
迁移入口为 [migration.rs](../../v2/crates/asc-policy-repository-sqlite/src/migration.rs)，
`Migration` 函数接收当前事务，有序 `MIGRATIONS` 列表中的第 N 项负责 N→N+1。
当前只注册 0→1 初始化，不导入旧 Policy 数据库，也不提供生产 v2 迁移。

runner 在同一个 `BEGIN IMMEDIATE` 事务中读取版本、顺序执行缺失步骤并更新 `user_version`；
DDL、数据转换及版本号一并提交，失败回滚。步骤可以执行 SQL 或转换持久 JSON，但不得自行
提交或执行外部 I/O。已到当前版本不重复迁移；未知新版本、非空 version 0 数据库显式拒绝。
版本升级在当前格式的记录解码和 discovery/reconcile 启动前完成。

以后发生真实 schema 变化时，在列表末尾添加升级函数及所需 SQL，保持已发布的
v1 `TableSpec` 和迁移步骤不变；同步更新数据解码和旧版本 fixture。
例如新增字段通过 1→2 步骤执行 `ALTER TABLE` 及必要的数据回填；仅修改 Rust 表声明
不会升级已有数据库。Policy 不使用事件存储的 `extra_columns` 自动补列机制。
schema 校验的预期结构由同一迁移链在内存空库中生成，随新增迁移步骤推进，
不再单独使用 v1 DDL 代表所有后续版本。
不增加自动降级、自动增列或“尽力可用”的修复。支持版本必须校验实际表、列、索引、约束和
编码；损坏及不兼容 JSON 一律显式拒绝。

写事务采用 `BEGIN IMMEDIATE`，先取得写权再读/判定/修改，减少读事务升级竞争。
BUSY 按有界 timeout 返回失败；磁盘满、只读、I/O、损坏分别保留内部错误分类。
错误绝不等价于记录不存在或远端 ABSENT。提交失败须确认 rollback/autocommit 状态；
无法确认的连接停止服务并隔离，重新打开核实结果，不能把未结束事务交给下一请求。
Repository 只负责事务、错误分类与连接恢复，不在持锁期间循环等待存储修复；Runtime 的暂停、
恢复和重试预算按第 7.4 节执行。CAS Conflict 属于并发结果，不等同于存储故障。
SQLite 的部分错误可能终止语句或整个事务，处理须核对连接状态，见
[SQLite transaction errors](https://sqlite.org/lang_transaction.html#response_to_errors_within_a_transaction)。

### 3.4 通用规则改造的首版格式边界

策略内容结构改造见 [生命周期契约第 7 节](POLICY_SCOPE_BINDING_CONTRACT_zh.md#7-通用规则结构改造计划)。
此次已决定不兼容旧开发期 Policy 数据库，不实现旧模板 JSON 转换；这是本次变更的明确边界，
不取消后续版本的迁移入口。

按首版交付处理，保持 `PRAGMA user_version=1` 及既有三个业务表。
物理表/列无需变化；首版 JSON 编码统一使用通用规则。全新空 version 0 数据库经既有
初始化入口事务创建，后续真实版本升级继续使用第 3.3 节的有序事务迁移入口。
本次不增加格式升级步骤，不导入旧开发期模板，不提供自动降级。
已有同版本开发库若包含旧 kind/files 模板，在启动记录解码及领域验证阶段显式拒绝；
该错误属于不兼容业务编码的严格打开失败，不能因物理 schema 相同就启动 worker。
未来首版发布后如再次修改持久 JSON，应递增数据库版本并实现需要支持的数据迁移。

受影响的 JSON 为 `policies.current_json`、`scopes.assignment_json` 中的 policySnapshots，
以及 `bindings.spec_json` 中的单策略快照；三处统一使用通用规则格式。
删除 Policy 后 current 为空，不代表 Scope/Binding 中没有旧模板；不能只检查 policies 表
就允许旧库启动。Scope/Binding 身份、revision、status_version、deployments 及 cleanup 的
职责不因模板格式改变而改变，不新增规则表、历史模板表或自动 schema converger。

旧库拒绝启动时不删除、置空、重建或自动替换原 DB/WAL，不进行网络 Apply/Delete。
使用新库不会接管旧库已部署的远端策略，也不代表旧部署已被清理。仍有旧部署时，
先使用能读取旧库的旧二进制删除 Scope 并确认清理完成，再人工安排新库；不能以新库启动成功
证明旧责任已经结束。不提供自动降级或运行中替换数据库。

验收须覆盖新库初始化/重开、三处快照重启解码、旧库及未知新版本拒绝、初始化失败回滚，
以及拒绝旧库后业务数据和部署责任仍保留。原有 crash 测试在通用规则库重新执行；
旧开发格式的既有结果不能代替这些验收。新增用例位于
[SQLite 契约测试](../../v2/crates/asc-policy-repository-sqlite/tests/contracts.rs)：
`general_policy_rules_survive_reopen_in_current_scope_and_binding_snapshots` 验证同一通用策略被
两个 Scope 复用、三处快照重开及来源 Policy 删除后的独立性；
`legacy_policy_payloads_are_rejected_without_discarding_saved_responsibility` 分别向三处快照
注入旧编码，验证启动拒绝且策略数据和 UNKNOWN 部署责任仍保留。

## 4. 存储模型

### 4.1 三个业务表

以下为首版目标 DDL，JSON 内部格式随 schema 版本固定。数据库约束之外，Repository 在读写时
调用领域验证并交叉核对 JSON 与投影列；非法数据报存储错误，不跳过记录继续启动。
`Revision` 保持正 u32；`status_version` 使用正 i64 范围，达到上限时拒绝递增，不回绕。
进程 `start_time` 是 u64，以规范十进制 TEXT 保存，避免截断到 SQLite 有符号 INTEGER。

```sql
CREATE TABLE policies (
    policy_id TEXT COLLATE BINARY PRIMARY KEY NOT NULL,
    last_allocated_revision INTEGER NOT NULL
        CHECK (last_allocated_revision BETWEEN 1 AND 4294967295),
    current_json TEXT CHECK (current_json IS NULL OR json_valid(current_json))
) STRICT;

CREATE TABLE scopes (
    scope_id TEXT COLLATE BINARY PRIMARY KEY NOT NULL,
    assignment_json TEXT NOT NULL CHECK (json_valid(assignment_json)),
    phase TEXT NOT NULL CHECK (phase IN ('ACTIVE', 'DELETING')),
    pinned_process_json TEXT CHECK (
        pinned_process_json IS NULL OR json_valid(pinned_process_json)),
    discovery_stopped INTEGER NOT NULL DEFAULT 0
        CHECK (discovery_stopped IN (0, 1)),
    CHECK (phase = 'DELETING' OR discovery_stopped = 0)
) STRICT;

CREATE TABLE bindings (
    binding_id TEXT COLLATE BINARY PRIMARY KEY NOT NULL,
    scope_id TEXT COLLATE BINARY NOT NULL REFERENCES scopes(scope_id) ON DELETE RESTRICT,
    policy_id TEXT COLLATE BINARY NOT NULL,
    boot_id TEXT NOT NULL,
    pid_namespace TEXT NOT NULL,
    pid INTEGER NOT NULL CHECK (pid BETWEEN 1 AND 4294967295),
    start_time TEXT NOT NULL CHECK (
        length(start_time) BETWEEN 1 AND 20 AND
        start_time NOT GLOB '*[^0-9]*' AND substr(start_time, 1, 1) BETWEEN '1' AND '9'),
    binding_revision INTEGER NOT NULL CHECK (binding_revision BETWEEN 1 AND 4294967295),
    spec_json TEXT NOT NULL CHECK (json_valid(spec_json)),
    phase TEXT NOT NULL CHECK (phase IN (
        'PENDING_APPLY', 'APPLYING', 'READY', 'APPLY_FAILED',
        'PENDING_DELETE', 'DELETING', 'DELETE_FAILED')),
    error_json TEXT CHECK (error_json IS NULL OR json_valid(error_json)),
    status_version INTEGER NOT NULL CHECK (status_version >= 1),
    deployments_json TEXT NOT NULL CHECK (json_valid(deployments_json)),
    last_write_id TEXT,
    last_write_digest BLOB,
    last_write_result_json TEXT CHECK (
        last_write_result_json IS NULL OR json_valid(last_write_result_json)),
    CHECK ((last_write_id IS NULL AND last_write_digest IS NULL AND last_write_result_json IS NULL)
        OR (last_write_id IS NOT NULL AND last_write_digest IS NOT NULL
            AND length(last_write_digest) = 32 AND last_write_result_json IS NOT NULL)),
    UNIQUE (scope_id, policy_id, boot_id, pid_namespace, pid, start_time)
) STRICT;

CREATE INDEX bindings_by_scope ON bindings(scope_id, binding_id);
PRAGMA user_version = 1;
```

`assignment_json` 只含不可变 selector 和完整 `policySnapshots`，Scope 身份/phase 由列聚合，
不在 JSON 中维护第二份可变 status。`spec_json` 是完整 PreparedBinding；身份、revision、
来源 Scope、Policy 与完整进程身份必须和投影列一致。所有列在同一事务写入。
初始版本不存 `Deleted` 行：目标确认清理后删除整个 Binding。

Policy 删除只置空 current_json，不删除 revision 分配头；current 的 revision 必须等于 head。
Scope/Binding 不对 current Policy 加外键，模板更新/删除不能阻断旧快照继续工作。
生产路径中同 Scope 内 Policy ID 唯一且 revision 固定，因此去重键不必再加 Policy revision。
Binding 创建由 `sync_scope_instances` 按 Scope 快照和进程身份展开；删除、重试与
reconciliation CAS 使用各自的原子写入口。`update_binding` 仅为 memory 测试后端的
revision/CAS 构造辅助方法，不属于生产 Repository 契约或 SQLite 实现。

系统继续生成新的 UUID，不复用已删除 Scope/Binding ID，不提供客户端指定旧 ID 的入口。
Policy revision 分配头不能按 TTL 回收，否则同 ID 重建可能复用 revision；历史 ID 很多时的
空间成本须监控，未来回收必须配套明确的 ID namespace/退休规则。

### 4.2 Deployment 是 Binding 内部责任

沿用 [BindingStateSnapshot 与 Deployment](../../v2/crates/asc-policy-repository/src/lib.rs)：

```text
BindingStateSnapshot
  binding: BindingView { spec, status { phase, error } }
  status_version: positive counter
  deployments[]:
    target: { route, id, cleanup }
    revision: binding revision
    presence: UNKNOWN | PRESENT
    last_confirmed: optional PRESENT | ABSENT
```

`target.cleanup` 是 Client 能独立执行清理所需的版本化 opaque 数据，不能只有无法重建的临时句柄。
沿用 TargetRef 的无凭据契约：cleanup 不保存认证 token，Client 根据 route 从受管配置取得凭据。
ABSENT 观察合并后从责任集合移除该 target；超时、panic 和不明确 not-found 均保留 UNKNOWN。
不存完整 plan、prepared request 或远端响应。公共 BindingView 不输出内部 cleanup 或 CAS 字段。

Deployment 通常只有一项，但旧 revision/不同 target 的清理可能暂时并存。用 Binding 行内 JSON
可一次读取/提交完整聚合，首版不引入单独 Deployment CRUD 和关联表。代价是数组修改要重新编码，
写入量高或目标数变大后再评估分表；当前已有单 Binding 执行串行的约束。

Policy/Scope 继续遵守编码后 1 MiB 单记录预算，公共列表条目预算 3 MiB、响应 frame 4 MiB。
公开列表保持 ID 的字节序（`COLLATE BINARY`），在库内分页，total/items 在同一读事务读取；
byte budget 缩短页时按实际 items 数推进。首版内部 deployments 最多 32 个不同 target，
整个数组编码后最多 1 MiB，在 UNKNOWN 登记前检查。达到界限必须在新增远端 I/O 前拒绝，
不能为省空间丢弃尚未清理的责任；已有 target 的观察使用已登记 cleanup，只更新观察字段。

### 4.3 route 与 endpoint 的恢复约束

`TargetRef.route` 是选择 Client factory 的稳定配置键，不是 URL。首版继续使用 daemon 当前的
单个 `agentsight` route 和本机 AgentSight，不增加 route 表、多 endpoint 管理或自动迁移。
当前装配及 Client 契约见 [daemon reconciliation](../../v2/apps/asc-daemon/src/reconciliation.rs)
与 [AgentSight reconciliation](../../v2/crates/asc-agentsight-client/src/client/reconciliation.rs)。

仅保存 route 不能检测重启后的地址变更：旧目标在 endpoint A，而同名 route 被改到 B 时，
B 返回不存在不能证明 A 已清理。因此目标方案将 AgentSight cleanup 升至 schema version 2，
在 Binding ID/revision 之外加入规范化 `endpoint`，与 UNKNOWN 责任一同提交后才允许远端 mutation。
该字段仅用于比对，不允许用数据库里的 URL 绕过当前受管配置发请求。

规范化复用 [transport 的 base URL 规则](../../v2/crates/asc-agentsight-client/src/transport.rs)：
比较包含 scheme、host、port、API path 的规范字符串，保留现有 URL 安全校验；不通过 DNS
解析或地址别名猜测两个 endpoint 等价。数据库不保存 token、Authorization header 或 URL 凭据。

Client 在 create/update/delete/observe 的任何远端调用（包括 preflight）前，必须确认所有涉及的
TargetRef 的 route、cleanup 版本和 endpoint 都与当前 Client 匹配。旧责任未通过校验时，
不能用当前地址生成的 cleanup 覆盖它，也不能开始新的目标下发。不匹配时返回明确配置错误，
保留原 Deployment，不连接 B、不记录 ABSENT；没有 endpoint 的旧 cleanup 不能补填为当前地址。
旧内存版本的 cleanup schema version 1 不作为新持久库的恢复输入，升级遵守第 10 节清理流程。

管理员须恢复 A 的配置，再通过现有 Scope retry 推进已进入终态失败的 Binding；清理所有指向 A
的责任后才能迁移到 B。凭据继续由 factory 每次执行从当前 token 文件读取，正常轮换不改变
endpoint，也不修改持久 cleanup。该校验只能发现地址配置变化，不能识别相同地址后面更换了
PEP 实例；PEP 身份校验、Ready 重审及跨进程 fencing 仍在本阶段边界之外。

## 5. status_version 与条件写

### 5.1 为什么单执行者还需要 CAS

WorkQueue 保证同一 Binding 不同时运行两个 reconcile，因此本阶段 Deployment 的合法写者只有
当前执行者。PAP 的 Scope 删除/显式 retry、入队失败收尾及 Runtime 异常收尾仍可与该执行者
竞争修改 status。status 值相同也不表示同一次操作：Pending → Failed → Pending 是 ABA。

保留三种版本含义：Policy revision 表示模板内容；binding revision 表示内部 spec 内容；
`status_version` 只保护生命周期和错误说明。Scope 不增加 revision，不引入整个聚合的全局版本。

新 Binding 的 status_version=1。每次实际改变 phase 或 error，在同一事务递增一次；
幂等 no-op 不增版，Deployment-only 写不增版。所有写路径均遵守，包括 PAP、claim、失败、
中断恢复及保留的底层 repo 契约。禁止直接无条件 `UPDATE phase`。

### 5.2 expected_version 来自哪里

1. Worker 从一次一致读取获得 `BindingStateSnapshot(status_version=v)`。
2. 用 v 认领 Pending，事务返回 `Applying/Deleting` 的新版本 v+1；认领失败则重新读并决策。
3. 此次执行的 completion 使用认领回执 v+1，不使用执行结束时随手读到的最新版本。
4. PAP 的创建/退休/retry 事务返回内部 `BindingIntentReceipt { binding, status_version }`。
   入队失败仅使用这次提交返回的版本，不能在失败后重新读取 Pending 并套用旧错误。
5. 删除、异常收尾和恢复同样携带自己实际读取/认领的版本；冲突表示重新决策，不能盲目重放旧结果。

事务的 status 部分可以用以下 SQL 表达，领域合法转换仍由端口验证：

```sql
UPDATE bindings
SET phase = :next_phase, error_json = :next_error,
    status_version = status_version + 1
WHERE binding_id = :id AND binding_revision = :revision
  AND status_version = :expected_version
  AND status_version < 9223372036854775807
RETURNING status_version;
```

完整 Snapshot 一次读出 spec/status/version/deployments，避免拼出不存在的组合。
version 不加入 CLI 输入或公共 JSON；通知队列仍只把 Binding ID 当 wakeup，执行前重读。
内部 PAP 返回回执是为了失败收尾，不让队列中的版本代替 Repository 当前事实。

### 5.3 独立合并观察与状态

条件写接口需区分以下三类事务，不能继续把“status 没变”作为整个结果事务的成功前提：

| 操作 | 条件与结果 |
|---|---|
| claim / 状态推进 | ID、binding revision、expected status_version 和合法 phase 匹配才推进；返回新版本 |
| 新目标 UNKNOWN 登记 | 检查认领仍有效，并将完整 TargetRef/cleanup 合并入最新 deployments，提交后才允许 mutating I/O |
| 执行结果提交 | 验证目标来自已登记责任，合并此执行者的观察；status 仅在原 claim 仍匹配时更新，返回两个独立结果 |
| 最终清理删除 | Delete claim 的版本匹配且本次有效观察证明全部目标 ABSENT，原子删除整个 Binding；有冲突则保留/合并责任 |

结果事务由 Repository 在写锁内读取最新行；PCP 提供可验证的目标观察和状态建议。
它不是无条件覆盖整个旧 Snapshot，也不能让任意观察更新未登记目标。
部署观察必须保留对应 target 和 binding revision 的来源，不能把旧 revision 的 PRESENT
归给新 spec。修改状态失败时，合法的部署观察仍能提交，返回 `status_applied=false`。

最终删除可以把最后的 ABSENT 观察与行删除放在同一个事务：不能要求数据库里在进入事务前
就已是空 deployments，否则最后一次 cleanup 永远无法提交。反之，仅“收到删除意图”不允许删行。
删除最后一条 Binding 时，同一事务检查父 Scope 为 Deleting、discovery_stopped 且无剩余
Binding，满足条件就同时删除 Scope 及其快照。若停止屏障尚未完成，则在保存屏障的事务中
检查并删除空 Scope。全部 Binding 删除成功后不再等待独立的后台 finalize 或用户再次 delete。
Active Scope 不因暂时没有 Binding 而删除，仍按 selector 继续发现实例。

所有写 API 是窄 patch，reconcile 不携带可覆盖的 spec。SQLite 不比较整段 Deployment JSON
来代替所有权；同 Binding 的异常收尾完成前也不释放执行 slot。多进程并发不在此方案保证内。

### 5.4 并发例子

```text
Apply 与 Scope delete：
  PendingApply v1 -> claim Applying v2 -> register UNKNOWN -> remote apply
  PAP: close Scope admission -> request PendingDelete v3
  old worker: merge PRESENT; CAS(expected=v2) cannot write Ready over v3
  delete worker: claim Deleting v4 -> remove/observe ABSENT -> delete aggregate

迟到的入队失败：
  admission receipt PendingApply v1
  worker/runtime: Failed v2; explicit Scope retry: PendingApply v3
  old enqueue returns failure: CAS(expected=v1) fails, v3 remains pending
```

即使 PAP 入队失败时没有 worker 正在执行，旧请求的失败处理仍可能晚于其他通知、补扫或新 retry。
使用提交回执同时覆盖这类未来扩展场景，不以“现在通常不会发生”省略版本契约。

### 5.5 commit 成功但回执丢失

沿用 write_id，并把最新 reconcile 写回执持久化：UUID、操作摘要、是否推进 status 及其返回版本。
摘要用 workspace 已有 SHA-256，输入是固定编码的 ID、expected 版本/修订号、操作种类和 patch；
同 write_id 不同内容返回 Invalid。回执不保存 plan、payload 或完整响应。

一次执行存在未确认写入时不得启动其下一次 reconcile 写。Repository 在同一事务更新数据和回执；
PAP 改 status 不清除此 reconcile 回执。提交后返回失败/panic，可重新打开连接查询该 write_id：
匹配则确认 AlreadyApplied；不匹配时重读权威状态重新决策，不能用最新版本强行重放。
只保留最近一次回执足够覆盖本地单执行者的未确认调用，不是永久操作历史或公共 exactly-once API。
删除成功后行不存在可确认该 ID 已清理；生产 ID 不复用，查询错误不能当作不存在。

PAP 的 Scope create 使用服务端先分配的 ID 核实不确定提交；无法确定时返回既有 persistence/internal 错误，
由查询核实。跨请求重复 create 不新增 idempotency-key 保证。不能因“不确定是否提交”而开始
远端下发，只有已读到持久意图和已确认 UNKNOWN 登记后才能继续。

## 6. PAP、发现与清理的事务边界

| 操作 | 一个原子事务内的工作 | 提交后的工作 |
|---|---|---|
| Policy create/update | 检查分配头/expected revision，更新 current 和 head；内容重放保持不变 | 返回已提交记录 |
| Policy delete | 检查精确 revision，清空 current，保留 head | Scope 不变 |
| Scope create | 再次核对所有请求 Policy 的精确 current；检查容量；保存不可变快照 | 启动 discovery，返回 Scope |
| PID 首次选中 | Active Scope 条件下保存首次 ProcessIdentity pin，创建对应 Binding | 返回 intent receipts 并通知 |
| 实例同步 | 检查 Scope Active；按完整实例键去重创建；为确认退出/不匹配的旧实例请求 Delete | 通知变更 ID；通知失败按原回执条件收尾 |
| Scope delete 第一步 | Scope 置 Deleting，关闭所有新 Binding 准入；重复调用不改变意图 | stop/join 对应 discovery |
| Scope delete 第二步 | 保存 discovery_stopped，子 Binding 请求 PendingDelete，保留 deployments；无子记录则同事务删除 Scope | 通知 cleanup；失败可重试删除流程 |
| Scope retry | 只重置对应终态失败 Binding 的 phase/error，递增版本 | 通知并开启新的进程内重试预算 |
| Binding claim/结果 | 依第 5 节验证并写入 status/目标责任或删除整个聚合；最后一条 Binding 与符合删除条件的 Scope 同事务移除 | 再执行远端 I/O 或发出下一 wakeup |

### 6.1 Policy 与 Scope 创建竞争

PAP 在事务外解析和校验模板，但 `put_scope` 必须在事务内校验所有精确 current
revision 和快照内容。与并发更新/删除有明确先后：

- Scope 提交先发生：保存完整旧版本，随后模板更新不影响它。
- Policy 更新先发生：旧 revision 不再 current，Scope 返回 not_found，不悄悄绑定新 revision。

不能只在事务外检查引用后再插入，也不新增历史 Policy 版本库。数据库不必保留被 Scope
引用的旧模板行，因为 Scope 自带完整 PreparedPolicy。

现有 32 个 discovery jobs 的上限保留。Scope admission 在事务内计数 Active Scopes 并占用
名额，避免第 33 个 Scope 提交后、线程创建前崩溃留下无法恢复的超额状态。
registry 仍检查实际 worker 容量；正在退出的 Deleting worker 可能使新建暂时返回 unavailable。
启动失败的补偿仍先保存删除意图并清理，日志记录补偿失败，返回原 discovery.start 错误；
已保存的 Active/Deleting Scope 都能在重启扫描中找到，不能为了回滚而丢弃其子 Binding。

### 6.2 Discovery 哪些内容落库

| 状态 | 持久化 | 原因 |
|---|---|---|
| selector、policySnapshots、Scope phase | 是 | Assignment 及删除意图是权威状态 |
| PID selector 的首次 ProcessIdentity | 是 | 即使 Binding 已清理，重启也不能跟随复用 PID 的新进程 |
| discovery_stopped | 是 | 清理 Scope 的持久屏障；只随 Scope 行存活 |
| 已选中实例 | 从现存非退休 Binding 恢复 | 不另存第二份实例账本 |
| scan fingerprint、procfs 缓存、线程句柄 | 否 | 从当前系统重新构建 |
| notify/queue/dirty/重试次数/deadline | 否 | Repository 补扫重建；单调时间不能跨进程直接复用 |

新增内部一致读 `ScopeDiscoverySeed`，包含 Assignment、pin 和仍属 Apply/Ready/ApplyFailed
的已选中 ProcessIdentity；排除 PendingDelete/Deleting/DeleteFailed。创建与重启均用此 seed
启动同一种 worker，不设另一套恢复扫描算法。进程名称/path selector 可继续发现新实例。

对 PID selector，首次 pin 和该实例的初始 Binding 必须同一事务提交；以后 pin 只能相等，
不能因子 Binding 消失清空。空扫描不设置 pin。boot ID、PID namespace、PID、start time
四项一起验证，reconcile/Client 使用原实例，禁止下发前重新把 PID 解析成其他进程。

恢复后的第一次扫描必须保留 seed 中尚未证实退出的实例。完整扫描、明确 ENOENT 或身份替换
才能触发退休；权限错误、局部读取失败和短暂 procfs 不可用不等价于进程消失。
selector 匹配缓存是提示，持久 Scope/Binding 才是责任来源。

### 6.3 删除与进程退出

Scope delete 一旦提交 Deleting 就不可撤销。stop 失败或崩溃可使它暂时停在 Deleting，
后续 delete 重试或启动恢复继续推进。此时 discovery 即使仍完成一次扫描，也不能通过
`sync_scope_instances` 创建新 Binding。stop/join 必须在写锁外完成，避免 worker 正等待同一数据库锁。

进程退出只退休该 Scope 下对应实例的 Binding。先保存 PendingDelete，再由原有 Client
使用登记的 cleanup 移除目标；进程消失本身不是目标已经 ABSENT 的证据。
名称/path Scope 保持 Active 并发现后续实例，PID Scope 保留旧 pin；不同 Scope 的责任互不删除。

重复 delete 不重置重试预算，也不把 DeleteFailed 自动改回 PendingDelete；只有显式 Scope retry
重试终态失败。ApplyFailed 因新的 Scope 删除意图转入 PendingDelete 属于新操作。
终态失败 Binding 和其所属 Deleting Scope 必须保留，以便查询错误和显式恢复。

全部 Binding 清理并删除成功后，随最后一条 Binding 同事务删除已停止 discovery 的 Scope
及其快照；原本没有 Binding 的 Scope 在停止屏障提交时直接删除。删除完成后 get 返回 not_found，
list 不再包含该 Scope。仅因进程退出而变空的 Active Scope 继续保留，不等同于用户删除 Scope。

## 7. 启动、恢复与 shutdown

### 7.1 启动顺序

1. 保留现有 system daemon 身份校验、runtime lease，并取得数据库 lease。
2. 严格打开数据库，核对 PRAGMA/schema/编码/外键，执行可写探针。
3. 按稳定 ID 分页恢复 Deleting Scope：不为其启动 discovery；独占租约证明旧本地 worker
   已退出，可以完成 stopped 屏障及子 Binding 删除意图；无子 Binding 则在同一事务删除 Scope。
4. 启动 PCP/Runtime，接入同一 Repository 和通知端口。按第 7.2 节恢复 Binding。
5. 按稳定 ID 分页读取 Active Scope 的一致 seed 并重建 worker；任何 worker 创建失败终止
   本次启动并 drain 已启动部分，保留持久记录供下次恢复。
6. 完成恢复扫描的准入设置和必需 worker 启动后开放 UDS/readiness。无需等待远端 Apply/Delete
   全部完成；远端暂时不可用体现为 Binding 重试/失败，不伪装成数据库启动成功或失败。

Scope 启动恢复新增内部 keyset scan：`WHERE scope_id > :after ORDER BY scope_id LIMIT :limit`，
无 cursor 的第一页不加 WHERE。不能复用公共 OFFSET list：清理第一页 Scope 会令后续 offset
跳过记录。Binding 继续使用已有 `scan_reconciliation(after, limit)`。所有扫描按页释放事务，
失败保留 cursor 并有界退避；启动时无法读取权威数据则不对外就绪。
Runtime 后续周期扫描从头重复，补齐并发插入在已过 cursor 之前的记录。

upstream 的 PII 加载、SkillSec 恢复/worker 和 SkillFS 接线继续保留；Policy 的 lease 与
启动失败清理接到同一 composition root，不创建第二个 daemon 或绕过已有 Action Runtime。

### 7.2 Binding 恢复表

| 持久 phase | 重启行为 |
|---|---|
| PendingApply / PendingDelete | 重新入队，从当前 spec 和已有 deployments 开始 |
| Applying / Deleting | PCP 按原版本条件标记为中断后 Pending，保留 deployments；按配置退避重新调度 |
| Ready | 保留；本阶段不自动审计 PEP 是否重启或丢失状态 |
| ApplyFailed / DeleteFailed | 保留错误，不自动重试；等待显式 Scope retry 或合法的新删除意图 |

WorkQueue、AttemptSchedule 的 attempts/deadline 重启从零开始；重复运行某次 reconcile
不会自行重置同一运行周期预算。不断崩溃可能延长累计重试，这是明确接受的首版限制。
不得把终态失败的重启读成新的用户 retry。UNKNOWN 目标始终随 Binding 恢复。

每次 Apply 重新从保存的 spec 翻译和准备，使用同一个 Binding ID/revision、Target ID 和
ProcessIdentity。对旧 boot/namespace/复用 PID 应拒绝新 Apply 并保留已有目标待清理，
由发现结果推进退休，不能向当前同 PID 下发旧策略。
没有 outbox：PAP 提交后通知丢失，由 paged catalog 补扫推进。通知成功不是持久性依据。

### 7.3 shutdown 与故障健康

关闭 UDS admission 并 drain 已受理请求 → stop/join 全部 discovery → drain/停止 Policy Runtime
→ 关闭连接 → 在外层 Tokio blocking drain/进程结束后释放数据库 lease。
SkillSec 的独立 drain 可继续与 Policy 链路并行；不能把 Policy 的 discovery 和 reconcile 停止顺序反转。

退出不等于用户删除 Scope。不要在正常 shutdown 中把全部 Active Scope 改 Deleting 或清空责任。
超时、SIGKILL、存储故障都可能留下非终态行，重启走同一个恢复路径。

### 7.4 存储故障的暂停与恢复

Repository 返回可区分的存储错误和提交确定性，负责 rollback、隔离连接、严格重开及回执查询；
Runtime 负责暂停新调度、定时重试和恢复执行。两者都不能把 SQL 错误改写成远端失败或 ABSENT。

| 故障 | 当前请求/执行 | 恢复条件 |
|---|---|---|
| BUSY，且已确认未提交 | 单次等待最多使用既定 200 ms busy timeout；PAP 返回可重试 unavailable，Runtime 退避后重读/重试存储步骤 | 成功取得事务并确认结果；不在 Repository 内无限循环 |
| FULL、只读、可恢复 I/O 错误 | 报告 Policy storage degraded，拒绝新 Policy 写入意图，暂停所有新的 Policy 远端调用；保留待完成工作 | 修复后由定时探测确认连接、schema/数据及真实可回滚写探针可用；先处理未确认写入，再恢复调度 |
| 提交结果不确定（包括提交边界错误/panic） | 隔离连接，保留执行所有权和原 write_id/patch；确认前不开始下一次写入或远端调用 | 严格重开后先按第 5.5 节查原回执/权威状态；无法确认就继续暂停，不能换 write_id 重放旧结果 |
| 损坏、schema/编码不兼容 | 停止 Policy 调度及写入，报告需人工处理；不自动删库、重建或切内存后端 | 管理员修复或从一致备份恢复后，重新启动并通过严格检查；不靠自动重试跳过错误 |

已发送的远端请求仍可能完成，暂停不等于取消成功。执行者保留观察、待提交 patch、write_id
以及同 Binding 的逻辑 slot，恢复时先核实/提交结果；status 仍使用原 claim 版本，不能覆盖已提交的并发删除意图。
等待使用 Runtime 定时调度，不长期占住 SQL mutex 或阻塞 worker 线程，也不能释放逻辑 slot
让另一执行者进入。若等待期间 daemon 退出，使用已保存的 UNKNOWN 责任及第 7.2 节恢复路径。

存储重试复用 Runtime 的 `storage_retry` 间隔，单次数据库等待有界；可恢复故障持续时可以继续
定时尝试，不受 `max_auto_retries` 或远端 attempts 上限终止。纯存储失败不递增远端预算，
也不单独产生 ApplyFailed/DeleteFailed；已经发送的远端尝试照常计数，恢复存储时不能清零。
回执查询和结果提交重试不算新的远端尝试。PAP 不自动重放结果不确定的 create 请求，沿用第 5.5 节。

恢复前保持 Policy 写入 admission 关闭；可靠读取仍可用于查询已提交状态，读取失败则明确返回
unavailable，不能返回空列表或 not_found。storage degraded 是运行健康状态，不写入每个 Binding
的业务 status，也不改变已有远端策略。启动阶段遇到这些错误仍遵循第 7.1 节，不提前开放 readiness。
读失败、写失败、CAS 冲突、远端失败分开计数。日志使用操作、资源 ID、phase、版本和安全错误码；
不输出完整策略、cleanup、prepared payload、SQL 参数或原始 panic 内容。

## 8. 崩溃窗口与恢复结果

| 崩溃位置 | durable 事实 | 重启要求 |
|---|---|---|
| Policy/Scope 事务提交前 | 原状态 | 不接受半条记录或部分策略集合 |
| Scope 已提交、worker 尚未创建 | Active Assignment | 重建 discovery；仍受 durable 容量限制 |
| PID pin/Binding 插入中 | 两者全有或全无 | 不出现无 pin 的已生效 PID Binding |
| Binding Pending 提交、通知未到 | Pending | catalog 补扫 |
| claim 后、准备/登记前 | Applying，原 deployments | 中断恢复，重新准备；没有新远端责任可丢失 |
| UNKNOWN 提交后、Apply 前 | 可清理 TargetRef + UNKNOWN | 允许幂等重试/观察；不得认定未执行而删责任 |
| 远端 Apply 成功、本地结果提交前 | UNKNOWN | 用相同身份重试或观察；删除仍可找到目标 |
| PRESENT/Ready 提交后、返回前 | 已提交结果及回执 | 查回执确认，不重复覆盖新 status |
| Apply 期间 Scope 删除 | Deleting Scope/新 PendingDelete 版本及目标责任 | 旧 Apply 不能回写 Ready，继续清理 |
| Scope Deleting 后、join 前/后 | 删除意图，stopped 可能为 false | 旧进程死亡后推进屏障，不启动该 Scope worker |
| 远端 delete 成功、本地删除前 | UNKNOWN/PRESENT 清理责任 | 重试；Client 可靠确认 ABSENT 后删记录 |
| 已停止 discovery，最后一条 Binding 与 Scope 的删除事务提交前/后 | 事务提交前两者仍在，提交后两者都已删除 | 未提交则恢复清理；已提交则无需另行删除 Scope |
| 事务返回 panic/连接结果不确定 | 可能提交，也可能回滚 | 清理/隔离连接，查原 write_id；不以错误推断 absence |
| 本机重启，PID 被复用 | 旧完整实例身份 | 不向新进程下发；旧责任继续可清理 |

这些窗口要求 mock 远端与被杀 daemon 分离，且 mock 目标账本在子进程退出后仍存在。
只关闭/reopen Repository 的测试无法覆盖“远端已生效、本地尚未确认”的窗口。

## 9. 实施分期与验收

P1～P4 的实现已接入。下表保留设计验收项目；各项实际覆盖、运行环境和限制见第 9.1 节，
不能把组件/mock 证据当作物理断电、RPM/systemd 或真实 AgentSight 验收。
先冻结端口/fixtures，再实现后端，最后接 daemon 和实际杀进程测试。

| 阶段 | 改动 | 完成条件 |
|---|---|---|
| P1：契约与 CAS | Snapshot 加 status_version；PAP intent receipt；PCP claim/观察结果分离；内存后端同步 | 所有 status 写路径携带版本；现有生命周期回归及 PSQL-01/02/03 通过 |
| P2：SQLite 后端 | 新 crate、严格连接/schema、三表 CRUD/事务、回执、keyset scan、pin | 两后端共用契约；严格打开、事务及关闭重开验证 |
| P3：daemon 恢复 | 数据库 lease、SQLite 装配、Scope seed/停止屏障恢复、存储暂停/恢复、Client endpoint 校验、shutdown | PSQL-09 至 12、17、18 通过；现有 PII/SkillSec/bootstrap 不回退 |
| P4：崩溃与交付 | 外部 mock、测试进程内 gates、SIGKILL 重启、升级/回滚说明 | 同步 gate 确认提交窗口；独立记录未验证的部署环境与硬件边界 |

| 验收 ID | 可执行要求 |
|---|---|
| PSQL-01 | snapshot 一致；每个真实 status/error 改动增版；no-op/Deployment-only 不增版；溢出明确失败 |
| PSQL-02 | barrier 控制 Apply/Delete 竞争：旧 Ready CAS 拒绝，但 PRESENT 被保留并最终清理 |
| PSQL-03 | admission 后 worker/显式 retry 造成 ABA：旧 enqueue failure 不污染新 Pending；panic 收尾也不能越权 |
| PSQL-04 | memory/SQLite 共用 CRUD、去重、全局 byte 排序、分页及预算 fixtures；删除 Policy 保留 head，Scope 快照不变 |
| PSQL-05 | 并发 Policy update/delete 与 Scope create 只出现允许的两个结果；第 33 个 Active Scope 未写入 |
| PSQL-06 | 事务多列原子性、invalid JSON/投影不一致/外键/版本不兼容 fail closed；deployment 数量/字节上限在 I/O 前拒绝；不自动删库 |
| PSQL-07 | close/reopen 保留 Binding spec/status/version/deployments 与 Policy head、Scope pin/屏障；真实写探针覆盖只读/满盘 |
| PSQL-08 | BUSY、FULL、IOERR、损坏、提交前/后错误；write_id 重放与内容碰撞；不确定连接不泄漏未提交事务 |
| PSQL-09 | 两个 socket 使用同 DB 时第二个 daemon 拒绝；路径别名/链接/权限不安全拒绝；租约覆盖超时 drain |
| PSQL-10 | 持久 Deleting Scope 跨多页恢复并删行无遗漏；最后 Binding 与符合条件的 Scope 原子删除，事务失败两者都保留；无 Binding 时停止屏障与 Scope 删除同事务；Active Scope 变空仍保留，seed 在首次部分扫描失败时不误退休 |
| PSQL-11 | PID pin 与初始 Binding 原子提交；原 Binding 消失后仍不跟随 PID reuse；boot/ns/start time 不符拒绝 Apply |
| PSQL-12 | Pending/Running 重建调度；Ready/终态失败不重试；自动预算重置；丢通知补扫；反复 delete 不重置预算 |
| PSQL-13 | 用已确认到达的 failpoint gate 在第 8 节各窗口 SIGKILL 子进程；同 DB 重启，责任不丢、最后目标与意图一致 |
| PSQL-14 | 外部 mock 在 Apply 生效后、结果保存前杀进程，删除仍能按原 TargetRef 清理；只接受可靠 ABSENT |
| PSQL-15 | 真实 UDS/CLI 创建模板和 Scope、更新模板、发现/清理、进程退出及重启；状态版本/cleanup 不泄漏到公共输出 |
| PSQL-16 | 新库初始化失败、未知新 schema、备份恢复、旧二进制回滚保护；fault 日志无策略/cleanup/原始异常内容 |
| PSQL-17 | Runtime 注入 BUSY/FULL/只读/IOERR，覆盖远端调用前与结果提交后；超过自动重试上限仍保留责任/逻辑 slot，纯存储重试不消耗远端预算、不占住 worker；修复后先核实原回执再恢复，已发送的远端 attempts 不清零；损坏/不兼容须人工处理 |
| PSQL-18 | 保存 endpoint A 的 Deployment 后重启，将同名 route 指向 B；所有远端操作在 I/O 前拒绝、不覆盖 cleanup、不写 ABSENT；恢复 A 并 retry 后可清理；同 endpoint 的规范等价形式及 token 轮换不误拒绝；无 endpoint 的旧 cleanup 拒绝 |

SIGKILL 用例必须用父子同步信号定位窗口，不能靠随意 sleep 猜测提交时点。
SQL 故障可在专用连接/写边界注入，至少结合实际 readonly/full/busy 场景验证映射。
CAS 并发测试应使用 barrier 而不是放宽 wall-clock 等待；只有明确执行的测试才能填写通过证据。
性能验收记录 FULL 下的写延迟、批量 Scope cleanup 事务时间及 WAL/文件占用，发现瓶颈再决定
分批或分表，不为测试更快把生产持久性降到 NORMAL。

实施各阶段运行 V2 workspace fmt、Clippy、测试；公共端口改动还运行 rustdoc。
CLI/daemon E2E 使用 source-built 或 RPM 产物并注明身份/环境，不能把 user namespace 测试
写成 systemd、真实宿主 root 部署或内核策略生效证据。

### 9.1 本地实现与验收证据

生产入口已从内存后端切换到 SQLite。以下为分次本地验证记录，未运行远端 CI。
持久化接线时的全 workspace 回归为 **998 passed、0 failed、3 ignored**；忽略项是既有 SkillSec 的宿主
root/CAP_CHOWN/mount 专用测试。随后新增批量事务、迁移入口及声明式 schema 约束测试，重跑 Policy SQLite crate：
**29 passed、0 failed**，包含 19 个 repository 契约、4 个初始化/迁移测试、3 个真实 SQLite 故障测试、
2 个 crash harness 测试和 1 个持久 endpoint 恢复测试。两组数量有重叠，不能相加。
迁移入口测试验证当前版本不重复初始化、未知版本和非空 version 0 拒绝，以及测试升级步骤
失败后 DDL/数据/版本一起回滚并可重试；它们不代表已提供历史 Policy 库的转换实现。
统一 schema 声明后，`asc-sqlite-kernel`、`asc-persistence-sqlite`、
`asc-policy-repository-sqlite` 共 **211 passed、0 failed**，其中包含上述 29 项。
共享生成器验证 `STRICT` 类型和表级约束，Policy 初始化验证停止屏障、完整写回执、实例唯一性及
Scope 外键限制；事件库既有建表与旧版本迁移 fixtures 均通过。

补充 [CLI→daemon→HTTP mock E2E](../../tests/v2/e2e/test_policy_delivery_e2e.py)：真实二进制、
SQLite、procfs、Adapter 和 Client 贯穿同一用例；Scope 建立后先更新模板，再启动匹配进程，
验证下发旧 revision、完整请求 fixture、Ready 数据行、远端删除及 Binding/Scope 回收。
与既有 Policy CLI 测试合跑 **3 passed、0 failed**，环境为 Python 3.11.6、source-built
debug 二进制及隔离的 user/mount/network namespace，使用私有 `/tmp`、`/var/log` 和 loopback。
默认 AgentSight 端口由 mock 占用，不新增配置入口；已有 token 保留，测试临时创建的 token
才由测试清理。预置 token 的独立复跑 **1 passed**，确认文件内容、inode、修改时间及权限均
保持不变。该证据不等同于真实 AgentSight/内核验收，也没有新增崩溃窗口覆盖。

| 覆盖 | 可执行证据及范围 |
|---|---|
| PSQL-01～05 | [共享契约](../../v2/crates/asc-policy-repository-sqlite/tests/contracts.rs) 在 memory/SQLite 两个后端验证 ABA、迟到入队失败、旧 Apply 观察合并、no-op 版本、去重/排序/分页、并发模板更新/删除与 Scope 准入、32 Scope 上限；PAP/PCP 的既有完整 fixtures 同步版本并回归 |
| PSQL-06～08 | 同一契约覆盖版本溢出事务回滚、部署数量/字节限制、坏 JSON/投影/外键/回执、未知 schema、关闭重开和 write_id 重放/碰撞；[存储故障测试](../../v2/crates/asc-policy-repository-sqlite/src/tests.rs) 使用 query_only、max_page_count 产生真实 READONLY/FULL，另用独立 SQL 写锁产生 BUSY；IOERR/提交回执丢失在写边界注入，没有定制 VFS 或硬件掉盘实验 |
| PSQL-09～11 | 私有权限、symlink/hardlink、独占 lease、lease 比 repository 活得更久；keyset 扫描中删行、停止屏障、最后 Binding/Scope 回收、Active 空 Scope、PID pin 关闭重开；[discovery seed](../../v2/crates/asc-daemon-core/src/scope_discovery.rs) 覆盖部分扫描不误退休和 PID reuse。外层 drain 持锁由 daemon 所有权结构保证，未新增卡死内核 I/O 的宿主演练 |
| PSQL-12/17 | [Runtime 测试](../../v2/crates/asc-policy-runtime/src/reconciliation/tests.rs) 注入 BUSY/FULL/READONLY/IOERR/OutcomeUnknown，分别覆盖远端前及实际结果已返回后的写入；连续失败超过自动预算，仍保留原 write_id/结果，释放 worker 给其他 Binding，恢复后不重复已完成调用。原有终态、补扫、panic 和预算测试均回归 |
| PSQL-13/14 | [crash harness](../../v2/crates/asc-policy-repository-sqlite/tests/crash_recovery.rs) 用父子同步 gate 确认 12 个窗口后 SIGKILL；外部 mock 维护独立 ledger，保留远端成功但结果未写入的副作用，重启后最终清空目标；Ready 不重复 Apply |
| PSQL-15 | [真实 daemon bootstrap](../../v2/apps/asc-daemon/tests/bootstrap.rs) 通过 UDS 创建/更新、SIGKILL 重启、确认旧 Scope 快照/新模板、删除后再次重启，并验证不同 socket 不能共用 DB；[procfs 组合](../../v2/apps/asc-daemon/tests/reconciliation.rs) 使用真实子进程、SQLite、PAP、Runtime、Adapter 和 scripted Client，覆盖实例退出清理及新实例；[CLI 流程](../../tests/v2/e2e/test_policy_cli_e2e.py) 使用 source-built 二进制，2 项通过。三种证据分别覆盖入口、执行组合和进程恢复，不冒充完整 CLI→真实 AgentSight 链路 |
| PSQL-16 | 未知 schema 保持原文件、坏数据拒绝打开、事务 panic 不泄漏未提交数据；关闭最后连接后复制主库并从备份恢复，核对 Deleting/UNKNOWN/回执。旧内存版本的安全回退依赖第 10 节运维步骤，不能靠新库阻止旧二进制忽略它 |
| PSQL-18 | [endpoint 恢复测试](../../v2/crates/asc-policy-repository-sqlite/tests/endpoint_recovery.rs) 保存 A 的责任、关闭重开、改配 B 后 DeleteFailed 且 A/B 均无 HTTP、cleanup 不变；恢复 A 并显式 retry，使用轮换 token 完成删除；[Client factory](../../v2/crates/asc-agentsight-client/tests/factory.rs) 另覆盖 create/update/delete 的调用前拒绝、等价 endpoint 和旧 cleanup 拒绝 |

crash gates 包括 Scope/Binding 提交、claim、UNKNOWN、远端 Apply 后结果写入前、Ready 提交、
删除意图、discovery 停止屏障、删除 UNKNOWN、远端删除后结果写入前、最终删除提交和恢复中再次崩溃。
这些是操作系统进程终止与本地事务恢复证据；外部 mock 只模拟目标协议，不是 AgentSight/内核。

本次环境为 Linux 本地 debug 构建。普通 workspace 测试在宿主普通用户下执行；真实 daemon
启动用独立 user/mount namespace 映射 root，并挂载私有 tmpfs `/tmp`，bootstrap **7 项通过**。
该测试环境不证明 RPM/systemd、宿主 root 权限部署或物理介质同步写保证。
fmt、Clippy（deny warnings）、rustdoc 和 `git diff --check` 通过。

主要命令（Rust 从 `v2/`，Python 从组件根执行）：

```bash
cargo test --workspace --offline --no-fail-fast -- --test-threads=1
cargo test --offline -p asc-policy-repository-sqlite -- --nocapture
cargo clippy --workspace --all-targets --offline -- -D warnings
cargo fmt --all -- --check
cargo doc --workspace --no-deps --offline
# In an isolated root environment, with this build's binaries on PATH:
cargo test --offline -p asc-daemon --test bootstrap -- --test-threads=1
agent-sec-cli/.venv/bin/python -m pytest tests/v2/e2e/test_policy_cli_e2e.py -q
```

`batch_discovery_and_cleanup_keep_all_children_atomic` 的一次本地样本：同一 Scope 的 200 个
实例，WAL/FULL 下批量准入事务 56.17 ms，批量删除意图事务 75.38 ms，逐 Binding 认领并回收
总计 781.52 ms；采样时主库 499,712 bytes、WAL 4,128,272 bytes、SHM 32,768 bytes。
这是 debug/小样本观测，没有吞吐或延迟 SLA，也不能外推目标主机 fsync 性能。

## 10. 兼容、升级与回滚

首版是新 SQLite schema，不自动导入已经消失的进程内状态，也不导入 V1 数据。
从当前内存 daemon 升级前，应先通过旧 daemon 删除 Scope 并确认目标清理完成；如果旧进程
已经崩溃，需按旧版本运维流程核查远端孤儿。新库不能凭空恢复没有持久记录的历史责任。

公开 Policy/Scope/Binding JSON 和命令保留，内部 status_version、pin、回执不暴露。Schema、Client cleanup
格式和 public wire 的版本是不同层次，不复用 Scope revision 来管理它们。

回滚前停止 admission、排空必要操作并制作一致数据库备份。只能运行明确支持该 schema 与
cleanup 编码的二进制。不能删除 policy-state.db、把 WAL 当缓存清理或切回内存后端后宣称
恢复完成；这些动作会丢失责任。若确需回到旧内存版本，先由支持新库的版本清理全部 Scope/Binding
并确认远端不存在，再由管理员执行迁移/回退。未知 schema 必须拒绝，不能悄悄降级。

离线备份先停止 daemon 并确认数据库已正常关闭、WAL 已 checkpoint，再复制主库到私有目录。
运行中备份须使用 SQLite backup API 或等价的一致快照方式，不能只复制主库；本次没有新增
备份 CLI。恢复保留目录 0700、文件 0600，由兼容版本执行严格 schema/记录校验后启动。

## 11. 设计审视结论

本设计适合当前单机 daemon：SQLite 事务保护本地意图与责任，完整 Scope 快照简化模板
生命周期，少量内部恢复字段足够重建 Discovery。新增状态版本是必要而有限的并发保护，
不会把用户接口变成可变 Scope 或手工 Binding CRUD。

审视后明确约束了以下容易漏掉的问题：

1. 单 reconcile 执行者不排除 PAP/异常收尾写 status；所有入口必须传播真实 expected_version。
2. stale completion 的 status CAS 失败不能回滚合法 Deployment 观察；最后 ABSENT 与删除可同事务。
3. PID pin 不能只存在 worker 或 Binding 中；Scope 还活着时必须保留，避免重启后换绑。
4. 恢复时边删 Scope 边 OFFSET 分页会漏项，必须用稳定 ID cursor。
5. socket singleton 不能保护同库不同 socket，需单独 DB lease，且覆盖退出 drain。
6. 事件 SQLite 的“损坏后重建”不能复用到权威状态；schema 不兼容与存储失败必须可见。
7. Scope 删除流程中，最后一条 Binding 与已停止 discovery 的 Scope 同事务删除；没有 Binding
   时在停止屏障事务中直接删除 Scope。Active Scope 不因实例暂时清空而消失。
8. 存储故障暂停 Policy 写入和新远端调用，恢复先确认原写入；存储等待不消耗远端预算或释放执行所有权。
9. route 名字相同不证明 endpoint 没变；cleanup 保存地址作比较，凭据轮换独立于持久责任。

保留的成本是：Scope/Binding 快照重复、FULL 写延迟、JSON 部署数组更新、Policy head 累积，
以及重启重置 retry budget。它们均有明确用途和验收边界。多 daemon fencing、Ready 远端重审、
长期审计历史与真正断电演练留给独立工作包，不通过本设计冒充已经具备。

# V2 数据持久化层迁移设计

本文记录把 v1 `agent-sec-cli` 的两条事件流（`security_events` 与 `observability`）
从 Python + SQLAlchemy 迁移到 v2 Rust 的设计决策、schema 契约、迁移能力契约与等价性
验收口径。

| 属性 | 值 |
| --- | --- |
| 文档性质 | 已实施的迁移设计与契约记录；不代表该层已接入 daemon |
| 核对日期 | 2026-09-11 |
| 范围 | v1 `security_events/` 与 `observability/` 两个包中与库、JSONL、展示相关的模块 |
| 非范围 | daemon 接线、`asc-state-migrator`、CLI/TUI 改造、correlation/review/session_report |
| 路径约定 | 下文模块落点相对于组件根目录 `src/agent-sec-core/` |
| 验收类型 | MIGRATION_EQUIVALENCE（见[《Rust 迁移总计划》](AGENT_SEC_RUST_MIGRATION_zh.md) §5.1） |

> 后续接线说明：本文描述 2026-09-11 的存储层迁移范围。当前 daemon 已使用显式路径的
> security-event 双写，并接入共用 scan lifecycle 与独立 telemetry；详见
> [共享执行生命周期](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md#54-已实现的共享生命周期)。下文“不接 daemon”只描述原工作包边界，
> 不代表当前 checkout 无生产消费者。observability ingestion/query 仍未接入 daemon。

## 1. 范围与非范围

### 1.1 迁入范围

v1 侧共约 3900 行 Python，落到 7 个新 crate：

| crate | 路径 | 职责 | v1 来源 |
| --- | --- | --- | --- |
| `asc-security-events` | `v2/crates/asc-security-events/` | 事件值类型、时间戳规范化、路径三级降级、修订登记表 | `security_events/{schema,config}.py` |
| `asc-observability` | `v2/crates/asc-observability/` | 六个 hook 的判别联合、metrics 白名单、相关 ID 截断 | `observability/{schema,models}.py` |
| `asc-event-log` | `v2/crates/asc-event-log/` | JSONL 追加写、flock、轮转与备份保留 | `security_events/writer.py` |
| `asc-sqlite-kernel` | `v2/crates/asc-sqlite-kernel/` | 与领域无关的 SQLite 内核：连接、schema 收敛、写入阶梯、维护闸门 | `security_events/orm_store.py` + 两个 `sqlite_writer.py` 的公共部分 |
| `asc-persistence-sqlite` | `v2/crates/asc-persistence-sqlite/` | 两条流的领域绑定：表契约、仓储、故障策略、迁移器 | `*/models.py`、`*/repositories.py`、`*/sqlite_{writer,reader}.py` |
| `asc-security-summary` | `v2/crates/asc-security-summary/` | 摘要文本渲染 | `security_events/summary_formatter.py` |
| `asc-event-sink` | `v2/crates/asc-event-sink/` | 双写装配、进程级单例、关停 | `security_events/__init__.py`、`observability/__init__.py` |

### 1.2 明确不做

- **不接 daemon。** 本层不被任何业务路径调用；接线点见 §11。
- **不做 `asc-state-migrator`。** 那是产品入口层的工作包，本层只提供库内 schema 收敛。
- **不迁调用方。** `correlation.py`、`review.py`、`cli.py`、`session_report.py` 以及各
  Agent Hook 都是本层的消费者，其测试不在搬迁范围（见
  [`v2/crates/DATA_TEST_MIGRATION.md`](../../v2/crates/DATA_TEST_MIGRATION.md) §Out of scope）。

## 2. OPEN 项定案

[《V2 扫描能力开发指引》](V2_SCAN_CAPABILITY_DEVELOPMENT_GUIDE_zh.md) §8 曾把三个决策
留给事件工作包。本节逐条定案。

### 2.1 是否兼容 V1 JSONL + SQLite 双写

**定案：兼容，且取向是「互操作 + 语义一致」，而不是逐字节相同。**

验收目标明确定为：**无论哪一侧写出的 JSONL，两侧代码都能正确解析，且解析后的记录
语义一致**。关注的是升级窗口内的兼容性，不是输出字节序列的复刻。

这不是「格式相似」而是有可执行断言的：`asc-event-log` 有四个互操作用例（含用 v1
真实产出的行做输入），反向也做了 v2 产出的行喂回 v1 `to_dict()` / `to_record()` 的
回环比对；差分矩阵有 4 项跨版本读写用例，第 18 项是逐记录语义比对。同一个 `.db`
文件两侧都能读写。

字节层面有**两处**已知差异，均不影响解析：

1. **分隔符空格**。v1 `json.dumps(record, ensure_ascii=False)` 用默认分隔符
   `", "` / `": "`（带空格）；v2 `serde_json::to_string` 是紧凑形式。
2. **嵌套 object 的 key 顺序**。v1 保留 Python `dict` 插入序；v2 的
   `serde_json::Map` 默认是 `BTreeMap`，按字典序。只影响 `details` / `metrics` /
   `metadata` 这些自由格式的嵌套值——**顶层 key 序已对齐**，serde 序列化 struct
   按字段声明顺序而非排序，而 v2 的字段序照搬 v1 `to_dict()` / `to_record()`。

为何不拉到逐字节：两处必须同时做才有意义，而其中一项代价过高。键序需要在
workspace 级打开 `serde_json/preserve_order`，而 Cargo 的 feature 是并集语义：它会
一次性改变**全部 19 个引用 `serde_json` 的 crate** 的 JSON 键序行为，其中包括
`asc-daemon-protocol` / `asc-daemon-client` / `asc-daemon-handler`（UDS 上的 JSON-RPC
线格式）与整个 policy 子树。而收益仅仅是「能用 `diff` 直接比两个文件」——这个
需求只出现在差分脚本里，而它已改成更强的逐记录语义比对。

若将来确实出现需要逐字节的场景，更合理的路径是局部方案：在 `asc-event-log` 内部用
`IndexMap` 承载嵌套值 + 写一个输出 `", "` / `": "` 的 `serde_json::ser::Formatter`，
不动 workspace feature。

### 2.2 原文保留期限

**定案：沿用 v1，不改。** `security_events` 30 天，`observability` 7 天，常量分别是
`DEFAULT_MAX_AGE_DAYS` 与 `DEFAULT_OBSERVABILITY_RETENTION_DAYS`。

两条流的裁剪都发生在 `close()` 而不是 `write()`：CLI 是短命进程，每次调用都是独立
进程，放在写路径里的计数式裁剪永远攒不够次数。裁剪走维护闸门（§5.6），跨进程限频。

### 2.3 sink deadline

**定案：本次不引入。**

理由是引入它会改变可观测的行为，而本工作包的验收红线是行为等价。v1 的写路径没有
deadline，只有 `busy_timeout=200ms` 这一层隐式上界；加一个显式 deadline 会让「原本
只是慢、最终写成功」的调用变成「被主动放弃」，差分矩阵会立刻测出差异。

风险与后续：真正需要 deadline 的场景是 daemon 常驻进程下的 sink 背压，那时调用方是
async runtime，写路径必须走 `spawn_blocking`（§11.1）。届时 deadline 应加在
**调用侧的 `spawn_blocking` 包装上**，而不是塞进本层的同步写路径——这样本层对 v1 的
等价性不受影响。

## 3. schema 版本策略

### 3.1 本次不 bump

`security_events` 保持 `user_version=3`，`observability` 保持 `user_version=1`。

理由很直接：**本次没有新增任何列**。两张表的列集、列序、列定义文本、索引名与索引列序
都与 v1 逐字相同（§4）。既然 schema 没变，bump 只会让 v1 进程把 v2 建的库判成
「版本超前」、跳过自动迁移并进入未承诺兼容的读写路径，凭空制造不兼容风险。

### 3.2 与「按 schema version 区分 V1/V2 event」的关系

《Rust 迁移总计划》§7.3 要求「V1 legacy event 与 V2 event 通过 schema version 区分并
可在迁移窗口混合查询」。这条与本次不 bump 并不冲突：

- 那里说的是**事件语义版本**（哪个实现写的、字段语义是否变了），而本文说的
  `user_version` 是**库结构版本**；
- 本次迁移不改任何字段语义，v2 写出的行与 v1 写出的行在同一张表里**没有区分的必要**，
  混合查询本身就是差分矩阵第 25 项验证的场景；
- 真正需要区分时（例如 V2 新增字段），走 §3.3 的新增修订流程，那时 `user_version`
  才会前进到 4，届时才需要「区分」这件事。

### 3.3 将来新增修订的正确改法

`AGENT_SEC_RUST_MIGRATION_zh.md` §8 要求任何 schema 变更先明确「旧版本能否读新库」与
回滚路径。新增一个修订必须同时做四件事，缺一即错：

1. 在 `SECURITY_EVENTS_SQLITE_SCHEMA_REVISIONS` 登记表里加一行 `(4, "描述")`。
   `SECURITY_EVENTS_SQLITE_SCHEMA_VERSION` 由登记表**派生**（`max_revision()`），
   不允许手写常量——手写就会出现「登记表说 4、常量说 3」的静默不一致。
2. 把新列加入 `COLUMNS`，并**同时**加入 `EXTRA_COLUMNS`。前者管新库的
   `CREATE TABLE`，后者管旧库的 `ALTER TABLE ADD COLUMN`；只改前者，旧库永远补不上
   这一列。
3. 若新列需要回填，实现或扩展 `SchemaMigrator`，并在回调里加**区间守卫**——现有
   `SecurityEventsMigrator` 只在 `from < 3 <= to` 区间内动作，跨过该区间是 no-op。
4. 明确旧版本读新库的行为：v1 与 v2 的只读路径都是「版本超前则告警并返回空」，
   `SELECT` 显式列名因此不会因多出的列而失败；写路径同样只告警并跳过自动迁移，若既有
   表结构仍兼容则允许继续 `INSERT`，不兼容则由实际语句失败暴露。回滚不能假定旧进程
   一定写不进新库，发布说明必须明确该未承诺兼容路径及验证结论。

## 4. schema 契约

两张表的完整契约。列序、索引名、索引列序都是**线上契约**：v1 进程和 v2 进程必须把同一
个库收敛成同一形状。

### 4.1 `security_events`

| 列 | 定义 |
| --- | --- |
| `event_id` | `TEXT NOT NULL PRIMARY KEY` |
| `event_type` | `TEXT NOT NULL` |
| `category` | `TEXT NOT NULL` |
| `result` | `TEXT NOT NULL DEFAULT 'succeeded'` |
| `timestamp` | `TEXT NOT NULL` |
| `timestamp_epoch` | `FLOAT NOT NULL` |
| `trace_id` | `TEXT NOT NULL DEFAULT ''` |
| `pid` | `INTEGER NOT NULL` |
| `uid` | `INTEGER NOT NULL` |
| `session_id` | `TEXT` |
| `run_id` | `TEXT` |
| `call_id` | `TEXT` |
| `tool_call_id` | `TEXT` |
| `verdict` | `TEXT` |
| `details` | `TEXT NOT NULL` |

索引：`idx_event_type`、`idx_category_epoch(category, timestamp_epoch)`、
`idx_trace_id`、`idx_timestamp_epoch`、`idx_verdict_timestamp_epoch`、
`idx_session_id_timestamp_epoch`、`idx_run_id_timestamp_epoch`、
`idx_session_run_timestamp_epoch(session_id, run_id, timestamp_epoch)`。

`extra_columns`（旧库靠 `ALTER TABLE` 补齐的）：`run_id`、`call_id`、`tool_call_id`、
`verdict`。

### 4.2 `observability_events`

| 列 | 定义 |
| --- | --- |
| `id` | `INTEGER NOT NULL PRIMARY KEY` |
| `hook` | `TEXT NOT NULL` |
| `observed_at` | `TEXT NOT NULL` |
| `observed_at_epoch` | `FLOAT NOT NULL` |
| `session_id` | `TEXT NOT NULL` |
| `run_id` | `TEXT NOT NULL` |
| `metrics_json` | `TEXT NOT NULL` |
| `metadata_json` | `TEXT NOT NULL` |
| `call_id` | `TEXT` |
| `tool_call_id` | `TEXT` |

索引：`idx_observability_observed_at_epoch`、
`idx_observability_hook_observed_at_epoch`、
`idx_observability_session_observed_at_epoch`、
`idx_observability_session_run_observed_at_epoch`。无 `extra_columns`——该流只有一个
修订。

### 4.3 三处容易被「修好」的写法

这三处都不是笔误，改掉会破坏等价性：

1. **`FLOAT` 而非 `REAL`。** 两者在 SQLite 里是同一种 affinity，但 SQLAlchemy 的
   `Float` 渲染成 `FLOAT`，而差分口径逐字节比 `PRAGMA table_info` 的 `type` 列。
2. **主键显式 `NOT NULL`。** SQLAlchemy 给每个主键列自动加 `NOT NULL`。缺了它，
   SQLite 会接受 `TEXT PRIMARY KEY` 为 `NULL`——v2 就比 v1 宽。
3. **`id` 不带 `AUTOINCREMENT`。** v1 声明了 `autoincrement=True`，但 SQLAlchemy 只在
   表上设了 `sqlite_autoincrement` 时才发出该关键字，v1 没有设。发出它会多出一张
   `sqlite_sequence` 表，并改变裁剪后的 rowid 复用行为。

### 4.4 PRAGMA 与差分口径

写连接的 PRAGMA 顺序取自 v1 `create_sqlite_engine`：`busy_timeout=200`、
`foreign_keys=ON`、`synchronous=NORMAL`、`wal_autocheckpoint=100`。只读连接额外
`query_only=ON`，且走 `file:...?mode=ro` URI。

**`auto_vacuum` 实测为 `NONE`（0），而不是代码里写的 `INCREMENTAL`。** SQLite 只在库
仍是「新库」时接受 `none → incremental`，而 `journal_mode=WAL` 会写头并结束这个窗口。
v1 `ensure_schema` 恰好是这个顺序，所以 **v1 那两行 PRAGMA 自诞生起就是空操作**。v2
逐字复刻因此结果相同。`asc-sqlite-kernel` 里有一个专门的钉子测试
`auto_vacuum_stays_none_exactly_as_in_v1`，作用是**阻止好意的顺序修复**：调整顺序会
「修好」意图但破坏等价性。

**而且即使不计等价性，单独调顺序也是净亏。** `INCREMENTAL` 本身不回收任何空间，
只额外维护 ptrmap 页让后续的 `PRAGMA incremental_vacuum(N)` 能按需回收；而两侧都
**没有任何 `incremental_vacuum` 调用**。所以只改顺序的结果是「多付 ptrmap 开销、一分
回收收益也没有」。真要拿收益得是三件套：顺序 + 裁剪后显式回收（或改 `FULL`）+
存量库迁移策略（`auto_vacuum` 建库时定型，存量库不走整库 `VACUUM` 改不了）。实际
影响仅「删除后磁盘不回缩」，且空页进 freelist 仍被后续写入复用，因此是「水位停在
历史峰值」而非无界增长。这项优化的正确时机是 v1 退役、v2 成为唯一写者之后。

差分比对的投影集合：`PRAGMA table_info` / `index_list` / `index_info` /
`user_version` / `auto_vacuum` / `journal_mode`，加上 `sqlite_master` 全集。两条流的
投影在两侧**逐字节一致**。

## 5. 迁移能力契约

v1 自带的库内迁移能力必须完整保留，这是本次的验收红线之一。

### 5.1 修订登记表与版本派生

见 §3.3 第 1 条。当前登记表：

| 修订 | 内容 |
| --- | --- |
| 1 | initial `security_events` table |
| 2 | add `run_id` / `call_id` / `tool_call_id` correlation columns |
| 3 | add `verdict` column and backfill from event details |

### 5.2 两层迁移机制

v1 用**两种不同机制**完成 1→3，v2 都保留：

- **1→2 靠通用收敛。** 缺列由 `extra_columns` 的 `ALTER TABLE ADD COLUMN` 补上，
  不需要任何领域代码。
- **2→3 靠注入回调。** `verdict` 列除了要加，还要从 `details` JSON 里回填，这需要理解
  领域数据，因此走 `SchemaMigrator` trait 注入。内核不知道 `verdict` 是什么。

回填要处理两种 `details` 形态（顶层 `verdict` 字符串、嵌套 `result.verdict`），并且对
畸形 JSON、非对象 `result`、缺失 key 都必须**跳过而不中断整批扫描**——否则一条脏数据会
让整个迁移卡住。这条有专门用例。

### 5.3 两阶段与版本写回

`ensure_schema` 分两阶段，这个划分本身是 v1 的：

- **阶段一（无事务）**：`journal_mode=WAL` → 读 `user_version` → 版本超前则告警退出 →
  `auto_vacuum` → 版本落后则跑 migrator。
- **阶段二（事务内）**：重读 `user_version` 并再次做超前保护 → `auto_vacuum` →
  `CREATE TABLE IF NOT EXISTS` → 补缺列 → 建缺失索引 → **仅当原版本落后时**写回
  `user_version`。

「仅当落后时写回」是降级保护的另一半：对一个已经是当前版本的库，收敛过程不写
`user_version`，因此一个更高版本的库不会被悄悄改成低版本。`tests/v1_fixtures.rs` 里
用**字节相等**断言了这一点。

### 5.4 快路径与 `force` 修复

`ensure_schema_if_needed` 先读 `user_version`：等于当前版本就直接返回，不做任何 DDL。
这是热路径优化，也是 v1 的行为。

`force=true` 用于**索引丢失后的修复**：版本没变但结构缺了东西时，跳过快路径强制走一遍
完整收敛。修复请求由内核在检测到 schema drift 时置位，并**保留到被真正使用为止**——
并发打开不会把它冲掉。

### 5.5 只读不迁移与标识符白名单

只读 store 永不建库、永不迁移。遇到结构未就绪的库，告警并让查询返回默认值（空列表 /
0），不抛错——这是 v1 的读路径语义。

所有会进入 DDL 的标识符走白名单校验，规则与 v1 一致（含「拒绝含数字的列名」这条略显
古怪但必须复刻的规则）。

### 5.6 维护闸门

裁剪与 WAL checkpoint 走 `run_sqlite_maintenance_if_due`：以 `<db>.maintenance` 标记
文件记录上次时间、`<db>.maintenance.lock` 做跨进程互斥，默认间隔一天。标记损坏、标记
时间在未来、间隔非正数都有明确行为（分别是「视为不存在」「视为过期」「总是执行」）。

**闸门只在维护回调返回成功时前进。** 但 writer 的 `run_maintenance` 有意吞掉裁剪错误，
所以「裁剪失败」在闸门看来仍是成功，标记照常写入——这与 v1 相同（v1 的 prune 也是内部
catch）。差别只在失败后是否 dispose 连接，见 §10.3。

## 6. 去重与抽象

### 6.1 v1 的重复

v1 把同一棵八步写入决策树写了两遍：`security_events/sqlite_writer.py` 是
fire-and-forget 版，`observability/sqlite_writer.py` 是抛错版。两份代码的**结构完全
相同**，只有终态动作不同。

v2 让这棵树只存在一次（`asc-sqlite-kernel::sink`），差异用注入的 `FaultPolicy` 表达。
内核决定**发生了什么**（`WriteFault`），策略决定**怎么响应**（是否 dispose、是否抛给
调用方、打什么日志）。

### 6.2 四个必须保持可见的不对称

抽象最大的风险是把不该抹平的差异抹平了。这四条写在 `fault.rs` 的模块文档里，任何后续
重构都必须重新确认：

1. **`Malformed` 的落点。** `security_events` 在 `repository.insert` 内部吞掉
   `ValueError`/`TypeError`，所以畸形记录看起来像「跳过的写入」；observability 调
   `insert_or_raise`，让它穿出去。
2. **corruption 重试失败后的 dispose。** 两条流在重试**因 busy 失败**时都不 dispose；
   在重试**因 malformed 失败**时不同——`security_events` 仍然 dispose，observability 不。
3. **写锁。** 只有 `security_events` 序列化写入（v1 持一把 `threading.Lock`）。
4. **`prune` 的时钟。** v1 只有 observability 接受注入的 `now`；v2 的 trait 两侧都接受。

`WriteFault` 有一个 `Database` 变体专门表达 v1「其它 `DatabaseError`」那条分支——它既不
dispose 也不请求 repair，用 `Io` 表达会多出一次 dispose。

## 7. crate 划分与官方清单的差异

《Rust 迁移总计划》§5.2 的 data 层清单是 `asc-security-events`、`asc-observability`、
`asc-session`、`asc-state`、`asc-persistence-sqlite`。本次实际落地多出 4 个 crate：

| 新增 crate | 为什么不塞进清单里的既有 crate |
| --- | --- |
| `asc-sqlite-kernel` | 内核对领域**零依赖**（有编译期断言：manifest 无领域依赖、源码无领域名词）。塞进 `asc-persistence-sqlite` 会让两条流的领域类型和通用内核互相可见，八步决策树很快会长出 `if stream == ...`。 |
| `asc-event-log` | JSONL 与 SQLite 是两条**独立**的落盘路径，v1 里也是两个文件。放进 persistence 会让「只写 JSONL 不写库」的场景被迫依赖 SQLite。 |
| `asc-security-summary` | 纯文本渲染，无 IO。与库耦合会让摘要格式的改动牵动持久化层。 |
| `asc-event-sink` | 双写装配 + 进程级单例。这是唯一持有全局状态的 crate，隔离出来才能让其余六个 crate 的测试完全无全局状态（§13）。 |

**偏离说明**：`V2_SCAN_CAPABILITY_DEVELOPMENT_GUIDE_zh.md` §8 曾把落点写成
`asc-persistence-sqlite/src/events.rs` 与 `migrations.rs`。实际布局是按流分目录
（`security_events/` 与 `observability/` 各自的 `table/repository/policy/migration/
writer/reader`），因为两条流的表契约、故障策略和迁移机制都不同，放在同一个
`events.rs` 里会是一个上千行的多流文件。该文档已同步修订。

## 8. 不保留的 v1 符号

以下 v1 符号在 v2 没有对应物，因为它们是 SQLAlchemy 固有的：

| v1 符号 | v2 替代 |
| --- | --- |
| `register_orm_models()` / 全局模型注册表 | 无。每个 store 显式接收 `&[TableSpec]` |
| `Base` / `declarative_base()` | 无。表契约是 `const` 数据 |
| `session_factory()` / `Session` / `begin()` | `SqliteStore::with_connection()` |
| `engine.dispose()` | `SqliteStore::close()`（语义不同，见 §10.3） |
| 读路径的 `except SQLAlchemyError: dispose()` | `ReadOnlySource::query_or_default()` 降级 |

完整台账在 `asc-event-sink/tests/api_parity.rs`：105 行逐符号对照 + 6 个编译期引用 +
3 条元测试（无未迁移条目、每个改名都有理由、v2 新增项恰好是约定的 4 个）。

## 9. 等价性验收

本工作包的 acceptance type 是 **MIGRATION_EQUIVALENCE**，四道证据：

| 证据 | 内容 |
| --- | --- |
| 差分矩阵 | 迁移期本地工具，27 项。两侧各有一个对偶探针，同一套子命令、同一输出格式，diff stdout。它不随仓库交付；结论已固化为 crate 内用例、API 台账与冻结 fixture |
| API 台账 | `api_parity.rs`，见 §8 |
| 冻结 fixture | `scripts/gen-v1-db-fixtures.sh` 生成 5 个历史版本库 + 9 份预期投影 JSON；`tests/v1_fixtures.rs` 只用 `cargo test` 就能验升级路径。**oracle 由 v1 生成**——从 v2 生成会让测试同义反复 |
| 测试搬迁账本 | [`TEST_MIGRATION.md`](../../v2/crates/DATA_TEST_MIGRATION.md)：v1 16 个文件 322 个用例逐条映射到 v2 的 364 个用例，废弃项恰好 2 个且都是 SQLAlchemy 实现细节 |

差分矩阵与账本分工不同：矩阵验「两个版本对同一个库行为一致」，账本验「v2 自身的分支
覆盖不低于 v1」。

## 10. 已知差异

### 10.1 无 `atexit`

v1 靠 `atexit` 注册 writer 的 `close`，进程退出时自动跑维护。Rust 没有等价物，因此
`asc-event-sink` 暴露显式的 `shutdown_sinks()`。**composition root 必须调它**，否则
裁剪与 checkpoint 永远不会发生（§11.2）。

### 10.2 `format_summary_at` 的存在理由

v1 的摘要格式化直接读真实时钟，导致「最后一个事件距今多久」这类输出不可测。v2 拆成
`format_summary()`（读时钟）与 `format_summary_at(now)`（注入时钟），后者是测试与差分
探针的入口。这是新增的公开符号之一，已在台账里登记。

### 10.3 无连接池

v1 持有 SQLAlchemy engine 及其连接池，v2 持有单个缓存的 `rusqlite::Connection`。可见
差异出现在**裁剪失败**时：v1 `dispose()` 整个池（池里可能残留坏事务状态的连接），v2
保留连接（一条语句失败后 rusqlite 连接依然可用）。两条流各有一个用例钉住 v2 的行为。
若将来引入连接池，这条必须重新评估。

### 10.4 展示层的 Python 动态类型残留

`details` 里的值原样打印时，v1 走 Python `str()`。v2 已复刻标量拼写
（`None` / `True` / `False`）；**容器类型的 Python `repr`**（单引号、`True` 而非
`true`）是**已接受的差异，不会去复刻**：v1 那个形式对使用者没有价值（甚至不是
合法 JSON），而复刻它需要手写一个 Python `repr` 模拟器（单引号、`: ` 分隔、嵌套
递归、不同的字符串转义规则）。交叉复审确认该差异在 v1 实际能产生的 payload 下
不可达——展示层读的那几个字段，其生产者都不写容器值。

同一类型的第三项（`{pct:.1f}` 的并列舍入）**不是差异**，已穷举验证：两侧对精确
二进制值做**半值取偶**（`6.25→6.2`、`18.75→18.8`），在 `total` ≤ 1000 且
`effective` ≤ `2 × total` 的全部 1,002,000 组上逐行全等。`sections.rs` 里的
`compliance_percentages_round_half_to_even_like_python` 钉住了这个规则，防止后续把
渲染改成自写的舍入 helper 而在 `x.x5` 上静默分歧。

### 10.5 v1 侧的一个真实缺陷

同一批事件里同时出现 `verdict: null` 与字符串 verdict 时，v1 `format_summary` 用
`None` 做 dict key 后 `sorted()` 跨类型比较，直接抛 `TypeError`——该命令完全不可用。
v2 正常渲染。这条**不是**按等价性复刻的，而是写成差分矩阵第 27 项**显式断言这个不
对称**：v1 必须失败且 stderr 含 `TypeError`，v2 必须成功。这样 v2 哪天退化成同样崩溃
会立刻暴露。是否向上游修 v1 待定。

## 11. 待接 daemon 时的接线点

### 11.1 `Send` 与 `spawn_blocking`

本层的写路径是**同步阻塞**的（SQLite 就是同步的）。已有编译期断言保证长生命周期类型
是 `Send`，因此可以放进 `spawn_blocking`。daemon 侧**不得**在 async 上下文里直接调写
路径——`busy_timeout=200ms` 加上 flock 等待足以卡住 executor 线程。

### 11.2 composition root 的责任

- 启动时解析 JSONL / SQLite 的绝对路径，构造
  `ConfiguredSecurityEventSinks` 并注入 `EventSinkAdapter`；不得让 daemon 走进程级
  `writer()` / `sqlite_writer()` 的默认环境路径单例（见 §13.1）；
- 启动时分别 warm 两条路径：SQLite 失败拒绝启动，JSONL 失败只告警并保持该副本的
  best-effort 重试；运行时事件写入仍按 v1 双写的独立 fail-open 语义处理；
- `run_with_shutdown_timeout` 返回后才调 `ConfiguredSecurityEventSinks::close()`：先让 Tokio
  排空 blocking dispatch，再跑保留期维护与 WAL checkpoint；
- 决定 `DropSink` 的落点。v1 把丢弃诊断写进 `cli.jsonl`，v2 默认写 stderr，注入式。
  这一条是**尚未定案的差异**，需要 daemon 侧确定诊断落点。

### 11.3 不属于本层的

`QueryScope`、owner principal 隔离、normal/auditor/admin 授权都在《Rust 迁移总计划》
§7.2 里，属于 daemon query 用例的工作包。本层只提供仓储与只读源，**不做任何授权判断**。

### 11.4 接线工作包的已定案取向

以下五条在接线工作包启动前定案，记录在此以免实现时重新讨论。前四条约束实现，第五条
是承认的缺口。

**1. 事件投影归 Finalizer，不归 handler。** 走
[《V2 扫描能力开发指南》](V2_SCAN_CAPABILITY_DEVELOPMENT_GUIDE_zh.md) §4.2/§4.3/§4.6 的
正序：先建 Action 合同与 Action Runtime，事件由唯一 Finalizer 产出，Capability 只提供
audit projection，**不直接调用本层的 writer**。抽象 port 必须按 Action 泛化，使
prompt-scan 与 pii-checker 能复用同一套 Finalizer 与 audit projector 接口，而不是长成
code-scan 专用形状。在 `CodeScanHandler` 里直连 sink 虽然能跑通，但会让 §4.3 的唯一终
态、超时不失主、显式 audit projector、blocking 容量边界全部失效，因此不采用。

**2. `pid` / `uid` 记 UDS peer credentials，不记 daemon 自己。** 这一条**必须在实现里
显式传递，不能依赖默认值**：`SecurityEvent::new()` 填的是 `std::process::id()` 与
`getuid()`，在 daemon 里就是 daemon 自身，语义是错的。v1 记的是调用方——实测一次 v1
`code_scan` 落盘记录为 `pid: 45806, uid: 502`，即那个 CLI 进程与发起用户。dispatcher 已
经在 `PeerCredentials::new(uid, gid, pid)` 处拿到内核认证的对端身份，需要把它一路传到
Finalizer 并覆盖这两个字段。

边界同样要写清：**peer credentials 在这里只用于事件 attribution**。授权仍然只走指南
§4.4 的服务端策略与 kernel peer credentials 判定，不因为事件里出现了 uid 就把它当作
授权依据；指南 §4.1 也要求不把客户端自报字段升级为可信 Principal。

**3. 相关性字段本轮留空，且不代填。** `trace_context` 不在本轮范围内，因此 v2 daemon
产出的事件里 `trace_id` 是空串（该字段是 `String` 而非 `Option`，doc 写的就是 empty
until then），`session_id` / `run_id` / `call_id` / `tool_call_id` 为 `None`。

与 v1 有一处可观测差异：v1 `RequestContext.__post_init__` 对 `trace_id` 有 UUID 兜底，
**v1 事件的 `trace_id` 恒非空**。这是按计划欠着的缺口，不是缺陷。特别注意
**不能拿 dispatcher 的 `request_id` 顶替 `trace_id`**——
[`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md) §3.2 明确 daemon 不为缺失的
`trace_id` 生成 trace ID，且 request ID 与 trace ID **不能混用**。代价是协议里要求必填
`session_id` 的 `sec.sessions.*` 查询在这批事件上查不到结果。

**4. 入库内容与 v1 逐字段一致，含代码原文。** `details` 保持 v1 形状
`{"request": <调用方传入的参数原样>, "result": <ScanResult>}`，其中
`details.request.code` 是**被扫描代码的全量原文**。这是 v1 既有事实而非新增暴露：v1
`backends/base.py::build_event_details` 直接 `copy.deepcopy(kwargs)`，且
[`SECURITY_ACTIONS_REFERENCE_zh.md`](SECURITY_ACTIONS_REFERENCE_zh.md) 能力总览里
`code_scan` 的「专用审计脱敏」列是**否**（`pii_scan` 是**是**，它才有删原文逻辑）。

改 `details` 形状会让 v1↔v2 互读产生真实差异，与本层的验收红线冲突，因此本轮不做最小化。
[`RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md`](RUST_SECURITY_CORE_EXECUTION_ARCHITECTURE_zh.md)
§14 第 4 条（code/prompt/command/path 原文的保留期限与 V2 最小化方案）仍是 `[OPEN]`；
将来落地时唯一的收口点是本 Capability 的 audit projector（指南 §4.3 第 5 条要求每个
Capability 都有显式 projector），改动面是一个文件加一个用例。

**5. 已知问题：事件从 per-user 库搬进共享 root 库。** v1 的数据目录分级是
`AGENT_SEC_DATA_DIR` → `/var/log/agent-sec`（tier 1）→ `$HOME/.agent-sec-core`
（tier 2）→ `/tmp/agent-sec-<uid>`（tier 3）。普通用户跑 v1 CLI 写不进 tier 1，落自己
home，**谁扫的代码进谁的库**；daemon 通常以 root 运行、落 tier 1，于是所有用户的事件
（含 `details.request.code` 原文）集中进同一个 root 拥有的库。

记录形态、字段与内容都没变，变的是文件归属与可见范围。本轮**不考虑多用户场景**，该问
题留待后续 PR 解决；届时的方向与 §11.3 的 owner principal 隔离、`QueryScope` 是同一件
事，不应在本层单独发明一套隔离机制。

**6. daemon 只使用系统级事件库，并在启动时验证双写目标。** `asc-daemon` 只接受
systemd/DaemonSet 显式设置的 `AGENT_SEC_DATA_DIR`；未设置时固定使用
`/var/log/agent-sec`，绝不回退到 `HOME` 或 `/tmp`。目录必须归 daemon 的有效用户所有且
为 `0700`；主 JSONL 与 SQLite 文件为 `0600`，SQLite 的 WAL/SHM sidecar 也受该私有目录
保护，因此生产 root daemon 的事件仅 root 可读写。

在绑定 UDS 前，daemon 必须实际创建并打开 JSONL、打开并初始化 SQLite。SQLite 不可用就
以非零状态退出，不使用 `NoopEventSink` 掩盖主审计库失效；JSONL 不可用只输出告警，仍绑定
UDS 并以 best-effort 方式继续尝试该副本。启动成功后，运行时任一侧的瞬时写入失败仍不阻断
另一侧或 capability 结果，保持 v1 双写的独立 fail-open 语义。

## 12. 测试工具定性

v1↔v2 差分探针只用于迁移期间验证，**不随仓库交付**，也不进入制品、RPM 清单或 CI；
已提交的验收证据是 crate 内的 API 台账、冻结 v1 fixture 和迁移测试账本。这样产品面不出现
直读 SQLite 的 CLI 或 TUI，仍符合
[`DAEMON_PROTOCOL_V1_zh.md`](DAEMON_PROTOCOL_V1_zh.md) DPV1-019。

## 13. 测试并行隔离契约

### 13.1 为什么是显式路径注入而不是环境变量

pytest 默认串行，所以 v1 用 `autouse` fixture 改 `AGENT_SEC_DATA_DIR` 是安全的。
`cargo test` 默认**同进程多线程**，改环境变量会互相污染。更硬的约束是 workspace 的
`unsafe_code = "forbid"`——`forbid` 不可被 `allow` 覆盖，因此测试**根本无法**调用
`env::set_var`。

结论是把「显式路径构造」变成设计约束：内核与两个领域 crate 的所有类型都必须支持
从构造函数接收路径，不能只提供读默认路径的无参构造。这条约束由测试反向钉住——如果
某个类型只有默认构造，它的测试就写不出来。

### 13.2 必须串行的用例

只有 `asc-event-sink` 有进程级全局状态（四个 `OnceLock` sink slot），它的 15 个有状态
用例统一取 `test_support::serial()`（进程内 `Mutex`）。**没有任何地方需要
`--test-threads=1`**。清单见 `TEST_MIGRATION.md` §Serial cases。

`asc-security-events::config` 是生产代码里唯一从环境解析路径的地方，其测试通过注入
`DataDirEnv` 与 `DaemonDataDirEnv` 驱动纯函数版本，因此连 v1 三级降级路径与 daemon 的
系统级 fail-close 路径也不需要串行。

### 13.3 `reset_sinks_for_test()` 为何存在

对应 v1 `conftest.py` 里重置三个模块级单例的行为。没有它，第一个用例初始化的 sink 会
带着自己的临时目录活到进程结束，后续用例全部写进已被删除的目录。它只在 `testing`
feature 下导出，产品构建里不存在。

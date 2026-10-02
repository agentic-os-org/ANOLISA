# ws-ckpt 与会话管理 IPC 协议

[English](../../en/cosh-ng/ipc-protocol.md)

## 概述

cosh-ng 通过 Unix Domain Socket 与 ws-ckpt 守护进程通信，实现工作空间快照管理。
通信使用 **bincode 序列化 + 4 字节小端长度前缀** 的帧格式。

## 架构

```
cosh-cli / cosh-core          ws-ckpt daemon
     │                              │
     │  Unix socket                 │
     │  /run/ws-ckpt/ws-ckpt.sock   │
     │─────────────────────────────→│
     │  [4B LE len][bincode req]    │
     │                              │
     │←─────────────────────────────│
     │  [4B LE len][bincode resp]   │
```

客户端实现位于 `crates/cosh-platform/src/checkpoint.rs`（`CkptClient`），
类型定义位于 `crates/cosh-types/src/checkpoint.rs`。

## 帧格式

每条消息由两部分组成。

```
┌──────────────────┬───────────────────────────────┐
│ 4 字节 LE u32    │ bincode 编码的枚举载荷        │
│ (payload 长度)   │ (WsCkptRequest / Response)    │
└──────────────────┴───────────────────────────────┘
```

- 长度前缀使用小端序无符号 32 位整数，表示后续 bincode 载荷的字节数
- 最大响应限制为 64 MiB，避免耗尽内存
- 默认超时为 5000 ms，可通过 `CkptClient::with_timeout()` 配置

## 请求类型（WsCkptRequest）

bincode 按枚举变体索引序列化（第一个变体 = index 0）。**变体顺序即二进制契约，不可重排。**

| 索引 | 变体 | 说明 |
|------|------|------|
| 0 | `Init { workspace }` | 初始化工作空间 |
| 1 | `Checkpoint { workspace, id, message, metadata, pin }` | 创建快照 |
| 2 | `Rollback { workspace, to }` | 回滚到指定快照 |
| 3 | `Delete { workspace, snapshot, force }` | 删除快照 |
| 4 | `List { workspace, format }` | 列出快照 |
| 5 | `Diff { workspace, from, to }` | 两个快照间的差异 |
| 6 | `Status { workspace }` | 查询状态 |
| 7 | `Cleanup { workspace, keep }` | 清理旧快照 |
| 8 | `Config` | 获取守护进程配置 |
| 9 | `ReloadConfig` | 重新加载配置 |
| 10 | `Recover { workspace }` | 恢复工作空间 |
| 11 | `HealthAdvisory` | 健康检查 |

## 响应类型（WsCkptResponse）

| 变体 | 对应请求 | 关键字段 |
|------|----------|----------|
| `InitOk { ws_id }` | Init | 工作空间 ID |
| `CheckpointOk { snapshot_id }` | Checkpoint | 快照 ID |
| `RollbackOk { from, to }` | Rollback | 回滚源和目标 |
| `DeleteOk { target }` | Delete | 被删除的快照标识 |
| `Error { code, message }` | 任意 | 错误码 + 人类可读描述 |
| `ListOk { snapshots }` | List | `Vec<SnapshotEntry>` |
| `DiffOk { changes }` | Diff | `Vec<DiffEntry>` |
| `StatusOk { report }` | Status | `StatusReport` |
| `CleanupOk { removed }` | Cleanup | 被移除的快照 ID 列表 |
| `ConfigOk { config }` | Config | `ConfigReport` |
| `ReloadConfigOk` | ReloadConfig | 无载荷 |
| `CheckpointSkipped { reason }` | Checkpoint | 跳过原因（如无变更） |
| `RecoverOk { workspace }` | Recover | 恢复的工作空间路径 |
| `HealthAdvisoryOk { ... }` | HealthAdvisory | 超限工作空间数、磁盘用量 |

## 错误码（WsCkptErrorCode）

| 索引 | 变体 | 说明 |
|------|------|------|
| 0 | `WorkspaceNotFound` | 工作空间不存在 |
| 1 | `SnapshotNotFound` | 快照不存在 |
| 2 | `AlreadyInitialized` | 工作空间已初始化 |
| 3 | `BtrfsError` | Btrfs 操作错误 |
| 4 | `IoError` | I/O 错误 |
| 5 | `InvalidPath` | 非法路径 |
| 6 | `ConfirmationRequired` | 需要确认（如删除 pinned 快照） |
| 7 | `InternalError` | 内部错误 |
| 8 | `SnapshotAlreadyExists` | 快照 ID 冲突 |
| 9 | `WriteLockConflict` | 写锁冲突 |
| 10 | `DiskSpaceInsufficient` | 磁盘空间不足 |

## 客户端使用

```rust
use cosh_platform::checkpoint::CkptClient;

// 默认路径 /run/ws-ckpt/ws-ckpt.sock
let client = CkptClient::default_path();

// 或指定路径和超时
let client = CkptClient::with_timeout("/custom/path.sock", 10000);

// 健康检查
if !client.is_available() {
    eprintln!("ws-ckpt daemon not running");
}

// 操作示例
let result = client.create("/home/user/project", "snap-001", Some("initial"), None, false)?;
let list = client.list(Some("/home/user/project"))?;
let restored = client.restore("/home/user/project", "snap-001")?;
```

## 关键约束

| 约束 | 说明 |
|------|------|
| 变体顺序不可变 | bincode 使用索引序列化枚举，重排即破坏线格式 |
| 新增只能追加 | 新的 Request/Response 变体只能在末尾添加 |
| 类型必须同步 | `cosh-types` 中的定义必须与 `ws-ckpt-common` 完全一致 |
| 超时处理 | 客户端对 read/write 设置超时，避免守护进程无响应时阻塞 |
| 长度限制 | 响应超过 64 MiB 视为异常，立即断开 |
| socket 路径 | 默认 `/run/ws-ckpt/ws-ckpt.sock`，可通过环境变量或 CLI 参数覆盖 |

## 测试验证

```bash
cd src/cosh-ng

# bincode 往返序列化测试
cargo test --locked -p cosh-types -- checkpoint

# 变体索引契约测试
cargo test --locked -p cosh-types test_request_bincode_variant_index

# CkptClient 单元测试（不需要运行的守护进程）
cargo test --locked -p cosh-platform -- checkpoint
```

## cosh-core 会话管理 JSON 协议

`cosh-core --session-control` 是 cosh-shell 用于发现、验证和清理 provider
对话的稳定内部边界。它从标准输入处理一个 JSON 请求，向标准输出写入一个 JSON
响应后退出。该模式会加载配置和作用域会话存储，但不会初始化 provider、
扩展、技能、Hook 或认证。

调用方必须发送要管理的规范工作空间路径。Core 会再次规范化该路径，推导工作
空间作用域存储，并在构造会话文件名之前验证每个小写规范 UUID。Core 还会从
`<workspace>/.copilot-shell/config.toml` 加载项目配置；不会使用管理进程无关
的当前目录来解析 `session.auto_persist` 或 `session.persist_dir`。标准输入限制
为 1 MiB；输入超限、非法 UTF-8、畸形 JSON 或缺少必填字段时，会在初始化存储
前统一返回 `invalid_request`。

### 请求动作

请求使用 `action` 区分。

```json
{"action":"list","workspace_scope":"/work/project","limit":20,"cursor":null,"all_workspaces":false}
{"action":"inspect","workspace_scope":"/work/project","session_id":"2d711642-b726-4b04-8d2a-8a0470f4ed24"}
{"action":"validate","workspace_scope":"/work/project","session_id":"2d711642-b726-4b04-8d2a-8a0470f4ed24"}
{"action":"prepare_clear_all","workspace_scope":"/work/project","protected_session_ids":[],"limit":4096,"cursor":null}
{"action":"clear","workspace_scope":"/work/project","session_ids":["2d711642-b726-4b04-8d2a-8a0470f4ed24"],"protected_session_ids":[]}
```

| 动作 | 契约 |
|------|------|
| `list` | 返回按更新时间倒序的摘要。`limit` 默认为 20，并限制在 1 至 100；使用 opaque `next_cursor` 读取下一页。当 `all_workspaces` 为 `true` 时，Shell 客户端请求更大的初始页大小 100 以减少首页不完整输出，但仍受核心侧 1 至 100 的相同限制。Core 会枚举存储根下所有工作空间哈希目录，返回所有工作空间的会话，并将非当前工作空间的会话标记为 `scope_mismatch`。`all_workspaces` 默认值为 `false`，以保持向后兼容。 |
| `inspect` | 即使健康状态不允许恢复，也返回摘要。 |
| `validate` | 完整加载信封，只有可恢复会话才成功。 |
| `prepare_clear_all` | 不加载或传输摘要，按 UUID 字典序分页返回可清理和受保护 ID。`limit` 限制在 1 至 4096；通过 `next_cursor` 继续。只有完整计划可放入单个 4096-ID 分页时才允许省略 `limit`，避免旧客户端静默接受不完整计划。 |
| `clear` | 独立删除每个请求 ID，并返回逐项跳过错误。每个请求最多接受 128 个 ID。 |

摘要包括 `session_id`、`workspace_scope`、创建和更新时间、模型、消息数、首条
提示、schema 版本，以及以下健康状态之一，即 `ready`、`corrupt`、
`incompatible` 或 `scope_mismatch`。
首条提示预览会在序列化前规范为单行，并限制为 160 个 Unicode 字符。列表先
使用有界文件系统元数据对 UUID 文件排序，再只读取当前请求分页，不会在每次
请求中反序列化全部历史。持久化、列表、验证和恢复统一将单个会话文件限制为
32 MiB；超限记录无需分配其内容即可报告为 `corrupt`。若单个条目在读取时
消失或无法读取，列表会继续扫描，直至填满当前分页或耗尽候选，因此首个被过滤
条目不会隐藏同页后续健康会话。cursor 编码上一页末尾的
文件系统倒序排序键，因此分页之间删除该条目不会令分页重回第一页。

### 响应信封

成功数据使用对应动作标记。

```json
{
  "ok": true,
  "data": {
    "action": "list",
    "sessions": [],
    "next_cursor": null
  }
}
```

请求级错误以状态码 1 退出。

```json
{
  "ok": false,
  "error": {
    "code": "not_found",
    "message": "session not found: 2d711642-b726-4b04-8d2a-8a0470f4ed24",
    "recoverable": true,
    "hint": "Refresh the session list and choose an existing entry."
  }
}
```

稳定错误码包括 `invalid_id`、`invalid_cursor`、`invalid_request`、
`not_found`、`io`、`corrupt`、`incompatible_version`、`scope_mismatch`、`conflict` 和
`active_session`，交互式调用方可处理这些错误。若 `clear` 仅有逐项失败，
响应仍为 `ok: true`；失败条目及其类型化错误位于 `data.skipped`。

`protected_session_ids` 是交互式删除的强制纵深保护。cosh-shell 会同时发送已
选择和已激活的 provider ID；即使这些 ID 也出现在 `session_ids` 中，Core
仍拒绝删除。`prepare_clear_all` 和 `clear` 都必须携带该字段；显式空数组表示
调用方确认当前没有受保护身份，省略字段则会在执行任何删除前拒绝整个请求。
shell 会抽干有界的 `prepare_clear_all` ID 分页，在确认前展示精确计划，随后
通过每批 128 条的 `clear` 提交这些 ID；该流程不会抽干所有摘要分页。Core
会拒绝超限的直接批次，并按 UTF-8 字节限制逐项 ID 与错误文本。Core 还会对
完整序列化 envelope 执行 1 MiB 硬预算检查，因此多字节输入无法绕过客户端响应
上限。
每条 summary 在进入分页累计前，还会分别将不可信 model 元数据限制为 256 个
UTF-8 字节、workspace 元数据限制为 4096 个 UTF-8 字节。因此体积较大但其余
字段合法的会话文件，不会让 `list`、`inspect` 或 `validate` 分配无界响应或
退化为 `invalid_request`。

若后续 `clear` 批次失败，shell 会保留已确认的 `deleted` 和 `skipped` 结果。
失败批次作为 `unknown_session_ids` 返回，尚未发送的 ID 作为
`unattempted_session_ids` 返回；UI 不得将其折叠成隐藏先前删除结果的请求级错误。

shell 为每个一次性管理操作设置统一的十秒 deadline，覆盖进程启动、请求 pipe
写入和响应收集。请求写入使用非阻塞 pipe，并由 deadline-aware 生命周期循环
重试，因此无论 leader 还是脱离进程组的后代持有 stdin，都不能让批量 `clear`
writer 永久阻塞。发生超时或传输失败时，shell 会关闭请求 pipe、终止进程组、
升级为强制终止、等待 leader，并 join 输出 worker。
输出 worker 使用可取消的轮询；完成 leader 和进程组清理后，它们会排空已经可读
的字节并停止，即使有脱离原进程组的后代仍持有继承的输出描述符，也不会延长
管理请求的超时边界。普通 poll 超时只会继续轮询，绝不会进入阻塞 read，因此
静默 leader 不会在生命周期设置 stop 标志前卡住 reader。
客户端还将 JSON 响应限制为 1 MiB、stderr 诊断限制为 256 KiB；任一输出超限
都会关闭 pipe，并终止和回收 session-control 进程组。

### 持久化兼容性

schema v1 信封包含不可变 provider 会话 UUID、规范工作空间、时间戳、模型、
乐观并发代数和模型可见消息。写入使用同目录临时文件、文件与目录同步、原子
重命名和短期 advisory 锁。进程退出时内核会释放锁，因此未加锁的锁文件可直接
复用，不会被视为冲突。规范 workspace 路径必须是有效 UTF-8；Core 会在派生
scope 或存储哈希前对无效路径返回 `invalid_request`。乐观并发代数必须单调
递增，已存储的 `u64::MAX` 代数会无损拒绝，不能覆盖现有历史。在 Unix 上，
scoped 目录权限为 `0700`，会话、临时和锁文件权限为 `0600`。旧版原始消息
数组以内存中的代数零加载。Core 在构造 store 时先一次性解析存储根路径中的
symlink，随后在该规范根之下逐级且不跟随 symlink 地安全打开或创建每个
scoped 路径组件，并固定 workspace hash 目录；因此经 symlink 管理的家目录
或 dotfile 布局可正常工作，而根之下后续的 symlink 替换仍会被拒绝。scoped
列举、会话及锁打开、临时
文件创建、原子 rename 和删除全部相对该描述符执行；会话和锁打开使用
`NOFOLLOW`。因此把一个 workspace hash 目录替换为指向另一 workspace 的
symlink，不能重定向 `load`、`persist`、`list` 或 `clear`。清除会话时会同步
删除配对的锁文件；崩溃写入者残留的过期临时文件会在目录下次以写模式
打开时被清扫。

使用规范 UUID
显式查找时，Core 只检查能够证明归请求工作空间所有的旧版扁平目录。使用新版
默认根目录时，这只包括请求工作空间原有的相对 `sessions/` 目录。自定义根只有
在配置值本身相对工作空间、不含 `..`、已经作为目录存在且经过 symlink 解析后
仍位于规范 workspace 内时，才参与 legacy 查找。Core 会逐级且不跟随 symlink
地打开规范 workspace，然后用已打开的描述符固定每个合格 legacy 目录。legacy
列举、会话打开及删除始终相对该描述符执行，会话打开使用 `NOFOLLOW`。因此并发
rename 加 symlink 替换不能把 load 或 clear 重定向到固定的 workspace-owned
目录之外。绝对路径、`~/` 和父目录逃逸根不参与 legacy 查找；它们可以指定
scoped 存储，但 scoped 访问会拒绝规范存储根之下任意路径组件中的
symlink。Core 不会根据
目录前缀包含关系推断 legacy 所有权，也不会检查进程 cwd 或作用域不明确的共享
扁平根目录。归属已确立的 workspace-owned legacy 会话会与 scoped 信封一同
出现在 `list` 摘要中，因此 picker、`prepare_clear_all` 与显式 `clear` 观察到
的是同一批会话；而在已确立目录之外的来源不明文件不会被 `inspect`、
`validate` 或加载认领、列出或改写。显式 `clear` 也可删除
损坏的旧版文件，同时继续遵守受保护 ID。迁移会锁定旧源，把 schema v1 信封
原子写入请求的工作空间作用域，然后删除旧文件。旧文件清理失败会作为类型化
持久化错误返回，并由后续持久化重试。同时存在两份副本时，clear 会先删除
legacy 副本，因此 legacy 权限错误不会删除较新的 scoped 历史或重新暴露旧内容。

JSONL headless 协议和本管理协议共用 `SessionStore::load`；交互式选择无法绕过
直接 `cosh-core --resume` 使用的验证逻辑。

JSONL result 会明确标记会话加载失败。

```json
{"type":"result","is_error":true,"errors":["session recovery failed [not_found]: session not found"],"session_error_code":"not_found","session_error_phase":"load","session_id":"..."}
```

cosh-shell 会区分用户选择的恢复与 active provider 会话的自动续接。Core 的
会话错误会单独携带 `session_error_code` 和 `session_error_phase`。`load`
阶段的 `not_found`、`corrupt`、`incompatible_version` 或 `scope_mismatch`，
以及任意类型化的 `persist` 失败，只会释放本次匹配的尝试身份。owned selected
attempt 会进入 `failed`，同时保留结构化 code、provider 原始 message 和按阶段
生成的恢复 hint；原有 active UUID 保持可用。普通 provider 错误文本不能伪装
成会话错误。无论 selected 还是 active 恢复，提交前 provider 返回的 ID 都必须
与尝试 ID 一致，且无关的 selected ID 会继续保持选择。单轮
`disable provider resume`
hint 会省略 `--resume`，但不会消费用户尚未执行的选择。

JSONL `system/init` 消息包含 `session_resumable`。值为 `false` 表示禁用了
`session.auto_persist`。消费方不得捕获输出的 UUID，而且只有本次调用确实通过
`--resume` 携带某个身份时，才能使该身份失效。一次 fresh fallback 因此不能
清除无关的 selected ID 或旧 active ID。即使本轮随后失败、被取消或异常退出，
该规则仍适用。为兼容尚未实现该扩展的 provider，此字段为可选；字段缺失时
保留既有会话 ID 捕获行为。

active ID、工作空间和调用 generation 由同一个状态锁持有。每次实际启动都会
领取 generation token；成功、失败、取消、不可恢复清理和身份不匹配转换仅在
该 token 仍属于最新尝试时提交。因此取消后延迟退出的旧 worker 无法清除或覆盖
新一轮提交，即使两轮恢复的是同一个 selected ID。若结构化会话结果之后又发生
传输错误，shell 会先完成结构化终态和会话状态收尾，再交付传输错误。替换或拒绝
selection 也会原子推进 generation；未携带 `--resume` 的 fresh turn 会把被抢占的
`restoring` owner 退回 `selected`。取消路径会先应用已解析的结构化会话错误，
再保持对外 `AgentCancelled` 语义。破坏性的会话管理会在整个 clear 操作期间
持有同一状态 lease，selection 也会在 validate 到提交的完整区间持有它。因此
clear 与激活按线性顺序执行，不再依赖可能过期的 protected ID 快照。

### 测试验证

```bash
cd src/cosh-ng
cargo test --package cosh-core
cargo test --package cosh-shell --test protocol
```

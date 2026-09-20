# ws-ckpt 设计文档

> ws-ckpt 是一个基于 btrfs CoW 快照的工作区状态管理系统，面向 AI Agent 工作流提供微秒级 checkpoint / rollback 能力。

---

## 设计哲学

AI Agent 在自主修改文件、调用工具时，每一步都可能"改坏"用户的工作区——写错文件、删错目录、把代码改得不可工作。Git 能解决长周期的版本管理，但不适合 Agent 这种"每一步操作都可能后悔"的场景：

- Git 需要显式 commit、需要干净的工作树、需要用户参与；
- Agent 需要的是**在每次 LLM 回合或工具调用前后，悄无声息地按下"保存点"**，错了能立刻 rollback，不影响主流程。

ws-ckpt 借助 btrfs 的 CoW 特性，把"保存当前工作区"变成**毫秒级、零额外空间**的元数据操作：

- Agent 不用 ws-ckpt 也能干活，但用了之后每次"改坏"都有兜底；
- 把 root 权限的特权操作（mount、losetup、btrfs subvolume）收敛到守护进程，CLI / Plugin 只走 Unix Socket，不需要 sudo；
- 把"哪个目录是工作区、哪些快照属于它"作为可持久化的 daemon 状态，daemon 重启不丢工作区。

**价值 = 让"试错型修改"有零成本兜底，同时把特权操作收敛到一个可审计的守护进程里。**

---

## 三层架构

| # | 组件                                   | 角色                          | 进程                   | 权限             |
| - | ------------------------------------- | ----------------------------- | ---------------------- | --------------- |
| 1 | CLI (`ws-ckpt`)                       | 用户 / 上层调用入口              | 短生命周期              | 普通用户          |
| 2 | Daemon (`ws-ckpt --daemon`)           | 状态机 + 调度器 + IPC server    | systemd 常驻           | root             |
| 3 | Backend (`BtrfsBase` / `BtrfsLoop`)   | 实际操作 btrfs 子卷的存储引擎     | daemon 内的 trait 对象 | 借 daemon 的 root |

调用方向：

```
Plugin / 用户 → ws-ckpt CLI → Unix Socket → ws-ckpt daemon → StorageBackend → btrfs subvolume
```

Daemon 是唯一持有 root 与 btrfs 操作能力的进程；CLI 和 Plugin 都只是协议客户端。

---

## IPC 协议

CLI 与 daemon 通过 Unix Domain Socket + 长度前缀 + bincode 二进制帧通信。

### 帧格式

```
[4 字节 LE 长度][bincode 载荷]
```

- Socket 默认路径：`/run/ws-ckpt/ws-ckpt.sock`（`0o666` 权限，非 root 可写）
- 最大帧大小：`MAX_FRAME_SIZE = 16 MiB`，超过则双方拒收
- 编解码统一在 [ws-ckpt-common](../src/crates/common/src/lib.rs) 中：`encode_frame()` / `decode_payload()`

### Request / Response 枚举

```rust
pub enum Request {
    Init,
    Checkpoint,
    Rollback,
    Delete,
    List,
    Diff,
    Status,
    Cleanup,
    Config,
    ReloadConfig,
    ReloadGlobalConfig,
    ReloadWorkspacePolicy,
    ConfigOverview,
    Recover,
    HealthAdvisory,
    GetWorkspacePolicy,
    ResetWorkspacePolicy,
    PatchWorkspacePolicy,
    WorkspaceIdentityV2,
    GuardedCheckpointV2,
    CheckpointEvidenceV2,
    Unregister,
}

pub enum Response {
    InitOk,
    CheckpointOk,
    RollbackOk,
    DeleteOk,
    ListOk,
    DiffOk,
    StatusOk,
    CleanupOk,
    ConfigOk,
    ReloadConfigOk,
    CheckpointSkipped,
    RecoverOk,
    HealthAdvisoryOk,
    WorkspacePolicyOk,
    ConfigOverviewOk,
    WorkspaceIdentityV2Ok,
    GuardedCheckpointV2Ok,
    CheckpointEvidenceV2Ok,
    GuardedCheckpointV2Rejected,
    Error,
    RecoverWithWarning,
    UnregisterOk,
}

pub enum ErrorCode {
    WorkspaceNotFound,
    SnapshotNotFound,
    AlreadyInitialized,
    BtrfsError,
    IoError,
    InvalidPath,
    ConfirmationRequired,
    InternalError,
    SnapshotAlreadyExists,
    WriteLockConflict,
    DiskSpaceInsufficient,
    CwdOccupied,
    CwdScanFailed,
}
```

调用流程：CLI 端把 `Request` 编码成帧 → daemon 端 `decode_payload` → 进入 dispatcher → 路由到 `workspace_mgr` / `snapshot_mgr` → 调用 `StorageBackend` trait → 把结果包成 `Response` 写回。

### 错误码体系

`ErrorCode` 枚举返回结构化错误，避免上层 parse 文本：

| 错误码                                                                                                                    | 含义                                                                             | 场景                                       |
| ------------------------------------------------------------------------------------------------------------------------- | -------------------------------------------------------------------------------- | ------------------------------------------ |
| `WorkspaceNotFound`                                                                                                     | 工作区未注册                                                                     | 调用方传了未 init 的路径                   |
| `SnapshotNotFound`                                                                                                      | 快照不存在 / 前缀无法解析 / 祖先链断裂                                           | rollback / delete 目标找不到；`-n` 遇到 head 为空、parent 链中断或深度不足 |
| `AlreadyInitialized`                                                                                                    | 工作区已被 daemon 管理                                                           | init 重复调用（幂等成功）                  |
| `SnapshotAlreadyExists`                                                                                                 | 同 workspace 内 snapshot id 冲突                                                 | checkpoint id 重复                         |
| `WriteLockConflict`                                                                                                     | 工作区正在写入                                                                   | inotify 检测到 in-flight 写                |
| `CwdOccupied`                                                                                                           | 确认有进程把 cwd 设在工作区内                                                    | init / rollback 拒绝执行（NOT retryable）  |
| `CwdScanFailed`                                                                                                         | `/proc` 扫描未能完成（canonicalize 失败 / procfs 不可读 / mountinfo 解析失败） | fail-closed：状态未知，**retryable** |
| `BtrfsError` / `IoError` / `InvalidPath` / `DiskSpaceInsufficient` / `ConfirmationRequired` / `InternalError` | 通用错误                                                                         | 见 dispatcher / backend                    |

CLI 把 `ErrorCode` 翻译成人类可读消息 + 退出码；Plugin 进一步映射成对 LLM 友好的提示（见 [plugin 设计文档](./ws-ckpt-plugin-design.md) 中的 `mapErrorToLLMMessage`）。

Guarded checkpoint V2 的完整说明见独立的
[双语设计文档](design/guarded-checkpoint-v2_zh.md)。

---

## DAG Lineage（快照血缘）

### 动机

原始设计中 snapshot 之间没有父子关系，rollback 必须显式指定目标 snapshot ID。AI Agent 工作流中频繁需要"撤回 N 步"的操作，显式 ID 对 Agent 不友好。

### 数据模型

- `SnapshotMeta` 增加 `parent_id: Option<String>`：指向创建该 snapshot 时的 head。`None` 表示 root 或历史孤立节点。
- `SnapshotIndex` 增加 `head: Option<String>`：当前工作区在 DAG 中的位置（最后一次 checkpoint 或 rollback 的目标）。

两个字段均标注 `#[serde(default)]`，确保旧 index.json 向前兼容。

### 语义

- **checkpoint**: `parent_id = head`，然后 `head = 新 snapshot`。
- **rollback --snapshot**: `head` 更新为 rollback 目标。
- **rollback --num-ancestors N**: head 算第 1 个祖先（`-n 1` = head 本身），head.parent 算第 2 个，依此递推。找到目标后执行 rollback 并更新 head。
- rollback 后继续 checkpoint 会产生 DAG 分支（不删除旧分支上的 snapshot）。

### 旧数据兼容

不做自动迁移。历史 snapshot 的 `parent_id = None`（孤立节点），`head = None`。`rollback --num-ancestors` 遇到 `head == None` 或 `parent_id == None` 时返回错误，提示使用 `--snapshot`。首次新 checkpoint 后 DAG 自动建立。

---

## 命令域

每个 CLI 子命令对应一个或多个 `Request` variant；dispatcher 是 1:1 路由表。

| #  | 命令           | Request          | 行为                                                                                                 |
| -- | -------------- | ---------------- | ---------------------------------------------------------------------------------------------------- |
| 1  | `init`       | `Init`         | 把目标目录改造为受管工作区：`ws-{SHA256(path)[:6]}` 命名子卷、rsync 数据进去、原目录替换为 symlink |
| 2  | `checkpoint` | `Checkpoint`   | `btrfs subvolume snapshot -r` 创建只读快照                                                         |
| 3  | `rollback`   | `Rollback`     | `-s` 按 ID 或 `-n` 沿 parent 链回退 N 个祖先；子卷原地替换 + 更新 head                          |
| 4  | `delete`     | `Delete`       | 删除指定快照子卷；全局唯一 ID 可省略 `-w`                                                          |
| 5  | `list`       | `List`         | 列出工作区（或全局）快照，支持 table / json                                                          |
| 6  | `diff`       | `Diff`         | 对比两个快照间的文件变更，输出 `+ / - / M / R`                                                     |
| 7  | `cleanup`    | `Cleanup`      | 按数量保留最近 N 个非 pinned 快照                                                                    |
| 8  | `status`     | `Status`       | 守护进程 uptime + 工作区列表 + 文件系统使用量                                                        |
| 9  | `config -g`  | `Config` / `ReloadConfig` / `ReloadGlobalConfig` | 查询或写入全局 `/etc/ws-ckpt/config.toml` 并 reload |
| 10 | `config -w`  | `GetWorkspacePolicy` / `PatchWorkspacePolicy` / `ResetWorkspacePolicy` / `ReloadWorkspacePolicy` | 查询、patch、reset 单个工作区的 `policy.toml` |
| 11 | `config`（无 scope） | `ConfigOverview` | 全局配置快照 + ws 覆盖统计 |
| 12 | `reload`     | `ReloadConfig` | 等价于 `systemctl reload ws-ckpt`                                                                  |
| 13 | `recover`    | `Recover`      | 把工作区还原成普通目录（撤销 init）                                                                  |
| 14 | `daemon`     | —               | 手工启动 daemon（生产路径走 systemd）                                                                |
| 15 | `unregister` | `Unregister` | 仅在 live 子卷缺失时移除注册，保留快照、备份与索引归档 |

### Agent 友好的设计点

- **自动 init**：dispatcher 收到 `Checkpoint` 时会先 `ensure_bootstrapped()` + `auto_init_workspace()`，未 init 的工作区会被透明初始化，Plugin 无需先调用 `init`
- **空工作区跳过**：`checkpoint` 发现工作区为空时返回 `CheckpointSkipped`，而不是创建无意义的空快照
- **前缀解析**：`SnapshotIndex::resolve_by_prefix` 允许传 snapshot id 的前缀；唯一即可命中，否则返回 `Ambiguous(n)`
- **diff 智能解析**：`btrfs send` 输出的临时 inode 引用（`o261-118-0`）会被自动解析为真实路径，并对同一文件的多次操作去重合并

---

## 各命令设计

每个命令独立讨论一次：CLI 形态、daemon 内的行为、值得展开的设计取舍。

### `init`

```bash
ws-ckpt init -w <workspace>
```

把普通目录改造成受管工作区：

1. `ws_id = "ws-" + SHA256(canonicalize(workspace))[:6]`——hash 路径而非 inode，daemon 重启后能稳定重算
2. 创建 btrfs 子卷 `<data_root>/<ws_id>`
3. 把原目录的数据 `rsync` 进子卷（btrfs-base 场景下若同卷则用 `cp --reflink=always`）
4. 删除原目录，替换为指向子卷的 symlink
5. 写入 `state.json` + `indexes/<ws_id>/index.json`

设计取舍：

- **路径作为身份**：用 path-hash 而非 inode，是为了让 daemon 重启 / 子卷恢复后还能找回工作区（inode 会变，path 不变）
- **三种重入路径**：① 已注册工作区的 init 是幂等 no-op；② daemon 重启后发现 symlink 仍指向 `<data_root>/<ws_id>`，触发 `adopt_existing_subvol` 重新登记；③ symlink 不存在但 ws_id 命中持久化记录，按"恢复模式"重建索引
- **cwd 占用守卫**：在 rsync 之前调用 `guard_cwd_occupants`（含 bind-mount 别名推导），避免 symlink swap 撕掉其他进程的 cwd。扫描失败返回 `CwdScanFailed`（可重试），确认有占用返回 `CwdOccupied`（不可重试）
- **非 UTF-8 路径拒绝**：canonicalize 后检查 `to_str().is_none()`，拒绝非 UTF-8 路径——lossy 字符串写入 manifest 后 daemon 重启会 IO 失败
- **故障安全**：BtrfsBase InPlace 模式下先 `rename` 原目录为 `.pre-init-bak`，再用 `cp --reflink=always`（CoW 零拷贝）迁移数据；任何步骤失败都能从 backup 完整恢复用户数据。`backup_owned` 标记保证只有本次 init 创建的 backup 才会被 cleanup 还原，不会覆盖用户的其他数据
- **初始化与生命周期锁**：init 在读取路径或孤儿备份之前取得 `init_lock`，注册完成后释放；recover/unregister 使用相同锁。锁序为 `init_lock` → `lock_wsid` → workspace `RwLock`，避免路径 rename 窗口与恢复操作竞争。不同工作区的初始化会串行排队。
- **中断初始化预检**：canonicalize 原路径和创建子卷之前检查 `.pre-init-bak`，使原目录已被改名的情况也能恢复。仅有备份时还原；备份与候选子卷同时存在时拒绝自动迁移，指向显式 recover。后端守卫在当前 init 的失败清理范围之外，防止删除旧子卷。

### `checkpoint`

```bash
ws-ckpt checkpoint -w <workspace> [-s <id>] [-m <message>] [--metadata <json>]
```

daemon 端的关键序列（[`snapshot_mgr::checkpoint`](../src/crates/daemon/src/snapshot_mgr.rs)）：

1. `resolve_workspace` — 接受绝对路径 / ws_id / 相对路径三种
2. `check_workspace_quiescent` — inotify 检测活跃写，命中即 `WriteLockConflict`
3. **snapshot id 校验**：拒绝空/纯空白 id，拒绝含 `/`、`\`、`.`、`..` 的路径穿越 id（clap parser 层拦截，#672）；再校验本工作区内唯一，否则 `SnapshotAlreadyExists`
4. 空目录检测 → `CheckpointSkipped`（不算错误）
5. `backend.create_snapshot()` — `btrfs subvolume snapshot -r`
6. 持久化 `index.json`

设计取舍：

- **不阻塞磁盘满**：btrfs 快照是元数据 + COW，满盘也能成功；空间不足只通过 `status` / health-check 上报，不在 checkpoint 路径上 fail
- **id 在 CLI 边界确定**：CLI 未收到 `-s` 时自动生成 ID；plugin 使用 `secrets.token_hex(4)` / `crypto.randomUUID().slice(0,8)` 显式提供 ID；daemon 不替调用方生成
- **metadata 用 String 而非 Value**：IPC 走 bincode，`serde_json::Value` 需要 `deserialize_any`，bincode 不支持；daemon 端再解析回 Value 存盘

### `rollback`

```bash
ws-ckpt rollback -w <workspace> -s <snapshot>        # 按 snapshot id 回滚
ws-ckpt rollback -w <workspace> -n 3                  # 沿 parent 链回退 3 个祖先
ws-ckpt rollback -w <workspace> -s <snapshot> --preview # 仅预览文件变更
```

`--snapshot/-s` 和 `--num-ancestors/-n` 互斥（clap `conflicts_with`），至少指定一个。`-n` 值由 `value_parser` 约束 `>= 1`。

`-n` 路径先调用 `SnapshotIndex::ancestor(n)` 沿 `parent_id` 链解析目标 snapshot，然后走与 `-s` 相同的子卷替换流程。

`--preview` 复用相同的目标解析逻辑，但不执行子卷替换。它直接复用 `backend.diff(ws_id, target, None)`，为当前工作区创建短生命周期只读快照并计算目标快照到当前工作区的标准 diff。输出表示目标快照之后发生、rollback 将撤销的变化，不尝试把 `DiffEntry` 反向转换成回滚动作；尤其 `Renamed` 没有结构化的起止路径，不能可靠反转。临时快照在成功或失败路径都会尽力删除。

`backend.rollback()` 的核心是**子卷原地替换**：

1. `rename(ws_path, ws_path.rollback-tmp)` — 把当前工作子卷挪到临时名
2. `btrfs subvolume snapshot <snap> <ws_path>` — 从目标只读快照创建可写子卷，直接放到原 ws_path 位置
3. `delete_subvolume(.rollback-tmp)` — 删除旧子卷（non-fatal）

锁策略：read lock 取 workspace path → 释放 → `guard_cwd_occupants`（慢 /proc 扫描在锁外）→ write lock 执行 rollback。慢 IO 不在 RwLock 内跑，避免阻塞其他工作区的并发请求。

设计取舍：

- 用户侧的 **symlink（`原目录 → data_root/ws_id`）全程不动**
- **不修改原快照**：rollback 不破坏 `<snapshot>` 子卷本身，工作区可以反复回到同一快照
- **必须 cwd 守卫**：子卷替换后，旧子卷内的 inode 全部失效，占用 cwd 的进程下次 `getcwd()` 会 ENOENT
- **snapshot id 校验**：同 checkpoint，拒绝空/路径穿越 id（issue #672）
- **支持 snapshot id 前缀**：调用方传 `abc12` 即可命中 `abc123def456`，`ResolveError::Ambiguous(n)` 提示用户加长前缀
- **ancestor 四层防御**：`ancestor()` 入口 `n < 1` 拦截 + CLI `value_parser(1..)` + hermes `num_ancestors >= 1` + openclaw `numAncestors < 1` 校验

### `delete`

```bash
ws-ckpt delete [-w <workspace>] -s <snapshot> [--force]
```

`-w` 可省略——daemon 会跨所有工作区查找：

- 全局唯一命中 → 直接删
- 多匹配 → 返回 `SnapshotNotFound` + "matches in multiple workspaces, please specify -w" 提示
- 完全没匹配 → 返回 `SnapshotNotFound`

设计取舍：

- **snapshot id 校验**：同 checkpoint/rollback，拒绝空/路径穿越 id（issue #672）
- **`--force` 跳过确认**：dispatcher 不区分交互；`ConfirmationRequired` 错误码留给 CLI 端决定要不要 prompt，daemon 永远只回 Ok / Error

### `list`

```bash
ws-ckpt list [-w <workspace>] [--format <table|json>]
```

省略 `-w` 列出所有工作区的所有快照，按 `created_at` 升序输出。

- **table 格式**：默认；适合人读，长 message 会截断
- **json 格式**：plugin / 脚本消费；不截断，metadata 原样输出

### `diff`

```bash
ws-ckpt diff -w <workspace> -f <from> -t <to>
```

走 `btrfs subvolume find-new` + `btrfs send --no-data` 计算两个快照间的变更，返回 `Vec<DiffEntry>`：

| 标记  | ChangeType   | 含义            |
| ----- | ------------ | --------------- |
| `+` | `Added`    | 新增文件 / 目录 |
| `-` | `Deleted`  | 删除            |
| `M` | `Modified` | 内容修改        |
| `R` | `Renamed`  | 重命名          |

设计取舍：

- **临时 inode 解析**：`btrfs send` 中间输出形如 `o261-118-0` 的临时引用（"inode 261，generation 118"），diff parser 会回查最终路径，对同一文件的多次操作（Added → Modified → Renamed）去重合并成一条
- **不输出 文件内行级别增删**：当前只列文件清单，不重建差异内容——重建需要 `btrfs send` 完整流，量级是文件本身大小，不适合在 IPC 帧内传

### `cleanup`

```bash
ws-ckpt cleanup -w <workspace> [--keep <N>]
```

手动触发的 cleanup，不指定 `keep` 参数时默认保留 20 个快照。区别于后台 `auto_cleanup_loop`：

|            | 手动 `cleanup`    | 后台 `auto_cleanup_loop`                    |
| ---------- | ------------------- | --------------------------------------------- |
| 触发       | CLI 显式调用        | scheduler 周期触发                            |
| 策略       | 命令行 `--keep N` | `auto_cleanup_keep`（Count / Age 两种模式） |
| 范围       | 单一工作区          | 所有工作区                                    |
| 必填工作区 | 是                  | 否                                            |

两者共用 `backend.cleanup_snapshots()`，CLI 路径选哪些 id 由 `snapshot_mgr` 决定。

### `status`

```bash
ws-ckpt status [-w <workspace>] [--format <table|json>]
```

返回 `StatusReport { uptime_secs, workspaces[], fs_total_bytes, fs_used_bytes }`。

- 不传 `-w`：所有工作区 + 全局 fs 使用量
- 传 `-w`：单工作区视图

设计取舍：

- **fs 查询失败不报错**：macOS 等不支持 btrfs 的环境下 `get_usage()` 会失败，daemon 用 `(0, 0)` sentinel 占位，让 status 命令仍有结果。

### `config`

`config` 子命令有 2 种 scope:

- `-g` / `--global` 操作 `/etc/ws-ckpt/config.toml`(daemon-wide 默认)
- `-w <workspace>` / `--workspace <workspace>` 操作 `/var/lib/ws-ckpt/indexes/<ws_id>/policy.toml`(per-ws 覆盖)
- 不带 scope 调用返回总览视图（全局配置快照 + 有覆盖的 ws 统计），**写入操作**必须显式带 scope。

```bash
ws-ckpt config                                 # 总览: 全局快照 + ws 覆盖统计

# 全局
ws-ckpt config -g                              # 查看
ws-ckpt config -g --auto-cleanup-keep 30d      # 写

# 局部
ws-ckpt config -w ~/proj                       # 三栏视图: effective / local / global
ws-ckpt config -w ~/proj --auto-cleanup-keep 5 # 仅这个 ws 保留 5 份
ws-ckpt config -w ~/proj --disable-auto-cleanup
ws-ckpt config -w ~/proj --reset               # 删除局部 policy,沿用全局
```

写入路径：`-g` 由 CLI 写盘后发 `ReloadConfig` 让 daemon 重读；`-w` 由 daemon 端原子 patch（读→改→save→刷内存→唤醒 scheduler），仅 reload 本工作区相关配置。

合并语义：`effective(field) = local.field.or(global.field)`。`-w` 仅可覆盖 `auto_cleanup` 与 `auto_cleanup_keep`，其余字段是 daemon-wide，带 `-w` 设置会被 CLI 拒绝。

设计取舍:

- **scope 强制显式**:老脚本 `ws-ckpt config --enable-auto-cleanup` 直接报错,绝不让"我以为改了全局其实改了局部"或反之的歧义存在。CHANGELOG 提供迁移指引
- **写盘和广播都在 CLI 触发**:daemon 自己不主动 watch 文件,避免和 systemd `reload` 信号路径冲突
- **bootstrap-time 字段标特殊**:`backend.type` / `img_size` / `img_max_percent` 修改后 reload 只 warn 不动镜像,必须 `systemctl restart ws-ckpt` 才生效——这是为了避免运行中改 backend 类型导致已注册工作区指向消失的 subvolume

### `recover`

```bash
ws-ckpt recover -w <workspace> [--force]
ws-ckpt recover --all [--force]
```

`init` 的逆操作：把受管工作区还原成普通目录。daemon 内 `backend.recover_workspace()`：

1. 确认 live 子卷可读后，删除 symlink（原路径释放）
2. `rsync -a --delete` 子卷内容回原路径（恢复为普通目录）
3. 删除所有 snapshot 子卷（扫描 `snapshots/{ws_id}/`）
4. 删除 workspace 子卷
5. 从 `state.json` / `index.json` 摘除登记

`--all` 批量 recover 所有已登记工作区，用于卸载前清场。未注册且有
`.pre-init-bak` 的工作区按原路径恢复：还原完整备份，保留候选子卷和快照；子卷可能是
中断复制后的部分内容，也可能有较新数据，不能据此删除备份。恢复拒绝覆盖非空目录
和普通文件。进入孤儿恢复前，会按规范化的父目录路径重新核对注册状态，避免别名
误入孤儿分支。已注册工作区正常 recover 后若仍有备份，将其归档为
`.pre-init-bak.recovered`（冲突时追加数字后缀，最多尝试 1000 个名称），
使用不覆盖目标的原子 rename 保留内容并释放原备份名，允许重新 init。
归档失败会返回包含检查、移动备份指引的警告。

常规响应保持 `RecoverOk { workspace }`；需要用户检查保留数据时返回
`RecoverWithWarning { workspace, warning }`。`Unregister`、`RecoverWithWarning`
和 `UnregisterOk` 追加在实际 IPC 枚举末尾，保留旧 bincode 变体编号与字段布局。
旧客户端不能识别新增操作/响应，使用新恢复路径时应同步更新 ws-ckpt CLI、daemon
和 cosh-ng。cosh-ng 镜像协议追加相同变体，并将恢复提示作为成功 JSON 的可选
`meta.warning` 返回；原路径存在性由 daemon 判断，允许中断初始化后的缺失路径。

设计取舍：
- **有意不加 cwd guard**：recover 是终止性的"拆除"操作，由 CLI 的 `ConfirmationRequired` prompt 守卫（交互式确认 + warning 提示用户自行检查 `/proc/*/cwd`）。不在 daemon 端加 `/proc` scan，避免管理员 `--force` 拆除时被 stale process 阻塞
- **rsync 失败即 bail**：rsync 失败后不继续删除快照和子卷（issue #674），保留完整数据供重试
- **生命周期锁**：recover 先取得 `init_lock`，再持有 `lock_wsid` 直到 `save_manifest` 完成，阻止并发 init 竞争路径、index_dir 和 manifest

### `unregister`

```bash
ws-ckpt unregister -w <workspace-or-id> [--force]
```

仅处理已注册但 live 子卷 `symlink_metadata` 返回 NotFound 的状态；子卷仍存在或
无法确认时拒绝操作。命令先将 `indexes/<ws_id>` 改名为
`indexes/<ws_id>.unregistered`（不覆盖已有归档），同步 `indexes/` 目录后，
在 manifest 锁内持久化移除结果，
再解除内存注册并移除指向缺失子卷的受管 symlink。持久化失败时还原 active index，
保留原内存注册和 symlink，允许直接重试。若进程在索引归档后、manifest 提交前退出，
启动时以 manifest 为准，校验并还原归档中的索引与策略，再注册工作区；
归档损坏会明确报错。manifest 已提交注销的归档不会复活注册。
快照目录和 `.pre-init-bak` 保留；响应 `UnregisterOk { workspace, retained_paths }`
明确表示未恢复数据，并只列出实际存在的保留位置。`--force` 与 recover 一样仅跳过 CLI 确认。

---

## 跨进程协调

ws-ckpt 的写操作会替换工作区 inode（btrfs subvolume + symlink swap），这要求多个进程协调：

### 1. Daemon 单实例（lockfile）

`/var/lib/ws-ckpt/daemon.lock` 通过 `flock(LOCK_EX)` 保证只有一个 daemon 进程持有 btrfs 操作权。崩溃残留的 lock 文件会被新 daemon 检测并接管。

### 2. 工作区写锁（inotify）

`fs_watcher::WorkspaceWatcher` 监听工作区 inotify 事件，`snapshot_mgr::checkpoint` 在创建快照前调用 `check_workspace_quiescent` 检查是否有 in-flight 写：

```rust
if !state.check_workspace_quiescent(&ws.ws_id).await {
    return Response::Error { code: WriteLockConflict, .. };
}
```

避免在 Agent 工具调用半途切快照导致快照内文件残缺。

### 3. cwd 占用守卫

init/rollback 都会让老 inode 失效，cwd 落在工作区内的进程会 ENOENT。Daemon 在执行前通过 `util::guard_cwd_occupants` 扫描 `/proc/*/cwd`（含 mountinfo bind-mount 别名推导），结果分三档：

- 无占用 → 继续
- `CwdOccupied` → NOT retryable，用户需移出进程
- `CwdScanFailed` → fail-closed + retryable（transient /proc race）

调用路径：init（rsync 之前）、rollback（backend 之前）。recover 有意不加——终止性操作由 CLI `ConfirmationRequired` 守卫。Plugin 在 hook 入口预先自查 cwd，节省一次 RPC 往返。

### 4. Seccomp

Daemon 在启动期、bootstrap 之后、listener 之前应用 `seccomp-bpf` syscall 过滤（`seccomp::apply_seccomp_filter`），限制可调用的系统调用面，降低 root 进程的攻击面。`TargetArch` 按编译时 `cfg(target_arch)` 选择（x86_64 / aarch64），而非运行时检测。失败仅 warn 不退出，保持向后兼容。

---

## 存储后端

`StorageBackend` trait（[`crates/common/src/backend.rs`](../src/crates/common/src/backend.rs)）抽象出所有 btrfs 操作；orchestration 层（dispatcher / workspace_mgr / snapshot_mgr）只调 trait 方法，不感知 btrfs 命令细节。

```rust
#[async_trait]
pub trait StorageBackend: Send + Sync {
    fn backend_type(&self) -> BackendType;
    async fn init_workspace(&self, original_path, ws_id) -> WorkspaceInfo;
    async fn create_snapshot(&self, ws_id, snapshot_id) -> ();
    async fn rollback(&self, ws_id, snapshot_id) -> PathBuf;
    async fn delete_snapshot(&self, ws_id, snapshot_id) -> ();
    async fn recover_workspace(&self, ws_id, original_path) -> ();
    async fn diff(&self, ws_id, from, to) -> Vec<DiffEntry>;
    async fn cleanup_snapshots(&self, ws_id, snapshot_ids) -> Vec<String>;
    async fn check_environment(&self) -> EnvironmentStatus;
    async fn get_usage(&self) -> (u64, u64);
    async fn bootstrap(&self, config) -> ();
    // …
}
```

### 后端实现

| BackendType   | 适用场景                              | 数据存放                                                                     |
| ------------- | ------------------------------------- | ---------------------------------------------------------------------------- |
| `BtrfsBase` | 宿主根分区已经是 btrfs                | 直接在宿主 btrfs 上创建 subvolume                                            |
| `BtrfsLoop` | 宿主是 ext4 / xfs 等非 btrfs 文件系统 | `/var/lib/ws-ckpt/btrfs-data.img` loop 镜像 + losetup + mkfs.btrfs + mount |

### 后端选择

`backend_detect.rs` 在 daemon 启动期决定后端：

1. **持久化优先**：`state.json` 记录上次使用的 backend，重启时沿用，避免数据漂移
2. **配置覆盖**：`/etc/ws-ckpt/config.toml` 的 `backend.type = "btrfs-base" | "btrfs-loop"` 强制指定
3. **auto 模式**：宿主根分区是 btrfs → `BtrfsBase`，否则 → `BtrfsLoop`

`BtrfsLoop` image 大小由两个配置共同约束（默认 `img_size = 30` GB，`img_max_percent = 40`）：

```
target = min(img_size GB, 宿主总空间 × img_max_percent / 100)
```

若 target 超过当前可用空间，降级为 `可用空间 × img_max_percent / 100`，避免占满磁盘。每次 bootstrap 自动 grow/shrink 到新 target。两个字段是 bootstrap-time only，修改后需重启 daemon 生效。

---

## 守护进程状态

`DaemonState`（[`crates/daemon/src/state.rs`](../src/crates/daemon/src/state.rs)）是 daemon 的运行时核心：

```rust
pub struct DaemonState {
    workspaces:    DashMap<String, Arc<RwLock<WorkspaceState>>>,  // ws_id → state
    path_to_wsid:  DashMap<PathBuf, String>,                       // 反向索引
    pub config:    std::sync::RwLock<DaemonConfig>,                // 热重载锁
    pub config_notify: Notify,                                      // reload broadcast
    pub mount_path: PathBuf,                                        // btrfs 挂载路径
    pub socket_path: PathBuf,                                       // Unix Socket 路径
    pub backend:   Arc<dyn StorageBackend>,
    pub start_time: std::time::Instant,
    bootstrapped:  OnceCell<()>,                                    // 懒初始化 BtrfsLoop
    watchers:      std::sync::Mutex<HashMap<String, WorkspaceWatcher>>,
    pub state_dir: PathBuf,                                         // /var/lib/ws-ckpt
    wsid_locks:    DashMap<String, Arc<Mutex<()>>>,                 // per-ws 生命周期锁
}
```

### 持久化布局

```
/var/lib/ws-ckpt/
├── daemon.lock                  # 写锁
├── state.json                   # 后端选择 + 工作区清单
├── btrfs-data.img               # BtrfsLoop 镜像
└── indexes/
    └── ws-xxxxxx/
        ├── index.json           # 该工作区的 SnapshotIndex
        └── policy.toml          # per-ws 策略覆盖（可选）
```

- `state.json`：backend 身份 + 所有已注册 workspace 的 `(ws_id, path)`
- `index.json`：单个工作区的快照索引；加载失败时从 btrfs snapshots 目录 `rebuild_from_fs` 回填
- `policy.toml`：per-ws 策略覆盖（可选，空 = 沿用全局）
- 所有写盘走 `atomic_write`（tmp + fsync + rename），保证崩溃安全
- `collect_workspace_entries` 通过 `path_to_wsid` 无锁遍历，避免 write-locked 的 ws 被跳过丢条目

### Daemon 启动顺序

[`crates/daemon/src/lib.rs`](../src/crates/daemon/src/lib.rs) 中 `run_daemon` 的关键步骤：

1. `geteuid().is_root()` 校验
2. 初始化 `tracing_subscriber`
3. 创建 `/var/lib/ws-ckpt/indexes/`
4. `lockfile::acquire` 单实例守卫
5. `startup::resolve_state`：加载 `state.json` → 创建 backend → bootstrap；bootstrap 在 backend
   mount/data root 就绪后同步恢复可证明的 interrupted rollback，再重建 in-memory state 和 watcher
6. `save_manifest`：持久化初始状态（bootstrap 可能改了 backend 选择）
7. `util::ensure_symlinks`：重启后修复工作区 symlink
8. `seccomp::apply_seccomp_filter`
9. `scheduler::start_scheduler` 启动后台任务
10. 注册 SIGTERM / SIGINT 处理器；SIGHUP 改成 no-op（reload 走 IPC）
11. spawn `listener::run_listener` 进入 accept 循环
12. 等待 shutdown 信号 → cancel token → drain listener → flush 所有 index.json → save final state.json → 删 lockfile

---

## 后台调度

`scheduler::start_scheduler` 启动两个后台任务，全部通过 `config_notify` push 唤醒（不轮询）：

| 任务 | 触发 | 行为 |
|------|------|------|
| **Auto-cleanup** | 每 `auto_cleanup_interval_secs` 或 config reload 唤醒 | 遍历所有 ws，按各自 effective policy 的 `Count(N)` 或 `Age(duration)` 删除过期快照。任一 ws 有局部 override 启用 cleanup 即运行，全部 disabled 时 park |
| **Health-check** | 每 `health_check_interval_secs` 或 config reload 唤醒 | 上报 fs 使用率 + 快照超限告警（>1000 / >90%），结果缓存供 CLI `HealthAdvisory` 拉取 |

config reload（`notify_waiters()`）会立即打断 sleep，让 loop 重读配置。`Notified` 在读 config 之前就 `enable()` 注册，避免 notify 丢失。

### Rollback 崩溃恢复

`.rollback-tmp` 是 rollback 把旧 live subvolume 移开后留下的中间状态，不能按文件名当作
可删除的 orphan。恢复由各 Btrfs backend 的 `bootstrap` 在 state rebuild、watcher、scheduler
和 listener 之前同步完成，并扫描 backend 自己的 data root：BtrfsBase 使用
`<btrfs-mount>/ws-ckpt-data`，BtrfsLoop 使用 loop image 的 mount root。

- tmp 是已验证的 Btrfs subvolume、live 不存在：使用 `RENAME_NOREPLACE` 恢复旧 live。
- tmp 不存在：幂等 no-op。
- live 与 tmp 同时存在，或任一路径是 symlink、普通目录、文件或无法验证：保留所有路径并
  阻止 backend bootstrap。没有 durable rollback commit evidence 时，daemon 不能证明应该
  保留哪一个 subvolume。
- recovery 只接管在 `snapshots/<workspace-id>/` 中有对应 ownership marker 的
  `.rollback-tmp`。该证据同时覆盖当前 ID 和 migration 保留的 legacy ID；没有 marker 的
  foreign path 只告警并保留。

当前 fail-closed 粒度是整个 daemon：一个 workspace 的 ambiguous rollback 会阻止其他
workspace 一同启动。这是缺少 per-workspace quarantine 时的安全取舍。该恢复也不能关闭
“新 live 已创建或旧 tmp 已删除，但 `index.head` 尚未持久化”的逻辑提交窗口；完整自动恢复
需要 durable rollback journal/commit record。真实 Btrfs crash injection 与断电持久性仍需
在隔离的特权环境验证。

---

## 配置

配置分为两层:

- **全局** `/etc/ws-ckpt/config.toml`,字段映射到 `FileConfig`
- **局部** `/var/lib/ws-ckpt/indexes/<ws_id>/policy.toml`,字段映射到 `WorkspacePolicy`(扁平,全 Optional)

合并语义按字段:`effective(field) = local.field.or(global.field)`。空文件 / `WorkspacePolicy::default()` = 完全沿用全局。

### 全局 (`config.toml`)

所有可热重载字段都在 `FileConfig`:

```toml
auto_cleanup                  = true
auto_cleanup_keep             = "30d"        # 或整数 20
auto_cleanup_interval_secs    = 86400
health_check_interval_secs    = 300

[backend]
type = "auto"                                # auto | btrfs-base | btrfs-loop

[backend.btrfs-loop]
img_size        = 30
img_max_percent = 40.0
```

### 局部 (`policy.toml`)

```toml
# 全部字段 Optional;Some 覆盖全局,None 沿用全局;空文件 = 完全沿用全局
auto_cleanup = true
auto_cleanup_keep = "30d"   # 同时支持整数(Count)/duration 字符串(Age),复用全局 CleanupRetention serde
```

仅这两个字段可 per-ws 覆盖。`auto_cleanup_interval_secs` / `health_check_*` / `img_*` / `backend.*` 全是 daemon-wide,带 `-w` 设置会被 CLI 拒绝。

### CleanupRetention 的两种模式

```rust
pub enum CleanupRetention {
    Count(u32),                       // TOML 整数：保留 N 个
    Age { raw: String, secs: u64 },   // TOML 字符串：保留 secs 秒内的
}
```

`raw` 保留用户的原始字符串（`"30d"`、`"2w"`）用于 round-trip 显示；`secs` 是 deserialize 时一次性解析的缓存。bincode 没有 `deserialize_any`，对 TOML/JSON 走 `is_human_readable()` 分支，对 wire 走 `CleanupRetentionWire` tagged 枚举。

### 重启才生效的字段

以下字段仅在 bootstrap 时读取，`reload` 不会应用：

- `backend.type` — 切换后端类型会导致已注册工作区指向不存在的 subvolume
- `backend.btrfs-loop.img_size` — 镜像大小调整需要 grow/shrink 操作
- `backend.btrfs-loop.img_max_percent` — 同上

修改后需 `systemctl restart ws-ckpt`。reload 时如果检测到这些字段变更，只 warn 不动作。

---

## Crate 结构

```
ws-ckpt/src/                              # Cargo workspace root
├── Cargo.toml                            # workspace = [common, daemon, cli]
├── config.toml.sample
├── crates/
│   ├── common/                           # 纯类型 + IPC 编解码（零副作用、零 root）
│   │   └── src/
│   │       ├── lib.rs                    # Request/Response/ErrorCode/SnapshotMeta/
│   │       │                             # DaemonConfig/FileConfig/CleanupRetention/
│   │       │                             # encode_frame/decode_payload
│   │       ├── backend.rs                # StorageBackend trait + EnvironmentStatus
│   │       ├── persist.rs                # DaemonStateFile / WorkspaceEntry
│   │       └── migration.rs              # state.json schema 演进
│   │
│   ├── daemon/                           # 守护进程 (binary: ws-ckpt-daemon)
│   │   └── src/
│   │       ├── lib.rs                    # run_daemon() 启动编排
│   │       ├── startup.rs                # state.json 加载 + backend 选择 + bootstrap
│   │       ├── listener.rs               # UnixListener accept + handle_connection
│   │       ├── dispatcher.rs             # Request → workspace_mgr/snapshot_mgr 路由
│   │       ├── workspace_mgr.rs          # init / recover / delete_snapshot
│   │       ├── snapshot_mgr.rs           # checkpoint / rollback / list / diff / cleanup
│   │       ├── state.rs                  # DaemonState (workspaces / config / backend)
│   │       ├── scheduler.rs              # auto-cleanup / health-check / orphan recovery
│   │       ├── index_store.rs            # index.json 加载/保存/重建
│   │       ├── fs_watcher.rs             # inotify 写锁检测
│   │       ├── backend_detect.rs         # auto / config / persisted 三选一
│   │       ├── backends/
│   │       │   ├── btrfs_base.rs         # 宿主即 btrfs
│   │       │   ├── btrfs_loop.rs         # loop 镜像 + losetup + mkfs + mount
│   │       │   └── btrfs_common.rs       # btrfs 命令封装 + diff parser
│   │       ├── seccomp.rs                # seccomp-bpf 过滤
│   │       ├── lockfile.rs               # daemon 单实例
│   │       └── util.rs                   # symlink / cwd guard / 路径解析
│   │
│   └── cli/                              # CLI 入口 (binary: ws-ckpt)
│       └── src/main.rs                   # clap Parser → encode_frame → IPC → render
│
├── plugins/
│   ├── hermes/                           # Hermes (Python) 插件，详见 plugin 设计文档
│   └── openclaw/                         # OpenClaw (TypeScript) 插件，详见 plugin 设计文档
│
├── skills/                               # 配套 OpenClaw skill 定义
├── systemd/                              # ws-ckpt.service 单元
└── docs/                                 # 本目录
```

### 依赖方向

- `cli → common`
- `daemon → common`
- `cli` 与 `daemon` 之间**只通过 IPC 协议**耦合，不共享代码

---

## 与 Plugin 的对接

Plugin（[Hermes](../src/plugins/hermes/) / [OpenClaw](../src/plugins/openclaw/)）不是 ws-ckpt 的协议客户端，而是 ws-ckpt CLI 的 subprocess 调用方：

```
LLM Agent runtime
  └── ws-ckpt Plugin (hooks: session_start / pre_llm_call / agent_end)
        └── subprocess: ws-ckpt checkpoint -w <ws> -s <uuid> -m "<msg>"
              └── Unix Socket → daemon → btrfs
```

这样做的取舍：

- Plugin 不依赖 daemon 协议版本，CLI 兼容性是唯一约束面
- Plugin 不需要 bincode / Unix Socket 客户端；写 Python / TypeScript 都很轻
- 代价是每次 checkpoint 多一次 fork + exec，但 ws-ckpt 自身是毫秒级操作，subprocess overhead 在可接受范围内

详细的 Plugin 设计见 [ws-ckpt-plugin-design.md](./ws-ckpt-plugin-design.md)。

---

## 与 OS Skills 的关系

`src/skills/ws-ckpt/SKILL.md` 提供了一份配套 OpenClaw 的 skill 描述，告诉 Agent："这台机器装了 ws-ckpt，你可以这样调用"。Skill 是 plugin 不可用时的兜底：支持 Agent 手动调用 CLI 完成 checkpoint/rollback 等操作，但**不支持 hook 自动快照**——自动 per-turn 快照只有 plugin 能做。

|          | OS Skill                               | Plugin                                    |
| -------- | -------------------------------------- | ----------------------------------------- |
| 角色     | 兜底选择 | 最佳适配              |
| 自动快照 | 不支持（无 hook 能力）                 | 支持（per-turn 自动 checkpoint）          |
| 输出     | Agent 自行解析 raw CLI 文本            | 结构化 Response，已翻译为 LLM 友好提示    |


---

## 不做的事

| 排除项                             | 原因                                                                              |
| ---------------------------------- | --------------------------------------------------------------------------------- |
| 跨主机分布式快照                   | btrfs 本身是单机文件系统；分布式留给上层（rsync / S3）做                          |
| 加密 / 压缩                        | 委托给 btrfs 自身的 `compress` mount option，不在 ws-ckpt 层做                  |
| Snapshot diff 的 binary patch 输出 | 当前只输出 `+/-/M/R` 文件列表，不重建文件内容变更                           |
| 替代 Git                           | Git 服务长期版本管理；ws-ckpt 服务的是短期"试错回退"，两者目标不同                |
| Windows / macOS 支持               | 依赖 btrfs，仅限 Linux                                                            |
| 跨用户共享工作区                   | 工作区 path 全局唯一即可被多用户访问，但权限隔离交给 fs ACL；ws-ckpt 自己不做 ACL |

---

## 前提条件

- Linux 内核 ≥ 5.x，支持 btrfs subvolume / snapshot / send-receive
- `btrfs-progs`（独立可执行进程调用，不静态链接，见 `LICENSE` 附注）
- 支持systemd（生产路径）启动`daemon`
- CLI / Plugin 不需要 root，只需对 `/run/ws-ckpt/ws-ckpt.sock` 有读写权限（socket 文件 0o666）
- BtrfsBase 后端：宿主机器必须有 btrfs 格式磁盘
- BtrfsLoop 后端：宿主在 `/var/lib/ws-ckpt` 有足够空间放镜像（target = `min(30 GB, 宿主总空间 × 40%)`,target 超过当前可用空间时降级为 `可用空间 × 40%`）

# Guarded Checkpoint V2

[English](guarded-checkpoint-v2.md)

Guarded Checkpoint V2 是供受治理调用方使用的追加式协议，不改变 legacy `Checkpoint`
行为。legacy 请求仍会执行 lazy bootstrap 与 auto-init；V2 的三个新请求只接受已经登记的
workspace identity，且不会 canonicalize、初始化、收养、修复或重新登记调用方提供的路径。

## 协议流程

1. `WorkspaceIdentityV2` 用登记路径的原始字符串做精确查询，返回 `ws_id`、daemon 保存的
   登记路径和 opaque generation token。查询还会在当下确认登记路径解析到
   `backend.data_root()/ws_id`；它不触发 backend 写操作。
2. `GuardedCheckpointV2` 只接受规范的 `ws-<6hex>[-N]` workspace ID，并在 per-workspace
   lifecycle lock 与 workspace write lock 下重新读取 live generation。token 不匹配或
   workspace 已经 unregister 时，在调用 snapshot backend 前拒绝。
3. `CheckpointEvidenceV2` 按 workspace、generation、checkpoint ID、operation digest 与
   Unix peer UID 精确查询持久化 evidence。查询不会比较当前 live generation，因为 rollback
   以后历史 checkpoint evidence 仍应保持可查询。

## Generation identity

daemon 从同一个以 `O_NOFOLLOW` 打开的 live-subvolume file descriptor 读取
`BTRFS_IOC_FS_INFO.fsid` 与 `BTRFS_IOC_GET_SUBVOL_INFO.uuid`，再以 V2 domain separator
做 SHA-256。FSID 区分不同 Btrfs 文件系统，subvolume UUID 区分同一文件系统中 rollback 后
替换出的 writable subvolume。ioctl、全零 identity、非 subvolume root 或非 Linux 环境一律
fail closed；没有 device/inode fallback。

该 token 标识 writable-subvolume generation，不标识 subvolume 内每一次普通内容写入。

登记路径可由 workspace owner 替换，因此它只是 daemon 保存的 user-facing representation。
V2 在 identity 与 create 前执行 point-in-time symlink mapping check。空 workspace 判断与实际
snapshot 一律使用内部 `data_root/ws_id`，不会通过可替换的登记路径读取另一个对象。
generation check 与 backend create 在同一组 daemon lock 下作用于内部 live subvolume。

## Guarded create 与 evidence

Guarded create 在 backend 前完成格式、登记、generation、ID 冲突、peer credential、
quiescence 与 metadata 检查。成功创建 snapshot 后，daemon 把 snapshot metadata 和 evidence
克隆到下一版 `SnapshotIndex`，对文件内容、rename 和父目录分别执行严格持久化，再发布到
内存。空 workspace 的 `Skipped` evidence 采用相同的 save-before-publish 顺序。

evidence 中的 `caller_uid` 来自 listener 读取的 `SO_PEERCRED`。它用于 receipt 归属，不是
daemon UID allowlist；socket 保持既有 `0o666` authority model。

同一 checkpoint ID 在 evidence 保留期间不可复用。guarded exact duplicate 只返回原
evidence，不会再次调用 backend。legacy `Checkpoint` 也拒绝复用被保留的 ID，避免 cleanup
后用相同 ID 创建的新 subvolume 被旧 evidence 错误认领。受治理客户端在写请求或响应不完整
时必须通过 `CheckpointEvidenceV2` reconcile，不应重放 create request 探测结果。

## 保留与持久化

每个 workspace 最多保留 256 条 guarded evidence。达到上限时，daemon 可以淘汰 `Skipped`
记录或 index metadata 已明确为 `missing` 的 `Created` 记录。被淘汰的记录之后查询为 Unknown，
且 ID 可以重新使用。cleanup 或 delete 成功时，只有 backend 确认对应 snapshot 已删除后才会
移除 evidence。临时从 index detach、但 backend 尚未确认删除的记录不可淘汰。

如果 256 条记录全部对应仍可能存在的 `Created` snapshot，新请求在 backend 前以
`EvidenceCapacityReached` 拒绝。该上限避免 world-writable socket 被用于无限放大
root-owned index；可见 snapshot 本身仍受既有 cleanup 与 pin policy 管理。

`GuardedCheckpointV2Rejected` 表示 backend 未运行，因此 checkpoint 确定没有副作用。
backend 开始执行后发生的错误使用 legacy `Response::Error`，调用方必须视为 uncertain 并查询
evidence。只有精确 evidence 存在，且 Created snapshot metadata 存在并满足
`missing == false` 时，Created 才是确定结论；其他结果均为 Unknown。

C0 没有 prepared journal 或永久 receipt ledger，所以 backend 成功与 evidence 持久化之间
的进程崩溃可能永久留下 Unknown。严格 parent-directory fsync 也可能在 rename 成功后报错，
因此重启前后的 evidence 可能不同。guarded save 使用严格 parent-directory fsync；所有
legacy index writer 也至少执行 temp-file fsync、rename 和 best-effort parent fsync。因此后续
legacy checkpoint、rollback、cleanup、delete 或正常关机不会把已确认的 evidence 重写成
未同步文件。

## 兼容性与验证

新 Request/Response variant 只追加在 bincode enum 末尾，所有 legacy discriminant 和
legacy `SnapshotMeta` wire layout 保持不变。`SnapshotIndex.governed_evidence` 使用
`#[serde(default)]`，旧 JSON index 读取为空 map。旧 daemon 无法解码 identity request 表示
不支持 V2；调用方不得降级到会 auto-init 的 legacy `Checkpoint`。

本增量只提供 ws-ckpt daemon protocol 与持久化基础，不接入 cosh-ng Gateway/Runtime，也不
解除 checkpoint provider authority 的准入限制。单元测试覆盖 Btrfs ioctl ABI、固定 hash
vector 和非 Btrfs fail-closed 行为。

pre-review revision `d9b704d7` 还在特权 x86_64 Linux ECS 上使用 private mount namespace
和临时 1 GiB device-mapper-backed Btrfs 完成实测。该实测覆盖真实 ioctl identity、stale
generation 在 backend 前拒绝、guarded create、duplicate idempotency、exact evidence、真实
writable-subvolume rollback 后 generation 改变、旧 generation 拒绝、daemon 重启后 evidence
reconcile，以及 caller UID mismatch 拒绝；临时设备、文件系统、socket 和运行状态均已清理。

尚未验证 power-loss injection、rollback 中途 kill 的 crash recovery、release build 或人工
Terminal。离线复制出 FSID 与 subvolume UUID 都相同的文件系统镜像不在本协议的 identity
threat model 内。

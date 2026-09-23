# 工作区路径身份与恢复确认

[English](workspace-path-identity.md)

## 注册不变量

每个工作区只有一个 ID 和一个用户可见的注册锚点。规范化锚点的父目录，遵循文件系统
的 symlink 和 `..` 语义，但不跟随最后一级工作区链接。注册、持久化重建、查询与冲突
检测使用同一规范化函数。对于悬空注册，可以保留缺失的普通父目录组件；断开的父目录
symlink 和无法解析的 `..` 会被拒绝。根目录和后端内部路径不能作为注册锚点。

两张注册表在同一个短时锁内发布。重复 ID 或规范化路径在任何映射改变之前就会失败。
注册表锁不会跨越异步操作或工作区锁。持久化记录中的冲突或内部锚点会明确报错，
不会静默选择其中一个所有者。

ID 或注册锚点优先于链接的当前目标。checkpoint、rollback 和工作区 policy 请求要求
注册仍然有效；recover 允许悬空注册。全局 status 保持只读并可列出悬空工作区；指定
工作区的 status 保留原有的悬空错误。Guarded V2 identity 仍要求精确注册路径。

重新接管只接受规范化后位于后端 data root 下的直接子项。若已有 index，使用其中记录的
用户锚点，并要求它仍解析到该 live root。没有 index 时，必须同时有用户链接和匹配的
快照目录。无法证明用户锚点的内部路径会被拒绝。

## 恢复协议

`RecoverPreview { workspace }` 在 daemon 内解析请求，并返回
`RecoverPreviewOk { preview }`。`RecoveryPreview` 包含 `ws_id`（孤立备份恢复时为空）、
`registration_path`、`snapshot_count` 和不透明的 `confirmation_digest`。快照数量覆盖
后端实际选择删除的目录，包括未记录在索引中的快照。

CLI 展示该结果并发送 `RecoverConfirmed { preview }`。daemon 使用返回的 ID，
不再重新解析原始别名，然后在初始化锁与工作区变更锁内重新计算预览。
若目标、索引快照集合或实际删除集合发生变化，需要重新确认。孤立备份预览绑定备份本身，
删除数量为零：该恢复会保留迁移存储和快照。`--force` 也使用相同预览与校验，
只跳过交互提示。

新增 bincode 枚举变体追加在末尾，原有请求和响应编号保持不变。旧客户端仍可使用
`Recover`。新版 CLI 要求 daemon 支持预览，不支持时明确失败，不退回未经校验的恢复。
升级或回滚时应同步操作 CLI 和 daemon。

## 恢复快照来源与删除

从文件系统重建元数据丢失的快照时，将其设为 pinned，并把 ID 记录到索引的
`recovered_orphans` 集合。旧索引默认使用空集合。来源记录在 checkpoint 写入和重启后
保留，随对应快照记录一起删除。Pinned 机制使恢复快照同时避开 Count 和 Age 清理，
不推测它们原来的保护策略。

`ListOrphans { workspace }` 追加为请求编号 28，复用现有 `ListOk` 响应，
按恢复来源而非 pinned 状态过滤。`SnapshotMeta`、旧请求编号和响应布局保持不变。
CLI 将其暴露为 `list --orphans`；旧 daemon 不支持该请求。旧版本写索引时可能丢弃
来源记录，但仍会保留已有的 pinned 标记。

Delete 仅解析完整 ID，全局查询和强制请求同样如此。全局存在重复 ID 时必须指定
工作区。不存在的 ID 不会被重新解释为其他快照的前缀；rollback 和 diff 保留前缀规则。

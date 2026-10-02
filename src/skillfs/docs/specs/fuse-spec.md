# skillfs-fuse Specification

**Crate**: `skillfs-fuse`
**Version**: 0.1.0
**Status**: Current implementation snapshot; see capability record for latest support matrix

---

## 1. Overview

`skillfs-fuse` 把 `skillfs-core` 中的技能集合暴露为 FUSE 文件系统。

实现重点：

- `/skills` 中显示 primary view 技能。
- 永远显示 `skill-discover`。
- 读取 `SKILL.md` 时动态执行编译。
- 透传 skill 目录中的其他真实文件和子目录。
- 支持物理写透传，并把 `SKILL.md` 变化同步回 store。

文件系统能力与 POSIX 兼容状态见：

- [`docs/skillfs-filesystem-capability-record.md`](../skillfs-filesystem-capability-record.md)
- [`docs/specs/posix-phase1-spec.md`](posix-phase1-spec.md)
- [`docs/testing/posix-phase1-acceptance.md`](../testing/posix-phase1-acceptance.md)

---

## 2. Public API

```rust
pub fn mount(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
) -> Result<(), FuseError>;

pub fn mount_background(
    mountpoint: &Path,
    source: &Path,
    store: SharedSkillStore,
    options: MountOptions,
    in_place: bool,
) -> Result<MountHandle, FuseError>;
```

`MountOptions` 和 `MountHandle` 维持代码中的结构与语义。

---

## 3. Filesystem Layout

普通 mount 模式：

```text
/mountpoint/
└── skills/
    ├── skill-a/
    │   ├── SKILL.md
    │   └── <physical files...>
    ├── skill-b/
    └── skill-discover/
        └── SKILL.md
```

in-place mount 模式：

```text
/source-as-mountpoint/
├── skill-a/
├── skill-b/
└── skill-discover/
```

说明：

- `skill-discover/SKILL.md` 是虚拟技能，用于列出 secondary views。
- 真实 skill 目录中的 `SKILL.md` 会在读取时动态编译。
- 其他物理文件和目录直接透传。
- in-place 模式下根目录就是技能目录，不再有 `/skills` 前缀。

---

## 4. Behavior Contracts

### 4.1 Read Path

- `readdir` 根目录显示 `/skills`，in-place 模式下根目录本身就是技能目录。
- `/skills` 显示默认 view 技能和 `skill-discover`。
- 真实技能目录内永远暴露 `SKILL.md`。
- 除 `SKILL.md` 外，技能目录中的其他物理文件和目录直接枚举并读取。
- 读取 `SKILL.md` 返回编译结果，而不是原始 frontmatter 文件内容。

### 4.2 `skill-discover`

- 当存在 secondary views 时，`skill-discover/SKILL.md` 会列出这些 view 中的技能。
- 列表里包含 `source_path`，供上层 agent 直接打开技能。
- 默认输出物理 source path。设置 `--skill-discover-root` 后，输出改为
  `<root>/<skill-name>/SKILL.md`，用于只共享 FUSE view 的独立 mount namespace。
- Secondary skill 不出现在 `/skills` 的目录枚举中，但配置为 view root 的路径
  仍可直接 lookup 和读取。
- 当没有 views 配置时，`skill-discover` 退化为简单的全部技能列表。

### 4.3 Write Path

- `write` / `create` / `mkdir` / `unlink` / `rmdir` / `rename` / `setattr(size)` 会透传到底层物理文件系统。
- `SKILL.md` 的 `create` 与 `write` 会发送 `SyncEvent::Reparse` 给后台 worker。
- `mkdir` skill 目录时会立即写入 degraded placeholder，保证 skill 在 `readdir` 中立即可见。
- `rename` skill 目录时会同步更新 store，使旧名立即消失、新名立即出现。
- 后台 worker reparse 时会强制使用目录名覆盖 `entry.metadata.name`，保证 stale frontmatter 不会把旧 key 写回 store。

### 4.4 Rejected Operations

以下操作仍统一返回 `EROFS`：

- `mknod`
- `symlink`
- `link`

### 4.5 POSIX Phase 1 Planned Behavior

Phase 1 计划补齐默认开启前最容易影响常用工具的一组 POSIX 语义。该小节描述计划目标，不代表当前代码已经全部实现。

计划新增或强化：

- fd-backed handle table，用真实文件描述符支撑 passthrough read/write/flush/fsync/release。
- `open` / `create` flags：
  - `O_RDONLY`
  - `O_WRONLY`
  - `O_RDWR`
  - `O_CREAT`
  - `O_EXCL`
  - `O_TRUNC`
  - `O_APPEND`
  - `O_DIRECTORY`
  - `O_NOFOLLOW`
- passthrough file offset read/write，不再为普通文件 read 全量读入内存。
- `flush`、`fsync`、`fsyncdir`。
- `statfs` 透传底层 source filesystem 的非零统计信息。
- `access` 支持 `F_OK` / `R_OK` / `W_OK` / `X_OK`。
- `setattr` 支持 mode、uid、gid、atime、mtime、size。
- `opendir` / `readdir` / `releasedir` 使用稳定目录快照。
- rename flags 不再静默忽略；未知或不支持的 flag 必须明确失败。
- errno 尽量保留底层 syscall 返回值，避免不必要地折叠为 `EIO`。

Phase 1 非目标：

- `symlink` / `readlink`
- `link`
- xattr
- special files / FIFO / device nodes
- `copy_file_range`
- `fallocate`
- watcher 接入
- `skillfs-views.toml` 热重载

### 4.6 POSIX Phase 1 Compatibility Boundaries

即使 Phase 1 完成，以下仍是 SkillFS 的产品语义，不追求和普通目录完全一致：

- `SKILL.md` 读取返回编译结果，而不是原始 source 文件。
- `SKILL.md` 写入仍落到底层 source 文件，并触发 store reparse。
- `skill-discover` 是只读虚拟 skill。
- 根目录和 `/skills` 是 view 驱动的虚拟目录。
- in-place mount 的底层访问路径当前仍依赖 Linux `/proc/self/fd/{n}`。

---

## 5. Internal Notes

- `EnvironmentProfile::detect()` 在 FUSE 启动时构建一次，用于 `compiler::compile`。
- in-place mount 会预打开 source dir fd，并通过 `/proc/self/fd/{n}` 避免 over-mount 自回环。
- sync worker 以 50 ms debounce 批量处理 `SKILL.md` reparse。
- store 的权威 key 是目录名，而不是 frontmatter 中可能滞后的 `name:` 字段。
- `mount_background` 主要用于测试。

---

## 6. Validation Baseline

已验证：

- `cargo test -p skillfs-fuse` 通过。
- `cargo check -p skillfs -p skillfs-fuse` 通过。

如果修改了公开挂载接口、路径解析逻辑或 store 同步逻辑，建议至少重跑这两条验证。

Phase 1 完成后，额外要求：

- `cargo test -p skillfs-fuse --test write_guard_tests`
- `cargo test -p skillfs-fuse --test posix_phase1_tests`
- 长期能力记录和 `POSIX_FS_TEST_MATRIX.csv` 中列出的已支持项有测试覆盖
  或明确延期说明。

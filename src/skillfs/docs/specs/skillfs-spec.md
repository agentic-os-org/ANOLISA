# SkillFS Architecture

**Version**: 2026-05 workspace state
**Status**: Implementation snapshot

---

## 1. Scope

workspace 当前产品面由三部分组成：

- `skillfs-core`
- `skillfs-fuse`
- `skillfs` CLI

SkillFS 当前已经从“纯只读 FUSE 视图”演进为“虚拟技能视图 + 物理写透传 + store 同步”的混合模型。

---

## 2. High-Level Architecture

```text
physical skills directory
  ├─ skill-a/SKILL.md
  ├─ skill-b/SKILL.md
  └─ category-x/skill-c/SKILL.md
            │
            ▼
      skillfs-core
        - parser
        - store
        - views
        - compiler
        - env
        - watcher (retained, not wired)
            │
            ▼
      skillfs-fuse
        - virtual readdir view
        - compiled SKILL.md read path
        - physical I/O passthrough
        - store sync worker
            │
            ▼
         skillfs CLI
        - mount
        - classify
        - validate
        - list
```

---

## 3. Runtime Data Flow

### 3.1 Load

1. CLI 根据 source 目录构建 `SkillStore`。
2. `SkillStore::load_from_directory` 解析所有 `SKILL.md`。
3. 如存在分类目录，读取 `_category.yaml` 的基本元数据。
4. 如存在 `skillfs-views.toml`，FUSE 启动时加载 views 配置。

### 3.2 Mount

1. CLI 调用 `skillfs-fuse::mount`。
2. FUSE 根据 views 决定 `/skills` 或 in-place 根目录中哪些技能可见。
3. `skill-discover` 始终可见，用于暴露 secondary views。
4. in-place 模式会预打开 source dir fd，并通过 `/proc/self/fd/{n}` 访问底层真实目录。
5. `--skill-discover-root` 可将 discover 路径映射到 reader 可见的 FUSE view；
   未设置时保留物理 source path。

### 3.3 Read

1. Agent 或用户读取 `<skill>/SKILL.md`。
2. FUSE 读取真实 `SKILL.md`。
3. `compiler::compile` 基于 OS、命令和环境变量进行编译。
4. 返回编译后的内容。

### 3.4 Write / Sync

1. 用户通过挂载点执行 `write`、`create`、`mkdir`、`rename`、`unlink`、`rmdir` 或 `truncate`。
2. FUSE 将物理 I/O 透传到 source 目录。
3. 涉及 skill 目录结构变化时，FUSE 立即更新 store 或写入 degraded placeholder，保证可见性立即收敛。
4. 涉及 `SKILL.md` 内容变化时，后台 sync worker 重新解析文件并 `upsert` 到 store。
5. sync worker 统一使用目录名覆盖 `metadata.name`，保证目录名始终是 store 的权威 key。

### 3.5 Write -> Store -> Views Mapping

```mermaid
flowchart TD
  A[User writes through mountpoint] --> B{Operation type}

  B -->|write/create SKILL.md| C[Physical file updated]
  C --> D[SyncEvent::Reparse skill_name]
  D --> E[Background sync worker]
  E --> F[parse SKILL.md from physical path]
  F --> G[Force entry.metadata.name = directory name]
  G --> H[store.upsert entry]

  B -->|mkdir skill dir| I[Physical directory created]
  I --> J[Insert degraded placeholder into store]

  B -->|rename skill dir| K[Physical rename]
  K --> L[Remove old store key]
  L --> M[Parse new path or insert placeholder]
  M --> N[store.upsert under new directory name]

  B -->|unlink SKILL.md / rmdir skill dir| O[Physical delete]
  O --> P[store.remove directory name]

  H --> Q{views_config snapshot}
  J --> Q
  N --> Q
  P --> Q

  Q -->|name in default view| R[Visible in primary view readdir]
  Q -->|name in secondary view| S[Visible in skill-discover]
  Q -->|name in no configured view| R
```

这张图对应当前实现的几个关键语义：

- 写操作先修改物理文件系统，再决定是否同步 store。
- 只有 `SKILL.md` 和 skill 目录结构变化会影响 store；普通透传文件不会改变视图。
- store 更新后不会自动修改或热重载 `skillfs-views.toml`；运行时仍使用挂载时加载的配置快照。
- 主视图成员由 `effective_default_skills` 在内存中计算：store 中仍存在的显式 default 成员，加上没有分配到任何 view 的成员。未分配成员可在 `/skills` 中看到，不会因此写入 TOML。
- 仅分配到 secondary views 的成员不进入主视图；`skill-discover` 按 secondary 名单与当前 store 的交集展示，而不是创建独立视图目录。
- 同时列入 default 和 secondary 的成员可以在两处展示；不能把“未分配”与“仅分配到 secondary”合称为隐藏成员。
- rename 后即使 frontmatter `name:` 仍是旧值，store 也只认目录名，不会把旧 key 写回。

---

## 4. Configuration Model

运行时配置使用 `skillfs-views.toml`。

它决定：

- 默认 view 技能集合。
- secondary views 技能集合。
- `skill-discover` 展示内容。
- 可选的 `--skill-discover-root` 决定 discover 输出物理 source path，还是
  reader 可见的 FUSE view path。

注意：当前 store 同步不会自动修改或热重载 `skillfs-views.toml`；views 仍然是挂载时加载的配置快照。

---

## 5. Functional Matrix

| 能力 | normal mount | in-place mount | 当前状态 |
|------|--------------|----------------|----------|
| 视图过滤 `readdir` | 是 | 是 | 已实现 |
| 编译读取 `SKILL.md` | 是 | 是 | 已实现 |
| 读取其他物理文件 | 是 | 是 | 已实现 |
| 写 `SKILL.md` | 是 | 是 | 已实现，触发 reparse |
| `mkdir` 新 skill 立即可见 | 是 | 是 | 已实现 |
| `rename` skill 无空窗 | 是 | 是 | 已实现 |
| rename 后 stale frontmatter 不复活旧名 | 是 | 是 | 已实现 |
| `unlink` / `rmdir` 移除 store 条目 | 是 | 是 | 已实现 |
| `setattr(size)` truncate | 是 | 是 | 已实现 |
| `mknod` / `symlink` / `link` | `EROFS` | `EROFS` | 已实现 |

---

## 6. Runtime Consistency Boundaries

### 6.1 写路径一致性

- 通过挂载点执行的写操作会进入 FUSE 回调链并同步更新物理层与 store。
- 通过挂载点修改 `SKILL.md` 时，读取结果可在后续请求中反映最新编译内容。

### 6.2 视图配置一致性

- 挂载启动和挂载期 FUSE 回调均不会重写 `skillfs-views.toml`；未分配技能的默认成员关系在内存中计算。
- 因此可能出现「views 名单与真实目录状态漂移」（例如重命名或删除后，toml 仍保留旧 skill 名）。
- 该漂移不会破坏 toml 文件格式。

### 6.3 normal 与 in-place 的关键差异

| 场景 | in-place (`source == mountpoint`) | normal (`source != mountpoint`) |
|------|-----------------------------------|----------------------------------|
| 通过挂载点修改 | 即时生效 | 即时生效 |
| 直接改 source（绕过挂载点） | 与挂载点同入口，通常等价 | 不保证即时生效 |

说明：normal 模式下，问题不在「是否挂到别处」，而在「是否绕过挂载点」。

- 通过挂载点路径进行读写：normal 与 in-place 都生效。
- 直接修改 source（绕过挂载点）：normal 下不保证即时同步到展示层与 store。

### 6.4 「可能需要重挂载」的判定规则

该描述用于覆盖不同变更类型的差异行为：

- 文件内容变更（已有 `SKILL.md`）：多数场景可直接反映。
- 目录结构/视图归属变更（新增、删除、重命名 skill，或修改 views）：通常需要重挂载才能稳定反映到展示层。
- normal 模式下绕过挂载点直接修改 source：可能需要重挂载或后续触发路径才能达成一致状态。

#### 6.4.1 路径可映射性说明

FUSE 写回调只会把可解析为技能路径的请求映射到底层 source。路径判定由 `parse_path` 与 `resolve_physical_path` 决定。

可映射路径：

- `SkillDir`
	- normal：`/mountpoint/skills/<skill>`
	- in-place：`/source/<skill>`
- `SkillMd`
	- normal：`/mountpoint/skills/<skill>/SKILL.md`
	- in-place：`/source/<skill>/SKILL.md`
- `Passthrough`
	- normal：`/mountpoint/skills/<skill>/<subpath>`
	- in-place：`/source/<skill>/<subpath>`

不可映射路径：

- `Root`
	- normal：`/mountpoint`
	- in-place：`/source`
- `SkillsDir`（仅 normal）
	- `/mountpoint/skills`
- `Invalid`
	- 不能解析为技能路径的请求，例如 normal 下 `mountpoint` 根目录下的非 `skills` 前缀路径。

不可映射路径的写操作不会落到底层 source，通常返回拒绝错误（常见为 `EROFS`）。

---

## 7. Scenario Comparison

### 7.1 挂载模式对比

「挂载模式」用于回答同一变更在 normal / in-place 下是否存在行为差异。

| 变更入口与类型 | in-place (`source == mountpoint`) | normal (`source != mountpoint`) |
|----------------|-----------------------------------|----------------------------------|
| 通过挂载点修改 `SKILL.md` | 回调触发；reparse 更新 store | 回调触发；reparse 更新 store |
| 通过挂载点 `mkdir/create/unlink/rmdir/rename` | 回调触发；同步物理层与 store | 回调触发；同步物理层与 store |
| 直接改 source 中已有 `SKILL.md`（绕过挂载点） | 与挂载点同入口 | 无回调；内容读取可能更新，store 不保证同步 |
| 直接改 source 目录结构（新增/删除/重命名 skill） | 与挂载点同入口 | 无回调；`/skills` 与 store 可能短时陈旧 |
| 挂载期间修改 `skillfs-views.toml` | 不热重载；重挂载生效 | 不热重载；重挂载生效 |

场景说明：

1. 通过挂载点修改：进入 FUSE 回调链，数据与展示通常同步。
2. normal 下绕过挂载点改 source 文件内容：可能出现「文件内容已变，列表/元数据未变」。
3. normal 下绕过挂载点改目录结构：常见「新目录已存在但列表未出现」或「旧目录已删但列表短时仍可见」。
4. 修改 views 文件：运行中不重载，重挂载后按新配置展示。

### 7.2 显式默认、次级分配与未分配对比

以下按挂载时的配置快照和当前 store 区分三类目录名：

- **显式默认**：配置的 default view 包含该名称；仅 store 中仍存在的成员可见。
- **仅次级分配**：至少一个 secondary view 包含该名称，default view 不包含；通过 `skill-discover` 展示，不进入 `/skills` 主列表。
- **未分配**：任何 view 都不包含该名称；只要在 store 中，就由 `effective_default_skills` 纳入主视图，不需要持久化自动分配。

| 变更类型 | 名称的配置归属 | 物理文件系统 | store | `/skills` 主列表 | secondary `skill-discover` | `skillfs-views.toml` |
|----------|----------------|--------------|-------|------------------|---------------------------|----------------------|
| 修改 `SKILL.md` 内容 | 显式默认 | 更新 | reparse 后更新 | 继续可见 | 如同时分配到 secondary，则同步展示新元数据 | 不变 |
| 修改 `SKILL.md` 内容 | 仅次级分配 | 更新 | reparse 后更新 | 不可见 | 展示新元数据 | 不变 |
| 修改 `SKILL.md` 内容 | 未分配 | 更新 | reparse 后更新 | 继续可见 | 不加入 secondary 名单 | 不变 |
| `mkdir` skill 目录 / 创建新 `SKILL.md` | 显式默认 | 创建或更新 | placeholder / reparse 后入库 | 入库后可见 | 如同时分配到 secondary，则可展示 | 不变 |
| `mkdir` skill 目录 / 创建新 `SKILL.md` | 仅次级分配 | 创建或更新 | placeholder / reparse 后入库 | 不可见 | 入库后可展示 | 不变 |
| `mkdir` skill 目录 / 创建新 `SKILL.md` | 未分配 | 创建或更新 | placeholder / reparse 后入库 | 入库后可见 | 不加入 secondary 名单 | 不变 |
| `unlink SKILL.md` / `rmdir` skill 目录 | 显式默认 | 删除 | 立即 remove | 旧成员消失 | 对应条目消失（若原先有次级分配） | 不变，可能保留旧名 |
| `unlink SKILL.md` / `rmdir` skill 目录 | 仅次级分配 | 删除 | 立即 remove | 不新增成员 | 对应条目消失 | 不变，可能保留旧名 |
| `unlink SKILL.md` / `rmdir` skill 目录 | 未分配 | 删除 | 立即 remove | 旧成员消失 | 无对应次级条目 | 不变 |
| `rename` skill 目录 | 新名称为显式默认 | 改名 | 删除旧 key，以新目录名入库 | 旧名消失；新名可见 | 按新名称的 secondary 分配展示 | 不变，可能保留旧名 |
| `rename` skill 目录 | 新名称仅次级分配 | 改名 | 删除旧 key，以新目录名入库 | 旧名消失；新名不进入主列表 | 按新名称的 secondary 分配展示 | 不变，可能保留旧名 |
| `rename` skill 目录 | 新名称未分配 | 改名 | 删除旧 key，以新目录名入库 | 旧名消失；新名在内存中进入主视图 | 不加入 secondary 名单 | 不变，可能保留旧名 |

`rename` 后按**新目录名**重新计算归属，旧目录名的配置分配不会自动迁移。例如仅次级分配的 skill 改为一个未分配名称后，新成员会在主视图可见，但 TOML 仍保留旧分配。

本表的 discover 列针对存在 secondary views 的配置。没有 secondary views 时，`skill-discover` 回退为 store 的简单列表；无配置时主视图展示全部 store 成员。若变更入口绕过挂载点，仍需同时参考 7.1 的挂载模式差异。

---

## 8. Test Coverage

`scripts/test.sh` 的挂载 smoke 同时配置显式默认的 `primary-skill`、仅次级分配的 `secondary-skill` / `tertiary-skill`，以及没有持久化分配的 `unassigned-skill`。它验证 `unassigned-skill` 在 `/skills` 可见、次级成员不在主列表，并对比挂载前后 `skillfs-views.toml` 的字节完全相同。

`crates/skillfs-core/src/views.rs` 的 `effective_default_includes_unassigned_without_changing_views` 回归还验证内存默认成员计算不会改变配置对象，包括没有显式 default view 的场景。

当前关键验证集中在 `crates/skillfs-fuse/tests/write_guard_tests.rs`：

- normal mount
  - read path smoke test
  - write passthrough smoke test
  - `mkdir` 立即可见
  - `rename` 无空窗
  - post-rename write 不复活旧名
- in-place mount
  - `mkdir` 立即可见
  - `rename` 无空窗
  - post-rename write 不复活旧名
- 拒绝操作
  - `mknod` / `symlink` / `link` 返回 `EROFS`

---

## 9. Functional Highlights

- 虚拟视图与物理文件系统解耦：目录列表由 views + store 决定，物理文件仍来自 source。
- `SKILL.md` 读写分离：read 返回编译结果，write 修改原始文件。
- 目录名是统一权威 key：重命名后即使 frontmatter `name:` 滞后，也不会把旧名字重新写回 store。
- in-place 模式通过 dir fd 绕行 FUSE 自身，避免 over-mount 自回环。

---

## 10. Remaining Deferred Work

保留但未接线：

- watcher 模块

如果未来继续扩展，可能的方向是：

- 接入 watcher 做绕过挂载点写入的兜底同步
- 继续收缩分类目录相关元数据
- 为 `skillfs-views.toml` 定义热重载或刷新策略

---

## 11. Validation Baseline

已验证：

- `cargo test -p skillfs-core`
- `cargo test -p skillfs-fuse`
- `cargo check -p skillfs -p skillfs-fuse`

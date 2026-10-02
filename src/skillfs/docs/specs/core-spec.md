# skillfs-core Specification

**Crate**: `skillfs-core`
**Version**: 0.1.0
**Status**: Implementation

---

## 1. Overview

`skillfs-core` 是 SkillFS 的共享基础库，负责：

- `SKILL.md` 解析。
- 技能目录扫描与内存存储。
- `skillfs-views.toml` 视图配置读写。
- 条件编译与命令归一化。
- 环境探测（仅 OS / commands / env vars）。
- watcher 模块保留，但尚未接入运行时主流程。

---

## 2. Public Data Structures

```rust
pub struct SkillEntry {
    pub metadata: SkillMetadata,
    pub parameters: Vec<Parameter>,
    pub returns: Vec<ReturnField>,
    pub body: String,
    pub parse_status: ParseStatus,
    pub source_path: PathBuf,
    pub last_modified: SystemTime,
}

pub struct SkillMetadata {
    pub name: String,
    pub description: String,
    pub version: String,
    pub tags: Vec<String>,
    pub enabled: bool,
    pub requires: Option<SkillRequires>,
}

pub struct SkillRequires {
    pub commands: Vec<String>,
    pub platforms: Vec<String>,
    pub env_vars: Vec<String>,
}

pub struct Parameter {
    pub name: String,
    pub param_type: ParamType,
    pub required: bool,
    pub description: String,
}

pub struct ReturnField {
    pub name: String,
    pub field_type: ParamType,
    pub description: String,
}

pub enum ParseStatus {
    Ok,
    Degraded(String),
    Error(String),
}

pub struct CategoryMeta {
    pub name: String,
    pub description: String,
}

pub struct ParseConfig {
    pub strict: bool,
    pub max_skill_size: usize,
    pub max_skills: usize,
}
```

---

## 3. Parser

代码位置：`src/parser.rs`

公开入口：

```rust
pub fn parse_skill_md(content: &str, dir_name: &str) -> SkillEntry;
pub fn parse_skill_file(path: &Path) -> Result<SkillEntry, ParseError>;
pub fn parse_skill_file_with_limit(path: &Path, max_size: usize) -> Result<SkillEntry, ParseError>;
```

行为要点：

- `parse_skill_md` 永远返回 `SkillEntry`，不会因为 frontmatter 或正文格式问题返回 `Err`。
- 文件 I/O 错误和超大文件由 `ParseError` 表示。
- 缺失 `name` 时回退到目录名。
- 缺失 `description` 时回退到正文首段。
- `## Parameters` 和 `## Returns` 会被解析成结构化列表。
- 解析质量通过 `ParseStatus` 表示。

---

## 4. Store

代码位置：`src/store.rs`

核心能力：

- `load_from_directory`
  - 支持 `{source}/{skill}/SKILL.md`
  - 支持 `{source}/{category}/{skill}/SKILL.md`
- 读取 `_category.yaml` 的 `name` / `description`
- `upsert`
- `remove`
- `get`
- `list`
- `len`
- `split_primary`

---

## 5. Views

代码位置：`src/views.rs`

`skillfs-views.toml` 是技能可见性配置。

能力：

- `ViewsConfig::load`
- `default_view`
- `secondary_views`
- `default_skills`
- `all_assigned_skills`
- `assign_to_default`
- `save`

FUSE 完全依赖这份配置来决定 `/skills` 和 `skill-discover` 的内容。

---

## 6. Compiler

代码位置：`src/compiler.rs`

能力：

- 处理 `@if / @else / @endif` 条件块。
- 支持 `os ==`、`os !=`、`has_command`、`has_env`、`&&`、`||`。
- 当文档中没有条件块时，执行少量命令归一化。

调用方：

- `skillfs-fuse` 在读取 `SKILL.md` 时调用 `compile`。

---

## 7. Env

代码位置：`src/env.rs`

`EnvironmentProfile` 包含：

- `os`
- `available_commands`
- `env_vars`

---

## 8. Watcher

代码位置：`src/watcher.rs`

状态：

- 模块存在。
- 单元/集成测试存在。
- 没有接入 CLI 或 FUSE 主流程。

因此它是“保留但未接线”的候选模块，而不是产品能力的一部分。

---

## 9. Validation Baseline

已验证：

- `cargo test -p skillfs-core` 通过。

如果后续继续调整 `skillfs-core`，建议至少保持这条验证命令持续通过。

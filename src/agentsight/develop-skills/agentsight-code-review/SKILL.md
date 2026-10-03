---
name: agentsight-code-review
description: 对当前分支的变更执行 AgentSight 专属代码审查。覆盖 6 个维度：硬性规则合规、eBPF 安全、FFI 边界、Footprint Ladder、流水线测试覆盖、文档同步。输出编号的 findings 列表，每条包含文件路径和行号。
---

# AgentSight Code Review

## 目标

对当前分支相对于 `main` 的全部变更执行代码审查，输出所有 findings。

## 触发时自动执行

### 步骤 1：收集变更

```bash
git diff origin/main..HEAD
git diff --stat origin/main..HEAD
git log --oneline origin/main..HEAD
```

### 步骤 2：按维度审查

对 diff 中每个变更文件，依次检查以下 5 个维度。**不要在发现第一个问题后停止，必须遍历全部文件和全部维度。**

#### 维度 1：硬性规则合规

对照 `AGENTS.md ## 0. 硬性规则`：

- 非测试代码中是否使用了 `unwrap()` / `expect()` / `dbg!()`
- 是否添加了 `#[allow(clippy::...)]` 但没有注释说明
- 单个模块是否超过 500 行（不含测试），超过 2000 行的文件是否有拆分计划
- PR diff 是否超过 800 行，复杂逻辑变更是否超过 500 行

#### 维度 2：eBPF 安全

仅当 diff 涉及 `src/bpf/` 或 `src/probes/` 时检查：

- BPF 程序是否兼容 kernel >= 5.8（不使用高版本才有的 helper）
- ring buffer 大小是否合理（参考现有探针配置）
- uprobe attach 的符号名是否正确，是否处理了符号不存在的情况
- BPF map 的 key/value 类型是否与 Rust 侧定义一致

#### 维度 3：FFI 边界

仅当 diff 涉及 `src/ffi.rs` 或 `cbindgen.toml` 时检查：

- 新增/修改的 `extern "C"` 函数是否在 `cbindgen.toml` 的 `after_includes` 中同步声明
- FFI 类型是否标注了 `#[repr(C)]`
- 是否有 panic 可能穿越 FFI 边界（缺少 `catch_unwind`）
- 指针参数是否做了 null check

#### 维度 4：Footprint Ladder

对照 `AGENTS.md ## 3. 代码表面增长控制`：

- 新增文件 → 是否可以通过扩展现有模块实现（级别 1-2）
- 新增 eBPF 探针 → 是否附带架构影响说明（级别 4）
- 新增 FFI 导出 → 是否附带架构影响说明（级别 5）

#### 维度 5：流水线测试覆盖

仅当 diff 涉及 `src/parser/`、`src/aggregator/`、`src/analyzer/`、`src/genai/`、`src/storage/` 时检查：

- 流水线逻辑变更是否包含集成测试
- 跨模块行为是否优先用集成测试而非单元测试
- 测试代码是否放在 `*_tests.rs` 或 `#[cfg(test)] mod tests` 中

#### 维度 6：文档同步

检查代码变更是否需要同步更新以下文档（根据变更内容自行判断）：

- `AGENTS.md` — 导航总览（Module Map、CLI、API、eBPF Probes、Configuration 等）
- `CLAUDE.md` — 构建命令、CLI 用法、配置说明
- `src/FFI_AGENTS.md` — FFI 层边界规则
- `src/UNIFIED_AGENTS.md` — 主编排器边界规则
- `src/storage/AGENTS.md` — 存储层边界规则
- `docs/PITFALLS.md` — 常见踩坑记录
- `docs/adr/` — 架构决策记录（涉及架构选型变更时需新增 ADR）
- `docs/ARCHITECTURE.md` — 架构设计文档
- `docs/DEVELOPMENT.md` — 开发指南
- `docs/design-docs/` — 模块设计文档

### 步骤 3：输出 Findings

使用编号列表输出，每条 finding 必须包含：

```
N. [维度名] 文件路径:行号 — 问题描述
   建议：具体修复方式
```

示例：

```
1. [硬性规则] src/storage/sqlite/token.rs:142 — 非测试代码使用了 unwrap()
   建议：改为 .map_err(|e| anyhow::anyhow!("..."))? 或 .unwrap_or_default()

2. [eBPF] src/bpf/gotls.bpf.c:87 — bpf_loop() 需要 kernel >= 5.17，不兼容 5.8
   建议：改用 bounded for 循环

3. [Footprint Ladder] src/newmodule/mod.rs — 新增模块文件（级别 3），未说明为何不能扩展现有模块
   建议：在 PR 描述中补充为什么级别 1-2 不够
```

### 无问题时

如果所有维度检查通过且自动检查全绿，输出：

```
✓ 全部 6 个维度检查通过，未发现问题。
```

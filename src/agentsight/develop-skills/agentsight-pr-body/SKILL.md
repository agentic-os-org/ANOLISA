---
name: pr-body
description: 分析当前分支的全部变更，自动生成或更新 PR 标题和正文。聚焦内容质量：解释 why、归纳 what、标注测试方式，遵循 anolisa 项目 PR 模板规范。适用于新建 PR 前生成描述，或已有 PR 需要更新描述。
---

# PR Body 生成器

## 目标

分析当前分支相对于 `main` 的全部变更（所有 commit，不仅是最新一条），生成或更新符合 `alibaba/anolisa` 规范的 PR 标题和正文。

## 触发时自动执行

### 步骤 1：收集变更信息

```bash
# 当前分支
git branch --show-current

# 全部 commit（从 main 分叉点起）
git log --oneline origin/main..HEAD

# 变更文件列表
git diff --stat origin/main..HEAD

# 完整 diff（用于分析变更内容）
git diff origin/main..HEAD

# 是否已有 PR
gh pr list --head $(git branch --show-current) --repo alibaba/anolisa --state open --json number,title,body
```

### 步骤 1.5：Code Review 自检

在生成 PR 描述前，先执行 `agentsight-code-review` skill 对当前变更进行自检。如果存在 findings，先修复再继续。

### 步骤 1.6：Preflight 检查（与 CI 门禁逐项对齐）

在分析变更前，运行以下检查并记录结果（后续自动填入 Checklist）。这些检查
镜像 `test-agentsight` CI job，目的是在 push 前本地拦截会导致 CI 失败的问题
（一次 push + CI ≈ 4 分钟，本地检查 ≈ 30 秒）。

```bash
# 在 agentsight 目录下执行（CI 锁定 toolchain 1.89.0；缺则先
# `rustup toolchain install 1.89.0 --component rustfmt --component clippy --component llvm-tools-preview`）
cargo +1.89.0 fmt --all --check                     # 1. 格式
cargo +1.89.0 clippy --all-targets -- -D warnings   # 2. lint
python3 scripts/check-arch-boundaries.py            # 3. 架构边界

# 4. 测试 + 覆盖率（CI 用 llvm-cov 跑测试，不是 cargo test；用默认 toolchain 即可——
#    覆盖率行映射与工具链版本无关，且 +1.89.0 需该工具链装 llvm-tools-preview，
#    dev 机常装在 stable 上。fmt/clippy 上面 pin +1.89.0 是因为 lint 规则版本敏感）
cargo llvm-cov --cobertura --output-path coverage.xml \
  --ignore-filename-regex '(\.skel\.rs|target/debug/build|target/release/build|src/probes/)'

# 5. 增量覆盖率门禁（与 CI 一致：对比 origin/main，阈值 80%）
git fetch origin main
diff-cover coverage.xml --compare-branch=origin/main --fail-under=80
```

- 五项全部通过才继续；任一失败按下面处理后重跑。
- `cargo fmt --check` 失败 → 跑 `cargo +1.89.0 fmt` 修复后重新检查。
- `cargo clippy` 失败 → 列出告警、修复、重新检查。
- 架构边界失败 → 按 `check-arch-boundaries.py` 的提示修正跨层依赖。
- **覆盖率门禁失败（增量 < 80%）→ 停止**，为新增/修改但未覆盖的行补测试
  （diff-cover 输出会列出每个文件的 Missing lines）；不要靠降阈值绕过。
- `diff-cover` / `cargo-llvm-cov` 未安装 → 安装后再验（`pip install diff-cover`；
  `rustup component add llvm-tools-preview`），**不要跳过本步却标记"已通过"**。

**6. Commit message 规范检查**：对 `git log origin/main..HEAD` 的每个 commit
逐条核对是否符合 conventional commit（commitlint 是独立的硬门禁，其中 **scope 必填**
是最容易漏的硬失败；fmt/clippy/覆盖率同样是硬门禁）：

- `type(scope): subject` 格式，**scope 必填**（缺 scope = CI 硬失败）。
- type ∈ {feat, fix, refactor, perf, docs, chore, test, ci, build, style, revert}。
- scope 建议用 {cosh, sec-core, skill, sight, tokenless, ckpt, memory, anolisa,
  deps, ci, docs, chore}（不在列内 CI 仅告警、不阻断）。
- header（首行）≤ 120 字符；**body/footer 每行 ≤ 100 字符**；subject 不用 Sentence/Start/Pascal/UPPER case。
- 不符合 → 用 `git rebase` / `git commit --amend` 修正后再继续。

> 可选：把以上检查装成 git **pre-push hook**，对所有人/所有 agent 通用（git 原生
> 机制，谁 push 都触发）。在 agentsight 目录跑 `make install-hooks` 即启用；
> 详见下文「步骤 7：pre-push hook（可选）」。

### 步骤 2：分析变更

从 diff 中提取以下信息：

1. **变更类型**：根据 commit type 和实际改动判断（feat/fix/docs/refactor/perf/test/ci）
2. **影响范围**：涉及哪些模块（对照 AGENTS.md Module Map）
3. **动机（Why）**：从 commit message、关联 issue、代码注释中推断为什么做这个变更
4. **内容（What）**：归纳净变更，忽略开发过程中的反复修改
5. **风险点**：是否涉及 eBPF、FFI、storage schema 等高风险区域
6. **Footprint Ladder 级别**：本次变更属于哪个级别（1-5）

### 步骤 3：生成 PR 标题

格式：`type(sight): 简要描述`

规则：
- 不超过 70 字符
- 用英文，动词开头（add/fix/update/refactor/remove）
- 描述净变更效果，不描述过程

示例：
```
feat(sight): add Go TLS uprobe for plaintext capture
fix(sight): handle qodercli wrapped SSE format
docs(sight): add scoped AGENTS.md for high-risk modules
refactor(sight): extract HTTP2 frame decoder from parser
```

### 步骤 4：生成 PR 正文

使用以下模板，**每个字段必须基于实际 diff 内容填写**，禁止使用占位符：

```markdown
## Description

<1-3 段说明，先写 why（动机），再写 what（做了什么）。>
<如果关联了 issue，说明该 issue 的背景。>

## Related Issue

closes #<issue-number>

## Type of Change

- [ ] Bug fix (non-breaking change that fixes an issue)
- [ ] New feature (non-breaking change that adds functionality)
- [ ] Breaking change (fix or feature that would cause existing functionality to not work as expected)
- [ ] Documentation update
- [ ] Refactoring (no functional change)
- [ ] Performance improvement
- [ ] CI/CD or build changes

## Scope

- [x] `sight` (agentsight)

## Key Changes

<按模块或逻辑分组列出关键变更，每条一行：>
<- `src/parser/sse.rs`: 新增 qodercli SSE 包装格式解析>
<- `src/bpf/gotls.bpf.c`: 新增 Go crypto/tls uprobe>

## Checklist

- [x] I have read the [Contributing Guide](../CONTRIBUTING.md)
- [x/空] `cargo fmt --check` pass（基于 preflight 结果勾选）
- [x/空] `cargo clippy --all-targets -- -D warnings` pass（基于 preflight 结果勾选）
- [x/空] `cargo llvm-cov` 测试 + 增量覆盖率 `diff-cover --fail-under=80` pass（基于 preflight 结果勾选）
- [ ] I have added tests that prove my fix is effective or that my feature works
- [ ] I have updated the documentation accordingly
- [x] Lock files are up to date (`Cargo.lock`)

## Testing

<具体说明如何验证这次变更，不要写"运行了 cargo test"这种泛泛的话。>
<示例：用 qodercli 发起 LLM 请求，确认 agentsight 正确解析 SSE 响应并提取 token 数。>
```

### 步骤 5：预览与确认

将生成的 PR 标题和正文**完整展示**给用户，然后使用 `AskUserQuestion` 工具询问用户下一步操作：

**问题**：「PR 内容已生成，请确认下一步操作？」

**选项**：
1. **创建 Issue 和 PR** — 自动创建关联 Issue（如果尚无关联 Issue），然后创建 PR
2. **仅创建 PR** — 跳过 Issue，直接创建 PR
3. **更新已有 PR** — 将生成的内容更新到当前分支已有的 PR（仅当已有 PR 时显示）
4. **仅输出，不提交** — 只展示内容，不执行任何 GitHub 操作

用户确认后再执行对应操作，**未经确认不得自动提交**。

### 步骤 6：执行操作

根据用户在步骤 5 中的选择执行：

**选择「创建 Issue 和 PR」**：

1. 先创建 Issue：
```bash
gh issue create --repo alibaba/anolisa \
  --title "<从变更中提取的 issue 标题>" \
  --body "<issue 描述：背景、问题、期望>"
```
2. 获取新建 Issue 编号，更新 PR body 中的 `closes #N`
3. 创建 PR：
```bash
gh pr create --repo alibaba/anolisa \
  --title "type(sight): 描述" \
  --body "$(cat <<'EOF'
<生成的 body，包含 closes #N>
EOF
)"
```

**选择「仅创建 PR」**：

```bash
gh pr create --repo alibaba/anolisa \
  --title "type(sight): 描述" \
  --body "$(cat <<'EOF'
<生成的 body>
EOF
)"
```

**选择「更新已有 PR」**：

```bash
gh pr edit <PR-NUMBER> --repo alibaba/anolisa \
  --title "type(sight): 描述" \
  --body "$(cat <<'EOF'
<生成的 body>
EOF
)"
```

更新时注意：
- 保留已有 body 中的图片（不要删除 `![...](...)`）
- 保留人工添加的补充说明
- 只更新自动生成的部分

**选择「仅输出，不提交」**：

不执行任何 GitHub 操作，流程结束。

### 步骤 7：pre-push hook（可选，对所有人/所有 agent 通用）

步骤 1.6 的检查也可装成 git **pre-push hook**——git 原生机制，无论人还是任何
AI agent `git push` 都会触发，比 skill 多一层兜底（防止漏跑 skill 直接 push）。

**启用**（opt-in，不影响他人；卸载用 `make uninstall-hooks`）：

```bash
cd src/agentsight
make install-hooks
```

**行为**：

- 仅当本次 push 的 commit 改动了 `src/agentsight/` 时才检查，否则直接放行
  （monorepo 好公民，不干扰其他组件）。
- 默认跑快检查：`cargo +1.89.0 fmt --check`、`cargo +1.89.0 clippy`、架构边界检查、
  以及 commit message 规范（conventional commit，scope 必填，跳过 Merge/Revert/fixup! 等）。
- 覆盖率门禁（`llvm-cov` + `diff-cover`）较慢（约 1 分钟），**默认跳过**；需要时用
  `PREPUSH_COVERAGE=1 git push` 启用。

**注意（monorepo 限制）**：`core.hooksPath` 是单值的。若你也在用 copilot-shell 的
husky（它把 hooksPath 指向 `.husky/`），两者只能启用其一；`make install-hooks`
检测到已有 hooksPath 会**警告而不强行覆盖**。统一的多组件 hook 调度不在本 skill 范围。

## 内容质量规则

1. **先写 Why 再写 What**：Description 第一段必须是动机，不是"本 PR 做了 X"
2. **描述净变更**：多次 commit 中的反复修改只看最终结果，不提开发过程
3. **具体到文件**：Key Changes 每条必须包含文件路径
4. **不说废话**：不要写 "improved code quality"、"various improvements" 等空泛描述
5. **Testing 要具体**：描述验证场景和预期结果，不要只写 "passed all tests"
6. **禁止泄露**：不包含本地绝对路径、内部 URL、密钥等敏感信息
7. **Issue 关联**：如果 commit message 中提到 issue 编号，自动填入 `closes #N`；无关联 issue 则写 `no-issue: <原因>`

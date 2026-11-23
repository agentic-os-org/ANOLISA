# Roadmap、Issue 与认领流程

[English](../en/workflow.md)

本文档说明候选方向如何从 GitHub Discussions 流转到可认领的 Issue、如何使用 `/claim` 认领社区任务，以及分拣与评审阶段的协作预期。自动化机器人与幂等标记的技术细节见 [`.github/TRIAGE.md`](../../.github/TRIAGE.md)。

## 1. 从 Community Roadmap 到 Issue

各组件维护者会在 GitHub Discussions 的 [Community Roadmap](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap) 中发布候选方向与重点征集议题。正式动工前，需在对应的 Issue 中确认范围并完成认领。

如果你想实现其中的某个方向：

1. 先检索开放的 Issue，确认是否已有覆盖该方向的条目。
2. 若尚无对应 Issue，请新建 Issue 并关联原 Discussion，说明你计划实现的范围。
3. 编写代码前，与组件维护者确认当前版本、边界与验收标准。

对于不涉及 Roadmap 的缺陷修复或局部改进，可直接按对应 Issue 模板提交。仅在改动极其微小时（例如纯文档拼写修正），才可在 PR 描述中使用 `no-issue: <原因>` 跳过 Issue。

## 2. 理解 Issue 标签

ANOLISA 将组件归属、工作流状态与社区可认领标识拆开维护：

| 标签 | 含义 |
| :--- | :--- |
| `component:<name>` | 标识该 Issue 归属的组件。 |
| `status:needs-triage` | 新提交的 Issue，等待首次归类与维护者响应。 |
| `status:needs-info` | 维护者需要提问者补充关键信息或复现步骤。 |
| `status:accepted` | 已确认在项目范围内，可以开始实现。 |
| `status:in-progress` | 已被认领并正在推进中。 |
| `status:blocked` | 受限于其他决策、上游修复或前置依赖。 |
| `good first issue` | 边界清晰、适合首次参与贡献者的入门任务。 |
| `action:helpwanted` | 已确认且特别欢迎社区协助推进的任务。 |

新提交的 Issue 可能会先经过自动路由和 Agent 辅助分析，帮助完成组件分类与上下文梳理；最终范围确认与状态标签调整由人工维护者完成。

## 3. 认领与释放任务（`/claim`）

当 Issue 至少带有一个可认领标签（`status:accepted`、`status:ready`、`good first issue`、`help wanted` 或 `action:helpwanted`），且不存在冲突的工作流状态（如 `status:in-progress`、`status:blocked`、`status:needs-info`）时，即可认领。

在符合条件的 Issue 下发表一条评论，且该行**仅包含以下文本**：

```text
/claim
```

如果因故无法继续推进，请及时评论释放：

```text
/unclaim
```

（`/assign` 与 `/unassign` 为等效别名。）

认领机器人会写入归属标记评论，并将状态更新为 `status:in-progress`。即便 GitHub 权限限制无法将外部贡献者设为正式 Assignee，机器人的归属标记依然是权威记录。

### 保持认领流转顺畅

- 建议同一时间只认领 1 个 Issue，把其他任务留给更多社区伙伴。
- 仅创建 Draft PR 而未执行 `/claim` **不会**锁定 Issue；如需在 Draft 迭代期间占锁，请先在 Issue 中评论 `/claim`。
- 若实现周期超出预期，请在 Issue 中简短同步进展（长期静置未更新的 Draft PR 不能无限续锁）。
- 认领工作流目前没有定时自动过期释放机制：连续 **7 天**无可见进展时，维护者或协作助手可能礼貌询问进度；满 **14 天**无进展（或提醒后满 **7 天**无回复）时，维护者可评论 `/unclaim` 礼貌释放认领，以便他人接手。

## 4. 提交 PR 与代码评审

1. 早期探索或尚未准备好正式合入时可使用 **Draft PR**（`isDraft=true`），完成准备后再点击 **Ready for review**（`isDraft=false`）进入正式评审与合入门禁；除非作者显式评论请求早期反馈，否则 Draft PR 不会触发正式评审。
2. 按[通用贡献指南](../../CONTRIBUTING_zh.md)运行受影响组件的本地构建、lint 与测试门禁。
3. 使用 `git commit -s` 为每个 Commit 添加 `Signed-off-by`，并遵守 [`specs/commit-message-standard.md`](../../specs/commit-message-standard.md)。
4. 完整填写 PR 模板并关联对应 Issue（`closes #<number>`）。
5. 响应 CI 检查、AI 辅助评审（请结合实际代码核实建议）以及 `CODEOWNERS` 维护者的反馈。
6. 当关联 Issue 由其他正式 PR 合入解决或关闭时，未合入的正式撞车 PR 会由维护者致谢、关联结论并关闭；尚未转为 Ready for review 的 Draft PR 不阻塞原 Issue 结案。

完整的所有权信号优先级、`/claim` × Draft PR 生命周期矩阵与 AI Agent 规范见 [`specs/issue-claim-and-pr-lifecycle.md`](../../specs/issue-claim-and-pr-lifecycle.md)（[中文评审参考](../../specs/issue-claim-and-pr-lifecycle_zh.md)）。关于响应预期、分级决策机制以及向 Triager / Reviewer / Maintainer 的成长路径，请参阅[社区治理](../../GOVERNANCE_zh.md)。

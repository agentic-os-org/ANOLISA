# ANOLISA Issue 认领与 PR 生命周期规范：中文评审参考

[English](issue-claim-and-pr-lifecycle.md)

> 本文仅供中文阅读和评审参考，不是完整译文。实际规则以英文原文为准。

## 核心结论

本规范统一定义社区贡献者、维护者与 AI Agent 对 **Issue `/claim` 认领锁**、**Draft PR（`isDraft=true`）**、**正式 PR（`isDraft=false`）**、**撞车 PR** 与 **Issue 结案** 的共同约定，避免「内部 Agent 抢社区已认领的单」「无锁 Draft 永久占坑」「对草稿刷屏评审」以及「已关 Issue 被他人未提交草稿卡死结案」四类冲突。

## 1. 认领与所有权信号的唯一判据

判断一个 Issue 是否已被占用时，人类与 AI Agent 统一按以下优先级识别：

1. **权威 `/claim` 机器人标记**（最高优先级）：`github-actions[bot]` 在评论首行写入的 `<!-- anolisa-claim owner=<login> state=claimed -->`（配合 `status:in-progress` 标签）。外部贡献者无法被 GitHub API 设为正式 `assignee` 时，仍以该标记为准；出现后续 `state=released` 标记则视为已释放。
2. **非 Bot 的直接 `assignee`**：无有效 claim 标记时，Issue 上直接指派的非 Bot `assignee` 表示正在实现该任务的人。
3. **正式 Open PR（`isDraft=false`）**：已声明 `closes/fixes/resolves #<n>` 的非草稿 PR，表示已有在途交付。
4. **不构成排他锁的信号**：未执行 `/claim` 的纯自由文本评论，以及**没有 `/claim` 锁的独立 Draft PR（`isDraft=true`）**，均不占坑。

## 2. `/claim` × `Draft PR` × 正式 PR 统一生命周期表

| 社区信号组合 | 是否持有排他开发锁（不重复开发）？ | 是否进入正式 Review？ | 原 Issue 由其他 PR 解决或无交付关闭后如何处理？ |
| --- | :---: | :---: | --- |
| **1. 有效 `/claim` 锁**（bot marker 或直接非 Bot assignee）且在活跃窗口内，尚未提 PR | **是**（等待或先在 Issue 沟通） | 否（尚无 PR） | 超期无进展时先礼貌提醒，再由维护者 `/unclaim` 释放后接手 |
| **2. 仅挂 `Draft PR`（`isDraft=true`），无有效 `/claim` 锁** | **否**（个人草稿不占坑，他人仍可 `/claim` 并提交正式 PR） | **否**（不自动发起正式评审） | **视为自然失效草稿，不阻塞原 Issue 结案与交付核验**；可酌情留一句礼貌告知，不强求代关作者草稿 |
| **3. 有效 `/claim` 锁 + `Draft PR`（`isDraft=true`）** | **是**（受 `/claim` 活跃窗口保护；长期停在 Draft 不能无限续锁） | **否**（点 Ready for review 后进入正式评审，除非作者显式请求早期反馈） | 若超期释放后由其他正式 PR 解决，剩余未转正草稿按第 2 行处理，不阻塞结案 |
| **4. 正式 Open PR（`isDraft=false`）且关联 `closes/fixes/resolves #<n>`** | **是**（已有在途交付，应在 PR 上协作而非重复实现） | **是**（进入正式评审与合入门禁） | **撞车正式 PR 必须显式收尾**：主 PR 合入或 Issue 关闭后，维护者/组件负责人必须致谢、指明已合入 PR 或结论并关闭该 PR |

## 3. `/claim` 超时节奏与释放规则

`.github/workflows/issue-claim.yml` **目前没有自动定时释放**，释放由认领者或具备维护权限的成员通过 `/unclaim`（或 `/unassign`）执行：

- **7 天无可见进展（礼貌提醒）**：连续 7 个自然日既无 Issue 进展回复、也无转为 Ready for review 的正式 PR（静置未更新的 Draft PR 不算正式提交），维护者或协作 Agent 可在 Issue 中礼貌询问是否仍在推进。
- **14 天无可见进展（释放认领）**：距认领或上次进展更新满 **14 个自然日**（或距维护者提醒满 **7 个自然日**，取较晚者）仍无回复或进展时，维护者可说明情况并评论 `/unclaim`，使 Issue 回到 `status:accepted`（当无其他阻塞 `status:*` 标签时）。
- 认领者只需简短回复进展即可保活；无法继续时应主动评论 `/unclaim`。

## 4. 独立单 PR（无关联 Issue）

- 在 GitHub 仍处于 Open 状态期间（无论是否 Draft），不得在跟踪系统中提前标记为已完成或已取消。
- 长期静置的独立 PR 可在维护者留言沟通并等待合理期限（默认 14 个自然日）后关闭。

## 5. AI Agent 必须遵守的核对项

- **开工前**：必须同时核查 `github-actions[bot]` claim marker、`status:in-progress`、直接非 Bot `assignees` 和非草稿 Open PR；不要撞车，也不要把无 `/claim` 的 Draft PR 误当成锁。
- **评审时**：先查 `isDraft`；除非作者显式评论请求早期反馈或维护者手动触发，不对 Draft PR 发起正式打分评审。
- **结案与审计时**：必须区分 `isDraft=false` 与 `isDraft=true`；正式撞车 PR 必须礼貌关单，未转正的 Draft PR 不阻塞已解决 Issue 的结案。

完整规范条款、工作流边界说明与细节见[英文原文](issue-claim-and-pr-lifecycle.md)。

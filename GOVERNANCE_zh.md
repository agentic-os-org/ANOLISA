# ANOLISA 社区治理

[English](GOVERNANCE.md)

本文档定义 ANOLISA 社区的角色体系、协作与决策机制，以及组件生命周期规则。ANOLISA 以中立、开放的方式建设面向 AI Agent 工作负载的系统软件栈，欢迎个人开发者、开源社区、OS 发行商与企业共同参与。

## 1. 治理原则

1. **开放与中立**：技术决策以公开讨论、工程事实与用户价值为依据，保持对多厂商与多 Agent 框架的开放兼容。
2. **贡献即话语权（Meritocracy）**：社区职责与评审权限来自持续、高质量的公开贡献，涵盖代码、文档、OS Skills、测试样本、Issue 分拣与代码评审。
3. **边界清晰**：各组件在其职责范围内自主演进；跨组件契约与公共协议通过公开提案协商决定。
4. **安全与质量底线**：所有社区参与均须遵守 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md)、[SECURITY.md](SECURITY.md) 及仓库工程规范。

## 2. 社区角色与晋升阶梯

ANOLISA 的角色体系与仓库维护元数据（[`.github/components.json`](.github/components.json)、[`.github/maintainers.json`](.github/maintainers.json) 与 [`.github/CODEOWNERS`](.github/CODEOWNERS)）保持一致。

| 角色 | 职责与权限 | 晋升方式 |
| :--- | :--- | :--- |
| **社区成员（Community Member）** | 使用 ANOLISA，参与 GitHub Discussions、提交 Issue、复现问题，或分享真实场景与测试样本。 | 参与任意公开社区互动即可。 |
| **贡献者（Contributor）** | 通过 PR 贡献代码、文档、OS Skills、压缩器、AW Provider/Adapter、发行版打包或评测用例；可通过 `/claim` 认领任务。 | 至少有 1 个 PR 被合入，或在社区中有经认可的实质性非代码贡献。 |
| **分拣与评审者（Triager / Reviewer）** | 协助维护组件 Issue 分类与状态流转（对应 `issue_triagers`），参与 PR 初审、复现验证与新人引导。 | 在特定组件或领域持续参与有效 Issue 分拣、PR 评审或合入多项高质量贡献；由对应 Component Maintainer 提名并经 2 名 Maintainer 同意。 |
| **组件维护者（Component Maintainer）** | 负责特定组件的技术路线、PR 最终批准与合并、版本发布及社区任务拆解（登记于 `.github/maintainers.json` 与 `.github/CODEOWNERS`）。 | 对该组件有持续、深入的高质量贡献与评审记录，熟悉组件契约与发布门禁；由现有 Maintainer 提名，经所在组件 Maintainer 与 Core Maintainers 共识通过。 |
| **核心维护组（Core Maintainers）** | 负责项目整体方向、跨组件契约与公共协议（如 AW 协议）、安全响应协调、组件立项/毕业/归档以及跨组件争议仲裁。 | 由各活跃组件代表及长期跨组件维护者组成；增补需经现有 Core Maintainers 多数同意。 |

### 活跃度与荣休（Emeritus）

- 任何 Triager、Reviewer 或 Maintainer 若因个人安排暂时无法继续承担评审或分拣职责，可随时申请转为 **Emeritus（荣休）**，保留荣誉记录并移出自动评审队列。
- 若维护者连续 **6 个月** 无公开社区活动，Core Maintainers 在联系确认后可将其调整为 Emeritus；恢复活跃时经简单确认即可重新激活。

## 3. 协作流转与 Agent 辅助运维

ANOLISA 将公开透明的人工治理与 Agent 辅助自动化结合起来，在保证评审质量的同时提升社区响应效率：

1. **方向探索**：组件维护者在 GitHub Discussions 的 [Community Roadmap](https://github.com/agentic-os-org/ANOLISA/discussions/categories/community-roadmap) 发布候选方向与重点征集议题。
2. **Issue 接入与分拣**：自动路由与 Agent 辅助分析协助完成组件归类、信息完整度检查与上下文梳理；人工维护者最终确认范围，并打上 `status:accepted`、`good first issue` 或 `action:helpwanted` 标签。
3. **认领与推进**：贡献者在可认领的 Issue 下单独评论 `/claim` 即可锁定归属，无法继续时评论 `/unclaim` 释放（详见 [Roadmap、Issue 与认领流程](contribute/zh/workflow.md)）。自动化任务与 Agent 助手可对长期无进展的认领发出温和提醒。
4. **评审与合入**：PR 须通过 CI 门禁与对应 `CODEOWNERS` 评审。AI 辅助评审（如 Qoder Review）用于辅助发现问题，技术判断与最终合并由人工 Maintainer 负责。

### 响应预期

- **首次响应**：维护者在自动分拣与 Agent 助手的协助下，尽力在 **5 个工作日** 内对新提交的 Issue 和 PR 给出首次响应。
- **认领超时释放**：被认领的 Issue 若连续 **14 天** 无可见进展且沟通无回复，经自动提醒后，维护者可礼貌释放认领，方便其他贡献者接手。

## 4. 决策机制

ANOLISA 按影响范围采用分级决策，优先遵循**懒惰共识（Lazy Consensus）**——提案公开后在公示期内无实质反对即视为通过：

| 决策级别 | 适用范围 | 流程与公示期 | 审批主体 |
| :--- | :--- | :--- | :--- |
| **L1 · 组件内日常变更** | Bug 修复、局部功能增强、文档更新、单组件测试与重构 | Issue + PR | 对应组件的 Component Maintainer（至少 1 名批准） |
| **L2 · 跨组件与公共契约变更** | 公共 CLI/配置格式变更、AW Provider/Adapter 协议演进、影响多个组件的破坏性改动 | 通过 GitHub Discussions（`Ideas`）或设计文档 PR 发起**轻量 RFC**，公示至少 **5 个工作日** | 所有受影响组件的 Maintainer 达成共识 |
| **L3 · 项目级与治理变更** | 引入新组件、组件毕业或归档、修订本治理文档、调整许可证或安全策略 | 公开 RFC Discussion，公示至少 **7 个工作日** | Core Maintainers 三分之二多数同意 |

当 L1 或 L2 讨论无法达成共识时，可升级至 Core Maintainers 协调或表决。

## 5. 组件与生态生命周期

ANOLISA 区分**仓库内核心组件**（登记于 [`.github/components.json`](.github/components.json)）与**独立生态扩展**（如仓库外 AW Provider、外部 Agent Adapter 及社区镜像目录条目）：

- **独立生态扩展**：开发者可基于公开协议（如 `aw-provider/v1alpha1`）在仓库外独立开发与分发 AW Provider 或 Adapter。如需收录至 [`providers/`](providers/README_zh.md) 或[镜像目录](docs/user-guide/zh/getting-started/images.md)，由对应维护者依据契约测试与文档规范评审合入。
- **仓库内组件生命周期**（与 `.github/components.json` 中的 `status` 字段对应）：
  1. **`incubating`（孵化中）**：完成 L3 立项讨论并满足[组件接入规范](specs/component-onboarding.md)（可编译骨架、基础测试、CI 接入、定位清晰且至少有 1 名 Owner）。
  2. **`active`（正式活跃）**：具备中英双语用户/开发者文档、可验证的发布制品或 Preview 安装路径、完备自动化测试与活跃维护团队，经 Core Maintainers 评审毕业。
  3. **`deprecated`（已归档）**：因架构演进被替代或长期缺乏维护者时，提前公示迁移方案后移入 `deprecated/`，不再纳入默认构建与官网主导航。

## 6. 法律合规、行为准则与修订

- **许可证与 DCO**：所有贡献默认采用 [Apache License 2.0](LICENSE) 许可，且每个 Commit 均须使用贡献者本人 Git 身份附带 `Signed-off-by`（DCO）。
- **行为准则**：所有社区空间（GitHub、钉钉群及线下活动）均受 [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) 约束。
- **安全披露**：安全漏洞严禁公开提交 Issue，请按 [SECURITY.md](SECURITY.md) 私密报告。
- **宪章修订**：本治理文档的修订遵循 L3 决策流程，并保持中英文版本语义同步。

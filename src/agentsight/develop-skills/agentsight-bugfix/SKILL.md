---
name: agentsight-bugfix
description: AgentSight fix issues 流程。了解问题 → 复现验证 → 根因分析 → 编码修复 → 验证修复，五阶段标准化调试迭代循环。确保 AI agent 遵循最小改动、逐轮验证的修复规范。
---

# AgentSight Fix Issues 流程

## 目标

在接到 AgentSight 的 Bug 修复任务时，严格按以下五个阶段执行，确保每轮迭代最小化改动、最大化验证。

```
了解问题 → 复现验证 → 根因分析 → 编码修复 → 验证修复
```

## 阶段 1：了解问题

**目标**：明确 Bug 的实际表现、预期行为和影响范围，在动手之前先想清楚。

- 阅读 issue 描述或 Bug 报告，提取关键信息：
  - **实际表现**：当前系统做了什么？（错误数据、缺失字段、异常行为等）
  - **预期行为**：系统应该做什么？
  - **触发条件**：什么场景下出现？（特定 Agent、特定 API、特定协议等）
  - **影响范围**：哪些数据/功能受影响？
- 定位相关代码模块，理解当前数据流路径（参考 `docs/ARCHITECTURE.md` 流水线：`Probes → Parser → Aggregator → Analyzer → GenAI → Storage`）
- **不要在这个阶段下结论或开始写代码**，信息不充分时的判断大概率是错的

**阶段产出**：四要素摘要（实际表现、预期行为、触发条件、影响范围）。

## 阶段 2：复现验证

**目标**：在实际环境中从零运行一次完整流程，亲眼看到 Bug 发生。

**硬性要求**：
- **必须在实际环境中运行 agentsight trace**，不要只看已有数据或旧 DB 就下结论
- **必须从零开始**：停掉旧进程 → 清理旧数据 → 启动 trace（开启 debug 日志）→ 触发目标场景 → 查询 DB 确认 Bug 存在
- 收集 debug 日志，用于阶段 3 分析

**阶段产出**：实际观察到的现象（DB 数据、日志关键行），与阶段 1 的预期做对比。

## 阶段 3：根因分析

**目标**：基于阶段 2 收集的日志和数据，定位数据流在哪个环节断了。

**原则**：
- **先看日志证据**，定位数据流断点，不要只凭代码推测
- 重点关注事件处理顺序（时序问题），agentsight 事件循环是串行的
- 验证旧代码的实际行为是否和预期一致（如 parsed_message 是否为 None、mapper 是否命中）

常见断点检查清单：

| 检查项 | 日志关键词 | 说明 |
|--------|-----------|------|
| AggregatedResult 类型 | `Analyzing aggregated result` | 是 SseComplete 还是 Http2StreamComplete？ |
| ParsedApiMessage 是否生成 | `Parsed OpenAI response` | 没有 = analyzer 没产出 Message |
| response_id 是否提取 | `chatcmpl-` | 从 SSE body 或 parsed message |
| mapper 是否命中 | `ResponseSessionMapper` | FileWrite 时序是否先于 LLM 事件 |
| PendingGenAI 是否入队 | `queued for deferred` | 没入队 = pending_response_id 为 None |
| complete_pending 是否更新 | `Promoted pending→complete` | UPDATE 是否包含 session_id |

**阶段产出**：明确指出数据流在哪个环节断了，以及断裂的原因。

## 阶段 4：编码修复

**目标**：针对阶段 3 定位的断点，做最小范围的代码修改。

**原则**：
- **只改解决该问题所需的部分**，无关改动不要顺手一起改
- 修复可能涉及多个断点时，每轮只修一个

提交前必须通过本地检查：

- `cargo fmt`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test`

## 阶段 5：验证修复

**目标**：构建新版本并部署到运行环境，从零验证修复有效。

- 构建 release 版本
- 部署到运行环境（本地直接使用，远程环境需上传部署）
- 按阶段 2 的流程**完整走一遍**：清 DB → trace → 触发请求 → 查 DB，确认修复有效

### 验证结果处理

| 结果 | 动作 |
|------|------|
| Bug 已修复，无副作用 | 进入完成确认 |
| Bug 部分修复 | 回到阶段 3，分析剩余断点 |
| Bug 未修复或出现新问题 | 回到阶段 3 重新分析，**不要在同一轮叠加更多修改** |

### 完成确认

- [ ] 问题场景在实际环境中从零验证通过
- [ ] `cargo test` 全部通过
- [ ] 无引入新的 clippy warning
- [ ] 改动范围仅限问题本身，无附带无关变更

## 注意事项

- **不要只看旧 DB 数据就下结论**，必须从零运行一次完整流程
- **不要顺手改无关代码**，每轮只修最小必要改动
- **验证失败时回到日志分析**，不要盲目叠加代码

# Orchestrator Rules

> 适用于 `src/unified.rs`（AgentSight 主编排器，当前 2200+ 行）。

1. unified.rs 只做组件装配和事件分发，禁止添加业务逻辑（解析、分析、存储）
2. 新流水线阶段必须放在独立模块，unified.rs 只调用其公开接口
3. `AgentSight::new()` 只做组件初始化和装配，不执行 I/O 或阻塞操作
4. `try_process()` 保持精简，通过委托给具体 analyzer/builder 处理，不内联处理逻辑
5. 新增字段前先考虑是否应属于子模块（Footprint Ladder 级别 1-2 优先）
6. 该文件已超过 2000 行，新增代码前必须评估是否可以提取到子模块

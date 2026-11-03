# ANOLISA adapter 来源选择

ANOLISA 继续拥有 receipt 记录的用户扩展副本。Cosh 负责来源选择与激活状态，包含来源身份
校验和事务回滚。原生本地路径安装当前不支持原地更新，因此 adapter 沿用现有文件交付流程。

`extensions` registry domain 提供两个 adapter 集成 action：

- `adapter-capabilities` 返回 `conditional_clear_user_source: true`。
  ANOLISA 在 enable 或 disable 修改扩展文件前探测该能力。
- `clear-user-source` 必须提供 `name` 和 `source_identity`。仅当用户来源选择与给定身份
  完全一致时移除选择。扩展已不存在、选择已清除、用户已选择其他来源时均可安全重试。

清理复用已有加锁的 catalog 状态事务，候选运行时通过健康检查后提交；验证失败时恢复原状态。
禁用列表会保留，并遵循原有运行时 generation 发布机制。移除文件或清理来源失败时，ANOLISA 保留 receipt 供重试。

重新启用涉及扩展身份变化时，ANOLISA 通过既有 disable 流程释放旧用户副本和来源选择，
完成后再替换持久化 receipt。清理失败保留旧 receipt，替换后激活失败保留新 receipt。
有效身份和目标目录相同的重新启用继续沿用原有原地交付流程。

enable 核对发现的用户来源身份，调用 `select-source` 和 `enable`，最后回读 `detail`。
status 检查身份、期望启用状态、原生健康状态和 receipt 文件。独立 registry 调用无法观测
已有会话，资源加载状态保留为 unknown，已有会话需要重启。`COSH_HOME` 统一用户扩展
和状态根目录；已有 `COSH_STATES_DIR` 对状态目录的覆盖优先级保持不变。

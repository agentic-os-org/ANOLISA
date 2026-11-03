# 更新实例策略

[English](../../../en/aw/deployment/policy-updates.md)

部署方负责暂停和恢复任务派发，AW 在确认实例空闲后激活完整策略。先按[Agent 接入页](../native-agents.md)配置受支持的 worker。

## 选择运行模式

| 模式 | 就绪条件 | 更新前等待 |
| --- | --- | --- |
| 普通 Qoder SDK 1.0.56 / Worker 1.1.78 | 受管 `startDaemon()`；无显式 daemon 时须在 30 秒内初始化 `createQuery()` | 全部 Query 完整关闭，回调和租约清理结束 |
| 普通 QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | 受管 App startup | 全部 reply 和清理结束 |
| Qoder 1.1.73 TUI、上述 SDK / App 的 `--reloadable` 模式 | 原生稳定桥接；SDK 另需显式 daemon 和初始化的 Query | 所有原生请求及后台清理结束 |
| 外部启动的 Qoder TUI / QwenPaw App | [首请求前 attach](process-attach.md) | 同受限桥接 |

普通 Qoder SDK、QwenPaw 保留严格 Post、受管 MCP 和私有执行租约；受限模式不提供 resident 严格 Post 和私有租约。
其他 Adapter 修改策略需重启。普通 Qoder SDK 的 Query 在轮次之间仍占用实例，关闭后才能更新；预热 worker 应先调用受管 `startDaemon()`。

## 应用候选策略

1. 暂停向目标实例派发新任务，等待表中的清理边界。
2. 准备候选文件。只修改 `providers`、`events` 和 `execution.default_event_budget_ms`；启动、审计和观测字段保持原值。
3. 校验、应用并查询实际版本，将 `TARGET` 换成配置目标名。

```bash
aw validate --config next.yaml --against current.yaml
aw update --config current.yaml --agent TARGET --policy next.yaml
aw status --config current.yaml
```

成功响应返回 `policy_revision`、前版本、审计 key 及准备和激活耗时。确认成功后恢复派发，每个新任务取得完整策略快照。
同一目标有多个实例时增加 `--instance ID`。所有 Session 共用实例策略，任务子 scope 不能单独更新。

也可直接编辑 `current.yaml`，再执行 `aw update --config current.yaml --agent TARGET`。
AW 保存该路径连接的服务身份；文件不会自动加载。`--policy` 形式不会改写任一文件，后续单文件更新需先写入完整期望策略。

## 失败与恢复

| 结果 | 下一步 |
| --- | --- |
| `busy` | 旧策略继续生效；等待请求和清理结束后显式重试 |
| 准备失败、版本冲突或能力不支持 | 修正候选或查询新版本，再决定是否重试 |
| 超时、结果不确定 | 查询实际版本后再派发工作 |
| 激活后审计持久化失败 | 停止受影响 worker，修复原因，按期望配置重启服务和 worker |
| 清理无法确认，持续 busy | 停止 worker 并重建实例 |

更新不排队。成功后继续由同一控制器串行安排更新和任务，防止其他派发方插入工作。
恢复旧策略使用同一流程提交完整旧配置。更换 AW、SDK、模型账号或启动字段时按[生命周期](lifecycle.md)重新部署。

## 控制器接入

```bash
aw update --socket "$AW_SOCKET" --instance "$AW_INSTANCE" \
  --policy next.yaml --expected-revision "$AW_POLICY_REVISION"
```

`--expected-revision` 指定并发前置版本，冲突不自动重试。CLI 没有 `aw update --pid` 选择器。
只读容器为启动、更新、status、stop 共用一个绝对 `XDG_RUNTIME_DIR`，属运行用户且权限 `0700`。
连接元数据位于 `$XDG_RUNTIME_DIR/aw/connections`，未设置时为 `/tmp/aw-UID/connections`；worker 和服务停止后再清理该运行时挂载。

原生忙碌确认、任务快照和审计提交顺序见[更新合同](../../../../../src/aw/docs/design/zh/runtime/policy-reload.md)。

# 更新实例策略

[English](../../../en/aw/deployment/policy-updates.md)

部署方负责暂停和恢复任务派发，AW 在确认实例空闲后激活完整策略。先按[Agent 接入页](../native-agents.md)配置受支持的 worker。

## 选择运行模式

| 模式 | 就绪条件 | 更新前等待 |
| --- | --- | --- |
| 普通或已准备的原生 Qoder SDK 1.0.56 / Worker 1.1.78 | 受管 `startDaemon()`；无显式 daemon 时须在 30 秒内初始化 `createQuery()` | 全部 Query 完整关闭，回调和租约清理结束 |
| 普通或已准备的原生 AgentScope 2.0.6 工厂 | 首次 `AwBridge.create_agent()` | 全部 reply、工具任务与 Scope 清理结束 |
| 普通或已准备的原生 QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | 受管 App 插件启动 | 全部 reply 和清理结束 |
| 已准备的原生 Claude SDK resident 工厂 | 独立进程组内调用 `prepare_native()` | 全部客户端断开，SDK、中继和 MCP 清理确认完成 |
| 已准备的原生 Pi worker 工厂 | `runNativeWorker()` 接入既有 `createSession` / `runWorker` 工厂 | 全部 Session 关闭，准备、abort 和 dispose 完成 |
| 已准备的原生 OpenClaw Gateway | 插件在 `gateway_start` 确认 | 成功完成 `agent_end`，工具回调排空 |
| 已准备的原生 Hermes 本机 chat | 插件已连接，受管 chat 启动 | 轮次完成，回调排空 |
| 已准备的原生 Qoder CLI（TUI 或 print），以及 `--reloadable`／外部 attach 的 Qoder TUI | 预装启动 Hook 确认每个进程；外部 TUI attach 使用独占 profile，并在[首请求前 attach](process-attach.md) | 所有原生请求及后台清理结束 |
| Qoder SDK / QwenPaw 的 `--reloadable`，或外部 attach 的 QwenPaw App | 原生稳定桥接；SDK 另需显式 daemon 和初始化的 Query | 同受限桥接 |

先按框架的[启动说明](../native-agents.md)完成一次准备。SDK 应用在自身启动流程中调用
`aw instance prepare`，保留已有 AW 工厂；OpenClaw、Hermes 和 QwenPaw 安装启动组件并
重启。准备先创建待就绪实例，原生确认后才就绪。尚未接线的运行中进程无法通过策略更新
获得 Hook 覆盖。Qoder 可在下次原生启动前安装启动 Hook，已有 TUI 仍可使用首请求前
attach 的接入方式。

普通 Qoder SDK、QwenPaw 保留严格 Post、受管 MCP 和私有执行租约；受限模式不提供
resident 严格 Post 和私有租约。Qoder SDK 的 Query 在轮次之间仍占用实例，关闭后才能
更新；预热 worker 应先调用受管 `startDaemon()`。已准备的 Pi 和 Claude 同样在整个
Session 或客户端关闭前保留原策略。

已准备的 OpenClaw 事件预算上限为 12,000 ms；Hermes 按指南准备回调超时后，保留
55,000 ms 上限。原生完成状态不确定时保持 busy，需要重启。OpenClaw 支持 Gateway run，
`tools.invoke` 在覆盖范围外；Hermes 支持本机 chat，`input.submit` 限于 quiet 单查询文本
入口。Hermes Skill 检查在策略更新前后沿用相同的来源选择与清单固定合同。
普通 `aw run` 启动的 Pi、Claude SDK、OpenClaw 和 Hermes 仍沿用原有重启要求。
其余原生输入与结果限制见各 Agent 指南。

## 使用一份默认配置

在控制端工作目录保存 `aw.yaml`，使用同一配置启动服务。已准备的原生应用报告就绪后，
暂停新任务派发，修改 `aw.yaml` 的策略，再执行：

```bash
aw instance update --agent TARGET
aw status --config aw.yaml
```

同一目标有多个实例时，用 `--instance ID` 选择。`aw instance update` 复用 `aw update`
的更新合同，不执行进程发现或附着。后续策略修改沿用已安装的启动接点和原生设置；
允许更新的策略字段之外的变化需要重启。

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

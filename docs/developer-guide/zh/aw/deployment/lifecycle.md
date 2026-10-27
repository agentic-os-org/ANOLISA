# 服务与容器生命周期

[English](../../../en/aw/deployment/lifecycle.md)

## 启动与持久化

镜像中安装相互匹配的 AW、Provider 和固定版本 Agent。准备原生 profile 与模型账号后，通过 `aw run` 启动应用。
Hermes 插件须安装到实际运行的 profile。先验证[原生效果](../validation.md)，再接入业务流量。

| 资源 | 部署方式 |
| --- | --- |
| 二进制、脚本、默认配置 | 镜像或只读挂载 |
| 可编辑配置 | 保存在临时容器层之外 |
| `daemon.state_dir` | 可写绝对路径，属运行 UID，权限 `0700`，父目录同 UID 且不可被他人写入 |
| `state_dir/journal` | 需要保留历史时持久化；只保存调用元数据 |
| socket、锁、`launch-*`、绑定 | 每次运行重新建立，不能恢复会话 |
| 原生 profile、工作区、后端数据 | 由框架或 Provider 独立管理 |

`endpoint: auto` 配合显式 state_dir 使用其中的 `aw.sock`。两个字段都为 auto 时使用 `XDG_RUNTIME_DIR`，未设置则用 `/tmp/aw-UID`。
每个独立 daemon 使用独立状态目录。持久卷需支持 `fsync`。

`startup: on_demand` 按需启动服务，`external` 要求外部服务已运行。需要外部监督器时以前台方式启动。

```bash
aw service serve --config /absolute/aw.yaml --state-dir /absolute/private/state
```

复用服务要求协议与启动配置匹配。编辑配置后按[策略更新](policy-updates.md)操作，或停止旧服务后重启。

## 停止与升级

先停止派发、排空任务并关闭 SDK 客户端，再退出 Agent，最后执行 `aw stop --config FILE`。
等待记录的 daemon PID 退出和 socket 消失。强制终止后，仅在确认旧服务退出后删除它的遗留 `aw.sock`，保留 journal。

CLI 与 daemon 使用 `aw-service/v1alpha7`。升级或回退前用旧二进制停止旧服务，再一起更换二者。
配置和 Provider 协议分别为 `aw/v1alpha1`、`aw-provider/v1alpha1`。Preview 跨版本升级使用新状态目录，保留旧配置与数据供回退。

## 设置 Agent 退出宽限期

目标字段 `spec.agents.<target>.shutdown_grace_ms` 接受 1..120000 ms，默认 2000。
例如设置为 `15000`，launcher 从首次转发外部退出信号起等待 15 秒，再强制清理自有进程组。重复信号不延长计时。

该值用于前台 launcher 的外部信号退出；内部 readiness/lifecycle 失败沿用两秒清理宽限，自然退出立即清理遗留子进程。
`aw stop` 和外部 attach 的进程由各自生命周期控制。部署方需协调应用排空期限与容器退出预算。
修改需重启 launcher；旧版 AW 不支持此字段，回退配置时删除它。

## 服务并发预算

Pi/Claude 常驻路径及时回收已完成 Hook 状态，按活动资源准入。以下为默认值，各字段接受 1..4096。

```yaml
spec:
  daemon:
    limits:
      bindings: 64
      events: 128
      calls: 64
      connections: 128
      invocations_per_binding: 256
```

| 字段 | 计量对象 |
| --- | --- |
| bindings | 宿主及准备中、空闲、排空中的客户端 |
| events | 活动 Hook 事件 |
| calls | 活动 Provider 调用 |
| connections | 活动服务连接，含控制 RPC；另有 16 条保留连接仅供控制和清理 |
| invocations_per_binding | 单个常驻客户端活动的模型工具调用 |

`aw status` 的 `resources.limits`、`resources.usage`、`peak_invocations_per_binding`、`admission_rejections` 和 `rejection_reasons` 用于核对负载。修改 limits 后排空并重启服务。
旧任务入口及受限 Qoder SDK 路径仍保留每实例 1024 条原生事件关联上限；出现 `native_event_limit` 时重启实例。
审计磁盘和 SDK 历史另设留存策略。接入合同见[常驻 SDK](../../../../../src/aw/docs/design/zh/adapters/resident-sdk.md)。

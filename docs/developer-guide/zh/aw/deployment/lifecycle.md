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

普通与 `--reloadable` 模式的 Qoder SDK worker 的外部信号先送到 worker PID，由其信号处理器依次关闭 Query
和原生 daemon。前台 worker 应处理 TERM/INT，并保持存活直到清理完成。
其他启动形态仍向进程组转发。终端产生的信号仍按内核前台进程组投递，服务部署不分配 TTY。
看护的首次停止信号只投递到 `aw run` PID。向包含 SDK 子进程的进程组或整个 cgroup
广播信号会绕过 worker 优先通知。systemd 使用 `KillMode=mixed`，先向主 launcher PID
发送 TERM，最终 KILL 覆盖 cgroup；`TimeoutStopSec` 应覆盖完整排空预算。
所有模式都在 worker 退出或期限结束后强制清理自有组。

该值用于前台 launcher 的外部信号退出；内部 readiness/lifecycle 失败沿用两秒清理宽限，自然退出立即清理遗留子进程。
`aw stop` 和外部 attach 的进程由各自生命周期控制。部署方需协调应用排空期限与容器退出预算。
修改需重启 launcher；旧版 AW 不支持此字段，回退配置时删除它。

## 配置常驻 SDK 容器的停止预算

[SDK 安全示例](../../../../../src/aw/crates/aw-service/examples/aw.sdk-security.yaml)
为 Qoder SDK 设置 `shutdown_grace_ms: 90000`。每个 Query 的各清理阶段共享60秒预算。
该阶段之后，daemon 凭据回调使用独立的最多12秒预算；随后停止原生 daemon。
launcher 的 10 秒或看护的 15 秒预算会提前截断这些操作。
仅更新 AW 二进制会保留部署中已有的期限配置。

各层期限应逐层覆盖，并在应用排空时保持服务可用。
以下示例适用于依次停止应用、AW、sec-core 的看护脚本：

| 层级 | 示例预算 |
| --- | --- |
| 应用：停止派发、关闭受管 SDK daemon | 在目标的 90 秒内完成 |
| `aw run`：`shutdown_grace_ms` | 90000 ms |
| 看护：等待应用的 `aw run` 进程组 | 95 秒 |
| 看护：依次停止 AW、sec-core | 5 秒、75 秒 |
| 容器：覆盖全部阶段并留出余量 | 185 秒 |

```bash
docker stop --time 185 CONTAINER
```

此顺序看护方案的 Kubernetes Pod 设置 `terminationGracePeriodSeconds: 185` 或更长，
Compose 设置 `stop_grace_period: 185s`。Kubernetes默认30秒会提前截断AW的90秒宽限；
如有 preStop 工作，应计入外层总预算。

外层总预算按看护脚本的实际停止顺序计算。一起修改看护的应用宽限和目标配置，
重启 launcher 后生效。Qoder 关闭请求入口后调用受管 `daemon.stop()`：
它关闭自有 Query，即使 Query close 失败也会尝试停止原生 daemon；
其 rejection 应按清理失败处理。避免先执行遇错即返回的
`Promise.all(query.close())`，导致首次失败后跳过 `daemon.stop()`。

后端故障必须立即封锁新任务。外部信号宽限为已有清理留出时间；
内部 readiness/lifecycle 故障仍按短期限强制清理。清理结果不确定时保留凭据与资产。
单独记录 SDK 清理完成状态，退出码 143 或 70 无法证明回调已排空。
部署前验证 warm worker 带未关闭 Query 的 TERM/INT、后端退出、慢 finalizer、
超期场景，同时检查 SDK 收尾、进程回收和运行时文件清理。

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

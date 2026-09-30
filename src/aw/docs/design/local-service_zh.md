# 本地服务与客户端

[English](local-service.md)

`aw-service` 通过独立 Linux 进程和可复用的同步 Rust 客户端提供 Provider 执行
能力。一个服务持有一份不可变的 AW 配置快照，管理绑定和事件，并持久保存执行
元数据。它不启动 Agent、不安装原生 Hook，也不认证 Agent 已采用 Provider 响应。

## 组件职责

| 组件 | 职责 |
| --- | --- |
| `aw-service::Server` | 私有端点、配置身份、绑定与事件生命周期、有界 RPC 和审计存储 |
| `aw-service::Client` | 使用调用方统一截止时间的版本化请求，不自动重试 |
| `aw-host` | Provider 发现、配置校验、准入及事件步骤的单次调用 |
| `aw-exec` | 子进程截止时间、输出限制、取消及进程组清理 |
| `aw-core::journal::FileJournal` | 持久预留和可验证的元数据记录链 |
| 后续 Adapter | 核实原生能力、归一化回调、保留原生调度并采用效果 |

服务将 `FileJournal` 作为存储复用，不执行 Core 计划，也不产生 Core 执行或采用
凭据。Provider 私有设置保留在 `spec.providers.<name>.config` 中，服务不内置
安全引擎规则。

## 本地协议

[wire.rs](../../crates/aw-service/src/wire.rs) 定义实验性协议
`aw-service/v1alpha1`，与 Provider 的 stdio 协议 `aw-provider/v1alpha1` 分开。
每条连接承载一个带长度前缀的 JSON 请求及一个响应，每帧上限为 2 MiB。

请求包含 `api_version`、`identity`、`deadline_ns` 和 `operation`。
首次 `status` 请求可以不提供身份，后续请求必须匹配本次服务启动生成的 generation
和原始配置字节的 SHA-256 revision。即使文件没有变化，重启也会使旧客户端与句柄
失效。修改文件不会热更新正在运行的服务。

| `operation.method` | 信封以外的输入 | 结果 |
| --- | --- | --- |
| `status` | 无 | PID、资源数量、审计健康状态及尚未验证的采用状态 |
| `bind` | `target`、可信 `capabilities`、绝对 `cwd`、完整 `environment` | 服务签发的 `instance_id`、身份和准备阶段 `audit_key` |
| `unbind` | `instance_id` | 在全部事件关闭后释放绑定 |
| `open_event` | `instance_id`、归一化 `event` | 事件 ID、实例 ID 和已准入步骤 ID |
| `invoke_step` | `event_id`、`instance_id`、`step_id` | 候选效果或执行失败，以及调用元数据 |
| `close_event` | `event_id`、`instance_id` | 取消未完成工作、等待子进程回收并确认关闭 |
| `audit` | 准备阶段或事件的 `key` | 已验证的持久记录信封和 `terminal` 标志 |
| `stop` | 无 | 确认收到停止请求；进程退出才表示清理完成 |

`bind` 实际执行 `describe` 和 `validate_config`。能力证据来自可信的同用户
调用方，不能来自 Provider 的自述。准备阶段固定绑定的工作目录、显式环境和配置
命令，后续请求不能修改。准备失败时，响应可能包含 `audit_key` 而没有成功绑定；
Rust 客户端通过 `Error::Attempt` 暴露该记录键。

当前服务准入 `tool.before` 的 `observe`/`block` 和 `tool.after` 的 `observe`。
启用尚不支持的事件、`ask`、结果替换、最终检查或更强执行保证会被拒绝。成功响应
中的空效果列表不增加限制，也不授予原生权限。成功的策略阻断与调用失败后
`failure_action` 为 `block` 是两种不同结果。

## 共享事件生命周期

Adapter 只打开一次事件，再按原生框架要求的顺序和并发方式发送各个 `invoke_step`
请求。客户端克隆可以并发调用不同步骤。服务不额外决定串并行策略，也不重新执行
失败步骤。一个步骤在同一事件中只能尝试一次。

Adapter 必须在属于同一原生事件的回调之间传递返回的句柄。原生 session 或 tool-call
ID 是关联数据，不能证明两个回调属于同一个服务事件。为每个步骤重新打开事件会
重置预算，不符合共享事件合同。

Linux `CLOCK_MONOTONIC` 截止时间覆盖连接建立和消息传输，一次调用的剩余时间
最多为 60 秒。`open_event` 将请求截止时间和配置事件预算中的较早值固定为事件
截止时间，后续步骤请求不能延长它。若某次 `invoke_step` 使用更短的 RPC 截止时间，
服务等待响应超时会取消整个共享事件，执行器随后完成所属进程清理。关闭事件会取消未完成工作，等待其 Provider
子进程回收，并写入终结记录后才确认完成。过期也会关闭无人管理的事件。调用方
必须在原生工具执行前收齐所需步骤结果。

每个事件 worker 持有 Host 引用和取消状态，在自身栈上借用一个 `aw_host::Event`，
通过有界通道将请求交给 scoped worker。共享期限和调用声明保留在这一个 Event 中，
注册表无需同时存放所有者与借用它的 Event，也不需要自引用结构。

当前资源上限为 16 个绑定槽、32 个打开的事件、每个绑定 64 个准入步骤、64 个并发
步骤调用和 64 条活跃连接。超过限制会明确拒绝或返回传输失败，不会隐式排队重试。
这些是服务准入限制，不是新增配置字段。

## 审计与结果不确定的调用

Provider 准备或事件执行前先做持久预留。每个步骤在调用前写入开始记录，在向
调用方返回效果前写入完成记录。审计写入失败时不能返回成功效果，服务也会停止
继续处理。

记录包含配置和 generation 身份、绑定与实例 ID、选定的原生关联字段、步骤、
Provider 和请求 ID、时长、字节数以及执行状态。不记录工具输入或结果、私有配置、
原始 stdout/stderr，也不将 Provider 诊断与效果原因文本写入 Journal。

准备记录键由 `bind` 返回，已记录的失败通过 `Error::Attempt` 返回。事件 ID
同时也是审计键。`audit` 验证存储链，单次查询文件限制为 1 MiB。`terminal: false`
表示尚无终结记录，可能仍在运行，也可能曾被中断；仅凭这个值不能判断进程崩溃。
事件关闭不证明原生 Agent 已采用效果或执行工具。

传输中断或客户端截止时间耗尽，可能使调用方无法确认执行结果。不能因此重试步骤
或推断获得许可，应查询审计记录。新启动的服务可以读取旧 generation 的记录，
但不能恢复其事件、复用旧句柄或重放执行。Journal 摘要用于发现损坏，不能阻止
同一用户改写存储。

## 端点、关闭与重启

前台 CLI 要求绝对 `--state-dir`，已存在的父目录必须属于当前用户，且组和其他用户不可写。服务以 0700
创建状态目录，或要求已有目录具备这一权限；在这些边界拒绝符号链接，不擅自修改
权限。socket 为权限 0600 的 `aw.sock`。排他的 `service.lock` 和 Unix 对端同 UID
校验约束服务所有权及本地访问。这是可信的同用户边界，不是 sandbox，也不隔离
同一用户的其他进程。

显式 `spec.daemon.state_dir` 和 `spec.daemon.endpoint` 必须匹配选定目录与
socket。`auto` 将路径选择交给启动方，但当前 CLI 仍要求 `--state-dir`。
配置中的启动偏好不会安装 supervisor，也尚未实现按需启动。

`Server` 不安装信号处理器，由接入方提供取消标志。`aw serve` CLI 处理
SIGINT/SIGTERM，取消活跃工作并等待连接和事件 worker。成功的 `stop` 响应仅确认
收到请求，必须等待前台进程退出才能认定关闭完成。执行器清理和文件系统同步不
提供硬实时保证。

正常关闭只删除该服务创建且 inode 仍匹配的 socket，保留 `service.lock` 和
`journal/`。强制终止后遗留的 socket 会使下一次启动明确报错。先确认上一个服务
进程已经退出，再显式删除其所属的 `aw.sock`；不要删除活跃服务的 socket 或锁文件。
使用同一目录重启会保留审计历史，并产生新的 generation。

## 源码入口

- [用户命令与本地演示](../../../../docs/user-guide/zh/user-entrypoint/aw.md#运行本地服务演示)
- [公共 API](../../crates/aw-service/src/lib.rs)、
  [Client](../../crates/aw-service/src/client.rs) 和
  [Server](../../crates/aw-service/src/server.rs)
- [Runtime](../../crates/aw-service/src/runtime.rs) 与
  [审计元数据](../../crates/aw-service/src/audit.rs)
- [开发检查](../../CONTRIBUTING_zh.md#运行时验收)

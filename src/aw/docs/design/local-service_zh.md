# 本地服务与客户端

[English](local-service.md)

`aw-service` 通过独立 Linux 进程和可复用的同步 Rust 客户端提供 Provider 执行
能力。一个服务持有一份不可变的 AW 配置快照，管理绑定和事件，并持久保存执行
元数据。`aw run` 客户端通过本次会话专用的原生 Hook 条目启动 Qoder CLI 1.1.64，
daemon 独立于前台 Agent。服务执行记录不能认证 Agent 已采用响应。

## 组件职责

| 组件 | 职责 |
| --- | --- |
| `aw-service::Server` | 私有端点、配置身份、绑定与事件生命周期、有界 RPC 和审计存储 |
| `aw-service::Client` | 使用调用方统一截止时间的版本化请求，不自动重试 |
| `aw-host` | Provider 发现、配置校验、准入及事件步骤的单次调用 |
| `aw-exec` | 子进程截止时间、输出限制、取消及进程组清理 |
| `aw-core::journal::FileJournal` | 持久预留和可验证的元数据记录链 |
| Qoder Adapter | 核实受支持的原生版本、归一化回调、保留原生调度并采用支持的效果 |
| `launch_service` | 解析私有路径，核实已有配置版本或按需启动服务 |

服务将 `FileJournal` 作为存储复用，不执行 Core 计划，也不产生 Core 执行或采用
凭据。Provider 私有设置保留在 `spec.providers.<name>.config` 中，服务不内置
安全引擎规则。

## 本地协议

[wire.rs](../../crates/aw-service/src/wire.rs) 定义实验性协议
`aw-service/v1alpha1`，与 Provider 的 stdio 协议 `aw-provider/v1alpha1` 分开。
每条连接承载一个带长度前缀的 JSON 请求及一个响应，每帧上限为 8 MiB。原生 stdin 和归一化事件各自仍受 1 MiB 限制；
帧空间包含字节数组编码为 JSON 后的膨胀。

请求包含 `api_version`、`identity`、`deadline_ns` 和 `operation`。
首次 `status` 请求可以不提供身份，后续请求必须匹配本次服务启动生成的 generation
和原始配置字节的 SHA-256 revision。即使文件没有变化，重启也会使旧客户端与句柄
失效。修改文件不会热更新正在运行的服务。

| `operation.method` | 信封以外的输入 | 结果 |
| --- | --- | --- |
| `status` | 无 | PID、资源数量、审计健康状态及尚未验证的采用状态 |
| `bind` | `target`、可信 `capabilities`、绝对 `cwd`、完整 `environment` | 服务签发的 `instance_id`、身份和准备阶段 `audit_key` |
| `unbind` | `instance_id` | 在全部事件关闭后释放绑定 |
| `release_instance` | `instance_id` | 取消并等待本实例清理，再释放绑定，不停止共享服务 |
| `open_event` | `instance_id`、归一化 `event` | 事件 ID、实例 ID 和已准入步骤 ID |
| `open_hook_event` | `instance_id`、归一化 `event`、原始 `native_input` 字节 | 共享原生事件句柄，后续回调沿用原截止时间 |
| `invoke_step` | `event_id`、`instance_id`、`step_id`、可选 `native_environment` | 结构化候选效果、显式原生字节/状态或执行失败，以及调用元数据 |
| `close_event` | `event_id`、`instance_id` | 取消未完成工作、等待子进程回收并确认关闭 |
| `audit` | 准备阶段或事件的 `key` | 已验证的持久记录信封和 `terminal` 标志 |
| `stop` | 无 | 确认收到停止请求；进程退出才表示清理完成 |

`bind` 对结构化 Provider 实际执行 `describe` 和 `validate_config`，显式原生
命令不执行该握手。能力证据来自可信的同用户
调用方，不能来自 Provider 的自述。准备阶段固定绑定的工作目录、显式环境和配置
命令。Qoder 启动器将 `QODER_PROJECT_DIR` 和 `CLAUDE_PROJECT_DIR` 固定为该已核实的目录，
保持命令兼容性，并根据启动快照还原 Qoder 的来源、版本及站点元数据。不会把任意回调环境
或原生 Hook 条目的 `env` 复制到 Provider 上下文，后续请求也不能修改。准备失败时，响应可能包含 `audit_key` 而没有成功绑定；
Rust 客户端通过 `Error::Attempt` 暴露该记录键。

环境变量全部为 UTF-8 时，仍使用 JSON 字符串映射；否则使用 `[key_bytes, value_bytes]`
键值对数组，其中键和值均为字节数组。Linux 环境字节在 Agent、原生 Hook 和 Provider
执行中保持不变。空键、重复键、NUL 字节及键中的 `=` 会被拒绝。

绑定准备期间，连接关闭、服务停止或期限到达都会取消 Provider 工作，并在清理后释放
绑定槽位；绑定响应写入失败也会释放刚创建的实例。`aw run` 等待该响应时会响应终止
信号；可复用客户端提供 `call_cancellable`，供调用方传入自己的取消标志。
准备阶段已经产生的副作用不回滚。

原生命令可通过 `invoke_step.native_environment` 传入完整回调环境，使用相同的字节保留
编码替换绑定环境，
不做合并；省略时沿用绑定环境。结构化 Provider 忽略该字段。快照最多 4096 项、
1 MiB，拒绝非法键和 NUL，不进入事件输入、关联摘要或审计。非法快照会占用步骤
执行权并返回明确执行故障，不延长事件期限。这是 RPC 字段，不是新增 `aw.yaml` 配置。

当前服务准入结构化 `tool.before` 的 `observe`/`block`、`tool.after` 的 `observe`，
以及这两个工具点位的显式原生步骤。
启用尚不支持的事件、结构化 `ask`、结果替换、最终检查或更强执行保证会被拒绝。成功响应
中的空效果列表不增加限制，也不授予原生权限。成功的策略阻断与调用失败后
`failure_action` 为 `block` 是两种不同结果。

## 共享事件生命周期

Adapter 只打开一次事件，再按原生框架要求的顺序和并发方式发送各个 `invoke_step`
请求。客户端克隆可以并发调用不同步骤。服务不额外决定串并行策略，也不重新执行
失败步骤。一个步骤在同一事件中只能尝试一次。

Adapter 必须在属于同一原生事件的回调之间传递返回的句柄。原生 session 或 tool-call
ID 是关联数据，不能证明两个回调属于同一个服务事件。`open_hook_event` 在已准备
实例内按非空 session/tool-call ID 和事件名关联，后续打开要求归一化事件与原始字节
均一致。已关闭事件和失败打开的声明保留到实例释放，不能借此重新获得预算。为每个
步骤打开独立事件不符合共享事件调度。当前每个实例最多关联 1,024 个事件。

Qoder Adapter 在同一 matcher 组内为每个 AW 步骤登记一个回调。默认并行及匹配组
sequential 行为仍由 Qoder 决定。每个回调打开或加入其事件，只占用自身步骤；即使
Qoder 没有调用剩余回调，未完成事件仍会过期。当前原生输入不可变：顺序命令返回
`updatedInput` 后，下一回调 stdin 可能变化，因此被判为关联不匹配，并按该步骤的
`on_error` 处理。支持重写链需要在共享截止时间下另行定义逐步骤输入快照合同；
转交原始输出不代表已支持这种组合或跨框架审批。

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
socket。`auto` 将路径选择交给启动方，`aw serve` 仍要求 `--state-dir`。
`aw run` 实现按需启动，`external` 要求已有服务。两个路径均为 `auto` 时，启动器
选择 `$XDG_RUNTIME_DIR/aw/<revision-prefix>`；未设置运行目录变量时使用
`/tmp/aw-UID/<revision-prefix>`。显式提供的运行目录必须已存在、属于当前用户并
具备私有权限。路径为适应 Unix socket 长度使用版本前缀，复用仍校验完整配置版本。

启动阶段持有状态目录专属的私有锁，并写入不可变配置快照。新服务使用独立 session、
空 stdin/stdout 和私有 stderr 日志。已有服务通过身份及审计健康检查后才复用；
未知或遗留端点不会被静默替换，启动失败只清理本次创建的子进程。不安装 supervisor。
`status`、`stop` 可通过 `--config FILE` 使用同一解析器，也可使用显式 socket。

前台启动器拥有自己生成的配置和绑定文件。Agent 退出后调用 `release_instance`，
取消并等待本实例清理，移除自身文件，其他实例与 daemon 继续运行。释放超时会让
实例保持清理状态并拒绝新事件；后续 release 可完成清理，不重新执行事件。

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

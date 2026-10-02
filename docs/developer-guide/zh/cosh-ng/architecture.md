# cosh-ng 架构

[English](../../en/cosh-ng/architecture.md)

cosh-ng 将交互式终端、Agent 运行时、确定性的操作系统 API 和 Gateway Task Plane 分开。
每个边界都能独立测试，也可以由其他程序单独集成。Package Gateway path 有意保持窄范围：
它围绕 contained Core Runtime 提供 durable local Task coordination，不是通用的 remote capability service。

## 上游系统视图

```text
bash/zsh <--- cosh-shell
                  |
                  | JSONL
                  v
              cosh-core
                  |
                  +--> provider / tools / MCP
                  |
                  +--> cosh-platform ---> cosh-types

caller ---> cosh-cli ---> cosh-platform ---> cosh-types

caller ---> cosh agent task ---> cosh-gateway（local Unix/systemd）
                                      |
                                      +--> Task/Event/Outbox SQLite
                                      +--> contained cosh-core

cosh-cli checkpoint -----------> existing ws-ckpt path（separate）
```

安装后的 `cosh` 启动器通常执行 `cosh-shell raw cosh-core`。`cosh-shell` 编译时不依赖工作空间中的其他 crate，运行时则维护一个长时间存活的 cosh-core 子进程。两端都可能独立失败或重启，因此 stdin/stdout 协议需要保持向后兼容。

Gateway addition 保持现有 Shell/Core/CLI path 不变。`cosh agent task` entrypoint 是 local Unix
control surface，不是 Shell slash-command surface，也不开放 network listener。现有的
`cosh-cli checkpoint` path 与 Gateway profile 独立，可以继续使用该 CLI domain 文档中的 ws-ckpt protocol。

## Gateway Task Plane

Gateway addition 增加两个 library crate 和一个安装后的 local entrypoint：

```text
cosh-gateway-contracts --> TaskAggregate --> SQLite Task/event/receipt/Outbox transaction
        |
        +---------------> generic Capability/Permit/Execution contract

cosh-gateway ----------> contained RuntimeSupervisor --> private COSH JSONL bridge
        |                          |
        +--> local Unix Task API   +--> fixed `core`/`gateway-brokered-v1` profile
        |
        +--> direct ACP `doctor`/`run` path（不受 Task Plane 治理）

task-only inventory：`ask_user_question`
```

Task reducer、SQLite store、Runtime supervisor 与 Outbox scheduler 组成 local control plane。
Package `serve` entrypoint 在 bind socket 前必须通过 live systemd containment 校验，会规范化配置的
workspace，并接纳固定的 `workspace/cosh/task-only-v1` target 与 `core`/`gateway-brokered-v1`
selector。Gateway crash 后，system manager 仍然拥有完整 Runtime cgroup。

Task-only profile 有意保持 execution boundary 无副作用。它的 Runtime inventory 只有
`ask_user_question`，不提供 checkpoint、write、Shell、slash command、Web、channel 或 remote
capability，也没有需要 approval 的 side effect。Task API 仍暴露 `submit`、`get`、`events`、
`append`、`cancel`、`retry` 和 `resolve-approval`，以便 durable contract 支持后续 profile。
`append` 回答 pending question；这个 profile 不会产生 approval flow。

Direct ACP `doctor` 与 `run` 仍可用于本地 Adapter interoperability，但它们在 durable Task Plane
之外启动 Adapter，不受 task-only inventory 治理。当前 Shell path 同样不变：`cosh-shell` 拥有
native PTY 与 compatibility cosh-core process。Shell slash command 仍属于 Shell，不是 Gateway command。

## Crate 职责

| Crate | 二进制 | 拥有 | 不应拥有 |
|---|---|---|---|
| `cosh-types` | 无 | 无副作用的响应、错误、配置、审计和现有 checkpoint wire type | 操作系统访问或运行策略 |
| `cosh-platform` | 无 | 发行版检测、软件包和服务适配器、审计策略与存储，以及供 `cosh-cli checkpoint` 使用的现有 ws-ckpt 客户端 | CLI 展示、Gateway Task policy 或 Agent 交互 |
| `cosh-cli` | `cosh-cli` | Clap 命令、JSON 响应、退出状态 | 平台适配器之外的发行版分支 |
| `cosh-core` | `cosh-core` | 模型服务、工具循环、Hooks、Skills、MCP、Extensions、注册表、会话和压缩 | 终端控制或前台 PTY 交互 |
| `cosh-shell` | `cosh-shell` | PTY 宿主、输入路由、卡片、审批、终端证据、界面、core 进程生命周期 | 模型服务实现或直接抽象操作系统 API |
| `cosh-gateway-contracts` | 无 | 无副作用的 Task、Runtime、Capability、identity、header 与 error contract，leaf string/digest 有界 | Storage、process ownership、transport、provider 或 OS execution |
| `cosh-gateway` | `cosh-gateway` | Durable Task reducer/store、Outbox scheduler、contained Core Runtime bridge、local Unix Task API 和 direct ACP entrypoint | Shell PTY、checkpoint/write target、remote listener、Shell slash command 或未治理的 side effect |

## 交互数据流

1. `cosh-shell` 在 PTY 中启动 bash/zsh，并安装 OSC 生命周期标记。
2. 输入路由把 Shell 语法发送给 PTY，把斜杠命令发送给本地控制入口，把自然语言发送给 Agent 适配器。
3. 默认适配器维护 cosh-core 进程，每轮 Agent 对话发送一条 JSONL 用户消息。
4. cosh-core 解析工作区配置、模型服务、Skills、Extensions、MCP 工具和会话状态，随后流式返回事件。
5. cosh-shell 治理这些事件，并渲染文本、问题卡片或审批卡片。
6. 经过审批的 Shell 命令交回前台 PTY。OSC 终端证据与 Agent 任务关联，并在 core 请求时返回。
7. Extension 重载等注册表修改复用同一个长期运行的 core，并在安全的版本边界发布。

## 确定性 CLI 数据流

```text
Clap command
  → command module 校验参数
  → cosh-platform 选择后端
  → 后端返回类型化数据或 CoshError
  → cosh-cli 输出 CoshResponse<T>
  → 成功退出 0，操作失败退出 1
```

软件包和服务写操作支持 `--dry-run`。现有的 `cosh-cli checkpoint` domain 通过 Unix socket，
以 bincode 和四字节小端长度前缀进行通信；这条 ws-ckpt path 与 task-only Gateway profile 独立。

## cosh-shell 模块职责

| 目录 | 职责 |
|---|---|
| `shell_host/` | PTY 生命周期、OSC 解析、Shell 集成、raw relay |
| `raw_input/` 和 `input/` | 终端模式、多行输入、输入 relay |
| `slash/` | 斜杠命令解析、注册和展示 |
| `adapter/` | 模型服务与 core 适配器、控制协议传输 |
| `agent/` | Agent 任务生命周期和受控事件 |
| `runtime/` | 编排、共享状态、分发和启动 |
| `approval/` 和 `question/` | 用户决策和控制响应 |
| `hooks/` | Hook 策略和执行，通过运行边界交接修改 |
| `tools/` | 命令风险模型、只读规则和工具展示 |
| `ui/` | 终端渲染和卡片组件 |
| `evidence/`、`journal/`、`ledger/` | 有范围限制的终端证据和决策记录 |

不要在 `cosh-shell/src/` 根目录新增实现文件。保持模块边界清晰，并在结构改动后运行 `crates/cosh-shell/scripts/check-layout.sh`。

## 兼容性和安全契约

- `CoshResponse<T>` 是稳定的自动化信封。
- 现有 `cosh-cli checkpoint` 的 ws-ckpt enum 顺序属于它的二进制 wire format；task-only Gateway
  不依赖这个 daemon。
- cosh-core 消息使用逐行 JSON，headless 模式的 stdout 不能混入日志或界面文本。
- 正在运行的 Agent 任务固定使用启动时的注册表版本。新版本检查通过后，在空闲时立即启用，否则等待安全时机。
- 会话状态按工作区隔离。恢复只还原模型可见对话，不还原历史终端证据。
- Core 读取工具固定在启动时规范化的工作区。后续 `cd` 只改变 Shell 目录，不会移动读取边界。越过路径或挂载点时会拒绝访问。
- 前台 Shell 交接串行执行。只有内核证据表明前台进程正在等待输入时，才应用输入等待超时。管道和全屏程序不受此限制。
- Linux 包路由可使用 `ID_LIKE` 中第一个可识别家族，但 typed 和 JSON 输出仍保留发行版的真实 `ID`。
- 工具自动审批在无法判断时拒绝执行。直接匹配原始命令子串不能充当安全边界。
- Gateway Task submission 固定使用 `workspace/cosh/task-only-v1` 与接纳的
  `core`/`gateway-brokered-v1` selector。Durable API 使用 idempotency key，因此客户端 I/O
  不确定时可以重试而不会重放未知 side effect。

## Gateway 与 ACP 交付边界

Package slice 是 durable local Task Plane，不是通用的 production Gateway。它支持的边界是
contained `core`/`gateway-brokered-v1` Runtime、固定的 `workspace/cosh/task-only-v1` target、
local Unix Task API，以及唯一的 `ask_user_question` inventory item。Checkpoint、write、Shell、
slash command、Web/channel 和 remote capability 都不在这个 profile 中。Generic approval 与
permit contract 仍可供后续 profile 使用，但这个 profile 没有需要 approval 的 side effect。

Direct ACP `doctor`/`run` 是 interoperability entrypoint，不是受治理的 Gateway Runtime。它们
有意不声明 Task durability、capability admission 或 remote execution。

继续阅读[开发 cosh-ng](getting-started.md)、[IPC 协议](ipc-protocol.md)和[测试](testing.md)。

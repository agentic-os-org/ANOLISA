# 配置与 Agent 启动

[English](../../en/aw/configuration.md)

`aw.yaml` 保存 Agent 入口、启用的 Provider 和运行数据位置。
模型账号仍使用 Agent 自己的配置。首次配置先完成[安装](installation.md)。

## 生成配置

沿用安装页的变量。`config init` 不覆盖已有文件；修改配置见下一节。

```bash
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/aw.yaml" --state-dir "$AW_HOME/state" \
  --qoder "$AW_QODER"
```

| 需要的能力 | 在生成命令中增加 |
| --- | --- |
| 只启动 Agent | 不增加 Provider 参数 |
| 工具输出压缩 | `--provider tokenless` |
| 工具安全检查 | `--provider sec-core --socket /run/agent-sec-core/daemon.sock` |
| 安全检查和压缩 | `--provider sec-core+tokenless --socket /run/agent-sec-core/daemon.sock` |

先安装所选 Provider；生成器会填入程序路径和校验信息。

## 修改已有配置

退出使用这份配置的 Agent，用原配置停止服务后再编辑。配置文件不会被监听；
已显式启用稳定桥接的实例可使用下述空闲更新流程。

```bash
aw stop --config "$AW_HOME/aw.yaml"
cp "$AW_HOME/aw.yaml" "$AW_HOME/aw.yaml.bak"
aw config show --config "$AW_HOME/aw.yaml"
```

可以直接编辑 YAML，也可以用 `aw config` 增删 Provider 和 Hook。
文件与父目录应属于当前用户，且不能被其他用户写入；配置编辑器要求绝对路径，
不接受符号链接。`show` 会显示配置中的私有值，分享输出前请自行删去敏感内容。

```bash
aw config validate --config "$AW_HOME/aw.yaml"
aw run --config "$AW_HOME/aw.yaml" --agent qoder
```

校验成功会打印 `configuration valid`。它检查字段和引用，实际程序是否可用会在启动时检查。
YAML 中的路径须写成真实绝对路径，AW 不展开 `$HOME` 等变量。
完整编辑命令和自定义规则示例见[配置工具参考](packages.md#编辑已有-provider-与-hook-配置)。

## 设置 Agent 退出宽限期

应用的退出处理需要更多时间关闭请求和 SDK 客户端时，在所选 Agent 的配置中设置
`spec.agents.<target>.shutdown_grace_ms`。该值为 `1` 到 `120000` 毫秒的整数，
省略时保留默认 `2000` 毫秒。例如，`shutdown_grace_ms: 15000` 从前台 launcher
首次向自有 Agent 进程组转发外部退出信号起，给予该组 15 秒宽限期；宽限期结束后，
AW 开始强制清理。重复退出信号不会重新开始计时。转发信号前的等待与最终进程清理
耗时不计入该区间，因此该字段不是整个退出操作的总期限。

该设置不改变内部 readiness 或生命周期失败时强制清理前的两秒宽限期。Agent 自然
退出后，立即开始清理其遗留子进程。它也不改变 `aw stop` 的服务退出行为，或通过
`aw agent connect` 接入的独立启动 Agent 的生命周期。

应用仍需停止接收新请求、排空工作并关闭 SDK 客户端。增加此宽限期不会自动完成这些
操作，也不保证 SDK 清理成功完成。请协调应用期限、容器或监督器的退出预算与该值，
并在实际部署镜像中验证退出行为。

修改仅对新启动的 launcher 生效，策略更新不能改变该字段。旧版 AW 二进制会拒绝
包含此字段的配置。回退前删除该字段，并按恢复后的默认两秒安排退出流程。

## 比较策略修改

准备独立的候选文件，与现有配置比较：

```bash
aw validate --config next.yaml --against current.yaml
```

两份文件都经过完整校验，只允许 Provider、事件和默认事件预算变化。
Agent 入口、原生调度、服务路径、审计、观测和元数据必须相同。
成功打印 `policy update scope valid; runtime activation is not checked`。
比较不运行 Provider、不修改文件、不检查原生就绪，也不更新运行中的 Agent。
启用稳定桥接的实例可使用下述显式空闲更新；其他实例使用前述停止、编辑、启动流程。
原有 `aw validate --config FILE` 继续打印 `configuration valid`。

`aw config validate` 与 `aw validate` 等价，支持 `--against` 和相对文件路径，
不执行编辑器的所有权检查。独立 `aw-package` 命令及其更严格的编辑器验证已删除。
初始化、查看和编辑继续使用 `aw config init`、`aw config show` 与编辑命令；
这些动作的文件安全检查并不意味着 `aw config validate` 替代了已删除的严格验证器。

## 注册原生 Hook

对于支持持久原生注册的 Agent，使用 `aw agent connect --config FILE --agent TARGET`，
并提供所需的原生选项，例如 Hermes 的 `--native-profile`。它沿用既有 Adapter 注册
和支持性检查，不安装 Agent 或软件包。旧的 `aw install --config FILE --agent TARGET`
语法仍兼容；与 `--prefix` 或 `--bundle` 混用会被拒绝。

## 更新预热实例

需要保留预热宿主并更新 AW Hook 时，在原启动入口显式启用稳定桥接：

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder --reloadable
```

运行时路径支持已验证的 Qoder 1.1.73 TUI、QwenPaw 2.2.2b4 / AgentScope 2.0.9 App，
以及 Qoder SDK 1.0.54 / bundled Worker 1.1.74。SDK 目标使用
[显式 daemon 和 Query 工厂](../../../developer-guide/zh/aw/native-agents.md#启动-qoder-sdk-worker)；
实例内所有受管 session 都须空闲，包括原生清理完成。
QwenPaw 按原生接入参考选择配置目标并设置已初始化的 `QWENPAW_WORKING_DIR`。
普通 `aw run` 保持现有行为；未预装稳定桥接的实例不能通过此命令升级。外部启动的原生进程可使用下方的安装流程。

启动或连接后，继续编辑同一份 `aw.yaml`。在另一个终端显式应用其中的完整期望策略：

```bash
aw update --config "$AW_HOME/aw.yaml" --agent qoder
```

AW 记录该配置路径已经连接的服务身份。编辑或原子替换文件不会改变连接、重启 Agent
或自动激活策略。busy、验证失败或冲突时，旧策略继续生效；文件保留期望修改，供显式重试。
保持启动字段不变，通过 `aw service status --config` 查看当前生效版本。
这些命令应使用相同的文件路径和运行时目录。

`qoder-sdk` 目标使用相同更新流程；目标配置名为 `sdk` 时，选择 `--agent sdk`。
所有受管 Query 共用实例策略；请求、后台任务清理和 Query 关闭期间保持 busy，
直到原生清理完成。原服务仍在运行时，使用已修改的文件重新启动会被拒绝。
再次启动前，先关闭 Worker 并停止该服务。

仍支持独立候选文件：`aw update --config aw.yaml --agent qoder --policy next.yaml`。
此形式应用 `next.yaml`，不改写任何文件。之后若使用单文件更新，先将完整期望策略写入
`aw.yaml`；AW 不会将一次更新按补丁自动合并。

命令选择该目标唯一的存活实例，并读取当前策略版本。这个版本仍用于乐观并发校验：
若另一个更新先完成，本次返回冲突，不会自动重试。没有匹配实例时报错；多个匹配实例时
要求补充 `--instance ID`。通过 `aw status --config "$AW_HOME/aw.yaml"` 查询实例 ID 和版本。
选择范围包括未就绪和未启用热更新的实例，不会在忙碌实例旁边默默选择一个空闲实例。

需要显式选择实例或接入外部管控时：

```bash
aw update --socket "$AW_SOCKET" --instance "$AW_INSTANCE" \
  --policy next.yaml --expected-revision "$AW_POLICY_REVISION"
```

`--socket` 替代 `--config`；`--instance` 也可用于消除 `--agent` 的多实例歧义。
`--expected-revision` 可选，在调用方需要指定前置版本时覆盖查询到的版本。冲突不自动重试。

成功返回实际激活的 `policy_revision`、前版本、审计 key、`preparation_ms` 和 `activation_ms`。
命令不修改配置、不重启原生宿主，也不会启动服务。保存的连接使 `aw update`、
`aw status` 和 `aw stop` 在策略编辑后仍访问原服务。该服务存活时，用已编辑文件再次
启动 reloadable 实例会在创建新 daemon 前被拒绝；请更新已有实例，或先停止服务再用
编辑后的配置启动。新 binding 初始使用服务默认策略；native attach 在报告成功前
应用当前期望策略。

热更新模式下，生命周期确认缺失或无法取得激活计划时，`tool.before` 会阻断。
成功取得计划后，每个步骤仍使用配置的 report/block 错误策略。

活跃 Agent 请求（含没有工具执行的模型等待）返回 `busy`，旧策略保持生效，更新不排队，
之后也不会自动应用。准备失败、版本冲突和不支持的原生策略同样保留旧策略。
超时无法确定是否已经生效时，先查询实际版本再派发工作。管控侧必须独占分配实例，并将
更新与 Session 派发串行化；成功确认不等于预留下一条请求。同一 App 的多个 Session 共用
实例策略，任一活跃请求都会阻止切换。

在创建或派发首个用户 Session 前完成策略更新。模型账号、原生 Agent 定义由框架自己的
Session 配置处理；通过 AW 修改这些字段仍需另起实例。Qoder 原生工具 Hook 使用顺序执行时，AW 目标也须设置 `qoder.sequential: true`，以保留相同调度。
Qoder 的 `Stop` 不等于空闲，须等
原生 idle_prompt 通知。QwenPaw 在正常耗尽流后释放请求/工具占用；确认完成前提前关闭流、取消或异常退出
无法确认原生清理完成，因此保留 busy。AW 在原生清理完成后交付最终响应及终结错误/中断片段，
因此正常返回的结构化工具错误不会阻止后续更新；无法确定结束时
使用新实例。

本 Preview 已有 CLI 接入回归和固定版本 AgentScope middleware 回归。原生清单还要求
独立校验和固定的 Qoder 1.1.73 TUI 启动及派发前空闲切换用例。必需的 QwenPaw App HTTP 测试
使用确定性本地模型和真实 shell/read_file 工具，验证同一进程在成功及结构化错误结果后的多次策略切换。完整的托管 Qoder TUI
工具轮次采纳仍需独立验收。

## 选择 Agent

一个配置可以保存多个目标，`--agent` 选择 `spec.agents` 中的名称。
生成器目前支持 Qoder 和 OpenClaw；QwenPaw、Hermes 及 SDK 应用使用
[原生接入参考](../../../developer-guide/zh/aw/native-agents.md)。

| Agent | 当前要求 | 使用入口 |
| --- | --- | --- |
| Qoder | CLI 1.1.64 或更新版本，原生 Hook 可用 | 交互或 print |
| OpenClaw | 2026.9.6，已初始化原生配置和状态目录 | 新启动的 Gateway |
| QwenPaw | 2.2.2b4，AgentScope 2.0.9 | App/API |
| Hermes | 使用[兼容版本及安装步骤](../../../developer-guide/zh/aw/native-agents.md#启动-hermes) | 本地 chat |
| AgentScope、Pi、Claude SDK | 需要在应用中显式接线 | [SDK 接入参考](../../../developer-guide/zh/aw/native-agents.md) |

### Qoder

从希望它操作的目录启动。`--` 后面的参数交给 Qoder。

```bash
aw run --config "$AW_HOME/aw.yaml" --agent qoder
aw run --config "$AW_HOME/aw.yaml" --agent qoder -- -p 'Read the current directory with a tool.'
```

已有附加设置文件时，使用 AW 的 `--native-settings /absolute/qoder.settings.json`。
AW 会保留原有设置并加入本次回调。

### OpenClaw

先停止使用同一原生 profile 的 Gateway。将变量改为已有安装和 profile 的绝对路径。

```bash
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw.mjs
export AW_OPENCLAW_CONFIG=/absolute/path/to/openclaw.json
export AW_OPENCLAW_STATE=/absolute/path/to/openclaw-state
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/openclaw.yaml" --state-dir "$AW_HOME/openclaw-aw-state" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
aw run --config "$AW_HOME/openclaw.yaml" --agent openclaw \
  --native-settings "$AW_OPENCLAW_CONFIG" --native-state-dir "$AW_OPENCLAW_STATE"
```

等待 `AW openclaw native hooks ready` 后，通过连接该 Gateway 的客户端使用。
需要压缩或安全检查时，在生成配置时增加上表参数。`agent --local` 会绕过这个 Gateway。

## 添加自己的规则

简单规则可以从 stdin 读取工具事件，向 stdout 输出 JSON `true` 或 `false`。
配置决定命中时观察还是阻断。执行故障由 `on_error` 处理，安全检查通常选 `block`。
按[布尔规则教程](../../../developer-guide/zh/aw/integration.md)准备脚本并验证允许和阻断两种情况。

Pi / Claude 常驻目标沿用同一配置格式，按 [SDK worker 说明](../../../developer-guide/zh/aw/native-agents.md#pi-和-claude-sdk-常驻宿主) 选择常驻入口及公共工厂。

## 接入已启动的原生进程

容器控制器可以照常启动 Qoder 1.1.73 TUI 或 QwenPaw 2.2.2b4 /
AgentScope 2.0.9 App，在派发首个请求前安装 AW。进程、终端和生命周期仍由控制器
管理。启动时记录 PID，在安装成功前保持请求派发暂停。

这些精确版本既是当前 Preview attach/update 路径允许的支持集合，也是实际验收版本。
尚未建立更宽的兼容范围；普通 `aw run` 的 Qoder 最低版本要求不代表此路径的支持范围。
扩展版本前，需要验证原生加载确认、完整请求空闲和移除契约。

| 原生入口 | 当前 Preview 的 attach/update 支持 |
| --- | --- |
| Qoder TUI | 支持 1.1.73；输入前通过 ConfigChange 确认，请求后等待原生 idle-prompt 确认 |
| Qoder 单次 `-p` / `--print` 执行 | 普通 `aw run` 支持 print；attach/update 不接纳此模式 |
| Qoder 常驻 headless 输入 | 尚未接纳或验收；需要单独验证输入前 readiness、多次请求及请求结束/空闲边界 |
| QwenPaw `app` | 支持 2.2.2b4 / AgentScope 2.0.9；它是 HTTP 后端，可通过 API 无界面使用，无须打开 UI |
| QwenPaw ACP / TUI | 不支持；固定版本的 TUI 使用 ACP 子进程，不经过 App 的 HTTP 插件管理端点 |
| QwenPaw `desktop` | 不是受支持的 attach 目标；它通过 `python -m qwenpaw` 启动独立 App，其入口及进程归属尚未纳入本版直接 `qwenpaw app` 的验收 |

容器预热时，使用已初始化的 profile 和 loopback 监听直接启动 `qwenpaw app`。
真实 attach 验收通过 HTTP 请求执行，无须桌面或浏览器会话。

Qoder SDK 的 `startDaemon()` 及其 Worker/session 控制是独立于 TUI 的入口。
PID attach 不提供每个 session 的策略隔离；SDK daemon 需要单独验证原生生命周期和隔离能力。
已支持的 QwenPaw App 版本也不代表旧版本兼容。

```bash
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID"
aw update --config current.yaml --agent TARGET
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID" --remove
```

`aw install --config ... --agent ... --pid ... [--remove]` 保留为相同操作的兼容入口。

分阶段增加 Hook 时，直接编辑同一份完整期望配置，保留已有 Provider 和步骤。
现有 `aw config add-provider` 和 `aw config add-hook` 支持 `--definition`，
用法见[配置工具参考](packages.md#编辑已有-provider-与-hook-配置)。
这些操作属于离线编辑；验证文件后，在空闲时显式执行 `aw update` 激活。
无须单独维护启动配置副本。之后用相同路径重新 attach 时，会先应用文件中的当前期望策略，
再报告连接成功。初始化工具当前生成 Qoder/OpenClaw 配置；
QwenPaw 目标按上方链接的原生集成参考准备。

连接命令在获得原生确认后返回实例、当前策略版本和进程身份。后续策略
变更复用 reloadable `aw run` 的 `update` 命令。`--remove` 检查完整的原生请求和
工具执行边界；忙碌时返回错误，保留保护且不排队。移除保留原生进程、无关 Hook 和
用户配置修改。移除期间也应暂停请求派发。普通 `aw run` 和 Hermes 安装保持现有行为。

每个预热进程使用独占的原生配置作用域。AW 与 Agent 必须处于同一用户、PID 和
mount namespace。所选原生配置目录必须是规范路径、归该用户所有且权限为 `0700`。
已发现的共享进程和重复挂载会被拒绝；控制器也必须阻止其他进程后续进入同一作用域。

- Qoder 使用 `HOME/.qoder/settings.json`，启动时不传 `--settings`、自定义配置目录
  或禁用 Hook 的选项。AW 读取运行中进程的环境和参数，合并所属配置组，并在用户
  输入前通过原生 `ConfigChange` 确认加载。已有 settings 必须是同用户的普通文件，
  且不允许组或其他用户写入。文件原先不存在时由 AW 创建，移除后保留空的原生配置
  文档。工作目录信任和原生权限仍由 Qoder 决定。
- QwenPaw 使用运行中 App 已初始化的 `QWENPAW_WORKING_DIR`。App 应监听
  `127.0.0.1`；AW 验证所选 PID 拥有该端口。原生插件 API 安装带私有描述文件的
  现有 AW middleware，必须收到该 App 的确认；仅 HTTP 成功不足以表示接入成功。
  首版不支持单独托管的 runtime。
  同环境中的其他 App 也应在初始环境中显式设置绝对路径的
  `QWENPAW_WORKING_DIR`。当原生 `.env` 加载可能决定 peer 的配置目录或 HOME
  展开结果时，AW 会报告该 PID 并拒绝挂载。当前不存在 `.env` 仍无法确认启动时
  使用的目录；请使用显式 profile 重启该 peer，或先停止它再挂载。

QwenPaw 还要求与 AW 处于相同的 network namespace。已安装插件目录缺失时，
detach 会查询原生运行时状态。确认插件不存在后允许清理；插件仍加载或状态端点
不可用时，保留所有权、暂存插件和桥接文件用于重试。仍加载时先恢复原有插件目录，
或在 API 恢复后重试 detach，成功后再继续派发请求。

首次挂载要求进程处于首请求前的空闲状态。桥接建立之前已经执行的工作无法由 AW
重建。请先移除挂载，再停止外部进程；首版不监督该进程，也不运行自动挂载回收器。
移除失败时保留所有权记录和桥接文件，可用相同命令显式重试。原生插件被替换或
服务代次改变时拒绝操作并要求检查，避免删除其他所有者的资源。
Qoder 回滚同样等待原生移除确认。取消或安装失败后若无法确认卸载，AW 保留所有权和
桥接文件，待原生回调恢复后通过 `--remove` 重试。将 AW 配置组移动到其他 Hook 事件
会被视为所有权编辑，移除前应恢复原配置组。

安装进程终止后遗留的未加锁空所有权记录，可由 connect 复用或通过 `--remove` 清除；
不会抢占仍在运行的安装进程持有的锁。强制终止不提供事务恢复保证：SIGKILL 可能留下
本地暂存文件；成功收到 Bind 回复后、记录持久化前中断，也可能留下未记录的服务绑定。
这种情况下，应由所属 supervisor 退出受影响的服务并清理其私有状态。
正常取消或安装失败仍执行回滚；本 Preview 尚无崩溃恢复日志或自动垃圾回收器。

## 服务并发预算

Pi/Claude 常驻客户端跨 execution 保留会话，已完成 Hook 状态会回收。资源准入独立于历史调用数：

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

以上为默认值，省略字段保留默认，各字段接受 1..4096 整数。bindings 包含宿主及准备中、空闲、
排空中的客户端；events 为 live Hook event；calls 为活动 Provider 调用；connections 为全部
活动服务连接，包含控制 RPC；invocations_per_binding 为单个常驻客户端活动的模型工具调用数。
共享连接额度用满后，额外 16 条连接只接纳控制/清理请求。过载拒绝新准入，已完成工作释放容量。
Pi/Claude 常驻模式没有累计工具次数配额，
旧入口保留原累计关联限制。

aw status --config aw.yaml 的 resources.limits、resources.usage、peak_invocations_per_binding、
admission_rejections 和 rejection_reasons 展示容量、用量、峰值与拒绝原因。根据实际负载选择
并发数，审计磁盘和 SDK 历史另设保留策略。修改 daemon limits 后先排空客户端再重启服务。
CLI/daemon 升级或回退同样先排空、停止后一起更换：wire v1alpha7 拒绝旧活动会话，配置和 Provider
协议版本保持不变。

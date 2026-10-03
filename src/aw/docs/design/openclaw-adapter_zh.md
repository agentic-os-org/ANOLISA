# OpenClaw Gateway 适配器

[English](openclaw-adapter.md)

适配器以新的前台 Gateway 启动 OpenClaw 2026.9.6，将 AW 步骤注册到
`before_tool_call` 和 `after_tool_call`。同一份 `aw.yaml` 选择 Provider 和工具事件策略；
模型配置、凭据、原生插件及会话状态仍由 OpenClaw 管理。

## 启动与进程归属

```sh
aw run --config aw.yaml --agent openclaw \
  --native-settings /absolute/path/openclaw.json \
  --native-state-dir /absolute/path/openclaw-profile
```

Agent 的 `argv` 必须选择 `openclaw gateway run`，也支持 Node 可执行文件后接官方
`openclaw.mjs` 入口。适配器先核实 OpenClaw 版本，再把此次启动接入服务。
`--force`、重置和切换 profile 的选项会被拒绝。AW 不接管已有 Gateway，也不会为了
获取端口而停止已有进程。

两个原生路径均需显式提供。状态目录必须已经存在并属于当前用户。AW 在 Gateway
运行期间持有该目录的 `.aw-launch.lock`；OpenClaw 自身的 Gateway 归属检查也保持
生效。`OPENCLAW_ALLOW_MULTI_GATEWAY=1` 会被拒绝。锁文件作为私有空文件保留，
没有 AW 启动使用该 profile 时可以删除。

AW 在此次启动的私有目录下生成配置副本和插件，把 `OPENCLAW_CONFIG_PATH` 指向
副本，把 `OPENCLAW_STATE_DIR` 指向选定的持久 profile。不替换 `HOME`，不复制凭据，
不改写原配置。配置副本对 OpenClaw 只读，在启动清理后删除。原生配置须为展开后的
JSON，不含 `$include`，并设置 `gateway.mode: local`；适配器不解析 JSON5 或相对 include。

原有插件条目和加载路径保持存在。AW 追加自己的加载路径，在非空的显式插件允许列表中
加入 `aw-native-hooks`；配置禁用 AW 插件或已经占用同名条目时拒绝启动。
注册回执仅在原生 `gateway_start` 回调中写入。启动器检查私有令牌、适配器名称和注册数量。回执是同一用户下的注册证据，
不是进程身份认证；30 秒内未完成注册时，仅终止本次新建的 Gateway 进程组。

## 原生工具语义

| 点位 | 原生调度 | AW 行为 |
| --- | --- | --- |
| `before_tool_call` | 按插件优先级降序串行，同级按注册顺序 | 每个 AW 步骤注册一个默认优先级回调；阻断后原生链结束 |
| `after_tool_call` | 回调并发执行，返回值被忽略 | 每个 AW 步骤注册一个回调；记录观察，不改写工具结果 |

OpenClaw 向每个 before 回调提供原始参数的副本。已有插件返回的参数修改由 OpenClaw
合并，不会作为新事件快照逐步传给后续 AW 步骤。AW 保留这一原生行为，不声称检查
已经覆盖所有插件最后修改的参数。

回调携带原始 `{hook, event, context}` JSON。结构化 Provider 收到归一化事件及原生
载荷。`sessionId`、`runId` 和 `toolCallId` 为必需字段。AW 使用 run 与 tool-call 标识
生成稳定调用标识，同一事件的多个回调共享一个 daemon 期限，后续 run 也不会误用
旧事件。任意原生工具名称和参数对象均予以保留。

原生命令接收 Gateway 回调时的环境，包括进程启动后加载的值。结构化 Provider
仍使用服务固定的启动环境。环境不会写入原生 JSON 载荷或元数据审计。

事件预算限定为 1–12,000 毫秒，在 OpenClaw 的 15 秒 before-handler 超时前留出回调
传输时间。原生命令成功时的 stdout 须为空或 JSON 对象；对象返回给原生 Hook 执行器。
命令失败、信号、超时和非法输出遵循步骤的 `on_error`：before 可以阻断或报告，after
报告。结构化 block 返回 `{block:true}` 及原因，中性响应不会授予原生权限。
原生 `requireApproval` 输出仍属于 OpenClaw 特有能力，AW 不宣称支持可移植的结构化 `ask`。

## 验收边界

普通测试不需要模型或云凭据，检查回调载荷、失败处理、有界执行、配置保留和注册
归属。可选的 `openclaw-native.mjs` 测试直接运行固定版本的官方 Hook 执行器，验证
before 串行、阻断终止、插件共存及 after 并行。

OpenClaw 的部分执行路径不会等待 after 回调结束。因此，Agent 一轮完成本身不能
证明 after 回调已完成。运行时验收需要覆盖 Gateway 退出和 AW 实例释放时的取消
边界；强制终止进程后不保证事件继续送达。

Agent 执行器也会为被 before 阻断的工具发出 `after_tool_call`，载荷包含错误结果。
收到 after 事件不代表工具已经执行。验收分别核实允许命令的文件标记存在、阻断命令
的文件标记不存在，不以回调数量代替工具执行证据。

支持的入口为 Gateway 内的 Agent 工具生命周期。直接调用 operator `tools.invoke`
HTTP/RPC 接口不会携带相同的 session/run 标识，也不经过相同的 after 点位，因此
不视为等价覆盖。

当前 daemon 在一个绑定实例中最多保留 1,024 个原生事件记录，直到实例释放。
长期运行的 Gateway 可能达到该上限，正常一次工具调用通常占用两个事件。后续事件
失败按 `on_error` 处理，并显示诊断。生产保留策略和按会话更新实例属于后续工作。

本适配器不覆盖已有 Gateway 接管、工具结果持久化或结果中间件、通用审批对话框、
OS 强制执行或最后一道受保护安全检查。

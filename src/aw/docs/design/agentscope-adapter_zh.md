# AgentScope 公共工厂 Adapter

[English](agentscope-adapter.md)

`agentscope` Adapter 面向 AgentScope **2.0.6**，要求 **Python >=3.11**
（[SDK 要求](https://pypi.org/project/agentscope/2.0.6/)），通过明确接线的公共 `Agent` 工厂
工作。复用 AW 本地服务、binding、有界 `aw hook` 客户端、前台 launcher、readiness
与生成文件清理；不把 SDK 标记为 QwenPaw，也不依赖 QwenPaw 插件加载器。

## 公共工厂合同

`aw run --config FILE --agent TARGET` 选择配置中的 `adapter: agentscope`。
`argv` 首项为应用 Python 解释器，随后为 `.py` 脚本或 `-m MODULE` 与应用参数。
不支持 `-I`、`-E`、`-c` 等解释器覆盖选项。有界 preflight 使用同一解释器核对
`importlib.metadata.version("agentscope") == "2.0.6"`。

AW 向前台 worker 提供自有 `aw_agentscope.py` 导入目录和 `AW_NATIVE_CONFIG`。
应用显式修改公共工厂：

```python
from aw_agentscope import AwBridge

bridge = AwBridge()
agent = bridge.create_agent(
    name=name,
    system_prompt=system_prompt,
    model=model,
    toolkit=toolkit,
    state=state,
    middlewares=existing_middlewares,
)
```

Bridge 调用真实公共 `Agent` 构造器，在现有列表后追加 AW middleware；构造成功后
才写 readiness。launcher 拒绝提前退出、没有 readiness 或来自其他 PID 的回执。
Bridge 不 monkeypatch SDK，也不自动发现应用里的任意 Agent。**每个新建、恢复或
重建 Agent 的工厂均须使用这个入口。** 首个 Agent 的回执只证明该工厂已接线，
不能证明其他工厂或路由的覆盖。完成工厂构造后再接收请求。

已有可信参数注入和委派 wrapper 应排在 AW middleware 前面，使 before 事件获得
最终 `tool_call.input`。AW 后面的 middleware 仍可改写参数，属于此合同以外的路径。
不替换或放宽原生权限门禁。

## 原生工具语义

Bridge 保持三条约束：被接受的策略输入保留 SDK 解码后的参数值和类型；每个阶段使用同一
事件预算与取消标记，收尾不会创建新事件或重置预算；执行权保留给 Toolkit，仅获授权的
最后一个 PostTool 步骤可以替换完整响应。超出传输值域的数据进入 `on_error`，而非舍入或改写。
传输截止时间覆盖子进程创建和通信，不会抢占 SDK 解码、工具执行或宿主文件系统操作。

AW 使用 AgentScope 的 `on_acting`，位于原生参数校验和权限检查之后，直接包围
`Toolkit.call_tool`。

- `tool.before` 等待每个 step 后才进入 Toolkit。退出码 2 转为原生
  `ToolResponse(state=DENIED)` 并跳过工具。传输失败按 step 的 `on_error` 处理：
  `block` 拒绝；`report` 记录诊断后继续。
  Bridge 在解码 Hook JSON 后立即拒绝不支持的审批指令，包含带 BOM 的 UTF-8 和
  UTF-16/32。CLI 保留原生字节，不再用编码规则不同的第二个 JSON parser 校验指令。
  JSON 解码递归失败按 `on_error` 处理：before 的 `block` 拒绝；`report` 与 after
  回调保留原生结果并继续后续步骤。
  在最后一个 step 之前拒绝时立即关闭共享 before 事件，未进入的 step 不会占用
  活跃事件容量直到预算到期。
- after wrapper 在 before wrapper 外层，因此 `tool.after` 会观察包含 AW 拒绝在内的
  原生 final response，即使工具没有执行。中间 `ToolChunk` 不触发 after。Toolkit
  普通异常和原生 interrupted response 保留原始状态及内容。
- before step 按配置顺序运行；只有观察的 after 链保持配置逆序退出，遵循 AgentScope
  洋葱语义。含一个最终 `replace_result` 步骤时反向注册，使 after 按声明顺序执行：
  观察步骤和原生用户 Hook 先收到原始结果，压缩再修改模型可见的最终文本。
  回调故障按 report 处理后仍继续压缩。所有步骤共享 deadline 和 once-only 声明，
  事件预算仍为 **1..55000 ms**。
- 生成的 Hook 配置只向最后一个已准入的结构化替换步骤授予替换权限。native 和观察
  Hook 的 stdout 不能替换结果，所有回调在最终步骤之前保留相同的关联响应。
  Bridge 校验该权限必须为布尔值，且只能用于 after。
- 同一阶段的 Hook 传输复用调用内的绝对截止时间，在第一次传输尝试时按事件预算加原有
  3 秒传输余量固定。Service 接受事件前的失败也不会为后续步骤重置等待时间。
  阶段超时后，后续步骤直接按各自错误策略处理，不再启动回调；下一次调用和另一个阶段
  分别获得自己的截止时间。清理保留独立的关闭时限。子进程超时或取消时清理自有进程组。
  每个已进入的 before/
  after 阶段都登记首次归一化 event/session/invocation 快照的调用级清理；report 返回
  退出码 0 不能证明 Service 已接受或完成步骤。
  阶段收尾通过内部 `aw hook --close-event true` 回调关闭关联。
  `CloseHookEvent` 在已有 lease 锁内阻止延迟 open，取消活跃 Provider 并等待
  事件从 registry 移除，不创建事件或刷新预算。关联与 worker 在初始化前共享取消标记，
  初始化期间释放 lease 锁；即使关闭超时，取消屏障也会保留，直到被阻塞的初始化退出。
  并发 open 仍共享同一个 handle。清理 RPC 截止时间为两秒，子进程
  上限为三秒（包括子进程创建）；清理失败会报告且不替换原始中断。即使取消首次发生在关闭过程中或
  被重复投递，调用级收尾也等待自有清理任务完成、尝试所有待关闭阶段之后再传播取消；
  每个阶段仍保留有界关闭等待。取消不改写成策略拒绝。
  若外部取消导致没有 final response，不承诺 after 回调。
  report 模式失败时保留关联供后续步骤使用。最后一个 before 步骤或提前拒绝会在
  进入工具函数之前关闭 before 阶段；调用级收尾关闭 after 阶段及被取消打断的 before
  阶段，避免未声明步骤在长时间工具执行期间或直到截止时间仍占用容量。
  清理复用同一个有界关联关闭操作，失败仅报告，不替换原生结果。
  正常完成的事件也接收幂等关闭，每次工具调用最多增加两个关闭回调。保留 Service
  自动完成和超时清理，但不再依赖跨进程识别故障才能收尾；不重试失败步骤。
- 回调 JSON 转义 Unicode 标量值。SDK 将 JSON 转义 surrogate pair 解码成单个标量后仍可使用。
  Python 字符串中的字面 surrogate 码点（包括相邻高低代理项和字典键）在传输前以
  `native_input_invalid_unicode` 失败，避免 JSON 合并码点后改变策略输入。
  `on_error: report` 保留原生执行值，`block` 拒绝，不会中断 turn。
  JSON 编码递归失败也按 Hook 错误策略处理；清理遇到同类错误时仅报告，不替换原生结果。
- 回调 JSON 中的整数须在 `i64`/`u64` 范围内（`-2^63` 至 `2^64 - 1`）。
  更大的整数在传输前以 `native_integer_out_of_range` 进入已配置的 Hook 故障策略；
  `report` 保留精确的原生执行值。
- 诊断尽力写出，每次最多 4096 字节。管道/终端使用独立打开的非阻塞描述符，不修改
  应用继承描述符的标志。写满、关闭或不支持的输出端可以丢弃诊断；策略拒绝仍保留完整
  原因。故障诊断也走同一路径，不调用应用的 logging handler。
  普通文件输出沿用文件系统写入语义；传输截止时间不约束外部卡死的文件系统，
  Bridge 不创建后台日志任务。
- 每次调用在回调点读取 `agent.state.session_id`，生成所有 before/after wrapper
  共享的新 `invocation_id`，用作归一化 `tool.call_id`。native payload 保留原生
  `tool_call.id`，允许同一会话跨轮次复用。独立工具和 Agent 保留原生并发。
  会话/调用 ID 必须非空且不超过 512 UTF-8 字节。
- 策略输入和 PostTool 命令投影使用 `effective_input`，通过固定版本 SDK 的
  `_json_loads_with_repair` helper 解码，不传 schema，与 `Toolkit.call_tool` 执行时
  解析参数的行为一致。已有可信应用 middleware 仍先执行。Bridge 在 native 数据中
  保留原始 `tool_call.input` 字符串，不改写 SDK 调用；包括递归限制在内的解码失败
  按 Hook 的错误策略处理。前置可信 middleware 改写后的参数若无法解码，`block`
  拒绝；`report` 交给 SDK 生成原生工具错误，不中断整轮。升级该私有 codec 需重新验证 SDK 锁版；它不是新的 middleware 入口。

`on_acting` 之前的原生参数/权限失败、外部执行请求、直接 `Toolkit` 调用及业务
dispatch 旁路不经过这里，需要独立接点。框架终态成功与内容中编码的业务失败分开。

## 启动与部署边界

首版支持一个前台 Python worker，不支持 fork 型 Gunicorn supervisor、子 worker
自动接线或远端沙箱。常驻业务服务需要显式处理每个 worker/会话的 launch 与 binding
归属。本地服务每实例最多保留 1024 个事件关联（约 512 对 before/after），release
之前不会因为 session ID 改变而重置预算。不隐含远端命令取消、热加载或 OS 隔离能力。

已有 `native-hook/v1alpha1`、结构化 Provider 和默认布尔 Hook 均可使用。布尔 Hook
返回 `true` 时产生配置的 effects；before 的 `effects: [observe, block]` 匹配 true 即
阻断，`false` 为不匹配。布尔 after Hook 只接受 observe。结构化 Provider 可替换一个
最终文本槽，保留 response ID、state、metadata 和 content block 字段。Media、多个
block、未成功结果及中间 chunk 不压缩；无效替换报告故障并保留原结果。错误无法撤销
已经执行的工具。

[SDK 生命周期配置](../../crates/aw-service/examples/aw.agentscope-lifecycle.yaml) 复用
sec-core 的 `scan_code`/`observe_tool` 和 Tokenless 的 `pre_tool`/`post_tool`。
PreTool 拒绝原始尝试，把完整重试参数交给模型；新调用必须重新通过原生权限和
sec-core。RTK 归属来自新执行参数，与被拒绝调用的 ID 无关，可避免重复压缩。
sec-core 映射必须匹配 Toolkit 的准确工具名和参数指针。RTK 需独立安装；
`aw-package configure` 仍面向 Qoder/OpenClaw。

## 验证

标准 `aw-service` 测试覆盖 preflight 拒绝、事件归一化，以及只声称版本但没有
工厂/readiness 的进程不会被采纳；无需安装 SDK。

独立真实 SDK 验收使用确定性离线 model，经过公共 `Agent.reply`、原生 ReAct 调度、
Toolkit、完整 service 与 `aw run` 路径。覆盖 streaming、before 等待、拒绝无副作用、
普通失败、工具中断、并发会话、同一会话复用 tool ID（相同与不同参数）、55 秒事件
预算内每会话连续 34 次提前拒绝后仍能成功执行、两个实例的隔离、实例清理与校验后的持久 journal 链：

Native Hook 额外覆盖每会话连续 34 次交替退出码 1 与 SIGTERM：report step 继续进入
block step，由后者关闭 before 事件并拒绝工具。最后的允许调用验证事件容量仍可用。

```bash
# In a Python environment with agentscope==2.0.6 installed, from src/aw:
cargo build --locked -p aw-service --bin aw
python -B crates/aw-service/tests/agentscope_native.py \
  --aw target/debug/aw --output target/agentscope-native
python -B crates/aw-service/tests/agentscope_cancellation.py \
  --aw target/debug/aw --output target/agentscope-cancellation
python -B crates/aw-service/tests/agentscope_arguments.py \
  --aw target/debug/aw --output target/agentscope-arguments
```

取消验收在同一活跃 binding 内，经公共 SDK turn 分别取消 34 次非末尾 before 回调
和 34 次非末尾 after 回调，再各取消 34 次中间回调仍等待 spawn、前一步已打开事件的
before/after turn，共 136 次。每次取消均先释放事件和已启动的 native Hook 子进程，随后正常
turn 仍执行全部策略步骤。另有 36 轮在 before/after 的首、中、末回调注入启动前
`EMFILE`：report 故障保留后续回调，block 故障跳过工具，且每轮均立即释放事件。
service 测试另覆盖先取消后 open、重复清理及 instance、
session 和 event 隔离。

另有 36 轮让真实 Hook 子进程经过私有 Unix socket proxy：`open_hook_event` 成功，
但 `invoke_step` 在到达 Service 前断开。report 退出码 0 保留后续步骤，block 跳过工具，
阶段收尾立即释放部分完成的事件，before 故障还断言工具入口的活跃事件数为零。
proxy 及其自有连接有界，退出时删除。

另有 48 轮真实 native Hook 覆盖八种 UTF 编码的 `ask`/`approve` action 与
`decision: ask`，分别验证两种故障策略：report 继续后续步骤，block 返回 `DENIED`
且不执行工具；每轮均释放事件。

另有三轮真实 native Hook 在 before-report、before-block 和 after-report 步骤输出
10,000 层嵌套数组（20,001 字节 JSON，位于 fixture 的 32 KiB 输出限制内），验证
解码故障诊断、后续步骤继续、拒绝无执行副作用、原生结果保留和事件释放。

参数验收在 `report` 和 `block` 两种策略下，对比 before/after 策略输入、SDK 权限
检查参数和工具实际执行参数。覆盖缺少闭合括号、单引号、JSON 代码块及嵌套 Unicode
数据，并验证 AW 可以拒绝修复后的调用且不执行工具。前置可信 middleware 还会在
原生校验之后注入 10,000 层参数：report 保留 SDK 工具错误，block 拒绝且无执行
副作用，两种策略下的后续正常轮次均成功。

输出和临时 state 均保存在指定 target 目录，自有临时目录在验收完成后删除。若 Unix
socket 路径超过 Linux 的 107 字节限制，测试明确报错，需使用短 worktree/target 路径。无需云账号、凭据或模型
请求；不等于业务容器或修改后的 SDK 发布物部署验收。


新增生命周期验收使用同一真实 SDK、公共 turn/Toolkit 路径、真实 AW Provider 和
Protocol v2 Tokenless CLI。scanner 使用 fixture；Toolkit 记录执行，不运行 Shell
命令。覆盖完整重试参数、新调用的权限和安全检查、RTK 归属、原始结果观察后的模型
上下文压缩、custom/native Hook 故障按 report 继续，以及错误、中断和 media 结果
保留。middleware 单测另验证 streaming 对象身份、响应封装保留和无效替换回退。

```bash
cargo build --locked --workspace
python -B crates/aw-service/tests/fixtures/agentscope/middleware.py
python -B crates/aw-service/tests/agentscope_lifecycle.py \
  --aw target/debug/aw --tokenless /absolute/path/to/tokenless \
  --output target/agentscope-lifecycle
```

# AW 原生 Agent 接入参考

[English](../../en/aw/native-agents.md)

本页用于组件接入与维护。安装和日常使用请阅读[用户指南](../../../user-guide/zh/aw/index.md)。

AW 将工具策略和 Hook 命令接入 Agent，同时保留它原有的交互界面。在 `aw.yaml`
中声明程序和事件后，AW 启动或复用本地服务，接通受支持的原生 Hook，并在 Agent
会话结束后继续保存执行记录。

当前 Linux 源码版本支持 Qoder CLI >=1.1.64、OpenClaw 2026.9.6、
QwenPaw 2.2.2b4 / AgentScope 2.0.9、Hermes 官方提交 `952c941e`，以及显式接入的
Claude SDK 0.2.88 worker（bundled CLI 2.1.161），以及 Pi Coding Agent/Core SDK 0.84.2 worker，
以及在前台 Python worker 中通过显式公共工厂接线的 AgentScope SDK 2.0.6。
其他首批 Adapter 独立交付。AW 不安装 Agent，也不配置模型账号；继续使用框架
原有的模型与认证配置。

安装请从 [Preview 包指南](../../../user-guide/zh/aw/installation.md)开始；源码构建与无需模型的策略验证见
[组件接入教程](integration.md)。随后使用下方共用配置验证
原生工具行为。字段定义集中在[配置参考](configuration.md)。


可选生命周期观测参见[通过 aw run 接入 AgentSight](../../../user-guide/zh/aw/observability.md)。

## 安装 Preview

使用 [Preview 包](../../../user-guide/zh/aw/installation.md) 安装并演示 AW + sec-core，无需编译。
下方源码构建说明面向开发者；Preview 尚非稳定发行。

## 当前支持范围

| 能力 | 状态 |
| --- | --- |
| 校验一份包含全部 16 个事件名的配置 | ✅ 识别事件不代表安装 Hook |
| 通过 AW 启动 Qoder CLI | ✅ 交互和 print 入口 |
| 工具前运行结构化 Provider | ✅ `observe`、`block` |
| 工具后运行结构化 Provider | ✅ `observe`；成功、失败及阻断尝试的覆盖随框架而异 |
| 工具前后执行原生脚本和命令 | ✅ 回调输入保持不变，转交字节输出与退出状态 |
| 保留 Qoder 已有 Hook 及其调度 | ✅ 默认配置和显式传入的附加配置文件 |
| 复用共享服务并持久保存执行元数据 | ✅ 按需启动或外部启动服务 |
| 通过 AW 启动 Hermes | ✅ 显式安装原生插件后的本地 chat |
| 通过 AW 启动 OpenClaw | ✅ 新 Gateway 中的 Agent 工具 Hook |
| 通过 AW 启动 QwenPaw | ✅ 官方 App/API 入口 |
| 启动 Pi SDK 0.84.2 worker | ✅ 公共 SDK 工厂；见[worker 指南](#启动-pi-sdk-worker) |
| 启动 Claude SDK 0.2.88 worker | ✅ 显式公共工厂 Bridge；见 [worker 指南](#启动-claude-sdk-worker) |
| 通过 AW 启动 AgentScope SDK | ✅ 单个前台 Python worker 中[显式接线的公共工厂](#启动-agentscope-sdk) |
| 启动其他首批框架 | ❌ 相应 Adapter 独立交付；QwenPaw 与 Qwen Code 分别识别 |
| 其他事件、跨框架 `ask`、输入改写或 OS 执行约束 | ❌ 当前结构化 Provider 路径不予准入 |
| 安装 Preview 包并生成 Qoder/sec-core 配置 | ✅ [Preview 指南](../../../user-guide/zh/aw/installation.md)，尚非稳定发行 |

对于 Qoder，`tool.after` 映射到成功调用后的 `PostToolUse`。Qoder 的 `PostToolUseFailure`
是独立事件，本 Adapter 尚未接入。原生 Hook 命令仍按 Qoder 的响应语义处理。
透传原生审批响应不代表 AW 已提供跨框架审批支持；交互式审批不在本期验收范围内。

## 四框架使用同一份配置

[共用 YAML](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/aw.shared.yaml)与
[布尔脚本](https://github.com/agentic-os-org/ANOLISA/blob/1c164c90d7849251f53768fb1d4a04fb97445c99/src/aw/crates/aw-service/examples/check-tool.py)让四个目标共用工具前
阻断、工具后观察策略。按[教程](integration.md)复制文件、
替换绝对路径并验证 Provider。这里沿用其 `AW_BIN`、`AW_CONFIG` 和 `AW_DEMO`
变量。包安装用户也可使用这两个文件，将 `AW_BIN` 指向已安装的二进制；Python 3
是该示例脚本的依赖。

先准备各 Agent 的受支持版本与原生模型配置。运行时只需安装所选目标。各框架使用
独立原生 profile，显式选择工具工作目录，并从 `$AW_DEMO/workspace` 启动。
共用 YAML 不修改模型凭据或原生权限，起步时逐个测试目标。

| 目标 | 原生前置条件 | 启动差异 |
| --- | --- | --- |
| Qoder | 支持原生命令 Hook 的 CLI >=1.1.64，已登录 | 交互或 print 入口；可选 `--native-settings` |
| OpenClaw | 2026.9.6，已配置的 JSON 和现有状态目录；先停止使用该 profile 的 Gateway | AW 新启 `gateway run`，须传入两个原生路径 |
| QwenPaw | 2.2.2b4 / AgentScope 2.0.9，已初始化 App 工作目录与模型 | 设置 `QWENPAW_WORKING_DIR`，使用 App/API，不使用 ACP/TUI |
| Hermes | 干净的官方提交 `952c941e`、已安装的 Python console script 和已初始化的本地 chat profile | 在该 profile 中安装一次 AW 插件，运行时使用同一 profile |

启动 Qoder：

```bash
cd "$AW_DEMO/workspace"
"$AW_BIN" run --config "$AW_CONFIG" --agent qoder
```

使用已有原生绝对路径启动 OpenClaw：

```bash
"$AW_BIN" run --config "$AW_CONFIG" --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

使用已初始化目录启动 QwenPaw：

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home "$AW_BIN" run \
  --config "$AW_CONFIG" --agent qwenpaw -- --host 127.0.0.1 --port 8096
```

阅读 Hermes 的 [profile 要求](#启动-hermes)后，安装并启动：

```bash
"$AW_BIN" install --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
"$AW_BIN" run --config "$AW_CONFIG" --agent hermes \
  --native-profile /absolute/hermes-profile
```

OpenClaw 和 QwenPaw 的服务终端保持运行，等 AW 输出原生 Hook 就绪信息后，再向
对应 Gateway 或 App 发起请求。OpenClaw 使用连接该 Gateway 的客户端，不使用
`agent --local`；QwenPaw 使用 App UI/API，不改用其他 CLI 入口。Qoder 和 Hermes
使用已启动的原生 chat。下方各框架章节说明共存方式与支持的生命周期范围。

### 验证原生行为

在另一个终端恢复教程中的 `AW_BIN`、`AW_CONFIG` 和 `AW_DEMO`，将 `AW_TARGET`
设为正在测试的目标。开始时标记文件应不存在；重复测试时换一个新名称。打印下述
两个提示词，分两轮发送到该目标的原生界面：

```bash
export AW_TARGET=qoder
printf 'Use your shell tool exactly once to run `printf AW_OK > "%s/%s.allowed"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
printf 'Use your shell tool exactly once to run `printf AW_DENY_TEST > "%s/%s.blocked"`. Report the result; do not retry or use another tool.\n' "$AW_DEMO/workspace" "$AW_TARGET"
```

这些命令即使执行也只写入演示标记。第二条输入包含策略的拒绝标记。完成两轮后检查：

```bash
cat "$AW_DEMO/workspace/$AW_TARGET.allowed"
test ! -e "$AW_DEMO/workspace/$AW_TARGET.blocked"
"$AW_BIN" status --config "$AW_CONFIG"
ls "$AW_DEMO/state/journal"
```

预期读到 `AW_OK`，第二轮收到 AW 策略拒绝，且 blocked 标记不存在。通过 AW
读取对应 journal：从列出的目录选择文件，将去掉 `.jsonl` 的 64 字符文件名作为
`AW_AUDIT_KEY`。

```bash
export AW_AUDIT_KEY=REPLACE_WITH_EVENT_KEY
printf '{"method":"audit","key":"%s"}\n' "$AW_AUDIT_KEY" | \
  "$AW_BIN" request --socket "$AW_DEMO/state/aw.sock"
```

目录同时包含准备与事件 journal。事件的 `result.records[0].record.plan` 中，
`kind` 为 `event`，instance 和 binding 应与启动时输出的 `AW instance` 对应。before
拒绝调用应有 `policy_block: true`；完成的 after 步骤应指向 Provider
`check-tool` 且 `status: ok`。审计保存元数据，不保存原始输入/结果或效果的原因
文本。已闭合事件有 `terminal: true`。以下检查共同组成验收记录：

| 检查项 | 证据 |
| --- | --- |
| 允许工具 | 工具已执行、allowed 标记存在、before 及受支持的 after 步骤成功 |
| 策略阻断 | AW 返回拒绝、blocked 标记不存在、before 审计记录策略命中 |
| Provider 失败 | 停止旧会话/服务后，使用 before 脚本路径不存在的配置副本启动；调用失败，`on_error: block` 阻止标记生成，审计记录故障而非策略命中 |
| 共存 | 保留一个已有原生 Hook/插件，确认其预期行为与 AW 一起生效 |
| 重启 | 退出 Agent、停止 AW、使用原配置重新启动；新工具仍受检查，旧审计键仍可查询 |

记录框架版本、AW 提交、实际命令与审计键。合成 Host 测试与启动就绪不能替代该表。
模型拒绝、未调用工具或原生权限拒绝不算 AW 阻断。各框架的 after 覆盖不同，例如
Qoder 仅报告成功工具，不应要求为每次被拒绝的调用补造 after 事件。

### 停止与清理

退出 Qoder/Hermes，在自己启动的 OpenClaw/QwenPaw 服务终端按 Ctrl-C。等待这些
进程退出后，再停止共享 AW 服务：

```bash
"$AW_BIN" stop --config "$AW_CONFIG"
```

停止响应只确认收到请求；等待记录的 daemon PID 退出且 `aw.sock` 消失。
QwenPaw 正常退出时删除自己的临时插件。Hermes 保留已安装插件；没有 AW 绑定的
普通 Hermes 启动不会启用 AW 回调。撤销安装时，停止 Hermes 与原生配置写入，
核对安装时报告的 `backup` 和 `displaced` 文件，将 `plugins/aw-native-hooks`
移到 profile 的 `plugins` 目录之外，再恢复选定的安装前配置。保留安装后有意
进行的原生修改，不要盲目整文件覆盖。AW 没有原生插件卸载命令。

保留审计证据后，按[教程的清理步骤](integration.md#5-验证原生效果并清理)
删除演示目录。框架 profile 单独保留。包卸载见
[Preview 指南](packages.md#6-停止升级与卸载)。

## 构建并启动 Qoder

AW 尚未通过 `anolisa install` 或 RPM 发布。开发者可以在 Linux 上用 rustup 和
仓库固定的工具链构建。另行安装 Qoder CLI 并核对版本。从仓库根目录执行：

```bash
cd src/aw
cargo build --locked -p aw-cli --bin aw
qodercli --version
target/debug/aw validate --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder
```

[Qoder 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qoder.yaml)使用
`argv: [qodercli]`。若需选择特定安装，将该项改为对应可执行文件的绝对路径。
`--agent qoder` 选择 `spec.agents` 下的命名对象；名称可以自定，
`adapter: qoder` 才是框架类型。

AW 要求 CLI 的语义版本 >=1.1.64，以此前测试基线为下限，不再固定一个版本。
`--version` 输出去掉首尾空白后，必须是 `1.1.66` 这样的裸 SemVer，长度为 1–128
个 UTF-8 字节。不接受前导 `v`、产品名、尾部说明或额外行。格式错误会明确说明
这个要求，并显示实际输出。AW 记录去掉首尾空白后的原文，不从中搜索版本号。
旧版本或非法版本文本在绑定前拒绝；1.1.64 的预发布版低于下限，构建信息仍保留。
使用相同原生 Hook 合同的 CLI 无需在 YAML 填版本，也无需降级。该合同包括
`--settings`、通过字面 `args` 执行的命令 Hook、`PreToolUse`/`PostToolUse`、
回调 ID/输入，以及退出码 2 阻断。
版本文本本身不能证明兼容；关闭 Hook 的配置、不支持的启动模式及非法回调仍会
失败。旧 AW 构建若要求 1.1.64，需更新 AW 才能使用这个行为。

例如，将目标命名为 `qodercli`，并选择已安装的可执行文件：

```yaml
agents:
  qodercli:
    adapter: qoder
    argv: [/absolute/path/to/qodercli]
```

将该段合入完整配置的 `spec` 下，再运行
`aw run --config /absolute/aw.yaml --agent qodercli`。目标名与原生可执行文件名、
版本号分别定义。

示例在工具前后消费 Hook 输入并返回 `{}`，不增加限制，也不是安全策略。可以让
Qoder 执行一个无副作用的只读工具，触发前后两个回调；没有工具调用的回答不会
触发它们。`--` 后的参数传给 Qoder，例如：

```bash
target/debug/aw run --config crates/aw-service/examples/aw.qoder.yaml --agent qoder -- -p 'Read the current directory name with a tool.'
```

AW 打开 Qoder 的原生终端界面，Qoder 退出后回到原 shell，并释放本次会话的绑定
与未完成工作。共享 daemon 继续运行，供使用同一配置的后续会话复用。

```bash
target/debug/aw status --config crates/aw-service/examples/aw.qoder.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.qoder.yaml
```

## 启动 AgentScope SDK

使用安装了 `agentscope==2.0.6` 的 Python 环境；这与 QwenPaw 已验证的
AgentScope 2.0.9 运行时分开。`argv` 指定 SDK 环境的 Python 和 `.py` 应用或
`-m MODULE`。AW 校验该 SDK 版本，提供 `aw_agentscope` 与受保护的运行配置，
等待真实公共工厂安装 middleware 后生成的回执。

在 `src/aw` 中，将该环境的 `python3` 放到 PATH 后执行：

```bash
target/debug/aw run --config crates/aw-service/examples/aw.agentscope.yaml --agent agentscope
target/debug/aw status --config crates/aw-service/examples/aw.agentscope.yaml
target/debug/aw stop --config crates/aw-service/examples/aw.agentscope.yaml
```

示例使用确定性的离线 model 与本地 Echo 工具，无需凭据。before 布尔 Hook 返回
false（没有 block 命中），after 返回 true（observe）。把 before 命令改为
`printf true` 即可拒绝工具；原生 final response 为 denied，after 观察该拒绝。
修改配置之前先停止服务。

已有应用在**每个**公共工厂中使用 `AwBridge().create_agent(...)`，原样传入已有
middleware 列表。可信参数注入应先于追加的 AW middleware。readiness 回执只
覆盖所构造的 Agent，不覆盖其他工厂或直接 dispatch 路由。
[工厂合同](../../../../src/aw/docs/design/zh/adapters/agentscope-adapter.md)说明顺序、预算、
调用身份与覆盖边界。

before 等待并可在工具副作用前阻断；after 等待一次 final `ToolResponse`，保留
success/error/interrupted，不重复中间 stream chunk。`on_acting` 之前的原生
参数/权限拒绝、直接 Toolkit 调用和外部执行请求需要另接。launcher 支持单个
前台 worker；fork 型 Gunicorn supervisor、多 worker 和远端部署不在此合同内。
常驻进程必须显式管理 service 每实例 1024 个事件关联的容量限制。

接入 PreTool 重试建议和最终 PostTool 压缩时，复制
[SDK 生命周期示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml)，
替换所有路径，并在 sec-core 映射 Toolkit 的准确工具名和参数指针。示例先做安全检查，
再执行 Tokenless PreTool；工具完成后先观察原始结果，再压缩一个成功的文本槽。
custom/native after Hook 放在最终压缩步骤之前；启用替换后 AW 按声明顺序执行此链。
native 和观察 Hook 不替换响应；只有最后一个已准入的结构化替换步骤可以修改模型
可见文本。Hook 故障按 report 处理后，后续步骤仍收到原始输入。
只有观察的链保持原生逆序退出。

PreTool 拒绝原始调用，向模型返回完整重试参数；每次新尝试重新经过原生权限和
sec-core。带 RTK 归属的重试即使使用新 call ID 也不会重复压缩。PreTool 需独立安装
RTK，PostTool 不依赖 RTK。文本槽适用条件、故障处理和遥测配置见
[Tokenless 行为](../../../user-guide/zh/aw/tokenless.md)。无效的最终响应替换指令会报告故障并保留原结果。

## 启动 QwenPaw

安装 QwenPaw 2.2.2b4 与 AgentScope 2.0.9，通过 QwenPaw 初始化模型配置。示例
使用 `argv: [qwenpaw, app]`。将 `QWENPAW_WORKING_DIR` 指向已初始化的绝对目录。
AW 只添加自身临时插件，保留已有插件和原生配置，App 退出后删除自身插件。

原生 middleware 保持嵌套顺序：before 正序、after 逆序展开，工具并发由宿主决定。
after 观察终结的 `ToolResponse`，包括 AW 拒绝结果，不对流式 chunk 重复调用。
当前点位覆盖 App 内实际执行的工具；外部执行工具以及进入 middleware 前已被拒绝
的尝试不在覆盖范围。事件预算为 1..55,000 ms。原生命令使用 AW 插件明确约定：
before 退出 2 表示拒绝，退出 0 表示继续，显式 ask/approve 尚不支持。
回调故障记录错误并按步骤的 `on_error` 处理：`report` 保留工具执行和 after 结果，
工具前的 `block` 返回拒绝；宿主取消继续传播。

原生 startup hook 在插件加载完成后确认注册；它不承诺 App 对外端口开放之前已经
防护。ACP/TUI、reload 和多 worker 启动仍被拒绝，这些入口的插件生命周期尚未验收。
看到原生 Hook 就绪提示后，通过 App 原有 API 或界面使用。

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home target/debug/aw run \
  --config crates/aw-service/examples/aw.qwenpaw.yaml --agent qwenpaw \
  -- --host 127.0.0.1 --port 8096
```

[QwenPaw 中性示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qwenpaw.yaml)
在工具前后执行命令，不安装安全策略。受支持的适配器可复用同一份
`aw-provider/v1alpha1` 策略配置；原生 Hook 的输出格式与事件覆盖仍按框架分别说明。

零 Provider、零事件步骤的配置也可以启动 App；AW 确认插件就绪，不注册工具 middleware。

## 启动 OpenClaw

另行安装 OpenClaw 2026.9.6。示例使用 `argv: [openclaw, gateway, run]`，必要时
改为可执行文件的绝对路径。显式传入已有原生 JSON 配置和已存在的绝对 state 目录。
AW 生成私有配置 overlay，保留该目录中的认证和会话；发现 Gateway 所有权冲突时
拒绝启动，不接管正在运行的实例。
AW 对受管 Gateway 禁用 Node 编译缓存，避免启动器重启子进程改变就绪 PID；
自定义包装器必须 exec Gateway。

每个 AW 步骤注册为一个原生插件 handler。OpenClaw 按原生优先级串行执行 before，
并发执行 after；其他插件的优先级保留。事件预算范围为 1..12,000 ms。被阻断的尝试
也可能带错误产生 after，因此 after 不代表执行成功。当前覆盖新 Gateway 内的 Agent
工具执行，operator `tools.invoke` 不属于完整前后接线入口。原生 Gateway startup
回调完成后才报告 Hook 已就绪，该状态与 AW 服务可连接分开判断。

```bash
target/debug/aw run --config crates/aw-service/examples/aw.openclaw.yaml --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

[OpenClaw 中性示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.openclaw.yaml)
在工具前后执行命令，不安装安全策略。受支持的适配器可复用同一份
`aw-provider/v1alpha1` 策略配置；原生 Hook 的输出格式与事件覆盖仍按框架分别说明。

## 升级服务

本版本使用本地协议 `aw-service/v1alpha4`。旧版 daemon 会在 Agent 绑定和回调前
被拒绝，返回 `protocol_version`。替换可执行文件前，先退出其 Agent 会话，再用
**旧版** `aw stop --config FILE`（或 `--socket ABSOLUTE_PATH`）停止每个旧服务；
随后一起更新 CLI 和 daemon，再次启动。不要删除仍在使用的 socket 或审计历史。
AW 配置和 Provider 协议仍分别为 `aw/v1alpha1`、`aw-provider/v1alpha1`。

## 布尔判断脚本

简单规则只需从 stdin 读取一个标准化事件 JSON，在 stdout 输出 JSON 布尔值
`true` 或 `false`，然后以退出码 0 结束。`true` 表示命中规则，AW 请求配置中的
effect；`false` 不请求 effect，也不覆盖原生权限。诊断可写 stderr，不进入 Provider
回复或审计。自定义 Hook 默认只在事件触发时调用：省略 Provider 的 `protocol`
和步骤的 `operation` 即可。AW 在本地校验配置和效果准入，不调用脚本的
`describe` 或 `validate_config`，只对已准入事件执行脚本。脚本不接收 method、
request ID 或摘要，也不用构造 response 对象。显式声明
`protocol: aw-provider/v1alpha1` 仍走完整结构化 Provider 握手。

例如，将以下规则保存为 `$AW_DEMO/check.py`。Python 是这个自选脚本的依赖，
不是 AW core 的依赖：

```python
import json
import sys

def contains(value):
    if isinstance(value, str):
        return "12345" in value
    if isinstance(value, list):
        return any(contains(item) for item in value)
    if isinstance(value, dict):
        return any(contains(item) for item in value.values())
    return False

event = json.load(sys.stdin)
print(json.dumps(contains(event["tool"]["input"])))
```

沿用[安装指南](../../../user-guide/zh/aw/installation.md)中的前缀与路径，由工具生成完整 YAML，无需手工拼装
Provider 和步骤。只提供所需 Agent 入口；本例声明两个框架：

```bash
"$AW_PREVIEW_PREFIX/bin/aw" config init --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw-command.yaml" --state-dir "$AW_DEMO/command-state" \
  --qoder "$AW_QODER" --node "$AW_NODE" --openclaw "$AW_OPENCLAW" \
  --provider command --check /usr/bin/python3 --effect block \
  --reason-code parameter_contains_12345 -- "$AW_DEMO/check.py"
"$AW_PREVIEW_PREFIX/bin/aw" validate --config "$AW_DEMO/aw-command.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw-command.yaml" --agent qoder
```

无需 sec-core 包或 daemon。同一份生成配置可配合 `--agent openclaw` 及其已有原生
profile 参数使用。两个框架向脚本提供相同的标准化工具字段，由 AW 将 block 翻译为
各自的原生 Hook 回复。生成的 transport 直接执行用户命令。
在 `spec` 内可这样声明自定义 Hook：

```yaml
providers:
  command:
    transport:
      type: stdio
      location: agent
      argv: [/usr/bin/python3, /absolute/path/check.py]
    timeout_ms: 1000
    max_output_bytes: 32
events:
  tool.before:
    enabled: true
    required: true
    steps:
      - id: check-command
        provider: command
        effects: [block]
        reason_code: parameter_contains_12345
        on_error: block
```

省略 `protocol` 等同 `aw-hook/v1alpha1`，省略 `operation` 等同 `invoke`，也可
显式声明这两个值。Hook 的 `config` 省略或填空对象，脚本选项通过字面 argv 传入。
`true` 请求该步骤 `effects` 列表中的全部效果，可携带步骤的 `reason_code`；
`false` 不请求效果。效果仍须满足 Adapter 能力。`block` 只用于工具前，
`observe` 也可用于工具后。完整 Provider 步骤的 `effects` 仍是准入上限，由
Provider 选择实际返回的效果。

生成器创建一个必需的工具前步骤，设置 `on_error: block` 和 1,000 ms 命令超时。
非零退出、超时、输出超限、stdin 未完整交付或非布尔响应均为执行错误，不当作
false；按步骤的失败策略处理。布尔 stdout 含空白上限为 `max_output_bytes` 和
32 字节中的较小值，服务将 stderr 限为 65,536 字节。AW 不插入隐式 shell。
准备阶段在本地校验配置和 Adapter 能力，不执行用户脚本。
`aw validate` 离线检查期望配置文档。

已有 `aw policy` 桥接配置仍受支持，保留私有 `config.on_true` 的效果映射和完整
Provider 协议，无需迁移。切换自定义规则时，将 transport argv 指向布尔脚本，
省略 `protocol`、`config` 和 `operation`，将命中后的 effects 和可选 reason code
放到步骤中。切换配置前退出旧 Agent 会话并停止旧服务。旧 AW 二进制不接受新的
缺省形式，回滚时保留原配置。

`aw config init` 只新建私有文件，不覆盖已有文件。移除策略时，不传 `--provider`
（或传 `--provider none`）生成新的基础文档，退出旧 Agent、停止其服务后显式切换
配置。Provider 与关联步骤一起移除；修改运行服务的文件不会触发重新加载。
通用的已有 YAML 编辑不属于此命令。

## 启动 Hermes

使用官方 Hermes 提交 `952c941e741e922a9be8fc403c8944c6e96318bb`。安装与启动
均拒绝已跟踪文件的已暂存或未暂存修改；重试前提交或 stash 本地修改，并恢复到
固定提交。
父环境与 profile dotenv 均须取消 `GIT_DIR`、`GIT_COMMON_DIR`，让 Git 正常发现
当前 checkout。跳过的优先 PATH 名称也须防止他人替换或创建，包含 sticky 可写
父目录。两个环境中的非空 GIT_CONFIG_SYSTEM、GIT_CONFIG_GLOBAL、
XDG_CONFIG_HOME 及已设置的 HOME 均须为绝对路径；取消不支持的相对值或改用
绝对路径。工作目录、profile、venv、editable checkout、AW 执行文件和启动状态路径须为 UTF-8。
Git 在工作目录与 checkout 中须解析为同一规范可执行文件。两个目录分别核验
各自的有效配置，包括 local 配置和条件 include；拒绝外部 filter 命令与 fsmonitor helper。示例为
`argv: [hermes, chat]`，请指向该安装的 Python console script，例如
`/absolute/hermes/venv/bin/hermes`，且使用绝对 Python shebang。显式路径和 PATH
查找均要求可执行普通文件；FIFO、目录及不可执行文件在原生探测前被拒绝。不支持 shell wrapper
或 `python -m` 入口。AW 使用该解释器进入同一个原生 CLI 进程，保留参数与工作
目录。原生 profile 加载决定生效的 `HERMES_SAFE_MODE`，包括 profile `.env` 的
覆盖；AW 拒绝 safe mode，并在 chat 调度工具前要求插件完成注册。先通过 Hermes 初始化 profile
与模型账号。`aw agent connect` 是显式的一次性原生 AW 插件安装：启用前备份原始配置，
保留未知配置字段，拒绝覆盖同名非 AW 插件。原生 YAML 写入器修改私有候选文件，
AW 再与真实配置原子交换。命令报告初始字节备份 `backup` 与永久保留的
`displaced` 文件（`<backup>.displaced`）；后者保留原 inode，包括已打开描述符
后续写入的内容。安装与回退期间停止原生配置写入，恢复前核对两份恢复文件。
写入器失败或发布前检测到冲突时，真实配置保持不变；在交换边界检测到冲突时，
命令报错，候选文件已经发布，被置换内容保留以供核对。
成功还要求删除配置暂存目录并同步其父目录。清理或同步失败会报告配置已发布
及两份恢复文件的路径，不返回成功或自动回退。该交换不向忽略 AW 锁的
写入器提供 compare-and-swap 保证。安装不会热加载或重启既有 Hermes 服务。
规范化后的 profile 目录、配置及随附插件文件必须属于当前用户，且其他用户不可写。
各级上级目录
须属于当前用户或 root，并禁止组或其他用户写入；像 `/tmp` 这样以 sticky bit
保护子目录的情况除外。已有 profile `.env` 同样必须是当前用户所有、组或其他用户不可写的普通文件
（接受 0600 或 0644）。AW 在任何原生探测前检查它且不跟随符号链接，包括
`--version`，因为导入 CLI 就会加载该文件。AW 拒绝不安全路径，不修改其权限。Ctrl-C 或 SIGTERM 会
取消安装的原生探测及写入进程、删除暂存副本，并保留信号退出状态。
安装与启动探测的错误会在信号退出前输出，包括无法确认进程组清理的错误。
安装在探测或修改 profile 前按启动时相同的本地 chat 限制校验已配置的原生参数。
仍支持只包含执行文件的 `argv`；此时在 `aw run` 的 `--` 后提供 `chat` 及其选项。
chat 的 `--help`/`-h` 会在插件就绪前退出，因此 AW 会在原生探测或修改 profile 前
拒绝配置参数及 `aw run` 追加参数中的这两个选项。查看原生帮助请直接运行
`hermes chat --help`。`--query=--help` 这类字面量查询值仍受支持。

AW 升级可能改变随附插件的字节内容。启动或安装报告插件不匹配或缺少文件时，先停止
Hermes 会话与插件写入者，检查 `<profile>/plugins/aw-native-hooks`，并将该目录
移到 `<profile>/plugins` 之外备份。重新执行相同的 `aw agent connect` 命令安装匹配文件；
保留的目录可用于检查或回退。

当 `providers` 和 `events` 为空时，AW 注册零个回调并正常启动。已有原生 Hook
仍会执行；AW 记录会话生命周期，不表示执行了每工具策略检查或工具审计。
仍须先显式安装插件。

后续 `aw run` 保留该 profile 的认证、历史与工作目录。原生 before/after 按 Hermes
注册顺序执行，before 阻断不会跳过后续已注册的回调；post 也可观察被阻断和失败的
尝试。AW 保留原生 Hook 返回解析和 Agent 自身审批，尚不支持通用 AW `ask`。
配置了 AW before 回调时，原生 Hook 和 tool-execution middleware 的参数改写
会被拒绝，包含 `execute_code` 内部调用：Hermes 可能在这些回调完成后才应用
改写，修改后的参数未经检查。仅观察的执行 middleware 仍可使用；空策略和
仅 after 路由保留原生改写行为。配置 AW 回调时，before 检查或工具执行期间的强制插件重载
会在卸载前被拒绝，以保留待执行的审计回调；工具之间仍使用原生重载。
当前入口为以 `--cli` 固定的本地 `chat`，不含原生 TUI、Gateway 或 ACP。
请使用完整 chat 参数；不支持 profile/worktree/恢复会话切换与选项缩写。事件预算最多 55,000 ms，
同时必须留在原生回调超时范围内。

```bash
target/debug/aw agent connect --config crates/aw-service/examples/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
target/debug/aw run --config crates/aw-service/examples/aw.hermes.yaml --agent hermes \
  --native-profile /absolute/hermes-profile
```

[Hermes 中性示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.hermes.yaml)
在工具前后执行命令，不安装安全策略。受支持的适配器可复用同一份
`aw-provider/v1alpha1` 策略配置；原生 Hook 的输出格式与事件覆盖仍按框架分别说明。

## 接入自己的程序

`spec.providers` 中的每个命名对象描述一个程序，事件步骤通过 `provider` 引用
它。根据现有程序选择协议：

| 协议 | 输入与结果 | 步骤字段 |
| --- | --- | --- |
| `aw-hook/v1alpha1`（缺省） | 命令读取一个标准化事件并返回 JSON true/false，AW 在命中时投影配置的效果，不执行脚本握手 | `effects`、`on_error`；可选 `reason_code`；省略 `operation` 或填 `invoke` |
| `aw-provider/v1alpha1` | AW 执行 `describe`、`validate_config`、`invoke`，响应包含经校验的候选效果 | `operation`、`effects`、`on_error` |
| `native-hook/v1alpha1` | 命令收到所选框架的原始回调 stdin，stdout、stderr 和退出状态按该 Adapter 合同处理 | `native: {}`、`on_error`；不填 `operation`、`effects` |

接入原生 Hook 时，将示例的 `transport.argv` 换成脚本的可执行文件及字面量参数。
AW 不插入 shell；使用 shell 语法时明确指定 `/bin/sh -c ...`。这种协议保留
`config: {}`，不执行 Provider 配置交互。Qoder 专用的原生输出不会自动转换成其他
框架的响应。

原生命令使用回调进程的实际环境，包括框架加载的 profile 变量；结构化 Provider
使用绑定时固定的启动环境。环境内容不写入事件或审计。

统一策略使用 `aw-provider/v1alpha1`，Provider 自己的设置写在 `config` 中。
可运行的[策略示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.yaml)展示工具前
`observe`/`block` 和工具后 `observe`。先执行
`cargo build --locked -p aw-provider --example policy`，再从 `src/aw` 运行，
因为示例命令使用相对路径。其中的阻断工具名仅供演示；测试阻断时，替换成 Agent
实际使用的工具名。样例 Provider 不是 sec-core。

`timeout_ms` 限制单次命令，`default_event_budget_ms` 限制整个事件，输出上限
约束返回字节数。Qoder 启动器接受 1 到 55,000 毫秒的事件预算。`on_error` 处理
执行故障，与 Provider 成功返回的策略阻断分开。
对原生命令，正常获得的非零退出仍是原生结果，不算 AW 传输故障。Qoder 在支持
阻断的点位将退出码 2 解释为阻断，其他非零退出是非阻断错误。observe 结果不授予
新权限，也不覆盖 Qoder 自身的工具权限。

## 保留已有 Hook 与调度

Qoder 继续加载原有 user、project 和 local 配置。AW 为本次会话生成回调条目，
不修改这些文件。如果原来通过 Qoder 的 `--settings` 传入额外 JSON 文件，改用：

```bash
target/debug/aw run --config ./aw.yaml --agent qoder --native-settings ./qoder.settings.json
```

AW 保留该文件的其他字段和 Hook 条目，加入自己的回调，再将合并配置传给 Qoder。
每个 AW 步骤成为独立的同步原生 Hook，串行或并行执行由 Qoder 决定，包括与
其他匹配 Hook 的共同调度。

设置 `spec.agents.qoder.qoder.sequential: true`，可将生成的组标记为顺序执行。
任一匹配的同步 Qoder 组要求 sequential 时，全部匹配的同步 Hook 都按顺序执行。
省略该字段或设为 false，不会覆盖已有组的 true。属于同一原生事件的步骤共享 AW
预算，后续回调不会重新计时。

当前支持 AW 步骤之间输入保持不变的脚本，不支持顺序输入重写链。若命令返回
`updatedInput`，后续回调输入变化会被拒绝，并按该步骤的 `on_error` 处理。
转交原生字节不代表已支持全部原生效果组合或审批流程。

启动器会拒绝禁用或替换接线的设置和参数，不会覆盖用户关闭 Hook 的选择，包括
直接传入的 `--settings`、`--setting-sources`、`--headless-fast-hooks` 和冲突的
原生配置。自定义配置根目录、其他 Qoder 配置目录模式、恢复会话、远程会话和 worktree 启动
尚不在当前 Adapter 范围内。短选项须分别传入，不能合并；请从所需工作目录启动。已有原生 Hook 的行为与审计
仍由它们自身负责。

## 服务生命周期与记录

`spec.daemon.startup: on_demand` 在选定端点没有服务时启动一个；`external`
要求服务已由外部启动。已有服务的协议和完整配置版本匹配时才会复用。
`endpoint: auto`、`state_dir: auto` 会在 `XDG_RUNTIME_DIR` 下选择按配置区分的
私有目录；该变量未设置时使用 `/tmp/aw-UID` 下的目录。显式路径必须是绝对路径，
并且指向同一个 `aw.sock` 位置。

服务保留启动配置快照，编辑 `aw.yaml` 不会热更新它。显式启用稳定桥接的实例可使用
[空闲策略更新](../../../user-guide/zh/aw/configuration.md#更新预热实例)，更换实例 Host，服务身份不变。停用某份配置前，用原文件或
socket 停止相应服务；修改配置后的 auto 路径可能指向另一个服务。退出 Agent 不会
停止其他会话或共享 daemon。本地端点是同用户访问边界，不提供 sandbox。

| 命令 | 用途 |
| --- | --- |
| `aw validate --config FILE` | 检查语法和静态引用，不执行程序 |
| `aw run --config FILE --agent TARGET [OPTIONS] -- ARGS` | 启动配置的 Agent 并接通受支持 Hook |
| `aw agent connect --config FILE --agent TARGET [--native-profile PROFILE]` | 向已有 Hermes profile 显式安装原生插件；Qoder、OpenClaw、QwenPaw、Pi、Claude SDK 和 AgentScope SDK 不支持 |
| `aw update --config CURRENT --agent TARGET --policy NEXT` | 更新已启用稳定桥接的空闲实例；busy 拒绝且不排队 |
| `aw serve --config FILE --state-dir ABSOLUTE_DIR` | 在前台运行服务 |
| `aw status --config FILE` 或 `aw status --socket ABSOLUTE_PATH` | 查看选定服务，不启动它 |
| `aw stop --config FILE` 或 `aw stop --socket ABSOLUTE_PATH` | 请求正常关闭服务 |
| `aw request --socket ABSOLUTE_PATH [--timeout-ms 1..60000]` | 从 stdin 读取一个开发者操作 JSON 对象，默认 5,000 毫秒 |

`--agent TARGET` 选择 `spec.agents` 中的命名对象，其 `adapter` 字段选择实现。
当前版本实现了 Qoder CLI、OpenClaw 2026.9.6、QwenPaw 2.2.2b4 / AgentScope 2.0.9
和 Hermes 官方提交 `952c941e` Adapter，以及显式 Claude SDK 0.2.88、Pi SDK 0.84.2
与 AgentScope SDK 2.0.6 worker。
`aw agent connect` 校验配置后分派到对应 Adapter；Hermes 支持持久化插件安装，
Qoder、OpenClaw、QwenPaw、Pi、Claude SDK 和 AgentScope SDK 拒绝此命令。
该命令不安装 AW 包或 Agent 软件，不启动 Agent，也不配置模型凭据。`aw run` 不会隐式调用 `agent connect`。
旧的 `aw install --config FILE --agent TARGET [--native-profile PROFILE]` 语法
保留相同的原生注册行为。软件安装使用 `aw install --prefix ABS_DIR --bundle ABS_DIR`；
两组参数不能混用。

| Adapter 专用参数 | 命令 | 当前支持情况 |
| --- | --- | --- |
| `--native-settings JSON_FILE` | `run` | Qoder：可选的附加 JSON 配置，与生成的 Hook 合并，原文件保持不变。OpenClaw：必填的现有原生 JSON 配置 |
| `--native-profile PROFILE` | `run`、`agent connect` | Hermes：`run` 和 `agent connect` 必填的已有 profile 绝对路径。Qoder 和 OpenClaw 拒绝此参数 |
| `--native-state-dir DIRECTORY` | `run` | OpenClaw：必填的原生状态目录绝对路径。Qoder 拒绝此参数，它不是 AW 服务的 `--state-dir` |

公共命令解析器识别这些 Adapter 专用参数，不代表框架已经支持它们。Pi、Claude SDK
和 AgentScope SDK 拒绝这些原生覆盖参数。QwenPaw 也拒绝它们，使用 `QWENPAW_WORKING_DIR`。
受支持的 AW 参数放在 `--` 前，之后的参数
按字面量交给 Agent；`agent connect` 不接受 Agent 参数。用 `aw --help` 查看命令格式
和当前支持范围。

源码构建时将 `aw` 替换为 `target/debug/aw`。`aw hook` 是启动器生成的内部回调，
无需用户手写。服务记录包含执行元数据，不包含工具输入或结果、Provider 私有配置
和原始 stdout/stderr。服务运行或调用完成不能证明原生策略已采用。原生回调被杀
或禁用时可能缺失检查，本版本不提供 final/protected 执行或 OS 兜底。

## 容器与持久化数据

在镜像中准备 AW 二进制、受支持框架版本、策略程序和默认配置。运行时提供原生
profile，并通过 `aw run` 启动所选 Agent；Dockerfile 无法保留活跃服务或可复用的
会话绑定。Hermes 须在启动前将匹配插件安装到**实际运行的 profile**。挂载到镜像
目录可能遮住构建镜像时安装的文件，参见 Docker 的
[挂载行为](https://docs.docker.com/engine/storage/bind-mounts/#bind-mounting-over-existing-data)。

| 数据 | 容器中的处理方式 |
| --- | --- |
| AW 二进制、脚本、固定 `aw.yaml` | 放入镜像或只读提供；可编辑配置保存在临时容器层之外 |
| `<state_dir>/journal/` | 需要跨容器重建保留执行历史时持久化；本版本审计固定开启且仅保存元数据 |
| Socket、锁、`launch-*` 和会话绑定 | 属于运行时产物，不能恢复会话；每次服务/Agent 启动建立新的运行状态 |
| 原生 profile、凭据、会话与工作区 | 按所选框架的要求保留，不属于 AW 审计数据 |
| sec-core 后端数据 | 由 sec-core 独立管理，见 [Preview 后端配置](packages.md#2-按需安装并启动-sec-core) |

即使不跨容器保留历史，AW 也需要可写的本地状态目录。需要持久历史时，将
`daemon.state_dir` 设置为支持 `fsync` 的本地持久文件系统上的绝对路径，保留
`endpoint: auto` 即使用该目录的 `aw.sock`。目录须属于运行 UID，权限为 0700；
直接父目录也须属于该 UID，且其他用户不可写。启动 AW 前，按实际容器 UID 准备
卷的属主。默认 `auto` 位于 `XDG_RUNTIME_DIR` 或 `/tmp` 下，属于临时存储。

长期运行的 Gateway/App 须按本 Preview 的上限安排生命周期：每个 AW 实例最多
保留 1,024 个不同的关联原生事件，同时启用 before 和 after 时，每次完成的工具
调用占用两个条目。耗尽后，打开新的关联事件以 `native_event_limit` 失败，此时
尚未运行步骤脚本。这是事件打开失败，不是 Provider 结果。AW 回调桥接将故障
转换为所选框架的原生响应；配置 `on_error: block` 的 before 回调请求拒绝执行。
工具的最终行为遵循该框架的原生 Hook 语义。退出并重新启动 AW 管理的 Agent 才会
建立新实例，另开原生会话不会重置上限。

当前状态目录同时包含 journal 与运行时产物。整体挂载持久卷时，正常停止会删除
所属 socket，强制终止容器则可能留下它。复用前确认旧服务已经退出，只清理该
遗留 `aw.sock`，不要删除活跃端点或 journal。独立 AW daemon 不共用一个状态目录。
容器关闭流程应先退出 Agent，再停止 AW 并等待所属进程。保留 journal 可以查询
历史，不会恢复或重放中断工具。Docker 卷在容器删除后继续保留，删除卷是单独
操作，见[卷生命周期](https://docs.docker.com/engine/storage/volumes/#a-volumes-lifecycle)。

## 常见接入问题

| 现象 | 下一步检查 |
| --- | --- |
| 静态校验通过但启动失败 | 检查所选原生版本/入口、程序路径和 Provider 私有配置；静态校验不执行它们 |
| 状态/profile 路径不安全 | 检查运行 UID、目录权限与显式原生路径，使用该用户拥有的私有目录 |
| 修改配置后行为不变 | 退出旧 Agent，用原配置/socket 停止旧服务，再启动编辑后的文件 |
| Hook 就绪失败 | 阅读原生启动错误，确认 profile/插件及受支持版本后再发请求 |
| 模型回答后没有审计 | 确认受支持工具确实到达回调；原生权限或模型拒绝可能阻止调用 |
| 强制终止后已有 socket | 确认旧服务退出，再按上文清理其遗留 socket |

## 运行本地服务演示

该演示无需 Agent 账号或模型请求。从 `src/aw` 执行：

```bash
cargo build --locked -p aw-provider --example policy
AW_DEMO_ROOT="$(mktemp -d "$PWD/target/aw-demo.XXXXXX")"
printf 'Socket: %s\n' "$AW_DEMO_ROOT/state/aw.sock"
target/debug/aw serve --config crates/aw-service/examples/aw.yaml \
  --state-dir "$AW_DEMO_ROOT/state"
```

在另一个终端进入 `src/aw`，使用打印的绝对 socket 路径：

```bash
AW_DEMO_SOCKET=/absolute/socket/path/printed/above
target/debug/aw status --socket "$AW_DEMO_SOCKET"
cargo run --locked -p aw-service --example local -- "$AW_DEMO_SOCKET"
```

示例使用合成能力和事件，检查 `read_demo`、阻断 `delete_demo`、观察工具后事件，
并输出审计键。没有启动原生 Agent 或工具。将 `AUDIT_KEY` 替换为返回的准备记录键
或事件 ID，可以查询记录：

```bash
printf '%s\n' '{"method":"audit","key":"AUDIT_KEY"}' | \
  target/debug/aw request --socket "$AW_DEMO_SOCKET"
target/debug/aw stop --socket "$AW_DEMO_SOCKET"
```

`terminal: false` 表示尚无终结记录，可能仍在运行，也可能已中断。调用超时后不得
作为新步骤重新执行。停止响应仅确认收到请求；等前台服务退出后，在原终端删除
本次演示目录：

```bash
rm -r -- "$AW_DEMO_ROOT"
```

正常关闭保留审计历史，删除所属 socket。强制终止后，AW 会报告遗留 socket，
不会自动删除。确认旧服务已停止后，再移除其所属 `aw.sock`；保留服务目录时，
保留其中的锁和 Journal。重启产生新的服务身份，历史记录仍可查询，但不会恢复
或重放旧事件。

[配置参考](configuration.md)列出完整字段和事件名。
[本地服务合同](../../../../src/aw/docs/design/zh/runtime/local-service.md)面向 Adapter 开发者，
说明操作 JSON、截止时间和生命周期。

sec-core 扫描通过独立的 `aw-provider-sec-core` 接入，见[配置与使用](../../../user-guide/zh/aw/security.md)。

模型可见的 PostTool 压缩使用可选的 [Tokenless Provider](../../../user-guide/zh/aw/tokenless.md)。

## 启动 Pi SDK worker

高代码 Pi 应用使用 `adapter: pi`、公开 Coding Agent/Core SDK 0.84.2 与
Node.js >=22.19.0。AW 管理前台 SDK worker，并接通工具前后两个 Hook 点。
按应用现有依赖流程安装这些 SDK 包，保留应用模型与认证配置。原生 Pi CLI 是
独立入口，这个 Adapter 不启动该 CLI。

在绝对路径的 `.mjs` 应用模块中暴露已有公共工厂与前台任务：

```javascript
export async function createSession({ sdk }) {
  return sdk.createAgentSession({
    cwd: process.cwd(),
    agentDir: "/absolute/existing-pi-agent-directory",
  });
}

export async function run(session, { args }) {
  await session.prompt(args.join(" "));
}
```

复制 [Pi 示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.pi.yaml)，
将 `argv` 改为 `[node, /absolute/application.mjs]` 并设置工具前后策略。
附带命令为中性示例。随后执行：

```bash
cd src/aw
target/debug/aw validate --config /absolute/aw.pi.yaml
target/debug/aw run --config /absolute/aw.pi.yaml --agent pi -- 'Your task'
target/debug/aw status --config /absolute/aw.pi.yaml
target/debug/aw stop --config /absolute/aw.pi.yaml
```

`--agent` 选择配置中的目标名，`adapter` 保持为 `pi`。`--native-settings`、
`--native-profile` 与 `--native-state-dir` 不是 Pi SDK 选项。AW 绑定前核验实际
安装的 SDK 版本，等待工厂与 callbacks 安装，随后报告 `AW pi native hooks ready`。
应用任务必须留在同一个前台 worker。new/resume/fork/import 替换会话使用传给
`run` 的 `createSession`，保证每次工厂返回后安装同样的 Hook 组合。已有 before
拒绝/参数修改、after 结果替换继续生效，AW after 观察最终结果。Pi 被拒绝、参数
非法、提前取消的工具没有 after callback。

Tokenless 使用[共享优化示例](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml)中的 Pi SDK 目标。
安全检查位于 `pre_tool` 之前，观察器/自定义 Hook 位于最后 `post_tool` 替换之前。
重试理由进入模型，建议命令只能通过新工具调用执行。PostTool 替换成功结果的唯一
文本块并保留 SDK metadata；report 故障保留原结果。原生 Hook 不能提供替换。
AW 每次生成 invocation UUID 配对，原工具 ID 保留于 `native`，提前拒绝时关闭事件。
精确 callback、身份、取消、容量与可
复现的 SDK 验收见 [Adapter 合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/zh/adapters/pi-adapter.md)。
修改策略前退出 Agent 并停止原服务；审计状态按同样的本地服务清理流程保留或删除。

## 启动 Claude SDK worker

使用 Claude Agent SDK 0.2.88 及其 bundled CLI 2.1.161。Adapter 启动一个前台
Python script 或 `-m` module，要求 worker 公共工厂显式使用
`aw_claude.create_client(options)`。模型认证继续由 SDK 配置。AW 为启动的 worker
通过 `PYTHONPATH` 提供此模块，不会自动向任意 SDK 应用注入 Hooks。

源码示例从 `src/aw` 运行，先准备选定的 Python 环境。固定 MCP 版本是为了处理已
复现的 MCP 2.x 不兼容问题：

```bash
python3 -m venv target/claude-sdk-venv
target/claude-sdk-venv/bin/pip install -r tests/claude-sdk-requirements.txt
```

复制 `crates/aw-service/examples/aw.claude-sdk.yaml` 为自己的配置，将 Agent argv
解释器改为 `target/claude-sdk-venv/bin/python` 的绝对路径。保留前台示例 worker 路径，
或替换为业务 script。在公共工厂完成普通 MCP tools、权限、resume 与既有 Hooks
配置后使用以下模式：

```python
from aw_claude import create_client

async with create_client(options) as client:
    await client.query(prompt)
    async for message in client.receive_response():
        handle_message(message)
```

```bash
target/debug/aw run --config /absolute/aw.claude-sdk.yaml --agent claude -- 'Use echo to return hello.'
target/debug/aw status --config /absolute/aw.claude-sdk.yaml
target/debug/aw stop --config /absolute/aw.claude-sdk.yaml
```

Claude SDK 事件预算必须为 1..55,000 ms。每个 `tool.before` 或 `tool.after` 路由
优先使用 `events.<name>.budget_ms`，未设置时使用 `execution.default_event_budget_ms`。
`aw run` 在静态配置校验后检查此 Adapter 专有限制。

worker options cwd 必须等于启动 cwd。真实 SDK initialization 握手接受 callbacks
后才发布 readiness。AW 在 `PreToolUse` 观察或拒绝，`PostToolUse` 与
`PostToolUseFailure` 均供 after 观察。被拒绝工具不补造 after。既有 Hook veto 和
原生权限继续有效。既有 pre-hook `updatedInput` 或 permission `updated_input`
发生修改时明确拒绝，因为并发 callbacks 无法保证 AW 检查最终参数。启用
`tool.before` 时，非空 `PermissionRequest` matchers 在初始化前被拒绝；请改用
受支持的 `PreToolUse` 或 `can_use_tool` callbacks。启用 `tool.before` 时也拒绝调用方
指定 `permission_prompt_tool_name`；受保护 `can_use_tool` 的 SDK 内部 stdio 权限
通道继续支持。调用方
`cli_path` 覆盖、缺失 bundled binary 均被拒绝。worker 不得 detach，也不得在一个
launcher binding 中运行多个 SDK clients/任务；Gunicorn 和业务部署需要自己的任务
生命周期接线。

CLI 2.1.161 对 SDK MCP `isError: true` 响应仍发 `PostToolUse`，callback 数据不含
该 flag，因此 after 观察不能分类所有业务失败。原生 Bash 执行失败使用
`PostToolUseFailure`。身份、取消与验证限制见
[Adapter 合同](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/docs/design/zh/adapters/claude-sdk-adapter.md)。

无模型的原生采纳验证使用真实 SDK/bundled CLI，连接本地确定 HTTP fixture。
先构建 AW，再运行测试；测试创建独立 profile，私有 UDS 目录也位于 output 下，
停止 owned services 后删除。socket 路径超过 107 bytes 时明确报错：

```bash
target/claude-sdk-venv/bin/python tests/test_claude_sdk_native.py --aw target/debug/aw --output target/claude-sdk-native
```

不再需要证据与环境时，仅删除新建的 `target/claude-sdk-native` 和
`target/claude-sdk-venv` 目录。AW 策略是示例，业务部署验收与这些本地原生检查分开。

工厂要求 `setting_sources=[]`、无 `settings` 覆盖、无 CLI plugins。`extra_args`
仅允许 `strict-mcp-config: None`；SDK 原生 `strict_mcp_config` 也保留。模型和认证
配置不变。通过 settings sources 发现项目 skills 的应用，需要先明确接入设计才能
使用该首版受限 Bridge。每个前台 worker 只允许创建一个 client。

当前 `adapter: claude-sdk` 支持上述受限前台 worker 工厂；`aw agent connect` 对该
Adapter 不提供持久安装命令，所有 `--native-*` overrides 均不支持。

### 启用 Bash PreTool 与 PostTool 优化

使用 [Tokenless 共享配置](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml)，
选择 `claude` target，替换 executable、worker 与状态目录的绝对路径。源码示例
`claude-worker.py` 除 echo 工具外显式开放并允许原生 Bash；组合业务工厂时保留应用
自己的权限策略。Provider 安装、RTK 要求与统计设置见 [Tokenless 指南](../../../user-guide/zh/aw/tokenless.md)。

PreTool 阻断原始 Bash 尝试，将重试参数返回模型；新调用重新经过所有策略及原生权限。
PostTool 只替换完成、非图片且未中断的 Bash stdout。完成观察器和自定义步骤放在唯一
最终 `replace_result` 前，看到原始结果；stderr 与其余原生字段保留。失败/report
保留原始结果。MCP tools 仍可观察，但原生回调不能可靠报告业务错误状态，因此不压缩。

构建 AW、其 Tokenless Provider 与当前 Tokenless Protocol v2 CLI 后，扩展原生验收
（PATH 中需要 RTK）：

```bash
cargo build --locked --manifest-path ../../providers/tokenless/Cargo.toml
target/claude-sdk-venv/bin/python tests/test_claude_sdk_native.py \
  --aw target/debug/aw --output target/claude-sdk-native \
  --tokenless ../tokenless/target/debug/tokenless \
  --tokenless-provider ../../providers/tokenless/target/debug/aw-provider-tokenless
```

# QwenPaw

[English](../../../en/aw/agents/qwenpaw.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 QwenPaw

已验证基线为 QwenPaw 2.2.2.post1 与 AgentScope 2.0.10。安装 QwenPaw 及其兼容依赖，
通过 QwenPaw 初始化模型配置。其他版本沿用插件、middleware 与原生任务生命周期接口检查，
AW 记录已安装版本。示例
使用 `argv: [qwenpaw, app]`。将 `QWENPAW_WORKING_DIR` 指向已初始化的绝对目录。
AW 只添加自身临时插件，保留已有插件和原生配置，App 退出后删除自身插件。

普通启动会检查每个已构造 Agent 实际安装的 resident middleware，并创建已确认的
任务作用域。Before 正序执行、After 逆序展开，原生工具并发由宿主决定。Offload 工具
保留作用域直到生成器、原生 Hook 和后台完成回调全部结束；已完成调用及时回收状态，
长会话按活跃调用容量准入。

后台 HTTP 输出和下一轮提示在全部必要原生回调成功后才发布；拒绝结果保留策略说明及
Tokenless 重试参数。脱敏函数必须幂等，以支持多个交付及 Hook 边界重复检查。
After Hook 接收脱敏后的终结 `ToolResponse`，包括 AW 拒绝结果。应用凭据扩展提供
每次 Agent reply 一个私有执行租约，由该 reply 的工具共享，并负责清理；见
[私有执行租约](../../../../../src/aw/docs/design/zh/adapters/private-execution.md)。
授权所依据的工具参数保持原样。`on_error: report` 保持配置的
回调故障报告策略，工具前 `block` 拒绝执行。结构化 Provider 和布尔 Hook 的工具后
`withhold_result` 阻止未核验结果交付及后续派发，仅普通 resident 启动准入该动作。
严格 After 路径暂扣匹配工具的流式 chunk；任务私有执行也会暂扣中间 chunk，直到终态脱敏。
原生 Hook 或生命周期故障会封锁 Agent，包括 AW 步骤配置为 `report` 的情况；
修复原因后重启失败的 Agent。清理超时或 finish/release 确认状态不明会在进程范围内
封锁新请求，须重启 worker；凭据保留至原生工作确认排空。Shutdown 与嵌套清理
共用十秒预算，包含取消后的排空。
事件预算为 1..55,000 ms，ask/approve 指令尚不支持。
原生权限检查在这些回调之前执行；外部执行工具和此前的权限拒绝沿各自原生路径处理。

Startup 回执确认插件加载，每个 Agent 在请求开始前另行通过实际安装检查。看到原生
Hook 就绪信息后使用 App/API；ACP/TUI、多 worker 启动继续被拒绝。`--reloadable`
保留独立的请求空闲更新合同；普通启动提供 resident 子作用域、长会话回收，以及 reply
完整排空后的显式更新。复用的 Agent 在每轮新 reply 预留任务后读取活动策略。
App startup 在首个 reply 前确认更新就绪；启动策略持续生效，直到更新成功。共享
子作用域尚无原生证据采集合同，普通 resident 启动会拒绝该配置。

每次安装使用独立的 `aw-native-<uuid>` 目录与 manifest ID，保留用户已有的
`aw-native` 插件及配置。`config.json` 加载和 MCP 动态更新由 QwenPaw 负责。
AW 原样转发原生工厂参数，工具生成器完成清理后再向 Coordinator 交付终态结果。

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home target/debug/aw run \
  --config crates/aw-service/examples/aw.qwenpaw.yaml --agent qwenpaw \
  -- --host 127.0.0.1 --port 8096
```

[QwenPaw 中性示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qwenpaw.yaml)
在工具前后执行命令，不安装安全策略。受支持的适配器可复用同一份
`aw-provider/v1alpha1` 策略配置；原生 Hook 的输出格式与事件覆盖仍按框架分别说明。

零 Provider、零事件步骤的普通配置也会安装 resident 准入与清理 middleware。

共用的 sec-core Provider 还提供[加载前 Skill 校验](../providers/sec-core.md#加载前校验-skill)。在 `tool.before` 为原生 `Skill` 工具配置 `check_skill`，并在 daemon 授权实际规范化来源目录。

## 保留原生 App 启动命令

已在独立 Python 环境中初始化 QwenPaw 时，可以一次安装 AW 启动模块，然后重启 App。
将 `aw.yaml` 中 `qwenpaw` 目标的 `argv` 配置为实际使用的可执行文件和参数，包含 host、port。
先使用同一份配置启动 AW Service。

在 `aw.yaml` 所在目录执行准备，将 Python 路径换为独立 QwenPaw 环境的解释器：

```bash
QWENPAW_PYTHON=/absolute/qwenpaw-venv/bin/python
AW_STARTUP_DIR="$("$QWENPAW_PYTHON" -c 'import sysconfig; print(sysconfig.get_path("purelib"))')"
aw instance setup --agent qwenpaw --directory "$AW_STARTUP_DIR" > aw-startup.json
export AW_INSTANCE_BOOTSTRAP="$(python3 -c 'import json; print(json.load(open("aw-startup.json"))["environment"]["AW_INSTANCE_BOOTSTRAP"])')"
export QWENPAW_WORKING_DIR=/absolute/qwenpaw-home
qwenpaw app --host 127.0.0.1 --port 8096
```

将返回的启动环境保存在应用的 service 或 shell 配置中。准备命令在明确选择的 Python
环境中创建 `.pth` 激活文件，保留已有 `sitecustomize`。目录须归当前用户所有，且禁止组和
其他用户写入；准备命令拒绝覆盖已有激活文件。App 自行准备实例，从 AW 私有目录加载普通 resident 插件，在原生插件启动完成后确认就绪。此前到达的请求不能构造 Agent；
实际启动命令与准入目标不一致时，在 App 启动前失败。

修改 `aw.yaml`，暂停任务派发后在另一终端更新：

```bash
aw status --config aw.yaml
aw instance update --agent qwenpaw
```

输入、工具、严格 After 与 reply 清理沿用普通模式的接点；原生 App、用户插件和 MCP
配置继续由 QwenPaw 管理。策略在回复与清理排空后激活，正常关闭释放实例和临时插件资产。
移除集成时，先停止 App，删除 `aw-startup.json` 列出的本次准备文件，取消
`AW_INSTANCE_BOOTSTRAP`，再重新启动。此路径支持单个原生 App 进程；ACP、TUI 和多进程
需要单独接入。

原生 MCP 连接配置与原生权限策略有各自的更新生命周期，需要串行修改并核验结果。
固定版本 QwenPaw 中，较早发起的连接重载若在权限更新后完成，可能恢复旧的原生权限策略。
AW 实例策略仍按独立的准入边界执行。

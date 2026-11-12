# QwenPaw

[English](../../../en/aw/agents/qwenpaw.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 QwenPaw

已验证基线为 QwenPaw 2.2.2.post1 与 AgentScope 2.0.10。安装 QwenPaw 及其兼容依赖，
通过 QwenPaw 初始化模型配置。其他版本沿用插件、middleware 与原生任务生命周期接口检查，
AW 记录已安装版本。示例
使用 `argv: [qwenpaw, app]`。将 `QWENPAW_WORKING_DIR` 指向已初始化的绝对目录。
普通启动识别出绝对路径 Python 标准启动脚本后，从私有临时包通过原生 Registry 注册 AW，
减少文件系统插件加载后的全局模块扫描，并保持工作区插件目录原样。AW 负责该集成及其
退出清理，QwenPaw 的文件系统插件列表不展示该集成。自定义入口、`--reloadable` 和外部
启动集成继续使用原生插件加载器。

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
App startup 在首个 reply 前确认更新就绪；启动策略持续生效，直到更新成功。
每个确认就绪的回复 Scope 独立保存有界原生回调观测，关闭前排空。已核实的原生身份
可查询配置的证据源；证据源缺失或不可用时保留具体缺口与未知覆盖。

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

识别出的绝对路径 Python 标准启动脚本，使用同一解释器与导入路径读取实际安装包的
字面量版本，避免为版本探测加载 QwenPaw App 配置。自定义入口和动态版本继续使用
原生 `--version`，共用五秒预算。原生 Hook 安装与就绪检查沿用原生接口。受管启动在同一
解释器、同一进程内执行原启动脚本，保留原参数和导入优先级；用户插件沿用原生加载与
隔离机制。Startup 回执确认前到达的请求最多等待 30 秒，再开始构造 Agent。等待边界在
原启动脚本执行前安装，覆盖首次插件加载前 Hub 初始化期间到达的请求；Agent 构造还须
通过 resident middleware 实际安装检查。AW 模块在原生插件初始化阶段、
Agent 加载前导入。初始化失败会停止 App，回执缺失或不匹配时阻止 Agent 构造。

## 分开统计 HTTP 启动与首个受保护操作

分别统计启动到 HTTP、HTTP 到 Agent 就绪，以及首个 Hook 的延迟。HTTP 监听可以早于
Agent 和插件启动完成，监听可用本身不代表已经能够处理受保护请求。

部署使用 [sec-core 首用等待](../providers/sec-core.md#重叠后端与-agent-启动)时，
外部 supervisor 可以解除 App 启动对后端健康的前置依赖。CPU 受限的容器可在 HTTP
可用后启动扫描预热，减少与 App 前期导入争抢 CPU，也可在首用时初始化能力。
只预热活动策略引用的能力。HTTP 启动由此独立于后端就绪；必需 Hook 安装、策略准入
和实际检查仍在受保护模型请求或工具操作前完成。按该里程碑启动后端或预热由 supervisor
负责，`aw run` 不调度或管理这些外部进程。
如果 App 初始化本身会触发受保护操作，应独立启动后端，避免该操作等待后端、后端又
等待 HTTP 的循环依赖。此时只后置主动扫描预热，首次实际检查仍初始化所需能力。

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

QwenPaw 2.2.2.post1 原版的异步 MCP 重载可能在权限更新确认后重新发布旧权限。
需要原生权限热更新时，采用包含[驱动发布修复](https://github.com/kongche-jbw/QwenPaw/commit/6e7fb8a19ce61eca3b8dbc86d1818fc94b7252a8)
的版本。已验证的 [2.2.2.post1 回补提交](https://github.com/kongche-jbw/QwenPaw/commit/099f645819218256364e7d63ae35dce21e383bda)
属于未发布 Preview：原生 App 和 AW 管理 App 均已验证并发重载期间撤权，拒绝调用没有
服务端副作用，后续正常重载可恢复。权限确认后的新调用采用最新策略，已经授权的在途调用
可以完成。

先停止 App，在专用环境中安装该固定提交构建的完整 wheel，再重启。源码构建使用
QwenPaw 的 [wheel 构建脚本](https://github.com/kongche-jbw/QwenPaw/blob/099f645819218256364e7d63ae35dce21e383bda/scripts/wheel_build.sh)，
保留 Console 静态资源。继续使用未修复版本时，运行期间保持原生 MCP 配置与权限不变；
变更应在 App 退出后完成，并核验持久配置后再启动。

`aw instance update` 更新 AW 策略，继续按回复空闲边界生效。AW 的 `tool.before`
阻断保护经过其 Hook 的后续调用；原生驱动的并发控制由上述 QwenPaw 修复提供。

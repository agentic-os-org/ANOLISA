# 应用自主管理的 Runtime SDK

[English](../../../en/aw/deployment/native-sdk.md)

应用自主管理启动并需要 AW 策略与生命周期管理时，可使用 `@anolisa/aw-sdk` 或异步
`anolisa-aw-sdk` Python 核心包。两个核心包提供连接传输、`Instance`、`Task`、`Admission`、
`TaskScope`、部署 Descriptor 加载、协作式控制与通用语言级 Source Runtime
（`@anolisa/aw-sdk/sources` 与 `aw_sdk.sources`）。各框架专属集成作为独立可选适配包分发
（`@anolisa/aw-sdk-qoder`、`@anolisa/aw-sdk-pi`、`@anolisa/aw-sdk-deepseek-harness`、`anolisa-aw-sdk-qwenpaw`、
`anolisa-aw-sdk-claude`），不再物理捆绑在核心包内。要求 Linux，Node 包要求 Node.js 22+。
AW 与 Provider 分别安装。当前交付源码构建候选，registry 发布沿 AW 发行流程。

AW 应与语言包采用同一候选源码版本构建。未提供 `aw-sdk/v1` 的既有发行版会拒绝
连接；协作式控制还要求 `aw-sdk-control/v1`。安装语言包不会升级 AW。

## 候选包结构与选型

候选构建产出 2 个通用核心包与 5 个可选框架适配包（3 个 Python wheel 与 4 个 npm tarball）。
请按适用发行渠道获取 AW 核心包与双语言 SDK 产物（例如可用的 OS 软件包、发布或 CI 归档，或按
[源码构建指南](../../../../../contribute/zh/aw/building.md)自行构建），并按
[安装指南](../../../../user-guide/zh/aw/installation.md#获取安装包)校验完整性。
各适配包严格锁定对应的核心包版本，按实际使用框架按需安装：

| 语言 | 包名 | 角色 | 主要导入 | Descriptor Profile |
| --- | --- | --- | --- | --- |
| Python（`>=3.11`） | `anolisa-aw-sdk`（核心包） | 传输、`Instance`/`Task`/`Admission`/`TaskScope`、Descriptor 加载、协作控制、`LanguageSource` | `aw_sdk`、`aw_sdk.sources` | —（`adapter: sdk`） |
| Python（`>=3.11`） | `anolisa-aw-sdk-claude`（可选） | 常驻 Claude Agent SDK Worker | `aw_sdk_claude`（`ResidentClaude`） | `claude-resident-hooks` |
| Python（`>=3.11`） | `anolisa-aw-sdk-qwenpaw`（可选） | QwenPaw 原生插件与工具后延迟插件 | `aw_sdk_qwenpaw`（`QwenPawPlugin`、`AfterOnlyPlugin`） | `qwenpaw-installed-native-hooks` |
| TypeScript / Node (`>=22.19`) | `@anolisa/aw-sdk-deepseek-harness` | [Cordis Bridge](../agents/deepseek-harness.md) | `createDeepSeekBridge` | `deepseek-cordis-hooks` |
| TypeScript / Node（`>=22`） | `@anolisa/aw-sdk`（核心包） | 传输、`Instance`/`Task`/`Admission`/`TaskScope`、Descriptor 加载、协作控制、`LanguageSource` | `@anolisa/aw-sdk`、`@anolisa/aw-sdk/sources` | —（`adapter: sdk`） |
| TypeScript / Node（`>=22`） | `@anolisa/aw-sdk-qoder`（可选） | Qoder Agent SDK 公开 Hook 桥接 | `@anolisa/aw-sdk-qoder`（`createQoderBridge`） | `qoder-public-hooks` |
| TypeScript / Node（`>=22.19`） | `@anolisa/aw-sdk-pi`（可选） | 常驻 Pi 编程 Agent Worker | `@anolisa/aw-sdk-pi`（`runPiWorker`） | `pi-worker-hooks` |

按接入场景选择路径：

- **自研 Python / Node.js Agent（零 Rust Adapter）**：仅安装核心包，使用 `LanguageSource`
  插桩输入与工具方法（见[通用应用接入](#通用应用接入)）。
- **常驻框架应用**：安装核心包 + 对应可选适配包，用 `aw config descriptor` 编译启动提示，
  原生启动应用后通过 `aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID` 协作挂接
  （见[部署 Descriptor](#部署-descriptor)）。

## 按配置接入 Qoder

在应用中同时安装核心包 `@anolisa/aw-sdk` 与可选适配包 `@anolisa/aw-sdk-qoder`。
使用 `createQoderBridge`（从 `@anolisa/aw-sdk-qoder` 直接导入，或经由兼容子路径
`@anolisa/aw-sdk/qoder` 按需转发）保留业务原生 Node 入口、daemon、认证、模型、权限和 Hook。
接入模块读取部署所用的同一份自有 `aw.yaml`，安装 Qoder 公开 Hook 并发布协作监听。
部署生成的 Descriptor 可让初始化直接读取已验证的接线，无需启动配置解析子进程。
未传入 `descriptorFile` 时，继续通过一次有界 CLI 调用复用 AW 离线 `config show`
解析 YAML／JSON。两条路径均不启动 AW 或 sec-core。

```javascript
import { createQoderBridge } from "@anolisa/aw-sdk-qoder";
import { query } from "@qoder-ai/qoder-agent-sdk";

const aw = await createQoderBridge({
  configFile: "/absolute/aw.yaml",
  query,
});
const session = aw.query({ prompt, options: nativeOptions });
for await (const message of session) deliver(message);
// At application shutdown:
await aw.close();
```

将 `spec.daemon.endpoint` 或 `spec.daemon.state_dir` 设置为私有目录中的绝对路径；
两项同时为 `auto` 时，本接入模块拒绝初始化。配置只有一个 `qoder-sdk` 目标时自动选择，
多个目标时传入 `target`。接入模块默认采用 `availability: "native_fallback"`，AW 失联后
原生 Query、Hook、工具和模型继续运行。该目标适用的每个启用事件设置 `required: false`；AW 可达时，
`on_error: block` 和明确拒绝仍然生效。该目标适用的启用事件要求 required 时，回退模式拒绝初始化。
需要在 AW 失联时停止受保护操作，可显式传入 `availability: "required"`。
可用性属于 SDK 生命周期选项，策略统一留在文件中。`qoder-public-hooks` 支持已配置输入和工具前的
观察与阻断，以及工具后的观察与 `replace_result`。替换步骤使用 `on_error: report`，排在观察步骤之后。
所有步骤接收原始工具结果。Adapter 在 `PostToolUse` 中通过 `hookSpecificOutput.updatedToolOutput`
替换受支持的文本槽，保留结果的其他字段。`PostToolUseFailure` 提供错误详情供观察，缺少可替换的结果槽。Provider 出错时，接入层向应用
传递错误并请求停止 Query。停止请求无法证明 Qoder 已扣留未经检查的结果。需要 `withhold_result`
或最终结果检查时，使用已验收的受管入口。

业务原生就绪后，部署方后台准备 sec-core，并以应用用户执行：

```bash
aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID --timeout-ms 10000
```

设置 `daemon.startup: on_demand` 时，命令启动或复用 AW。PID 对应已安装接入模块的 Node
应用。接入层只为各自启用的事件安装 `SessionStart` 和 `UserPromptSubmit`。两项都关闭时，
模型推理可与挂接并行，首次 `PreToolUse` 固定 Task 策略。required 准入最多等待
`attachTimeoutMs`，默认 5000 ms。回退模式可在 AW 不可用时继续原生运行。只配置工具后策略时，
仍使用内部工具前观察关联调用。准入后的每个 Query 持有一个 Scope。

目标启用 `session.start` 时，新生成的 `qoder-public-hooks` Descriptor 包含可选的
`observe_session_start` 字段。
接入层将缺少该字段的旧 Descriptor 按关闭处理。启用 `session.start` 时重新生成
Descriptor，变更任一启动事件后重启接入模块。

`aw.query` 接受原生 Query 输入，返回保留原方法的代理；消费结束、提前退出迭代或显式
`close()` 会关闭原生 Query 与 Scope。应用结束时关闭其余 Query。每个 Query 的 cwd
和复制后的环境构成任务上下文；`QODER_PERSONAL_ACCESS_TOKEN` 留在原生认证中，
不进入 AW Hook 环境。其他敏感变量通过接入模块的 `environment` 选项排除。
同一 Query 保留一种 AW 接线。该路径使用公开 Hook，不向原 SDK 添加模块补丁或 preload。

## Pi Worker 接入

在 Node.js 22.19 及以上同时安装核心包 `@anolisa/aw-sdk` 与可选适配包 `@anolisa/aw-sdk-pi`
（兼容子路径 `@anolisa/aw-sdk/pi` 会在已安装该可选包时转发调用）。业务模块导出
`createSession({ sdk, cwd, args, ...options })` 和
`runWorker({ args, createSession, closeSession, withSession })`。模型、设置、工具和
原生回调仍在业务工厂配置。受保护操作通过传入的 `createSession` 和
`session.prompt()` 执行。

```javascript
import { runPiWorker } from "@anolisa/aw-sdk-pi";

await runPiWorker({
  application: "/absolute/pi-worker.mjs",
  configFile: "/absolute/aw.yaml",
  descriptorFile: "/absolute/aw-pi.descriptor.json",
  args: process.argv.slice(2),
  automatic: false, // 设为 true 时常驻 worker 重建后自动向 Service 重连
});
```

部署阶段用 `pi-worker-hooks` Profile 编译 Descriptor，Worker 启动时不再调用
`aw config show`；省略 `descriptorFile` 时使用离线解析。在配置中选择 Pi 目标，
显式指定 daemon endpoint 或状态目录。进程进入 Worker 前启动 SDK 监听；可选布尔参数
`automatic`（默认 `false`）透传给 `instance.listen({ automatic })`，设为 `true` 时支持
常驻 worker 重建后免外部追 PID 自动重连。部署侧以应用所属 Linux 用户接入。该入口始终要求 AW 可用。

```bash
aw config descriptor --config /absolute/aw.yaml --agent pi \
  --profile pi-worker-hooks > /absolute/aw-pi.descriptor.json
aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID --timeout-ms 10000
```

版本支持与 `aw run` 托管入口共用 Pi Adapter 的同一份能力声明，SDK 入口不另设版本判断。
Coding Agent 与 Agent Core 的已安装版本均被接受；仅当两者为相同的正式版本且位于已验收的
0.84.2–1.5.2 区间、并具备原生事件派发接点时，Worker 才声明 `withhold_result`。
区间外的版本照常提供工具 Hook，但需要严格结果保护的配置会在注册时被拒绝，不会静默降级。
本次 SDK 接入的输入检查和严格结果套件在 0.84.2、1.3.0、1.5.2 三个固定版本上验收。

创建会话须等待 AW Ready，`attachTimeoutMs` 默认 5000 毫秒。每个会话使用一个确认后的
Scope，Worker 验证并安装原生 Hook 后确认，关闭会话时排空工作并释放该策略快照。
会话 Scope 复用托管入口的常驻 Scope 协议，由 SDK Task 承载注册、确认、续租与释放。
工具策略也支持现有 `run(session, { args, createSession, closeSession })` 工厂。
输入检查要求 `runWorker`，从仅工具配置切换为启用输入检查时须重启 Worker。

## 公共准入边界

TypeScript、Python 均在 Instance／Task 旁导出 `Admission`。
Source 先安装原生回调，再为每个原生请求创建一个 Admission，传入已安装事件集合及复制的
目录／环境。在最早的受保护输入或首次工具进入前调用 `prepare()`。
公共 SDK 负责有界 Ready 等待、唯一 Task、取消和关闭；Source 负责原生捕获、字段映射和
效果采纳。策略准入与 Provider 执行继续由 Service／Host／executor 承接。

已安装 `input.submit` 时，可选可用性同样先等待准备；达到 attach 期限后才允许可选输入
回退，明确错误和取消继续传播。required 工具准入等待就绪；可选工具策略在未接入时立即
固定原生回退。`close()` 会取消待完成准入；释放 Task 前应先停止并排空原生任务。
Source 负责在正确边界调用 `prepare()`，SDK 不从 PID 推断模型派发或工具执行。

Qoder bridge 和[配置式 QwenPaw 插件](python-sdk.md#安装与接入)复用这条生命周期。
部署 Descriptor 使用现有已安装能力 Profile；自动 Source 发现和解析仍属后续交付。

通用 `adapter: sdk` 目标未配置内置 Profile 时，运行 `aw config descriptor`
并指定 `--profile application-hooks`，使用公共 `aw-hook-source/v1` 应用能力上限。

## 内置插桩 Profile

使用包含目标包与运行时所需 Profile 的 AW 构建。将
`spec.agents.<target>.adapter` 设为 `sdk`，在 `spec.agents.<target>.sdk.profiles`
列出内置 ID。应用的 Python 或 Node.js 环境须安装与 AW 构建匹配的核心 SDK，
并使用会执行预加载入口的常规启动方式。当前生产构建的内置目录为空，仓库测试构建提供
Python 和 Node.js 两套测试 Profile。

将 `config` 设为配置绝对路径，`target` 设为 SDK 目标。
运行 `aw run --config "$config" --agent "$target"`，应用通过 `required`
模式接入 AW。AW 管理预加载文件及应用进程组，进程组退出后清理文件。进程退出时若缺少
SDK 排空确认，`aw run` 报告 `source_cleanup_unconfirmed` 并以失败退出。
Python 在守护线程中运行 SDK 事件循环。未进入显式
`async with preloaded_profile_source.request()` 的调用使用进程级 Scope。

应用自行启动时，将 `directory` 设为私有启动目录的绝对路径，执行：

```bash
aw instance setup --config "$config" --agent "$target" --directory "$directory"
```

将输出中的 `environment` 应用到启动环境。Python 启动脚本会继续加载应用原有的
`sitecustomize`，Node.js 保留现有的 `NODE_OPTIONS`。预加载入口在应用工作开始前
消费本次启动的环境变量。用 `aw connect --config "$config" --sdk-pid APPLICATION_PID`
挂接已运行的应用，用 `aw disconnect --config "$config" --sdk-instance APPLICATION_ID`
释放空闲连接。挂接结果提供 `application_id`。协作模式下，新调用或显式请求通过当前
连接准入 Scope，`required` 调用在 SDK 截止时间内等待挂接确认。

可用 `aw config descriptor --config "$config" --agent "$target"` 单独编译并检查同一份
Descriptor。如需交给 SDK 读取，将输出文件安装为 `0400`。配置或构建内置 Profile 版本
变更后，选择新的私有目录重新运行 setup。应用退出后可删除 setup 输出列出的文件。
自定义 Profile 部署和另外七个可插桩事件归入 M3。
参见 [Profile 能力](../../../../../src/aw/docs/design/zh/core-admission.md#插桩-profile)。

## 部署 Descriptor

常驻框架应用的协作式挂接分为三步：

1. **部署阶段编译绑定配置的 Descriptor**（免去运行期 YAML 解析与 `aw` 子进程启动）：

```bash
umask 077
aw config descriptor --config /absolute/aw.yaml --agent sdk \
  --profile qoder-public-hooks > /absolute/aw-sdk.json
```

2. **原生启动应用**，传入 `configFile` 与 `descriptorFile` 两个绝对路径，安装原生 Hook 并发布 SDK 监听：

```javascript
const aw = await createQoderBridge({
  configFile: "/absolute/aw.yaml",
  descriptorFile: "/absolute/aw-sdk.json",
  query,
});
```

3. **在应用与外部 Provider 后端就绪后按 PID 协作挂接**（同 Linux 用户）：

```bash
aw connect --config /absolute/aw.yaml --sdk-pid APPLICATION_PID --timeout-ms 10000
```

配置 `daemon.startup: on_demand` 时，`aw connect --sdk-pid` 会按需启动或复用 AW，等待该进程发布
SDK 监听并确认 Ready，不启动 Agent，也不会向任意 PID 注入。

编译 Descriptor 时须使用与 SDK 匹配的 AW 构建及应用和 Service 共同使用的精确配置文件。发布前检查命令退出状态，
先生成到暂存位置，成功后将 JSON 与 `aw.yaml` 一同安装再启动应用。
若 `claude-resident-hooks` Descriptor 含非空的 `allow_plaintext_http_hosts` 列表，
安装时须清除该 Descriptor 文件的所有写权限位，例如使用 `0400`。

| 目标框架 | `--profile` | SDK 入口 |
| --- | --- | --- |
| Qoder SDK | `qoder-public-hooks` | `createQoderBridge(..., descriptorFile)`（`@anolisa/aw-sdk-qoder`） |
| DeepSeek Harness | `deepseek-cordis-hooks` | `createDeepSeekBridge({ configFile, descriptorFile })` |
| Pi SDK | `pi-worker-hooks` | `runPiWorker({ ..., descriptorFile })`（`@anolisa/aw-sdk-pi`） |
| QwenPaw | `qwenpaw-installed-native-hooks` | `QwenPawPlugin.from_config(..., descriptor_file=...)`（`anolisa-aw-sdk-qwenpaw`） |
| Claude SDK | `claude-resident-hooks` | `ResidentClaude.from_config(..., descriptor_file=...)`（`anolisa-aw-sdk-claude`） |

Profile 版本是 Hook 合同标识，不随原生包版本变化。
编译复用现有能力准入，记录目标、Profile、明确的 socket、输入边界与可用性；
不启动 Service、Provider 或原生应用。QwenPaw Profile 要求服务可用，Qoder 记录
目标适用事件的保护要求。两条加载路径均保留 Qoder 调用方选择的可用性，默认仍为
`native_fallback`。Descriptor 要求保护时须显式传入 `availability: "required"`；
冲突选择会在安装监听前失败。

SDK 加载时验证 schema 版本、准确的 Profile 和可选显式目标，再计算配置原始字节的摘要。
两个输入均须为应用用户持有、不超过 4 MiB 的普通文件，硬链接数为 1，无特殊模式位，
且 group／world 无写权限。路径须为绝对路径，禁止符号链接和 `..`；直接父目录也须由
应用用户持有且 group／world 不可写。Descriptor 缺失、格式错误、不匹配或过期均使
初始化失败；显式指定 Descriptor 后，失败不会转为 `config show` 或原生执行。
配置仅修改空白也需要重新生成。此加载路径不调用 AW 可执行文件、不解析 YAML。

Descriptor 编译器与两个 SDK 读取器均支持 ext2/3/4、XFS、Btrfs、tmpfs 和 OverlayFS，并检查已打开
文件及固定父目录的文件系统类型。其他文件系统会被拒绝，部署方可显式选择既有 CLI
加载 API。Node 接受有效写权限 mask 通过 mode 检查的只读 POSIX ACL；Python 和 CLI
还会拒绝文件扩展 ACL 及父目录的原生 ACL。部署无扩展 ACL 的普通文件可使两语言
行为一致。

Descriptor、配置及其父目录须由可信部署方控制。摘要用于发现配置不匹配，不验证发布者
身份，也无法阻止有写权限者改变接线提示。Service 继续负责权威策略准入和执行。
接线变化时先排空应用，部署匹配文件对并重启。恢复离线 CLI 加载时，省略
`descriptorFile`／`descriptor_file`，确保对应 AW 可执行文件可用后重启。

## 通用应用接入

在 `aw.yaml` 中配置名为 `app`、`adapter: sdk` 的目标并保留应用普通 `argv`（应用自行启动，`aw run` 不启动 `adapter: sdk`）：

```yaml
agents:
  app:
    adapter: sdk
    argv: [/usr/bin/env, python3, /absolute/path/to/agent.py]
```

独立启动 AW Service，并将服务报告的私有 socket 设为 `AW_SOCKET`：

```bash
aw service serve --config /absolute/aw.yaml
```

对任意自研 Node.js/TypeScript 或 Python Agent，只需安装核心 SDK 包，即可用 `LanguageSource`
（`@anolisa/aw-sdk/sources` 或 `aw_sdk.sources`）完成输入与工具方法插桩，无需编写任何 Rust Adapter：

**Node.js / TypeScript（`@anolisa/aw-sdk/sources`）**：

```javascript
import { LanguageSource, SourceDenied } from "@anolisa/aw-sdk/sources";

class CustomAgent {
  async ask(prompt) { return this.echo({ text: prompt }); }
  async echo({ text }) { return `echo:${text}`; }
}

const agent = new CustomAgent();
const source = LanguageSource.create({
  endpoint: process.env.AW_SOCKET,
  target: "app",
  availability: "required",
});

try {
  await source.connect();
  await source.withSession({ sessionId: "example-js-agent" }, async (session) => {
    session.instrument(agent, {
      inputMethods: ["ask"],
      toolMethods: { echo: "echo" },
    });
    try {
      console.log(await agent.ask("hello"));
    } catch (error) {
      if (error instanceof SourceDenied) console.log(`AW denied ${error.event}`);
      else throw error;
    }
  });
} finally {
  await source.close();
}
```

**Python（`aw_sdk.sources`）**：

```python
import asyncio
import os
from aw_sdk.sources import LanguageSource, SourceDenied


class CustomAgent:
    async def ask(self, prompt: str) -> str:
        return await self.echo(text=prompt)

    async def echo(self, *, text: str) -> str:
        return f"echo:{text}"


async def main() -> None:
    agent = CustomAgent()
    async with LanguageSource.create(
        endpoint=os.environ["AW_SOCKET"],
        target="app",
        availability="required",
    ) as source:
        async with source.session(session_id="example-python-agent") as session:
            session.instrument(
                agent,
                input_methods=["ask"],
                tool_methods={"echo": "echo"},
            )
            try:
                print(await agent.ask("hello"))
            except SourceDenied as denied:
                print(f"AW denied {denied.event}")


asyncio.run(main())
```

TypeScript 与异步 Python SDK 共用注册、Ready、Task/Scope、Provider 执行及审计合同；
语言级 Source Runtime（`@anolisa/aw-sdk/sources` 与 `aw_sdk.sources`）的行为约束如下：

- 双语言均通过 `LanguageSource.create(...)` 创建带有标准 `aw-hook-source/v1` 能力声明与
  路由校验的 `Instance`（支持 `required` 与 `native_fallback`）。
- 每次请求开启一个独立会话（TypeScript 使用 `source.withSession(...)`／`source.session(...)`，
  Python 使用 `async with source.session(...)`），持有单一 `Admission` 与不可变 `Task`；
  缩小会话事件集合无法跳过已准入的 Task 路由（触发 `source_route_coverage_incomplete`）。
- TypeScript 提供 `session.wrapInput`、`session.wrapTool`、`session.wrapInputStream`／
  `session.wrapToolStream` 与 `session.instrument`；Python 提供 `session.wrap_input`、
  `session.wrap_tool` 与 `session.instrument`。`input.submit` 与 `tool.before` 阻断时在
  原生执行前抛出 `SourceDenied`。输入和工具包装器在准入前深拷贝普通 JSON 参数，
  提取函数与原生执行共用这份副本，包括流的首次读取。
  对于类实例等业务参数，可提供 TypeScript 的 `snapshotArg` 或 Python 的 `snapshot_arg`。
  回调应返回业务函数所需类型的独立对象，并复制函数执行和流迭代期间会读取的可变输入。
  例如，只有文本字段的请求可以使用 `snapshotArg: req => new RequestData(req.text)`。
  参数需要独立副本时，缺少回调或返回原对象都会触发 `source_input_snapshot_required`。
  插桩时，在 `toolMethods` 或 `tool_methods` 的对应方法选项中设置回调。Python 也支持在
  `instrument` 上设置 `snapshot_arg` 作为各方法的默认值。自定义工具输入提取函数应读取
  副本，并包含策略需要检查的全部字段。SDK 会在派发前再次核对这些字段。
- 在 TypeScript 中直接迭代生成器或工厂的结果，使用 `wrapInputStream`、`wrapToolStream`
  或 `{ stream: true }`。使用自动识别时，原生异步生成器函数的包装器返回迭代器，普通工厂的
  包装器返回 Promise。两者可以有相同的 TypeScript 函数签名，调用方可先用
  `await wrapped(...args)` 取得结果。工厂需要 SDK 管理流清理或收集工具结果时，使用显式流包装。
- Task 准入了 `tool.after` 路由时，SDK 最多收集 `maxStreamItems`／`max_stream_items`
  项流式输出；提供 `extractResult`／`extract_result` 时接收收集到的原生分块（普通 JSON
  分块在产出前深拷贝隔离）。仅有前置路由或进入原生回退的 Task 直接传递工具结果和流元素。
  结果提取、规范化或收集失败时，异常携带 `toolExecuted`／`tool_executed`。
- 被包装的同步函数运行在事件循环线程上；阻塞型 I/O 或耗时计算应使用 `async` 函数或卸载到
  后台执行器（例如 Python 的 `asyncio.to_thread`），以免阻塞取消、其他会话和关闭超时。
- 会话关闭时先停止活动流并等待在途的 `submit`／`beforeTool`／`before_tool` 检查、
  已准入的原生调用及其 `tool.after` 观察完成，随后才恢复插桩方法并释放 Task。
  Python 会取消正在进行的读取。
  Node 应用将 `session.signal` 传给可能阻塞的原生读取。如需支持调用方取消，用
  `AbortSignal.any` 合并会话和调用方信号，将同一信号传给原生读取并由 `extractSignal` 返回。
  SDK 保留直接作为参数传入的 `AbortSignal` 引用。
  等待工作完成超过关闭超时（默认取会话超时，也可在单次关闭传入 `{ timeoutMs }`／`timeout_ms`）后，
  关闭调用报告 `source_close_pending`，期间继续保留插桩方法并持有 Task。
  业务解除原生工作阻塞后，可再次等待关闭完成。Source 持有 Instance 时，再次调用 `source.close()`。
  取消后，会话关闭会在释放 Task 后抛出原生清理错误，包括正在进行的读取或流工厂抛出的错误。
  `source.close()` 开始后，所有已有会话会并发进入关闭并拒收新调用，新建会话会收到 `source_closed`。
  若首次关闭超时，重试时仍会收到两次调用之间发生的原生清理错误。
- 同一对象实例的同一方法同一时刻只能由一个活动 `SourceSession` 插桩（重复插桩报
  `source_already_instrumented`）；并发处理多会话时请按请求使用独立对象实例或按会话直接包装函数。

### 输入／结果改写与故障扣留（`replace_input`、`replace_result`、`withhold_result`）

按 `LanguageSource` 实例显式开启，保持既有 observe/block 调用方完全兼容：

- 在 TypeScript 的 `LanguageSource.create(...)` 传入 `replaceInput: true`、
  `replaceResult: true` 和／或 `withholdResult: true`（Python 为 `replace_input=True`、
  `replace_result=True`、`withhold_result=True`）；路由确认会拒绝任何未声明对应效果或
  `withhold_result` 故障动作的策略步骤。
- **`replace_input`**：当 `tool.before` Provider 返回获准修改后的参数时，`wrapTool`／
  `wrap_tool` 安全重建普通 JSON dict/object 参数（或调用可选 `applyInput`／`apply_input`
  处理自定义参数形态），并在原生执行前复核投影输入；无法安全重建的形态报
  `source_input_replace_unsupported`，绝不静默丢弃改写。
- **`replace_result`、`withhold_result` 与严格输出隔离**：`tool.after` 返回替换文本时，
  `wrapTool`／`wrap_tool` 替换字符串或普通结构化文本封装（或调用可选 `applyResult`／
  `apply_result`），不支持的形态报 `source_result_replace_unsupported`。
  在严格后置路由（`replace_result` 或 `on_error: withhold_result`）下：
  - 未配置 `extractResult`／`extract_result` 时，SDK 仅接受严格普通 JSON 值（TypeScript
    要求稠密普通数组且不含额外命名／不可枚举／Symbol／访问器属性，普通对象仅含可枚举字符串数据属性；
    非流式 TypeScript 工具直接返回顶层 `undefined` 时为兼容无返回值函数会原样返回 `undefined`，
    其送审 `tool.after` 投影为 `null`，而对象／数组内部或流分块中的 `undefined` 会被拒绝），
    向调用方交付**与审核记录完全一致的独立 JSON 副本（exact audited JSON value）**；
    不透明自定义类型、稀疏数组、带额外属性的数组以及未缓冲惰性迭代器／生成器均失败关闭
    （`source_result_isolation_required` 或 `source_stream_withhold_requires_buffering`）。
  - 配置 `extractResult`／`extract_result` 时，提取器在独立副本上仅求值一次，AW 审核其返回的规范化 JSON 投影。
    非流式严格路由对不透明非 JSON 对象默认拒绝，仅当调用方同时提供 `snapshotResult`／
    `snapshot_result`（隔离原生对象副本）时放行；此时 AW 审核的是提取投影而非不透明原生字段。
  - `on_error: withhold_result` 下必需 `tool.after` 检查故障时抛出 `SourceResultWithheld`
    （`toolExecuted`／`tool_executed = True`），绝不返回未审核原文；`on_error: report` 下，
    `tool.after` 的已审计可恢复 Provider 故障会记录日志并放行原结果，Scope／RPC／取消／审计等故障仍失败关闭。
- **流式诚实边界与统一字节预算**：未缓冲的流式工具无法撤回已产出分块。当 `tool.after`
  准入 `replace_result` 或 `on_error: withhold_result` 时，未缓冲流（异步生成器或同步生成器／迭代器）
  在启动原生生成器前即报 `source_stream_withhold_requires_buffering`。设置 `bufferStream: true`／
  `buffer_stream=True` 可将隔离后的严格普通 JSON 分块快照缓冲至 `maxStreamItems`／
  `max_stream_items` 与 `maxStreamBytes`／`max_stream_bytes`（默认 `1048576` 字节）双上限。
  该字节预算仅适用于流缓冲（普通非流式结果无此流预算），按紧凑 UTF-8 JSON 计量（TypeScript 为
  `JSON.stringify` UTF-8 字节，Python 为 `separators=(",", ":")` 且 `ensure_ascii=False`；传输层
  采用 ASCII 转义的 JSON-RPC 帧可能略长于紧凑计量值）。严格流缓冲拒绝无法准确计量的不透明自定义类型
  （即使已配置 `extractResult`／`extract_result` 或 `snapshotResult`／`snapshot_result`，仍报
  `source_result_isolation_required`），并将 `maxStreamBytes`／`max_stream_bytes` 统一应用于
  **缓冲分块快照数组（含空流 `[]` 占 2 字节）**与**单次提取求值后实际送审 `tool.after` 载荷**的紧凑
  UTF-8 JSON 字节长度。超限或扣留时立即关闭底层迭代器，并保证零分块产出。

完整示例见 [TypeScript Source 示例](../../../../../src/aw/sdk/typescript/examples/language-source.mjs)
与 [Python Source 示例](../../../../../src/aw/sdk/python/examples/language_source.py)。

如需直接调用底层语义回调，可配置名为 `app` 的目标并保留应用普通 `argv`，安装核心候选包后设置
`AW_SOCKET`，运行
[TypeScript 示例](../../../../../src/aw/sdk/typescript/examples/application.mjs)或
[Python 示例](../../../../../src/aw/sdk/python/examples/application.py)。示例在工具执行前
调用 `task.emit`，采用阻断决定，执行后提交结果观察并释放 Task；具体决定来自配置策略。
`install(routes)` 确认所有路由已安装后才进入 Ready。应用保留原生权限检查，等任务
执行结束后再释放 Scope。

通用入口支持输入提交、工具调用前和工具调用后，字段与效果见
[通用语义入口](../../../../../src/aw/docs/design/zh/runtime/native-sdk.md#通用语义入口)。
具有这些语义的应用只需核心 SDK 即可复用，无需新增框架专属 Rust Adapter。原生 Hook 或语言级
插桩来源可向同一入口供给事件；自动发现与覆盖选择属于后续交付。
Qoder 的丰富原生效果继续安装下述可选适配包，Python/QwenPaw 见 [Python SDK](python-sdk.md)。

生命周期 RPC 默认超时 1000 ms；回调 RPC 至少容纳准入事件预算加 1000 ms 传输余量。
Service 在有序步骤间持续使用同一事件截止时间。

## 安装与连接

在应用目录安装核心候选 tarball，并按需仅安装实际使用的可选框架适配包：

```bash
npm install /absolute/artifacts/anolisa-aw-sdk-0.2.2.tgz
# 可选框架适配包（按需安装）：
npm install /absolute/artifacts/anolisa-aw-sdk-qoder-0.2.2.tgz
npm install /absolute/artifacts/anolisa-aw-sdk-pi-0.2.2.tgz
```

使用应用用户持有的私有目录中的绝对 socket 路径。同用户运行 AW，完整配置中设置
`agents.qoder.adapter: qoder-sdk`。选择 `native_fallback` 时，该目标适用的已启用事件均需
设置 `required: false`；Provider 的明确拒绝继续生效。独立执行
`aw service serve --config /absolute/aw.yaml`，使用该服务报告的 socket。
SDK 不启动服务或 Agent。

已安装的 `@anolisa/aw-sdk-qoder` 包内包含 `examples/qoder.mjs` 配置式接入示例。显式管理生命周期时，沿用业务原有的
认证、daemon、模型、权限、配置来源和 Hook。将现有选项传给
`qoderOptions(task, options)`，创建原生 Query 后立即通过返回对象执行
`bind(query)`。每个 Query 使用独立 task，先关闭 Query，再关闭 task；应用退出时
关闭 Instance。同一 Query 只使用一种 AW 接线方式，避免与原 AW Query factory 重复。

创建 `Instance` 时显式传入 endpoint、target 和 availability。`@anolisa/aw-sdk-qoder` 导出
`qoderProfile` 用于加载 Descriptor，导出 `qoderCapabilities(inputEnabled, sessionStartEnabled)` 用于登记，并提供
`qoderInstallRoutes(routes, inputEnabled, sessionStartEnabled)` 供 `install(routes)` 校验协商后的路由。使用 WarmQuery 时，每个预热
条目创建独立的 `Admission`，启动前将它与原生选项传给 `qoderOptions`，消费条目时绑定实际 Query。
安装回调和登记能力使用相同的启动事件开关。`install(routes)` 在登记和每个任务获取策略快照时调用，只确认
应用已接入的路由。`cwd`、`environment` 默认采用应用值。Query 使用不同目录或环境时，在 Task 或
Admission 选项中传入实际目录和复制后的环境。

`await instance.connect()` 执行一次有界连接尝试。只有 native-fallback 下的传输不可用
或登记尚未完成时返回 `false`。`instance.start()` 启用单一后台重连流程，通过
`state`、`lastError` 查看就绪状态或明确错误。`stop()` 停止重连；`close()` 进一步
释放本应用的 Scope 和登记。这些调用保留共享 Service 与原生 daemon 的生命周期。

Ready 记录应用对路由安装的确认；后续每个 Query 是否实际安装 Hooks、是否采用返回效果，
仍由应用接线和原生验收证明。

## 可用性与任务归属

| 模式 | 服务缺席或连接丢失 | 明确错误或拒绝 |
| --- | --- | --- |
| `required` | 操作失败 | 保留错误或拒绝 |
| `native_fallback` | 有界检测后返回中性 AW 回调 | 保留错误或拒绝 |

生命周期 RPC 默认超时 1000 ms；回调等待时限覆盖准入事件预算与传输余量。
首次传输故障使当前连接退役。`native_fallback` 的活动任务固定为回退，后续回调立即返回；
`required` 的活动任务保持失败，恢复前拒绝新受保护任务。两种模式的后台重连均继续重试
传输不可用；明确的协议或配置错误停止重连。恢复时重新登记并确认 Ready，仅新任务使用恢复后的登记。不重放回调、工具或结果未知的
Provider 调用。已经采用的输入／结果改写继续保留。

可向 `instance.task({signal})` 或 `task.invoke` 传入 `AbortSignal`，用户取消继续传播。
协议不匹配、输入无效、配置拒绝、远端期限错误与 Provider 失败继续报错。
Qoder before Hook 失败拒绝工具，post Hook 的 Provider 失败停止 Query，并保存在
`integration.failure`。传输回退保持 Query 运行。消费结束后检查 `integration.failure`，
向业务暴露策略执行错误。

Service 仍可达时，回调 RPC 失败或取消会先清理该事件，再传播错误。原生代码在有序步骤之间提前结束时，
调用 `await task.closeEvent(hook, originalPayload)`。清理使用原 Scope 和独立的有界请求，
即使调用方的 signal 已取消也会执行。关闭成功后保留 Task，并拒绝该事件的迟到步骤；
正常完成的事件继续由 Service 自动退休。

## 协作式 attach 与 detach

应用调用 `instance.listen()` 发布已安装的 SDK 接线，可先于 AW 启动。配置的 Service
可用后，列表显示该应用；收到 attach 前保持未接入。使用
`instance.listen({automatic: true})` 同时启用自动登记与 CLI 控制。
后台流程在 `start()` 和 `listen()` 中选择一种；重新监听前等待 `await instance.stop()` 完成。两种 attach 方式复用同一登记和 Ready
路径；应用仍可在启动监听前调用 `connect()` 显式完成初始化。

使用应用的同一用户执行以下命令，将第一条命令返回的准确 `application_id` 填入变量：

```bash
AW_SOCKET=/absolute/private/aw.sock
aw instance list --socket "$AW_SOCKET"
AW_SDK_INSTANCE=application_id_from_list
aw connect --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
aw disconnect --socket "$AW_SOCKET" --sdk-instance "$AW_SDK_INSTANCE"
```

应用确认 Ready 或释放空闲登记后，结果才显示 `completed`。存在活动任务时 detach 返回
`sdk_control_busy`，任务结束后再操作。选定实例脱离后，在 Service 恢复时仍保持未接入，
直到下一次 attach。既有回退任务继续原生运行，新任务使用恢复后的登记。detach 保留原生
应用与 Service；`await instance.stop()` 撤下发现记录并停止轮询，`close()` 还会释放本实例的任务与登记。

同一 PID 内的实例具有不同 ID。Service 重启后发现 ID 改变，操作前重新 list。
未运行 SDK 监听的应用无法 attach。私有 socket 仅接受 Service 同用户连接，应用确认还须
通过 PID／启动时间身份核验。CLI 不能改变应用选择的可用性模式或远程安装 Hook。

`--timeout-ms` 默认 5000，范围为 100–30000。超时或 CLI 中断代表确认结果未知，后续操作前
先通过 `instance list` 检查。Service 在期限后使保留的请求过期，迟到 Ready 无法激活该请求。
列表提供最近请求结果和已接入登记；普通 `aw service status` 也展示 SDK Ready 与 owner。

## 已验证范围

Qoder 接线使用公开 `UserPromptSubmit`、`PreToolUse`、`PostToolUse` 和
`PostToolUseFailure` 回调。AW 保留原 Hook 分组并追加自己的回调。Worker 可以并发调度分组，
原生观察 Hook 仍可能收到原始结果。其他 Hook 也返回替换时，需验证组合后的实际输出。
AW 检查收到的回调快照，后续原生改写超出这个接点的覆盖范围。
SDK 1.0.56／Worker 1.1.78 验收覆盖真实 Bash、原生 Hook 共存、Query 中途 AW 退出、
新任务恢复接入、工具副作用恰好一次，以及 AW／原生拒绝时无工具副作用。同版 SDK 和 Worker 的
WarmQuery 在工具事件和后续模型轮次采用了 AW 替换后的 Bash 结果，已有原生 Post Hook 仍收到
原始结果。包顶层不导入 Qoder，不读取私有 binding 文件。
其他框架版本按声明能力协商，原生验收结论对应实际测试版本。

原 resident `aw run` 路径保留更完整的托管 MCP、凭据与固定 Worker 接点。
当前公开 Hook SDK 尚未提供这些附加托管能力。原生 MCP 配置继续由应用选项传入。
Skill 检查沿用 Adapter resolver，并要求对应原生 Query 接口。
输入改写与 Skill／MCP 组合需要单独完成原生验收后再进入业务采用。

归属与 wire 合同见 [SDK 协议](../../../../../src/aw/docs/design/zh/runtime/native-sdk.md)；
现有 required 入口见 [Qoder 指南](../agents/qoder-sdk.md)。

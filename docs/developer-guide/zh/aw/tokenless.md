# Tokenless Provider 接入参考

[English](../../en/aw/tokenless.md)

本页用于组件接入与维护。安装和日常使用请阅读[用户指南](../../../user-guide/zh/aw/tokenless.md)。

可选的 Tokenless Provider 将已完成工具的模型可见文本替换为 Tokenless 按配置的可逆性
策略选中的压缩结果。AW 检查替换边界，不独立证明语义无损。
Qoder、OpenClaw、QwenPaw、Hermes、Pi、AgentScope SDK 2.0.6 及 Claude SDK 原生 Bash 共用一份 Provider 配置，支持 PreTool 重试建议
与 PostTool 压缩。Schema 压缩和授权恢复仍是独立能力。

## 安装与启用

使用 [AW Preview 包](../../../user-guide/zh/aw/installation.md)。`aw-provider-tokenless` 包含从同一源码提交
构建的桥接程序和 Tokenless CLI，以及固定到
`b1c0dc00649c50fbe8930f849c800d4d6ca12091` 的 RTK v0.49.0，附带 Tokenless 的
统计和 pytest 补丁。要求匹配的 `aw-core`；`aw-all-in-one` 将完全相同的组件内容
和回执与 core、sec-core 组合。无需 Python wheel 或 Tokenless 原生插件；Agent
安装及其运行依赖仍由用户准备。

RTK 位于不可变前缀内的 `libexec/aw/providers/tokenless/rtk`。源码 pin、setup
脚本、两份补丁、共享的 Tokenless home/path-policy 源文件与 Apache-2.0 许可位于 `share/doc/aw-provider-tokenless/rtk`，
它们和二进制的哈希均纳入 Tokenless 组件回执。`provenance.json` 记录上游与
ANOLISA 源码提交、各输入的来源和哈希，以及 RTK 复用 home/path-policy 模块的原因。
源码构建通过官方 setup 脚本创建本次
专用源码目录，保留已有 RTK 源码缓存与本地修改，打包后清理临时源码；
不替换用户 RTK 或全局 PATH。受管理 RTK 的安装前缀必须可编码到 PATH，不能包含
`:`；配置在发布前明确拒绝这种路径。
PostTool 不依赖 RTK。包生成的 PreTool 先校验受管理 RTK 及其直接父目录：两者必须
属于当前用户或 root，且不能允许所有用户写入；父目录必须是真实目录，不能是符号链接。
仅组可写不会被拒绝，与 Tokenless 的依赖信任规则一致。仅在 Tokenless 子进程的
受控 PATH 前加入其目录；重试建议使用绝对执行路径。受管理 RTK 缺失、被修改、
不可执行或不可用时，返回 Provider 错误并沿用已有 `on_error` 策略，不回退用户 RTK。
PostTool 和只读 view 继续工作。现有 view/schema 及其 unknown 就绪字段保持不变。

先安装 core，再把匹配的 Tokenless 扩展安装到同一前缀；也可以将 all-in-one 安装到新
前缀。Qoder/OpenClaw 使用 `aw config init --provider tokenless`，并提供既有
Agent 参数。选择 `--provider sec-core+tokenless --socket ABS_FILE`，可生成执行前
sec-core 检查，追加 PreTool，保留完成观察，并在既有 after 步骤末尾追加压缩。安装组件不会自动启用。
原有单独 `sec-core` 选项保持既有行为。

受支持的 target 接入时复制[共用示例](https://github.com/agentic-os-org/ANOLISA/blob/main/providers/tokenless/examples/aw.yaml)，
替换可执行路径，并将 `RTK_SHA256_FROM_RECEIPT` 替换为
`PREFIX/.aw-packages/aw-provider-tokenless.json` 中 RTK 条目的 `sha256`，
将两个 state 目录都放到不可变安装前缀之外。原生 profile 和各框架
启动参数见 [AW 指南](native-agents.md#四框架使用同一份配置)。

AgentScope SDK 的每个公共工厂都需通过 `AwBridge` 接线；
[SDK 生命周期示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.agentscope-lifecycle.yaml)
组合 sec-core 检查/观察与两项 Tokenless 操作。Python 和工厂设置见
[SDK 启动合同](native-agents.md#启动-agentscope-sdk)。

Pi 使用 [Pi 启动指南](native-agents.md#启动-pi-sdk-worker)中的 SDK 应用工厂；
包配置生成器仍只面向 Qoder/OpenClaw。

Provider 私有配置只接受 `{version: 1}`。固定 argv 为
`aw-provider-tokenless --cli ABS_FILE --data-dir ABS_DIR`，可追加
`--rtk ABS_FILE --rtk-sha256 HEX`。包配置生成器从已校验的安装内容生成这两个参数；
不含它们的手写配置沿用 Tokenless 原生依赖发现。view 命令不包含这两个参数。
桥接通过 Protocol v2 调用 `tokenless compress`，为该子进程设置 `TOKENLESS_DATA_DIR`。CLI 统计使用 AW
提供的 Agent、Session 和工具调用标识。CLI 统计度量选中的压缩，不证明原生采用。

AW Journal 与 Tokenless 统计具有独立的内容留存边界。Tokenless CLI 默认开启 SQLite
统计和 SLS 日志，可由配置或继承的环境覆盖。产生压缩收益时，SQLite 统计可能将完整的
压缩前后工具文本写入 `$TOKENLESS_DATA_DIR/stats.db`，即使恢复能力为 `none`。
SLS sink 将指标和归属信息写入本地 JSONL，默认路径为
`/var/log/anolisa/sls/ops/tokenless.jsonl`；当前实现不将工具正文写入 SLS，也不执行上传。
若宿主运行 anolisa telemetry uploader 或 SLS/Logtail 采集，指标和归属标识可能由这些
组件另行上传；该通道仍不含工具正文。
如需关闭两个 sink，在启动 AW、绑定 Agent 之前设置 `TOKENLESS_STATS_ENABLED=0`
和 `TOKENLESS_SLS_ENABLED=0`。这两个开关不会删除已有记录。

## PreTool 与权限行为

生成模板和共用示例包含 `tool.before` → `pre_tool`，设置为 `effects: [block]` 和
`on_error: report`。受支持的 Shell 工具为 `Bash`、`exec`、`terminal`、`shell`、
`run_shell_command` 和 `execute_shell_command`（不区分大小写），且要求非空字符串
`command`。其他输入不调用 CLI，直接保持中立。
由 Tokenless 判断是否建议 RTK 命令；AW 阻断本次调用并向模型展示完整的重试参数，
不静默改写输入，不批准或执行命令。新调用必须重新经过 sec-core 与原生权限检查。
即使 PreTool 中立，扫描拒绝或失败仍然生效；外部 Hook 调度保持既有行为。

Tokenless 接入层从实际执行参数中识别独立的归属 RTK 包装，并将
`output_optimization: rtk` 传入 PostTool。重试可以使用不同于原阻断调用的 ID。
带引号或转义的参数字面量，以及符号链接状态目录，均保留该归属。支持的执行前缀为
字面环境变量赋值、`env NAME=value ...`、不带选项的 `sudo`、`noglob`、`command`、
`exec`，以及 `shadowenv exec --`；这些形式可以组合。其他包装形式（包括额外选项）、
裸 RTK 命令、外部归属或复合 Shell 命令不推断 RTK 归属。缺失 RTK、CLI 故障、非法
响应或超时按 `on_error: report` 报告，不会关闭 sec-core 的 `on_error: block`，也不
授予原生批准。仅需 PostTool 时，只删除 before 中的 `rewrite-command` 步骤，保留
共用 Provider 和 after 链。

本次恢复能力仍为 `none`。恢复入口、Stash、恢复作用域及恢复输出旁路由 Tokenless
接入层负责；启用 PreTool 不会自动获得这些能力或 Schema 压缩。

## 结果与故障行为

| 框架 | 模型可见结果替换入口 |
| --- | --- |
| Qoder | `PostToolUse` → `hookSpecificOutput.updatedToolOutput` 字符串 |
| OpenClaw | 下一次模型请求前的 `registerAgentToolResultMiddleware` |
| QwenPaw | Acting middleware 返回的最终 AgentScope `ToolResponse` |
| Hermes | `transform_tool_result` 最终字符串回调 |
| Pi SDK | 原 callback 之后的 Core `afterToolCall` content override |
| Claude SDK | `PostToolUse` → `hookSpecificOutput.updatedToolOutput` 原生 Bash envelope |
| AgentScope SDK 2.0.6 | 公共工厂 Bridge 返回最终 `ToolResponse` |

OpenClaw 要求 middleware API 和显式插件启用；AW 生成的原生 profile 声明 middleware
合同。观察型 `after_tool_call` 的返回值无法替换模型可见内容。

仅投影受支持的单个文本槽。Media、多个 Content Block、流式中间块以及未成功的工具
结果保持原样。替换保留原生 metadata 和 envelope 字段。Qoder 要求字符串替换，因此结构化
envelope 会序列化到该字符串中。
AW 通过启发式规则推断 `content_origin`。提取到命令时标为 `command_output`；
没有命令时，按工具名将已知 Shell 工具标为 `command_output`，
已知文件读取工具标为 `file_content`，其余标为 `api_response`。

Tokenless 会将 `cat page.html` 这类纯文件打印细化为 `file_read`，并用于统计。
`file_read` 保留适用的输出压缩并保护 HTML 源码；`file_content` 跳过压缩。

OpenClaw 的 after 事件 `tool_name` 统计和 AW Journal 元数据使用小写策略名
（例如 `Bash` 变为 `exec`）；before 事件保留原名用于 sec-core 查找。跨阶段关联应使用
Session/工具调用 ID，而不是工具名。

恢复能力显式为 `none`，因此 Tokenless 只能采用无需授权恢复入口的允许候选。不追加
压缩副本或上下文消息。`dry_run`、`no_savings` 和不可用恢复均保留原结果。CLI 失败、
非法响应、身份不匹配、超时和输出超限属于 AW 故障，由 `on_error: report` 处理，原生
结果保留。AW 持久 Journal 仍仅记录元数据，不保存替换文本。

在 `tool.after.steps` 中保留既有完成观察和用户自定义 Hook，将压缩放在最后。
启用结果替换时，Qoder、QwenPaw、Hermes、Pi 和 AgentScope SDK 按声明顺序执行这些步骤，观察步骤看到原结果。
只有最后一个结构化步骤可以请求 `replace_result`；替换后还有启用步骤，或有第二个
替换步骤时，准入会拒绝。观察 Provider 保持原输入形态，只有替换 Provider 收到文本
投影。所有步骤共享一个事件截止时间；观察或用户 Hook 故障按 `report` 处理，预算
未耗尽时继续执行后续步骤。

Qoder 保留独立的原生回调，仅在 AW 服务内部约束自身 after 调用的顺序，不强制设置
原生 `sequential`，不改变外部 Hook 的调度。用户显式配置该选项时，仍遵循 Qoder
对整个匹配计划生效的原生语义。QwenPaw 和 AgentScope SDK 反向注册 after 链，使 middleware 解包时
保持配置顺序；Hermes 保留完成观察，并在原生最终结果转换阶段压缩。

OpenClaw 的原生完成事件发生在结果 middleware 之后，因此先压缩，再在原生 after
点位按配置顺序执行完成观察。观察保留原生 `error`、`durationMs` 等完成字段，结果
使用 OpenClaw 对压缩前内容的脱敏视图。两个阶段共享 AW 截止时间和单次执行声明。
同一插件实例的所有会话合计最多保留 32 个待观察的原结果快照，完成或会话结束时清理。
包含两个原生视图的序列化回调帧必须小于等于 1 MiB，因此可用工具文本大小更小，具体
取决于 JSON 转义和元数据。达到快照数量或帧大小限制时跳过压缩。
middleware API 缺失时警告并停用压缩，保留 before 阻断和原生完成观察。
启动回执报告 `degraded: "result_middleware_unavailable"`，AW 输出
`native hooks ready (degraded: result replacement unavailable)`，不因此导致启动失败。
回执仍统计配置步骤数，不表示缺失的替换 handler 已注册。AW 步骤列表之外的 Hook 仍由框架调度。
这些会话应禁用重叠的 Tokenless 原生插件，避免重复压缩和统计。

Provider 消息沿用 AW 的 1 MiB 限制，包括原事件和投影文本。若仅投影导致超限，AW 省略投影，
保留原事件的审计；压缩降级为不替换。CLI 调用共享剩余 AW
预算，并限制 stdout/stderr。Host 限制整个 Provider 进程；直接调用方必须关闭 stdin
并自行限制生命周期。

禁用 PostTool 时删除压缩步骤；仅在没有 PreTool 步骤继续引用时删除 Provider。
停止 AW 后用新配置重新启动。已有会话不会
热重载。外部 Tokenless state 目录按数据保留需求保留或删除。

Claude SDK 使用 [AW 指南](native-agents.md#启动-claude-sdk-worker) 中的显式 worker 工厂。单个 AW callback 已按顺序等待步骤。仅原生 Bash stdout 可替换，通过 `PostToolUse` 的 `hookSpecificOutput.updatedToolOutput` 返回原生 envelope；MCP 状态不可靠，结果保持原样。配置 AW replacement 时拒绝既有原生 callback 的输出改写。

## Panel 只读查询

桌面接入只读取一个快照，不解析私有配置，也不直接调用通用服务 RPC：

```bash
aw inspect --config /absolute/path/to/aw.yaml --json
```

使用匹配的 AW 二进制。新生成的 Tokenless 配置自动登记可选 view；已有配置需先退出
Agent、停止原服务，再离线编辑。在 Tokenless Provider 既有 `transport` 同级添加，
不要放进私有 `config`：

```yaml
view:
  protocol: aw-provider-view/v1alpha1
  implementation: tokenless
  transport:
    type: stdio
    location: agent
    argv:
      - /absolute/path/to/aw-provider-tokenless
      - --view
      - --cli
      - /absolute/path/to/tokenless
      - --data-dir
      - /absolute/path/to/tokenless-state
```

CLI、数据目录和用户上下文应与压缩一致。当前只查询登记目录中的 `stats.db`；
非空 `TOKENLESS_STATS_DB` 或有效 DSH 别名覆盖返回 unsupported。AW 不直接读组件数据库。
缺库返回 no_data，不初始化空库。已有库通过公共 CLI 查询，可能发生迁移或维护，
不能理解为文件系统完全无写入。

快照版本为 `aw-panel/v1alpha1`，时间字段 `generated_at_ms` / `observed_at_ms`
使用 Unix 毫秒。内容包括服务状态、Provider 卡片和本次服务启动后最近最多 20 条完成步骤
元数据，新记录在前。服务重启清空该展示缓存，持久审计保持不变。离线查询不启动服务，
也不准备事件 Provider。`health.summary` 只确认配置；准入、原生就绪、后台存活状态
保持 unknown，采用状态保持 unverified。未登记 view 的 Provider 仍正常运行，指标显示 unsupported。

Tokenless 指标复用 `stats summary --json --limit 200`（schema `1.1`）。窗口为组件
存储中筛除 dry-run 前的最近 200 条记录，可能含其他接入的数据。展示时标注该范围内的
选中候选估算收益，不称为今日、当前 Agent、计费或净收益。录制开关保持 unknown；
关闭录制后仍可能存在历史数据。快照不含工具正文、凭据、私有 argv、环境或配置；
组件原有留存策略仍适用。

view 失败只影响对应卡片。每 Provider 最多两秒，完整快照最多三秒，另有有界执行器
清理宽限；输出最多 1 MiB。客户端合并重复刷新，丢弃旧请求、旧配置/服务 generation
的响应，隐藏时暂停轮询，可见时最多每五秒刷新。此 CLI 不实现 GUI、批准或策略修改。
回退到旧 AW 二进制前，先离线删除可选 `view` 字段。

[Schema](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-provider/schemas/view-v1alpha1.schema.json)
的 `$defs/panel` 定义快照；[fixture](https://github.com/agentic-os-org/ANOLISA/tree/main/src/aw/crates/aw-provider/examples/views)
覆盖正常、离线、缺组件、无数据、部分失败和陈旧状态。
[开发合同](../../../../src/aw/docs/design/zh/providers/provider-views.md)定义登记、错误和关联规则。

## 从源码构建

在 Linux 上从仓库根目录执行：

```bash
cargo build --locked --manifest-path providers/tokenless/Cargo.toml
cargo build --locked --manifest-path src/tokenless/Cargo.toml -p tokenless-cli
```

将生成的 `providers/tokenless/target/debug/aw-provider-tokenless` 和
`src/tokenless/target/debug/tokenless` 路径填入共用示例。干净源码的 Preview 构建器
支持 `--component tokenless` 或 `--component all`。

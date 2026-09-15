# Tokenless 演进路线

[English](evolution-roadmap.md)

Tokenless 统一压缩管线演进的规范参考。各 crate 中引用的章节编号
（`§4.1`–`§6`）、设计原则与里程碑标记（`M1`、`M4`）均指向本文档。
本文档汇总了已落地代码与已合并实现 PR 中所编码的演进路线，状态以
Tokenless 0.7.14 为准，含该版本发布后合入的 PostTool 管线重构
（PR #2974）与 protocol v2 生命周期（PR #2978）。

## 目标

一条共享的 Rust 压缩管线，同时服务 CLI hook、进程内 Runtime 与各框架
适配器。以单一版本化兼容边界取代各适配器私有的载荷格式：protocol v2
承载四个类型化生命周期操作——BeforeModel、PreTool、PostTool、
Retrieve——于带归因的信封之中，适配器只需构建操作专属的请求，取回构建
宿主信封所需的类型化结果事实。protocol v1 的泛化
`CompressionRequest` / `CompressionResponse` 对（0.7.13 发布）已被取代，
不再被解析。必须保持原样的 UI 或业务对象永不进入协议。

压缩始终是可选优化：管线中的任何失败都不允许使请求失败，任何未应用的
结果都原样输出原始内容。

## 设计原则

原则编号与已发布代码中的引用保持一致；未被已发布代码引用的编号在此
不做复述。

- **原则 2 —— 按内容路由，按接缝约束。** 一个压缩器只有在支持检测出的
  内容类型、运行于请求的接缝、且适配器声明了其所需的全部能力时才运行。
  所有能力默认为 `false`，未声明任何能力的适配器得到的是 passthrough
  而非无法输出的候选者：无法替换模型可见输出的宿主不会运行重塑响应形态
  的压缩器；只有在宿主发布了检索工具时才挂载 Stash，可恢复有损标记因此
  不会成为死链。已发布的 JSON 域压缩器（§5.3）运行于 `post_tool` 并以
  `replace_output` 为门槛：它在没有检索工具的宿主上仍会运行，并按实际
  发生的情况声明可恢复性——无法入 Stash 的截断报告为 `unrecoverable`；
  不存在可信 Retrieve 路径时，有损候选者被直接拒绝（原则 5）。
- **原则 5 —— 显式可恢复性声明。** 每个应用的变换都报告其恢复状态——
  `lossless`、`retrievable` 或 `unrecoverable`；宿主未发布可信检索工具
  时，有损候选者被拒绝（`recoverability_unavailable`）。
- **原则 6 —— 失败开放，诊断有界。** 压缩器失败绝不使请求失败；首个
  失败被保留为以 `DIAGNOSTIC_MAX_BYTES`（4 KiB）为界的诊断，`output`
  始终恰好携带适配器应输出的内容。

## 架构（§4）

### §4.1 Protocol 边界

`tokenless-protocol` 定义兼容边界。protocol v1（0.7.13 发布，
PR #2783）承载带 `protocol_version` 的泛化
`CompressionRequest` / `CompressionResponse` 对，以及 `Seam`、
`Capabilities`、`Disposition`、`Reversibility` 类型。protocol v2
（PR #2978）以四个类型化生命周期操作取代它——`before_model`（发往模型
的工具声明）、`pre_tool`（执行前的工具参数）、`post_tool`（一条完成的
工具结果）、`retrieve`（一个可见 stash 标记）——由
`RequestEnvelope` / `ResponseEnvelope` 连同请求 `Attribution` 承载。
兼容规则：

- 载荷按严格模式解析：未知字段被拒绝而非忽略，任何线格式变更都是
  刻意的。
- `RequestEnvelope::from_json` / `ResponseEnvelope::from_json` 先校验
  版本再解析形态，不受支持的版本报告 `UnsupportedVersion` 而非误导性的
  形态错误。
- 响应必须携带其请求所选的操作（否则 `OperationMismatch`）；不兼容的
  形态必须启用新的 `protocol_version`，绝不允许并行的适配器私有载荷。
- 进程内调用方直接使用操作专属的载荷类型；信封仅服务于 CLI 与其他
  跨进程传输。

### §4.2 内容检测与域分派

Runtime 的 `post_tool` 模块承载 protocol `ContentType` 分类（`json`、
`search_results`、`build_log`、`stack_trace`、`diff`、`html`、
`tabular`、`source_code`、`plain_text`、`unknown`）与确定性的有界成本
检测器。第一阶段只把 JSON 域分派给压缩器；其余检测出的域在其压缩器被
刻意接入之前原样通过。

检测是内容的纯函数，绝不完整解析任何格式；昂贵解析留在被选中的
压缩器内部。检查量以首部 64 KiB 前缀为界，仅 JSON 括号嗅探额外检查
至多 64 KiB 的末尾窗口；按行的检查以 200 行为上限。检查从最具特征的
形态到最一般的形态依次进行，且检测天生保守：HTML 仅识别以其自身起始
的完整文档，源代码需要 shebang 或多个声明关键字行，类二进制输入归为
`unknown`（里程碑 M4 策略：歧义片段不做分类）。

### §4.3 PostTool 执行与端到端裁决

Runtime 的 `PostToolPipeline` 让 post-tool 请求经过有界检测、域分派与
一次性最终裁决，对原始内容与候选者做一次性比较。未移除归一化 token、
违反必需可逆性、或超出超时预算的候选者被整体拒绝；其试写的 Stash 写入
按 `(key, generation)` 回滚，并原样输出原始内容。请求还携带适配器观察
到的内容来源（`ContentOrigin`）：来源决定截断阈值——命令输出与
API/文件内容的阈值相差一个数量级以上——文件内容则原样通过。

### §4.5 适配器边界

适配器拥有各自的宿主私有契约。每个生命周期请求只携带操作专属的事实；
响应携带适配器构建宿主信封所需的类型化结果（`disposition`、检测出的
`content_type`、`applied_operations`、`recoverability`、
`before_tokens`/`after_tokens` 与 `tokenizer_id`、`stash_keys`、有界的
工具错误上下文）。适配器无需本地兜底逻辑：`output` 永远可直接输出。

### §4.6 接缝

Agent 循环暴露四个生命周期接缝，各对应一个 protocol 操作：
`before_model`（发往模型的工具声明，如 schema 压缩）、`pre_tool`
（执行前的工具参数，如 RTK 命令重写）、`post_tool`（主压缩接缝）、
`retrieve`（恢复一个可见 stash 标记，检索时以当时可见的标记集授权）。
响应中只出现已提交且已应用结果的 Stash key；被回滚的候选者绝不泄漏
key。

## 决策与契约（§5）

### §5.1 Token 计数器决策

所有 token 计数使用字符类启发式 `heuristic-v1`（CJK ≈ 每字符 1
token，其他 ≈ 每 4 字符 1 token），由 `tokenless-protocol` 统一实现、
`tokenless-stats` 再导出——不使用 provider 的 tokenizer。计数是用于
裁决与归因的归一化 token，而非计费估算。估算器的字符类别或比率一旦
变化就必须启用新的计数器 ID，不同 ID 产出的记录在没有显式按计数器
拆分的情况下永不合并为同一序列。

### §5.2 路由契约

未知或歧义内容路由到 passthrough，尚无实现的接缝同样如此。检测只把
记录形态的 JSON（`{...}` / `[...]`）路由给 JSON 压缩器；标量根原样
通过，检测出但尚无压缩器接入的域在其压缩器落地前原样通过。误分类在
设计上退化为失败开放的 passthrough 路径。

### §5.3 Response 清理接入 PostTool 管线

既有的 JSON response 清理由 JSON 域压缩器（`tokenless-compressors` 中的
`JsonCompressor`）实现，`post_tool` 生命周期操作、CLI
`compress-response` 命令、`TokenlessRuntime::compress_response` 与
Python 绑定背后的共享路径改经 Runtime 自有的 `PostToolPipeline`。
一个超时预算（进程内 10 秒）守护整次运行；超时时返回原始内容并回滚
Stash 写入。可恢复性按实际发生的情况声明：
无截断 → 无损，全部截断已入 Stash → 可恢复，其余 → 不可恢复。

### §5.4 单一外部 hook 入口

此前分散在公共 Python hook 与 CLI 子命令中的四项决策——JSON 检测、
工具阈值选择、TOON 选择、最终尺寸接受——收敛到一个共享接缝路由器：

- `tokenless compress` 子命令承载 protocol 信封（stdin
  `RequestEnvelope` → stdout `ResponseEnvelope`），与进程内 Runtime
  生命周期方法二者经由同一入口路由；
- 外部 hook 变为仅处理信封的适配器：构建一个请求、至多启动一个
  `tokenless` 子进程、把响应翻译为宿主信封；
- 适配器契约夹具按已迁移的 agent 覆盖五类行为（passthrough /
  replacement / no-savings / timeout / malformed）；
- 新路由行为由运行时配置开关控制，默认关闭，随接线变更一并引入。

状态：已随 0.7.14 发布（PR #2844）。公共 Python hook
（`compress_response_hook.py`、`compress_schema_hook.py`）已迁移到统一
入口，现发送 protocol v2 信封（PR #2978）；codex / hermes / openclaw /
dsh / SDK 适配器在迁移前保持现有 direct-API 路径。

### §5.5 统计迁移

归因列（`agent_id`、`session_id`、`tool_use_id`）与 retrieve 事件已
进入统计 schema（PR #2885）；protocol v2 在 core 内由操作结果派生统计
记录，归因随请求信封进入统计，不再单独上报（PR #2978）。遗留的
dry-run 测量通道（`CompressResult.compressed_output`，记录预测候选
文本）为仍走 direct API 的适配器保留，待其迁移后移除。

### §5.6 共享词汇与平价

CLI、Runtime 与语言绑定共享同一套 disposition 名称与线格式字符串
（protocol 的 `Disposition` 枚举），所有计数经由同一个
`heuristic-v1` 估算器，保证一切裁决使用相同数字。里程碑 M1 出口门槛
要求 CLI 与 Runtime 在该词汇上达成一致；行为平价按五类行为对每个已
迁移的 agent 做断言。

## §6 压缩器包

内容域压缩器以无状态引擎的形式位于 `tokenless-compressors`，返回完整
结果；内容路由、最终裁决与 Stash 提交/回滚由 Runtime 拥有。第一阶段只把
`JsonCompressor` 接入 PostTool 管线。新的域压缩器通过把其引擎刻意接入
Runtime 而加入，绝不做投机性接入——在此之前，未接入的域原样通过。

## 里程碑标记

- **M1** —— 出口门槛：CLI 与 Runtime 就共享 disposition 词汇达成
  一致（§5.6）。在 response 压缩接入注册表、Runtime 的 pre-protocol
  disposition 枚举退役时达成。
- **M4** —— 保守检测策略：歧义片段不做分类（§4.2）。已编码于已发布
  的检测器中。

## 实现状态

| 章节 | 交付物 | 状态 | 参考 |
|---------|-------------|--------|-----------|
| §4.1 | Protocol 边界：v1 压缩信封，被 v2 生命周期操作取代 | v1 已随 0.7.13 发布；v2 于 0.7.14 后合入 | PR #2783、PR #2978 |
| §4.2 | 内容分类、检测器、域分派 | 已随 0.7.13 发布，0.7.14 后重构 | PR #2788、PR #2974 |
| §4.3 | Runtime 自有的 PostTool 执行与端到端裁决 | 已随 0.7.13 发布，0.7.14 后重构 | PR #2799、PR #2974 |
| §5.3 | Response 清理接入 PostTool 管线 | 已随 0.7.13 发布，0.7.14 后重构 | PR #2816、PR #2974 |
| §5.4 | 统一外部 hook 入口、契约夹具、运行时开关 | 已随 0.7.14 发布 | PR #2844 |
| §5.5 | 统计归因迁移 | 归因列与 retrieve 事件已发布；遗留 dry-run 通道待适配器迁移 | PR #2885、PR #2978 |
| §6 | 域压缩器包（JSON 优先） | 0.7.14 后合入 | PR #2974 |

遗留的 `compress-response` / `compress-schema` / `compress-toon`
子命令与 pre-pipeline Python helper 保留到所有使用方迁移到统一入口
为止；其移除是之后的专项步骤。

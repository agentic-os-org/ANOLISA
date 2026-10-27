# semantic-arbiter

[English](README.md)

Agent 无关的语义判断组件，支持可配置决策模型后端与基于记录时钟的确定性检查。Rust 库依据冻结的任务、候选和证据判断保留意图与任务范围。百炼和 TypeSafe JEV 适配器在确定性资源约束与本地重建要求内解释任务意图。结果属于建议；消费方负责授权和执行。

## 使用方式

- `check` 在 `check_at_ms` 对冻结的结构化输入执行确定性求值；它不重放 Agent 或其动作。该命令使用 `semantic-arbiter/v1alpha1` 与 `deterministic_check` 模式。
- `evaluate` 保留 `semantic-arbiter/v1alpha2`，在实时截止期限内执行模型求值。确定性硬阻断保持 `block`；满足模型求值条件时，模型歧义、超时或后端/响应错误产生 `unknown` 答案和候选 `nop`，绝不产生 `ask`。
- `describe` 以 JSON 列出支持的协议、场景、后端和消息上限。
- `--help` 提供人可读的用法和示例；`help <command>` 解释各子命令。

两种请求格式都可选携带 `candidate.native_tool: {"name":"Bash","input":{"command":"printf test"}}`。应提供完整原生参数对象，而不是提取后的命令摘要。名称须非空白且不超过 128 个 UTF-8 字节；紧凑 input JSON 最多 65536 字节。存在该字段时，使用 `contract::candidate_revision(&candidate)` 设置 `binding.candidate_revision`，证据也须保留相同绑定。带域分隔的 SHA-256 覆盖操作、资源和递归按键排序的原生数据。任务范围求值将全部参数视为不可信数据，而非指令或权限，且不执行其中任何内容。省略 `native_tool` 时保留旧序列化、输入摘要及调用方自定 revision；修订后的任务范围问题会改变模型配置摘要。

场景选择使用 `preservation` 和 `task_scope`。当前 `check` 输出 `allow/block`；当前 `evaluate` 输出 `nop/allow/block`。所有操作及未来场景共用完整 CandidateControl 契约，顺序为 `nop < allow < ask < block`。`ask` 表示确定性的审批决策；模型歧义不会产生 `ask`。类型、校验和 Approval Directive 投影现已预留，真实审批执行与 Adapter 准入仍待消费方集成。

公共库现已提供四组显式扩展契约。`EvidenceSourceRegistry` 冻结协作式采集且受 deadline 约束的来源输出。`JudgmentUnitRegistry` 为每个封闭的现有场景保存恰好一个单元，因此消费方可以替换成果保护或任务范围单元，同时不会增加任意线协议场景，也不使用动态发现。`Evaluator::with_units` 使用同一个注册表完成问题准备、绑定单元修订的配置摘要和答案推导；`EvaluationRecord` 冻结期望响应供后续复核，中央校验会拒绝非法自定义答案。`composition::compose_outcome` 确定性组合类型化贡献，消费方侧 executor API 对类型化 Directive 执行准入、占用、分发和独立查询。生产证据源、真实 Executor、原生 Adapter 集成，以及生产级持久 claim、审计和效果核验仍待消费方集成。模型概率阈值是未经校准的决策参数，不能证明语义准确率或提供安全保证。

## 源码快速运行

尚无安装器和 RPM 分发。源码构建使用 Linux 检出目录，需准备 Rust/Cargo、rustfmt、clippy 和 Make。从仓库根目录执行：

```bash
cd src/semantic-arbiter
make build
./target/debug/semantic-arbiter describe
make check
```

仓库还提供可运行的开发者示例，分别演示替换一个封闭场景判断单元、运行内存 executor fixture，以及从内存 EvidenceSource 采集证据：

```bash
cargo run --example judgment_unit
cargo run --example executor_fixture
cargo run --example evidence_source
```

配置、合成输入、命令和错误见 [CLI 指南](../../docs/user-guide/zh/semantic-arbiter/index.md)，字段定义和校验规则见 [协议参考](docs/design/protocol_zh.md)。后端配置选择 `bailian` 或 `typesafe`；`thresholds.preservation_ppm` 和 `thresholds.task_scope_ppm` 默认均为 `900000`。百炼样例显式将二者设为 `800000`，并提供工作区端点模板。

## 模块边界

| 模块 | 职责 |
|---|---|
| `contract` | 任务/候选绑定、证据来源、预算和单元答案 |
| `evidence` | 显式来源注册、有界采集、冻结证据和覆盖缺口 |
| `config` | 后端选择、端点配置、凭证环境变量名称和阈值 |
| `model` | Noul 概率、后端接口、显式测试夹具和有界 HTTP 传输 |
| `judgment` | 封闭场景单元注册表、版本化问题和集中校验的概率解释 |
| `evaluation`、`deadline` | 模型求值、冻结的复核记录和原始实时截止期限 |
| `composition` | 确定性贡献合并和细粒度能力声明 |
| `executor` | 消费方授权、幂等 claim/dispatch/query 契约和内存夹具 |
| `outcome` | 类型化建议、Directive 投影和三态回执 |
| `protocol`、`validation`、`wire` | 能力声明、解码、绑定和语义校验 |
| `check` | 基于记录时钟的确定性检查和固定建议模板 |
| `main` | 有界 CLI 输入、后端构造和脱敏 JSON 输出 |

一个 Cargo package 包含公共库、CLI 和测试，无 AW 或 ws-ckpt 路径依赖。仅合成请求生成器需要 Python。应使用公共解码和校验入口。模型响应校验需要原始请求、配置和独立留存的 `EvaluationRecord`；该记录绑定原始配置和冻结的期望响应，复核时不会再次调用判断单元或模型。导出的 trace 本身不提供认证。CLI 保持使用内置单元、固定组合和现有协议版本。

消费方必须独立认证任务入口和证据来源，保留不可变绑定，并在执行 Directive 前核验授权、截止期限和实际效果。`Outcome::directives` 仅投影建议。executor 契约接收受信 `AuthorizationContext`，要求受支持的协议版本和细粒度能力，并将每个幂等键绑定到 `api_version`、`request_id`、完整 `Binding` 和不可变 `Directive`。已有 claim 只执行查询，不再次分发；传输成功不能证明效果已应用，未知效果绝不自动重放。`executor::fixture` 仅供内存测试和示例使用，不能证明原生 Adapter 支持。答案 `unknown`、候选 `nop`、候选 `ask` 和执行回执 `unknown` 是不同状态。退出码为零只表示完整判定，不表示许可。组件不输出密钥或原始上游响应正文。远端可用性和模型准确率需要单独进行授权验证。

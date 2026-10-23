# semantic-arbiter

[English](README.md)

Agent 无关的语义判断组件，支持可配置决策模型后端与基于记录时钟的确定性检查。Rust 库依据冻结的任务、候选和证据判断保留意图与任务范围。百炼和 TypeSafe JEV 适配器在确定性资源约束与本地重建要求内解释任务意图。结果属于建议；消费方负责授权和执行。

## 使用方式

- `check` 在 `check_at_ms` 对冻结的结构化输入执行确定性求值；它不重放 Agent 或其动作。该命令使用 `semantic-arbiter/v1alpha1` 与 `deterministic_check` 模式。
- `evaluate` 保留 `semantic-arbiter/v1alpha2`，在实时截止期限内执行模型求值。确定性硬阻断保持 `block`；满足模型求值条件时，模型歧义、超时或后端/响应错误产生 `unknown` 答案和候选 `nop`，绝不产生 `ask`。
- `describe` 以 JSON 列出支持的协议、场景、后端和消息上限。
- `--help` 提供人可读的用法和示例；`help <command>` 解释各子命令。

场景选择使用 `preservation` 和 `task_scope`。当前 `check` 输出 `allow/block`；当前 `evaluate` 输出 `nop/allow/block`。所有操作及未来场景共用完整 CandidateControl 契约，顺序为 `nop < allow < ask < block`。`ask` 是确定性的审批决策，不是歧义回退；类型、校验和 Approval Directive 投影现已预留，运行时 `ApprovalExecutor` 与 Adapter 准入仍待 A4/A10。实时取证、动态注册、可配置组合、其他 Executor 和 AW Adapter 也尚待实现。模型概率阈值是未经校准的决策参数，不能证明语义准确率或提供安全保证。

## 源码快速运行

尚无安装器和 RPM 分发。源码构建使用 Linux 检出目录，需准备 Rust/Cargo、rustfmt、clippy 和 Make。从仓库根目录执行：

```bash
cd src/semantic-arbiter
make build
./target/debug/semantic-arbiter describe
make check
```

配置、合成输入、命令和错误见 [CLI 指南](../../docs/user-guide/zh/user-entrypoint/semantic-arbiter.md)，字段定义和校验规则见 [协议参考](docs/design/protocol_zh.md)。后端配置选择 `bailian` 或 `typesafe`；`thresholds.preservation_ppm` 和 `thresholds.task_scope_ppm` 默认均为 `900000`。百炼样例显式将二者设为 `800000`，并提供工作区端点模板。

## 模块边界

| 模块 | 职责 |
|---|---|
| `contract` | 任务/候选绑定、证据来源、预算和单元答案 |
| `config` | 后端选择、端点配置、凭证环境变量名称和阈值 |
| `model` | Noul 概率、后端接口、显式测试夹具和有界 HTTP 传输 |
| `judgment` | 版本化保留意图与任务范围问题，以及概率解释 |
| `evaluation`、`deadline` | 模型求值、留存观测和原始实时截止期限 |
| `outcome` | 类型化建议、Directive 投影和三态回执 |
| `protocol`、`validation`、`wire` | 能力声明、解码、绑定和语义校验 |
| `check` | 基于记录时钟的确定性检查和固定建议模板 |
| `main` | 有界 CLI 输入、后端构造和脱敏 JSON 输出 |

一个 Cargo package 包含公共库、CLI 和测试，无 AW 或 ws-ckpt 路径依赖。仅合成请求生成器需要 Python。应使用公共解码和校验入口。模型响应校验需要原始请求、配置和独立留存的 `EvaluationRecord`；导出的 trace 本身不提供认证。

消费方必须独立认证任务入口和证据来源，保留不可变绑定，并在执行 Directive 前核验授权、截止期限和实际效果。`Outcome::directives` 仅投影建议。答案 `unknown`、候选 `nop`、候选 `ask` 和执行回执 `unknown` 是不同状态。退出码为零只表示完整判定，不表示许可。组件不输出密钥或原始上游响应正文。远端可用性和模型准确率需要单独进行授权验证。

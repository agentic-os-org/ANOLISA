# AW 源码布局与 CI 开发规则

[English](../../en/development/validation.md)

这些规则由 [AGENTS.md](../../../../AGENTS.md) 按任务索引；修改前必须阅读适用章节。

检出目录名称不改变组件归属，也不决定使用其他组件的技能或验证流程。

工作区及原生验收放 `tests/`，运行时桥接资源放 `crates/aw-adapter/native/`。
变更 crate 边界时同步更新 `scripts/check_layout.py`、行为测试和架构文档。

Rust、Python、JavaScript（`.js`、`.mjs`、`.cjs`）文件上限为 699 行，包含测试和嵌入夹具，
600 行开始提示。现有 `crates/aw-contracts/src/validation.rs` 的 711 行例外固定保留。
超限前按职责拆分为具名相邻模块，保留测试入口与断言，不通过压缩代码或新豁免绕过限制。
`mod.rs` 只允许仓库规范中的 `tests/common/mod.rs`。

AW 与 Provider PR 运行共享回归清单、workspace 测试、格式检查与 Clippy。主线、发行分支推送和手工检查额外运行两种 umask 下的包测试及 rustdoc。仅改动 Tokenless 后端输入时运行真实消费者兼容测试，同时修改 AW 时两项均运行。

日常检查无需凭据。夜间集成一次构建全部包，复用带校验和、绑定候选 SHA 的二进制做
固定版本原生验收。保留精确版本证据、有期限的进程、完整报告和人工验收边界，缺失或
跳过的 suite 必须使集成失败。使用私有原生工具环境，不因 runner loader 覆盖或共享
可写缓存而削弱准入。发布前对精确 release SHA 重跑。

`AW / required` 在范围选择后汇总并行 AW 与 Tokenless job。缓存、job 完整结果和
已测 SHA 保持独立；后续产物失败或取消仍应阻止通过。本地入口保留相同检查，无需依赖
Actions 编排。范围选择覆盖完整变更，并保留候选／已测 SHA 身份。

测试 harness 只编译一次，直接枚举并复用到 workspace 测试。不因只修改某个 Adapter
而跳过共享安全、Tokenless 或生命周期测试。门禁修改需覆盖遗漏、忽略测试和失败路径
的负向行为测试。

源码、协议 schema、测试夹具与生成产物分开。构建／测试输出放 `target/`，约束进程
存活时间，清理本任务子进程、profile、socket 和临时状态。

# AW

[English](README.md)

AW 提供统一配置、版本化能力合同和可嵌入的 Core 编排。`aw-config` 校验配置结构和引用；`aw-provider` 离线检查外部 Provider 消息与能力准入；`aw-contracts` 检查数据结构及记录间关系；`aw-core` 通过调用方提供的 Host 执行固定计划，并持久记录执行事实。AW 没有独立服务进程，原生 Agent 控制和最终工具执行仍由接入方负责。

当前接口仍处于实验阶段。测试使用合成记录，实际运行时接入需要另行验证。

## 校验配置

在仓库根目录使用离线示例检查配置：

```bash
cd src/aw
cargo run --locked -p aw-config --example validate -- crates/aw-config/examples/aw.minimal.yaml
```

校验通过表示配置语法和静态引用正确，不会启动 Agent 或启用策略。
字段和示例见[配置指南](../../docs/developer-guide/zh/aw/configuration.md)，
开发环境、测试与 CI 说明见[参与 AW 开发](CONTRIBUTING_zh.md)。

## 嵌入 Core

`aw-core` 提供 `Core::prepare`、`Core::execute`、可信 Host/Clock/Journal 端口，
以及支持持久写入的 Linux `FileJournal`。准备阶段在调用任何 Provider 前检查完整
计划；执行阶段先记录调用再分发，Journal 确认后才返回终态结果。失败或中断的事件
仍保留占用记录，不自动重试或恢复。

所有权、取消、失败和接入约束见 [Core 执行与存储](docs/design/core-execution_zh.md)。
Core 测试使用合成 Host；Agent 原生接入和效果采用需要单独进行运行时验收。

## 源码参考

- [用户指南与可用范围](../../docs/user-guide/zh/user-entrypoint/aw.md)、
  [配置参考](../../docs/developer-guide/zh/aw/configuration.md)、
  [起步模板](crates/aw-config/examples/aw.minimal.yaml)、
  [完整示例](crates/aw-config/examples/aw.yaml)与
  [配置 API](crates/aw-config/src/lib.rs)
- [已注册的 Schema](schemas/)与[合成输入输出样例](tests/fixtures/contracts.json)
- [公共 API](src/lib.rs)、[记录校验](src/validation.rs)与[计划校验](src/orchestration.rs)
- [外部 Provider 协议与准入](docs/design/provider-protocol_zh.md)及
  [Provider API](crates/aw-provider/src/lib.rs)
- [编码测试](tests/canonical.rs)、[Schema 测试](tests/schemas.rs)、
  [记录测试](tests/contracts.rs)与[计划测试](tests/orchestration.rs)

Registry 包含 21 个 Schema 资源。`crates/aw-contracts/schemas/` 中的 8 份 v1 文件仅作参考，未注册到当前库。调用方需要匹配 Schema ID 和摘要，当前没有自动版本转换。

收到 wire 记录后，先用 `canonical::parse` 严格解析字节，再检查结构。结构检查通过不代表记录之间的关系正确，也不授予执行权限。计划级检查的用法见公共 API 文档，证据认证和实际动作仍由调用方负责。

用户配置由独立的 `aw-config` crate 及其 `aw/v1alpha1` Schema 处理。
文件使用一个包含 `apiVersion`、`kind`、`metadata`、`spec` 的 `AWConfiguration`
对象，Provider 实例是 `spec.providers` 中的命名对象。Schema 识别 QwenPaw、
Qoder CLI、OpenClaw、Hermes 及全部 16 个事件名，不表示适配器已经实现。
配置中没有运行时 `status`。`aw-provider` 校验外部提供的操作声明和私有配置响应，
并依据调用方信任的 Adapter 能力准入工具步骤；它不执行发现，也不证明原生采用。
Provider 执行和绑定安装由后续增量交付。与既有 wire 合同的关系见
[配置设计](docs/design/configuration_zh.md)。

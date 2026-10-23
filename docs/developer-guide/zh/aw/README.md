# AW 开发指南

[English](../../en/aw/README.md)

这里说明如何构建 AW、接入组件，以及维护 Adapter 和 Provider。
安装与日常使用请从 [AW 用户指南](../../../user-guide/zh/aw/index.md)开始。

| 任务 | 文档 |
| --- | --- |
| 看懂 crate 关系和请求流转 | [架构总览](architecture.md) |
| 本地编译并交付安装包 | [从源码构建 AW](building.md) |
| 开发、收录和分发 Provider | [Provider 协作](providers.md) |
| 接入自己的检查脚本或 Provider | [组件接入教程](integration.md) |
| 查询 YAML 字段与校验规则 | [配置参考](configuration.md) |
| 接入框架、SDK worker 或排查 Hook 行为 | [原生 Agent 接入参考](native-agents.md) |
| 维护安装包、完整性校验与配置编辑工具 | [打包与配置工具参考](packages.md) |
| 查询 Tokenless 的工具映射、结果替换和统计协议 | [Tokenless Provider](tokenless.md) |
| 对接 sec-core 后端和工具参数 | [sec-core Provider](sec-core.md) |
| 查询 AgentSight 生命周期与观测配置 | [AgentSight](agentsight.md) |

协议设计和测试入口见 [AW 组件 README](../../../../src/aw/README_zh.md#开发与验证)。

# 更新日志

[English](CHANGELOG.md)

## [0.1.0] - 2026-11-07

### 新增

- **AW**：首次发布 v0.1.0，用一份策略配置接入多个原生 Agent，统一审计、取消和实例清理
  （[#9222](https://github.com/agentic-os-org/ANOLISA/pull/9222)）。
- **AW**：将用户提交和工具 Hook 接入结构化 Provider、脚本及原生命令。
  提交前和工具执行前可阻断；结构化后置 Provider 可替换获准的结果文本。
- **AW**：独立提供 sec-core、Tokenless 桥接包。sec-core 检查提示词注入、输入／输出
  PII、代码及受支持的本地 Skill；Tokenless 压缩工具结果并提供命令重试建议。
- **AW**：支持多个已配置 Agent 共享本地服务、按实例停止，以及受支持常驻模式下任务排空后的策略更新。
- **AW**：通过 Makefile 分阶段安装并保留原生包管理器的文件归属，为独立 Provider 提供本地 bundle 安装入口。

### 支持范围矩阵

支持 Linux。下表适用于普通启动模式；受限热更新模式有额外准入要求。

| Adapter | 工具执行前观察／阻断 | 工具执行后观察／文本替换 | 用户提交观察／阻断 |
| --- | --- | --- | --- |
| AgentScope | 支持 | 支持 | 支持 |
| Claude SDK | 支持 | 支持 | 支持 |
| Hermes | 支持 | 支持 | 支持 |
| OpenClaw | 支持 | 支持 | 暂不支持 |
| Pi | 支持 | 支持 | 支持 |
| Qoder CLI | 支持 | 支持 | 支持 |
| Qoder SDK | 支持 | 支持 | 支持 |
| QwenPaw | 支持 | 支持 | 支持 |

提交检查覆盖各 Adapter 的已接入文本入口。结果替换作用于已接入的文本位置，保留原生元数据和失败状态。
Qoder CLI 后置检查覆盖成功的工具调用。Skill 检查覆盖 Qoder SDK、QwenPaw、AgentScope、
Hermes 的本地来源及 Pi 的 Skill 提交。Qoder SDK、QwenPaw 和 AgentScope 的普通常驻／工厂模式支持空闲策略更新。

MCP、原生配置、私有凭据及各模式的具体范围见[能力说明](../../docs/user-guide/zh/aw/capabilities.md)，
可复现版本基线见[原生测试矩阵](tests/native-matrix.json)。

### 部署

- 配置框架、业务工厂、模型及后端服务，安装 Provider 后通过配置启用策略。
- Provider 通过进程协议独立接入。
- 预览版升级时同步调整 Provider 路径与策略配置；保留上一版产物及配置用于回退。

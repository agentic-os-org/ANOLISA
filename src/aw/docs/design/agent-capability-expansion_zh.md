# Agent 能力矩阵扩展

[English](agent-capability-expansion.md)

AW 按 Agent 行与事件/效果列扩展，保持统一入口
`aw run --config aw.yaml --agent <target>`。每个支持的格子都需要可执行契约测试和
明确的原生验收边界，规划中的格子不代表运行时能力。本提案先接 DeepSeek Harness
headless，Codex CLI 作为独立接入后续完成。

## 能力与证据模型

编译后的 Adapter catalog、配置及 Provider 请求 schema 和回归矩阵必须一致。每行对应 Adapter，
原生清单记录固定版本、入口、责任人和准确验收范围。列对应事件/效果组合，初期为
执行前观察、执行前阻断、执行后观察、执行后替换结果；工具类型限制随验收证据说明。

每个支持的组合必须有测试。矩阵词汇中剩余的组合必须显式声明不支持，并有配置准入
拒绝测试。部分支持不能静默跳过安全或 Tokenless 义务：按声明能力选择适用的共享测试，
保留独立 Provider 契约回归和所有现有 Adapter 的覆盖。

继续从现有清单生成中英文 README 矩阵。Roadmap 与已支持能力分开，确定性 callback
契约测试与固定版本真实框架验收分开呈现。

## 接入顺序

1. 显式表达部分能力，覆盖遗漏声明、遗漏测试、忽略测试、不支持效果及事件的拒绝。
2. 通过原生插件接入 DeepSeek Harness headless，复用 AW launcher 和 Provider 协议，
   闭环安全拒绝、Tokenless 新调用重试、最终文本采纳与资源清理。
3. 独立接入 Codex CLI 原生 Hooks。先验收执行前观察/阻断和执行后观察，
   再按原生工具路径单独验收成功结果替换。
4. 逐列扩展会话生命周期、子 Agent 身份与取消，再扩展模型/上下文事件。
   在不同框架中验证语义后形成 AW 公共契约，不能直接按原生回调名称认定兼容。

## DeepSeek Harness 边界

固定官方运行时版本，使用 headless profile，保留原生模型配置和权限。
通过私有启动叠加配置注册 AW 插件，不能覆盖用户 profile。安全拒绝不能被后续插件
放宽；Tokenless 建议必须阻断原调用，要求新调用重新经过安全与原生授权，
不能原地改写参数或产生批准。

文本替换必须保留结构化值、metadata 和失败状态。原生 post-execute 后还有最终内容
处理，因此验收要检查最终工具结果和程序化调用中的嵌套工具，不能只统计 AW 回调成功。

上游契约基线：[工具流水线](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/docs/tool-execution-pipeline.md)
与 [CLI profiles](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/apps/cli/reference/README.md)。

## Codex CLI 后续边界

采用原生 PreToolUse/PostToolUse Hooks，保留原生信任和权限规则。写入配置不代表 Hook
已获信任或已生效。匹配的原生 Hooks 可以并发执行；AW 排序自身的 Provider 调用，
同时必须验收与独立用户 Hook 的共存。附加上下文和阻断反馈不等于成功结果压缩。

分别验收 shell、长时命令完成、MCP 和嵌套调用路径，覆盖调用身份、取消与不支持路径。
后续任务复用 DeepSeek 接入中的能力矩阵改造，不复制一套矩阵，也不在 Provider 中加入
框架分支。按固定 CLI 版本重新核对[官方 Hooks 契约](https://learn.chatgpt.com/docs/hooks)。

## 代码边界与 CI

Adapter 负责原生注册、归一化、效果采纳与卸载；公共 launcher 负责生命周期，
aw-exec 负责有界进程。Provider 只依赖公开协议，不依赖框架 API。
按上述职责拆分文件，沿用现有文件大小门禁。

修改 `src/aw/**` 的 PR 运行全部快速安全、Tokenless、生命周期、矩阵及布局检查。
其他 PR 跳过 AW；修改 AW 消费的公开契约时应显式验证兼容性。
Nightly/手动流程构建一次 all-in-one，固定版本原生任务复用带校验和的二进制，
校验完整的预期报告、真实工具行为和进程清理。

原生任务独立并行，缓存按版本和锁文件区分。上游升级单独验收后更新准入版本，
发布前复验确切发布 SHA。不能用 fixture callback 成功或空报告代替原生证据。

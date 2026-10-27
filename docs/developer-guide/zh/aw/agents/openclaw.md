# OpenClaw

[English](../../../en/aw/agents/openclaw.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 OpenClaw

另行安装 OpenClaw 2026.9.6。示例使用 `argv: [openclaw, gateway, run]`，必要时
改为可执行文件的绝对路径。显式传入已有原生 JSON 配置和已存在的绝对 state 目录。
AW 生成私有配置 overlay，保留该目录中的认证和会话；发现 Gateway 所有权冲突时
拒绝启动，不接管正在运行的实例。
AW 对受管 Gateway 禁用 Node 编译缓存，避免启动器重启子进程改变就绪 PID；
自定义包装器必须 exec Gateway。

每个 AW 步骤注册为一个原生插件 handler。OpenClaw 按原生优先级串行执行 before，
并发执行 after；其他插件的优先级保留。事件预算范围为 1..12,000 ms。被阻断的尝试
也可能带错误产生 after，因此 after 不代表执行成功。当前覆盖新 Gateway 内的 Agent
工具执行，operator `tools.invoke` 不属于完整前后接线入口。原生 Gateway startup
回调完成后才报告 Hook 已就绪，该状态与 AW 服务可连接分开判断。

```bash
target/debug/aw run --config crates/aw-service/examples/aw.openclaw.yaml --agent openclaw \
  --native-settings /absolute/openclaw.json --native-state-dir /absolute/openclaw-state
```

[OpenClaw 中性示例](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.openclaw.yaml)
在工具前后执行命令，不安装安全策略。受支持的适配器可复用同一份
`aw-provider/v1alpha1` 策略配置；原生 Hook 的输出格式与事件覆盖仍按框架分别说明。

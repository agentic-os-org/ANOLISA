# OpenClaw

[English](../../../en/aw/agents/openclaw.md)

从 `src/aw` 运行源码示例；先按[接入教程](../integration.md)构建 AW。

## 启动 OpenClaw

单独安装 OpenClaw，已验证基线为 2026.9.6。其他版本接入时，AW 记录已安装版本，
要求原生插件完成注册并收到 Gateway 就绪回执。示例使用 `argv: [openclaw, gateway, run]`，必要时
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

## 准备原生启动并更新策略

OpenClaw 2026.9.6 的前台 Gateway 可以在专用目录安装一次启动桥接，随后在专用终端中设置启动环境，并重启一次 Gateway。Node 版本需要满足所安装 OpenClaw 的运行要求。`aw.yaml` 中的 Gateway 参数应与原生命令一致。

```bash
aw instance setup --config /absolute/aw.yaml --agent openclaw \
  --directory /absolute/aw-openclaw-startup \
  --native-settings /absolute/openclaw.json \
  --native-state-dir /absolute/openclaw-state > openclaw-startup.json
export AW_INSTANCE_BOOTSTRAP="$(python3 -c 'import json; print(json.load(open("openclaw-startup.json"))["environment"]["AW_INSTANCE_BOOTSTRAP"])')"
export NODE_OPTIONS="$(python3 -c 'import json; print(json.load(open("openclaw-startup.json"))["environment"]["NODE_OPTIONS"])')"
openclaw gateway run
```

预加载模块在原生启动前准备绑定，插件在 `gateway_start` 确认就绪。私有配置覆盖层仅向 AW 插件授予会话 Hook 访问权，用于登记原生请求的占用状态。桥接保留会话、请求标识和策略元数据，不导出该生命周期 Hook 收到的会话正文。用户的原生配置保持完整。

在另一个终端中，使用同一份配置更新空闲实例：

```bash
aw instance update --config /absolute/aw.yaml --agent openclaw
```

从 `before_agent_run` 到工具回调完成且 `agent_end` 成功，同一请求固定使用一个策略版本。两个工具事件都支持在请求之间增删步骤，初始策略也可以为空。缺失完成回调、原生取消、活动请求标识重复或无法确认清理时，实例持续占用更新门禁，需要重启该 Gateway 后恢复管理。未获请求准入的工具调用会被阻断。原生 `tools.invoke` 仍不属于这个受管入口。

桥接保留已完成的请求标识，用于拒绝延迟回调和标识复用。每个 binding 最多接纳 4,096 个不同请求，包含活动和已完成请求。达到上限后阻断新请求；继续使用前需重启 Gateway，建立新的 binding。

稳定桥接顺序执行前置步骤，并发执行普通后置观察步骤；含结果替换时，在之前的观察步骤完成后执行最终替换步骤。被拒绝工具的原生完成回调仍会留下审计。事件预算上限为 12,000 ms。

退出 Gateway 和专用终端后，可删除 `setup` 输出中列出的启动文件。下一次原生启动使用原配置。已经运行的 Gateway 需要重启后才能使用这些准备结果。

# 跑通一条工具策略

[English](../../en/aw/integration.md)

使用一个布尔脚本，在本地验证允许、阻断和故障，再接入真实 Agent。
需要 Linux、rustup、C 编译器和 Python 3。仓库固定 Rust 工具链；原生验收另需所选 Agent 与模型账号。

## 1. 构建并准备配置

从仓库根目录执行，后续保留同一终端的变量。

```bash
cd src/aw
cargo build --locked -p aw-cli -p aw-host --bins --examples
export AW_SOURCE="$PWD"
export AW_BIN="$AW_SOURCE/target/debug/aw"
export AW_HOST="$AW_SOURCE/target/debug/examples/host"
export AW_DEMO="$(mktemp -d "$AW_SOURCE/target/aw-onboarding.XXXXXX")"
export AW_CONFIG="$AW_DEMO/aw.yaml"
install -d -m 700 "$AW_DEMO/workspace" "$AW_DEMO/state"
cp "$AW_SOURCE/crates/aw-service/examples/aw.shared.yaml" "$AW_CONFIG"
cp "$AW_SOURCE/crates/aw-service/examples/check-tool.py" "$AW_DEMO/check-tool.py"
chmod 600 "$AW_CONFIG" "$AW_DEMO/check-tool.py"
printf 'AW_BIN=%s\nAW_CONFIG=%s\nAW_DEMO=%s\n' "$AW_BIN" "$AW_CONFIG" "$AW_DEMO"
```

编辑 `$AW_CONFIG`，将占位符换成展开后的绝对路径，AW 不对 YAML 做 shell 插值。

| 字段 | 填入内容 |
| --- | --- |
| `spec.daemon.state_dir` | 打印出的 AW_DEMO 加 `/state` |
| `spec.providers.check-tool.transport.argv[0]` | Python 3 绝对路径 |
| `spec.providers.check-tool.transport.argv[1]` | 打印出的 AW_DEMO 加 `/check-tool.py` |
| `spec.agents.<id>.argv` | 原生验收前填写所选框架入口，本地 Host 验证可保留占位符 |

示例在 before 输入含 `AW_DENY_TEST` 时请求阻断，after 请求观察。它是接线测试规则。
已安装 core 的应用也可用 `aw config init` 创建 Qoder/OpenClaw 配置，再按[编辑教程](reference/configuration-editor.md)添加规则。

## 2. 验证规则

```bash
"$AW_BIN" validate --config "$AW_CONFIG"
cd "$AW_DEMO/workspace"
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_OK"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.before demo_shell '{"command":"printf AW_DENY_TEST"}'
timeout 15s "$AW_HOST" "$AW_CONFIG" tool.after demo_shell '{"command":"printf AW_OK"}'
```

预期依次为 `candidate_effects: []`、`block/demo_marker_denied`、`observe/demo_tool_observed`，每次 `failure` 为 null。
Host 使用合成事件，输出 `adoption: not_tested`，不会执行事件中的 Shell 命令。

复制配置为 `$AW_DEMO/aw.failure.yaml`，仅把脚本路径改为不存在的文件，使用该副本重跑第一条 Host 命令。
应非零退出、failure 非 null、`failure_action: block`。验证后删除该副本。

## 3. 接入原生 Agent

按[Agent 索引](native-agents.md)准备框架与 profile，沿用 `AW_BIN`、`AW_CONFIG`、`AW_DEMO` 执行[原生效果验收](validation.md)。
验收同时核对 AW 拒绝结果、工具标记和审计；模型自行拒绝时应重新选择能到达工具回调的用例。

扩展规则时选择所需协议。

| 方式 | 程序实现 | 参考 |
| --- | --- | --- |
| 布尔 Hook | 读取事件，输出 JSON true/false，退出 0 | [Hook 教程](providers/hooks.md) |
| 原生 Hook | 所选框架的原始输入、输出与退出状态 | [Hook 教程](providers/hooks.md) |
| 结构化 Provider | `describe`、`validate_config`、`invoke` | [协议](providers/protocol.md) |

安全 Provider 还需映射实际工具名和输入指针，见 [sec-core](providers/sec-core.md)。

## 4. 清理

退出 Agent，停止共享 AW 服务并确认所属进程退出。保留所需证据后删除本次目录。

```bash
cd "$AW_SOURCE"
rm -r -- "$AW_DEMO"
test ! -e "$AW_DEMO"
unset AW_CONFIG AW_DEMO AW_HOST AW_BIN AW_SOURCE
```

框架 profile 单独保留，Hermes 插件按[回退步骤](agents/hermes.md#升级与清理)处理。构建产物保留在 `src/aw/target/`。
修改 AW 实现后的检查见[贡献指南](../../../../src/aw/CONTRIBUTING_zh.md)。

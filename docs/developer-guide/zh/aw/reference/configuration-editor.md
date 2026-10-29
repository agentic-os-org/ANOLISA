# 配置编辑命令

[English](../../../en/aw/reference/configuration-editor.md)

编辑器修改已有 YAML/JSON 文件中的 Provider、事件和步骤。先退出 Agent 并停止服务，或在受支持实例上按[空闲更新](../deployment/policy-updates.md)流程应用修改。

## 命令

| 命令 | 行为 |
| --- | --- |
| `aw config show --config ABS_FILE` | 以格式化 JSON 打印完整工作配置 |
| `aw config show --config ABS_FILE --provider NAME` | 打印一个 Provider |
| `aw config show --config ABS_FILE --event EVENT [--id ID]` | 打印一个事件，或其中按事件作用域 ID 选择的步骤 |
| `aw config add-provider --config ABS_FILE --name NAME --definition FILE` | 添加完整 Provider 定义 |
| `aw config remove-provider --config ABS_FILE --name NAME` | 删除没有引用的 Provider |
| `aw config add-event --config ABS_FILE --event EVENT --definition FILE` | 添加显式配置的事件 |
| `aw config add-hook --config ABS_FILE --event EVENT --definition FILE` | 追加完整步骤，定义中包含其 `id` |
| `aw config remove-hook --config ABS_FILE --event EVENT --id ID` | 删除指定事件下的步骤 |

`--config` 使用当前用户拥有、无符号链接的绝对文件路径，文件和父目录不得被组或其他用户写入。拒绝硬链接、特殊权限及扩展访问 ACL；需要 Linux procfs。
定义文件包含一个完整 YAML/JSON 对象，最多 4 MiB、32 层。`show` 会显示私有值。

相同定义重复添加或删除不存在的对象保留原文件；同名不同定义报冲突。先删除引用步骤，再删除 Provider。每条命令独立原子发布，重写注释和格式，修改前保留备份。
`validate` 只校验字段与引用，接受相对路径。完整文件安全与并发规则见[编辑合同](../../../../../src/aw/docs/design/zh/configuration/configuration-editing.md)。

## 给空配置添加规则

完成[安装](../../../../user-guide/zh/aw/installation.md)，沿用 `AW_HOME`、`AW_PREFIX`、`AW_QODER`，另需 Python 3。
创建独立演示目录和配置，再添加检测 `AW_DENY_TEST` 的布尔规则。

```bash
export AW_DEMO="$(mktemp -d "$AW_HOME/hook-demo.XXXXXX")"
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_DEMO/aw-core.yaml" --state-dir "$AW_DEMO/state" --qoder "$AW_QODER"
```

```bash
cat > "$AW_DEMO/check-tool.py" <<'PYTHON'
import json
import sys

event = json.load(sys.stdin)
print(json.dumps("AW_DENY_TEST" in json.dumps(event["tool"]["input"], ensure_ascii=False)))
PYTHON
cat > "$AW_DEMO/provider.yaml" <<YAML
transport:
  type: stdio
  location: agent
  argv: [/usr/bin/python3, "$AW_DEMO/check-tool.py"]
timeout_ms: 1000
max_output_bytes: 32
YAML
cat > "$AW_DEMO/event.yaml" <<'YAML'
enabled: true
required: true
steps: []
YAML
cat > "$AW_DEMO/hook.yaml" <<'YAML'
id: check-marker
provider: company
effects: [block]
reason_code: demo_marker_denied
on_error: block
YAML
chmod 600 "$AW_DEMO/check-tool.py" "$AW_DEMO/provider.yaml" "$AW_DEMO/event.yaml" "$AW_DEMO/hook.yaml"
"$AW_PREFIX/bin/aw" config add-provider --config "$AW_DEMO/aw-core.yaml" \
  --name company --definition "$AW_DEMO/provider.yaml"
"$AW_PREFIX/bin/aw" config add-event --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/event.yaml"
"$AW_PREFIX/bin/aw" config add-hook --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/hook.yaml"
"$AW_PREFIX/bin/aw" config show --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --id check-marker
"$AW_PREFIX/bin/aw" config validate --config "$AW_DEMO/aw-core.yaml"
```

启动后请求工具打印 `AW_OK` 和 `AW_DENY_TEST`。前者应执行，后者应收到 AW 阻断。退出 Agent 后停止服务，确认进程退出，再删除本次演示目录。

```bash
aw run --config "$AW_DEMO/aw-core.yaml" --agent qoder
aw stop --config "$AW_DEMO/aw-core.yaml"
rm -r -- "$AW_DEMO"
```

## 原生配置来源

`spec.agents.<target>.native_config.sources` 为嵌入式 Adapter 记录有序来源引用。
每项必填 `id`、`kind`、`scope`、`reference`；可选 `base_dir` 保留原生相对路径语境。
`kind` 支持 `file`、`environment`、`argument`、`discovery`。引用保存路径或查找键，
凭据值继续保存在原生来源中。ID 在同一目标内唯一，与原生 Hook、MCP、插件名称独立。

| 命令 | 行为 |
| --- | --- |
| `aw config add-native-source --config ABS_FILE --agent TARGET --definition FILE` | 追加 AW 来源描述 |
| `aw config remove-native-source --config ABS_FILE --agent TARGET --id ID` | 移除描述，保留原生来源 |
| `aw config native-sources --config ABS_FILE --agent TARGET` | 输出脱敏的来源类别和顺序，不加载来源 |

编辑器不打开来源引用。它严格校验 AW 描述结构，保持已有顺序，拒绝同 ID 的不同定义，
复用原有原子发布流程。移除描述不卸载原生对象。移除最后一项会删除可选的 `native_config` 段；
降级到旧 AW 构建前，应从所有目标移除此段。编辑会重写 AW YAML 文件；原生文件的
字节、注释和未知字段完整保留。来源描述属于启动设置，`validate --against` 会拒绝在
AW 策略更新中修改它们。原生文件监听和动态配置继续由原生 loader 管理。

`native-sources` 输出 `native: declared_not_loaded`、`aw: not_admitted`，仅展示来源
序号和类别，自定义 scope 等字符串也被省略。`show` 继续提供明确的原始值输出。
保存描述不会改变现有 `aw run` 的来源选择；内置 Adapter 仍使用其文档中的原生参数、
环境变量和 settings 选项。使用新 API 的应用须按下节接入原生 loader。

## 接入原生 loader

`aw_adapter::configuration::NativeConfiguration::new(&config, target)?.load(loader)`
将有序引用直接交给调用者的原生 loader。调用者沿用原有 cwd、环境、argv、认证、权限和
选项。loader 负责解析、默认发现、优先级、相对路径、Hook 顺序及动态更新，返回值可以是
存活的配置句柄。原生错误原样传播。此 API 不执行 Hook 来发现能力，也不要求完整工具清单。

loader 返回 `NativeLoad { value, sources }`：原生值或存活句柄，以及按原生优先级排列的
实际已消费来源。应包含隐式发现的来源，省略缺失或被忽略的声明。运行期来源报告使用
Adapter 提供的实际证据，离线 CLI 报告只展示声明。

返回的 `LoadedConfiguration<T>` 保留原生结果，报告 `loaded` / `not_admitted`。
其 `admit(capabilities, provider_evidence)` 方法复用现有 Provider 准入。不可变结果保留
准确的 AW 配置、目标和已准入步骤，包括事件与步骤 selector 的交集。报告为
`admitted_not_active`，激活仍须经过 service 的接点确认和策略代次门禁。消费者将结果与
对应运行代次一起保留；回调、Provider 或桥接故障不得修改该范围。步骤范围外沿用原生
决策，覆盖内失败遵循已准入的错误动作。

必跑的 `native_configuration` 消费者测试分别执行无 AW 和有 AW 的原生 loader，再运行
实际 Host 策略调用。测试覆盖未知字段、来源顺序、动态更新、原生错误、Provider 失败、
编辑冲突和脱敏，证明通用 API 在确定性框架 fixture 中的行为。Qoder SDK、QwenPaw 业务
接入及其现有原生配置限制由后续集成处理。

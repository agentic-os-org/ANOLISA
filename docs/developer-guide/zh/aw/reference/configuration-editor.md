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

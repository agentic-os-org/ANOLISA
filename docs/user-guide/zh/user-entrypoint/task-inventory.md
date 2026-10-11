# 离线基准任务清单

[English](../../en/user-entrypoint/task-inventory.md)

ClawEval 任务清单读取本地基准任务元数据，不启动评测、模型或服务。使用 CSV 输出可在电子表格或其他 CSV 工具中审阅筛选结果。

## 导出任务

准备好 `benchmark/claweval-runner/claw-eval/tasks` 下的任务文件后，使用 Python 3.11 或更新版本运行：

```bash
cd benchmark/claweval-runner
python scripts/list_tasks.py --format csv > tasks.csv
python scripts/list_tasks.py --prefix T --difficulty hard --format csv > hard-tasks.csv
```

前缀可选 `T`、`M`、`C`；难度可选 `simple`、`easy`、`medium`、`hard`、`expert`。CSV 沿用分组视图筛选，按任务目录名称升序排列。任务文件按 UTF-8 读取，原文件保持不变。

## CSV 参考

表头始终包含以下六列，顺序不变：

| 列 | 值 |
| --- | --- |
| `task_id` | 元数据标识，可能与目录名称不同 |
| `task_name` | 元数据中的任务名称 |
| `difficulty` | 已记录的难度；缺少时为 `unknown` |
| `prefix` | 标识的首个字符，转换为大写 |
| `category` | 类别元数据文字 |
| `tags` | 标签元数据文字，例如 `[general, review]` |

缺少名称、类别或标签时保留空单元格。导出沿用既有元数据字段，不会把标签文字转换为规范化列表。

输出采用不含字节序标记的 UTF-8，即使控制台为 ASCII 编码也可保留文字。记录使用标准 CSV 的 CRLF 行尾；字段内的逗号、引号及 CR/LF 换行均会正确加引号，CSV 读取器可恢复原始文字。应按 CSV 解析，不能直接按行或逗号拆分。

空目录或无筛选结果时，输出表头，不输出数据记录。CSV 不混入分组标题、总数或“No tasks found”文字。任务目录错误仍写入标准错误流，并以非零状态退出。

## 分组输出

```bash
python scripts/list_tasks.py
python scripts/list_tasks.py --format grouped
```

两条命令均保留既有分组视图。`--format` 选择一种格式，默认 `grouped`。CSV 导出不增加依赖，也不会执行任务。

环境准备和执行流程详见 [ClawEval runner 概览](../../../../benchmark/claweval-runner/README_zh.md)。

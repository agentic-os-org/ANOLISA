---
name: agentsight-auto-format
description: 编辑代码文件后自动运行对应格式化工具，保持代码风格一致。适用于任何 AI coding agent。
---

# 代码自动格式化

## 目标

编辑 `.rs`、`.py`、`.ts`、`.tsx` 文件后，自动运行对应的格式化工具，保持代码风格一致。

## 触发时自动执行

每次编辑代码文件后（Edit、Write 等操作），对被修改的文件运行对应的格式化命令。格式化失败时静默跳过，不阻断工作流。

### 格式化规则

| 扩展名 | 命令 | 备注 |
|--------|------|------|
| `.rs` | `rustfmt <file>` | 直接调用，不需要 crate 上下文 |
| `.py` | `ruff format <file>` | 没有 ruff 时用 `black --quiet <file>` |
| `.ts` `.tsx` | `prettier --write <file>` | 需要本地已安装 prettier |

### 注意事项

- 只格式化本次编辑的文件，不要全量格式化
- 格式化工具不存在时跳过，不报错
- 不要用 `cargo fmt`（需要 crate 上下文，对单文件不友好）
- 不要用 `npx prettier`（会触发网络下载，有供应链风险）
- 提交前仍需运行 `cargo fmt --check` / `cargo clippy` 做最终检查

## Hook 自动化

本目录下的 `post-edit-format.py` 脚本可挂接到任何支持 PostToolUse hook 的 agent，实现机械式自动格式化。

脚本从 stdin 读取被编辑的文件信息（支持 hook JSON 和纯文本路径），自动分发到对应 formatter。用法：

```bash
echo "/path/to/file.rs" | python3 develop-skills/agentsight-auto-format/post-edit-format.py
```

各 agent 的 hook 配置方式不同，请参考对应 agent 文档将脚本挂在 PostToolUse（Edit/Write）事件上。hook 配置属于开发者本地设置，不提交仓库。

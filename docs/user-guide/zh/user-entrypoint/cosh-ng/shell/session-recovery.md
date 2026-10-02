# 会话恢复

[English](../../../../en/user-entrypoint/cosh-ng/shell/session-recovery.md)

使用cosh-core适配器时，`cosh`可以恢复当前工作空间保存的Agent对话。恢复会还原模型可用的消息，不会还原终端进程、旧终端输出、审批卡片或其他临时UI状态。

## 恢复会话

打开选择器或直接选择已知的会话UUID：

```bash
cosh --resume
cosh --resume 2d711642-b726-4b04-8d2a-8a0470f4ed24
```

也可以在提示符中管理会话：

| 命令 | 用途 |
|---|---|
| `/session` | 打开当前工作空间的会话选择器。 |
| `/session list` | 列出包含完整会话UUID的有界分页。 |
| `/session list --all` | 列出同一保存根目录下所有工作空间的会话。 |
| `/session resume <id>` | 按UUID选择一个会话。 |
| `/session new`（`/new`） | 开始新的Agent对话，不删除旧记录。 |
| `/session status` | 查看已选择和当前活跃的会话状态。 |
| `/session clear <id>...` | 确认后清理指定会话。 |
| `/session clear --all` | 确认后清理所有可清理会话。 |

选择会话不会调用模型，恢复从下一次Agent请求开始。如果恢复失败，Shell仍可使用；刷新列表后重试，或开始新会话。

## 工作空间和安全边界

- 会话属于创建它的规范化工作空间。`/session list --all`可以显示其他工作空间的会话，但`resume`会拒绝作用域不匹配的会话，也不会改变工作目录。
- 只有健康且属于当前工作空间的条目可以恢复。损坏或不兼容的条目仍可在确认后识别并清理。
- 清理始终确认准确的ID或数量。已选择会话和当前Provider会话受到保护，`clear --all`也会跳过它们。
- 默认保存根目录为`~/.copilot-shell/cosh-core/sessions/`。设置`session.persist_dir`可修改它；设置`session.auto_persist = false`可让会话只保留到当前`cosh`进程结束。

在选择器中，使用`Up`/`Down`或`j`/`k`移动，按`Enter`恢复，按`Space`标记条目，按`d`后再按`y`确认清理，按`Esc`或`Ctrl+C`取消。

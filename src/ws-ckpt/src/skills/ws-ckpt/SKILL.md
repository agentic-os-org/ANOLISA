---
name: ws-ckpt
description: >
  工作区快照管理。用户说"保存一下"、"存个快照"时创建 checkpoint，仅限 Linux;
  说"回滚"、"撤销"、"恢复到之前"时 rollback;说"删掉快照"时 delete;
  说"对比快照"、"快照改了什么"时 diff;
  说"看看快照"、"有哪些快照"时 list;说"查看快照状态"、"查看快照剩余空间"时 status。
---
# ws-ckpt 工作区快照管理

基于 btrfs COW 快照，为任意工作区提供 checkpoint/rollback；实际耗时取决于文件系统、工作负载和机器环境。

## 工作区路径（关键 — 必须遵守）

⚠️ **绝对禁止猜测或推断工作区路径。**

工作区级操作（checkpoint、rollback、diff、cleanup，以及按工作区执行的 delete/list/status）需要
`-w <workspace>`。执行这些操作前，必须按以下顺序确定路径：

1. 用户在**当前消息中明确给出**了路径 → 直接使用
2. 否则 → **必须向用户询问**："请提供工作区路径（传给 `-w` 的目录）"，拿到回复后再执行

不得从环境变量、默认路径、或任何隐含上下文中猜测。确定后，本次会话内复用同一个路径。

以下全局查询/操作允许省略 `-w`，不要为此追问路径：

- `list`：列出所有已注册工作区的快照；
- `status`：查看 daemon 全局状态；
- 快照 ID 全局唯一时的 `delete`。若 ID 有歧义，再要求用户提供 `-w`。

cwd 占用的拦截由 daemon 层统一处理,skill 不再做前置守卫。

## 触发规则

| 用户说 | 执行命令 | 说明 |
|--------|----------|------|
| "保存一下"、"存个快照"、"checkpoint"、"备份当前状态" | `checkpoint` | 创建快照 |
| "回滚"、"撤销"、"恢复到之前"、"rollback"、"改坏了" | `rollback` | 回滚到指定快照 |
| "对比快照"、"快照差异"、"diff"、"改了什么" | `diff` | 查看两个快照间的文件变更 |
| "删掉快照"、"清理快照"、"delete snapshot" | `delete` | 删除指定快照 |
| "看看快照"、"有哪些快照"、"list"、"列一下" | `list` | 列出快照 |
| "状态"、"空间"、"status"、"工作区怎么样" | `status` | 查看工作区状态 |

## 命令用法

### checkpoint — 创建快照

```bash
ws-ckpt checkpoint -w <workspace> [-s <snapshot-id>] [-m <message>] [--metadata <json>]
```

- `-w`:工作区路径(必填)
- `-s`:快照 ID(可选，省略时自动生成）
- `-m`:快照描述(可选)
- `--metadata`:附加 JSON 元数据(可选)

```bash
ws-ckpt checkpoint -w <path-to-workspace> -s before-refactor -m "重构前备份"
ws-ckpt checkpoint -w <path-to-workspace>  # ID 自动生成
```

### rollback — 回滚到快照

```bash
ws-ckpt rollback -w <workspace> [-s <snapshot> | -n <steps>] [--preview]
```

- `-w`:工作区路径(必填)
- `-s`:目标快照 ID（与 `-n` 互斥）
- `-n`:沿父链回滚 N 步（与 `-s` 互斥）
- `--preview`:预览文件变更，不实际执行回滚

```bash
ws-ckpt rollback -w <path-to-workspace> -s before-refactor
ws-ckpt rollback -w <path-to-workspace> -n 1          # 回滚到上一个快照
ws-ckpt rollback -w <path-to-workspace> -s snap1 --preview  # 仅预览
```

### diff — 查看快照间差异

```bash
ws-ckpt diff -w <workspace> -f <from-snapshot> [-t <to-snapshot>]
```

- `-w`:工作区路径(必填)
- `-f`:源快照 ID(必填)
- `-t`:目标快照 ID(可选，省略时与当前工作区状态比较)

```bash
# 两个快照之间的差异
ws-ckpt diff -w <path-to-workspace> -f before-refactor -t after-refactor

# 快照与当前工作区的差异
ws-ckpt diff -w <path-to-workspace> -f before-refactor
```

### delete — 删除快照

```bash
ws-ckpt delete -s <snapshot> --force [-w <workspace>]
```

- `-s`:要删除的快照 ID(必填)
- `--force`:跳过确认，agent执行必须要求跳过确认
- `-w`:快照 ID 跨工作区重复时必须指定

```bash
ws-ckpt delete -s old-snap --force
```

### list — 列出快照

```bash
ws-ckpt list [-w <workspace>] [--format table|json] [--limit N] [--cursor TOKEN]
```

- 省略 `-w` 列出所有工作区的快照

```bash
ws-ckpt list
ws-ckpt list -w <path-to-workspace>
ws-ckpt list --format json
```

默认命令会自动读取全部游标页，JSON 仍输出单个数组。仅当用户明确要求分页时使用
`--limit N`；响应中的 `next_cursor` 可通过 `--cursor '<token>'` 继续读取。游标是不透明
且绑定查询范围的，不要解析或修改。若记录包含 `detail: "summary"` / `omitted_fields`，
必须明确告知用户该记录因超大 `message`/`metadata` 只返回摘要，但快照 ID 仍可用于
rollback、diff、delete 等操作。摘要保留的 `created_at`、`pinned`、`missing` 仍在
`meta` 下；被省略字段缺席，不应当作原本为空。

### status — 查看状态

```bash
ws-ckpt status [-w <workspace>]
```

- 省略 `-w` 查看全局状态

```bash
ws-ckpt status
ws-ckpt status -w <path-to-workspace>
```

### config — 查看或修改自动清理策略

`ws-ckpt config` 的作用域由 scope 决定:不带 scope = 只读 overview(全局配置 + workspace 覆盖统计,带任何修改 flag 会报错);`-g` 全局;`-w <workspace>` 局部。

```bash
# 全局(daemon-wide)
ws-ckpt config -g                                 # 查看
ws-ckpt config -g --enable-auto-cleanup
ws-ckpt config -g --auto-cleanup-keep 30d         # 或整数 20

# 局部(只覆盖本 workspace 的 auto_cleanup / auto_cleanup_keep)
ws-ckpt config -w <path-to-workspace>             # 三栏视图: effective / local / global
ws-ckpt config -w <path-to-workspace> --auto-cleanup-keep 5
ws-ckpt config -w <path-to-workspace> --disable-auto-cleanup
ws-ckpt config -w <path-to-workspace> --reset     # 删除 policy.toml,沿用全局
```

`-w` 仅可覆盖 `auto_cleanup` 和 `auto_cleanup_keep`,interval/image/health-check 等是 daemon-wide,带 `-w` 设置会被 CLI 拒绝。

## 注意事项

- checkpoint 用 `-s` 指定快照 ID（可省略，自动生成）；rollback 和 delete 也用 `-s` 指定快照 ID
- daemon 必须运行中(`systemctl status ws-ckpt` 确认),否则所有命令报连接错误
- 执行破坏性操作前务必先 checkpoint

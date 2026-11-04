---
name: ws-ckpt
description: >
  工作区快照管理。用户说"保存一下"、"存个快照"、"留个底"时创建 checkpoint，仅限 Linux;
  说"回滚"、"撤销"、"恢复到之前"、"刚才那版改回去"时 rollback;说"删掉快照"时 delete;
  说"对比快照"、"快照改了什么"时 diff;
  说"看看快照"、"有哪些快照"时 list;说"查看快照状态"、"查看快照剩余空间"时 status;
  说"自动保存"、"每次改完留快照"、"定时快照"、"每小时备份一次工作区"时按"会话自动快照"
  和"定时快照（crontab）"两节操作。用户明确要求使用 git（如"用 git 存一下"、"git stash"）
  时不使用本 skill，尊重指定工具。
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
| "保存一下"、"存个快照"、"留个底"、"checkpoint"、"备份当前状态" | `checkpoint` | 创建快照 |
| "回滚"、"撤销"、"恢复到之前"、"刚才那版改回去"、"rollback"、"改坏了" | `rollback` | 回滚到指定快照 |
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

## 会话自动快照（仅 CLI + skill，未安装插件时）

本节是**会话内的行为约定**，不依赖插件 hook，只在当前会话生效；跨会话/无人值守的定时备份见下一节。已安装 OpenClaw 插件时**不要**使用本节——插件的 `autoCheckpoint` 已在每回合结束自动建快照，两套并存会产生重复快照，此时经 `ws-ckpt-config` 工具管理。

### 开启流程

1. 建立一次**初始基线**快照：`ws-ckpt checkpoint -w <workspace> -s auto-baseline -m "自动快照基线"`（`-s` 前缀用 `auto-` 便于辨识）。
2. 之后每个回合结束时判断：本回合**是否实际修改了工作区文件**（写、删、移动、重命名都算；纯阅读、回答问题、执行只读命令不算）。
   - 修改了 → 在回复用户前建一个回合快照：`ws-ckpt checkpoint -w <workspace> -s auto-turn-<序号> -m "<一句话概括本回合改动>" --metadata '{"auto":true,"type":"turn_end"}'`。**一个回合无论改了多少文件只建一个**。
   - 没修改 → 不建，也不必向用户提及。
3. 真实执行了 rollback 的回合**不再建回合快照**：rollback 本身已把 head 移到目标快照。

### 撤销语义（重要，防止回错版本）

- 回合快照保存的是**改动后**的状态。因此"撤销这一回合"应当回到它的**父快照**（回合开始前的状态），不是最新快照本身。
- `-n 1` 沿父链解析到的是 **head 本身**（即最近一次快照，通常就是刚建好的回合快照），用它回滚等于撤销"快照之后"对 live 工作区的改动；**撤销最近一个已快照的回合要用 `-n 2`**（head 的父）。
- 拿不准目标时先 `ws-ckpt list -w <workspace> --limit 5` 确认父链，再用 `--preview` 预览，向用户复述将要恢复到的快照及其 message 后再执行。
- 回滚目标不明确（用户没说是哪个"之前"）或影响面超出用户要求（会丢掉多个回合的改动）时，先确认再执行。

## 定时快照（crontab，仅 CLI + skill 时）

用户要"定时快照"、"每小时备份一次工作区"等周期性备份且未安装插件时，引导用用户的 crontab 调 CLI：

```bash
# 添加：每小时整点快照一次（% 必须转义为 \%）
crontab -l 2>/dev/null; echo '0 * * * * /usr/local/bin/ws-ckpt checkpoint -w <workspace> -s "cron-$(date +\%s)" -m "scheduled snapshot" --metadata "{\"auto\":true,\"type\":\"cron\"}" >/dev/null 2>&1' | crontab -
```

- **增/删/关闭只动本工作区的行**：编辑 crontab 时以"是否为该 `-w` 路径的 ws-ckpt 行"为过滤条件，其他工作区和无关任务的原样保留；关闭 = 删除该工作区的行。
- 展示修改前后各一行 diff 征得用户确认后再写入。
- 保留策略（旧快照自动清理）用 `ws-ckpt config -w <workspace> --auto-cleanup-keep <N|30d>` 配置，**不要**默认改 `config -g` 的 daemon 全局配置。
- cron 表达式表达不了的调度意图（如"工作日下班时"）不要降级成近似表达式，给出最接近的选项等用户确认。

⚠️ **插件共存**：插件的 cron 管理会**整行替换**同一工作区的所有 `ws-ckpt checkpoint -w <path>` 定时行。用户环境里装有插件时，cron 一律经 `ws-ckpt-config`（key `cronSchedules`，`add/remove/set`）管理，不要手工写 crontab，否则两边互相覆盖。

## 口语备份与恢复的优先级

- "留个底"、"存一下当前状态"、"刚才那版改回去"等未指定工具的请求 → 用 ws-ckpt 承接。
- 用户明确说了 git（`git stash`、`git commit`、`git status` 等）→ 走 git，不抢。
- 工作区**尚未注册**时，首次 checkpoint 会透明 auto-init，有必须先告知的副作用：目录数据被 rsync 进 btrfs 子卷，原目录被**替换为指向子卷的 symlink**（路径字符串不变，但 `ls -la` 会看到它是链接；`recover` 可撤销）。说明并征得同意后再执行。
- "撤销上一步"恢复到最近修改**前**的父快照（见撤销语义），不回到该修改后的快照，也不为此补建回合快照。

## 注意事项

- checkpoint 用 `-s` 指定快照 ID（可省略，自动生成）；rollback 和 delete 也用 `-s` 指定快照 ID
- daemon 必须运行中(`systemctl status ws-ckpt` 确认),否则所有命令报连接错误
- 执行破坏性操作前务必先 checkpoint
- 会话自动快照是行为约定不是强制兜底：会话中断、上下文压缩后约定可能失效；需要强制保证时用插件的 hook 自动快照或 crontab

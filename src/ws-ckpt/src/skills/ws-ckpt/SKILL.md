---
name: ws-ckpt
description: >
  Linux 工作区快照(ws-ckpt)。用户想保存或备份工作区当前状态("保存一下"、"留个底"、"先备份"),
  想撤销或恢复文件改动("回滚"、"撤销"、"改回去"、"恢复到之前"、"刚才那版"),
  想查看、对比或删除历史版本,或要开启/关闭每回合自动快照、定时快照、快照保留策略时使用。
  用户没提"快照"、只表达保存或恢复工作区文件的意图时同样适用;用户点名 git 操作(commit、branch、reset 到某提交)时不适用。
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

## 用户没点名工具时

用户只说"留个底"、"改回去"这类话、没指定用什么工具时,优先用 ws-ckpt,并在回复里说明用的是 ws-ckpt 快照:

- **保存**:工作区已注册 → 直接 checkpoint。未注册 → 告诉用户 ws-ckpt 可以为该目录建快照,说明 `init` 会把目录迁到 btrfs 子卷(原路径变为指向子卷的软链),征得同意后再 init + checkpoint。
- **恢复**:先 `ws-ckpt list -w <workspace>`,按时间和 message 找到用户说的那一版;目标不明确时询问,不要猜测。用 `rollback --preview` 展示影响范围;回滚作用于整个工作区,预览包含用户没要求撤销的改动时先确认,再执行 rollback。找不到对应快照时如实说明,再考虑 git 等其他手段,并建议开启[自动快照](#自动快照)。
- 用户点名 git 时用 git。ws-ckpt 快照可保存未跟踪、未提交的文件状态,不要求先 git add/commit;git commit 只保存暂存区内容。

## 触发规则

| 用户说 | 执行命令 | 说明 |
|--------|----------|------|
| "保存一下"、"存个快照"、"checkpoint"、"备份当前状态" | `checkpoint` | 创建快照 |
| "回滚"、"撤销"、"恢复到之前"、"rollback"、"改坏了" | `rollback` | 回滚到指定快照 |
| "对比快照"、"快照差异"、"diff"、"改了什么" | `diff` | 查看两个快照间的文件变更 |
| "删掉快照"、"清理快照"、"delete snapshot" | `delete` | 删除指定快照 |
| "看看快照"、"有哪些快照"、"list"、"列一下" | `list` | 列出快照 |
| "状态"、"空间"、"status"、"工作区怎么样" | `status` | 查看工作区状态 |
| "开启自动快照"、"关闭自动快照" | 见[自动快照](#自动快照) | 每个改动回合结束时建快照 |
| "定时快照"、"每小时存一次"、"关掉定时快照" | 见[定时快照](#定时快照) | 管理 crontab |
| "保留最近 N 个"、"快照保留 7 天" | `config -w` | 见[config](#config--查看或修改自动清理策略) |

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
- `-n`:沿当前 HEAD 的父链选择第 N 项（1 = HEAD 本身,2 = HEAD 的父节点;与 `-s` 互斥）
- `--preview`:预览文件变更，不实际执行回滚

```bash
ws-ckpt rollback -w <path-to-workspace> -s before-refactor
ws-ckpt rollback -w <path-to-workspace> -n 1          # 恢复当前 HEAD 快照,丢弃之后未快照的改动
ws-ckpt rollback -w <path-to-workspace> -s snap1 --preview  # 仅预览
```

### diff — 查看快照间差异

```bash
ws-ckpt diff -w <workspace> -f <from-snapshot> [-t <to-snapshot>] [--format text|json]
```

- `-w`:工作区路径(必填)
- `-f`:源快照 ID(必填)
- `-t`:目标快照 ID(可选，省略时与当前工作区状态比较)
- `--format`:输出格式，默认 `text`；`json` 输出文件级差异数组

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

用户没说"全局"时一律用 `-w`,只改当前工作区:"保留最近 N 个" → `--enable-auto-cleanup --auto-cleanup-keep N`;"保留 7 天" → `--enable-auto-cleanup --auto-cleanup-keep 7d`;"恢复默认" → `--reset`。

## 自动快照

默认关闭。用户说"开启自动快照"后在本会话内持续生效,直到用户说"关闭自动快照"。已启用 ws-ckpt 插件时,开关改用插件的 `ws-ckpt-config`（`autoCheckpoint`）,不再同时执行下面的 skill 自动快照流程,避免重复建快照或关闭不生效。

开启时先按上文规则确定工作区路径,用只读查询核对注册状态(不会自动 init)。本节命令中的 `<shell-quoted-workspace>` 与 turn_end 命令相同,按下文的参数数组或引用方法传入:

```bash
ws-ckpt status -w <shell-quoted-workspace>
```

- 成功且输出该工作区信息 → 已注册,即使快照数为 0。
- 明确返回 `Error [WorkspaceNotFound]` → 未注册;不要仅凭退出码 1 或 list 为空判断。
- 连接失败、注册异常或其他错误 → 停止并报告,不要继续 init 或 checkpoint。

未注册时先说明 `init` 会迁移目录并把原路径变为软链,首次 `checkpoint` 也会自动 init;征得同意后再执行下面的命令,拒绝或 init 失败则不继续开启。

```bash
ws-ckpt init -w <shell-quoted-workspace>   # 已注册时无副作用
ws-ckpt checkpoint -w <shell-quoted-workspace> -m "session start" --metadata '{"auto":true,"type":"initial"}'
```

基线成功后才告知已开启;失败时告知尚未开启,不要让用户误以为已有保护快照。

初始快照只在开启时建立一次,用于恢复到改动前。之后持续覆盖本会话,不需要用户每轮提醒。**回复用户之前**,按本回合实际发生的文件修改判断,不要仅凭请求里是否有“修改”字样判断:

| 本回合 | 动作 |
|--------|------|
| 实际新建/修改/删除了工作区内文件(含命令产生的文件变更) | 建一个快照,见下方命令 |
| 只读(查看、搜索、问答、list/diff/status),或尝试修改但未产生文件变更 | 不建 |
| 执行过真实 rollback(非 `--preview`) | 不建,保持回滚后的父链 |

message 取本回合请求的前 80 字。执行工具支持参数数组时,把本会话已确定的工作区路径和 message 原文作为独立参数传入,不额外添加 shell 引号。

通过 shell 执行时,下面两个占位符这样生成:

- `<shell-quoted-workspace>`:对本会话已确定的 `<workspace>` 路径做 shell 引用,不是另一个路径,不需要重新查询。
- `<shell-quoted-message>`:对截断后的 message 做同样的 shell 引用。

引用方法:先把内容中的每个单引号 `'` 替换为 `'"'"'`,再在整体两端加单引号;也可用 `shlex.quote` 生成。例如 `/mnt/my ws` 得到 `'/mnt/my ws'`,`O'Reilly` 得到 `'O'"'"'Reilly'`。将引用结果直接替换对应占位符,不要再加一层双引号或用 eval 再次求值。无法安全传入 message 时改用固定值 `'turn_end snapshot'`,并在回复中说明降级。

```bash
ws-ckpt checkpoint -w <shell-quoted-workspace> -m <shell-quoted-message> --metadata '{"auto":true,"type":"turn_end"}'
```

turn_end 快照保存该回合改动后的状态,message 通常是该回合请求(降级时为 `turn_end snapshot`)。撤销某回合应选该快照的 `parent_id`,不是按 message 选该快照本身。`-n 1` 指当前 HEAD;HEAD 正是最近回合的 turn_end 快照时,撤销该回合用 `-n 2`。若先建保护快照,先确定目标 ID 再用 `-s` 回滚,避免新增快照改变 `-n` 的含义。

- 每回合最多建一个,不要每改一个文件建一次;用户要中间点时再按 checkpoint 规则显式建。
- turn_end 快照失败不中断任务,但必须在回复中明确告知本回合改动未保存为快照及错误原因,不要宣称已保护。
- 开启状态不跨会话保留。用户要求长期开启时,建议用户把"对 <workspace> 开启 ws-ckpt 自动快照"写进 agent 的全局或项目指令文件。

## 定时快照

通过当前用户的 crontab 定时执行 checkpoint,与 ws-ckpt 插件写入的行格式一致:

```
<cron 表达式> <ws-ckpt 绝对路径> checkpoint -w '<workspace>' -s "cron-$(date +\%s)" -m "scheduled snapshot" --metadata '{"auto":true,"type":"cron"}' >/dev/null 2>&1
```

- 只用标准 5 段 cron 表达式。用户意图无法精确表达(如"每 90 分钟")时不要写近似值,给出最接近的选项,等用户确认。
- 工作区路径含单引号或换行时不写 crontab,告知用户原因。
- 已启用 ws-ckpt 插件时改用插件的 `ws-ckpt-config` 工具,否则插件会在会话开始时按自身配置覆盖这些行。

查看本工作区的定时快照:

```bash
crontab -l 2>/dev/null | grep -F -e "checkpoint -w '<workspace>' " -e "checkpoint -w <workspace> "
```

修改时先单独执行 `crontab -l`:失败且报错不是 `no crontab for` 时停止并报告,不要写回。然后用下面的模板把本工作区的**完整**表达式列表整体写回(增加 = 原列表加一项;删除 = 去掉一项),其他 crontab 行原样保留。关闭时移除全部表达式,写成 `for expr in; do`,保留循环体、过滤和写回步骤;空列表使循环执行零次,只移除本工作区的旧行:

```bash
ws='<workspace>'; bin=$(command -v ws-ckpt) || exit 1
{
  crontab -l 2>/dev/null | grep -vF -e "checkpoint -w '$ws' " -e "checkpoint -w $ws "
  for expr in '0 * * * *'; do   # 本工作区最终保留的全部表达式
    echo "$expr $bin checkpoint -w '$ws' -s \"cron-\$(date +\\%s)\" -m \"scheduled snapshot\" --metadata '{\"auto\":true,\"type\":\"cron\"}' >/dev/null 2>&1"
  done
} | crontab -
crontab -l | grep -F -e "checkpoint -w '$ws' " -e "checkpoint -w $ws "   # 核对写入结果
```

关闭后先确认 `crontab -l` 成功,再确认无本工作区匹配行(此时 grep 返回 1 是预期结果),才告知已关闭。

定时快照依赖 crond 运行(`systemctl is-active crond`)。

## 注意事项

- checkpoint 用 `-s` 指定快照 ID（可省略，自动生成）；rollback 和 delete 也用 `-s` 指定快照 ID
- daemon 必须运行中(`systemctl status ws-ckpt` 确认),否则所有命令报连接错误
- 执行破坏性操作前务必先 checkpoint;已开启自动快照且上次快照后没有新改动时不必重复建

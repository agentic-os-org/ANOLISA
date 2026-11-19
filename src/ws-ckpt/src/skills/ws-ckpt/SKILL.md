---
name: ws-ckpt
description: >
  Linux 工作区快照(ws-ckpt)。用户想保存或备份工作区当前状态("保存一下"、"留个底"、"先备份"),
  想撤销或恢复文件改动("回滚"、"撤销"、"改回去"、"恢复到之前"、"刚才那版"),
  想查看、对比或删除历史版本,或要开启/关闭每回合自动快照、定时快照、快照保留策略时使用。
  用户要求为 AW 自动快照绑定或切换工作区时也使用。
  当前会话可调用原生 ws-ckpt-config 时，直接用原生工具查询或修改插件的自动快照。
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
| "自动快照开了吗"、"开启自动快照"、"关闭自动快照" | 见[自动快照](#自动快照) | 查询或修改当前模式的开关 |
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

用户没说"全局"时一律用 `-w`,只改当前工作区:"保留最近 N 个" → `--enable-auto-cleanup --auto-cleanup-keep N`;"保留 7 天" → `--enable-auto-cleanup --auto-cleanup-keep 7d`;"恢复默认" → `--reset`。

## 自动快照

默认关闭。只按本轮用户消息确定动作。同一个工作区只启用一套自动快照。

| 用户本轮说什么 | 本轮动作 |
|---|---|
| 「自动快照开了吗？」 | 查询并报告实际状态，随后结束。本轮只查状态，即使上轮开启失败也保持实际状态。 |
| 「请开启自动快照」「请关闭自动快照」 | 选择执行方式，完成用户要求的开关操作。 |
| 「把 AW 自动快照的工作区切换到 `/work/demo`」 | 仅按本轮明确指定的绝对路径切换绑定。缺少路径时向用户询问。 |
| 「怎么开启自动快照？」 | 说明步骤。 |
| 意图不明确 | 问用户是要查询还是修改。 |

### 选择执行方式

**先检查用户是否明确指定执行方式。** 有明确选择时按本轮选择处理。其余情况下，本会话已选定模型模式时，查询或关闭该模式直接走 [模型模式的会话操作](#会话操作)，即使已经关闭也一样。当前会话可调用原生 `ws-ckpt-config` 时，直接使用原生工具。

需要选择执行方式时，默认先用 Shell 工具运行无参数的路由脚本。从当前加载的 `SKILL.md` 所在目录定位脚本，将 `/absolute/path/to/skill` 替换为该目录的绝对路径。

```sh
sh '/absolute/path/to/skill/scripts/auto-snapshot-route.sh'
```

查询、开启或关闭需要选路时，每轮只运行一次脚本，按本轮输出执行。上次探测缺插件且当前原生工具仍不可用时，本轮重新探测。

没有 AW 会话标记或原生插件路由的普通会话默认走模型模式，即使本机已安装 AW。

- **用户明确选择 AW**（例如「用 AW 开启」）或要求绑定、切换 AW 工作区时，给脚本附加 `--aw`。绑定或切换收到 `aw_bound` 才执行。
- **用户明确选择模型执行**（例如「由你直接执行，不用 AW」）且本轮需要选路时，给脚本附加 `--model`。
- **OpenClaw 或 Hermes** 原生工具不可用时，默认用无参数命令识别当前绑定。用户明确选择原生插件，或已知当前是未通过 AW 接入的对应宿主时，用 `--openclaw` 或 `--hermes`。继承的宿主环境变量不足以覆盖 AW 绑定。

| 脚本输出 | 本轮动作 |
|---|---|
| `openclaw_plugin_present` 或 `hermes_plugin_present` | 使用可用的原生 `ws-ckpt-config`。工具不可用时，报告插件已安装、当前会话无法调用，并结束本次操作。 |
| `openclaw_plugin_missing` | 告知用户当前缺少 OpenClaw 原生插件，并给出 `ws-ckpt plugin install --runtime openclaw`。即使只查状态或请求关闭，也给出完整命令并结束本次操作。询问如何接入时给出同一命令。 |
| `hermes_plugin_missing` | 查询、开启或关闭时，回复「当前 Hermes 会话缺少 ws-ckpt 原生插件。安装命令为 `ws-ckpt plugin install --runtime hermes`。」然后结束本次操作。询问如何接入时给出同一命令。 |
| `aw_bound` | 使用下方 AW 操作脚本。 |
| `aw_setup` | 说明当前会话未接入 AW，给出下方启动方法。尚未安装 AW 时提示先安装，随后结束本轮。接入提示无需用户先提供工作区路径。 |
| `aw_unavailable` | 报告当前 ws-ckpt Provider 会话绑定不可用，提示检查下方会话环境，结束本轮。 |
| `model` | 使用下方模型流程。 |

脚本缺失、执行失败或输出无法识别时，报告路由检查失败并结束本轮。

### 通过 AW

从工作区之外执行 `aw run --config aw.yaml --agent <目标>`。如果 `aw.yaml` 的 ws-ckpt Provider 配置了该目标的 Skill 目录，首次 `input.submit` 会安装缺失的 Skill。Agent 需要在发现 Skill 后才能在会话中使用它。

在目标的启动环境中配置 `WS_CKPT_AW_TARGET`（AW 目标名）和 `WS_CKPT_AW_PROVIDER`（Provider 可执行文件绝对路径），供 Skill 绑定工作区和管理开关。绑定后报告目标、工作区以及该工作区已有的开关状态。绑定只选择工作区，已有开关按工作区和目标分别保存。下次提交和开关操作会读取新绑定。

按本轮意图选择下面**一条**命令。把路径替换为当前加载的 Skill 目录绝对路径。用户明确指定工作区时，在同一条命令追加 `--workspace '/用户指定的绝对路径'`。开启前，脚本用 `ws-ckpt status` 检查工作区是否已注册。

| 本轮动作 | Shell 命令 |
|---|---|
| 查询 | `python3 '/absolute/path/to/skill/scripts/auto-snapshot-aw.py' status` |
| 绑定或切换 | `python3 '/absolute/path/to/skill/scripts/auto-snapshot-aw.py' bind --workspace '/用户指定的绝对路径'` |
| 开启 | `python3 '/absolute/path/to/skill/scripts/auto-snapshot-aw.py' enable` |
| 关闭 | `python3 '/absolute/path/to/skill/scripts/auto-snapshot-aw.py' disable` |

脚本负责核对绑定和写后回查。**成功后按返回的 `workspace`、`target` 和 `enabled` 直接回复，本轮不要再调用 `status`。** 按需说明 `last_result`（`null` 表示尚无结果）。若回查显示开关已达到目标，但返回 `persistence_confirmed: false`，告知当前状态及持久化尚未确认。其他错误按脚本给出的实际状态回复，状态未知时明确说明。

日常开关无需修改 `aw.yaml` 或重启 Agent。Provider 在 `input.submit` 时读取开关，并在打点前检查 Agent 的实际启动目录。开关已启用时，查状态或关闭的消息也可能先触发当次提交快照。如果 `last_result` 为 `checkpoint_cwd_inside_workspace`，说明开关已启用时有一次打点因 Agent 从工作区内启动而跳过，提示从工作区之外重启后可恢复打点。

### 由模型执行自动快照

#### 会话操作

在会话中记住选定的执行方式、已确认的工作区、开关状态和首次提示是否已发送。基线成功才将开关设为开启；关闭后保留执行方式和工作区。

- **查询模型模式**时，根据会话状态只回复对应的一句「本会话的模型自动快照已开启。」或「本会话的模型自动快照已关闭。」回复到此结束。已选定模型模式后，查询无需工具调用。
- **关闭模型模式**时，在会话内设为关闭，直接告知用户并停止轮末打点。无需任何工具操作。已有快照仍可查看和回滚。

#### 开启

1. 开启前确定工作区，可复用本会话已确认的路径。本会话尚未发过首次提示时，在首次写入前单独发送「自动快照依赖模型遵循 Skill 指令，可能漏拍。」发送后立即记为已提示，后续重试或重开直接跳过这条提示。如果只能在最终回复里展示提示，先发送提示并结束本轮，等待用户确认后再写入。
2. 执行 `ws-ckpt status -w <shell-quoted-workspace>`。成功表示已注册，直接执行第 3 步。只有明确返回 `Error [WorkspaceNotFound]` 时，才说明目录迁移及软链接影响，等用户确认后执行 `ws-ckpt init -w <shell-quoted-workspace>`。用户拒绝、初始化失败或查询遇到其他错误时结束本轮，保持关闭。
3. 建立初始基线。成功后记录工作区和已开启状态，按这个格式发出最终回复并结束「本会话的模型自动快照已开启。工作区为 `<工作区>`。基线快照为 `<快照 ID>`。」最终回复只报告这三项。失败时报告尚未开启及原因。

```sh
ws-ckpt checkpoint -w <shell-quoted-workspace> -m "session start" --metadata '{"auto":true,"type":"initial"}'
```

#### 轮末打点

模型模式开启期间，回复前根据本轮实际发生的文件变更决定是否打点。

| 本轮结果 | 动作 |
|---|---|
| 实际新建、修改或删除了工作区内文件 | 创建一次轮末快照。 |
| 只读或未产生文件变更 | 跳过。 |
| 执行过真实 rollback | 跳过，保留回滚后的父链。 |

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

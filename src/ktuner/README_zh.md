# ktuner — 确定性内核调优引擎

[English](README.md) | **中文**

面向 AI agent 的内核参数调优引擎，属于 ANOLISA 的一部分。ktuner 针对运行中的系统评估 207 条规则，输出结构化 JSON 调优建议。设计上由 cosh/agent 通过 `ktuner <command> [options]` 调用。

磁盘 `rq_affinity` 建议会考虑 `threadirqs` 启动参数和 PREEMPT_RT。引号内的参数值不会引入独立 flag 或 `--` 分隔符；实际分隔符后的参数属于 init。

## 用法

```bash
# 诊断 — 输出评分和建议，附带主机信息
ktuner check
ktuner check --category net
ktuner check --category net --category mem   # 可重复：取并集
ktuner check --conservative    # 仅高置信度
ktuner check --only vm.swappiness   # 只报告点名的参数
ktuner check --exclude vm.dirty_ratio   # 报告其余全部、去掉这一项

# 应用建议（需要 root 权限）
sudo ktuner tune --dry-run     # 预览，不做实际变更
sudo ktuner tune               # 全部应用
sudo ktuner tune --conservative
sudo ktuner tune --category net --category mem   # 只应用这些分类
sudo ktuner tune --exclude vm.dirty_ratio   # 应用其余全部、跳过这一项
sudo ktuner tune --only vm.swappiness   # 只应用点名的参数

# 修正单个参数（需要 root 权限）
sudo ktuner fix <param>        # 例如 sudo ktuner fix vm.swappiness
sudo ktuner fix <param> --dry-run   # 预览单个参数，不做实际变更

# 解释一个或多个参数为何需要修改（一次运行共用一份诊断）
ktuner why <param> [<param>…]  # 例如 ktuner why net.core.somaxconn vm.swappiness

# 回滚变更（需要 root 权限）
sudo ktuner rollback          # 破坏性且终结（删除 ledger）
sudo ktuner rollback --list   # 只读预览回滚将恢复的内容
sudo ktuner rollback --list <param> [<param>…]  # 只预览回滚这些参数将恢复的内容
sudo ktuner rollback <param> [<param>…]  # 回滚点名的已记录参数，例如 vm.dirty_bytes
sudo ktuner rollback --exclude vm.swappiness   # 恢复其余全部已记录条目、跳过这一项
sudo ktuner rollback --list --exclude vm.swappiness   # 预览同一集合
sudo ktuner rollback --category net   # 只恢复账本中该分类的条目
sudo ktuner rollback --list --category net   # 预览同一集合
```

## JSON 输出

所有输出以 **JSON 格式写入 stdout**。错误以 **JSON 格式写入 stderr**。stdout 不包含 ANSI 颜色、进度条或人类可读的格式化文本。

对象键按字母顺序输出。读取字段时应使用键名，不应依赖字段顺序。

### 退出码

| 退出码 | 含义 |
|--------|------|
| 0      | 成功（check：系统已最优；tune/fix/rollback：已应用） |
| 1      | check：存在调优建议（非错误，表示系统可改善）；tune：存在建议但当前环境均不可应用（status "blocked"，如容器内只读 /proc/sys）；rollback：恢复未完成 |
| 2      | 错误（详情见 stderr JSON） |

`rollback` 在所有记录值恢复完成时返回 `0`（空记录为成功的无操作），
任一值恢复失败、路径缺失或清理未能完成（持久化配置文件或回滚记录本身未能删除）时返回 `1`，
记录读取失败等命令错误返回 `2`。
未完成的恢复仍在 stdout 输出 JSON 计数，并保留回滚记录以便重试；未能删除的持久化文件同样计入失败，
因为它会在下次启动时重新写入调优值；回滚记录本身未能删除时，`rollback --list` 会继续列出这些已恢复的条目。

### check 输出

```json
{
  "counts": {
    "cpu": 1,
    "high_confidence": 11,
    "io": 3,
    "memory": 8,
    "network": 35,
    "performance": 47,
    "security": 8,
    "writable": 55
  },
  "environment": "物理机/虚拟机",
  "predicted_score": 100,
  "recommendations": [
    {
      "category": "security",
      "confidence": "high",
      "current": "0",
      "param": "net.ipv4.tcp_rfc1337",
      "reason": "防止 TIME_WAIT 状态下的 RST 攻击",
      "recommended": "1",
      "subcategory": "network",
      "writable": true
    }
  ],
  "score": 30,
  "services": [
    "Nginx",
    "PostgreSQL"
  ],
  "system": {
    "cpu_cores": 12,
    "cpu_cores_host": 12,
    "cpu_model": "AMD Ryzen 9 7945HX with Radeon Graphics",
    "disks": [
      {
        "available_schedulers": [
          "none",
          "mq-deadline",
          "kyber"
        ],
        "hidden": false,
        "holders": [],
        "name": "sdd",
        "nr_requests": 950,
        "read_ahead_kb": 128,
        "rq_affinity": 1,
        "scheduler": "none",
        "type": "HDD"
      },
      {
        "available_schedulers": [
          "none",
          "mq-deadline",
          "kyber"
        ],
        "hidden": false,
        "holders": [],
        "name": "sdb",
        "nr_requests": 950,
        "read_ahead_kb": 128,
        "rq_affinity": 1,
        "scheduler": "none",
        "type": "HDD"
      }
    ],
    "kernel": "6.6.87.2-microsoft-standard-WSL2",
    "memory_gb": 11,
    "memory_gb_host": 11,
    "network": [
      {
        "name": "eth0",
        "speed_mbps": 10000
      }
    ],
    "numa_nodes": 1,
    "os_distro": "Ubuntu 24.04.3 LTS"
  },
  "total_checked": 196,
  "workload": "mixed"
}
```

`counts` 描述报告里的建议：按性质分为 `performance` 与 `security`，另有
`high_confidence` 与 `writable`，以及每个分类各一个计数——`network`、
`memory`、`io`、`cpu`——各自等于以同名分类运行 `check --category` 时保留的
建议条数。安全类建议只计入 `security` 一次，因此四个分类计数之和等于
`performance`。

`system` 是引擎诊断的那台机器。除了一直携带的四个键，它还给出发行版
（`os_distro`）、CPU（`cpu_model`），以及规则读取的两份清单：`disks`
列出每块磁盘的全部九个已采集字段——其中 `hidden`、`holders`、
`available_schedulers` 正是磁盘规则的门槛，因此「某条建议为什么没出现」
可以直接从报告里解释——`network` 则是每个网卡的 `name`/`speed_mbps`，
`speed_mbps: 0` 表示速率未知，不是零。`cpu_cores` 与 `memory_gb` 是规则
用于缩放的生效值，会被约束进程的 cgroup 限额夹紧；`cpu_cores_host` 与
`memory_gb_host` 是宿主本身的值——没有限额约束时两者相等，有限额时宿主值
更大。本页的示例把 `disks` 与 `network` 截短了；真实报告会列出读到的每一块
磁盘和网卡。

`--category` 可重复：每个值各自保留其分类（名称与从前相同），展示的条目
是所给分类的并集，因此 `check --category net --category mem` 一次覆盖两者
——`tune --category net --category mem` 规划的是同一个并集。同一分类的别名
（`net`、`network`、`网络`）重复给出时每个条目只保留一次；某个值不是已知
分类时报错（exit 2）而不是被忽略。

`check --only <param>`（可重复）只报告点名的建议，`check --exclude <param>`
（可重复）报告其余全部。两者接受与 `fix`/`why` 相同的拼写（斜杠/点号别名，
字面点号的网卡名），在 `--category`/`--conservative` 过滤之后生效，收窄的是
过滤后剩下的那份报告。未命中范围内任何建议的名字不是错误：按给定拼写、按
给定顺序回显在 `unmatched_only`（或 `unmatched_exclude`）键里——报告为空时
所有给出的名字都会出现。该键只在给出对应参数、且至少一个名字什么都没命中时
出现，因此不带新参数时 key 集合与以往完全一致。两个参数互斥：同时给出是用法
错误（退出码 2、stderr JSON），在读取系统信息之前就被拒绝。

`score` 与 `total_checked` 描述系统、保持不变；`predicted_score`、`counts`
与退出码跟随剩余的那份报告——与 `--category` 一直以来的做法相同——因此什么都
没选中时输出空 `recommendations` 列表、退出 0（诊断没有 blocked 状态，这点与
`tune` 不同）。收窄到一条建议时只报告该条：

```json
{"counts": {"cpu": 0, "high_confidence": 1, "io": 0, "memory": 0, "network": 1, "performance": 1, "security": 0, "writable": 1}, "environment": "物理机/虚拟机", "predicted_score": 30, "recommendations": [ ... ], "score": 30, "services": [], "system": {"cpu_cores": 12, "cpu_cores_host": 12, "cpu_model": "AMD Ryzen 9 7945HX with Radeon Graphics", "disks": [{"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdd", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}, {"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdb", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}], "kernel": "6.6.87.2-microsoft-standard-WSL2", "memory_gb": 11, "memory_gb_host": 11, "network": [{"name": "eth0", "speed_mbps": 10000}], "numa_nodes": 1, "os_distro": "Ubuntu 24.04.3 LTS"}, "total_checked": 208, "workload": "mixed"}
```

未命中的名字被回显，而不是让命令失败：

```json
{"counts": {"cpu": 0, "high_confidence": 0, "io": 0, "memory": 0, "network": 0, "performance": 0, "security": 0, "writable": 0}, "environment": "物理机/虚拟机", "predicted_score": 30, "recommendations": [], "score": 30, "services": [], "system": {"cpu_cores": 12, "cpu_cores_host": 12, "cpu_model": "AMD Ryzen 9 7945HX with Radeon Graphics", "disks": [{"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdd", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}, {"available_schedulers": ["none", "mq-deadline", "kyber"], "hidden": false, "holders": [], "name": "sdb", "nr_requests": 950, "read_ahead_kb": 128, "rq_affinity": 1, "scheduler": "none", "type": "HDD"}], "kernel": "6.6.87.2-microsoft-standard-WSL2", "memory_gb": 11, "memory_gb_host": 11, "network": [{"name": "eth0", "speed_mbps": 10000}], "numa_nodes": 1, "os_distro": "Ubuntu 24.04.3 LTS"}, "total_checked": 208, "unmatched_only": ["no_such_ktuner_param"], "workload": "mixed"}
```

### why 输出

`ktuner why <param>` 解释一个参数。规则给出推荐的参数携带 `check` 对同一参数发布的
字段；没有任何写路径会采纳时另带 `skip_reason`；已达推荐值（或在规则之外、从自己的
sysfs/sysctl 文件读值）的参数输出当前值并以 `0` 退出：

```json
{
  "current": "Linux",
  "param": "kernel.ostype",
  "status": "optimal"
}
```

两个及以上参数在一次运行里回答，共用一份诊断：逐参数单独调用会让每个进程都完整跑一遍
系统诊断，两次诊断之间内核状态若变化，答案还可能互相矛盾。输出携带 `count` 与 `params`
——按命令行顺序每个参数一条，按 `fix`、`tune --exclude/--only` 相同的别名政策去重
（`vm/swappiness` 与 `vm.swappiness` 只回答一次；`block/sda/scheduler` 这类 sysfs
身份保持原样）。每个元素与该参数单独调用时的对象完全相同：

```json
{
  "count": 2,
  "params": [
    {
      "current": "60",
      "param": "vm.swappiness",
      "status": "optimal"
    },
    {
      "category": "performance",
      "confidence": "high",
      "current": "4096 131072 6291456",
      "param": "net.ipv4.tcp_rmem",
      "reason": "万兆网卡场景下增大 TCP 接收缓冲区上限，充分利用带宽-延迟积",
      "recommended": "4096 131072 16777216",
      "subcategory": "network",
      "writable": true
    }
  ]
}
```

至少一条是推荐项时 `why` 退出 `1`，全部 optimal 时退出 `0`——与 `check` 对同一批
参数使用的 0/1 语义相同。找不到或读不到值的参数，以单参数相同的错误拒绝整条命令
（`2`，stderr JSON 点名该参数，stdout 为空）：批处理绝不回答半个问题，拼错一个名字
不会在退出码 0 的背后丢掉其余参数。
### tune 输出

```json
{"applied": 5, "boot_unit_enabled": true, "score_after": 35, "score_before": 30}
```

`boot_unit_enabled` 报告非 sysctl 配置的启动重放是否已就绪。若本次运行写入了块设备或
THP 参数、但 `systemctl enable ktuner-nonsysctl.service` 未成功，该键为 `false`：
值当前已生效，重启后却不会重放，只看 `applied` 无法看出这一点。单元启用成功、或本次
没有 `/proc/sys` 之外的持久化内容时为 `true`。`fix` 的返回体携带同一个键。

写入互斥 sysctl 对的一半时，内核会把另一半清零，返回体用 `cleared` 列出本次运行
清零的孪生（例如 `"cleared": ["vm.dirty_ratio"]`）。列表恰好是账本记录的孪生，
`rollback` 会恢复它们；原本就是 0 的孪生、或因值未变化而未被清零的孪生不会列出。
没有清零任何孪生时不出现该键。

真实运行也会点名它写了什么：`written` 每次写入一条，形如
`{param, previous, applied}`——`previous` 是账本记录的原值（`rollback` 恢复的就是
它），`applied` 是内核实际生效的值（内核调整过请求时就是调整后的值）。这正是单参数
`fix` 报告的那一对，批量运行因此不必再用 root 读一次账本，就能说出改了哪些参数、
原来是多少。该键恒出现，与 `failed` 一致：没有写入任何参数时返回空列表。原值读不到的
只写参数报告 `previous: null`，与 `fix` 的答案相同。清零孪生的写入仍算一条：孪生由
`cleared` 点名，列表长度即 `applied`。

当本环境过滤掉了部分建议（不可写，或运行时危险）时，真实 `tune` 会在
`would_skip` 中列出这些项及原因，形状与 dry-run 预览一致，便于与 `check`
对账——部分成功后 `check` 仍会对这些参数报 exit 1：

```json
{"applied": 4, "failed": [], "score_after": 35, "score_before": 30, "would_skip": [{"param": "vm.nr_hugepages", "reason": "runtime_dangerous"}]}
```

全部被过滤时的短路输出同样携带 `would_skip` 列表（与计数并存）。

`tune --exclude <param>`（可重复）把点名的建议移出计划：不写入、不进回滚
账本、不持久化。排除在 `--category`/`--conservative` 过滤之后生效；被排除
项在 `would_skip` 中以原因 `excluded` 列出（操作者本人的指示优先于
`unwritable` 和 `runtime_dangerous`）；某个名字没有命中范围内的任何建议
时不算错误——它按给定拼写回显在 `unmatched_exclude` 中，让没起作用的排除
可见而不是静默（计划为空时所有给出的名字都会出现在其中）：

```json
{"blocked": 1, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_skip": [{"param": "kernel.dmesg_restrict", "reason": "excluded"}]}
```

全部被排除时与任何无可应用项的计划一样，输出 `status: "blocked"`、退出码
1（`check` 仍会报告这些参数）；`blocked_excluded` 与其余短路计数并列，三者
加总等于 `recommendations`：

```json
{"applied": 0, "blocked": 55, "blocked_excluded": 55, "blocked_runtime_dangerous": 0, "blocked_unwritable": 0, "dry_run": true, "recommendations": 55, "status": "blocked", "would_apply": [], "would_skip": [ ... ]}
```

`--exclude` 管不到内核自身的副作用：互斥 sysctl 对（`vm.dirty_bytes`/
`vm.dirty_ratio`、`vm.dirty_background_bytes`/`vm.dirty_background_ratio`、
`vm.overcommit_kbytes`/`vm.overcommit_ratio`）中写入一半会把另一半清零
（`mm/page-writeback.c`、`mm/util.c`），因此被排除的参数在其对应项被写入时
仍会被内核清零。ktuner 照旧把被清零的原值记入账本（`rollback` 因此能恢复，
不丢原值）、只持久化真正写入的那一半，重启时由内核重现同样的清零状态；
内置规则从不同时把一对孪生的两半放进计划。

`tune --only <param>`（可重复）只把点名的建议留在计划里：拼写与 `fix`/`why`
一致，在 `--category`/`--conservative` 过滤之后生效，是 `--exclude` 的镜像
——两者互斥，同时给出是用法错误（exit 2、stderr JSON）。范围内未被点名的
条目与排除一样处理（不写入、不进回滚账本、不持久化），在 `would_skip` 中以
原因 `not_selected` 列出；`not_selected` 优先于 `unwritable` 和
`runtime_dangerous`，计数不随环境漂移。没有命中范围内任何建议的名字不算
错误——按给定拼写回显在 `unmatched_only` 中（计划为空时所有给出的名字都会
出现）。全部未选中时与任何无可应用项的计划一样输出 `status: "blocked"`、
退出码 1；`blocked_not_selected` 与其余短路计数并列，四者加总等于
`recommendations`：

```json
{"blocked": 11, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_skip": [{"param": "net.core.netdev_max_backlog", "reason": "not_selected"}, ...]}
```

未命中的名字选不中任何条目；当计划只由未命中的名字选定时它无可应用项，
短路输出用各自的键报告计数：

```json
{"applied": 0, "blocked": 53, "blocked_not_selected": 53, "blocked_runtime_dangerous": 0, "blocked_unwritable": 0, "dry_run": true, "recommendations": 53, "status": "blocked", "unmatched_only": ["vm.swappiness", "no_such_ktuner_param"], "would_apply": [], "would_skip": [ ... ]}
```

`--only` 与 `--exclude` 一样只筛计划，管不到内核自身的副作用：互斥 sysctl 对
中写入一半仍会把另一半清零（`mm/page-writeback.c`、`mm/util.c`），因此被
`--only` 留在计划外的参数在其对应项被写入时仍会被内核清零。ktuner 照旧把被
清零的原值记入账本，`rollback` 因此能恢复。

`tune --dry-run` 输出的是预览；`status` 与短路路径使用同一套取值
（此处为 `planned`；无可应用项时为 `optimal`/`blocked`）。`would_apply`
列出真实运行会写入的项，`would_skip` 列出本次运行不写入的项及原因
（`unwritable`、`runtime_dangerous`，或选择器点名时的 `excluded` /
`not_selected`），`blocked` 为这些项的数量；`would_clear` 列出这些写入会在内核中
清零的互斥孪生——与 `fix --dry-run` 同名同义，只是范围覆盖整个计划，取值来自
写入路径记账所用的同一张表。写互斥对的一半必然在核内清零另一半，与选择器是否点名
无关，因此该键就是把上面那段说明摆到预览里；没有孪生的计划不带此键，多个写入命中
同一个孪生时只列一次，顺序同计划顺序：

```json
{"blocked": 1, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_clear": ["vm.dirty_ratio"], "would_skip": [{"param": "vm.nr_hugepages", "reason": "runtime_dangerous"}]}
```

`ktuner why` 对没有任何写路径会采纳的推荐同样携带该原因
（`skip_reason`：`unwritable` 或 `runtime_dangerous`；计划会写入的项无此
字段），使解释输出与计划不矛盾。`check` 的推荐项也发布同一分类，
使诊断输出无需 dry run 就携带该原因。

### fix --dry-run 输出

`ktuner fix <param> --dry-run` 以与 `tune --dry-run` 相同的形态预览这一次
单参数写入（范围就是该参数）：`dry_run`、`status`（此处为 `"planned"`）、
`blocked`、`would_apply`（本次写入会落地的那一条建议，与 `check` 的条目同形）
以及 `would_skip`。它不写任何东西——不写内核、不进回滚账本、不持久化、
不加账本锁——并以 `0` 退出：

```json
{
  "blocked": 0,
  "dry_run": true,
  "status": "planned",
  "would_apply": [
    {
      "category": "performance",
      "confidence": "high",
      "current": "1000",
      "param": "net.core.netdev_max_backlog",
      "reason": "万兆网卡场景下增大网卡收包队列深度，避免高流量时软中断处理不及导致丢包",
      "recommended": "65536",
      "subcategory": "network",
      "writable": true
    }
  ],
  "would_skip": []
}
```

`would_skip` 在本命令里恒为空：单参数命令的所有拒绝情形都走命令自身的错误
通道——参数不在计划里（`parameter not found or already optimal: <param>`）、
不可写、运行时危险，或非 root 的真实运行——其 stderr JSON 与退出码与
`ktuner fix <param>` 完全一致，因此
`ktuner fix <param> --dry-run && ktuner fix <param>` 的预检不会被与被预览
命令不一致的预览误导。与 `tune --dry-run` 一样，预览不需要 root；真实
`fix` 仍需要。

写入互斥 sysctl 对的一半时，内核会把另一半清零（`mm/page-writeback.c`、
`mm/util.c`），预览用 `would_clear` 列出这一孪生，取值来自写入路径记账时
用的同一张表（参数没有孪生时不出现该键）：

```json
{
  "blocked": 0,
  "dry_run": true,
  "status": "planned",
  "would_apply": [
    {
      "category": "performance",
      "confidence": "medium",
      "current": "0",
      "param": "vm.dirty_bytes",
      "reason": "大内存服务器 (125 GB) 使用 dirty_ratio 百分比会导致脏页过多、IO 突刺，改用固定字节限制更平稳",
      "recommended": "268435456",
      "subcategory": "memory",
      "writable": true
    }
  ],
  "would_clear": [
    "vm.dirty_ratio"
  ],
  "would_skip": []
}
```

真实 `fix` 用 `cleared` 列出这次写入清零的孪生，形状与 `would_clear` 相同；
写入没有清零任何孪生时不出现该键。

真实 `fix` 返回 `fixed`（按输入原样回显的参数名）与 `param`（这次写入在账本中记录的键：
建议自身的写法，即 `rollback --list` 列出、`rollback <param>` 回答、`tune` 的 `written`
条目携带的那个键）。用别名（`VM/SWAPPINESS`、`vm/swappiness`）指定参数时两者不同，
与账本对账应使用 `param`。

真实 `fix` 运行时，只有孪生持有已配置的（非零）原值，才会把被清零的原值
记入账本（`rollback` 因此能恢复）；持久化重现清零后的状态。

### rollback 输出

```json
{"failed": 0, "restored": 5, "restored_params": [ ... ], "skipped": 0, "status": "Full"}
```

回滚两个方向都点名：`restored_params` 列出本次实际恢复的账本 key，按整批落地的顺序排列。
集合并不等于调用方点名的参数——互斥孪生会随清零它的条目一起恢复，`--exclude` /
`--category` 的选择也在引擎内部展开——所以只有这次运行自己说得清；被内核拒绝、却由孪生的
恢复带回来的写入同样在这里被点名。

回滚留下了什么也点名：读到 `failed` 或 `skipped` 的调用方不必再用 root 跑一次
`rollback --list` 才知道哪些参数还留着调优。`failures` 为每个没能落地的条目给出一条
`{"param", "error"}`（内核拒绝写入、回读仍被调整、被后续写入覆盖），`missing` 点出记录
路径已不存在的条目。三个键都只在有话可说时出现，所以既没有恢复、也没有留下任何东西的回滚
逐字节返回上面四个计数器。计数含义不变：`failed` 仍计入所有没落地的项，包含收尾没能删掉的
持久化文件与账本，`skipped` 仍是被跳过的缺失路径。四种调用形态都会报告它们——全量回滚、
单个位置参数、一批参数，以及 `--exclude`、`--category` 包装的本体。

```json
{"failed": 1, "failures": [{"error": "Is a directory (os error 21)", "param": "vm.dirty_ratio"}], "missing": ["net.core.somaxconn"], "restored": 1, "skipped": 1, "status": "Partial"}
```

### rollback <param>… 输出

`sudo ktuner rollback <param>` 只恢复该参数命中的账本条目，其余条目保留。参数拼写与
`fix`、`why` 相同（点/斜杠别名、含字面点的网卡名）：

```json
{"failed": 0, "param": "vm.dirty_bytes", "restored": 2, "restored_params": ["vm.dirty_bytes", "vm.dirty_ratio"], "skipped": 0, "status": "Full"}
```

`param` 是被恢复的账本条目，拼写与 `rollback --list` 一致。持久化文件按剩余账本重新
生成；账本清空时走与全量 rollback 相同的收尾（先删持久化文件，再删账本）。内核互斥的
参数对（`vm.dirty_bytes` / `vm.dirty_ratio`、`vm.overcommit_kbytes` /
`vm.overcommit_ratio`、`dirty_background_` 对）连同账本记录的孪生一起恢复：写任一半都会
把另一半清零，只恢复一半无法让账本描述内核的真实状态，因此 `restored` 把两条都计入，
`restored_params` 也把两条都点名（孪生用自己的账本 key）。
写入失败或路径缺失的条目连同其孪生一起保留，退出码 `1`，可重试；账本里没有的参数是命令
错误（`2`，stderr JSON），绝不静默成功。`status` 描述本次尝试（`Full` / `Partial` /
`Nothing`），不表示账本已清空。普通 `ktuner rollback` 与 `ktuner rollback --list` 行为不变。

两个及以上参数把多参数调优一次撤掉：整批在同一把账本锁内完成，持久化文件按剩余账本只重新
生成一次，而不是每个参数各生成一次。输出保留聚合计数键，把 `param`（字符串）换成 `params`
（数组）——每个位置参数解析出的账本 key，按输入顺序去重（`vm/swappiness` 与
`VM.SWAPPINESS` 都报 `vm.swappiness`）：

```json
{"failed": 0, "params": ["vm.swappiness", "net.core.somaxconn"], "restored": 2, "restored_params": ["vm.swappiness", "net.core.somaxconn"], "skipped": 0, "status": "Full"}
```

随条目一起恢复的孪生由 `restored_params` 点名，不出现在 `params` 里：数组回答这一批点名了
什么，报告回答它实际撤掉了什么；同时点名一对互斥孪生也只恢复一次。写入失败或路径缺失的参数照样出现在 `params` 里，并且由
`failures` / `missing` 点名，能不能恢复由计数器和 `status` 表达。部分成功的批只退役落地的条目，未落地的连同其孪生保留记录，退出码
`1`、`status` 为 `Partial`；一条都没落地时账本与持久化文件都不改动。任一名字不在账本里就在
动任何东西之前拒绝整条命令（`2`，stderr JSON）——与单参数版本同一条命令错误：静默跳过会让拼错
一个名字丢掉一整批回滚，而命令仍然退出 `0`。无参数的 `rollback` 与 `rollback --list` 不变；
`rollback --list <param>…` 预览一批回滚（见下）。

所有只撤掉部分账本的 rollback（指定参数、`--exclude`、`--category`）都会按剩余条目重新生成
持久化文件，并为非 sysctl 配置的启动重放重新执行 `systemctl enable`。enable 被拒绝时，输出
多一个 `"boot_unit_enabled": false`（与 `tune`、`fix` 的键相同）：仍在账本里的块设备或 THP
配置重启后不会自动重放。启动重放已就绪、或本次没有重新生成时不出现这个键，输出保持不变。

### rollback --list 输出

`sudo ktuner rollback --list` 预览回滚将恢复的内容——只读，不写入不删除（ledger 是 0700 root 目录下的 0600 文件，因此与 rollback 共用 root 门槛；损坏的 ledger 报错而不是当作空列表）。每个条目保留原有的 `param`/`applied`/`previous`，附带记录该条目的规则给出的 `category`（与 `check` 的词条同一小写拼写；本字段出现之前写入的记录、或库导入的记录为 `null`，任何类别都不选中它们），并新增内核当前的实际状态：`live` 从该条目自己的路径读取，使用与其他所有界面相同的读取器（sysfs 选项列表取方括号中的活动项、多值 sysctl 折叠为单空格）；`drifted` 表示 `live` 是否仍与 `applied` 一致，比较方式与写入校验相同——内核按自身形态呈现的值（方括号选项列表、TAB 分隔的多值、领先 token 回显）不算漂移；`cleared_by` 见下：

```json
{"count": 3, "pending": [
  {"applied": "none", "category": null, "cleared_by": null, "drifted": null, "live": null, "param": "block/sda/scheduler", "previous": "mq-deadline"},
  {"applied": "0", "category": "performance", "cleared_by": null, "drifted": true, "live": "20", "param": "vm.dirty_ratio", "previous": "20"},
  {"applied": "1", "category": "performance", "cleared_by": null, "drifted": false, "live": "1", "param": "vm.swappiness", "previous": "60"}
]}
```

被内核作为互斥孪生副作用清零的条目记录 `applied = "0"`（内核实际写入的值），因此当它不再为 0 时即为漂移；`cleared_by` 点名是哪个参数的写入把这个 0 放进去的，用的是该参数自己那一行 `param` 的拼写，两行不必知道配对表就能对上。没有它，这一行看起来就像 ktuner 主动关掉了一个没人提过的旋钮。当账本里没有记录清零点时它是 `null`：调用方要求的 0，以及清零点不在本次列表中的孪生（上面那份账本没有 `vm.dirty_bytes`，所以它的 `vm.dirty_ratio` 不点名任何参数）。该键与 `live`、`drifted`、`category` 一样在每个条目上恒出现。`category` 绝不按参数名重算（安全发现可以落在 `net.*` 旋钮上），因此本字段出现之前写入或由导入写入的记录报 `null` 而不是猜测——与路径无法读取（设备已消失、模块未加载、write-only 旋钮）时报 `live: null` 和 `drifted: null` 同一口径：读不到值不算错误，漂移也不会改变退出码。列表是快照——预览与回滚之间内核可能变化。普通 `ktuner rollback` 行为不变：恢复、定稿 ledger 并清理。

`sudo ktuner rollback --list <param> [<param>…]` 把预览收窄到 `rollback <param> [<param>…]`
将恢复的内容。名字的解析与回滚完全相同（同样的拼写、同一套查找），所以列出的是每个点名的条目，
加上账本为它记录的互斥孪生——回滚会把孪生一起恢复；同一条目点名两次或用两种拼写，只列一次。
输出的键和条目字段与不带参数的列表相同，顺序同样按 param 排列——
`rollback --list vm/swappiness vm.dirty_bytes`：

```json
{"count": 3, "pending": [
  {"applied": "1073741824", "category": "performance", "cleared_by": null, "drifted": false, "live": "1073741824", "param": "vm.dirty_bytes", "previous": "0"},
  {"applied": "0", "category": "performance", "cleared_by": "vm.dirty_bytes", "drifted": false, "live": "0", "param": "vm.dirty_ratio", "previous": "20"},
  {"applied": "1", "category": "performance", "cleared_by": null, "drifted": false, "live": "1", "param": "vm.swappiness", "previous": "60"}
]}
```

配对的两半会一起列出，所以被清零那一半的 `cleared_by` 指向的正是本次列表中另一行——点任一半
预览到的都是同一对，也就是回滚它时会恢复的那一对。

账本里没有的名字是与回滚相同的命令错误（`2`，stderr JSON，stdout 为空），预览因此不会承诺
一次会被拒绝的回滚；没有账本时所有名字都算未记录。两种情况都不写入任何东西。此前 `--list`
带任何参数都是用法错误。

`sudo ktuner rollback --list --drifted` 只列出 `drifted` 为 `true` 的条目——那些被别的写者改掉的调优。输出形态不变，`count` 是过滤后的条数，因此直接回答「我丢了哪些」。路径读不到（`drifted: null`）的条目**不在**其中：读不到值不能当作漂移的证据，用不带 `--drifted` 的 `--list` 仍能看到它们。同时点名参数时，先按名字收窄预览，再对剩下的条目按漂移过滤。`--drifted` 不带 `--list` 是用法错误（stderr JSON、退出码 2），与其他参数错误一样在 root 检查之前拒绝。

### rollback --exclude 输出

`sudo ktuner rollback --exclude <param> [<param>…]`（可重复）一次事务恢复账本中除点名条目以外的全部
条目：一把账本锁，只在最后按剩余条目重新生成一次持久化文件。输出保留全量回滚的键；没有命中账本的
`--exclude` 名字不算错误——按给出顺序回显在 `unmatched_exclude` 中，让没起作用的排除可见而不是静默
（与 `tune --exclude` 同一条规则）：

```json
{"failed": 0, "restored": 3, "skipped": 0, "status": "Full"}
```

被排除的条目留在账本里，与 `rollback <param>…` 未点名的条目留在账本里同一语义。互斥孪生在任何地方都是
同一个恢复单元：排除任一半都保留整个单元——被排除的条目与账本为它记录的另一半都不动——因为写入留下的
一半会在内核里把被排除的一半清零（`v6.6 mm/page-writeback.c:518`、`v6.6 mm/util.c:810`），只恢复一半还会让
账本描述的内核状态与实际不符。没有任何单元可恢复时（全部条目被排除，或剩余单元的孪生被排除），输出与空
账本相同（`status: "Nothing"`、退出码 `0`），账本（仍保存被排除的记录）不删除。

`sudo ktuner rollback --list --exclude <param>` 以 `--list` 的同形只读预览同一集合，`--drifted` 仍对剩余
条目过滤。没有命中账本的名字同样回显；没有账本时所有名字都算未命中，预览是空清单。点名恢复与点名排除
是方向相反的指令，`rollback <param>…` 与 `--exclude` 同时给出是用法错误（`2`，stderr JSON），与其他
参数错误一样在 root 检查之前拒绝。普通 `ktuner rollback`、`ktuner rollback --list` 与
`ktuner rollback <param>…` 不变。

### rollback --category 输出

`sudo ktuner rollback --category <cat> [--category <cat>…]` 只恢复账本中类别匹配的条目，其余记录留在
账本里；`sudo ktuner rollback --list --category <cat>` 以只读方式预览同一集合，形态与 `--list` 相同
（每个条目附带记录的 `category`；`--drifted` 仍对剩余
条目过滤）。取值与 `check`、`tune` 相同——可重复，取并集——选中的正是 `tune --category <cat>` 会写的
那些条目：类别随每条规则写入的记录一起保存，因为参数名说明不了它来自哪条规则。`net.core.bpf_jit_harden`
是 `net.*` 参数上的安全建议，`rollback --category net` 不能碰它。`--exclude` 叠加在类别选择之上（先按
类别选、再排除，与 `tune --category … --exclude …` 一致），未命中名字的回显与 `rollback --exclude`
相同：

```json
{"failed": 0, "restored": 1, "skipped": 0, "status": "Full", "unclassified": ["vm.vfs_cache_pressure"]}
```

没有类别的记录——本字段出现之前写入的账本，或库导入写入的记录——任何类别都不选中，也**不按参数名猜**。
这些记录按名字回显在 `unclassified` 中（param 序），只在非空时出现：

```json
{
  "count": 1,
  "pending": [
    {
      "applied": "1",
      "category": "performance",
      "drifted": false,
      "live": "1",
      "param": "vm.swappiness",
      "previous": "60"
    }
  ],
  "unclassified": [
    "vm.vfs_cache_pressure"
  ],
  "unmatched_exclude": [
    "vm.typo"
  ]
}
```

选中集合为空时（没有任何条目属于所给类别，或选中的单元都被 `--exclude` 挡住），输出与空账本相同
（`status: "Nothing"`、退出码 `0`），账本（仍保存其余记录）不删除。点名恢复与按类别选择是方向相反的
指令，`rollback <param>… --category <cat>` 是用法错误（`2`，stderr JSON），与其他参数错误一样在 root
检查之前拒绝；未知类别也在那里被拒绝，报错文字与 `check`、`tune` 相同。普通 `ktuner rollback`、
`ktuner rollback --list`、`ktuner rollback <param>…` 与 `ktuner rollback --exclude …` 不变；本版本
读入的账本保持原有字节——类别字段只为规则写入的条目而写。

### 错误输出（stderr）

```json
{"error": "tune requires root (sudo ktuner tune)"}
```

网络 `net.ipv4.conf` 和 `net.ipv6.conf` 参数保留网卡大小写与字面点：`ktuner why net/ipv4/conf/Br0.100/forwarding` 指向 `Br0.100`。也接受点分隔别名。网卡包含字面点时，持久化使用首个分隔符为斜杠的键保留路径含义。当前内置规则不生成逐 VLAN 推荐。

## 安全性

- **代码执行拒绝列表**：`kernel.core_pattern`、`kernel.modprobe`、`kernel.hotplug`、`kernel.poweroff_cmd`、`kernel.modules_disabled`、`kernel.kexec_load_disabled`、`kernel.usermodehelper.*`、`fs.binfmt_misc.*` 在任何写路径（tune/fix/rollback）中都被无条件阻止。匹配基于解析后的文件系统路径而非参数拼写，因此 slash/dot/traversal 变体均会被拦截。
- **运行时危险参数**：在运行主机上改动不安全的参数（`vm.nr_hugepages`）在同一写入咽喉点被所有调用方拒绝——tune 不将其纳入计划（在 `would_skip` 中标记 `runtime_dangerous`），fix 拒绝并建议持久化，库导入同样无法实时应用。同一参数的 slash/dot 拼写均会被拦截。
- **并发操作**：tune、fix、库导入和 rollback 从原值读取、写入、记账到持久化共用一把锁。原值在持锁后读取；无法读取账本时阻止新写入。此保护协调 KTuner 操作，不覆盖外部 sysctl 写入者或崩溃恢复。
- **回滚安全**：部分失败时保留回滚账本；原始值不会丢失。
- **无自主 root 执行**：ktuner 检查 `euid == 0`，若非 root 则报错退出。cosh 的 sandbox-guard 加上权限提示确保人类在任何 `sudo ktuner tune` 执行前批准操作。

## 安装

通过 ANOLISA 组件管理器（RPM 后端）安装 ktuner：

```bash
sudo anolisa install ktuner --backend rpm
```

ktuner 仅以 RPM 形式发布。需显式传入 `--backend rpm`：默认后端解析的是 raw 工件，其中没有 ktuner 的发布版本，且不存在跨后端回退。

也可以通过 yum/dnf 安装：

```bash
sudo yum install ktuner
```

安装内容：
- `/usr/local/bin/ktuner` — CLI 二进制文件
- `/usr/share/anolisa/components/ktuner/component.toml` — 组件契约

或从源码构建：

```bash
cd src/ktuner
cargo build --release
sudo install -m 0755 target/release/ktuner /usr/local/bin/ktuner
```

# ktuner

ktuner 是面向 AI Agent 的确定性内核调优引擎。它对运行中的系统评估 207 条规则，输出结构化 JSON 建议，让 Agent（或用户）能安全地诊断、应用、回滚内核参数改动。

---

## 概述

ktuner 是规则引擎，不是 LLM：每条建议都来自读取 `/proc/sys` 和 `/sys` 的硬编码规则，因此结果可复现、可解释。它覆盖网络、内存、I/O、CPU、安全类参数，对当前系统打分，并预测调优后的分数。

它设计为被 cosh 及其他 ANOLISA 兼容 Agent 作为工具调用，但命令行同样可以手动使用。

磁盘 `rq_affinity` 建议会考虑 `threadirqs` 启动参数和 PREEMPT_RT。引号内的参数值不会引入独立 flag 或 `--` 分隔符；实际分隔符后的参数属于 init。

---

## 安装

ktuner 以 RPM 形式发布，支持 Linux x86_64（仅 system mode）。通过 ANOLISA 组件管理器安装，显式选择 RPM backend——ktuner 没有 raw 发布，且 backend 之间没有回退：

```bash
sudo anolisa install ktuner --backend rpm
```

也可以直接安装 RPM 并让 ANOLISA 纳入管理：

```bash
sudo yum install ktuner
sudo anolisa --install-mode system adopt ktuner
```

或者从源码构建：

```bash
cd src/ktuner
cargo build --release
# 二进制在 target/release/ktuner
```

只读使用可直接运行二进制（`./target/release/ktuner check`）。若要让 `ktuner` 全局可用——`ktuner tune`（需 root）以及 cosh 首次运行集成（只运行可信路径下 root 拥有的二进制）都要求如此——请安装到系统路径：

```bash
sudo install -o root -g root -m 755 target/release/ktuner /usr/local/bin/ktuner
```

下面的示例假设 `ktuner` 已在 `PATH` 中。

---

## 快速开始

```bash
# 诊断 —— 只读，无需 root
ktuner check                   # 分数 + 所有建议
ktuner check --category net    # 仅某一类别
ktuner check --category net --category mem   # 可重复，取并集
ktuner check --conservative    # 仅高置信度建议
ktuner check --only vm.swappiness   # 只报告点名的参数
ktuner check --exclude vm.dirty_ratio   # 报告其余全部、去掉这一项

# 预览改动但不应用（dry-run）
sudo ktuner tune --dry-run

# 应用建议（需要 root）
sudo ktuner tune               # 应用全部
sudo ktuner tune --conservative
sudo ktuner tune --exclude vm.dirty_ratio   # 应用其余全部、跳过这一项
sudo ktuner tune --only vm.swappiness       # 只应用点名的参数

# 修复单个参数
sudo ktuner fix vm.swappiness
sudo ktuner fix vm.swappiness --dry-run   # 预览单个参数，不做实际变更

# 解释参数为何应该改 —— 多个参数共用一份诊断
ktuner why net.core.somaxconn vm.swappiness

# 撤销 ktuner 做的所有改动，或点名的已记录参数
sudo ktuner rollback          # 破坏性且终结：恢复并删除 ledger
sudo ktuner rollback --list   # 只读预览回滚将恢复的内容
sudo ktuner rollback --list vm.swappiness   # 只预览回滚 vm.swappiness 将恢复的内容
sudo ktuner rollback vm.swappiness net.core.somaxconn   # 回滚点名的已记录参数
sudo ktuner rollback --exclude vm.swappiness   # 恢复其余全部已记录条目、跳过这一项
sudo ktuner rollback --list --exclude vm.swappiness   # 预览同一集合
sudo ktuner rollback --category net   # 只恢复账本中该分类的条目
sudo ktuner rollback --list --category net   # 预览同一集合
```

`sudo ktuner fix <param> --dry-run` 以与 `tune --dry-run` 相同的形态预览这一次
单参数写入（`dry_run`、`status`、`blocked`、`would_apply`、`would_skip`，
写入会清零互斥孪生时两者都会多出 `would_clear`），不写入任何东西并以 `0` 退出，
便于改参数前先看清结果。它不需要 root；参数不在计划里、不可写或运行时危险时，
与 `sudo ktuner fix <param>` 给出完全相同的 stderr JSON 与退出码，因此预检不会
与被预览的命令不一致。

真实 `fix` 返回 `fixed`（按输入原样回显的名字）与 `param`（这次写入在账本中记录的键，
即 `rollback --list` 列出、`rollback <param>` 回答的那个键）。用 `vm/swappiness`
这类别名指定参数时，两者不同。

真实 `tune` 或 `fix` 用 `cleared` 列出写入清零的互斥孪生，形状与 `would_clear`
相同：恰好是账本为 `rollback` 记录的孪生。没有清零任何孪生时不出现该键。

真实 `tune` 也会点名它写了什么：`written` 每次写入一条，给出参数、`previous`
（账本记录的原值，`rollback` 恢复的就是它）与 `applied`（内核实际生效的值）——
正是单参数 `fix` 已经报告的那一对。该键恒出现；没有写入任何参数时返回空列表，
原值读不到的只写参数报告 `previous: null`，因为这次写入无法记账。

所有输出为 stdout 上的 JSON，错误为 stderr 上的 JSON。退出码：`0` 成功、
`1` check 发现可改进项或 rollback 仍有值未恢复、`2` 命令错误。
回滚遇到写入失败或路径缺失时返回 `1`（包括部分恢复），仍在 stdout 输出 JSON 计数，
并保留记录以便重试。本体还会点名恢复了什么——`restored_params` 列出实际落地的账本 key，
它不等于调用方点名的集合：互斥孪生会随清零它的条目一起恢复，`--exclude` / `--category`
的选择也在引擎内部展开——以及留下了什么：`failures` 为每个没能落地的条目给出一条
`{"param", "error"}`，`missing` 列出记录路径已不存在的条目；三个键都只在有话可说时出现，
所以既没有恢复、也没有留下任何东西的回滚保持原来的四个计数器，四种调用形态的报告方式一致。空记录为成功的无操作（`0`）；
记录缺失或无法读取为命令错误（`2`）。
`sudo ktuner rollback <param>` 对命中的账本条目遵循同一套契约——账本同时记录互斥孪生时
两者一起恢复（写任一半都会把另一半清零）——并在 JSON 中以 `param` 回显该条目；
账本里没有的参数是命令错误（`2`）。

位置参数可以给多个，一次撤掉多参数调优：整批在同一把账本锁内完成，持久化文件按剩余账本
只重新生成一次，两个参数之间不会留下写了一半的状态。输出保留聚合计数键，把 `param` 换成
`params`——每个参数解析出的账本 key，按输入顺序去重。每个名字都适用与单个参数相同的契约；
只恢复了一部分的批退役已落地的条目、保留其余以便重试并退出 `1`，而任一名字不在账本里就在
动任何东西之前拒绝整条命令（`2`），与单参数版本同一条命令错误。

`check --only <param>`（可重复）只报告点名的建议，`check --exclude <param>`
（可重复）报告其余全部。名字的解析与 `fix`/`why` 完全一致，选择在
`--category`/`--conservative` 过滤之后生效。未命中范围内任何建议的名字不是
错误：按给定拼写回显在 `unmatched_only`（或 `unmatched_exclude`）中——报告为空
时所有给出的名字都会出现。`score` 与 `total_checked` 描述系统、保持不变；
`predicted_score`、`counts` 与退出码跟随剩余的这份报告，与 `--category` 一直
以来的做法一致，因此什么都没选中时输出空建议列表、退出 `0`（诊断没有 blocked
状态）。两个参数互斥，同时给出是命令错误（`2`、stderr JSON），在读取系统信息
之前就被拒绝。
报告中的 `counts` 对象统计它携带的建议：按性质分为 `performance` 与
`security`，另有 `high_confidence`、`writable`，以及每个分类各一个计数——
`network`、`memory`、`io`、`cpu`——各自等于以同名分类运行 `check --category`
时保留的建议条数（安全类建议只计入 `security` 一次）。与其他计数一样，它们跟随
`--category`、`--only`、`--exclude` 过滤后剩下的报告。
报告中的 `system` 对象描述这台机器本身：原有的 `kernel`、`cpu_cores`、
`memory_gb`、`numa_nodes` 之外，新增 `os_distro`、`cpu_model`、`disks`
清单（每块磁盘一条，携带磁盘规则的门槛字段 `hidden`、`holders`、
`available_schedulers`）与 `network` 清单（每个网卡 `name`/`speed_mbps`，
`speed_mbps: 0` 表示速率未知，不是零）。`cpu_cores` 与 `memory_gb` 是规则
用于缩放的生效值，会被约束进程的 cgroup 限额夹紧；`cpu_cores_host` 与
`memory_gb_host` 是宿主本身的值——没有限额约束时两者相等，有限额时宿主值
更大。
`ktuner why <param>…` 解释一个或多个参数。单个参数保持原有的平铺形态：推荐项携带
`check` 对同一参数发布的字段（没有任何写路径会采纳时带 `skip_reason`），已达推荐值的
参数输出 `{"param": …, "current": …, "status": "optimal"}`。两个及以上参数在一次运行里
回答、共用一份诊断——逐参数单独调用会让每个进程各跑一遍完整的系统诊断。输出携带
`count` 与 `params`：按命令行顺序每个参数一条，按 `fix` 相同的别名政策去重
（`vm/swappiness` 与 `vm.swappiness` 是同一参数；`block/sda/scheduler` 这类 sysfs
身份保持原样），每个元素与该参数单独调用时的对象完全相同。至少一条是推荐项时 `why`
退出 `1`，全部 optimal 时退出 `0`；找不到或读不到的参数以自身错误拒绝整条命令——
退出 `2`、stderr JSON 点名该参数、stdout 为空——批处理绝不回答半个问题。
`tune --exclude <param>`（可重复）把点名的建议移出计划：不写入、不进回滚账本、
不持久化。排除在 `--category` 与 `--conservative` 过滤之后生效；被排除项在输出中
以 `would_skip`（原因 `excluded`）列出，未命中范围内任何建议的名字则出现在
`unmatched_exclude` 中（计划为空时是全部给出的名字），而不是让命令失败。把全部建议
排除是 `blocked` 运行、退出码 `1`，与任何无可应用项的计划一致。互斥 sysctl 对中写入另一半时，内核仍会把
被排除的一半清零；ktuner 记录被清零的原值，`rollback` 可以恢复。

`tune --only <param>`（可重复）是 `--exclude` 的镜像——两者互斥，同时给出是
命令错误（`2`）。只有点名的建议留在计划里；范围内其余条目出现在 `would_skip`
中、原因为 `not_selected`，未命中范围内任何建议的名字回显在 `unmatched_only`
中而不是让命令失败（什么都没选中是 `blocked` 运行、退出码 `1`）。与
`--exclude` 一样，它只筛计划：写入互斥孪生的另一半时，内核仍会把被留下的
一半清零。

---

网络 conf 参数中的网卡身份区分大小写。`net/ipv4/conf/Br0.100/forwarding` 和 `net.ipv4.conf.Br0.100.forwarding` 指向同一网卡；`br0.100` 是不同的身份。IPv6 遵循相同规则。网卡包含字面点时，持久化记录使用首个分隔符为斜杠的 sysctl.d 键，让 systemd 保留这些点。此行为支持已有有效记录或自定义库推荐；当前内置规则不生成逐 VLAN 推荐。

`sudo ktuner rollback --list` 也会报告当前实际状态：每个待回滚条目新增 `live`（此刻从该条目路径读到的值；路径读不到时为 `null`——设备已消失、模块未加载）、`drifted`（`live` 是否仍与记录的 `applied` 一致；没有可比较的 live 值时为 `null`）、`category`（记录该条目的规则给出的类别——与 `check` 同一小写拼写；记录本身没有类别时为 `null`，例如本字段出现之前写入的记录或导入写入的记录）和 `cleared_by`（当内核把这一条作为互斥配对的副作用清零时，点名是哪一个条目的写入清零了它——用那一行自己发布的 `param` 拼写，两行因此能对上；没有孪生清零它的条目为 `null`，也就是调用方要求的 0；清零点不在本次列表中的孪生也为 `null`）。没有它，一行没人要求过的 `applied` 为 `0` 会看起来像 ktuner 主动关掉了这个旋钮。这些字段都是对现有形态的新增——`count`、`pending` 以及记录的 `param`/`applied`/`previous` 取值都不变——读不到值不算错误，命令仍以 `0` 退出。取值的呈现与比较与 ktuner 其他输出一致，sysfs 选项列表或多值 sysctl 不会被误报为漂移。`sudo ktuner rollback --list --drifted` 把列表收窄到记录值与内核不再一致的条目——被别的写者改掉的那些；路径读不到的条目不在其中（不带 `--drifted` 的 `--list` 仍能看到），`--drifted` 不带 `--list` 是用法错误。

`sudo ktuner rollback --list <param>…` 把这份预览收窄到 `rollback <param>…` 将恢复的内容：每个点名的条目，加上与它一起记录的互斥孪生，形态与顺序同完整列表——配对的两半都会列出，所以被清零那一半的 `cleared_by` 指向的正是本次列表中的一行。账本里没有的名字是与回滚相同的命令错误（`2`）。

`sudo ktuner rollback --exclude <param>…` 在一次事务里恢复账本中除点名条目以外的全部条目，`sudo ktuner rollback --list --exclude <param>…` 以只读方式预览同一集合（`--drifted` 仍对剩余条目过滤）。被排除的条目留在账本里；与它一起记录的互斥孪生同样保留——写任一半都会在内核里把另一半清零（`v6.6 mm/page-writeback.c:518`、`v6.6 mm/util.c:810`），孪生对是一个恢复单元，排除任一半都保留整个单元。输出保留全量回滚的键；没有命中账本的名字回显在 `unmatched_exclude` 中而不是让命令失败，与 `tune --exclude` 报告惰性排除的方式一致。全部条目被排除时什么都不恢复，输出与空账本相同（`status "Nothing"`、退出码 `0`），账本保留这些被排除的记录。`rollback <param>…` 与 `--exclude` 是相反的指令，同时给出是用法错误（`2`，stderr JSON）。

`sudo ktuner rollback --category <cat>…` 只恢复账本中类别匹配的条目（取值与 `check`、`tune` 相同，
可重复、取并集），`sudo ktuner rollback --list --category <cat>` 只读预览同一集合，`--drifted` 仍对
剩余条目过滤。选中的正是 `tune --category <cat>` 会写的条目——类别随规则写入的记录一起保存，因为参数名
说明不了它背后的规则是不是安全规则（`net.core.bpf_jit_harden` 是 `net.*` 参数上的安全建议），选择
从不按参数名猜。`--exclude` 叠加在类别选择之上：先按类别选、再排除，未命中名字的回显与
`rollback --exclude` 一致。没有类别的记录（本字段出现之前写入的账本，或库导入）任何类别都不选中，
按名字回显在 `unclassified` 中。选中为空时什么都不恢复，输出与空账本相同（`status "Nothing"`、
退出码 `0`），账本保留；`rollback <param>…` 与 `--category` 同时给出是用法错误（`2`，stderr JSON），
未知类别按 `check`、`tune` 的报错文字拒绝。

## 权限边界

| 命令 | Root | 作用 |
|------|------|------|
| `check`、`why` | 否 | 只读诊断；绝不写内核 |
| `tune --dry-run` | 否 | 预览改动，不写入 |
| `tune`、`fix`、`rollback` | 是（`sudo`） | 写 `/proc/sys`；非 root 直接报错。`rollback --list` 只读但同样需要 root（ledger 是 0700 root 目录下的 0600 文件） |

安全保证：

- **代码执行 deny-list**：可能导致代码执行的参数（`kernel.core_pattern`、`kernel.modprobe`、`kernel.hotplug` 等）在所有写入路径上被无条件阻止。匹配基于解析后的文件系统路径，因此拼写变体无法绕过。
- **并发操作**：tune、fix、库导入和 rollback 通过同一把锁串行执行原值读取、写入、账本更新和持久化。过期诊断值不会被用作新的回滚原值；无法读取账本时阻止新写入。此保证不覆盖外部 sysctl 写入者或崩溃恢复。
- **回滚安全**：已应用的改动会被记录；部分回滚失败时绝不丢弃其余参数的原始值。
- **无自主 root**：ktuner 非 root 运行时一律报错。通过 cosh 调用时，沙箱守卫与权限提示确保任何 `sudo ktuner tune` 都经人工批准才执行。

---

## 配合 cosh 使用

cosh 通过 skill 定义（`src/os-skills/system-admin/ktuner/`）自动发现 ktuner，无需接线——用自然语言提问即可：

```
> “看看这台机器的内核参数能不能优化”
> “按数据库负载优化内核”
```

首次 Linux 认证时，如果系统路径下有可信的 ktuner，cosh 会显示一行非阻塞提示。用 `/ktuner enable` 查看只读的 `ktuner check` 报告，或 `/ktuner disable` 不再提示。也可以在 `/settings` 里通过 `general.ktunerCheck` 修改。cosh 绝不自行应用改动。

---

## 参见

- [Copilot Shell](copilot-shell/QUICKSTART.md)
- [OS Skills](os-skills.md)
- 完整参考：`src/ktuner/README.md`

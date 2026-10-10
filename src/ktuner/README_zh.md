# ktuner — 确定性内核调优引擎

[English](README.md) | **中文**

面向 AI agent 的内核参数调优引擎，属于 ANOLISA 的一部分。ktuner 针对运行中的系统评估 207 条规则，输出结构化 JSON 调优建议。设计上由 cosh/agent 通过 `ktuner <command> [options]` 调用。

## 用法

```bash
# 诊断 — 输出评分和建议
ktuner check
ktuner check --category net
ktuner check --conservative    # 仅高置信度

# 应用建议（需要 root 权限）
sudo ktuner tune --dry-run     # 预览，不做实际变更
sudo ktuner tune               # 全部应用
sudo ktuner tune --conservative
sudo ktuner tune --exclude vm.dirty_ratio   # 应用其余全部、跳过这一项

# 修正单个参数（需要 root 权限）
sudo ktuner fix <param>        # 例如 sudo ktuner fix vm.swappiness

# 解释某个参数为何需要修改
ktuner why <param>             # 例如 ktuner why net.core.somaxconn

# 回滚变更（需要 root 权限）
sudo ktuner rollback          # 破坏性且终结（删除 ledger）
sudo ktuner rollback --list   # 只读预览回滚将恢复的内容
sudo ktuner rollback <param>  # 回滚单个已记录参数，例如 vm.dirty_bytes
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
    "high_confidence": 5,
    "performance": 34,
    "security": 6,
    "writable": 40
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
    "cpu_cores": 2,
    "kernel": "6.6.102+",
    "memory_gb": 8,
    "numa_nodes": 1
  },
  "total_checked": 196,
  "workload": "mixed"
}
```

### tune 输出

```json
{"applied": 5, "score_after": 35, "score_before": 30}
```

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

`tune --dry-run` 输出的是预览；`status` 与短路路径使用同一套取值
（此处为 `planned`；无可应用项时为 `optimal`/`blocked`）。`would_apply`
列出真实运行会写入的项，`would_skip` 列出本次运行不写入的项及原因
（`unwritable`、`runtime_dangerous`，或 `--exclude` 点名时的 `excluded`），
`blocked` 为这些项的数量：

```json
{"blocked": 1, "dry_run": true, "status": "planned", "would_apply": [ ... ], "would_skip": [{"param": "vm.nr_hugepages", "reason": "runtime_dangerous"}]}
```

`ktuner why` 对没有任何写路径会采纳的推荐同样携带该原因
（`skip_reason`：`unwritable` 或 `runtime_dangerous`；计划会写入的项无此
字段），使解释输出与计划不矛盾。`check` 的推荐项也发布同一分类，
使诊断输出无需 dry run 就携带该原因。

### rollback 输出

```json
{"failed": 0, "restored": 5, "skipped": 0, "status": "Full"}
```

### rollback <param> 输出

`sudo ktuner rollback <param>` 只恢复该参数命中的账本条目，其余条目保留。参数拼写与
`fix`、`why` 相同（点/斜杠别名、含字面点的网卡名）：

```json
{"failed": 0, "param": "vm.dirty_bytes", "restored": 2, "skipped": 0, "status": "Full"}
```

`param` 是被恢复的账本条目，拼写与 `rollback --list` 一致。持久化文件按剩余账本重新
生成；账本清空时走与全量 rollback 相同的收尾（先删持久化文件，再删账本）。内核互斥的
参数对（`vm.dirty_bytes` / `vm.dirty_ratio`、`vm.overcommit_kbytes` /
`vm.overcommit_ratio`、`dirty_background_` 对）连同账本记录的孪生一起恢复：写任一半都会
把另一半清零，只恢复一半无法让账本描述内核的真实状态，因此 `restored` 把两条都计入。
写入失败或路径缺失的条目连同其孪生一起保留，退出码 `1`，可重试；账本里没有的参数是命令
错误（`2`，stderr JSON），绝不静默成功。`status` 描述本次尝试（`Full` / `Partial` /
`Nothing`），不表示账本已清空。普通 `ktuner rollback` 与 `ktuner rollback --list` 行为不变。

### rollback --list 输出

`sudo ktuner rollback --list` 预览回滚将恢复的内容——只读，不写入不删除（ledger 是 0700 root 目录下的 0600 文件，因此与 rollback 共用 root 门槛；损坏的 ledger 报错而不是当作空列表）：

```json
{"count": 2, "pending": [{"applied": "1", "param": "vm.swappiness", "previous": "60"}]}
```

普通 `ktuner rollback` 行为不变：恢复、定稿 ledger 并清理。

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

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

# 修正单个参数（需要 root 权限）
sudo ktuner fix <param>        # 例如 sudo ktuner fix vm.swappiness

# 解释某个参数为何需要修改
ktuner why <param>             # 例如 ktuner why net.core.somaxconn

# 回滚所有变更（需要 root 权限）
sudo ktuner rollback
```

## JSON 输出

所有输出以 **JSON 格式写入 stdout**。错误以 **JSON 格式写入 stderr**。stdout 不包含 ANSI 颜色、进度条或人类可读的格式化文本。

### 退出码

| 退出码 | 含义 |
|--------|------|
| 0      | 成功（check：系统已最优；tune/fix/rollback：已应用） |
| 1      | check：存在调优建议（非错误，表示系统可改善） |
| 2      | 错误（详情见 stderr JSON） |

### check 输出

```json
{
  "score": 30,
  "predicted_score": 100,
  "total_checked": 196,
  "recommendations": [
    {
      "param": "net.ipv4.tcp_rfc1337",
      "current": "0",
      "recommended": "1",
      "reason": "防止 TIME_WAIT 状态下的 RST 攻击",
      "confidence": "high",
      "category": "security",
      "subcategory": "network",
      "writable": true
    }
  ],
  "counts": { "performance": 34, "security": 6, "high_confidence": 5, "writable": 40 },
  "system": { "kernel": "6.6.102+", "cpu_cores": 2, "memory_gb": 8, "numa_nodes": 1 },
  "environment": "物理机/虚拟机",
  "workload": "mixed",
  "services": ["Nginx", "PostgreSQL"]
}
```

### tune 输出

```json
{ "applied": 5, "score_before": 30, "score_after": 35 }
```

### rollback 输出

```json
{ "restored": 5, "failed": 0, "skipped": 0, "status": "Full" }
```

### 错误输出（stderr）

```json
{ "error": "tune requires root (sudo ktuner tune)" }
```

## 安全性

- **代码执行拒绝列表**：`kernel.core_pattern`、`kernel.modprobe`、`kernel.hotplug`、`kernel.poweroff_cmd`、`kernel.modules_disabled`、`kernel.kexec_load_disabled`、`kernel.usermodehelper.*`、`fs.binfmt_misc.*` 在任何写路径（tune/fix/rollback）中都被无条件阻止。匹配基于解析后的文件系统路径而非参数拼写，因此 slash/dot/traversal 变体均会被拦截。
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

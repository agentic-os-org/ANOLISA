---
name: ktuner
version: 0.1.0
description: 内核参数自动调优。分析系统配置，输出调优建议并可一键应用，支持自动回滚。使用场景：系统性能优化、安全加固、内核参数诊断。
layer: core
lifecycle: production
allowedTools:
  - run_shell_command
---

# ktuner — 内核参数自动调优

## 核心能力

ktuner 是确定性规则引擎（非 LLM），评估 207 条内核调优规则，输出 JSON 格式的诊断结果和建议。

**所有输出在 stdout，格式为 JSON。错误输出在 stderr，格式也是 JSON。**

## 使用流程

### 第一步：诊断

```bash
ktuner check
```

输出示例：
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
      "writable": true
    }
  ],
  "system": { "kernel": "6.6.102+", "cpu_cores": 2, "memory_gb": 8 },
  "workload": "mixed",
  "services": ["Nginx", "PostgreSQL"]
}
```

**根据 score 判断是否需要调优**：90+ 优秀，70-89 良好，低于 70 建议调优。

可选参数：
- `--category net|mem|io|cpu|security` — 只看某一类
- `--conservative` — 只看高置信度建议

### 第二步：向用户解释建议

遍历 recommendations 数组，用中文向用户解释每条建议的 reason 和影响。

如果用户想了解某个具体参数：
```bash
ktuner why <参数名>
```

### 第三步：应用（需要 root，必须用户确认后才执行）

**⚠️ 重要：tune 和 fix 会修改内核参数，必须先向用户展示建议内容，得到明确确认后再执行。**

预览模式（不修改）：
```bash
sudo ktuner tune --dry-run
```

应用全部建议：
```bash
sudo ktuner tune
```

只修一个参数：
```bash
sudo ktuner fix <参数名>
```

### 第四步：回滚（如果需要）

```bash
sudo ktuner rollback
```

输出：
```json
{ "restored": 5, "failed": 0, "skipped": 0, "status": "Full" }
```

## 退出码

| 退出码 | 含义 |
|--------|------|
| 0      | 成功（check 时表示系统已最优） |
| 1      | check 发现有建议（不是错误，表示可以优化） |
| 2      | 错误（详见 stderr 的 JSON） |

## 约束

- tune / fix / rollback 需要 **root 权限**（使用 `sudo`）
- check / why 不需要 root
- `kernel.core_pattern`、`kernel.modprobe` 等代码执行参数被禁止写入
- 调优后如发现性能劣化，使用 `sudo ktuner rollback` 恢复

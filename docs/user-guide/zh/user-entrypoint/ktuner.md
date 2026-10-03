# ktuner

ktuner 是面向 AI Agent 的确定性内核调优引擎。它对运行中的系统评估 207 条规则，输出结构化 JSON 建议，让 Agent（或用户）能安全地诊断、应用、回滚内核参数改动。

---

## 概述

ktuner 是规则引擎，不是 LLM：每条建议都来自读取 `/proc/sys` 和 `/sys` 的硬编码规则，因此结果可复现、可解释。它覆盖网络、内存、I/O、CPU、安全类参数，对当前系统打分，并预测调优后的分数。

它设计为被 cosh 及其他 ANOLISA 兼容 Agent 作为工具调用，但命令行同样可以手动使用。

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
ktuner check --conservative    # 仅高置信度建议

# 预览改动但不应用（dry-run）
sudo ktuner tune --dry-run

# 应用建议（需要 root）
sudo ktuner tune               # 应用全部
sudo ktuner tune --conservative

# 修复单个参数
sudo ktuner fix vm.swappiness

# 解释某个参数为何应该改
ktuner why net.core.somaxconn

# 撤销 ktuner 做的所有改动
sudo ktuner rollback
```

所有输出为 stdout 上的 JSON，错误为 stderr 上的 JSON。退出码：`0` 成功、`1` check 发现可改进项（非错误）、`2` 错误。

---

## 权限边界

| 命令 | Root | 作用 |
|------|------|------|
| `check`、`why` | 否 | 只读诊断；绝不写内核 |
| `tune --dry-run` | 否 | 预览改动，不写入 |
| `tune`、`fix`、`rollback` | 是（`sudo`） | 写 `/proc/sys`；非 root 直接报错 |

安全保证：

- **代码执行 deny-list**：可能导致代码执行的参数（`kernel.core_pattern`、`kernel.modprobe`、`kernel.hotplug` 等）在所有写入路径上被无条件阻止。匹配基于解析后的文件系统路径，因此拼写变体无法绕过。
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

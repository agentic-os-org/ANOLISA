---
name: install-qoder
version: 1.0.0
description: 在 Linux/macOS 服务器上安装 Qoder CLI（qodercli）并接入 ANOLISA Tokenless 压缩插件。覆盖官方安装脚本、qodercli 登录、PATH 修正、tokenless 插件注册（anolisa adapter enable tokenless qoder）、安装验证与卸载。当用户需要安装 Qoder、部署 qodercli、在 Qoder 中启用 Tokenless 压缩或排查 tokenless@local 插件注册问题时使用此技能。
layer: application
lifecycle: usage
---

# Qoder CLI 安装与 Tokenless 接入

Qoder CLI（命令名 `qodercli`）是 Qoder 的命令行编码代理。Tokenless 为 Qoder
提供命令改写与响应压缩（通过 Qoder 原生插件机制注册 `tokenless@local`）。本技能
安装 Qoder CLI 本体，并把 Tokenless 插件注册进去。

命令事实与 ANOLISA 自带的 tokenless qoder 适配器
（`src/tokenless/adapters/tokenless/qoder/scripts/install.sh`）保持一致。

## 系统要求

- **OS**: Linux 或 macOS
- **网络**: 需要联网（下载官方安装脚本）
- **Shell**: Bash（官方安装脚本由 bash 执行）
- **依赖**: `curl`（下载安装脚本）、`python3`（Tokenless hook 依赖）

## 安装工作流

复制此清单跟踪进度：

```
Task Progress:
- [ ] Step 1: 预检环境（curl / python3）
- [ ] Step 2: 安装 Qoder CLI
- [ ] Step 3: 登录 qodercli
- [ ] Step 4: 安装 Tokenless（若未安装）
- [ ] Step 5: 注册 Tokenless Qoder 插件
- [ ] Step 6: 验证
```

### 一键脚本（推荐）

```bash
bash scripts/install.sh                 # 安装 qodercli + 注册 tokenless 插件
bash scripts/install.sh --skip-tokenless # 只安装 qodercli
```

脚本自动完成：预检 → 官方安装脚本 → qodercli 探测（含版本目录）→ 插件能力检查
→ Tokenless 注册（优先 `anolisa adapter enable`）→ 验证 `tokenless@local`。

### Step 1: 预检环境

```bash
command -v curl && command -v python3
```

`python3` 缺失时 Tokenless hooks 无法工作，须先安装（如 `sudo dnf install -y python3`）。

### Step 2: 安装 Qoder CLI

使用官方安装脚本（与仓库文档一致）：

```bash
curl -fsSL https://qoder.com/install | bash
```

安装后 `qodercli` 位于 `~/.qoder/bin/qodercli/` 下（按版本分目录），通常安装器
会自动修正 PATH。找不到命令时手动添加：

```bash
export PATH="$HOME/.qoder/bin/qodercli:$PATH"
echo 'export PATH="$HOME/.qoder/bin/qodercli:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

### Step 3: 登录

```bash
qodercli login
```

按提示完成账号登录（交互式，无法由脚本代劳）。

### Step 4: 安装 Tokenless（若未安装）

```bash
command -v tokenless || \
  curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

### Step 5: 注册 Tokenless Qoder 插件

优先使用 anolisa CLI（推荐，写入组件记录）：

```bash
anolisa adapter enable tokenless qoder
```

npm 直装且无 anolisa 记录时，直接运行适配器自带脚本：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/qoder/scripts/install.sh
```

底层动作（适配器实际执行）：`qodercli plugins install <插件目录> --scope user`。

### Step 6: 验证

```bash
qodercli plugins list --json
```

应看到 `tokenless@local` 条目：`scope` 为 `user` 且 `enabled` 为 `true`。

```bash
anolisa adapter status tokenless   # anolisa 管理的安装
```

**完全重启 Qoder IDE / qodercli 会话**后插件才会加载（Qoder 会缓存插件配置）。

## 故障排查

| 症状 | 处理 |
|------|------|
| `command not found: qodercli` | 确认 `~/.qoder/bin/qodercli` 在 PATH；新开 shell 重试 |
| 官方安装脚本下载失败 | 检查网络/代理：`curl -fsSL https://qoder.com/install -o /dev/null` |
| `qodercli lacks 'plugins install' support` | Qoder CLI 版本过旧，缺少插件生命周期命令；升级后重试 |
| `tokenless@local` 未出现在 plugins list | 重跑 Step 5；确认 python3 存在；重启 IDE 后再查 |
| 旧 hook 路径仍生效 | Qoder 缓存插件配置，完全重启 IDE；仍异常见 tokenless 文档 Qoder plugin cache issue |
| 插件重复注册 | 适配器脚本幂等（重复安装会覆盖），重复条目先 `qodercli plugins uninstall tokenless --scope user` 再重装 |

## 卸载

```bash
# 1. 注销 Tokenless 插件（anolisa 管理的安装）
anolisa adapter disable tokenless qoder

# 2. 或直接通过 qodercli 注销
qodercli plugins uninstall tokenless --scope user

# 3. （可选）移除 Qoder CLI 本体：删除其安装目录
rm -rf "$HOME/.qoder"
```

注意：卸载 Tokenless 插件不会删除 `~/.tokenless/`（统计数据库被所有适配器共享）。

---
name: install-codex
version: 1.0.0
description: 在 Linux/macOS 上安装 OpenAI Codex CLI 并接入 ANOLISA Tokenless 插件（RTK 命令改写与环境错误诊断）。覆盖 npm 安装、tokenless CLI 就绪检查、anolisa-tokenless 插件市场注册（codex plugin add tokenless@anolisa-tokenless）、验证与卸载。当用户需要安装 Codex CLI、部署 codex、在 Codex 中启用 Tokenless 或排查 tokenless 插件注册问题时使用此技能。
layer: application
lifecycle: usage
---

# Codex CLI 安装与 Tokenless 接入

Codex CLI（命令名 `codex`）是 OpenAI 的本地编码代理。Tokenless 为 Codex 提供
RTK 命令改写（PreToolUse）与环境错误诊断（PostToolUse 注入 additionalContext）；
受 Codex 协议限制不做响应压缩。本技能安装 Codex CLI 本体，并把 Tokenless 插件
通过 `anolisa-tokenless` 插件市场注册进去。

命令事实与 ANOLISA 自带的 tokenless codex 适配器
（`src/tokenless/adapters/tokenless/codex/scripts/install.sh`）保持一致。

## 系统要求

- **OS**: Linux 或 macOS
- **网络**: 需要联网（npm registry）
- **Shell**: Bash
- **依赖**: Node.js（含 `npm`）、`python3`（Tokenless hook 脚本依赖）
- **Tokenless CLI**: `tokenless` 命令须可用（见 Step 4）

## 安装工作流

复制此清单跟踪进度：

```
Task Progress:
- [ ] Step 1: 预检环境（node/npm/python3）
- [ ] Step 2: 安装 Codex CLI
- [ ] Step 3: 验证 codex
- [ ] Step 4: 确保 Tokenless CLI 就绪
- [ ] Step 5: 注册 Tokenless Codex 插件
- [ ] Step 6: 验证
```

### 一键脚本（推荐）

```bash
bash scripts/install.sh                  # 安装 codex + 注册 tokenless 插件
bash scripts/install.sh --skip-tokenless # 只安装 codex
```

脚本自动完成：预检 → npm 安装 → codex 探测（含 `~/.local/bin` 等标准路径）→
tokenless CLI 就绪检查 → 插件注册（优先 `anolisa adapter enable`）→ 验证
`codex plugin list`。

### Step 1: 预检环境

```bash
node --version && npm --version && command -v python3
```

Node.js 版本过低时，先通过 nvm 安装 LTS：

```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
source ~/.bashrc && nvm install --lts
```

### Step 2: 安装 Codex CLI

官方 npm 包（上游仓库 github.com/openai/codex）：

```bash
npm install -g @openai/codex
```

不要使用 `sudo npm install -g`（会造成权限问题）。出现 `EACCES` 时修正 npm
prefix：

```bash
mkdir -p ~/.npm-global
npm config set prefix '~/.npm-global'
echo 'export PATH="$HOME/.npm-global/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
npm install -g @openai/codex
```

### Step 3: 验证 codex

```bash
codex --version
```

`command not found` 时确认 npm 全局 bin 目录（如 `~/.npm-global/bin`）在 PATH。

### Step 4: 确保 Tokenless CLI 就绪

Tokenless 插件的 SessionStart hook 会检查 `tokenless` 命令（查找顺序：PATH、
`~/.local/bin/tokenless`、`/usr/local/bin/tokenless`、`/usr/bin/tokenless`）：

```bash
command -v tokenless || \
  curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

源码开发场景也可用 `cargo build --release -p tokenless-cli` 自行构建（需 Rust
工具链），本技能默认走官方安装脚本。

### Step 5: 注册 Tokenless Codex 插件

优先使用 anolisa CLI（推荐，写入组件记录）：

```bash
anolisa adapter enable tokenless codex
```

npm 直装且无 anolisa 记录时，直接运行适配器自带脚本：

```bash
bash ~/.local/share/anolisa/adapters/tokenless/codex/scripts/install.sh
```

底层动作（适配器实际执行）：在 `${XDG_DATA_HOME:-$HOME/.local/share}/anolisa/
codex-marketplace` 建立本地插件市场（名称 `anolisa-tokenless`），然后：

```bash
codex plugin marketplace add ~/.local/share/anolisa/codex-marketplace
codex plugin add tokenless@anolisa-tokenless
```

### Step 6: 验证

```bash
codex plugin list
```

应看到 `tokenless@anolisa-tokenless` 一行，STATUS 为 `installed, enabled`。

```bash
anolisa adapter status tokenless   # anolisa 管理的安装
```

**新开一个 Codex 会话**后插件才会加载（旧会话不会动态加载新插件）。

## 故障排查

| 症状 | 处理 |
|------|------|
| `command not found: codex` | 确认 npm 全局 bin 目录在 PATH：`npm config get prefix` |
| `EACCES`（npm 全局安装） | 用 Step 2 的 npm prefix 修正流程 |
| `codex plugin add` 报找不到市场 | 先执行 `codex plugin marketplace add ~/.local/share/anolisa/codex-marketplace`，再 add |
| plugins list 中 STATUS 为 `not installed` | 重跑 Step 5 的 `codex plugin add tokenless@anolisa-tokenless` |
| SessionStart 报 tokenless 不可用 | 回到 Step 4 安装 tokenless CLI 并确认 `command -v tokenless` |
| 插件未生效 | 关闭旧会话，新开一个 Codex 会话再验证 |
| 无压缩统计 | 正常：Codex 协议限制 PostToolUse 不能替换输出，Tokenless 在 Codex 上只做命令改写与环境错误诊断 |

## 卸载

```bash
# 1. 注销 Tokenless 插件（anolisa 管理的安装）
anolisa adapter disable tokenless codex

# 2. 或直接通过 codex 注销
codex plugin remove tokenless@anolisa-tokenless
codex plugin marketplace remove anolisa-tokenless
rm -rf "${XDG_DATA_HOME:-$HOME/.local/share}/anolisa/codex-marketplace"

# 3. （可选）移除 Codex CLI 本体
npm uninstall -g @openai/codex
```

注意：卸载 Tokenless 插件不会删除 `~/.tokenless/`（统计数据库被所有适配器共享）。

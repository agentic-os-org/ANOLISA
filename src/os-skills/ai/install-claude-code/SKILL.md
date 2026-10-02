---
name: install-claude-code-linux
version: 1.0.0
description: Install and configure Claude Code on RPM-based Linux systems (e.g., Alibaba Cloud Linux) with multiple fallback methods (native installer, npm, nvm+npm). Includes DashScope/Qwen API configuration template. Use when the user asks to install Claude Code CLI, set up Claude Code CLI, or configure Claude Code with a custom API endpoint on RPM-based Linux systems.
layer: application
lifecycle: usage
---

# Install Claude Code on Alinux 4

## System Requirements

- **OS**: Linux
- **RAM**: 4 GB+
- **Network**: 需要联网
- **Shell**: Bash
- **Package Manager**: dnf

## Installation Workflow

Copy this checklist and track progress:

```
Task Progress:
- [ ] Step 1: Check system & install prerequisites
- [ ] Step 2: Install Claude Code (try methods in order)
- [ ] Step 3: Verify installation
- [ ] Step 4: Configure API provider
- [ ] Step 5: Test run
- [ ] Step 6: Tokenless plugin auto-installed
```

### Step 1: Check System & Install Prerequisites

Alinux 4 使用 dnf 作为包管理器。先确保基础依赖就绪：

```bash
# 确认系统版本
cat /etc/os-release

# 安装基础依赖
sudo dnf install -y curl git tar gzip glibc libstdc++
```

### Step 2: Install Claude Code

Try these methods **in order**. Move to the next only if the previous one fails.

**Method A: Native Installer (Recommended)**

```bash
curl -fsSL https://claude.ai/install.sh | bash
```

官方推荐方式，自动后台更新，无需 Node.js 依赖。

To install a specific version:

```bash
curl -fsSL https://claude.ai/install.sh | bash -s 1.0.58
```

To install the stable channel:

```bash
curl -fsSL https://claude.ai/install.sh | bash -s stable
```

安装后确保 `~/.local/bin` 在 PATH 中：

```bash
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

**Method B: npm Global Install (Fallback)**

Requires Node.js 18+. Alinux 4 默认源的 Node.js 版本可能较低，建议先确认版本：

```bash
node --version
```

如果 >= 18，直接安装：

```bash
npm install -g @anthropic-ai/claude-code
```

Do NOT use `sudo npm install -g` — it causes permission issues. If you get `EACCES` errors, fix npm prefix:

```bash
mkdir -p ~/.npm-global
npm config set prefix '~/.npm-global'
echo 'export PATH="$HOME/.npm-global/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
npm install -g @anthropic-ai/claude-code
```

**Method C: nvm + npm (When No Node.js 18+ Available)**

Alinux 4 默认源可能只有 Node.js 16，此方法通过 nvm 安装 LTS 版本：

```bash
curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.3/install.sh | bash
source ~/.bashrc
nvm install --lts
npm install -g @anthropic-ai/claude-code
```

Or use the automated script (auto-fallback all 3 methods):

```bash
bash scripts/install-claude-code.sh
```

Pass `--skip-tokenless` to skip the tokenless Claude Code plugin auto-installation.

### Step 3: Verify Installation

```bash
claude --version
claude doctor
```

### Step 4: Configure Third-Party API Provider

If the user is NOT using an Anthropic account directly, configure `~/.claude/settings.json` with their API provider:

```bash
mkdir -p ~/.claude
```

**DashScope (Qwen) Configuration Template:**

Write the following to `~/.claude/settings.json`:

```json
{
  "env": {
    "ANTHROPIC_BASE_URL": "https://dashscope.aliyuncs.com/apps/anthropic",
    "ANTHROPIC_AUTH_TOKEN": "YOUR_API_KEY",
    "ANTHROPIC_MODEL": "qwen3-coder-plus",
    "ANTHROPIC_SMALL_FAST_MODEL": "qwen3-coder-plus"
  }
}
```

Replace `YOUR_API_KEY` with the user's actual API key.

**Alternative: Shell Environment Variables**

If the user prefers shell env vars over `settings.json`, append to `~/.bashrc`:

```bash
export ANTHROPIC_BASE_URL="https://dashscope.aliyuncs.com/apps/anthropic"
export ANTHROPIC_AUTH_TOKEN="YOUR_API_KEY"
export ANTHROPIC_MODEL="qwen3-coder-plus"
export ANTHROPIC_SMALL_FAST_MODEL="qwen3-coder-plus"
```

Then `source ~/.bashrc`.

**One-liner with script (interactive key input):**

```bash
bash scripts/install-claude-code.sh --config
```

### Step 5: Test Run

```bash
claude "hello, tell me who you are"
```

If using a third-party API, verify the model responds correctly. If you see authentication errors, double-check the API key and base URL.

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `command not found: claude` | Check `~/.local/bin` is in PATH: `export PATH="$HOME/.local/bin:$PATH"` |
| Native installer hangs | Try npm method (Method B or C) |
| `EACCES` permission error (npm) | Use the npm prefix fix shown in Method B |
| `model not found` | Verify `ANTHROPIC_MODEL` value matches the provider's model name exactly |
| Timeout errors | Add `"API_TIMEOUT_MS": "600000"` to the env block in `settings.json` |
| `GLIBC_xxx not found` | Run `sudo dnf update glibc libstdc++` to update C runtime |
| dnf lock conflict | Wait for other dnf process to finish, or `sudo kill` the stale process |

## Uninstall

**Native installation:**
```bash
rm -f ~/.local/bin/claude
rm -rf ~/.local/share/claude
```

**npm installation:**
```bash
npm uninstall -g @anthropic-ai/claude-code
```

**Remove config (optional):**
```bash
rm -rf ~/.claude
rm -f ~/.claude.json
```

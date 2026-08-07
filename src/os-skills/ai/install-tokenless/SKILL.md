---
name: install-tokenless
version: 1.0.0
description: Install and configure Tokenless (LLM token optimization toolkit) on Linux or macOS. Use when the user asks to install Tokenless, set up token optimization, reduce LLM token usage, or enable Tokenless for an agent framework (cosh, OpenClaw, Hermes, Qoder, Claude Code, Codex, Qwen Code).
layer: application
lifecycle: usage
---

# Install Tokenless

Tokenless is the token optimization component of ANOLISA. It compresses tool schemas, API responses, and CLI output to reduce LLM token consumption — without changing prompts or agent behavior.

## System Requirements

- **OS**: Linux (glibc) or macOS
- **Architecture**: x86_64 or arm64
- **Network**: Internet access required
- **Shell**: Bash

## Installation Workflow

Copy this checklist and track progress:

```
Task Progress:
- [ ] Step 1: Install Tokenless CLI
- [ ] Step 2: Verify installation
- [ ] Step 3: Enable for an agent framework (optional)
- [ ] Step 4: Test compression
```

### Step 1: Install Tokenless CLI

Try these methods **in order**. Move to the next only if the previous one fails.

**Method A: anolisa CLI (Recommended)**

```bash
curl -fsSL https://get.agentic-os.sh | bash
anolisa install tokenless
```

This installs the full ANOLISA component suite including adapters.

**Method B: npm Global Install**

Requires Node.js 16+. Installs prebuilt binaries for your platform:

```bash
npm install -g anolisa-tokenless
```

This automatically installs `tokenless`, `rtk`, and `toon` binaries plus framework adapters.

**Method C: Standalone curl Install**

One-liner that tries npm first, falls back to source build:

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

Options via environment variables:

```bash
# Pin a specific version
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | TOKENLESS_VERSION=0.7.4 bash

# Custom install directory
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | TOKENLESS_INSTALL_DIR=/usr/local/bin bash
```

### Step 2: Verify Installation

```bash
tokenless --version
```

If `command not found`, ensure the install directory is in PATH:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

### Step 3: Enable for an Agent Framework (Optional)

Tokenless can integrate with agent frameworks via adapters. Find available agents and enable:

```bash
# If installed via anolisa CLI
anolisa adapter scan
anolisa adapter enable tokenless <agent>
anolisa adapter status tokenless

# If installed via npm, adapters are at:
ls ~/.local/share/anolisa/adapters/tokenless/

# Run a framework-specific install script, e.g. for Claude Code:
bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh
```

Supported frameworks:

> **Note:** The script paths below are examples for npm-installed adapters. Not all frameworks ship a standalone install script, and directory names may differ. The recommended integration path is `anolisa adapter enable tokenless <framework>` (for anolisa CLI installs) or check `ls ~/.local/share/anolisa/adapters/tokenless/` to confirm the adapter directory exists before running a script directly.

| Agent | Adapter install script |
|-------|----------------------|
| cosh / Copilot Shell | `anolisa adapter enable tokenless cosh` |
| OpenClaw | `bash ~/.local/share/anolisa/adapters/tokenless/openclaw/scripts/install.sh` |
| Hermes | `bash ~/.local/share/anolisa/adapters/tokenless/hermes/scripts/install.sh` |
| Qoder | `bash ~/.local/share/anolisa/adapters/tokenless/qoder/scripts/install.sh` |
| Claude Code | `bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh` |
| Codex | `bash ~/.local/share/anolisa/adapters/tokenless/codex/scripts/install.sh` |
| Qwen Code | `bash ~/.local/share/anolisa/adapters/tokenless/qwencode/scripts/install.sh` |

Restart the agent CLI, IDE, or gateway after enabling.

### Step 4: Test Compression

```bash
printf '%s\n' \
  '{"status":"ok","data":{"name":"demo","items":[1,2,3]},"debug":{"trace":"verbose"},"metadata":null}' \
  | tokenless compress-response
```

If the output is shorter than the input (debug/metadata fields removed), Tokenless is working.

Check savings after using an agent:

```bash
tokenless stats list --limit 5
tokenless stats summary
```

## Troubleshooting

| Symptom | Fix |
|---------|-----|
| `command not found: tokenless` | Add install dir to PATH: `export PATH="$HOME/.local/bin:$PATH"` |
| npm install fails with EACCES | Fix npm prefix: `mkdir -p ~/.npm-global && npm config set prefix '~/.npm-global'` |
| `GLIBC_xxx not found` | Linux binaries require glibc 2.17+. Update: `sudo dnf update glibc` |
| No stats after enabling | Content may not have passed through Tokenless or had no compressible fields |
| musl Linux (Alpine) | Prebuilt binaries not available; build from source |

## Uninstall

**npm installation:**
```bash
npm uninstall -g anolisa-tokenless
```

**anolisa CLI installation:**
```bash
anolisa uninstall tokenless
```

**Standalone curl installation:**
```bash
rm -f ~/.local/bin/tokenless ~/.local/bin/rtk ~/.local/bin/toon
rm -rf ~/.tokenless
rm -rf ~/.local/share/anolisa/adapters/tokenless
```

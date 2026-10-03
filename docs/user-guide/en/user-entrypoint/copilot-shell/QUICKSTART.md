# Quick Start

> Welcome to Copilot Shell!

This guide helps you get started with AI-powered coding and system administration
in just a few minutes. By the end, you'll know how to use Copilot Shell for
common development and operations tasks.

## Prerequisites

Make sure you have:

- A **terminal** on an Alibaba Cloud Linux (Alinux) machine
- A code project or system to manage
- One of the supported authentication methods configured (see [Authenticate](#step-2-authenticate) below)

## Step 1: Install

### RPM (recommended)

```bash
sudo yum install copilot-shell
```

### Build from Source

Requires [Node.js 20+](https://nodejs.org/download). Check your version with `node -v`.

```bash
cd deprecated/copilot-shell
make build
```

After a successful build, the bundled output is available at `dist/cli.js`.

## Step 2: Authenticate

When you start Copilot Shell for the first time, you'll need to configure authentication:

```bash
cosh
```

Use the `/auth` command inside the session to choose your provider:

```bash
/auth
```

### Supported Providers

| Provider | Description |
|----------|-------------|
| Alibaba Cloud Auth | Default method. Auto-detects ECS and launches Web authentication (browser link + QR code); in non-ECS environments, enter AK/SK directly. |
| OpenAI Compatible | Supports DashScope, DeepSeek, Kimi, GLM, MiniMax, or any OpenAI-compatible endpoint |

> [!TIP]
>
> To switch accounts or authentication method later, use the `/auth` command within a session.

## Step 3: Start Your First Session

Launch Copilot Shell in any project directory:

```bash
cd /path/to/your/project
cosh
```

You'll see the welcome screen and session info. Type `/help` to view all available commands.

> [!NOTE]
>
> You can also use the aliases `co` or `copilot` instead of `cosh`.

## Step 4: Enable Sandbox Hooks (recommended)

Copilot Shell ships with built-in sandbox-guard hooks that intercept tool calls
and enforce security policies — preventing unauthorized file system access or
dangerous operations. These hooks are not active until you install them.

Inside Copilot Shell, run:

```
/hooks install
```

This command copies the bundled `sandbox-guard.py` script to
`~/.copilot-shell/hooks/` and registers it in your user settings. You only need
to run this once — the configuration is saved and persists across sessions.

> [!NOTE]
>
> This step requires `agent-sec-core` (linux-sandbox) to be installed at
> `/usr/local/bin/linux-sandbox`. When a dangerous command is detected,
> `sandbox-guard.py` wraps it inside the `linux-sandbox` binary for execution.
> If you built ANOLISA using the default `./scripts/build-all.sh`,
> `agent-sec-core` is included and installed automatically.

> [!TIP]
>
> To verify the hooks are active, run `/hooks list` inside Copilot Shell. You
> should see `sandbox-guard` and `sandbox-failure-handler` listed as enabled.

## Talking to Copilot Shell

### Ask Questions

Copilot Shell analyzes your files and answers questions. Ask about the codebase:

```
Explain the directory structure of this project
```

Or ask about system status:

```
Show current disk usage and the top memory-consuming processes
```

> [!NOTE]
>
> Copilot Shell reads files on demand — no need to manually add context.
> It also has built-in OS-level skills for system administration tasks.

### Making Code Changes

Try a simple coding task:

```
Add a hello world function to the main file
```

Copilot Shell will:

1. Find the appropriate file
2. Show the proposed changes
3. Ask for your confirmation
4. Apply the modifications

> [!NOTE]
>
> Copilot Shell always asks permission before modifying files. You can review
> one by one, or enable "accept all" mode for the current session.

### System Administration

Copilot Shell integrates OS-level skills for common operations tasks:

```
Check for any failed systemd services
```

```
Analyze nginx access logs and find the top 10 IPs in the last hour
```

```
Set up a cron job to clean /tmp every day at 3 AM
```

### Working with Git

Git operations become natural language conversations:

```
What files have I changed?
```

```
Commit my changes with a descriptive message
```

```
Create a new branch called feature/quickstart
```

```
Help me resolve the merge conflicts
```

### Fixing Bugs or Adding Features

Describe your needs in natural language:

```
Add input validation to the user registration form
```

Or fix existing issues:

```
There's a bug: users can submit empty forms, help me fix it
```

Copilot Shell will:

- Locate the relevant code
- Understand the context
- Implement the fix
- Run available tests

### Entering Interactive Shell

Use the `/bash` command to enter an interactive shell from within Copilot Shell:

```
/bash
```

Type `exit` to return to the Copilot Shell session.

### Other Common Workflows

**Refactor code**

```
Refactor the authentication module to use async/await instead of callbacks
```

**Write tests**

```
Write unit tests for the calculator functions
```

**Update documentation**

```
Update the README with installation instructions
```

**Code review**

```
Review my changes and suggest improvements
```

> [!TIP]
>
> **Remember**: Copilot Shell is your AI pair programmer and sysadmin
> assistant. Talk to it like you would a helpful colleague — describe what you
> want to achieve, and it will help you get there.

## Common Commands

| Command | Function | Example |
|---------|----------|---------|
| `cosh` | Launch Copilot Shell | `cosh` |
| `/auth` | Switch authentication method | `/auth` |
| `/hooks install` | Install sandbox-guard hooks (run once after install) | `/hooks install` |
| `/hooks list` | View all registered hooks and status | `/hooks list` |
| `/help` | Show help | `/help` or `/?` |
| `/bash` | Enter interactive shell | `/bash` |
| `/model` | Switch model | `/model` |
| `/compress` | Replace chat history with summary to save tokens | `/compress` |
| `/clear` | Clear screen | `/clear` (shortcut `Ctrl+L`) |
| `/theme` | Switch theme | `/theme` |
| `/language` | View or switch language settings | `/language` |
| → `ui [lang]` | Set UI language | `/language ui zh-CN` |
| → `output [lang]` | Set LLM output language | `/language output English` |
| `/quit` | Exit | `/quit` or `/exit` |

**Use shortcuts for efficiency**

- Press `?` to see all keyboard shortcuts
- Use Tab for command completion
- Press ↑ to browse command history
- Type `/` to see all slash commands

## Pro Tips

**Be specific with your requests**

- Instead of: "fix the bug"
- Try: "fix the login bug where users see a blank screen after entering wrong credentials"

**Use step-by-step instructions**

- Break complex tasks into steps:

```
1. Create a new database table for user profiles
2. Create an API endpoint to get and update user profiles
3. Build a webpage that allows users to see and edit their information
```

**Let Copilot Shell explore first**

- Before making changes, let it understand your code:

```
Analyze the database schema
```

## Getting Help

- **Within Copilot Shell**: Type `/help` or just ask "how do I..."
- **Documentation**: Browse the [User Guide](../../README.md)
- **Bug reports**: Submit an Issue in the project repository

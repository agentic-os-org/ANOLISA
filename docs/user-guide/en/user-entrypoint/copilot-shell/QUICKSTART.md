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

## Common Commands

| Command | Function | Example |
|---------|----------|---------|
| `cosh` | Launch Copilot Shell | `cosh` |
| `/auth` | Switch authentication method | `/auth` |
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

## Getting Help

- **Within Copilot Shell**: Type `/help` or just ask "how do I..."
- **Bug reports**: Submit an Issue in the project repository
# Quick Start

> 👏 Welcome to Copilot Shell!

This quickstart guide will have you using AI-powered coding and system administration in just a few minutes. By the end, you'll understand how to use Copilot Shell for common development and operations tasks.

## Before you begin

Make sure you have:

- A **terminal** on an Alibaba Cloud Linux (Alinux) machine
- A code project or system to manage
- One of the supported authentication methods configured (see [Authenticate](#step-2-authenticate) below)

## Step 1: Install Copilot Shell

### RPM (recommended)

```bash
sudo yum install copilot-shell
```

### Build from source

Requires [Node.js 20+](https://nodejs.org/download). You can check your version with `node -v`.

```bash
cd deprecated/copilot-shell
make build
```

After a successful build, the bundled binary is available at `dist/cli.js`.

## Step 2: Authenticate

When you start Copilot Shell for the first time, you'll need to configure authentication:

```bash
cosh
```

Use the `/auth` command inside the session to choose your provider:

```bash
/auth
```

### Supported providers

| Provider | Description |
|----------|-------------|
| Aliyun Authentication | Default. On ECS: auto-detects and launches web auth (browser link + QR code). No ECS: enter AK/SK directly. |
| Qwen OAuth | Free tier with 1,000 requests/day — follow on-screen prompts |
| Custom Provider | Any OpenAI-compatible endpoint — DashScope, DashScope Coding Plan, DeepSeek, Kimi, GLM, MiniMax, or your own |

> [!tip]
>
> To switch accounts or providers later, use the `/auth` command within Copilot Shell.

## Step 3: Start your first session

Open your terminal in any project directory and start Copilot Shell:

```bash
cd /path/to/your/project
cosh
```

You'll see the welcome screen with your session information and recent conversations. Type `/help` for available commands.

> [!note]
>
> You can also use the aliases `co` or `copilot` instead of `cosh`.

## Step 4: Enable sandbox hooks (recommended)

Copilot Shell ships with built-in sandbox-guard hooks that intercept tool calls and enforce security policies — preventing unauthorized file system access or dangerous operations. These hooks are not active until you install them.

Inside Copilot Shell, run:

```
/hooks install
```

This command copies the bundled `sandbox-guard.py` script to `~/.copilot-shell/hooks/` and registers it in your user settings. You only need to run this once — the configuration is saved and persists across sessions.

> [!note]
>
> This step requires `agent-sec-core` (linux-sandbox) to be installed at `/usr/local/bin/linux-sandbox`. When a dangerous command is detected, `sandbox-guard.py` wraps it inside the `linux-sandbox` binary for execution. If you built ANOLISA using the default `./scripts/build-all.sh`, `agent-sec-core` is included and installed automatically.

> [!tip]
>
> To verify the hooks are active, run `/hooks list` inside Copilot Shell. You should see `sandbox-guard` and `sandbox-failure-handler` listed as enabled.

## Chat with Copilot Shell

### Ask your first question

Copilot Shell will analyze your files and provide answers. You can ask about your codebase:

```
explain the folder structure
```

Or ask about system state:

```
show me the current disk usage and top memory consumers
```

> [!note]
>
> Copilot Shell reads your files as needed — you don't have to manually add context. It also has access to OS-level skills for system administration tasks.

### Make your first code change

Try a simple coding task:

```
add a hello world function to the main file
```

Copilot Shell will:

1. Find the appropriate file
2. Show you the proposed changes
3. Ask for your approval
4. Make the edit

> [!note]
>
> Copilot Shell always asks for permission before modifying files. You can approve individual changes or enable "Accept all" mode for a session.

### System administration

Copilot Shell integrates with OS-level skills for common operations tasks:

```
check if there are any failed systemd services
```

```
analyze the nginx access log for the top 10 IPs in the last hour
```

```
set up a cron job to clean /tmp every day at 3am
```

### Use Git with Copilot Shell

Git operations become conversational:

```
what files have I changed?
```

```
commit my changes with a descriptive message
```

```
create a new branch called feature/quickstart
```

```
help me resolve merge conflicts
```

### Fix a bug or add a feature

Describe what you want in natural language:

```
add input validation to the user registration form
```

Or fix existing issues:

```
there's a bug where users can submit empty forms - fix it
```

Copilot Shell will:

- Locate the relevant code
- Understand the context
- Implement a solution
- Run tests if available

### Drop into an interactive shell

Use the `/bash` command to enter an interactive shell from within Copilot Shell:

```
/bash
```

Type `exit` to return to the Copilot Shell session.

### Other common workflows

**Refactor code**

```
refactor the authentication module to use async/await instead of callbacks
```

**Write tests**

```
write unit tests for the calculator functions
```

**Update documentation**

```
update the README with installation instructions
```

**Code review**

```
review my changes and suggest improvements
```

> [!tip]
>
> **Remember**: Copilot Shell is your AI pair programmer and sysadmin assistant. Talk to it like you would a helpful colleague — describe what you want to achieve, and it will help you get there.

## Essential commands

Here are the most important commands for daily use:

| Command | What it does | Example |
|---------|--------------|----------|
| `cosh` | Start Copilot Shell | `cosh` |
| `/auth` | Change authentication method | `/auth` |
| `/hooks install` | Install sandbox-guard hooks (run once after install) | `/hooks install` |
| `/hooks list` | Show all registered hooks and their status | `/hooks list` |
| `/help` | Display help for available commands | `/help` or `/?` |
| `/bash` | Drop into an interactive shell | `/bash` |
| `/model` | Switch between configured models | `/model` |
| `/compress` | Replace chat history with summary to save tokens | `/compress` |
| `/clear` | Clear terminal screen | `/clear` (shortcut: `Ctrl+L`) |
| `/theme` | Change visual theme | `/theme` |
| `/language` | View or change language settings | `/language` |
| → `ui [lang]` | Set UI interface language | `/language ui zh-CN` |
| → `output [lang]` | Set LLM output language | `/language output Chinese` |
| `/quit` | Exit Copilot Shell | `/quit` or `/exit` |

## Pro tips for beginners

**Be specific with your requests**

- Instead of: "fix the bug"
- Try: "fix the login bug where users see a blank screen after entering wrong credentials"

**Use step-by-step instructions**

- Break complex tasks into steps:

```
1. create a new database table for user profiles
2. create an API endpoint to get and update user profiles
3. build a webpage that allows users to see and edit their information
```

**Let Copilot Shell explore first**

- Before making changes, let it understand your code:

```
analyze the database schema
```

**Save time with shortcuts**

- Press `?` to see all available keyboard shortcuts
- Use Tab for command completion
- Press ↑ for command history
- Type `/` to see all slash commands

## Getting help

- **In Copilot Shell**: Type `/help` or ask "how do I..."
- **Documentation**: Browse the [User Guide](../../README.md)
- **Issues**: File an issue on the project repository

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

- **OS**: Linux or macOS. Windows is not supported by any method below; use WSL2 there.
- **Architecture**: x86_64 or arm64
- **Network**: Internet access required
- **Shell**: Bash

Per-method prerequisites:

| Method | Requires | Installs |
|--------|----------|----------|
| A — anolisa CLI | `curl` | full component suite including adapters |
| B — npm | Node.js 16.7+ with `npm`, glibc Linux or macOS | `tokenless`, `rtk`, adapter resources |
| C — curl | `curl`, `tar`, plus Node.js 16.7+ **or** a Rust toolchain (`cargo`) depending on the path taken | npm path: as Method B. Source-build path (Linux only): the `tokenless` CLI only |
| D — Skill | whichever of A/B/C the agent runs | as that method |

The package postinstall enables the `claude-code` adapter by running that adapter's own `install.sh`, which registers the plugin with the Claude CLI when one is reachable; with no Claude CLI it skips registration and says so. That registration lives in Claude's own configuration, outside both the npm prefix and the adapter directory, so deregister it with the adapter's `uninstall.sh` (or `scripts/uninstall.sh`) rather than by deleting the package.

The source-build path is validated on Linux only, and the installer refuses it on macOS: there it either takes the npm path or exits with an error, and never runs `cargo`. On musl Linux (Alpine) the source build is the only available path, because prebuilt binaries are glibc-linked.

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

On Alinux with the YUM repository configured, the managed RPM is the alternative
to this method, and the documented install priority puts it ahead of every route
below:

```bash
sudo yum install anolisa tokenless
sudo anolisa --install-mode system adopt tokenless
```

`adopt` records the directly installed RPM in system state so the adapter commands
can use its component contract. Methods B–D create no anolisa component record, so
prefer Method A or the RPM route whenever either is available.

**Method B: npm Global Install**

Requires Node.js 16.7+ — the release `fs.cpSync` arrived in, which the package postinstall needs to place the adapter resources. Installs prebuilt binaries for your platform:

```bash
npm install -g anolisa-tokenless
```

This automatically installs the `tokenless` and `rtk` binaries plus the framework adapter resources. (`toon` is no longer a standalone binary — TOON encoding is a `tokenless` subcommand.)

The adapter resources land in `~/.local/share/anolisa/adapters/tokenless`, which is shared with the anolisa CLI. Ownership there has to be **proven**, not assumed: the postinstall only refreshes a tree carrying the marker (`.tokenless-owner`) that this package or the standalone curl installer wrote. Anything else is kept unchanged and reported — a managed anolisa component install, a tree left by a release from before the marker existed, or a directory somebody copied by hand — because nothing about its content says who put it there, and every framework registration pointing into it would dangle otherwise. The copy this package ships stays available inside the package under `adapters/tokenless`; pass `ANOLISA_TOKENLESS_FORCE_ADAPTERS=1` to take the directory over anyway.

The npm package declares `os: linux, darwin`, so this method is unavailable on Windows and on musl Linux.
Intel macOS (x86_64) has no published platform package yet either: `@anolisa/tokenless-darwin-x64` is a
release build target, not a registry artifact, so Intel macOS has **no supported install route in this release**.
Method A does not cover the platform and Method C only reaches its npm path there. Do not pass
`TOKENLESS_FORCE_BUILD=1` on macOS — the installer refuses the source build on that platform and exits with an
error instead of running `cargo`. Use Linux or Apple Silicon macOS until that package is published.

**Method C: Standalone curl Install**

One-liner that tries npm first and builds from source instead. The method is chosen **before** npm runs (npm unavailable, musl Linux, or `TOKENLESS_FORCE_BUILD=1`). Once `npm install` has been invoked the installer does not switch method automatically — npm's exit status cannot prove its postinstall left no framework registration behind — so a failure at that stage is reported as an incomplete installation, keeping and naming whatever could not be rolled back. Re-run it, or choose the source build explicitly with `TOKENLESS_FORCE_BUILD=1`:

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | bash
```

Options via environment variables. Pass them to `bash`, not to `curl`, otherwise the installer never sees them:

```bash
# Pin a specific version
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | TOKENLESS_VERSION=0.7.4 bash

# Custom install directory
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | TOKENLESS_INSTALL_DIR=/usr/local/bin bash

# Force the source build even when npm is available (needs cargo; Linux only)
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/install.sh | TOKENLESS_FORCE_BUILD=1 bash
```

A pinned version is a hard pin: the source build downloads only the matching `tokenless/v<VERSION>` tag, and fails if that tag does not exist. It never falls back to the `main` branch.

The source-build path installs the `tokenless` CLI only — no `rtk` and no adapter resources. Treat it as a CLI-only install (see Step 3). It runs on Linux only.

The installer records every path it created in `~/.local/share/tokenless/install-receipt`, which the uninstall step below relies on.

### Step 2: Verify Installation

```bash
tokenless --version
```

If `command not found`, ensure the install directory is in PATH:

```bash
export PATH="$HOME/.local/bin:$PATH"
```

### Step 3: Enable for an Agent Framework (Optional)

Pick the enable path that matches how Step 1 installed Tokenless. Installation only places files on disk; it never registers Tokenless with an agent.

**Installed via Method A (anolisa CLI)** — the component is recorded, so use the adapter commands:

```bash
anolisa adapter scan
anolisa adapter enable tokenless <agent>
anolisa adapter status tokenless
```

**Installed via Method B, or via Method C's npm path** — the package postinstall copied the adapters to `~/.local/share/anolisa/adapters/tokenless/`, but there is **no anolisa component record**, so `anolisa adapter enable` is not available. Run the framework's bundled script instead:

```bash
ls ~/.local/share/anolisa/adapters/tokenless/          # confirm the directory exists
bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh
```

**Installed via Method C's source-build path** — CLI-only. No `rtk` and no adapter resources were installed, so no adapter can be enabled. Use the `tokenless` subcommands directly, or reinstall through Method A or B for agent integration.

> **Note:** The script paths below are examples for npm-installed adapters. Not all frameworks ship a standalone install script, and directory names may differ. Always check `ls ~/.local/share/anolisa/adapters/tokenless/` first. For anolisa CLI installs, `anolisa adapter enable tokenless <framework>` is the recommended path.

| Agent | anolisa CLI install | npm-based install |
|-------|---------------------|-------------------|
| cosh / Copilot Shell | `anolisa adapter enable tokenless cosh` | no bundled script — cosh integrates through the ANOLISA cosh extension, so use the anolisa CLI path |
| OpenClaw | `anolisa adapter enable tokenless openclaw` | `bash ~/.local/share/anolisa/adapters/tokenless/openclaw/scripts/install.sh` |
| Hermes | `anolisa adapter enable tokenless hermes` | `bash ~/.local/share/anolisa/adapters/tokenless/hermes/scripts/install.sh` |
| Qoder | `anolisa adapter enable tokenless qoder` | `bash ~/.local/share/anolisa/adapters/tokenless/qoder/scripts/install.sh` |
| Claude Code | `anolisa adapter enable tokenless claude-code` | `bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh` |
| Codex | `anolisa adapter enable tokenless codex` | `bash ~/.local/share/anolisa/adapters/tokenless/codex/scripts/install.sh` |
| Qwen Code | `anolisa adapter enable tokenless qwencode` | `bash ~/.local/share/anolisa/adapters/tokenless/qwencode/scripts/install.sh` |

Each bundled adapter also ships a matching `scripts/uninstall.sh` next to its `install.sh`; use it to disable that framework again. Run it **before** removing the adapter resources — a registration points into that directory, so deleting the directory first leaves the framework hooked, plugged or symlinked to a path that no longer exists. The [Uninstall](#uninstall) section gives the full order per install method.

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

Uninstall must match how Tokenless was installed, and must only remove files that
installation created. Do not run a blanket `rm` across `~/.local/bin` or the
adapter tree — those paths may belong to another method.

**anolisa CLI installation (Method A):**
```bash
anolisa uninstall tokenless
```

**Standalone curl installation (Method C):** the installer recorded its method,
npm prefix, install directory, and every file it created in
`~/.local/share/tokenless/install-receipt`. Remove exactly those:

```bash
curl -fsSL https://raw.githubusercontent.com/alibaba/anolisa/main/src/tokenless/scripts/uninstall.sh | bash

# Preview first, or also delete the runtime data directory:
#   ... | bash -s -- --dry-run
#   ... | bash -s -- --purge
```

The receipt-driven uninstaller removes the recorded binaries from the recorded
install directory (including a custom `TOKENLESS_INSTALL_DIR`), uninstalls the
global npm package from the recorded prefix when the npm path was used, removes
the adapter directory only when that npm run created it, strips only the PATH
line the installer appended, and leaves `~/.tokenless` (stats and stash data)
in place unless `--purge` is passed. A source-build install recorded no adapters,
so none are removed.

`~/.local/share/anolisa/adapters/tokenless` is shared with the anolisa CLI and
with a direct `npm install -g`. When that directory already belonged to another
installation, the npm postinstall leaves it untouched, the curl installer compares
it against the snapshot it took beforehand and only restores it if something
really did replace it, does not record the directory, and says so — the receipt
then owns only the launcher links, and this uninstaller leaves the shared
resources and their framework registrations alone. A snapshot that cannot be taken
at all stops the run before `npm install -g` replaces anything.

Each recorded file also carries its sha256, and the adapter directory carries the
digest of its stamped `manifest.json`. A path whose content no longer matches was
taken over by another installer (anolisa, a manual `npm install -g`) and is kept.
Before deleting the adapter directory the uninstaller runs each bundled
framework's own `scripts/uninstall.sh`, so an enabled framework registration is
removed rather than left pointing at a deleted directory. If one of them fails,
the uninstaller **stops short**: the adapter resources and the receipt are both
kept and it exits non-zero, so the registration still resolves and the script the
warning names still exists. Fix that framework and re-run to finish.

Content alone is not proof of ownership — a newer anolisa or npm install of the
same version leaves byte-identical binaries and manifests behind. So the receipt
also records this install's id, the link target each launcher resolves to, and an
ownership marker (`.tokenless-owner`) inside the adapter tree and the npm module
directory. A path whose recorded identity or marker no longer matches belongs to
that newer install and is kept, including its framework registrations and its npm
package.

Re-running the installer with a different method retires the previous receipt, so
switching npm → source does not orphan the `rtk` launcher, the npm global package
or the adapter tree — but only once the replacement is verified. The previous
install is moved aside first and put back if the new one fails, so a missing tag,
a build error or an unwritable directory leaves the working install and its
receipt exactly as they were. That guarantee is fail-closed: if the copy aside
cannot be made at all — no staging directory, or a recorded file that cannot be
read — the installer stops before writing anything instead of replacing an install
it could not put back. An npm → npm upgrade keeps a copy of the previous
package payload for the same reason: the launcher links resolve into it, so a
broken new binary is rolled back instead of being left behind a working-looking
link. The new receipt is written to a temporary, moved into place and flushed
before anything is retired; when it cannot be written and the previous receipt
cannot be removed either, the run fails and restores the previous install,
because a stale receipt would let a later uninstall delete the new one. Where the previous npm prefix and the install
directory overlap (`--prefix ~/.local` with `~/.local/bin`), that package is
retired by hand instead of through `npm uninstall`, which would otherwise take the
freshly installed CLI with it. A failed npm attempt is rolled back the same way,
so the source-build fallback never inherits an unowned package, `rtk` link or
adapter tree.

**Direct npm installation (Method B), not through the curl script:** no receipt
exists, so undo the three things the install did, in this order. Deregister the
frameworks enabled in Step 3 **first**, while their resources are still on disk —
otherwise every hook entry, plugin directory and symlink keeps pointing at a
directory that no longer exists:

```bash
# 1. Once per framework enabled in Step 3 (claude-code, codex, hermes, openclaw,
#    opencode, qoder, qwencode, qwenpaw).
bash ~/.local/share/anolisa/adapters/tokenless/<framework>/scripts/uninstall.sh

# 2. Remove the global package.
npm uninstall -g anolisa-tokenless

# 3. Remove the adapter resources the package postinstall created.
rm -rf ~/.local/share/anolisa/adapters/tokenless
```

The uninstaller stops short rather than half-finishing, and exits non-zero when it
does: a framework registration it could not remove keeps the adapter resources and
the receipt, and a global npm package it could not remove (no npm on PATH) keeps
the package, that prefix's launcher links and the receipt. Fix the cause and
re-run — the receipt is what makes the retry possible.

This is the order the receipt-driven uninstaller uses internally, and the order
the Tokenless troubleshooting page prescribes for an npm installation. Step 3
also removes the resources a Method C npm install writes, so skip it when
another Tokenless installation on this machine still needs them — and skip it
whenever the postinstall reported that it kept a managed adapter tree, because
that directory was never this npm install's to delete.

If the receipt is missing (for example after a manual cleanup), the uninstaller
exits with an error rather than guessing; remove `<install-dir>/tokenless` and
`<install-dir>/rtk` yourself, using the directory you actually installed into.

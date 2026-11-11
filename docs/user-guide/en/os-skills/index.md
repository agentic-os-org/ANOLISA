# OS Skills

OS Skills is a system management and DevOps skill library for AI Agents. It provides pre-built skills that enable Agents to perform common system administration and automation tasks.

---

## Overview

OS Skills covers three main areas:

- **System Administration** — user management, service control, package operations, filesystem tasks
- **Cloud Integration** — cloud resource queries, instance management, network configuration
- **DevOps Automation** — CI/CD pipeline management, container operations, deployment workflows

---

## Installation

```bash
anolisa install os-skills
```

From a source checkout, developers can install the collection for cosh:

```bash
make -C src/os-skills install
```

For a regular user, this keeps the existing `~/.copilot-shell/skills/`
directory; root installs to `/usr/local/share/anolisa/skills/`.
`AWPRJ` defaults to `anolisa` for system resources and can be overridden.
Use `SKILLS_DIR=/absolute/path` to select another collection directory;
use the same value when uninstalling. Start a new cosh session and run
`/skills` to check the installed collection.

Local system copies take precedence over RPM copies in
`/usr/share/anolisa/skills/` when their skill names match. Use one delivery
method for the collection; when managing an older source installation,
set `SKILLS_DIR` to its original directory.

`make uninstall` removes only the skill directories recorded by
`make install` in `<datadir>/anolisa/components/os-skills/install-manifest`.
A collection without that manifest is left untouched and the target exits
non-zero, so it never removes a same-name skill directory that os-skills did
not install. `make install` follows the same rule in the other direction: a
same-name directory it did not create is refused unless you pass
`REPLACE_UNOWNED=1`, which replaces it and records it for later removal. Only
`1` enables this; `0` or an unset value keeps the refusal, and any other value
is rejected before anything changes. An
installation made before the manifest existed therefore needs one
`REPLACE_UNOWNED=1 make install` before it can be removed. Recorded directories
are managed as a whole and replaced on every install, so keep personal files out
of them and give your own skills a different name. If an install fails or is
interrupted, it puts the previous skills back and leaves the manifest unchanged.
An interruption after the manifest has recorded the new skills keeps them
installed instead and asks you to run `make install` again to finish.
When it cannot undo a step, for example because the collection directory
became read-only, the error names what is left: a new copy it could not remove,
or the `.os-skills.*/.old` directory that still holds a previous skill. Names
recorded by earlier installs stay in the manifest, so a skill dropped from a
newer release is removed as well. RPM installations are removed by the package
manager and raw
`anolisa install` deployments by `anolisa uninstall os-skills`, not by
`make uninstall`.

A receipt belongs to a single collection: `make install` refuses a directory
that the receipt does not record, and `make uninstall` removes the receipt
together with the skills, so pass a separate `MANIFEST=` for each collection
you keep. Uninstalling one collection keeps the receipts and shared component
metadata of the others.

---

## Quick Start

Once installed, OS Skills are available to any ANOLISA-compatible Agent runtime. The Agent can invoke skills via natural language:

```
> "Check disk usage on all mounted filesystems"
> "Restart the nginx service"
> "Show running containers and their resource usage"
```

---

## Skill Categories

### System Administration

| Skill | Description |
|-------|-------------|
| `disk-usage` | Check filesystem disk usage |
| `service-ctl` | Start/stop/restart system services |
| `process-mgmt` | List and manage processes |
| `user-mgmt` | User and group management |
| `package-ops` | Package install/remove/query |

### DevOps Automation

| Skill | Description |
|-------|-------------|
| `container-ops` | Docker/Podman container management |
| `log-analysis` | Search and analyze system logs |
| `network-diag` | Network diagnostics (ping, traceroute, port check) |
| `cron-mgmt` | Cron job management |

---

## Usage with Agent Runtimes

OS Skills integrates with cosh and other ANOLISA-compatible runtimes automatically. Skills are discovered at startup and made available to the Agent's tool inventory.

```bash
# Verify skills are loaded
anolisa status os-skills
```

---

## Configuration

Configuration file: `~/.config/os-skills/config.toml`

```toml
[skills]
# Enabled skill categories
enabled = ["system", "devops"]

[safety]
# Require confirmation for destructive operations
confirm_destructive = true
```

---

## See Also

- [anolisa CLI](../getting-started/anolisa-cli.md)

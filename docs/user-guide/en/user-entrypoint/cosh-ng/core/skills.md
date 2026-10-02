# Skills

[中文版](../../../../zh/user-entrypoint/cosh-ng/core/skills.md)

Skills are reusable instructions for recurring operating tasks. Add a Skill,
then let the Agent load it when the task matches.

## Manage Skills in cosh

```text
/skills
/skills detail <name>
/skills enable <name>
/skills disable <name>
```

Use `detail` to check which source won when names collide. Disabled Skills are
not offered to the Agent.

## Where Skills are loaded

The first matching name wins, in this order:

1. `<workspace>/.copilot-shell/skills/`
2. Paths in `skills.custom_paths`
3. `~/.copilot-shell/skills/`
4. `$XDG_DATA_HOME/anolisa/skills/` (default `~/.local/share/anolisa/skills/`)
5. Skill directories from Extensions
6. `/usr/local/share/anolisa/skills/`
7. `/usr/share/anolisa/skills/`

The raw backend of `anolisa install os-skills` uses the user data directory
for user installs and `/usr/local/share/anolisa/skills/` for system installs.
An unset, empty, or relative `XDG_DATA_HOME`, or one containing `.` or `..`
path segments, uses the default above. Within custom or Extension directories,
the first directory containing a name wins for both listing and loading.

For a custom system prefix, run the cosh installed under that same prefix.
For example, an installation under `/opt/x` searches
`/opt/x/usr/local/share/anolisa/skills/` before
`/opt/x/usr/share/anolisa/skills/`. These replace the host system roots;
user and project paths keep their priority. Use `skills.custom_paths` to
include skills installed under a different prefix.

Existing directories are watched and rescanned after changes.

## Create a Skill

The preferred layout is `<skill-name>/SKILL.md`; a flat `<name>.md` file is
also supported.

```markdown
---
name: service-health
description: Inspect a systemd service and summarize actionable evidence
allowedTools:
  - shell
---

# Service health

Inspect status and recent logs before proposing a change. Ask for approval
before restarting the service.
```

`name` and `description` are required. `allowedTools` is optional and may be a
YAML list or a comma-separated string.

## Add shared directories

Use `skills.custom_paths` to search team-maintained directories without copying
their files:

```toml
[skills]
custom_paths = ["~/team-skills", "/opt/company/skills"]
```

Paths expand `~`, `${VAR}`, and `$VAR`. Project paths are relative to the
workspace where Core starts.

# Skills

[中文版](../../../zh/cosh-ng/core/skills.md)

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
4. The user installation's selected directory, then
   `$XDG_DATA_HOME/cosh-ng/skills/` and `$XDG_DATA_HOME/anolisa/skills/`
5. Skill directories from Extensions
6. The system installation's additional directory and the `cosh-ng/skills/`
   and `anolisa/skills/` directories under `$XDG_DATA_DIRS`
7. Additional directories in `skills.system_paths`

A recorded system directory already in the standard list keeps that position;
only additional installation paths take precedence over the standard directories.
With the default system data directories, recording `/usr/share/anolisa/skills/`
in an RPM installation preserves the priority of `/usr/local/share/anolisa/skills/`
over it.

The raw backend of `anolisa install os-skills` uses the user data directory
for user installs and `/usr/local/share/anolisa/skills/` for system installs.
`XDG_DATA_HOME` defaults to `~/.local/share`; `XDG_DATA_DIRS` defaults to
`/usr/local/share:/usr/share`. Unset or empty variables use these defaults.
An invalid `XDG_DATA_HOME` also uses its default; invalid entries in
`XDG_DATA_DIRS` are ignored. Paths must be absolute without `.` or `..` segments.
Within each level, the first directory containing a name wins for both listing
and loading.

For a custom system prefix, run the cosh installed under that same prefix.
For example, an installation under `/opt/x` searches
`/opt/x/usr/local/share/anolisa/skills/` before
`/opt/x/usr/share/anolisa/skills/`. These replace the host system roots;
user and project paths keep their priority. Source installations also follow
the selected `PREFIX`, `AWPRJ`, and `SKILLDIR`. Use `skills.system_paths` to
include other installed collections at system priority.

Existing directories are watched and rescanned after changes.

## Create a Skill

The preferred layout is `<skill-name>/SKILL.md`; a flat `<name>.md` file is
also supported. A configured path may point directly to a directory containing
`SKILL.md` for a single Skill.

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

For additional installed collections, set `skills.system_paths` in the system
or user configuration. These directories are searched after the default system
directories and do not override user or Extension Skills:

```toml
[skills]
system_paths = ["/usr/local/share/agent-sec-core/skills", "/usr/local/share/ws-ckpt/skills"]
```

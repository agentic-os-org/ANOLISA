# Skills System

Skills enable Copilot Shell to perform domain-specific tasks such as system
diagnostics, security audits, and project initialization. Skills are defined
as Markdown files, supporting local loading and community skills via Clawhub.

## View Available Skills

```
/skills
```

Lists all discovered skills in the current session.

## Skill Discovery Priority

Copilot Shell searches for skills in the following priority order (higher
priority overrides same-named skills at lower priority):

1. **Project-level skills**: `.copilot-shell/skills/`
2. **Custom paths**: Directories configured via `skills.customPaths`
3. **User-level skills**: `~/.copilot-shell/skills/`
4. **Extension skills**: `skills/` under extension directories
5. **System-level skills**: `/usr/share/anolisa/skills`

## Skill Structure

Each skill is a directory containing a `SKILL.md` file:

```
~/.copilot-shell/skills/
└── my-skill/
    └── SKILL.md
```

`SKILL.md` is a Markdown file describing the skill's name, trigger conditions,
execution steps, and other information. The AI follows these instructions to
complete tasks.

## Custom Skill Paths

Add additional skill search directories via configuration:

```json
{
  "skills": {
    "customPaths": [
      "~/my-skills",
      "/opt/team-skills"
    ]
  }
}
```

Paths support `~` (home directory) and `$VAR`/`${VAR}` (environment variable)
expansion.

## Clawhub Remote Skills

Clawhub is Copilot Shell's remote skill registry, providing community-shared
skills.

### Search Skills

```
/clawhub search <keyword>
```

### Install Skills

```
/clawhub install <skill-name>
```

### Update Skills

```
/clawhub update <skill-name>
```

### Configure Registry URL

```json
{
  "clawhub": {
    "registry": "https://cn.clawhub-mirror.com"
  }
}
```

## OS Skills

ANOLISA ships with a set of operating system skills (`os-skills`) covering:

- **System Administration**: User management, service management, network configuration
- **Monitoring & Performance**: System resource analysis, performance diagnostics
- **Security**: Security audits, vulnerability scanning
- **DevOps**: CI/CD, container management
- **AI**: AI Agent deployment

These skills become automatically available after ANOLISA installation.

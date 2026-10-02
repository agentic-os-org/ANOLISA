# @agentsight/dsh-plugin

AgentSight observability plugin for DeepSeek Harness (dsh).

## Install

From the monorepo checkout (local path):

```bash
cd src/agentsight/dsh-plugin
pnpm install && pnpm run build
dsh plugin --profile web add .
```

Or via `--patch` for a quick trial (no profile modification):

```bash
dsh --profile web --patch ./src/agentsight/dsh-plugin/cordis.patch.yml
```

## Capabilities

| Tool | Description |
|------|-------------|
| `agentsight_status` | Report plugin status and version |

## Development

```bash
pnpm install
pnpm run build
```

## License

Apache-2.0

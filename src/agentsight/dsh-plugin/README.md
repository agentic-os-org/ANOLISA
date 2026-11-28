# @agentsight/dsh-plugin

AgentSight observability plugin for DeepSeek Harness (dsh).

## Install

From the monorepo checkout (local path), use Node.js 22.13 or later and pnpm 11.25.0:

```bash
cd src/agentsight/dsh-plugin
pnpm install --frozen-lockfile --ignore-scripts && pnpm run build
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

`make -C src/agentsight build-dsh-plugin` uses the same lockfile via pnpm 11.25.0.
The aggregate `make build` and RPM build use this target.

To build from within this directory:

```bash
pnpm install --frozen-lockfile --ignore-scripts
pnpm run build
```

## License

Apache-2.0

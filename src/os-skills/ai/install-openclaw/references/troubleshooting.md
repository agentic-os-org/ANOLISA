# OpenClaw Troubleshooting

Use this when non-interactive OpenClaw installation or Alibaba Cloud Model Studio configuration fails.

## Quick Checks

Always start by rerunning the one entry script. It checks Node.js/npm, installs
missing prerequisites with `dnf`/`yum` on Alibaba Cloud Linux 4 or Anolis-like
images, installs OpenClaw with npm, writes config, and restarts the gateway.

```bash
python3 scripts/install_openclaw.py \
  --billing payg \
  --api-key "$BAILIAN_API_KEY"
```

If global npm install needs `/usr/local` permissions, the script automatically
uses `sudo env NPM_CONFIG_REGISTRY=... npm install -g openclaw@latest`.

```bash
openclaw --version
openclaw models list
openclaw agent --message "hello" --agent main
openclaw gateway health
openclaw status
openclaw plugins list
openclaw channels status --probe
openclaw logs --follow
```

If an agent shell appears stuck during gateway startup, use `scripts/install_openclaw.py`. It starts OpenClaw through `openclaw gateway install` and `openclaw gateway restart`, waits for the gateway port, then runs an agent message smoke test. Do not run `openclaw gateway --force` as a long-lived foreground command from an agent shell.

If the script stops because the gateway port is used by a non-OpenClaw process, ask the user whether to stop that process or choose another port with `--gateway-port`.

If `openclaw agent --message ...` prints `EMBEDDED FALLBACK`, do not count the model smoke test as healthy. Rerun the installer first so it can reset stale read-only local operator device state, then check whether the gateway itself is healthy:

```bash
openclaw gateway health
openclaw status
journalctl --user -u openclaw-gateway.service -n 200 --no-pager
```

If the fallback says `pairing required` or `scope upgrade pending approval`, rerun the installer instead of manually approving devices. The script clears stale read-only local operator pairing state and retries the message smoke test:

```bash
python3 scripts/install_openclaw.py \
  --billing payg \
  --api-key "$BAILIAN_API_KEY"
```

Check the config:

```bash
python3 - <<'PY'
import json, os
p = os.path.expanduser('~/.openclaw/openclaw.json')
d = json.load(open(p))
print('primary:', d.get('agents', {}).get('defaults', {}).get('model', {}).get('primary'))
print('gateway:', d.get('gateway', {}))
print('providers:', list(d.get('models', {}).get('providers', {}).keys()))
for k, v in d.get('models', {}).get('providers', {}).items():
    print(k, v.get('api'), v.get('baseUrl'))
print('channels:', d.get('channels', {}).keys())
PY
```

If gateway reports `Unrecognized key: "aliyunModelStudio"`, remove that root key from `~/.openclaw/openclaw.json`. It is not accepted by the OpenClaw schema.

## API Key or 401

Symptoms:

- `No API key found for provider ...`
- `HTTP 401: Incorrect API key provided`
- gateway starts but model calls fail

Likely causes:

- API key is empty, expired, copied with whitespace, or from a different billing plan.
- Base URL does not match the billing plan.
- OpenClaw has stale cached model config under `~/.openclaw/agents/main/agent/models.json`.

Fix:

```bash
python3 scripts/install_openclaw.py \
  --billing coding \
  --api-key "$CODING_PLAN_API_KEY" \
```

If stale cache is suspected, delete only the cached provider section or move the file aside, then restart OpenClaw:

```bash
mv ~/.openclaw/agents/main/agent/models.json ~/.openclaw/agents/main/agent/models.json.bak
openclaw gateway restart
```

## Billing/Base URL Mismatch

Expected OpenClaw provider config:

| Billing | Provider ID | Base URL |
|---|---|---|
| Pay-as-you-go | `bailian` | `https://dashscope.aliyuncs.com/apps/anthropic` |
| Pay-as-you-go Singapore | `bailian` | `https://dashscope-intl.aliyuncs.com/apps/anthropic` |
| Coding Plan | `bailian-coding-plan` | `https://coding.dashscope.aliyuncs.com/apps/anthropic` |
| Token Plan | `bailian-token-plan` | `https://token-plan.cn-beijing.maas.aliyuncs.com/apps/anthropic` |

Do not mix `/compatible-mode/v1` endpoints into OpenClaw's Alibaba Cloud Model Studio Anthropic config. Those endpoints are for OpenAI-compatible clients, not the documented OpenClaw provider shape.

## Wrong Default Model

Symptoms:

- `agents.defaults.model.primary` points to `qwen/...`, `modelstudio/...`, or another old provider.
- `openclaw models list` does not show the intended `bailian-*` provider.

Fix by re-running the script with the intended billing mode and model:

```bash
python3 scripts/install_openclaw.py \
  --billing token \
  --api-key "$BAILIAN_TOKEN_PLAN_API_KEY" \
  --model-id qwen3.6-plus
```

## Gateway Auth

For local single-machine setup, the script defaults to:

```json
{
  "gateway": {
    "mode": "local",
    "bind": "loopback",
    "auth": { "mode": "none" }
  }
}
```

For remote/shared access, do not leave auth disabled. Re-run with:

```bash
python3 scripts/install_openclaw.py \
  --gateway-auth-mode keep \
  --doctor-fix \
  --api-key "$BAILIAN_API_KEY"
```

## DingTalk Plugin Issues

Default plugin:

```bash
openclaw plugins install @soimy/dingtalk
```

Verify:

```bash
openclaw plugins list
openclaw channels status --probe
```

If schema validation says `must NOT have additional properties`, remove unsupported fields from `channels.dingtalk` and rerun the installer.

## Device Identity Required

If the dashboard/browser reports `device identity required`, reset pending devices before opening the dashboard again:

```bash
openclaw devices clear --pending --yes
openclaw dashboard --no-open
```

If needed:

```bash
openclaw devices clear --pending --yes
openclaw dashboard --no-open
```

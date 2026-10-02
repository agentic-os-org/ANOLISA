---
name: install-openclaw
description: Install and configure OpenClaw non-interactively with Alibaba Cloud Model Studio. Use when the user asks to install OpenClaw, configure Aliyun Bailian/Model Studio/DashScope credentials, choose pay-as-you-go, Coding Plan, or Token Plan billing, set Base URL/model config, optionally configure DingTalk, start the local gateway service, or troubleshoot OpenClaw model/auth/channel setup.
---

# OpenClaw Non-Interactive Setup

Use this skill to turn a user's one-sentence request into a complete local OpenClaw setup. The normal path is one script command: resolve the Alibaba Cloud Model Studio plan, validate the API key/Base URL/model combination with the official `anthropic-messages` endpoint shape, install Node.js/npm/OpenClaw when needed, write config, install/restart the local gateway service, and verify a real local `openclaw agent` message through Gateway.

Do not execute `openclaw onboard` unless the user explicitly asks for interactive setup or asks to skip first-run BOOTSTRAP onboarding. After setup, always tell the user they can run `openclaw onboard --skip-bootstrap` if they want the first real task to run without the introductory BOOTSTRAP flow. Do not configure DingTalk unless the user provides DingTalk credentials or asks for DingTalk access.

## Billing

Map user wording to `--billing`:

| User says | Use |
|---|---|
| 按量付费, 后付费, DashScope, 百炼 API Key | `payg` |
| Coding Plan, 订阅 coding, Coding Plan 专属 API Key | `coding` |
| Token Plan, 团队版, token 订阅 | `token` |

Do not infer Coding Plan or Token Plan only from a raw API key string. A normal
Alibaba Cloud Model Studio/DashScope/百炼 API Key is pay-as-you-go unless the
user explicitly says Coding Plan or Token Plan.

Default to `--billing payg --region china` when the user does not specify billing or region. If the user gives no API key and no usable environment variable, ask for the key and include the matching source URL:

- Pay-as-you-go: https://help.aliyun.com/zh/model-studio/get-api-key
- Coding Plan: https://bailian.console.aliyun.com/cn-beijing/?tab=model#/efm/coding_plan
- Token Plan: https://bailian.console.aliyun.com/?tab=plan#/efm/subscription/overview

Use `--region singapore` only for pay-as-you-go Singapore. If the user gives a custom Alibaba Cloud compatible endpoint, pass it through with `--base-url`; otherwise use the billing-specific defaults from the official OpenClaw guide. For deeper billing details, read `references/aliyun-model-studio-openclaw.md`.

## Execute

If the user asks to install OpenClaw but has not provided an API key, run the
basic dependency precheck first:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing payg \
  --precheck-only
```

After precheck passes, tell the user the matching API key source URL printed by
the script. If the user already provided an API key, run the full install
directly; the script performs the same dependency precheck before installing.

Run exactly this script from the installed skill directory for full install:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing payg \
  --api-key "$BAILIAN_API_KEY"
```

If this skill is installed under a different agent home, keep the same relative script path:

```bash
python3 <install-openclaw-skill-dir>/scripts/install_openclaw.py ...
```

The authoritative implementation is `scripts/install_openclaw.py`.
The normal parameters are `--billing`, `--api-key`, `--api-key-env`, `--region`,
`--base-url`, `--model-id`, `--npm-registry`, `--dry-run`, `--precheck-only`,
`--skip-preflight`, `--skip-tokenless`, and DingTalk-specific flags.

## Examples

Pay-as-you-go:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing payg \
  --api-key "$BAILIAN_API_KEY"
```

Coding Plan:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing coding \
  --api-key "$CODING_PLAN_API_KEY"
```

Token Plan:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing token \
  --api-key "$BAILIAN_TOKEN_PLAN_API_KEY"
```

Prefer `--api-key-env NAME` when an environment variable already contains the key:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing payg \
  --api-key-env BAILIAN_API_KEY
```

Custom endpoint:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing payg \
  --api-key-env BAILIAN_API_KEY \
  --base-url "https://dashscope.aliyuncs.com/apps/anthropic"
```

With DingTalk:

```bash
python3 /home/ecs-user/.copilot-shell/skills/install-openclaw/scripts/install_openclaw.py \
  --billing coding \
  --api-key-env CODING_PLAN_API_KEY \
  --dingtalk-client-id "dingxxxxxx" \
  --dingtalk-client-secret "$DINGTALK_CLIENT_SECRET" \
  --install-dingtalk-plugin
```

## What The Script Does

- Checks Node.js/npm and installs them with `dnf`/`yum` when needed.
- Runs dependency precheck before installing or writing config.
- Runs a model endpoint pre-flight check before writing config or starting Gateway. It uses the official Alibaba Cloud OpenClaw `anthropic-messages` shape and validates the resolved API key, Base URL, and model ID. Pass `--skip-preflight` only when the endpoint is temporarily unreachable and the user accepts deferring validation to Gateway startup.
- With `--dry-run`, still resolves config and runs the model pre-flight check, but skips local install/config/gateway changes. Combine with `--skip-preflight` only for an offline local-flow preview.
- With `--precheck-only`, checks only local dependencies and prints the API key source; it does not require or validate an API key.
- Installs OpenClaw with npm unless `--skip-install-openclaw` is passed.
- Uses npm registry `https://registry.npmmirror.com` by default; override with `--npm-registry`.
- Writes only OpenClaw schema-supported config fields.
- Writes Alibaba Cloud Model Studio config with `api = anthropic-messages`.
- Sets `gateway.mode = local`, `gateway.bind = loopback`, and `gateway.auth.mode = none` for local single-machine setup.
- Starts OpenClaw through `openclaw gateway install` and `openclaw gateway restart` unless `--skip-gateway` is passed.
- After the Gateway port is listening, runs a bounded local message smoke test to verify the local operator device has `operator.write`. If stale read-only local operator device state exists, the script clears it and retries the message smoke test.
- If the gateway port is occupied by OpenClaw, it clears that stale listener. If the port is occupied by another process, it stops and prints the process details for the user to decide.
- **Auto-installs the tokenless OpenClaw plugin** after OpenClaw is installed, for token usage tracking and response compression. Pass `--skip-tokenless` to opt out.
- Prints first-run BOOTSTRAP guidance by default. Tell the user `openclaw onboard --skip-bootstrap` keeps the first real task from being interrupted by OpenClaw's introductory `BOOTSTRAP.md` flow, and that they can still adjust `IDENTITY.md`, `USER.md`, and `SOUL.md` later under the OpenClaw workspace.

## Verify

The script prints useful checks. Prefer:

```bash
openclaw models list
openclaw agent --message "hello" --agent main
openclaw gateway health
openclaw status
```

The script already runs the message smoke test unless `--skip-gateway-write-check` is passed. Do not treat `EMBEDDED FALLBACK` as success; if it mentions `pairing required`, `scope upgrade pending approval`, or `missing scope: operator.write`, read `references/troubleshooting.md`.

## References

- Standard Alibaba Cloud OpenClaw guide: https://help.aliyun.com/zh/model-studio/openclaw
- Read `references/aliyun-model-studio-openclaw.md` for the billing matrix and API key source URLs.
- Read `references/dingtalk-setup-guide.md` only when configuring DingTalk.
- Read `references/troubleshooting.md` when model/auth/channel checks fail.

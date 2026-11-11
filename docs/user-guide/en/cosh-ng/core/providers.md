# Model providers and authentication

[中文版](../../../zh/cosh-ng/core/providers.md)

Use `/auth` to configure a model in the interactive terminal. For a managed or headless setup,
define the provider in the system or user config file; project config cannot
add credentials or provider definitions.

## Automatic model access on ECS

With a compatible Cosh Shell/Core, a user-initiated model request on ECS can use
automatic access to OS Console model service without `/auth` when no provider, endpoint, or credentials
have been selected. Existing model configurations keep their current behavior.
This path uses the fixed model `auto`; project/user configuration, environment
variables, CLI model overrides, runtime model switches, and resumed sessions
cannot change it. Use `/auth` to configure another provider for a specific model.
You do not need to run `/auth` before using automatic access.
This path requires server support for ECS instance identity exchange.

Automatic access removes manual credential setup; it does not promise a permanently free service. Monthly free allowances will be introduced after the current preview period. Once the available allowance is exhausted, requests stop unless the user purchases a top-up or explicitly enables pay-as-you-go. OS Console determines current availability and quota; Cosh never enables paid access automatically.

When activation is required, the terminal displays a QR code and the
[OS Console link](https://console.aliyun.com/alinux/welcome?source=cosh). Activate with the account
that owns the ECS instance. Cosh checks automatically and resumes the current
question without confirmation. Temporary credentials stay in memory and are
refreshed before subsequent requests as expiry approaches; no AK/SK is saved.

- `Esc` cancels the current wait. Cancelled questions are never replayed automatically.
- `n` opens automatic access preferences for this Shell session or future sessions. Disabling
  the prompt also stops automatic model access.
- Session preferences end when the Shell exits. Persistent preferences live in
  `~/.copilot-shell/cosh/preferences.toml` and survive `/new` or a Core restart.
  Session suppression displays its own confirmation. When no model is configured,
  subsequent questions show setup guidance in the Shell's selected language.
- Use `/auth` to configure another model or explicitly re-enable automatic access.
  Re-enabling does not replay a cancelled question.

An explicit server quota failure produces a quota warning without automatic
retries; check OS Console or use `/auth` to change providers. Network, permission, and identity errors
are reported separately from activation requirements.

## Choose a provider interactively

```text
/auth
```

The picker offers Aliyun AK/SK, DashScope, OpenAI-compatible, Coding Plan, and
Token Plan profiles. Built-in plan endpoints use the China catalog by default.
Choose the international catalog before starting `cosh` when needed:

```bash
COSH_SERVICE_SITE=international cosh
```

`china`/`cn` and `international`/`intl`/`global` are accepted values. This
only changes built-in plan endpoints; it does not rewrite a saved custom URL.

## Configure a provider

Put this example in `~/.copilot-shell/config.toml` (or the administrator file
`/etc/copilot-shell/config.toml`) and export the key in the environment:

```toml
[ai]
active_provider = "dashscope"
active_model = "qwen3.7-plus"

[ai.providers.dashscope]
type = "dashscope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = "${DASHSCOPE_API_KEY}"
model = "qwen3.7-plus"
```

Other common profiles use the same shape:

```toml
[ai.providers.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
api_key = "${OPENAI_API_KEY}"
model = "gpt-4o"

[ai.providers.deepseek]
type = "deepseek"
base_url = "https://api.deepseek.com/v1"
api_key = "${DEEPSEEK_API_KEY}"
model = "deepseek-chat"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "${ALIBABA_CLOUD_ACCESS_KEY_ID}"
access_key_secret = "${ALIBABA_CLOUD_ACCESS_KEY_SECRET}"
security_token = "${ALIBABA_CLOUD_SECURITY_TOKEN}"
model = "qwen3.7-plus"
```

For an ECS RAM role, use `type = "aliyun"` with
`auth_source = "ecs_ram_role"`; static AK/SK values are then unnecessary.

The `aliyun` provider picks its SysOM API host automatically: on an ECS instance
that can reach the in-VPC proxy it uses that, which lets instances without
public egress work, and otherwise it uses the public host. Set
`sysom_endpoint` only to pin a specific host:

```toml
[ai.providers.aliyun]
type = "aliyun"
auth_source = "ecs_ram_role"
sysom_endpoint = "https://sysom.cn-shanghai.aliyuncs.com"
model = "qwen3.7-plus"
```

`sysom_endpoint` is deliberately separate from `base_url`: `base_url` is an
OpenAI-compatible setting and is ignored by the `aliyun` provider.

The override order is `COSH_SYSOM_ENDPOINT`, then `sysom_endpoint`, then the
VPC probe, then the public endpoint. Overrides accept a bare host with an
optional port or an HTTP(S) URL; URL paths are discarded and invalid overrides
are ignored. Only automatically selected VPC routes connect directly, matching
the probe. Explicit overrides (including an explicitly configured VPC host) and
public fallback retain system proxy settings. Use `NO_PROXY` when an explicit
endpoint should bypass your proxy.

Inference allows 3 seconds for connection setup and 120 seconds of read
inactivity. Successful reads reset the inactivity timer; healthy streaming
responses have no total-duration deadline.

| `type` | Use |
|---|---|
| `dashscope` | DashScope OpenAI-compatible endpoint with Qwen reasoning support |
| `openai` | OpenAI request conventions, including `max_completion_tokens` |
| `deepseek` | OpenAI-compatible endpoint with reasoning-content support |
| `aliyun` | Alibaba Cloud SysOM with AK/SK or ECS RAM role |
| any other value | Generic OpenAI-compatible behavior |

Set `explicit_cache = true` only for DashScope when you want explicit cache
markers. Leave it unset or `false` for the default behavior.

## Precedence and missing credentials

The active provider and model are resolved in this order: configuration layers,
then `COSH_AI_PROVIDER`/`COSH_MODEL`/`COSH_OUTPUT_LANGUAGE`, then provider
fields and their environment fallbacks. `--model <name>` overrides only the
model; use `COSH_AI_PROVIDER` or `active_provider` to switch providers.

| Variable | Fallback |
|---|---|
| `OPENAI_BASE_URL` | OpenAI-compatible base URL |
| `DASHSCOPE_API_KEY`, then `OPENAI_API_KEY` | API-key providers |
| `ALIBABA_CLOUD_ACCESS_KEY_ID`, `ALIBABA_CLOUD_ACCESS_KEY_SECRET`, `ALIBABA_CLOUD_SECURITY_TOKEN` | Aliyun credentials |

If a key is missing, interactive Core asks for authentication. A standalone
headless client must answer that control request or configure credentials before
startup. See [Configuration](../configuration.md) for file-layer rules and
[Headless mode](headless-mode.md) for the control protocol.

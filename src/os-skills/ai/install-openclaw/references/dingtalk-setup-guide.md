# DingTalk Setup for OpenClaw

Use this reference only when the user wants DingTalk access in addition to model setup.

## Create the DingTalk App

1. Open https://open-dev.dingtalk.com/.
2. Create an internal enterprise app.
3. Add bot capability.
4. Use Stream message receiving mode.
5. Publish the app.

## Credentials

| DingTalk value | Script argument |
|---|---|
| AppKey / Client ID | `--dingtalk-client-id` |
| AppSecret / Client Secret | `--dingtalk-client-secret` |
| Robot Code | `--dingtalk-robot-code` |

The default script path uses the official OpenClaw DingTalk plugin package:

```bash
openclaw plugins install @soimy/dingtalk
```

The script can do this automatically with:

```bash
--install-dingtalk-plugin
```

## One-Command Example

```bash
python3 scripts/install_openclaw.py \
  --billing coding \
  --api-key "$CODING_PLAN_API_KEY" \
  --dingtalk-client-id "dingxxxxxx" \
  --dingtalk-client-secret "$DINGTALK_CLIENT_SECRET" \
  --install-dingtalk-plugin
```

## Generated Channel Config

Default plugin `dingtalk` writes:

```json
{
  "plugins": {
    "enabled": true,
    "allow": ["dingtalk"],
    "entries": {
      "dingtalk": { "enabled": true }
    }
  },
  "channels": {
    "dingtalk": {
      "enabled": true,
      "clientId": "dingxxxxxx",
      "clientSecret": "your-secret",
      "robotCode": "dingxxxxxx",
      "dmPolicy": "open",
      "groupPolicy": "open",
      "messageType": "markdown"
    }
  }
}
```

## Verify

```bash
openclaw plugins list
openclaw channels status --probe
```

If schema validation fails, remove unsupported fields from `channels.dingtalk` and rerun the installer.

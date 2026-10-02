# RAM Policies: alibabacloud-sysom-diagnosis

This Skill uses the sysom-osops CLI. Remote diagnosis is routed through SysOM
OpenAPI gateway actions.

## Required Permissions

| API | RAM Action | Used by | Description |
|-----|------------|---------|-------------|
| InitialSysom | `sysom:InitialSysom` | Credential validation inside remote commands | Verify credential validity and SysOM role authorization |
| InvokeAgentCli | `sysom:InvokeAgentCli` | All remote diagnosis commands | Gateway action for catalog queries, diagnosis execution, and task polling |

## Minimum Permission Policy

```json
{
  "Version": "1",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": [
        "sysom:InitialSysom",
        "sysom:InvokeAgentCli"
      ],
      "Resource": "*"
    }
  ]
}
```

## Notes

- `sysom-osops memory classify` runs locally and does not require cloud
  permissions.
- Remote commands across memory, IO, network, load, and Java memory require the
  permissions above.
- Avoid broader wildcard permissions when a custom least-privilege policy can be
  attached to the RAM user or ECS RAM Role.
- Do not paste AK/SK values into the conversation. Configure credentials outside
  the Agent session.

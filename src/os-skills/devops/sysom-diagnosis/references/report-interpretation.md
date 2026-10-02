# Report Interpretation

Use the default SysOM envelope as the Agent-facing contract.

## Default Structure

```json
{
  "ok": true,
  "command": "sysom-osops memory classify",
  "agent": {
    "status": "warning",
    "summary": "Human-readable diagnosis summary.",
    "findings": [
      {
        "severity": "high",
        "title": "Short title",
        "detail": "Root cause, key entities, and evidence summary.",
        "category": "root_cause"
      }
    ],
    "next_steps": [
      {
        "kind": "command",
        "label": "Focused follow-up",
        "command": "sysom-osops memory oom",
        "reason": "The missing entity this command can fill."
      }
    ]
  }
}
```

## Fields

| Field | How to use |
|-------|------------|
| `ok` | Business success flag. If false, read `error.code` and `agent.summary`. |
| `command` | Command that produced this envelope. Use it to avoid repeating the same action. |
| `agent.status` | Overall status: normal, warning, error, or critical. |
| `agent.summary` | Primary diagnosis summary. Present it unless it conflicts with a stronger root-cause finding. |
| `agent.findings[]` | Evidence and conclusion list. Each finding may contain only `severity`, `title`, `detail`, and `category`. |
| `agent.next_steps[]` | Focused follow-up actions or safe remediation steps. Treat them as prioritized, not as a checklist. |

## Finding Priority

Prefer findings in this order:

1. `category=root_cause`.
2. Highest severity.
3. Best match to the user's reported symptom.
4. Most complete `detail` for holder, magnitude, mechanism, and safe next action.

Lower-severity findings can be mentioned as context when they are already in the
same envelope, but they should not trigger extra commands after the main symptom
is explained.

## Answer Shape

Final answers should include:

- Root cause in plain language.
- Exact entities visible in `summary` or `detail`: process, service, cgroup,
  file path, segment, device, victim, scope, or limit/current as applicable.
- Evidence chain: which SysOM command produced which fact.
- Safe remediation or the next focused action from `next_steps[]`.

If a required entity is missing because the command failed or returned partial
evidence, say which entity is unknown and run one focused follow-up only if it is
needed to answer the user's symptom.

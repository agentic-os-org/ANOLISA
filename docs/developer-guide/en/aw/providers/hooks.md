# Write custom Hooks

[中文版](../../../zh/aw/providers/hooks.md)

## Boolean rules

A simple rule reads one normalized tool event, prints JSON `true` or `false` to stdout and exits 0. Write diagnostics to stderr.
Save as `/absolute/check.py`, replacing the path with your file's actual absolute path.

```python
import json
import sys

event = json.load(sys.stdin)
print(json.dumps("AW_DENY_TEST" in json.dumps(event["tool"]["input"])))
```

Merge these fields under `spec` in a complete configuration. If a Provider or event already exists, merge the required steps and retain existing rules.

```yaml
providers:
  command:
    transport:
      type: stdio
      location: agent
      argv: [/usr/bin/python3, /absolute/check.py]
    timeout_ms: 1000
    max_output_bytes: 32
events:
  tool.before:
    enabled: true
    required: true
    steps:
      - id: check-command
        provider: command
        effects: [block]
        reason_code: demo_marker_denied
        on_error: block
```

`true` requests the step's declared effects; `false` requests none. Omitted protocol means `aw-hook/v1alpha1`; omitted operation means `invoke`.
The script runs when an event occurs. Nonzero exit, timeout, malformed or oversized output follows `on_error`. Boolean output is bounded by the smaller of 32 bytes and the configured limit.

The [editing tutorial](../reference/configuration-editor.md) runs a complete example from empty configuration. To generate a new file, append `--provider command --check /usr/bin/python3 --effect block -- /absolute/check.py` to the complete `aw config init` command in the [installation guide](../../../../user-guide/en/aw/installation.md). Retain its prefix, configuration, state directory and Agent arguments, and replace the script path.

## Reuse native commands

Existing framework scripts use `protocol: native-hook/v1alpha1` and `config: {}`; steps set `native: {}` and `on_error`, omitting operation/effects.
They receive native callback stdin; the selected Adapter interprets output and exit status. Qoder exit code 2 can block tools; other frameworks use their own contracts.
`transport.argv` is a literal argument array. Specify `/bin/sh -c` explicitly if Shell syntax is required.

## Extend into a Provider

Use the [Provider protocol](protocol.md) to return structured candidate effects or validate private configuration.
All three step forms obey event budgets, tool selectors and Adapter capabilities. [Validate native behavior](../validation.md) for allow, block, failure and cleanup.

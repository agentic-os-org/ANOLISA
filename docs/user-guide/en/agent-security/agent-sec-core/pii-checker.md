# PII Checker User Guide

[中文版](../../../zh/agent-security/agent-sec-core/pii-checker.md)

PII Checker detects personal data and credentials in Agent inputs and outputs. It returns a
structured verdict, produces safe evidence and optional redacted text, and records sanitized
Security Events for audit and Observability correlation.

## Use the bundled Skill

With V1 `agent-sec-cli` installed and the `pii-checker` Skill available to the Agent,
ask it to check a specified file for personal data or credentials, or to generate
a redacted copy. The Skill reports redacted evidence and does not rewrite the input
file unless requested. A completed scan with no findings is not a guarantee that the
content contains no sensitive information.

The Skill ships in `agent-sec-skills` for RPM installations and in the ANOLISA raw
package's shared skills directory. Cosh-NG discovers that directory; the OpenClaw and
Hermes adapters declare `pii-checker` for delivery into their respective Skill directories.
Other Agent installations must make the Skill available through their own discovery paths.

## Scan text

Provide exactly one input source: inline text, standard input, or a UTF-8 file.

```bash
# Inline text
agent-sec-cli scan-pii --text "contact alice@example.com"

# Standard input
printf '%s' 'token=secret-value-1234567890' \
  | agent-sec-cli scan-pii --stdin --redact-output

# UTF-8 file
agent-sec-cli scan-pii --input ./agent-output.txt --format text
```

Useful options:

| Option | Purpose |
|--------|---------|
| `--format json\|text` | Select structured JSON or human-readable output; default is `json` |
| `--redact-output` | Include `redacted_text`; the input file is never modified |
| `--include-low-confidence` | Include findings below the default confidence threshold |
| `--raw-evidence` | Include raw evidence in local CLI output only |
| `--max-bytes N` | Scan at most `N` UTF-8 bytes and mark the result as truncated |
| `--source SOURCE` | Label the audit context, such as `user_input` or `tool_output` |

Supported source labels are `user_input`, `tool_input`, `tool_output`, `model_output`,
`observability`, `manual`, and `unknown`.

## Built-in detection

The built-in detector combines regex matching, format validation, and context-based confidence
adjustment.

| Category | Types | Default severity |
|----------|-------|------------------|
| Personal data | `email`, `phone_cn`, `credit_card`, `cn_id` | `warn` |
| Credentials | `private_key`, `bearer_token`, `api_key`, `jwt` | `deny` |
| Alibaba Cloud credentials | `aliyun_access_key_id`, `aliyun_access_key_secret` | `deny` |
| Secret fields | `generic_secret_field` | `deny` |

Credit card, Chinese ID, and JWT candidates are validated before becoming findings. Surrounding
security keywords can increase confidence, while fixture markers such as `example`, `dummy`,
`test`, and `sample` can lower it. Findings below the default `0.5` threshold are omitted unless
`--include-low-confidence` is set.

## Verdicts and redaction

The scanner aggregates findings into one verdict:

| Verdict | Meaning |
|---------|---------|
| `pass` | No finding remains after confidence filtering |
| `warn` | Findings exist, but none has `deny` severity |
| `deny` | At least one finding has `deny` severity |

Each finding includes its type, category, severity, confidence, span, detector metadata, and
redacted evidence. `--redact-output` also returns a redacted copy of the scanned text. Overlapping
findings are preserved, while their overlapping spans are merged and replaced once. If different
spans overlap, the complete merged range is fully redacted so a shorter match cannot leave a
sensitive suffix visible.

`--raw-evidence` is intended only for local troubleshooting. Raw evidence is never written to
Security Events. Host integrations consume the same verdict and finding schema; whether a host
only observes a finding or blocks an operation depends on that host's configured PII policy.

## Host hook policy

Set `PII_CHECKER_HOOK_ENABLED=false` to skip host PII hooks entirely. When enabled,
`PII_CHECKER_MODE` accepts `observe`, `warn`, `ask`, or `block` and defaults to
`observe` on hosts with native notice support. Hermes accepts only `observe` and `block`;
legacy `warn` / `ask` values fall back to `observe` with a host diagnostic. It never injects
advisories through the final assistant response. On Hermes, `block` can enforce a `deny` only at
`pre_tool_call`. Model output is scanned for audit through `post_llm_call`, but it is never
modified or blocked because Hermes has no plugin-usable pre-stream output gate. On other hosts,
an unsupported `ask` or `block` boundary may fall back
to `warn`; post-execution hooks never claim to undo an external side effect. The environment
policy overrides Hermes/OpenClaw capability configuration. `debug` maps to `observe`, and `deny`
maps to `block`. Qwen Code additionally accepts the legacy `PII_CHECKER_ENABLED` switch when
`PII_CHECKER_HOOK_ENABLED` is absent.

`PII_CHECKER_HOOK_ENABLED` and `PII_CHECKER_MODE` are read by all six hosts. Two further
variables are only read by some of them:

| Environment variable | Default | Hosts that read it |
|----------------------|---------|--------------------|
| `PII_CHECKER_TIMEOUT` | `5` | Qoder, Codex, Qwen Code (Qwen Code caps it at 8 seconds) |
| `PII_CHECKER_INCLUDE_LOW_CONFIDENCE` | `false` | Qoder, Qwen Code |

cosh, Hermes, and OpenClaw do not read those two environment variables. Hermes supports both
settings through capability configuration (`timeout` and `include_low_confidence`). OpenClaw
supports only `piiIncludeLowConfidence`; its PII scanner CLI timeout is fixed at 10 seconds.
cosh uses a fixed timeout and never requests low-confidence findings.

The host Agent reads these variables when it loads the plugin. Restart the Agent process that
hosts the hook after changing them; the hook and agent-sec-core are not separate policy services.

Scanner verdict `deny` describes finding severity. Hook policy `block` controls whether the
current adapter attempts enforcement.

## Custom regex rules

PII Checker optionally loads custom business-specific types from one fixed user-level file:

```text
~/.config/agent-sec/pii-checker/rules.yaml
```

The YAML top level is a list. Each rule contains a unique custom type, one regex, and an optional
severity.

```yaml
- type: dogfood_order_no
  regex: '(?i)(?<=order_no[=:])DFT-[A-Z0-9]{8}'
  severity: warn

- type: dogfood_customer_token
  regex: 'DFT-[A-Z0-9]{16}'
  severity: deny
```

| Field | Required | Description |
|-------|----------|-------------|
| `type` | Yes | Lowercase snake_case custom type, unique within the file |
| `regex` | Yes | One regex expression; use inline flags such as `(?i)` |
| `severity` | No | `warn` or `deny`; defaults to `deny` |

The complete regex match is the finding and redaction span. Capture groups and named capture groups
do not change that span. If a regex matches both a field name and its value, both are redacted. Use
lookaround when the full match must cover only the value. Multiple formats for one type must be
combined with regex alternation (`|`); the same type cannot appear in multiple rules.

Custom findings use category `custom`, confidence `1.0`, detector `custom_rule`, and engine `regex`.
They are fully redacted with a stable type marker such as `[DOGFOOD_ORDER_NO_REDACTED]` and flow
through the same verdict, policy, Security Event, and Observability paths as built-in findings.

No CLI option, environment variable, XDG override, system-level file, or multi-file merge is
supported for the custom rules path.

## Custom rule validation and runtime limits

The complete custom ruleset is accepted or rejected as one unit.

| Limit or rule | Value |
|---------------|-------|
| Maximum file size | 256 KiB |
| Maximum number of rules | 100 |
| Maximum regex length | 2,048 characters |
| Maximum regex group nesting depth | 64 |
| Type format | `^[a-z][a-z0-9_]{0,63}$` |
| Allowed severity | `warn` or `deny` |
| Per-rule matching timeout | 20 ms |
| Total custom matching budget per scan | 200 ms |
| Maximum custom findings per scan | 100 |

Unknown YAML fields, duplicate types, built-in type names, invalid regexes, and regexes that match an
empty string make the complete custom ruleset invalid. Other zero-length matches encountered at
runtime are ignored.

Rules with `deny` severity run before `warn` rules. File order is preserved within each severity.
The 100-finding limit caps emitted custom findings but does not stop evaluation of later rules;
`truncated` becomes `true` only when an additional valid match is omitted. Per-rule or total time
limits may still stop the remaining custom rules and are reported separately.

When the file content changes, the next scan validates and compiles the new content automatically.
If the new version is invalid, the previous valid version is not reused. Built-in detection remains
active and `scan-pii` still completes successfully.

## Custom rule status

Every default scan includes sanitized custom rule state in `summary.custom_rules`:

```json
{
  "custom_rules": {
    "status": "loaded",
    "rule_count": 2,
    "runtime_error_count": 0,
    "budget_exhausted": false,
    "truncated": false
  }
}
```

`status` is `absent` when the file does not exist, `loaded` when validation succeeds (including an
empty list), and `invalid` when reading, YAML parsing, schema validation, or regex compilation fails.
An invalid status includes a sanitized `error_code`; loaded or invalid content may include its
SHA-256 digest. Runtime counters do not contain input text or regex content. A direct `scan-pii`
invocation also prints a sanitized invalid-configuration warning to stderr while still exiting
successfully.

The current `error_code` values are:

| Error code | Meaning |
|------------|---------|
| `read_error` | The rules file could not be read |
| `file_too_large` | The rules file exceeds 256 KiB |
| `invalid_utf8` | The rules file is not valid UTF-8 |
| `invalid_yaml` | The YAML content cannot be parsed safely |
| `top_level_not_list` | The YAML top level is not a list |
| `too_many_rules` | The file contains more than 100 rules |
| `invalid_rule_schema` | A rule has missing, unknown, incorrectly typed, or unsupported fields |
| `invalid_rule_type` | A rule type does not match the required naming format |
| `duplicate_rule_type` | The same custom type appears more than once |
| `reserved_rule_type` | A custom type conflicts with a built-in PII type |
| `invalid_regex` | A regex cannot be compiled, exceeds 64 nested groups, or its load-time validation times out |
| `regex_matches_empty_text` | A regex can produce a zero-length match on an empty string |
| `load_error` | An unexpected loader error was handled in fail-open mode |

## Security Events and Observability

Every scan follows the existing `pii_scan` Security Event path. Events contain the source, verdict,
summary, finding type, severity, category, span, and redacted evidence. They do not contain the
custom rules path, regex expressions, or raw sensitive matches.

Host hooks remain fail-open when custom rules are invalid and do not add a separate host warning.
The sanitized `summary.custom_rules` state in the Security Event is the structured audit source for
hook invocations.

Observability uses the existing trace context and input hash to correlate telemetry with the
Security Event instead of storing another copy of finding details.

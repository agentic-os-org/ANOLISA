# PII Checker User Guide

[中文版](../../../zh/agent-security/agent-sec-core/pii-checker.md)

PII Checker detects personal data and credentials in Agent inputs and outputs. It returns a
structured verdict, produces safe evidence and optional redacted text, and records sanitized
Security Events for audit and Observability correlation.

## V2 phase 1

The V2 RPM provides Rust `agent-sec-cli` and `agent-sec-daemon`. Use the RPM built by
`./scripts/rpm-build.sh agent-sec-core-v2` in a dedicated Linux validation environment;
this migration does not switch existing Agent hosts or their services to V2.
After installing that RPM, start its system service, which creates the protected runtime directory:

```bash
sudo systemctl start agent-sec-core
```

Scan through the daemon's default socket:

```bash
agent-sec-cli scan-pii --text 'contact alice@company.cn' --source manual
```

The default endpoint is `/run/agent-sec-core/daemon.sock`. A nonempty `AGENT_SEC_DAEMON_SOCKET`
overrides it, and `agent-sec-cli --socket /absolute/path/to/daemon.sock scan-pii ...` takes precedence.
The CLI does not start a daemon or fall back to Python. Files and stdin are read by the CLI;
the daemon receives text, never a caller-selected input file or rules path. Empty text is valid.
`--text-stdin` aliases `--stdin`. Completed `pass`, `warn`, and `deny` scans exit `0`;
scan/connection failures exit `1`, and CLI usage errors exit `2`. `deny` is a finding
classification, not a PDP authorization decision.

Without `--max-bytes`, V2 does not truncate input by default. An explicit positive byte limit
keeps a valid UTF-8 prefix; malformed UTF-8 fails. Requests and responses must fit the V2
4 MiB frame limit, including JSON escaping and envelope overhead; oversize input fails explicitly.
Spans count Unicode characters, matching Python positions, rather than UTF-8 bytes or UTF-16 units.

V2 keeps the existing top-level result fields and adds evidence metadata under `summary`:

| Field | Meaning |
|-------|---------|
| `execution_status` | `completed` or `failed` |
| `coverage.status` | `complete`, `partial`, or `unavailable` |
| `coverage.reasons` | Safe codes for truncation, invalid rules, matching limits, or scan failure |
| `input_sha256` | SHA-256 of text received by the detector; never a claim about omitted input |
| `scanned_input_sha256`, `scanned_bytes` | Digest and byte length of the prefix actually scanned |
| `scanner_version` | Detection semantics version; V2 currently reports `2.0.0` |
| `ruleset_id` | Identity of the scanner version and immutable built-in/custom rule configuration |

`bytes_scanned` retains the V1 prefix counter and can include an incomplete UTF-8 tail excluded
from `scanned_bytes`. A `partial` result may still be `pass`: verdict only aggregates observed
findings. Check coverage before treating evidence as complete.


V2 bounds the returned report to 512 KiB of formatted JSON after detection and full-text
redaction finish. Oversize reports retain the first finding of each type/severity and omit raw
evidence. `summary.findings_truncated=true` records reduced findings or evidence. Verdict,
`summary.total`, category/severity
counts, digests and scan coverage still describe all findings detected in the scanned input.
Hook notices use these totals and indicate omitted details. The returned findings are then
representative evidence, not an exhaustive list of locations.

If the full `redacted_text` still exceeds that budget, V2 returns
`[REDACTED: output size limit]` for the entire text and sets
`summary.redacted_text_omitted=true`; it never returns a partly redacted remainder.
Both markers are absent when false. Output reduction alone does not make scan coverage partial,
and completed scans still exit `0`. The transport-free `PiiScanner` retains its full report;
the executor applies this response bound before audit projection and Finalizer.

## Detection semantics version

`summary.scanner_version` identifies detection behavior independently of the AgentSecCore package
version and RPC schema. V2 reports `2.0.0` in successful and failed scan reports and audit results;
built-in findings use engine `regex_v2`, custom findings use `fancy_regex`. `ruleset_id` includes
this version plus the built-in patterns and custom configuration, so rule-engine changes remain
traceable even when the YAML bytes are unchanged. A CI commit SHA identifies the tested build;
it is not the detector's semantics version.

Version 2.0.0 retains the 11 supported types, confidence scoring, Unicode spans and redaction,
with these intentional changes from V1:

- JWT candidates include compact empty claims (`{}`), which V1's minimum payload length missed.
  Large integer claims and deeply nested JSON objects are checked structurally without inheriting
  Python integer-conversion or recursion limits. Malformed JSON is still rejected; detection does
  not authenticate the signature or authorize use of the token.
- Chinese ID date and checksum validation normalize decimal digits consistently, including fullwidth
  dates/check digits and `Ｘ`/`ｘ`, while retaining the original text and character spans.
- All-zero card placeholders are rejected even though they satisfy Luhn. Other length/checksum
  checks remain; a finding does not confirm that a card number has been issued.
- Custom patterns use the native dialect described below. A behavior difference from Python alone
  does not make a valid V2 rule invalid or its coverage partial.

The retained V1 corpus checks unchanged behavior; separate positive, negative and resource-limit
cases define V2 changes. These checks cover the supported formats, not a guarantee of zero errors
on arbitrary data. Short/unlabeled secrets, formats outside the 11 types, and contextual ambiguity
still need application-specific rules and representative examples.

## Use the bundled Skill

With `agent-sec-cli` installed and the `pii-checker` Skill available to the Agent,
start the daemon first when using V2. Ask it to check a specified file for personal data or credentials, or to generate
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

Built-in rules ship in the binary. Custom rules are separate configuration:

| Runtime | Custom file | Reload behavior |
|---------|-------------|-----------------|
| V2 | `/etc/agent-sec/pii-checker/rules.yaml` | Validated and compiled once at daemon startup; restart to update |
| V1 | `~/.config/agent-sec/pii-checker/rules.yaml` | New content is loaded on the next scan |

The administrator may select another absolute V2 file with
`agent-sec-daemon --pii-rules /absolute/path/rules.yaml --socket /run/agent-sec-core/daemon.sock`.
All callers share one immutable rule set. V2 never reads the caller's HOME, chooses rules by owner,
or imports old user directories automatically. The scan RPC cannot override this selection.

The YAML top level is a list. Each rule contains a unique custom type, one regex, and an optional
severity.

```yaml
- type: dogfood_order_no
  regex: '(?<=order_no[=:])DFT-[A-Z0-9]{8}'
  severity: warn

- type: dogfood_customer_token
  regex: 'DFT-[A-Z0-9]{16}'
  severity: deny
```

| Field | Required | Description |
|-------|----------|-------------|
| `type` | Yes | Lowercase snake_case custom type, unique within the file |
| `regex` | Yes | One regex expression; V2 uses native `fancy-regex` syntax |
| `severity` | No | `warn` or `deny`; defaults to `deny` |

The complete regex match is the finding and redaction span. Capture groups and named capture groups
do not change that span. If a regex matches both a field name and its value, both are redacted. Use
lookaround when the full match must cover only the value. Multiple formats for one type must be
combined with regex alternation (`|`); the same type cannot appear in multiple rules.

Custom findings use category `custom`, confidence `1.0`, detector `custom_rule`, and engine `regex` in V1 / `fancy_regex` in V2.
They are fully redacted with a stable type marker such as `[DOGFOOD_ORDER_NO_REDACTED]` and flow
through the same verdict, policy, Security Event, and Observability paths as built-in findings.

V1 has no rules-path override. Neither runtime merges multiple rule files. V2 rules are detector
configuration; they are not PAP policies and do not themselves authorize operations.

## Custom rule validation and runtime limits

The complete custom ruleset is accepted or rejected as one unit.

| Limit or rule | Value |
|---------------|-------|
| Maximum file size | 256 KiB |
| Maximum number of rules | 100 |
| Maximum regex length | 2,048 characters |
| Maximum YAML nesting depth | 64 |
| V2 regex group recursion limit | 64 parser frames, including the root |
| Type format | `^[a-z][a-z0-9_]{0,63}$` |
| Allowed severity | `warn` or `deny` |
| Per-rule matching limit | V1: 20 ms; V2: 1,000,000 backtracking steps |
| Total custom matching budget per scan | 200 ms |
| Maximum custom findings per scan | 100 |

Unknown YAML fields, duplicate types, built-in type names, invalid regexes, and regexes that match an
empty string make the complete custom ruleset invalid. Other zero-length matches encountered at
runtime are ignored.

Rules with `deny` severity run before `warn` rules. File order is preserved within each severity.
The 100-finding limit sets `truncated` only when an additional valid match is omitted.
V1 continues evaluating later rules; V2 stops remaining custom matching at that point and reports
partial coverage. V2 checks its 200 ms budget between matching operations and does not promise to
interrupt one match after 20 ms. Backtracking and other matching limits preserve earlier findings.

V2 uses the locked `fancy-regex` dialect directly. Valid flags such as `(?i)` / `(?x)`,
comments, lookaround and set operations are accepted when the engine supports them. Rules are
not rewritten or rejected solely because Python would interpret them differently:

- `$` asserts the input end by default; `(?m)$` also asserts line ends. `\z` is a strict input end,
  while `\Z` accepts the position before trailing newlines.
- `\h` / `\H` are hexadecimal / non-hexadecimal classes. Use `[ \t]` for horizontal space.
- `(?i)` uses the engine's Unicode case folding; it does not reproduce Python's dotted/dotless-I
  folding. `\<` / `\>` are word-edge assertions outside character classes.
- Character classes support nested sets and set operations. For a named-group participation
  conditional, use `(?(<name>)yes|no)`; Python's `(?(name)yes|no)` spelling has different semantics.

`invalid_regex` is reserved for actual parse/compile/engine-limit or load-time evaluation failures.
For example, branch-reset groups and variable-length lookbehind remain unsupported. The engine
counts its root frame toward group recursion: 63 nested groups are accepted, while 64 can fail;
nested character classes have separate engine limits. YAML aliases and multiple documents are
rejected. Matching limits report partial coverage instead of hiding skipped work.

Validate migrated custom rules against both matching and non-matching examples, including Unicode
and trailing newlines, using the versioned V2 dialect. Arbitrary administrator patterns cannot be
judged for business correctness by compilation alone.

V2 keeps the startup rule set for every request until restart. At the next startup, an invalid
replacement disables the entire custom set; no old valid set is silently reused. V1 reloads on the
next scan. Built-in detection remains active in both cases; V2 marks coverage as partial.

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

`status` is `absent` when the default file does not exist (V2 explicitly selected missing files are `invalid`), `loaded` when validation succeeds (including an
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
| `invalid_regex` | A regex is unsupported, cannot compile, exceeds engine limits, or fails load-time evaluation |
| `regex_matches_empty_text` | A regex can produce a zero-length match on an empty string |
| `load_error` | V1: an unexpected loader error was handled in fail-open mode |

## Security Events and Observability

Every scan follows the existing `pii_scan` Security Event path. Events contain the source, verdict,
summary, finding type, severity, category, span, and redacted evidence. They do not contain the
custom rules path, regex expressions, or raw sensitive matches.

Host hooks remain fail-open when custom rules are invalid and do not add a separate host warning.
The sanitized `summary.custom_rules` state in the Security Event is the structured audit source for
hook invocations.

Observability uses the existing trace context and input hash to correlate telemetry with the
Security Event instead of storing another copy of finding details.

## V2 audit and rule migration

V2 uses the shared Action Runtime and Finalizer. Completed, partial, failed scans and authorized
requests with invalid scan parameters produce one terminal scan event during normal execution.
Malformed envelopes, unknown methods, authorization failures, and transport failures are handled
at their respective entry boundaries. A crash or forced shutdown can prevent finalization.

Audit persists only safe request metadata, digests, rule identifiers, coverage, redacted findings,
and error codes. Raw text, raw evidence, complete `redacted_text`, rule contents, and input-bearing
exception messages are excluded. JSONL/SQLite sink health is distinct from scan success; the existing
daemon startup still requires SQLite initialization. The V2 event-query CLI is not yet available.

Hooks can pass top-level `--trace-context '{"session_id":"session-1"}'` before `scan-pii`.
V1 snake_case/camelCase aliases are normalized; strings are trimmed and limited to 256 characters.
These are opaque correlation fields, not OpenTelemetry TraceIds. UID/GID/PID always come from UDS
peer credentials. `agent_name` is bounded caller metadata and grants no authority.

To migrate rules, review the selected V1 file, resolve conflicting type names, and explicitly place
the approved YAML in the administrator-managed V2 file. Start/restart the V2 daemon, check
`summary.custom_rules.status`, `ruleset_id`, and coverage, then run positive and negative samples.
No automatic collection of per-user files occurs. To revert a rules change, restore the previous
central file and restart. To roll back the runtime, stop the validation daemon and restore the V1
RPM/entrypoint and its independently retained user rules; V2 never modifies those V1 files.

The six Hook adapters are tested through fixed events and real Rust PII subprocesses, including
observability redaction. Unmigrated observability storage is isolated in that test harness. This is
Hook contract acceptance, not a live-host cutover or full PIP/PDP/PEP integration. See the
[two-stage design](../../../../../src/agent-sec-core/docs/design/PII_V2_MIGRATION.md).

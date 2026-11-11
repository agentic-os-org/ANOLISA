# DeepSeek Harness

[中文版](../../../zh/aw/agents/deepseek-harness.md)

DeepSeek Harness 0.2.1-rc.5 input and tool Hooks are verified on Linux through its headless
CLI. Install DSH and the AW sec-core Provider and CLI, start sec-core, then retain
the model, credentials and native permissions in `DSH_HOME`. AW supplies its
plugin through a launch-scoped patch; it does not edit DSH configuration. AW
records the installed DSH version and does not gate Agent launches by version or
profile name. Validate other combinations through [native acceptance](../validation.md).

From `src/aw`, replace the absolute executable and socket paths in
[`aw.deepseek.yaml`](../../../../../src/aw/crates/aw-service/examples/aw.deepseek.yaml),
then run:

```bash
target/debug/aw run --config crates/aw-service/examples/aw.deepseek.yaml --agent dsh -- "Review this repository"
```

The command passes task text as a headless app argument. Replace `headless` in
`argv` and use that profile's native app arguments for another business entry.
`aw run` waits for plugin readiness; the DSH Agent loop waits for Hook
registration before tool work and releases the binding when the process exits.
The sample uses the same sec-core Provider contract as Qoder SDK and QwenPaw:
it checks submitted user text for prompt injection and PII before the model request,
checks tool arguments for PII, scans `execute_shell_command` before execution,
and scans tool results for PII. Add each custom code-executing tool to
`providers.security.config.tools`
and to the `scan-code` selector. A denied call does not execute. The sample uses
`on_error: report` for output checks. DSH runs a tool's `finalizeContent` after
`tools/post-execute`; text added there is outside AW's scan and can reach the
model. DSH 0.2.1-rc.5 has no writable Hook after finalization, so this Adapter
rejects `withhold_result` at admission. Do not use this integration when strict
inspection of all model-visible tool output is required.

The verified native surface is `agent/pre-step`, `tools/pre-execute`, `tools.guard`
and `tools/post-execute`. `input.submit` checks text blocks in the claimed user
messages; a configured check rejects submissions with non-text content. A rejected
step makes no model request. DSH model configuration and permissions remain native.

AW recognizes 16 Hook names in its configuration vocabulary. Its shared Provider
execution currently supports `input.submit`, `tool.before` and `tool.after`; this
Adapter maps those three events to the DSH plugin. The other names cannot be
enabled for DSH until AW defines their Provider execution and effect contracts and
the Adapter proves a matching native boundary. A DSH lifecycle notification alone
does not establish a policy decision or its adoption.

Real tool-call acceptance currently covers headless. Web, SDK and ACP have
plugin-readiness checks; their tool calls and custom profiles need acceptance.
DSH's model-facing `skill` tool passes through `tool.before`, but its arguments
contain only the skill name. DSH resolves the winning skill inside the tool and
also injects explicitly invoked `/skill` content during `agent/pre-step`. Neither
path currently supplies the resolved Skill object to AW. A `check_skill` step
fails admission for this Adapter. Post-tool text replacement applies to a
successful single-text root result with a safe retained value and metadata;
native value replacements and nested programmatic results retain their native
content.

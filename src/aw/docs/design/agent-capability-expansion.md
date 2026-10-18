# Agent capability expansion

[中文版](agent-capability-expansion_zh.md)

AW expands through Agent rows and event/effect columns while preserving one
entrypoint: `aw run --config aw.yaml --agent <target>`. Each supported cell needs
executable contract tests and a documented native acceptance boundary. A planned
cell is not a runtime capability. This proposal starts with DeepSeek Harness
headless; Codex CLI follows as a separate integration.

## Capability and evidence model

Keep the compiled Adapter catalog, configuration and Provider request schemas and regression matrix
consistent. A row identifies an Adapter; its native inventory records the pinned
version, entrypoint, owner and exact acceptance boundary. Columns represent
event/effect pairs, initially before/observe, before/block, after/observe and
after/replace_result. Tool-type limitations belong next to the cell's evidence.

Require tests for every supported pair. Require explicit unsupported declarations
and admission rejection tests for the remaining pairs in the matrix vocabulary.
Do not make partial support silently bypass security or Tokenless obligations:
derive applicable shared tests from declared capabilities, and retain independent
Provider contract tests. Preserve every existing Adapter's coverage.

Generate the bilingual README grids from the existing manifest. Keep roadmap
entries separate from supported capabilities; distinguish deterministic callback
contracts from acceptance against an actual pinned framework.

## Integration sequence

1. Make partial capabilities explicit and test missing declarations, missing tests,
   ignored tests, unsupported effects and unsupported events.
2. Integrate DeepSeek Harness headless through native plugin hooks. Close the
   security denial, fresh-call Tokenless retry, final text adoption and cleanup
   loops using the common AW launcher and Provider protocol.
3. Integrate Codex CLI through native Hooks in a separate change. Initially qualify
   before/observe, before/block and after/observe; qualify successful result
   replacement separately for each native tool path.
4. Extend columns with session lifecycle, then subagent identity/cancellation,
   then model/context events. Establish semantics in different frameworks before
   treating a native callback name as a common AW contract.

## DeepSeek Harness boundary

Use a pinned official runtime and its headless profile. Keep native model
configuration and permissions. Register the AW plugin through a private launch
overlay; do not overwrite the user's profile. Security vetoes must survive other
plugin decisions. Tokenless retry must block the original call and require fresh
security/native authorization; never rewrite arguments or emit approval.

Text replacement must preserve structured values, metadata and failure status.
The native post-execute pipeline can be followed by final content processing, so
acceptance must inspect final tool results, including nested programmatic calls,
instead of counting successful AW callbacks.

Upstream contract baseline: [tool pipeline](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/docs/tool-execution-pipeline.md)
and [CLI profiles](https://github.com/deepseek-ai/deepseek-harness/blob/687f282e8625c6b8d11ff58211a90524be2e4317/apps/cli/reference/README.md).

## Codex CLI follow-up boundary

Use native PreToolUse/PostToolUse Hooks and preserve native trust and permissions.
Configuration written to disk does not establish that a Hook is trusted or active.
Matching native Hooks can run concurrently; AW orders its own Provider calls but
must test coexistence with independent user Hooks. Additional context and blocking
feedback do not establish successful Tokenless result replacement.

The follow-up must qualify shell, long-running command completion, MCP and nested
tool paths separately, with invocation identity, cancellation and unsupported
paths tested. Use the capability work from the DeepSeek change; do not duplicate
the matrix model or add framework branches to Providers. Recheck the pinned CLI
against the [official Hooks contract](https://learn.chatgpt.com/docs/hooks).

## Code ownership and CI

Adapters own native registration, normalization, effect adoption and teardown.
The common launcher owns lifecycle; aw-exec owns bounded processes. Providers
consume their public protocol without knowledge of native framework APIs. Split
files by these responsibilities and retain the existing source-size limits.

PRs touching `src/aw/**` run all quick security, Tokenless, lifecycle, matrix and
layout checks. Other PRs skip AW; changes to consumed public contracts need
explicit compatibility validation. Nightly/manual integration builds all-in-one
once and reuses checksummed binaries for fixed-version native jobs. Validate
exact expected report contents, observed tool behavior and process cleanup.

Run native jobs independently with version/lockfile-based caches. Test upstream
version upgrades separately before changing the accepted pin. Revalidate the
exact release SHA before release; never replace missing native evidence with a
successful fixture callback or empty report.

# Semantic Arbiter AW Provider

[中文版](README_zh.md)

An observe-only AW Provider records semantic task-scope judgments for explicitly configured single-file changes.
It compares a complete native tool call with one operator-supplied task using the public Semantic Arbiter API.
It is for controlled shadow evaluation: even a semantic `block` produces no AW effects and never executes a tool, applies an obligation, or grants authorization.

## Boundary and sensitive data

**Opt in only with permission to retain and transmit the payload.** Private records contain the full AW request/event, task text, native arguments (including replacement/file contents), model state/questions, parsed probabilities and the advisory result. The task and complete mapped candidate are sent to the configured model. These can contain secrets or personal data even though the Provider never records authentication headers, credential values obtained from the environment, or raw remote responses. AW `metadata_only` audit does **not** disable this separate sensitive store. Use synthetic data first.

Records are local observations, not authentication, execution receipts, durable replacements for the arbiter's trusted in-process `EvaluationRecord`, or proof of filesystem identity. A configured workspace/resource ID is an operator label. Path checks are lexical: the Provider neither opens candidate files nor verifies symlinks, hard links, mounts, actual effects, or native tool semantics. Native permission/security checks remain independent and authoritative.

Exact configured `tool.native_name` values `Edit`, `Write`, `edit`, and `write` map to `replace`; `Delete` and `delete` map to `delete`. A mapping selects exactly one top-level `/file_path` or `/path` string. Write input must contain that path and string `content`; Edit input must contain that path, string `old_string` and `new_string`, with optional boolean `replace_all`/`allow_multiple`. Delete input contains only the selected path. These are explicit operator-declared input contracts, not claims that every Agent exposes these names. Unknown input keys, arbitrary Bash, MultiEdit, multi-resource calls and other dialects are uncovered, never inferred. This narrow projection does not certify native tool behavior.

## Configure and run

There is no `anolisa install` integration or RPM for this independent Provider yet. Source builds are the available developer route; it is not automatically bundled with AW. Build from this directory in a checkout containing the matching public libraries:

```sh
cargo build --release --offline --locked
cargo test --workspace --offline --locked
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --offline --locked -- -D warnings
```

The workspace has its own lockfile and uses the AW toolchain pin. Linux and procfs are required. Public path dependencies are `aw-provider` 0.1.0 and `semantic-arbiter` 0.1.0 from the same source revision; standalone SDK publication is not assumed. `aw-host`/`aw-config` are used only for public integration tests. Offline builds require an already populated dependency cache; no third-party sources are downloaded by the tests.

1. Inspect [examples/config.json](examples/config.json) and the equivalent inline config in [examples/aw.yaml](examples/aw.yaml). Both are **synthetic templates**, not production endpoints or credentials. Replace the binary, Agent, workspace and state paths. Copy the built binary to your independently managed executable location, then select it with `transport.argv` (no Provider CLI arguments).
2. Supply one known native session ID, task version/text, adapter, absolute workspace path and stable operator-owned workspace ID. Task origin is always `operator`. Define explicit path-to-resource mappings and independent allowed/preserved resource lists; preservation overrides allowance. Update the task/version and reload the AW configuration for a new task/session. There is no task discovery, history mining, candidate-derived permission, automatic multi-task selection or session inference. If the native session ID is not known, do not claim coverage.
3. Manually create the record root as the runtime user with mode `0700`, outside the Agent's writable workspace where possible. Every pathname component must be a real directory, not a symlink. Validation opens/stats this existing root without creating files or reading credentials. A missing root, wrong owner or mode is rejected.
4. Keep `model.config_revision: aw_request`. At invocation it is replaced with a domain-separated hash of the full AW request `config_revision`, matching the evaluation binding. Complete native session, instance/binding and call IDs are domain-separated and hashed without truncation. The public `candidate_revision` hashes the complete candidate, including original tool name and JSON input. Task/resource IDs retain their explicit configured meaning.
5. Select a validated public `ModelConfig` backend. Production uses certificate-verified HTTPS; credentials are supplied only through the configured `key_env` in AW's child environment. The example uses explicitly labeled literal-loopback HTTP and a synthetic variable name. Validation does not contact any endpoint. Do not copy real keys into JSON/YAML or records. Configure the actual backend only after reviewing the public Semantic Arbiter backend documentation/source.
6. Start AW from the exact configured workspace using your normal supported Agent setup:

```sh
aw run --config /absolute/path/to/aw.yaml --agent target
```

The Provider process cwd **and** `event.native.cwd` must exactly equal `workspace_path`; session/adapter must also match. Missing instance/binding/call IDs, mismatches and unmapped operations become `uncovered` records without inference. Relative candidate paths are resolved against the configured workspace; `..`, `.`, duplicate/trailing slashes, backslashes and paths outside it are rejected. No filesystem canonicalization or file reads occur.

Use `effects: [observe]` and `on_error: report` as shown. `describe` advertises only `evaluate_task_scope` / `tool.before` / `observe`. Valid invocations always return the original request digest, `status: ok` and `effects: []`; semantic errors/denials remain private observations. A small structured stderr diagnostic exposes only a stable status code. Malformed requests and failed `validate_config` return sanitized protocol errors with zero exit. AW still owns process termination, protocol/transport failures and native admission; observe-only does not guarantee that every host configuration permits a tool.

## Records, bounds and cleanup

Each invocation reserves a unique private `record-*` directory beneath the preexisting root. All record files are `0600`, synchronized and atomically published without overwriting an existing name:

- `00-input.json`: full AW input, persisted before backend construction or inference.
- `10-evaluation.json`: mapped candidate, task, IDs and original budget, when covered and within limits.
- `20-model-request.json`: actual state, questions and backend identity, before inference.
- `30-model-result.json`: parsed reply or sanitized error, including timeout cancellation when the process remains alive.
- `40-result.json`: `evaluated`, `uncovered` or `skipped`, plus the advisory evaluation if available.

`evaluated` means an evaluation response was retained, not that the model answered successfully: inspect `evaluation.judgments` for unknown answers or model errors and `evaluation.outcome.control.candidate` for the advisory decision. `uncovered` means the input or context is outside the supported mapping, including oversized candidates. `skipped` means evaluation could not run, for example because credentials were absent or preparation exhausted the budget.

A killed process can leave only earlier files or `.pending` files. Their reservation remains consumed. No record file authorizes an action; no automatic retry, fallback, cleanup, rotation or remote upload occurs. Initial recording failure prevents inference. Recording failures are visible via the structured stderr code even when a final status cannot be saved.

`max_records` is 1–10,000; `max_storage_bytes` is 4 MiB–1 GiB. Each invocation conservatively consumes a **fixed 4 MiB reservation**, even for tiny/incomplete records. Both limits are checked with a nonblocking directory lock, so concurrent invocations cannot overreserve; lock contention skips inference rather than waiting. The root is opened component-by-component without following symlinks and pinned by descriptor. External mutation by the same user is not a security boundary; unexpected contents are rejected. Manually review/archive/delete owned records only after stopping users of the directory; deleting a record releases its reservation. Protect backups to the same sensitivity level.

AW accepts up to 1 MiB per request. The arbiter accepts 256 KiB per evaluation and 64 KiB of complete native JSON arguments. Larger candidates are recorded intact in `00-input.json` and marked `oversized_candidate`, never truncated for inference. `Ingress::capture` runs before stdin. The deadline starts there, uses the minimum of AW `budget_ms` and model timeout, and includes input waiting, parsing, mapping and persistence; preparation never renews it. A timeout may leave a partial record. The host must enforce its full process lifetime, including output and filesystem stalls.

## Validation and ownership

Tests launch the actual binary, a local HTTP fixture and public AW Host; no real Agent, remote model or real credential is used. They cover allow/deny with empty effects, full candidates, explicit permissions, identity changes, unsupported input, oversized input, malformed config, credentials/errors, private permissions, symlink replacement, nonblocking/concurrent quota admission, atomic publication, killed writers and preparation/HTTP deadlines. These are contract tests, not native Agent adoption or production model-quality evidence.

Maintainer/contact: ANOLISA maintainers via repository issues. Source: this directory and the public libraries under `src/aw` and `src/semantic-arbiter`; license: repository Apache-2.0. See the [Provider contribution guide](../README.md). Disable the AW step/provider and stop active invocations before removing the independently installed binary or privately retained data.

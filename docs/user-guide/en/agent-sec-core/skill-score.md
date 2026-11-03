# Skill safety assessment

[中文版](../../zh/agent-sec-core/skill-score.md)

Skill Score plans tasks for one Skill, executes them in controlled Docker environments, and produces an evidence-qualified safety score. The standard ASC CLI provides native Rust planning, judging and scoring. Dynamic execution is an explicit optional installation. Model-assisted stages consume tokens.

## Install

The fork preview has two independent delivery archives, each with a `.sha256` file. The base `sec-core-bundle-<version>-linux-x86_64.tar.gz` contains the standard base raw package, indexes, release manifest and matching ANOLISA installer. Downloading or installing it does not require the optional bundle. Verify the checksum from the trusted release, extract into an empty directory, then unpack its `v1/anolisa-cli-0.3.20-linux-x86_64.tar.gz` to obtain the installer. The formal default channel may not contain this preview yet.

Install the base component from the extracted repository:

```sh
sudo anolisa --install-mode system install sec-core --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

Base install, update and repair do not obtain the scoring Python/Harbor runtime or build container images. `plan` needs only the base CLI and model configuration; other ASC adapters retain their existing dependencies.

For dynamic execution, download and verify `skill-score-bundle-<version>-linux-x86_64.tar.gz`, then extract it into the same directory. It contains the small optional raw package, source archive and runtime manifest, with Dockerfiles, build scripts, worker code and dependency locks. It contains no prebuilt container images or third-party Python runtime. Provide Linux x86_64, local Docker with BuildKit/Compose, usable network namespaces, firewall/conntrack and ptrace/strace, then explicitly install:

```sh
sudo anolisa --install-mode system install sec-core-skill-score --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

Installation builds the work image and ASC network sidecar from the verified sources, obtains locked Python 3.12/Harbor dependencies, and checks the generated environment. The base image is pinned by digest; package managers inside the Dockerfiles still need network access. Different builds can produce different image IDs because not every system/transitive package is locked. An installation-local `image-build.json` binds the release manifest hash, actual base, tool inventory and both immutable config IDs, plus manifest IDs when present in the OCI export; readiness and execution verify that receipt. No pre-known output digest is claimed. Legacy Docker exports bind verified config IDs without inventing manifest IDs. A failed, timed-out or cancelled build reports the path to its private diagnostic log; successful builds remove that log.

Both images are required for the current execution backend. Allow up to 30 minutes for component preparation (the image-build subprocess has a 25-minute limit), and reserve disk for image layers and temporary local export. Reserve memory for the controller and sidecar in addition to the work container's 4096 MiB. Build/network failure aborts installation without activating an incomplete runtime. `ANOLISA_SKILL_SCORE_SOURCE_DIR` can supply verified Python/uv archives and wheels. `ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE=1` forbids dependency-download fallback but image construction still needs a network-capable build environment; `ANOLISA_SKILL_SCORE_OFFLINE=1` refuses fresh image construction. A restored, prepared FC template does not download or rebuild.

`run` checks readiness and fails with an installation or repair suggestion when dependencies are absent or inconsistent. It never downloads, builds or pulls dependencies. There is no `prepare` command. Updating the base does not automatically update or install the optional component; install/update both to compatible versions explicitly. `install --all` explicitly selects all indexed components, including the optional one.

To remove dynamic execution dependencies, use `sudo anolisa --install-mode system uninstall sec-core-skill-score`. The hook removes owned inactive private environments, including environments retained by repair, while keeping the base component. Active runs or unresolved cleanup records block removal with a recovery message. Do not delete those records merely to bypass the check; first verify recovery of the exact request resources. Shared dependency caches and Docker images are not pruned by uninstall.

## Configure the model service

Place this secret-free configuration in `/etc/agent-sec/model-service.json`, or select another file with `AGENT_SEC_MODEL_CONFIG`:

```json
{
  "schemaVersion": 1,
  "remote": {
    "provider": "bailian-payg-anthropic",
    "baseUrl": "https://dashscope.aliyuncs.com/apps/anthropic",
    "defaultModel": "qwen3.8-max",
    "apiKeyEnv": "AGENT_SEC_MODEL_API_KEY"
  }
}
```

Inject `AGENT_SEC_MODEL_API_KEY` through the invoking environment or platform secret management. Never put its value in arguments, task JSON, templates or release manifests. The Judge and evaluated Agent share the resolved provider configuration; the worker receives only the necessary Agent model credentials. Existing local/Ollama scanner defaults are unchanged.

For an operator-managed Anthropic Messages proxy, set `provider` to `anthropic-compatible` and `baseUrl` to its trusted prefix, for example `https://models.example.invalid/anthropic`. ASC appends `/v1/messages`; do not include that suffix in the prefix. The remaining fields are unchanged. URLs must use HTTP(S), a DNS hostname or IPv4 address, and contain no embedded credentials, query, fragment, whitespace or backslash. Explicit ports are supported. HTTP is for a trusted private network; use HTTPS across untrusted networks. Dynamic execution requires IPv4 resolution and permits the configured host through the existing network sidecar; this is a host policy, not a URL-path policy.

ASC passes the referenced value through the existing Anthropic credential channel, including `x-api-key` for its Rust calls. It does not generate or parse user/task identities or submit billing records. The calling platform must authorize the proxy and generate its access context; the proxy owns authentication and metering. The same configuration supplies planning, judging and Claude Code execution. Keep credentials scoped and short-lived where the proxy supports it: the evaluated Agent receives the value, and an allowed model endpoint is not a data-loss prevention boundary.

The proxy must support the existing Messages request parameters (`thinking` and `output_config` included), token usage fields, and Claude Code streaming/tool calls. Unsupported behavior fails explicitly, without dropping parameters or falling back to a different service. Offline compatibility tests do not establish model quality, network reachability or billing correctness.

An existing but invalid configuration fails. If no configuration file exists, the migration path accepts the legacy `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` pair and emits a deprecation notice. Both legacy variables are required and retain Bailian endpoint restrictions; a custom proxy requires the explicit configuration file. To roll back proxy routing, restore the Bailian configuration above.

## Plan and execute

```sh
agent-sec-cli skillsec score plan \
  /work/input/skill --output-file /work/output/task.json

agent-sec-cli skillsec score run \
  /work/output/task.json --output-file /work/output/report.json \
  --evidence-dir /work/output/evidence
```

`plan` accepts a directory containing `SKILL.md` and creates `agent-sec-skill-score/task-v1`. The content hash covers the complete execution input, including binary files, dependency directories, paths and executable permissions. Symlinks, special files and inputs exceeding the supported bounds are rejected. The model's text coverage is recorded separately. `run` creates and verifies a fresh private snapshot before executing it. Old preview tasks using the partial digest must be replanned; they are not silently upgraded. Preserve the Skill path and content between stages.

`run -` reads stdin. Output files must not exist; evidence directories must be absent or empty. Outputs belong outside the Skill and cannot overlap internal evidence files. Standard output contains one public JSON result, while progress goes to stderr. Use files when a platform truncates command output.

Scoring is explicitly local and independent of the ASC daemon; it does not certify Ledger state. Explicit `--socket`, `--timeout-ms`, `--otel-context` and `--trace-context` options are rejected. `skill-ledger` remains an alias for `skillsec`.

## Read results

| Exit | Meaning |
| --- | --- |
| `0` | The requested stage completed; `run` completed execution, adjudication and resource cleanup. A risk finding may still block the Skill. |
| `1` | Dependency, execution, model, evidence, cancellation or cleanup failure. Retain any partial report. |
| `2` | Invalid input, schema or content binding, including a changed Skill or old digest. |

For task-file, Skill-path, runtime-file, worker-start and artifact I/O failures,
stderr includes `stage=... path="..."` followed by the operating-system error.
For example, `stage=task.resolve path="/work/input/task.json": No such file or directory (os error 2)`
identifies a missing task input, while `runtime.base-manifest.read` identifies
an installation record and `runtime.image-receipt.read` identifies the prepared
image receipt. Paths are quoted and control characters escaped; task contents
and worker arguments are not included. Exit codes and report schemas are unchanged.
Keep stderr even when no report was generated. In SkillEval, inspect the returned
`artifact_dir` for `cli.log` and `runtime.log` when log collection succeeded.

Report-v1 binds `content_hash` to the task and separates `decision`/`score` from `execution_status`, `dynamic_complete` and `cleanup_status`. Consumers require complete execution and cleanup; a score alone is insufficient. `execution[].completed` describes task completion, which differs from complete observation, probe activation and risk confirmation. An evidenced failure in the original Skill can be assessable even when its business task failed. An early Critical stop leaves unexecuted probes explicitly incomplete.

`planning_notes` preserves unverified, non-blocking planning context. Runtime limitations, missing evidence and unsupported environments remain visible. Do not infer success by filtering limitation text. Preserve the task, report, `internal-report.json`, `evidence-catalog.json` and collected artifacts together. Evidence is bounded and read without following links; collection hashes establish integrity after collection, not independent authenticity of files writable by the evaluated container. The deployment owner controls artifact access and retention.

Each `execution` item separates `execution_complete` (execution and evidence are complete) from `completed` (the business task succeeded). An Agent refusal or a Skill error can be fully assessed without making the business task successful.

## Platform integration and release migration

ASC uses unmodified Harbor 0.22.0 through its public task Compose API. The work container shares the ASC sidecar's network namespace without Docker socket or network-management capabilities. The sidecar uses a fixed model-host TCP policy, preserves direct local/ICMP behavior, and rejects other traffic. This is not a general-purpose domain firewall. It uses an independent read-only hostname mapping; changing the work container's hosts file does not redirect the proxy. IPv6 needs separate acceptance.

SkillEval owns FC templates, outer instance lifetime, credential injection, transfers, cancellation and operations. Template construction explicitly installs the matched base and optional components, and builds/verifies both images once. Restored instances execute the same ASC CLI as a standalone host. ASC owns only the inner request's Docker resources and reports their cleanup separately from SkillEval's outer FC cleanup.

Skill dependencies must be available inside the ASC work image. Installing a document library or office tool only in the outer FC template does not make it available to the evaluated Skill. A deployment can reuse a pinned functionality-image Dockerfile to build its business dependencies, then add the ASC Agent and observation layer. Record the recipe commit and checksum, upstream base digest, actual installed versions and resulting image identities. Rebuilding a recipe does not establish that the result is identical to an existing published image. Standalone and FC installations each retain their verified image-build receipt.

After the feature joins mainline and passes the same acceptance gates, switch the trusted release/index origin from the fork to the formal release channel. Keep the CLI, component names, model configuration and task/report contracts. OCI distribution is a future optimization, not a prerequisite for the first or formal release.

## Build and verify

Develop on Linux using the pinned Rust toolchain and Docker/Compose. `packaging/skill-score/build-preview.sh` builds the native base, optional source component, matched installer and release-v4 metadata, then emits two independent bundles. Source and artifact identities bind the release. The former `RAW_SKILL_SCORE_PAYLOAD` mechanism is rejected; scoring Python 3.11 is not included. The retained Python scorer serves as a differential test oracle.

For a prepared business base, set `ANOLISA_SKILL_SCORE_BASE_IMAGE` at explicit installation to a registry reference pinned by digest or a local immutable Docker image ID. A local image also requires `ANOLISA_SKILL_SCORE_BASE_RECIPE`, a JSON file with `source_repository`, `source_commit`, `dockerfile_path`, `dockerfile_sha256` and `upstream_base_image` (`reference`, `image_id`, `platform`). The builder validates the source/upstream identity and records actual output IDs in the installation-local receipt. These are trusted deployment inputs, not per-task options. Do not confuse Docker IDs with archive checksums or invent registry digests for images that have not been pushed.

The preview workflow checks installed package behavior and network controls before publication. Independent Linux CLI and FC SkillEval model runs remain distinct acceptance surfaces. Freeze labels and report normal/violating samples, activation, incomplete runs and cleanup separately; a small zero-false-positive sample is not a population-wide error-rate claim. See the [native design](../../../../src/agent-sec-core/docs/design/skill-score-native-design.md) for rollback and acceptance requirements.

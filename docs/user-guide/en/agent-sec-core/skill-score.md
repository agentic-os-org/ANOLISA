# Skill safety assessment

[中文版](../../zh/agent-sec-core/skill-score.md)

Skill Score plans tasks for one Skill, executes them in controlled Docker environments, and produces an evidence-qualified safety score. The standard ASC CLI provides native Rust planning, judging and scoring. Dynamic execution is an explicit optional installation. Model-assisted stages consume tokens.

## Install

The first native preview uses a matched fork release with two raw components, one separate image archive and an installer supporting candidate preparation. The formal default channel may not contain this preview. Use the installer included in the trusted release (version 0.3.20 or newer), and verify every artifact against its release-v3 manifest before executing it. SkillEval's bundle script prepares the local repository automatically.

After arranging the verified raw packages and indexes under a local `v1/` repository, install the base component:

```sh
sudo anolisa --install-mode system install sec-core --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

Base install, update and repair do not download Python, Harbor, Docker or images for scoring. `plan` needs only the base CLI and model configuration. Existing Python dependencies for other ASC adapters are separate.

For dynamic execution, first provide a Linux x86_64 host with a local Docker daemon, Compose, usable network namespaces, firewall/conntrack support and ptrace/strace. Reserve memory for the controller and sidecar in addition to the work container's 4096 MiB allocation. Then explicitly install the optional component at the same version:

```sh
sudo anolisa --install-mode system install sec-core-skill-score --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

This small raw package contains ASC's worker, shared contracts, dependency locks and installation helper. Installation obtains the locked Python 3.12/Harbor dependencies from their upstream distribution sources. It verifies the separate `skill-score-images.tar.gz` archive before loading it, then checks both immutable image IDs and their platform. Neither raw package embeds the container images. The work image and ASC network sidecar are both required; there is one network implementation for standalone Linux and FC.

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

An existing but invalid configuration fails. If no configuration file exists, the migration path accepts the legacy `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` pair and emits a deprecation notice. Both legacy variables are required.

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

Report-v1 binds `content_hash` to the task and separates `decision`/`score` from `execution_status`, `dynamic_complete` and `cleanup_status`. Consumers require complete execution and cleanup; a score alone is insufficient. `execution[].completed` describes task completion, which differs from complete observation, probe activation and risk confirmation. An evidenced failure in the original Skill can be assessable even when its business task failed. An early Critical stop leaves unexecuted probes explicitly incomplete.

`planning_notes` preserves unverified, non-blocking planning context. Runtime limitations, missing evidence and unsupported environments remain visible. Do not infer success by filtering limitation text. Preserve the task, report, `internal-report.json`, `evidence-catalog.json` and collected artifacts together. Evidence is bounded and read without following links; collection hashes establish integrity after collection, not independent authenticity of files writable by the evaluated container. The deployment owner controls artifact access and retention.

Each `execution` item separates `execution_complete` (execution and evidence are complete) from `completed` (the business task succeeded). An Agent refusal or a Skill error can be fully assessed without making the business task successful.

## Platform integration and release migration

ASC uses unmodified Harbor 0.22.0 through its public task Compose API. The work container shares the ASC sidecar's network namespace without Docker socket or network-management capabilities. The sidecar uses a fixed model-host TCP policy, preserves direct local/ICMP behavior, and rejects other traffic. This is not a general-purpose domain firewall. It uses an independent read-only hostname mapping; changing the work container's hosts file does not redirect the proxy. IPv6 needs separate acceptance.

SkillEval owns FC templates, outer instance lifetime, credential injection, transfers, cancellation and operations. Template construction explicitly installs the matched base and optional components, and loads the verified archive once. Restored instances execute the same ASC CLI as a standalone host. ASC owns only the inner request's Docker resources and reports their cleanup separately from SkillEval's outer FC cleanup.

Skill dependencies must be available inside the ASC work image. Installing a document library or office tool only in the outer FC template does not make it available to the evaluated Skill. A deployment can reuse a pinned functionality-image Dockerfile to build its business dependencies, then add the ASC Agent and observation layer. Record the recipe commit and checksum, upstream base digest, actual installed versions and resulting image identities. Rebuilding a recipe does not establish that the result is identical to an existing published image. The resulting ASC image is distributed through the same verified archive and used by both standalone and FC execution.

After the feature joins mainline and passes the same acceptance gates, switch the trusted release/index origin from the fork to the formal release channel. Keep the CLI, component names, model configuration and task/report contracts. OCI distribution is a future optimization, not a prerequisite for the first or formal release.

## Build and verify

Develop on Linux using the pinned Rust toolchain and Docker/Compose. `packaging/skill-score/build-preview.sh` builds the native base, optional raw component, image archive, matched installer and release-v3 metadata. Source and artifact identities bind the release. The former `RAW_SKILL_SCORE_PAYLOAD` mechanism is rejected; scoring Python 3.11 is not included. The retained Python scorer serves as a differential test oracle.

For a prepared business base, set `ASC_WORK_BASE_IMAGE` to a registry reference pinned by digest or a local immutable Docker image ID. A local image also requires `ASC_WORK_BASE_RECIPE`, pointing to JSON with `source_repository`, `source_commit`, `dockerfile_path`, `dockerfile_sha256` and `upstream_base_image` (`reference`, `image_id`, `platform`). The build verifies the upstream reference and platform against Docker, records the recipe in the work-base metadata, and appends `.base.<12-character-image-ID>` to the preview version. Custom-base packages therefore do not share the automatic default preview's package names or release tag. These are maintainer build inputs, not options required for end users to run a Skill. A Docker image ID may identify its configuration or manifest depending on the storage backend; it is not an archive checksum or an invented registry digest.

The preview workflow checks installed package behavior and network controls before publication. Independent Linux CLI and FC SkillEval model runs remain distinct acceptance surfaces. Freeze labels and report normal/violating samples, activation, incomplete runs and cleanup separately; a small zero-false-positive sample is not a population-wide error-rate claim. See the [native design](../../../../src/agent-sec-core/docs/design/skill-score-native-design.md) for rollback and acceptance requirements.

# Skill Score Mainline Architecture and Delivery Design

[简体中文](skill-score-native-design_zh.md)

This document records the approved architecture implemented on this branch. Validation is established by test records bound to commits and artifacts. Development started from ASC `codex/skill-score-public-compose@0f9355446ed3831b90826a1e8f21f72c2ad915a5` and SkillEval `codex/security-fc-integration@6cb567a7e62ab4def8703c042d42dabcaec61989`. Official main `23cd43a3df09aff4988528171af1b89fc5e742fb` was fetched on 2026-10-27; this branch has not been rebased onto it. See the [previous runtime design](skill-score-runtime.md) for the historical Python preview.

This revision fixes three delivery decisions: the release carries small image-build sources, base ASC stays lightweight by default, and dynamic-assessment execution dependencies require explicit installation. OCI registry distribution is a future optimization, not a parallel first-release implementation, mainline merge condition, or formal-release prerequisite.

## 1. Goal and agreed choices

Make dynamic Skill security assessment part of AgentSecCore (ASC) while keeping base installation lightweight. The standard `sec-core` package contains the Rust commands and scoring core; users explicitly install an optional execution component when they need dynamic assessment, then invoke `agent-sec-cli skillsec score plan/run`. SkillEval is one platform that calls this component. Standalone Linux and FC assessments use the same core, worker, and network implementation.

| Area | Design decision |
| --- | --- |
| Public entry point | Retain `agent-sec-cli skillsec score plan/run`; add no `prepare` command, separate scoring product, or service |
| Scoring implementation | Rust owns planning, model adjudication, evidence decisions, deduplication, scoring, and reports |
| Dynamic execution | Retain the Python 3.12 worker and unmodified Harbor; do not rewrite the third-party execution framework |
| Networking | Use the same ASC compatibility sidecar on ordinary Linux and FC; users do not select a backend |
| Installation | Base ASC does not automatically install scoring execution dependencies; an explicitly installed optional component contains the first-party worker, locks, compatibility manifest, and installation helper |
| Image distribution | Publish build sources only; explicit installation builds both images and verifies a source-bound local receipt |
| Runtime behavior | Check readiness before execution; report missing or incompatible dependencies with repair guidance; do not install, pull, or build during assessment |
| Model access | Extend the shared ASC model client; scoring and the evaluated Agent share configuration resolution while existing scanners retain their local defaults |
| Platform ownership | SkillEval retains FC templates, instances, credential injection, uploads/downloads, task state, timeouts, cleanup, and operations |

Default installation adds no scoring-specific Python, Harbor, Docker, or image downloads, and does not enable dynamic execution automatically even when Docker is present. The first release of dynamic execution still requires Linux, working Docker/Compose, compatible kernel capabilities, and sufficient resources. Installing the optional component does not imply that ASC can modify any host kernel or grant cloud permissions.

## 2. Architecture, processes, and responsibilities

| Location | Component | Responsibility and call relationship |
| --- | --- | --- |
| User terminal or SkillEval scheduler | Caller | Submit a Skill through the fixed CLI and file contracts; read tasks, reports, and evidence |
| Execution host: standalone Linux or FC instance | Rust CLI + `asc-skill-score` | Create input snapshots, plan probes, invoke the worker, adjudicate evidence, and produce reports; this is the assessment controller |
| Separate process on the same host | Python Harbor worker | Call Harbor APIs, create probe environments, initialize fixtures/mocks, run the Agent, collect and normalize evidence, and reclaim this run's resources |
| Docker environment for each probe | Work container | Run the evaluated Agent, Skill, and observation tools without a Docker socket or network administration capabilities |
| Separate container for the same probe | ASC network sidecar | Enforce fixed egress policy; share a network namespace with the work container while holding the required network capabilities |
| Outside the controller and work container | Model service | Receive separate requests from the Rust Judge and evaluated Agent; attribute outputs and costs to each role |

On standalone Linux, the user invokes ASC directly and ASC manages the inner Docker resources. With SkillEval, the platform creates an FC instance and runs the same ASC command inside it: ASC owns inner Docker resources, and the platform owns the outer FC instance. ASC contains no FC SDK, template ID, account information, or cloud management credentials.

FC isolates the assessment instance as a whole. Inner containers still separate the untrusted evaluated program from the controller. The controller, work container, and network sidecar therefore remain distinct rather than becoming one process with all privileges.

### 2.1 Rust and Python boundary

Add only one business-logic crate: `src/agent-sec-core/v2/crates/asc-skill-score/`.

| Module | Responsibility |
| --- | --- |
| `contracts.rs` | Types and validation for task/report and internal Finding/Probe objects |
| `source.rs` | Input snapshots, complete content digests, path and output boundaries |
| `scanners.rs` | Reuse V2 prompt/code/PII capabilities and migrate necessary scoring-rule adapters |
| `fixtures.rs`, `masb.rs`, `redaction.rs` | Probe fixtures, rule branches, and structured redaction |
| `judge.rs` | Prompts, structured output, retry budgets, and model-call audit records |
| `evidence.rs` | Evidence trust, actor attribution, dynamic conclusions, and the Critical confirmation threshold |
| `score.rs` | Root-cause deduplication, deductions, exact rounding, and final reports |
| `worker.rs` | Installed-version checks, subprocess protocol, timeout/cancellation, and resource state |
| `lib.rs` | Static signals, source audit, probe planning, `plan`/`run` orchestration, and the overall budget |

The existing `asc-cli` `skill_score.rs` changes from executing a private Python CLI to calling this crate directly. The Python worker retains Harbor hooks, evidence recovery after failures, and ATIF/observer adapters. The first release does not rewrite all observation normalization merely to make the worker thinner. The worker does not act as the model Judge, choose risk severity, or compute final deductions.

Remove the scoring-specific Python 3.11 environment, not all Python components in ASC; other existing adapters are unaffected. Keep the old Python scoring implementation as a migration test oracle and remove it from release packages after equivalence checks pass.

Existing mainline rules emphasize the V2 Rust runtime and daemon path. The same mainline PR series must explicitly add a contract for a directly invoked local assessment capability: it does not connect to the daemon and is not a fallback when the daemon is unavailable. Its Python Harbor adapter is an explicitly listed external execution dependency. Renaming or separating processes alone does not establish compliance with the existing rules.

## 3. User installation and execution

These commands describe the native Rust core and optional component added on this branch. Preview installation uses the bound fork release index and installer; availability through the official channel requires mainline integration and a formal release.

1. Install base ASC through the standard entry point, without scoring execution dependencies or a Docker requirement.
2. When dynamic assessment is needed, prepare Docker/Compose and the required permissions, then explicitly install the optional `sec-core-skill-score` component. It obtains locked third-party dependencies and builds both images from verified sources, binding actual outputs in a local receipt. Users need not select a network backend, handle image files, or install third-party libraries individually.
3. Configure the shared model service and credentials once.
4. Generate a plan, execute it, and read the report and evidence directory.

```bash
sudo anolisa --install-mode system install sec-core
sudo anolisa --install-mode system install sec-core-skill-score

agent-sec-cli skillsec score plan /absolute/path/to/skill \
  --output-file /absolute/path/to/task.json

agent-sec-cli skillsec score run /absolute/path/to/task.json \
  --output-file /absolute/path/to/report.json \
  --evidence-dir /absolute/path/to/evidence
```

Retain `run -` for stdin, one JSON object on stdout, progress on stderr, and refusal to overwrite existing output files unexpectedly. Output directories must be outside the evaluated input. `plan` uses the Rust core in the base package and model configuration; it requires neither the optional execution component nor Docker/Harbor. `run` requires the full execution environment. If the optional component is absent, it returns nonzero with explicit installation guidance; if an installed component is damaged, it recommends repair. Neither case triggers downloads.

This is an optional installation unit within ASC, not a second public CLI or a new service. Reuse the existing `anolisa install <component>` form without adding `--with`, `score install`, or `prepare`. The current installer has no implemented automatic component dependency solver. The first release therefore requires installing base ASC first; the optional component's helper checks its version rather than assuming that a dependency declaration installs it automatically.

The lightweight guarantee applies to `anolisa install sec-core`. The existing `anolisa install --all` enumerates every component and will include the optional component once registered. Document this as an explicit request to install everything; do not describe `--all` as equally lightweight.

### 3.1 Shared model configuration

Add `/etc/agent-sec/model-service.json`, with `AGENT_SEC_MODEL_CONFIG` overriding its path. The first release has one shared remote configuration; it introduces no provider routing, multiple profiles, or new secret service.

The format below retains the preview's default model name without promising that model's long-term availability:

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

Configuration stores only a key reference; the user session or platform secret management injects its value. The `bailian-payg-anthropic` provider retains its HTTPS endpoint restrictions. The explicit `anthropic-compatible` provider accepts an operator-authorized HTTP(S) DNS/IPv4 prefix and appends `/v1/messages`; it rejects URL credentials, query, fragment, whitespace and backslashes, and never follows redirects. Judge and Agent share the resolved provider/model and opaque credential. Platform identity generation, endpoint authorization and metering belong to the caller/proxy, not ASC. Platform-specific provider names must be translated before creating ASC configuration. Judge requests to compatible proxies use the basic Messages fields (`model`, `system`, `messages`, and `max_tokens`). Bailian requests additionally set temperature, thinking and effort. Both providers use the same retry budget, usage parsing and redaction. Verify real inference separately for each proxy.

Add an explicit remote entry point and Bailian implementation to `asc-model-client`. Keep the existing `create_client()`, Ollama local-address restriction, and scanner defaults unchanged. Scoring uses the remote entry point; weakening the local factory's validation must not silently change all scanners.

Resolve configuration once. The Rust Judge uses a typed client; the worker receives only the internal `ANTHROPIC_*` environment needed by the Agent. An existing but invalid shared configuration file fails directly rather than being completed from legacy variables. When the file is absent, migration compatibility accepts the legacy `ANTHROPIC_API_KEY` and `ANTHROPIC_BASE_URL` pair, retains validation requiring both, and emits a deprecation notice without the key. Removing this compatibility later requires a separate announcement.

Redaction of logs, source displays, and model inputs uses the actual resolved set of secrets, not just one legacy variable. The evaluated Agent receives necessary model credentials but no cloud management credentials, registry credentials, or unrelated host environment. Deployment owners control model-key quotas and lifetimes. Allowing a model endpoint does not prevent all data transfer through that endpoint.

## 4. Input, execution, and report contracts

### 4.1 Bind planning and execution to the same input

`plan` first creates a private input snapshot, then computes its digest and audits that snapshot. `run` creates a new private snapshot from its input, verifies the digest, and passes only the verified snapshot to Harbor. Changing the original directory after planning produces an explicit requirement to plan again.

The digest covers every file actually delivered to the Agent, its relative path, type, and execution-relevant permission bits, including dependency directories and binaries. Text limits for model review no longer define the scope of the execution-input digest. The first release rejects symbolic links, special files, escaping paths, and inputs exceeding limits, identifying the offending path. It does not silently omit files and continue. Future link support requires explicit materialization and digest rules plus separate tests.

The source catalog accepts up to 32,768 reviewed lines and 4 MiB of expanded reference keys and line text. Exceeding either budget fails input validation before model calls.

The model may review only part of the input. Reports distinguish the complete execution-input digest from model-review coverage. Truncation, scan failure, and missing essential context must not be interpreted as an absence of risk.

The source view uses collision-free opaque markers and `source_redaction` provenance. Equal protected source spellings share a marker; language escapes, length, substrings, encoding and regex behavior are not inferred from it. The original execution bytes and digest remain unchanged. Planning rejects inserted markers in executable inputs or expected business results. Serialized JSON inside evidence is sanitized recursively with depth and size bounds; unsupported duplicate views are omitted rather than exposed unchanged.

Probes normally use supplied inputs. An optional `input_mode: fixed_target` represents an original script that directly references the fixed guard file or mock endpoint without external input. Its `fixed_target_refs` must identify lines in that entry script and in the targeted finding, including the exact quoted target literal. No dummy fixture, payload, input preparation or mock response is permitted. Computed targets remain unsupported by this mode. Source binding locates the probe; dynamic confirmation still requires actual entry execution, a matching process tree and correctly ordered effect evidence. Invoke the absolute `/skills/subject/<entry_path>`: a same-named copy is not the entry, and a relative path with unobserved current directory cannot establish activation. No input read is invented. Canary disclosure additionally requires an observed canary read and the matching sink effect from that origin. Business oracles for all probes exclude reserved guard, canary and sink effects; those belong to security activation and findings.

Retain the public `agent-sec-skill-score/task-v1` field structure and `skill_path`/`content_hash` binding, while replacing the current partial-content digest with the complete digest. Old plans may fail the new validation and **must be regenerated with `plan`**; do not accept or rewrite old digests automatically. This is an explicit upgrade compatibility change requiring release notes and SkillEval contract tests. If implementation needs new required fields, increment the task schema rather than silently adding required conditions to v1.

### 4.2 Internal worker protocol

Continue using short-lived subprocesses and private request/response files; add no service, socket, or cross-language FFI. Identify the internal protocol explicitly as `agent-sec-skill-score/worker-v1`.

Requests contain a request ID, `environment|run` operation, snapshot path, evidence path, deadline, required Agent configuration, and one probe. They contain no secrets or Judge answers. Responses contain the same version and request ID, a result or classified error, evidence coverage, resource identifiers, and cleanup state. Write responses to a temporary file and replace atomically; stdout/stderr are logs only.

Rust locates fixed worker and Python installations through its own release manifest and verifies code, protocol, Harbor, and image versions. It does not fall back to system Python. Inherited environment uses an explicit minimal allowlist; executable paths and arguments are fixed. Files have private permissions, and deserialization enforces type, size, and path limits.

### 4.3 Network and evidence boundaries

Maintain one ASC sidecar implementation without modifying Harbor or offering two selectable network policies. Its compatible iptables match path avoids the observed native nft FIB expression problem, but other kernel and permission prerequisites remain. Test capabilities rather than inferring support from a kernel version.

The work container drops `NET_ADMIN/NET_RAW` and shares the sidecar's network namespace. The trusted controller generates fixed policy and hostname-to-IPv4 mappings and mounts them read-only into the sidecar. Policy and sidecar health must be established before starting the evaluated program. Preserve the current local-destination/ICMP rules, permitted TCP destinations, and denial of other egress; test the actual scope of those exceptions. The first release does not claim support for arbitrary network protocols, IPv6, or every cloud kernel. Proxy exit must not remove blocking rules, and policy failure must never fall back to an open network.

Before submitting evidence to a model, ASC itself validates paths, file types, individual and total sizes, and digests. Reject link escapes, untrusted hard links, special files, and archives inconsistent with their declared contents. Use restricted directories and safe file reads to prevent replacement after checks; SkillEval's post-download validation alone is insufficient. The work container cannot write controller configuration, final reports, or trusted manifests. Model decisions cite evidence with provenance; deterministic code computes the final score without executing arbitrary model-generated commands.

Record the producer and the parties able to modify each evidence type. Logs and snapshots writable by the evaluated container require verification: their digests establish consistency after collection, not that the evaluated program did not fabricate them. High-confidence dynamic effects require independently verifiable observation or corroborating evidence. Self-reported files alone cannot justify a stronger conclusion; this requirement does not add an external observation service in the first release.

Remove the actual model key as bytes, including from binary evidence. Reject secret-bearing paths, oversized evidence, and evidence that cannot be processed safely; discard the request's raw directory while retaining sanitized errors and resource coordinates. If resource cleanup is unconfirmed, reject and discard that request's raw evidence because a remaining container may still modify it. Retain recovery coordinates outside the exportable tree; if disposal fails, return a fatal error without producing a report. Parse JSON, redact its structure, then serialize it. Source redaction masks actual secrets without corrupting ordinary expressions such as `token = Path(...).read_text()`. Label original-content digests separately from redacted displays so redaction does not change audit meaning.

### 4.4 Express completion, risk, and cleanup separately

Retain the existing risk and scoring fields in `agent-sec-skill-score/report-v1`. Add the following optional fields with corresponding consumer tests:

| Field | Meaning |
| --- | --- |
| `execution_status` | `complete / incomplete / failed / cancelled`: state of the execution and assessment chain |
| `dynamic_complete` | No blocking planning errors and every planned probe completed with required evidence; zero probes must not count as dynamic success |
| `execution[].execution_complete` | Execution and evidence are complete for this probe, separate from business-success field `completed` |
| `cleanup_status` | Inner resources managed by ASC: `complete / pending / unknown` |
| `failure_stage` and error details | Failure location, such as environment, Agent, collection, model adjudication, or cleanup, with actionable information |

The native Rust path emits these fields; older preview reports do not have the same completion contract. A complete assessment can still find risk. An Agent refusal proves only that refusal in this run, not that an attack occurred or the sandbox blocked it. Keep `planning_notes` separate from blocking `limitations`.

A completed probe has finished execution with sufficient evidence for a conclusion; it does not necessarily mean the business task succeeded or the dynamic target activated. Record those outcomes separately. A risk conclusion does not substitute for coverage of every planned probe. Even when confirmed Critical risk stops execution early, list unexecuted probes, set `dynamic_complete=false`, and return nonzero. An explicit early-stop reason distinguishes this outcome from infrastructure failure.

Preserve blocking `planning_errors` in the report and fail completeness when they exist: valid probes cannot hide failures to plan other required probes. Nonblocking explanations remain in `planning_notes`. This explicitly tightens the historical preview's completeness behavior.

Public exit codes: `0` means the assessment chain produced a complete result; risk decisions `blocked/needs_review` alone are not infrastructure failures. `2` means an input, contract, or content-binding error. `1` means an environment, model, execution, evidence-coverage, cancellation, or unconfirmed-cleanup failure. **Incomplete dynamic execution returns nonzero even when a score exists**, tightening current behavior. Preserve valid partial reports where possible on nonzero exits; consumers must check both the report and exit code.

Do not collapse safety judgment, dynamic completion, and resource cleanup into one Boolean. SkillEval's outer `cleanup_pending` continues to describe platform-instance cleanup and separately consumes ASC's inner state. Deleting the outer instance must not rewrite an inner failure as success.

Harbor mounts per-request staging directories for Agent logs, verifier logs and artifacts. It takes a bounded snapshot of regular files into a host-owned directory before converting trajectories or reading verifier rewards. Only the host converter writes the final trajectory. If cancellation is observed while committing the assessment, the report records `execution_status=cancelled`, `failure_stage=cancellation` and exit code 1.

## 5. Timeouts, cancellation, and failures

`run` proceeds through input/output-path validation, installation and environment checks, probe execution, evidence collection and validation, model adjudication, deterministic scoring, then final cleanup and report finalization.

The total budget covers probes, model calls, and retries, with explicit time reserved for collection and cleanup. Authentication errors, invalid configuration, and protocol mismatches fail directly. Only classified transient errors may retry within the remaining budget, respecting Retry-After. Repairing structured model output has its own bounded model retry policy; it must not combine with network retries into an unlimited budget.

On cancellation, stop scheduling new work and send SIGTERM to the independent worker process group so Harbor can recover evidence and reclaim resources. Use the current 45-second cleanup grace period initially, then forcibly terminate only this run's process group if necessary. Afterwards, inspect/reclaim only the Compose project owned by this request; never run a global prune. Killing a process does not prove its containers were deleted. If cleanup cannot be confirmed, preserve resource identifiers and `pending/unknown` state.

Derive the Compose project name and ownership labels from the request ID in advance and record them in the controller's private resource ledger before creation. Resources must remain discoverable if the worker crashes; the final response is not the sole source for finding resources that need cleanup.

In FC, SkillEval's instance TTL and outer destruction provide the final resource backstop. The platform deadline must include ASC cleanup and artifact-download time; it must not destroy the instance immediately when the Agent timeout expires.

## 6. Release artifacts and installation transaction

### 6.1 Public source release and local build receipt

The preview publishes four files: a lightweight `sec-core-bundle-<version>-linux-x86_64.tar.gz`, a small optional `skill-score-bundle-<version>-linux-x86_64.tar.gz`, and one `.sha256` companion for each. The base bundle contains the standard base raw package, indexes, installer and release manifest; the optional bundle contains the optional raw package, runtime manifest and source archive. Both extract into the same `v1/` repository without duplicate payload files. Default installation never needs the optional archive.

`release-v4` and `runtime-v2` bind build inputs rather than prebuilt outputs. The optional raw package includes the first-party worker, locks, Dockerfiles, build script, observer/scanner sources and ASC sidecar sources, each covered by the runtime file inventory. It includes no image layers, Python interpreter or dependency wheels. Explicit installation obtains locked third-party runtime dependencies and builds the two images. No extra public prepare command or user-selected network backend is introduced.

The default work base is pinned by registry digest. For a business base, a trusted installer may supply `ANOLISA_SKILL_SCORE_BASE_IMAGE` (immutable registry reference or local image ID) and `ANOLISA_SKILL_SCORE_BASE_RECIPE` (the existing fixed source/upstream provenance JSON). SkillEval builds its own private business recipe and supplies that result; its Dockerfile is not copied into the public ASC release. Installation can use `ANOLISA_SKILL_SCORE_BUILD_NETWORK=host` when required by the build host, otherwise Docker's default build network is used. This does not change the evaluated Agent's restricted network policy.

The builder checks tool versions, inventory and platform, locally exports the images to verify their config IDs and, when available, OCI manifest IDs, and removes the temporary export. It records `image-build.json` beside the generated worker environment. The receipt binds the immutable runtime-manifest SHA256, ASC source/version, actual base/provenance, tool inventory and exact work/egress image IDs. Readiness binds the receipt's SHA256. Rust and the worker independently validate receipt ownership and use only verified IDs; mutable tags are not execution authority.

Different installations can produce different image IDs: the source and base inputs are fixed, but system packages and some transitive dependencies are not fully reproducible. Acceptance binds both the published source bundle and the local build receipt. This design reduces Release size; it does not promise smaller first-install downloads. Builds require Docker BuildKit, network access and temporary disk capacity. Failure, timeout or cancellation must not activate an incomplete environment; only owned build processes are terminated, without global image pruning. Build diagnostics are stored in a mode-0600 cache log. Failure reports its path without echoing its contents; success removes the log.

### 6.2 Optional-component installation, updates, and repair

When the user explicitly installs or updates the optional component, the installer verifies the package and manifest, checks the base ASC version and host, verifies build sources and dependencies, prepares a separate runtime and builds the images, validates imports, protocol, Docker/Compose, both image IDs, and network capabilities, then activates the optional component and manifest and records success.

Base-package installation is independent of Docker state. Once the user explicitly requests the optional component, missing Docker, an unavailable daemon, insufficient permissions, download failures, hash mismatches, and dependency conflicts must fail that installation clearly. Do not record scoring-ready state or affect installed base ASC. Failed preparation during an optional-component upgrade retains the old execution environment and reports an error; it does not downgrade to “base-only installation.”

Store runtimes by manifest digest. The CLI locates the runtime matching its own manifest rather than a global “latest worker.” Mark ready only after preparation succeeds. Repeated raw installation checks the explicit business base image and recipe against the saved receipt before reporting readiness. Runtime capability checks still run because Docker or kernel conditions can change.

Before this change, raw initial installation had hooks, while the update path performed only dependency preflight. This branch adds candidate preparation to `distribution/anolisa` for both install and update, completing lengthy preparation before removing old files. The generic installer owns verified component steps, timeouts, cancellation, and transaction records; ASC's helper owns Python/Harbor/image knowledge. The base raw package requires ANOLISA 0.3.21 for installer-owned discovery contracts. The optional raw package requires 0.3.22 for candidate preparation. This manager also records the original repository when installing or replaying the base. Public installation records identify the index by SHA256. The manager resolves that fingerprint through an owner-only repository file. Skill Score reads the installed base contract snapshot to match versions. The preview binds the digest of the installer built with candidate preparation support.

Run the new helper from the verified candidate package's staging directory. Do not depend on an installed path that does not exist on first installation or still contains the old helper during an upgrade. Preparation and subsequent activation share the existing component installation lock and transaction records so concurrent installations cannot overwrite one another.

Same-version installation/update can currently return NoOp, so simply recommending reinstallation of the same version cannot repair deleted dependencies. Extend the existing repair lifecycle so `anolisa repair sec-core-skill-score` rebuilds missing runtimes or images from that component's installed manifest. Base `anolisa repair sec-core` does not install it. If the optional component has never been installed, recommend install; repair must not enable a new capability implicitly. This branch implements the component target and dependency repair. `run` offers guidance without downloading anything itself.

Base ASC and the optional component use the same ASC release version and compatibility manifest, checked during installation and execution. The first release adds no cross-component version solver or atomic upgrade framework. Update each explicitly; while versions are incompatible, `run` refuses execution and identifies the matching component update needed. Updating base ASC must not fetch dynamic-execution dependencies the user has not selected; base capabilities remain usable.

The system Docker socket group can acquire shared lifecycle leases and create private worker request records. Installation, repair and removal acquire an exclusive lease and retain unfinished requests. Entries that do not match a complete worker record cannot block these operations. For records from an unauthorized or former user of a legacy shared directory, the manager checks containers, networks, and volumes on the local daemon. A matching Compose project or an unavailable resource inventory requires operator recovery.

The base package owns the sole public CLI. The optional component owns its worker, versioned runtimes, ready state, and installation records. The packages must not own the same files or overwrite each other's entry points. Uninstalling the optional component preserves base ASC and reclaims only resources it owns that are not used by active tasks, without global image cleanup.

The helper must be idempotent, bound downloads, avoid secrets in logs, and terminate its subprocess group on cancellation. Dependency-preparation failure leaves old files and runtimes usable; activation failure restores old files and the manifest. Retain old dependencies and images so running tasks do not lose resources. Verified caches may remain for retries; the helper removes incomplete temporary runtimes. System-package installation has external side effects that may not be fully reversible; report residue rather than claiming full rollback. Global image cleanup is prohibited.

Restoring an old version must also restore its matching manifest and runtime. The current update interface rejects downgrades, so do not promise an existing one-command `update` rollback. Provide a verified operational recovery procedure for the first release. A generic downgrade command is a separate decision, not a new requirement of the scoring architecture.

### 6.3 Host, template, and offline boundaries

Use the standard raw path on Ubuntu; do not require forcing RPMs onto Ubuntu. RPM remains an alternative format. A scoring-enabled RPM needs separate validation of dependency installation, scripts, and failure semantics; raw-hook guarantees cannot simply be assumed for it.

Completing scoring preparation requires a working Docker daemon to build and check inner images. During FC template finalization, the platform starts Docker, then explicitly installs both the base package and optional component and runs acceptance checks. A Dockerfile build stage without a daemon may populate download caches but cannot mark scoring ready; image construction and checks must still complete later. An FC template is a versioned definition of the platform runtime and configuration, not an individual inner work container.

Installation and execution must use the same Docker daemon. First-release acceptance covers the local system daemon and corresponding user permissions. Do not switch automatically to rootless Docker, a remote `DOCKER_HOST`, or another context; explicitly reject configurations outside the validated scope. FC templates must also verify that Docker's data-root and images survive template restoration. An installation record alone does not prove the images are present.

Python/uv and wheel inputs can be supplied locally. `ANOLISA_SKILL_SCORE_DEPENDENCIES_OFFLINE=1` disables their network fallback; image construction still requires a network-capable build host. `ANOLISA_SKILL_SCORE_OFFLINE=1` refuses a fresh image build. This source release does not claim air-gapped first installation. A prepared template preserves worker environments, build receipts and Docker data-root so restored tasks perform no installation or rebuilding.

## 7. SkillEval integration and formal-release migration

SkillEval continues to use its existing security assessment pipeline. It must not copy scoring rules, Harbor code, or a second third-party installation implementation into its repository.

| Stage | SkillEval responsibility |
| --- | --- |
| Template build | Pin ASC version and manifest; prepare Docker; explicitly install the base package and optional scoring component, which builds the verified sources and binds actual image IDs; check execution capabilities, then publish the template through existing platform processes |
| Instance execution | Inject shared model configuration and limited credentials; upload the fixed Skill; run `plan/run`; retain the existing same-path/content binding |
| Result retrieval | Download files and apply existing archive validation; consume new completion/cleanup states; show risk, uncertainty, and failure; retain evidence |
| Lifecycle | Own FC TTL, cancellation, download retries, outer cleanup, and operational observability; record inner and outer resource states separately |

SkillEval commit `145ab88d27ca35405fef4278a705d25045dc3d36` adds restricted shared-configuration transfer and private file injection alongside the legacy `ANTHROPIC_*` variables. It does not forward arbitrary host environment variables. Its template installer delegates image preparation to the optional component and retains readiness checks after template restoration. Acceptance of the released ASC candidate remains separate from these source changes.

Source download and image construction happen during template construction or updates. Each assessment reuses dependencies and images in the published template without downloading or loading them again. If template restoration loses images, fail explicitly and let the platform rebuild or repair the template; task execution must not silently install dependencies. Standalone users similarly prepare the environment during optional-component installation or updates, then reuse it for each `run`.

First publish a pinned preview of the same structure from the user's fork and pass standard-package, standalone, and FC acceptance. After merging the capability into mainline, publish through formal ASC channels, continuing to use source bundles. SkillEval changes package/archive sources, pinned version/manifest, and template version, then repeats acceptance; CLI, task/report contracts, and execution topology stay consistent. Establish formal artifact addresses, retention policies, publication permissions, and download connectivity from SkillEval build environments. Access from the current test account does not prove production access. Formal release does not require a simultaneous migration to an OCI registry.

## 8. Code changes and implementation order

| Order | Repository/location | Main work | Completion criterion |
| --- | --- | --- | --- |
| 1 | ANOLISA: ASC contracts, fixtures, design standards | Freeze commands, input digest, report/worker protocol, and the explicit local-assessment architecture contract; freeze the Python oracle and golden fixtures | Testable migration boundary and reviewable compatibility changes |
| 2 | ANOLISA: `asc-model-client`, new `asc-skill-score`, CLI | Shared Bailian access; Rust planning/evidence decisions/scoring; direct worker invocation | Differential tests with fixed model responses/evidence pass without changing unrelated scanner behavior |
| 3 | ANOLISA: `skill-score-runtime`, networking and evidence | worker-v1, complete snapshots, safe evidence collection, source redaction, failure cleanup, one compatibility sidecar | Input/evidence boundaries and failure matrix pass Linux validation |
| 4 | ANOLISA: ASC packaging, component registration and release CI; `distribution/anolisa` | Lighten the base package; publish the small optional source bundle; verify SHA256/image IDs; explicit dependency installation; initial-install/update transactions and repair; retire the old scoring Python 3.11 package | No heavy dependency downloads by default; archive installation is verifiable; optional-component updates and repair work; incompatible versions are explicitly rejected |
| 5 | SkillEval: security pipeline, `deploy/fc-security`, build scripts and docs | Shared model configuration, state consumption, explicit optional-component installation in templates, update the original PR | FC platform regression passes with the same candidate ASC artifacts |
| 6 | Both repositories: release validation and documentation | Freeze the candidate, run real samples/failure tests, record the support matrix and FPR | Evidence binds to actual commits/artifacts and defines what is ready to merge or release |

Stages 2 and 3 may run in parallel after stage 1 stabilizes contracts. Installer and release changes may also proceed in parallel. Complete deterministic compatibility checks before removing the Python release implementation; do not remove the oracle first and infer migration correctness from nondeterministic online model output.

## 9. Acceptance criteria and false-positive evaluation

### 9.1 Deterministic compatibility

Compare normalized Python/Rust plans and reports using frozen inputs, model responses, and execution evidence. Cover root-cause deduplication, severity, existence/dynamic multipliers, hygiene, Decimal half-up rounding, Critical confirmation thresholds, errors, and planning notes. Explicitly listed existing defects may be fixed; other scoring changes must not be bundled into migration. Fixed-response differential tests do not replace real model-task tests.

### 9.2 Installation and execution matrix

- Clean Ubuntu installation, upgrade, cache reuse, missing daemon, lost network, hash mismatch, insufficient disk, cancellation, and version/protocol mismatch.
- Base install/update/repair downloads no scoring dependencies or images, with or without Docker. `run` reports an absent optional component without downloading it. Failed explicit installation of the optional component leaves base ASC unaffected; its failed upgrade retains the old execution environment.
- Incompatible base/optional-component versions refuse execution; the optional component can be added after base installation; uninstalling it preserves base commands and makes dynamic run explicitly report that it is not installed.
- Old version remains usable when new-runtime preparation fails; activation failure restores it; repair restores deleted same-version dependencies; complete/incomplete offline materials; restored templates run without downloads.
- Tampered or oversized source bundles; wrong platforms; missing sidecars; mismatched build receipts; failed or interrupted builds. Failures must not write ready state or affect images used by other versions; execution never triggers registry pulls.
- Source cache hits still require digest verification, and built images still require receipt-bound identity checks. Standalone Linux and FC each record actual config IDs and any available OCI manifest IDs, even when Docker storage backends expose different `.Id` values. Legacy exports must verify config bytes and layer diffIDs; damaged OCI metadata must not fall back to legacy parsing. Check classic and containerd stores, reject unbound IDs, role swaps and wrong platforms, and execute by the resolved immutable ID. After template construction, block artifact-download sources and confirm restored instances can still run assessments with only the network access actually required by the task, proving there are no per-task downloads.
- Normal tasks, known violations, Agent refusal, model timeout, cancellation, missing/oversized/fabricated/inconsistent evidence, worker failure, cleanup failure, and isolation between concurrent tasks.
- Network allow/deny behavior, DNS and local-destination exceptions, startup races, sidecar exit, and missing capabilities; failures never open the network.
- The same ASC commit, release manifest, source-bundle SHA256, and the installation-local build receipt run public `plan/run` on native Linux and FC/SkillEval. Record the actual Docker ID selected on each host. FC additionally pins the SkillEval commit, template revision, and model configuration.

### 9.3 FPR methodology

Label benign and violating samples independently before execution. The benign set covers file reads/writes, authorized cleanup, command invocation, archives, legitimate credential use, allowed networking, and external-input parsing, including real Skills beyond demonstration fixtures. Pair violations with benign controls for similar workflows and retain a holdout not used for tuning.

Report case-level and finding-level false positives, unsupported deductions, and incorrect blocking/needs-review decisions alongside violation detection, dynamic-target activation, and confirmed effects. Report environment failures, insufficient evidence, and unknown outcomes separately. Publish total sample counts and valid-assessment counts. Do not count unfinished samples as true negatives or discard failures before reporting “0%.” Repeated runs measure randomness; they do not increase the number of independent samples. State statistical uncertainty for small samples.

Reduce false positives through correct context, redaction that preserves semantics, evidence attribution, and decision thresholds, not by lowering weights or dropping samples. Freeze the first mainline acceptance set and expected judgments: confirmed benign behavior must not gain false positives, and migration must not let existing violations pass. Set production FPR targets after expanding the sample set; six benign samples are not a sufficient basis for a production metric.

## 10. Existing evidence and remaining work

The following receipts describe historical coverage and do not validate the native implementation on this branch. New acceptance records must separately bind the actual commits, packages, and image identities:

| Version/evidence | Supported conclusion | Unsupported extrapolation |
| --- | --- | --- |
| Eight-sample FC test of ASC `d47ece18` + SkillEval `76ba36e3` | Six benign samples ultimately completed with 0/6 observed risk false positives; source risks were identified in two violating samples | Both violations were refused by the Agent; neither dynamic-effect confirmation nor proof of sandbox blocking was obtained |
| ASC `0f935544` | Artifact identity, Ubuntu installation, collector/network regressions, and one B04 FC recheck | Not a complete eight-sample run of this version, nor evidence for the future Rust implementation |
| SkillEval `6cb567a7` | Workspace check and 15 local regressions after mainline synchronization | No FC/model-task rerun on the newly merged commit |

Before and after implementation, establish addresses, publication permissions, and retention policies for formal packages and archive attachments; connectivity from SkillEval build environments to dependency/artifact sources; and the supported Linux/architecture/FC-template scope. A new OCI registry address or authorization is outside the first-release work. Extend validation from the verified Linux x86_64/Ubuntu and FC Gen2 paths. Do not assume support for arm64, other distributions, or FC Gen1.

Mainline merge requires at least input consistency, safe evidence collection, redaction, explicit states, Rust differential tests, and representative standalone/FC regression on the final candidate artifacts. Broader production FPR evaluation can continue without blocking submission of a reviewable implementation, but publication must accurately state coverage limits.

## 11. Current source locations

- [Public CLI dispatch](../../v2/apps/asc-cli/src/commands/skill_score.rs), [shared model client](../../v2/crates/asc-model-client/src/lib.rs).
- [Existing Python scoring pipeline](../../agent-sec-cli/src/agent_sec_cli/skill_score/pipeline.py), [input loading](../../agent-sec-cli/src/agent_sec_cli/skill_score/io.py), [Bailian adapter](../../agent-sec-cli/src/agent_sec_cli/skill_score/provider/bailian.py).
- [Worker dependencies](../../skill-score-runtime/pyproject.toml), [Harbor execution adapter](../../skill-score-runtime/src/agent_sec_skill_score_runtime/harbor_runtime.py), [evidence normalization](../../skill-score-runtime/src/agent_sec_skill_score_runtime/observations.py).
- [Component manifest](../../packaging/anolisa/component.toml), [installation planner](../../../../distribution/anolisa/crates/anolisa-core/src/planner.rs), [raw update operations](../../../../distribution/anolisa/crates/anolisa-cli/src/commands/tier1/install/owned_ops.rs).
- [Existing GHCR publication workflow](../../../../.github/workflows/docker-nightly.yaml), [current scoring archive publication and installation validation](../../../../.github/workflows/sec-core-skill-score.yaml).
- [Existing Skill Score user contract](../../../../docs/user-guide/en/agent-sec-core/skill-score.md), [SkillEval integration PR](https://code.alibaba-inc.com/chengshuyi.csy/skilleval/codereview/30699594).

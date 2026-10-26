# Skill safety assessment

[中文版](../../zh/agent-sec-core/skill-score.md)

The optional assessment component plans probes for one Skill, executes them in Docker, and produces an evidence-qualified safety score. Planning and judging call a supported Bailian Anthropic-compatible model endpoint and consume tokens. The preview uses the standard `sec-core` raw package and preserves its V2 host CLI and daemon.

## Install the optional component

On Ubuntu and compatible Linux x86_64 distributions, download a validated fixed fork release and check artifact sizes and SHA-256 values against its trusted manifest. Place `index-v2.toml`, `index.toml`, `components-v2.toml` and the raw package in a local `v1/` directory, then use the existing ANOLISA installer:

```sh
sudo anolisa --install-mode system install sec-core --backend raw \
  --repo 'file:///absolute/path/to/preview/v1' --version '<preview-version>'
```

Use the version from a successful `sec-core` preview release. The official installer appends `/v1` when the repository root does not end with it, so the flat GitHub Release asset URL cannot be used directly as the repository root. The SkillEval bundle script prepares this local v1 layout automatically. The official default channel does not yet contain this optional assessment. The standard raw package contains the V2 binaries and an isolated scoring CLI/worker with pinned Python dependencies. The public entry point is `/usr/local/bin/agent-sec-cli skillsec score plan/run`. The Rust CLI directly replaces its process with the private Python scoring runtime; the scoring logic is unchanged. The old preview launcher `agent-sec-skill-score` is not installed. Private scoring runtimes reside under `/usr/local/lib/anolisa/sec-core/skill-score/{cli,runtime}`. No additional scoring daemon is required. Match the target CPU and glibc baseline before installing.

The same release provides `release-manifest.json` (`agent-sec-skill-score/release-v2`), the raw repository indexes, `skill-score-images.tar.gz`, `images.json`, tool inventory and source archive. Verify all lengths and SHA-256 values against a manifest obtained from the trusted release channel. The manifest must declare `cli: "agent-sec-cli"` and `command_prefix: ["skillsec", "score"]`; consumers reject older preview command contracts. Container images are separate from the raw package. During a clean template build, load the matching archive with `docker load -i skill-score-images.tar.gz` and verify image IDs against `images.json`.

Both the work image and the ASC egress sidecar are required; runtime startup does not build or pull missing images. A Linux x86_64 host, Docker Compose, usable network namespaces, the packaged firewall/conntrack rules, and ptrace/strace support are required. Reserve memory outside the inner container's 4096 MiB allocation for Docker, the CLI, worker and sidecar. An image build alone is not runtime acceptance.

## Plan and execute

Provide `ANTHROPIC_API_KEY` and, when needed, `ANTHROPIC_BASE_URL` through the process environment or your platform's secret injection. Do not put credentials into command arguments, task JSON, templates or release manifests.

```sh
agent-sec-cli skillsec score plan \
  /work/input/skill --output-file /work/output/task.json

agent-sec-cli skillsec score run \
  /work/output/task.json --output-file /work/output/report.json \
  --evidence-dir /work/output/evidence
```

`plan` accepts one directory containing `SKILL.md`. Its task uses `agent-sec-skill-score/task-v1`, records the resolved Skill path and content hash, and carries the audit and probes. Keep the exact content and absolute path available to `run`. `run` rejects content drift before invoking the worker. A task can also be read from stdin with `run -`.

Output files must not already exist. Evidence directories must be absent or empty. Outputs must be outside the evaluated Skill, and may not conflict with internal evidence paths. Prefer new output directories for every attempt. Standard output also contains the public JSON; file transfer is recommended when a platform limits command output.

Scoring runs locally and does not use the daemon. Explicit `--socket`, `--timeout-ms`, `--otel-context` and `--trace-context` options are rejected for scoring. The caller controls the evaluation deadline and cancellation. `skill-ledger` remains a compatibility alias for `skillsec`; a package without the optional scoring payload reports an installation error when executing a scoring stage.

## Interpret results

| Exit | Meaning |
|---|---|
| `0` | The requested stage completed without a reported execution/persistence failure. Validate the report's coverage as well. |
| `1` | Dependency, execution, model or persistence failure. A partial report may exist and should be retained. |
| `2` | Invalid input or contract, including content drift. |

A public report uses `agent-sec-skill-score/report-v1` and binds `content_hash` to the task. `score` and `decision` describe the safety assessment; they do not establish dynamic completeness. Verify that `execution` accounts for every planned probe and each entry is `completed: true`; nonempty `limitations` means incomplete coverage. Missing evidence or an unsupported environment must remain visible. A high score with incomplete execution is not a passed dynamic assessment.

The optional `planning_notes` string array preserves non-blocking caveats from the task's `audit.limitations`, such as an absent category of input. Display these as unverified planning context; they neither establish nor prevent dynamic completion. Actual planning errors, unavailable environments, timeouts, missing observations and failed adjudication remain incomplete conditions. `planning_notes` is also retained in `internal-report.json`; the task format is unchanged.

Retain the public task/report and evidence directory together, including `environment.json`, `internal-report.json` and `evidence-catalog.json`. Collect partial evidence before deleting a failed sandbox. Evidence may contain sensitive Skill content; the platform controls access and retention.

## Runtime ownership

Harbor 0.22.0 is consumed without editing its source. ASC supplies a task Compose definition and its own sidecar image through the public environment API. The sidecar enforces a static model-host TCP allowlist; local destinations and ICMP remain direct, and other traffic is rejected. This is not a general-purpose domain firewall or Harbor phase-dependent policy implementation.

The sidecar's read-only policy holds the allowlist and resolved IPv4 addresses. GOST uses that mapping and an explicit empty fallback resolver, so changes to the work container's shared `/etc/hosts` cannot redirect the proxy. A differently cased TLS name that misses the mapping fails closed. Proxy failure leaves filtering rules installed; startup failure prevents the work container from starting. IPv6 acceptance requires a separate IPv6-enabled test environment.

A deployment platform creates the outer sandbox, uploads an immutable Skill snapshot, invokes these commands, downloads bounded artifacts and deletes the exact sandbox. ASC does not receive cloud credentials or implement FC lifecycle APIs. For FC, install the standard raw package, Docker and the images inside the FC template; SkillEval only orchestrates them. Artifact downloads and image loading occur during template construction, not for every evaluation.

## Build and verify

Developers build on Linux x86_64 with Rust 1.93.1, uv, native compiler dependencies, Docker and Compose. The `CI / Skill score raw preview` workflow builds and installs the complete raw package on Ubuntu, checks the installed CLI/worker and Docker network behavior, and publishes a fork prerelease only after those checks pass. The workflow does not publish an FC template or prove real-model scoring acceptance.

The preview build uses `packaging/skill-score/build-payload.sh` and `RAW_SKILL_SCORE_PAYLOAD` to opt into assessment content in the standard `make package-raw` build. `RAW_PREVIEW_VERSION` sets the staged preview version without changing the source product version. The default raw build remains V2-only. Preview releases require a clean source commit and include fixed artifact hashes; `build-rpms.sh` remains a separate maintainer path for RPM delivery.

Run the Linux egress regression against the installed worker and freshly built images:

```sh
PYTHONPATH=src/agent-sec-core/agent-sec-cli/src:src/agent-sec-core/skill-score-runtime/src \
python src/agent-sec-core/tests/integration-test/skill-score/validate_egress.py \
  --output /absolute/new/egress-validation
```

This network regression does not call a real model and does not replace full `plan → run → report` acceptance with normal, violating, environment-failure and timeout samples.

If the builder cannot reach package sources through its Docker bridge, explicitly set `ASC_IMAGE_BUILD_NETWORK=host` for the build command. This changes only trusted Docker build steps; assessment containers retain their independent network policy.

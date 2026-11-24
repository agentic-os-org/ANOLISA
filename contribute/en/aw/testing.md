# Native and release acceptance

[中文版](../../zh/aw/testing.md)

[native-matrix.json](../../../src/aw/tests/native-matrix.json) records pinned versions, dependencies, commands, owners and report contracts.
Start daily development with the [AW gate](../../../src/aw/CONTRIBUTING.md). Use this page for releases and native seam changes.

## Select validation scope

| Layer | Coverage | Entrypoint |
| --- | --- | --- |
| Fixture gate | Protocol, failures, security/compression composition and lifecycle | `python3 src/aw/scripts/check.py` |
| Native SDK / CLI | Real framework wiring and effect adoption | `native_acceptance.py`, selected by Agent |
| Real backend compatibility | Four Tokenless/RTK and AW Provider contracts | `check_tokenless.py` |
| Release integration | Packages, backends, native manifest and resident containers | AW Integration workflow |

For a failed nightly run, open **AW / Integration result** in the run summary.
Find the first failed case in its Agent row, then open that Agent's evidence
artifact for the command log. A missing report points to the producer job.
Use **Re-run failed jobs** for a transient failure while the build artifacts
are available. If an intermediate artifact has expired, rerun all jobs for
that commit.

OpenClaw automation covers the official loader/Hook runner; validate complete Gateway model flow separately.
Qoder SDK hosted MCP still requires manual container evidence. Report the actual boundary recorded in the manifest.

## Qoder execution-asset coverage

The `qoder-sdk` manifest target includes these required suites:

| Suite | Acceptance boundary |
| --- | --- |
| `resident-parser` | Pinned SDK parser: approval before wrapper publication, native-mode rejection, unused-ticket release and callback/consumer ownership |
| `native-command` | Official Worker: daemon/direct execution, logical permission/Post input, application veto, command-specific deny in default/acceptEdits, timeout-background EOF and close/settlement ordering |
| `declarative-profile` | Installed Bridge: native settings acknowledgement before prompt release, declarative snapshot visibility, daemon reuse and Query cleanup |

The fixture gate also covers bounded asset reads, invalid declarations, snapshot
cleanup and cache invalidation. Hosted suites need runtime-only test credentials.
Use the target runner below; preserve each suite's report and verify that native
processes and AW scopes have retired. Local parser transport alone does not
establish native Worker authorization behavior.

## User submission acceptance

The Qoder SDK and QwenPaw targets include `input-submission`,
`input-submission-backend-error` and `input-submission-timeout-block`. They use
the candidate AW/Provider and real V2 CLI/daemon in fast mode. The normal suite
checks prompt and user PII denial before model requests, clean input, recovery
after a rejected turn and preserved native configuration. Qoder also checks
native submit refusal. QwenPaw inspects actual model requests; Qoder uses the
native zero-turn/API-time result and absence of assistant messages.

Run from `src/aw` with the four candidate binaries in `target/native-binaries`:

```bash
python3 crates/aw-cli/tests/fixtures/sec_core/input_submission.py \
  --agent qwenpaw --bin target/native-binaries \
  --agent-bin "$(command -v qwenpaw)" --image ubuntu:24.04 \
  --output target/input-submission-evidence
```

For Qoder use `--agent qoder-sdk --npm /absolute/path/to/private-sdk-install`
and the runtime `QODER_PERSONAL_ACCESS_TOKEN`. Add `--fault backend-error` or
`--fault timeout-block` for isolated backend failures. Use a new output directory
for each run. The runner retains results, native messages/model requests and
cleanup reports, and removes its runtime directory and daemon container. Pinned
parser/middleware suites separately cover callback cancellation and cleanup.

## Tool PII acceptance

Both `qoder-sdk` and `qwenpaw` targets include `tool-pii`, `tool-pii-backend-error`,
`tool-pii-timeout-block` and `tool-pii-cancel`. They use the candidate AW/Provider
and real sec-core CLI/daemon. The baseline covers normal execution, input refusal
without a marker, actual output replacement, existing code denial, native errors
and preserved native settings. QwenPaw checks SSE and the next model request;
Qoder checks adopted `tool_result` and the subsequent assistant messages.
Fault suites withhold results; cancellation occurs after the output scan starts.

Run from `src/aw` with the four binaries in `target/native-binaries`, a private
SDK installation and the stock App dependencies. For an isolated QwenPaw check:

```bash
python3 crates/aw-cli/tests/fixtures/sec_core/tool_pii.py \
  --agent qwenpaw --bin target/native-binaries \
  --agent-bin "$(command -v qwenpaw)" --image ubuntu:24.04 \
  --output target/tool-pii-evidence
```

For Qoder SDK use `--agent qoder-sdk --npm /absolute/path/to/private-sdk-install`
with the runtime PAT in `QODER_PERSONAL_ACCESS_TOKEN`. Add `--fault backend-error`,
`--fault timeout-block` or `--fault cancel` for a separate output-failure run.
Each output directory must be new. `results.json`, `cleanup.json`, backend/process
records and native messages/model requests are retained; runtime directories and
the owned daemon container are removed. Multi-block/media, managed MCP and
background PII result delivery require separate native acceptance.

## One native target

Use a clean, committed checkout with manifest-required Python, Node 24.16.0, uv 0.11.13 and SDK dependencies.
`target/native-binaries` must contain nightly bundles and checksums for the same candidate SHA. Qoder and QwenPaw require Docker; Qoder hosted models need a PAT during execution.
Run from `src/aw`; the output directory must not already exist.

```bash
python3 -B scripts/check_native.py --candidate-sha "$(git rev-parse HEAD)"
python3 -B scripts/native_acceptance.py --agent agentscope \
  --candidate-sha "$(git rev-parse HEAD)" --bin target/native-binaries \
  --output target/native-evidence/agentscope
```

Replace `--agent` with a manifest target. Resolve failures and missing evidence; do not omit cases to report success.

## Integration and resident containers

Full mainline acceptance uses a 40-character candidate SHA already merged to main. Forks can run credential-free mode, retaining outstanding Qoder hosted-model checks.

```bash
gh workflow run aw-integration.yml --ref main -f candidate_sha=<40-character-SHA>
gh workflow run aw-ci.yml --repo <owner>/anolisa --ref <branch> \
  -f integration=true -f credential_free=true
```

Local resident containers reuse candidate bundles; image inputs must be committed. Tests use a regular user, synthetic data and loopback fixtures, with no external network, credentials or host mounts.

```bash
python3 -B scripts/resident_container.py --candidate-sha "$(git rev-parse HEAD)" \
  --bin target/native-binaries --output target/resident-evidence
```

## Evidence and cleanup

Reports include candidate/tested SHAs, package digests, SDK versions, commands, deadlines, per-case results and cleanup outcomes.
`unittest`/`tap` verify test counts; JSON verifies named cases or actual completed counts. Exit 0 also requires a complete report.
Allow and denial cases check native results, tool markers and AW audit; failure and timeout cases verify blocking without delayed execution.

Runners reclaim owned processes, temporary profiles, sockets, containers and unique image tags. On Docker timeout or cleanup failure, inspect and remove exact resources using commands in the report.
After evidence review, remove this run's `target/native-evidence/agentscope` and `target/resident-evidence`; retain shared build caches.
Validate production endpoints, real load and deployment shutdown in the actual deployment environment.

For input-submission cancellation, add `--fault cancel` to either runner command.
The test pauses the real daemon, observes the active scanner process, closes the
native Query or stops the App turn, and verifies a cancelled service record,
scanner exit and zero model requests. The nightly matrix requires both reports.

## Application-owned SDK package

The default gate runs the shared protocol consumer, Node lifecycle/transport tests,
and an offline out-of-tree npm install. Build the core and Qoder tarballs from the repository root:

```bash
mkdir -p src/aw/target/sdk-package
npm pack ./src/aw/sdk/typescript --pack-destination src/aw/target/sdk-package
npm pack ./src/aw/sdk/typescript-adapters/qoder --pack-destination src/aw/target/sdk-package
```

For real Qoder acceptance, install both tarballs and the selected native Qoder SDK
in a separate application directory. Copy `src/aw/sdk/tests/native-qoder.mjs` and
`src/aw/sdk/tests/fixtures/sdk-deny.py` there. Set `AW_TEST_BINARY` to the absolute
candidate executable and provide native `QODER_PERSONAL_ACCESS_TOKEN` through the
existing credential store. Run `timeout --kill-after=10s 300s node native-qoder.mjs`.
It creates a private temporary service/work directory and tests online operation,
service loss, recovery, AW denial and native denial with actual Bash side effects.
The application retains daemon/Query cleanup. Credentialed acceptance is separate
from the required offline package gate.

The default gate also builds the Python wheels offline and tests installed consumers.
For a real QwenPaw App, install the matching `anolisa-aw-sdk` and
`anolisa-aw-sdk-qwenpaw` wheels into the same directory outside the checkout.
Set `AW_SDK_PYTHONPATH` to that directory, `AW_TEST_BINARY` and `AW_QWENPAW_BINARY`
to the candidate executables, and `AW_TEST_OUTPUT` to a private evidence directory. Run
`timeout --kill-after=10s 180s python3 src/aw/sdk/tests/native_qwenpaw.py` from the
repository root. It uses the stock App, a controlled model and native observer
plugin, and verifies AW loss while a tool is entering, recovery and denial.

Both package gates exercise CLI discovery and independent same-PID instances,
busy detach, Service restart and expired Ready confirmation. The Service unit
suite also checks forged owner identity, conflicting requests and release proof.

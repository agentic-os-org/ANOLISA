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
gh workflow run aw-preview.yml --ref main -f candidate_sha=<40-character-SHA>
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

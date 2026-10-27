# Standalone Skill assessment delivery

[中文版](skill-score-runtime_zh.md)

> Historical Python preview design, retained as the migration oracle baseline. Native Rust scoring and the separate optional installation supersede the delivery described here; see the [native design](skill-score-native-design.md) and [current user guide](../../../../docs/user-guide/en/agent-sec-core/skill-score.md).

This change adds the standalone score source to ANOLISA baseline `5351c892f2e89d6be799ebdbeea948c173d3d7c2`. The import is bound to the source attachment in `packaging/skill-score/source-import.json`; a release tag's base commit is not treated as the complete source. Runtime image and package acceptance is separate from unit testing.

The assessment CLI owns planning, source hash validation, evidence interpretation and report versioning. The Python 3.12 worker owns the existing Docker/Harbor execution. The preview standard raw package adds private runtimes under `/usr/local/lib/anolisa/sec-core/skill-score/{cli,runtime}` and exposes `agent-sec-cli skillsec score plan/run` in the V2 CLI. Its explicit local dispatch uses Unix exec before observability initialization, retaining the PID, process group, standard streams and Python exit status. It does not connect to the daemon or inherit its request timeout. The existing `skill-ledger` spelling remains an alias for `skillsec`. The CLI uses the current native prompt backend and rejects scanner error/degraded/truncated outcomes instead of treating them as clean scans.

Harbor 0.22.0 remains unmodified. The worker writes the task's public `environment/docker-compose.yaml`, enables Harbor public network mode, and records ASC's effective static network policy separately. Main shares the ASC sidecar network namespace and waits for health; both services have explicit health checks. The main container drops NET_ADMIN and NET_RAW.

The sidecar image and firewall scripts belong to ASC. The rules avoid a native nft FIB expression by using the packaged iptables match path, preserve local-destination and ICMP behavior, proxy TCP, and reject other outbound traffic. The sidecar does not flush enforcement rules when GOST exits. Missing policy or failed startup prevents main startup.

A per-run read-only GOST configuration contains inline allowlist entries and fixed hostname-to-IPv4 mappings. An explicit resolver with no nameservers prevents fallback to the shared Docker hosts file. Unknown or differently cased names fail closed rather than using an attacker-modified resolution source. This delivery implements one static policy for the entire task; it does not expose Harbor phase-dependent network policy switching.

The deployment platform owns sandbox creation, TTL, upload/download, cancellation, retry, credentials and release/rollback. It restores the same Skill content at the same absolute path for plan and run, and downloads files rather than interpreting truncated stdout. No FC SDK, template ID or cloud credential enters the ASC package.

The contract uses `agent-sec-skill-score/task-v1` and `agent-sec-skill-score/report-v1`. A content mismatch rejects run before worker invocation. A report may exist after a nonzero exit; platform completion requires separately checking exit state, content binding, every planned probe and evidence coverage. A high score does not imply successful dynamic execution.

Report-v1 carries optional `planning_notes: string[]` copied from task `audit.limitations`. These unverified, non-blocking planning caveats are retained in the public report and `internal-report.json` without becoming dynamic `limitations`. The task-v1 input remains unchanged. Actual planning errors and execution, observation or adjudication failures retain their existing limitations and failure handling. Consumers display planning notes separately and continue to reject incomplete coverage regardless of score; they must not filter limitation text to infer success.

The preview build creates a standard `sec-core` raw package and repository indexes, a separate compressed image archive, source archive and release-v2 manifest. It uses the existing ANOLISA installer with a local `v1/` repository assembled from the verified flat release assets. The separate RPM maintainer path remains available. Build output never rewrites source inventory files. The optional build-network override affects Docker build steps only. Publishing requires a clean source revision; dirty development artifacts are explicitly marked.

Validation has three distinct layers: portable contract/configuration tests; Linux native-package and network regression; real model/observer/scoring end-to-end acceptance. A successful earlier prototype is not evidence that a new package or deployment has passed the latter two layers. See the [user contract](../../../../docs/user-guide/en/agent-sec-core/skill-score.md) for invocation and result semantics.

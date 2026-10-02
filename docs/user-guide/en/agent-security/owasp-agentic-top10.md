# OWASP Agentic Top 10 Security Control Mapping

[中文版](../../zh/agent-security/owasp-agentic-top10.md)

ANOLISA maps all ten OWASP Agentic Top 10 risk categories to security controls
across its components. Use this reference to identify the controls available for
your Agent deployment, the paths that enforce them, and the remaining gaps.

## Scope and assessment rules

- **Framework:** [OWASP Top 10 for Agentic Applications 2026](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/).
- **Source baseline:** ANOLISA commit [`3f4b22e3d645`](https://github.com/alibaba/anolisa/commit/3f4b22e3d645057270155d905964085ddcf58d5a).
  All implementation and test links below are pinned to this commit.
- **Scope:** the combined, implemented capabilities of ANOLISA components,
  including AgentSecCore, cosh-ng/cosh-gateway, copilot-shell, agent-memory,
  AgentSight, the ANOLISA service, and SkillFS. Each rating applies to the listed
  integration paths and configuration; installing one component does not enable
  the entire mapping. Linux-only controls require a supported Linux deployment.
- **Full:** the primary mitigation controls are implemented within the specified
  component, integration, and configuration scope.
- **Partial:** relevant controls exist, but the primary control chain has known gaps.

These are ANOLISA's source-based architectural self-assessment ratings, not OWASP
certification or a guarantee for every deployment or attack. **Mapped** means
that each risk category has an identified control mapping; it does not mean that
every OWASP mitigation recommendation is fully implemented.

**Evidence status:** implementation and existing test cases were inspected for
this mapping. Referenced tests are repository evidence, not tests executed by
this documentation change. Runtime security tests and adversarial deployment
validation were not run; deployment-specific enforcement remains to be verified
on the selected Host and platform. Documentation validation is recorded in the PR.

## Coverage summary

**7 Full / 3 Partial** across ten risk categories.

| Risk category | Rating | Primary components and controls |
|---|---|---|
| ASI01 Agent Goal Hijack | Full | AgentSecCore Prompt Scanner and blocking hooks at integrated inputs |
| ASI02 Tool Misuse and Exploitation | Full | cosh-ng tool approval, hook decisions, and governed execution |
| ASI03 Identity and Privilege Abuse | Full | cosh-gateway caller, Task, Run, target binding, and execution authorization |
| ASI04 Agentic Supply Chain Vulnerabilities | Partial | Skill Ledger, Skill signature verification, artifact checksums, and selected release SBOMs |
| ASI05 Unexpected Code Execution | Full | AgentSecCore Code Scanner and Linux sandbox |
| ASI06 Memory and Context Poisoning | Full | agent-memory injection filtering, optional scoping, and retrieval risk markers |
| ASI07 Insecure Inter-Agent Communication | Partial | Local caller authentication, ACP session binding, and SkillFS channel authentication |
| ASI08 Cascading Failures | Full | copilot-shell loop termination and budgets, bounded retries, ANOLISA rate limits, and SkillFS restart limits |
| ASI09 Human-Agent Trust Exploitation | Full | Human approval and risk presentation in supported Hosts |
| ASI10 Rogue Agents | Partial | AgentSight credential-exfiltration monitoring, event correlation, and audit |

## Control mapping

### ASI01 — Agent Goal Hijack · Full

- **Risk:** untrusted instructions redirect an Agent away from the user's goal.
- **Implemented controls:** AgentSecCore scans prompt input and uses supported
  Host hooks to reject detected injection before the model call.
- **Conditions:** the scanner must be available and the blocking hook enabled.
  For example, Codex requires `PROMPT_SCANNER_MODE=deny`; OpenClaw requires
  `promptScanBlock=true` to block a `deny` verdict.
- **Implementation and verification evidence:** the [Codex prompt hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/codex-plugin/hooks-plugin/hooks/prompt_scanner_hook.py#L96)
  blocks `warn` and `deny` in deny mode; the [OpenClaw hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/openclaw-plugin/src/capabilities/prompt-scan.ts#L76)
  gates dispatch on its configured blocking mode. Existing [Codex hook tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/tests/unit-test/codex_hooks/test_prompt_scanner_hook.py#L220)
  cover observe/deny behavior and scanner-error fallback.
- **Remaining limits:** these examples scan the current user input, not every
  tool result, memory, or retrieved document. Observe mode does not block;
  scanner errors and unavailable CLI paths can fail open. Detection is heuristic
  and does not establish universal resistance to goal hijacking.

### ASI02 — Tool Misuse and Exploitation · Full

- **Risk:** an Agent invokes tools outside the intended operation or approval scope.
- **Implemented controls:** cosh-ng mediates supported tool requests through
  approval and policy handling, preserves hook rejections, and keeps hook `ask`
  decisions and unknown provider tools pending even in Trust mode.
- **Conditions:** the Host/provider must expose the supported approval or governed
  execution path. Configure the corresponding hook and approval policy for the
  tools whose execution requires intervention.
- **Implementation and verification evidence:** the [approval bridge](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge.rs#L19)
  checks hook decisions, known tool identities, and shell policy before execution.
  Existing [approval bridge tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge_tests.rs#L707)
  cover blocked/high-risk requests and unknown tools remaining pending.
- **Remaining limits:** approval coverage depends on provider integration. Trust
  can approve eligible known tools automatically, and tool calls outside these
  paths do not acquire approval enforcement from the presence of a hook alone.

### ASI03 — Identity and Privilege Abuse · Full

- **Risk:** a caller reuses another Agent's authority, approval, or execution context.
- **Implemented controls:** cosh-gateway derives local caller identity from peer
  credentials, binds brokered requests to the active Task/Run/target, and validates
  an exact, unexpired execution permit before atomically consuming it.
- **Conditions:** requests must traverse the gateway's authenticated socket and
  implemented brokered execution path, with a runtime and driver that support it.
  Caller identity is local to the installation and operating-system account.
- **Implementation and verification evidence:** [socket admission](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/server.rs#L85),
  [scheduler admission](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/scheduler/brokered/execution.rs#L27),
  and [permit consumption](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/storage/ledger/execution.rs#L3)
  validate the actor, Task, Run, target, runtime fence, input, and expiry.
  Existing [brokered scheduler tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/scheduler/brokered/tests.rs#L1101)
  cover fabricated success rejection, durable denial, and expiry.
- **Remaining limits:** the [Core/Codex launch capability projection](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/protocol/launch.rs#L144)
  advertises delegated local authority and `gateway_brokered_effects=false`.
  This rating does not assert that every native tool effect is gateway-brokered
  or that local account identity provides federated per-Agent identity.

### ASI04 — Agentic Supply Chain Vulnerabilities · Partial

- **Risk:** compromised Skills, dependencies, or release artifacts enter an Agent's
  trusted execution environment.
- **Implemented controls:** Skill Ledger authenticates its manifest before checking
  file drift and scan status; asset verification checks signed Skill manifests and
  files. ANOLISA verifies raw artifact checksums. The cosh-ng and tokenless prebuilt
  release paths generate SBOMs and validate their artifact bundles.
- **Conditions:** use trusted signing keys, managed Skills, the applicable admission
  policy, and the release/install paths that perform these checks. A recorded scan
  result or an SBOM alone does not reject a malicious dependency.
- **Implementation and verification evidence:** [Skill Ledger checks](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/skill_ledger/core/checker.py#L137),
  [Skill signature and file verification](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/asset_verify/verifier.py#L295),
  [raw artifact download verification](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-cli/src/commands/tier1/install/raw.rs#L495),
  and the [cosh-ng](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/.github/actions/build-cosh-ng-prebuilt/build.sh#L200)
  / [tokenless](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/.github/actions/build-tokenless-prebuilt/build.sh#L263)
  release checks implement these controls. Existing [asset verification backend tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/tests/unit-test/security_middleware/backends/test_asset_verify_backend.py#L25)
  exercise success, failure, skipped discovery, and exception handling with mocks.
- **Remaining limits:** [distribution signature fields](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-core/src/distribution.rs#L18)
  are metadata; the inspected [raw index fetch](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-cli/src/commands/tier1/install/raw.rs#L91)
  and artifact install path do not enforce publisher signature verification.
  The reviewed release paths also do not establish a complete dependency
  vulnerability detection and remediation gate. See [Known gaps](#known-gaps).

### ASI05 — Unexpected Code Execution · Full

- **Risk:** generated or supplied code executes with unintended host access.
- **Implemented controls:** AgentSecCore scans code at integrated pre-tool hooks;
  its Linux sandbox restricts filesystem and process visibility and, according to
  policy, network access using bubblewrap namespaces and seccomp.
- **Conditions:** enable blocking on the code-scanning Host hook and route execution
  through the sandbox on Linux with a restrictive filesystem/network policy.
- **Implementation and verification evidence:** the [Hermes code hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/hermes-plugin/src/capabilities/code_scan.py#L29)
  supports blocking `warn`/`deny`; [sandbox argument construction](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/linux-sandbox/src/bwrap_args.rs#L96)
  and [seccomp setup](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/linux-sandbox/src/seccomp.rs#L24)
  apply execution restrictions. Existing [Linux sandbox tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/linux-sandbox/tests/suite/bwrap_seccomp.rs#L94)
  cover reads, denied writes, writable roots, and network-related restrictions.
- **Remaining limits:** observe mode and scanner failures may allow execution.
  Full filesystem plus full network access bypasses bubblewrap isolation in the
  cited path. Code scanning is heuristic; sandbox effectiveness depends on the
  selected policy and Linux facilities.

### ASI06 — Memory and Context Poisoning · Full

- **Risk:** poisoned stored content is later treated as trusted instructions or facts.
- **Implemented controls:** agent-memory filters injection-like facts during
  heuristic consolidation, supports scoped retrieval, and marks suspicious
  keyword-search snippets for the consuming adapter.
- **Conditions:** use the filtering consolidation path and scoped `memory_search`;
  set a valid `agent_scope` and trusted `MCP_CLIENT_NAME` when using configured
  isolation. The consumer must act on retrieval risk markers and control scope
  overrides and direct file access.
- **Implementation and verification evidence:** [fact filtering](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/consolidation/heuristics.rs#L102),
  [search scope selection](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/tools/memory_search.rs#L41),
  and [scoped keyword retrieval](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/index/store.rs#L300)
  implement these paths. Existing [injection-pattern tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/safety.rs#L167)
  and [scope tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-memory/src/index/store.rs#L1903) cover their local contracts.
- **Remaining limits:** shared scope is the default; a missing identity or invalid
  configured scope falls back to shared retrieval with a warning. Direct memory
  writes are not universally injection-filtered, and a `suspicious` marker is not
  a blocking decision. These controls do not authenticate arbitrary memory writers
  or guarantee the truth of stored facts.

### ASI07 — Insecure Inter-Agent Communication · Partial

- **Risk:** forged, substituted, or misbound messages are accepted from an untrusted peer.
- **Implemented controls:** cosh-gateway authenticates local callers and binds ACP
  permission requests to a session and active execution context. Skill Ledger and
  SkillFS authenticate private socket channels with shared-secret proofs bound to
  session nonces, direction, and payload.
- **Conditions:** use the authenticated local socket/ACP paths and correctly
  provision both SkillFS channel endpoints and protected key files.
- **Implementation and verification evidence:** [gateway peer credentials](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/daemon/server.rs#L85),
  [ACP permission binding](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-gateway/src/runtime/acp_port/permission.rs#L1),
  and [SkillFS channel proofs](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/agent-sec-cli/src/agent_sec_cli/skill_ledger/skillfs_peer_auth.py#L95)
  enforce local channel checks. Existing [peer authentication tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/tests/unit-test/skill_ledger/test_skillfs_peer_auth.py#L134)
  cover wrong secrets, tampering, and wrong direction; the checked-in
  [container peer-auth probe](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/skillfs/scripts/container-peer-auth-probe.py#L275)
  includes a stale-proof replay case, not a result from this documentation change.
- **Remaining limits:** these controls protect Host/runtime and component channels.
  The inspected paths do not provide a general Agent-to-Agent trust layer that
  verifies peer identity and delegated capabilities across independently operated
  Agents. Local channel authentication alone does not close that gap.

### ASI08 — Cascading Failures · Full

- **Risk:** repeated calls, runaway delegation, retries, or restart loops amplify a failure.
- **Implemented controls:** copilot-shell terminates detected loops, limits turns,
  and checks subagent time budgets; retries have finite attempts and backoff.
  ANOLISA's helper service limits requests per UID, and SkillFS stops restarting
  a managed worker after repeated fast failures.
- **Conditions:** keep copilot-shell loop detection enabled, configure applicable
  session/subagent budgets, and use the bounded retry, helper, and managed SkillFS
  paths. Limits act at their owning component's boundary.
- **Implementation and verification evidence:** [turn and loop termination](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/core/client.ts#L660),
  [subagent budgets](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/subagents/subagent.ts#L345),
  [bounded retries](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/utils/retry.ts#L98),
  [helper rate limits](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/anolisa/crates/anolisa-core/src/daemon_server.rs#L252),
  and [SkillFS restart termination](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/skillfs/crates/skillfs-cli/src/managed.rs#L734)
  enforce these limits. Existing [loop detection tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/services/loopDetectionService.test.ts#L71)
  and [retry tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/copilot-shell/packages/core/src/utils/retry.test.ts)
  exercise detector thresholds and retry behavior.
- **Remaining limits:** subagent time budgets are checked at turn boundaries and
  after model streaming; exhausting the budget does not immediately interrupt an
  in-flight model or tool call. These are component-local controls, not a global
  circuit breaker for every external service or Agent network. Warning-only paths
  are not counted as termination. Stopping a loop does not compensate already
  completed external operations.

### ASI09 — Human-Agent Trust Exploitation · Full

- **Risk:** users approve harmful actions because an Agent's presentation obscures
  their actual effects or encourages excessive trust.
- **Implemented controls:** supported Hosts provide explicit approval decisions
  with tool/command previews and risk information. In Trust mode, cosh-ng retains
  hook-required approval and interactive review for blocked execution assessments
  or High assessments involving system control or an unresolvable launcher chain.
  Other High-risk requests can still be automatically approved in Trust mode.
- **Conditions:** use a Host/provider with native approval support and policies
  that require review for the relevant action; users must inspect the presented
  operation before approving it.
- **Implementation and verification evidence:** [approval details](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/approval/cards.rs#L187)
  expose the request, preview, risk, and assessment; the [approval bridge](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge.rs#L44)
  preserves explicit review paths, with the [assessment predicate](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge.rs#L138)
  defining which command assessments require interactive approval. Existing
  [Trust-mode approval tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/cosh-ng/crates/cosh-shell/src/agent/approval_bridge_tests.rs#L802)
  check the boundary: `reboot` remains pending while a High shell-syntax request
  is automatically approved.
- **Remaining limits:** human approval cannot guarantee a correct judgment. An
  adapter's `ask` setting is not proof of native approval: the [Codex Skill Ledger hook](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agent-sec-core/codex-plugin/hooks-plugin/hooks/skill_ledger_hook.py#L444)
  falls back to a warning where approval is unsupported. That fallback is not
  counted as human confirmation.

### ASI10 — Rogue Agents · Partial

- **Risk:** an Agent exhibits dangerous behavior that requires detection and intervention.
- **Implemented controls:** AgentSight monitors supported credential-exfiltration
  behavior, correlates security events into cases, and persists audit evidence.
  A containment request workflow records process identity, source policy, and
  pending intent, but does not establish an effective restriction in the current
  real backend.
- **Conditions:** deploy the supported Linux monitoring stack with a valid Audit
  policy, matching credential sources and destination scope, and working event
  collection and audit persistence.
- **Implementation and verification evidence:** [credential event processing](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L860)
  and [audit ingestion and correlation](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-audit/src/service.rs#L211)
  implement monitoring and evidence handling. The [containment request workflow](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/src/security/containment.rs#L226)
  exists, but the real backend [rejects credential Enforce mode](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L784)
  and [does not support live policy handoff](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L496).
  Existing [containment adapter tests](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/src/security/containment_adapter_tests.rs#L269)
  use a test enforcer to check stale acknowledgements and audit-binding retention;
  the [backend handoff test](https://github.com/alibaba/anolisa/blob/3f4b22e3d645057270155d905964085ddcf58d5a/src/agentsight/crates/agentsight-enforcer/src/actplane.rs#L1664)
  checks the unsupported-handoff result. These tests do not prove runtime blocking.
- **Remaining limits:** credential blocking and live policy handoff are missing
  from this containment path at the source baseline. Pending requests and finite
  duration fields therefore do not count as active or temporary restrictions.
  Detection and audit do not provide general goal-drift detection or automatic
  quarantine of every rogue Agent.

## Known gaps

| Category | Existing protection | Why it remains Partial |
|---|---|---|
| ASI04 | Signed Skill checks, artifact hashes, and selected release SBOMs | Publisher signature metadata is not enforced by the inspected raw install path; dependency vulnerability detection and remediation are not established as a complete gate in the reviewed release paths. |
| ASI07 | Local peer credentials, ACP session/context checks, and SkillFS authenticated channels | These local channels do not establish a general trust mechanism for independently operated Agent peers and their delegated capabilities. |
| ASI10 | Credential-exfiltration monitoring, event correlation, and audit | The real backend rejects credential Enforce mode and does not support live policy handoff; the containment request workflow does not establish effective blocking. |

Closing any gap requires an implemented enforcement path and evidence of its
integration. Metadata fields, transport connectivity, or a planned mechanism do
not change the rating by themselves.

## References

- [OWASP Top 10 for Agentic Applications 2026](https://genai.owasp.org/resource/owasp-top-10-for-agentic-applications-for-2026/)
- [ANOLISA user guide](../README.md)
- [AgentSecCore quick start](agent-sec-core/QUICKSTART.md)
- [Prompt Scanner](agent-sec-core/prompt-scanner.md)
- [Code Scanner hook configuration](agent-sec-core/code-scanner.md)
- [Skill Ledger](agent-sec-core/skill-ledger.md)
- [Asset verification](agent-sec-core/asset-verification.md)

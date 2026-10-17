# sec-core health diagnostic boundary

[中文版](sec-core-health_zh.md)

The explicit `aw-provider-sec-core --health --helper ABS --socket ABS` entry
supports installation checks without invoking a security operation. It calls
only the public `agent-sec-daemon-health` helper, which authenticates the root
Unix-socket peer and validates `daemon.health`. The caller selects a trusted,
receipt-verified helper; the diagnostic is not an executable sandbox.

The independent `aw-sec-core-health/v1alpha1` schema lives in
`../../crates/aw-provider-sec-core/schemas/health-v1alpha1.schema.json`.
`helper_installed` is file presence and `socket_configured` is an absolute input
path, not verification of the AW configuration. Only a successful helper yields
`authenticated_reachable`; missing components, helper errors, output limits and
process deadlines are unavailable. Output excludes paths, environment and raw
helper diagnostics. The CLI budget is one second and each output stream is
limited to 4096 bytes. The test-only peer-check bypass is removed from the child
environment. AW does not duplicate or relax sec-core's peer authentication.

Admission and check results are always unknown. AW target admission belongs to
the current instance/generation, and a scan verdict belongs to that invocation;
health cannot supply either. This entry is deliberately separate from the
existing Provider event and view protocols. It is not registered in `aw inspect`
and does not change that command's output. A later unified read surface needs
its own reviewed compatibility contract.

The Provider package includes the helper and official systemd/configuration
reference assets, covered by its existing receipt. Assets are not installed as
system services. An existing root backend remains owned by its system package;
AW install/uninstall does not change `/etc`, stop shared services or migrate
backend data. Automatic system installation/upgrade/removal remains outside
this change. Runtime `scan_code` keeps its per-call authentication and failure
policy, while describe/config validation/completion observation remain offline.

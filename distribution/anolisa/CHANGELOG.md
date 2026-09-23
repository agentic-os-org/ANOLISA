# Changelog

[中文版](CHANGELOG_zh.md)

All notable changes to ANOLISA will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.16] - 2026-09-23

### Added

- Add prebuilt CLI archives and the `@anolisa/cli-darwin-x64` npm package for
  Intel Macs running macOS 11 or later. The website installer remains ARM-only
  until Intel Mac installation is enabled separately
  ([#3448](https://github.com/agentic-os-org/ANOLISA/pull/3448)).

### Changed

- Recognize Intel macOS as a supported target for `cosh-ng` in the component
  catalog, allowing ANOLISA to resolve compatible releases when available
  ([#3448](https://github.com/agentic-os-org/ANOLISA/pull/3448)).

## [0.3.15] - 2026-09-22

### Fixed

- RPM-installed adapters can now verify and use files split across declared
  subpackages, including adapter bundles and shared skills. Missing packages or
  unverifiable files still stop activation instead of accepting unverified inputs
  ([#3417](https://github.com/agentic-os-org/ANOLISA/pull/3417)).
- RPM transaction and upgrade preflight errors now redact repository credentials,
  paths, and query strings from native package-manager diagnostics, including
  configured URLs containing whitespace. HTTP(S) origins remain visible for
  troubleshooting
  ([#3431](https://github.com/agentic-os-org/ANOLISA/pull/3431)).

## [0.3.14] - 2026-09-20

### Added

- Manage OpenCode local plugins with `anolisa adapter enable tokenless opencode`,
  `anolisa adapter status tokenless`, and `anolisa adapter disable tokenless opencode`.
  Existing links to the same plugin can be adopted, conflicting user entries are
  preserved, and interrupted changes can be retried. Custom configuration directories
  are supported through `OPENCODE_CONFIG_DIR`. Restart OpenCode after enabling or
  disabling; runtime loading remains reported as `unknown`
  ([#3346](https://github.com/agentic-os-org/ANOLISA/pull/3346)).
- `anolisa bug --component cosh-ng` now exports a local diagnostic bundle and includes
  its health findings and collection summary in Markdown and `--json` reports.
  `COSH_SHELL_BIN` can override the installed binary. Failed collection provides a
  reason and a manual retry command; `--dry-run` skips collection, and bundles are
  never uploaded automatically
  ([#3317](https://github.com/agentic-os-org/ANOLISA/pull/3317)).

### Fixed

- `anolisa bug --component <name>` now finds visible system installations from user
  mode and reads recent logs from the installation's scope. Unreadable cross-scope
  logs produce a warning without aborting the report
  ([#3317](https://github.com/agentic-os-org/ANOLISA/pull/3317)).

## [0.3.13] - 2026-09-20

### Changed

- RPM installs, updates, removals, and dependency preflight checks now support
  Yum 3 and DNF 4. ANOLISA prefers `yum` and uses `dnf` only when `yum` is absent;
  requested versions remain exact, and system restrictions such as version locks
  cause a failure instead of silently selecting another version
  ([#3318](https://github.com/agentic-os-org/ANOLISA/pull/3318)).
- RPM candidate queries now read and validate the configured repository metadata
  directly, without requiring `dnf repoquery`. Repository access honors proxy and
  private CA settings in `/etc/yum.conf`, or `/etc/dnf/dnf.conf` when the former
  is absent. Local package queries and Raw installs also work without a configured
  RPM repository; errors from a configured repository are still reported
  ([#3318](https://github.com/agentic-os-org/ANOLISA/pull/3318),
  [#3353](https://github.com/agentic-os-org/ANOLISA/pull/3353)).
- Raw installs now name the requested platform and list published alternatives
  when the component has no installable artifact for the host OS and architecture,
  including in `--json` errors, making platform gaps distinguishable from unknown
  component names
  ([#3326](https://github.com/agentic-os-org/ANOLISA/pull/3326)).

### Fixed

- Runtime dependency checks, provisioning, and `anolisa doctor` now consistently
  recognize RPM and Debian derivative distributions through `ID` and `ID_LIKE`.
  Unsupported package families receive manual installation guidance instead of
  selecting an unrelated package manager from `PATH`
  ([#3318](https://github.com/agentic-os-org/ANOLISA/pull/3318)).
- Cloud metadata collection now continues reading other fields after an HTTP
  error or empty response for one key, preserving available instance information
  in telemetry snapshots. Connection and transfer failures still stop subsequent
  queries to an unreachable endpoint
  ([#3308](https://github.com/agentic-os-org/ANOLISA/pull/3308)).

## [0.3.12] - 2026-09-16

### Changed

- `anolisa upgrade --dry-run` now checks the combined set of new RPM installs
  with the native dependency solver when run as root. Conflicts appear as
  errors; non-root previews remain available with an explicit warning that
  install conflicts were not checked. Rerun with `sudo` and preserve any
  `--target` option to perform the check
  ([#3294](https://github.com/agentic-os-org/ANOLISA/pull/3294)).
- The bundled ws-ckpt adapter metadata now requires OpenClaw `>=2026.2.13`,
  preventing adapter enablement on older hosts whose config writes can persist
  expanded environment values and runtime defaults
  ([#3239](https://github.com/agentic-os-org/ANOLISA/pull/3239)).

### Fixed

- `anolisa adapter enable <component> openclaw` now explicitly activates the
  plugin after installation, restoring plugins left disabled by a previous
  uninstall. Capability consent is detected separately for activation, and
  `--dry-run` includes the activation command
  ([#3223](https://github.com/agentic-os-org/ANOLISA/pull/3223)).
- Sandbox install and uninstall now honor DNF's exit status even when its
  output contains phrases such as “already installed” or “No match”. Failed
  installs no longer continue to later setup phases or record success
  ([#3226](https://github.com/agentic-os-org/ANOLISA/pull/3226)).
- RPM package and file-owner queries now distinguish clean absence from
  database and command failures, avoiding false “not installed” observations
  ([#3230](https://github.com/agentic-os-org/ANOLISA/pull/3230)).
- Raw component service management now reports failed `systemctl is-active`
  queries instead of treating them as missing units; valid inactive and
  failed service states remain supported
  ([#3240](https://github.com/agentic-os-org/ANOLISA/pull/3240)).
- Runtime dependency checks now stop automatic provisioning when RPM queries
  fail or system-package and language-runtime probes encounter permission,
  execution, or signal failures. `anolisa doctor` retains the diagnostic and
  recommends inspecting the probe instead of installing a supposedly missing
  dependency
  ([#3280](https://github.com/agentic-os-org/ANOLISA/pull/3280),
  [#3285](https://github.com/agentic-os-org/ANOLISA/pull/3285)).
- Debian runtime dependency checks now distinguish usable packages, absent
  packages, incomplete package states, and query failures. Incomplete states
  require manual recovery before provisioning; `doctor` directs users to
  inspect the package state. Multi-Arch queries select the native architecture
  unless the dependency explicitly requests one
  ([#3292](https://github.com/agentic-os-org/ANOLISA/pull/3292)).

## [0.3.11] - 2026-09-09

### Added

- Register `ktuner` in both component catalogs for Linux x86_64 RPM
  discovery and installation when the configured repository provides it
  ([#2084](https://github.com/alibaba/anolisa/pull/2084)).

### Changed

- `anolisa adapter enable <component> openclaw` now accepts the plugin's
  declared capabilities when the host advertises `--accept-capabilities`.
  Unsafe-install bypass still requires separate explicit authorization
  ([#3126](https://github.com/alibaba/anolisa/pull/3126)).
- `status` and `doctor` now allow content edits to raw-installed
  `type = "config"` files, including directory-expanded files, while still
  checking presence, file type, path safety, permissions, and capabilities.
  Legacy state recovers config kinds where the installed manifest is
  unambiguous. Update and uninstall behavior is unchanged; back up edited
  configs before those operations. Older CLI versions cannot read the new
  `kind = "config"` state: before downgrading, back up state and change those
  owned-file entries to `kind = "file"`, restoring content digest checks
  ([#3142](https://github.com/alibaba/anolisa/pull/3142)).

### Fixed

- OpenClaw adapter cleanup can now finish when uninstall reports no tracked
  package and `plugins list --json` confirms the plugin is absent with clean
  diagnostics. Inconclusive results retain cleanup ownership for a retry
  ([#3118](https://github.com/alibaba/anolisa/pull/3118)).
- Component index reads now use a temporary private cache when the system
  cache is not writable, allowing ordinary users to resolve components for
  previews while retaining index validation. RPM install preflight still
  requires root with DNF 4
  ([#3127](https://github.com/alibaba/anolisa/pull/3127)).
- Fresh raw installs now record file digests after `post_install` and
  `post_enable` hooks, avoiding false corruption reports for hook-written
  content while detecting subsequent changes to immutable files. Repair and
  update still restore the package payload without replaying install hooks
  ([#3141](https://github.com/alibaba/anolisa/pull/3141)).
- RPM installs, including `--dry-run` and merged `install --all` transactions,
  now check DNF conflicts before creating recovery journals, so solver
  refusals no longer leave false pending operations. Use `sudo` for RPM
  `--dry-run` with DNF 4; the check may refresh repository metadata
  ([#3158](https://github.com/alibaba/anolisa/pull/3158)).
- `anolisa list` now uses the same authoritative RPM package mapping as
  install, preventing legacy aliases or stale provider declarations from
  showing the wrong component as installed
  ([#3158](https://github.com/alibaba/anolisa/pull/3158)).
- `anolisa forget` now directs journal-only pending operations to
  `anolisa repair <component>` instead of reporting nothing to forget,
  preserving the recovery evidence
  ([#3158](https://github.com/alibaba/anolisa/pull/3158)).

## [0.3.10] - 2026-09-06

### Added

- QwenPaw plugin adapters can now be discovered, enabled, inspected, and
  disabled through `anolisa adapter`. For Tokenless packages that include the
  QwenPaw bundle, use `anolisa adapter enable tokenless qwenpaw`. Installation
  and removal use QwenPaw's CLI and verify the resulting plugin files; failed
  removal preserves the plugin directory and receipt for a retry, and cleanup
  uses the working directory recorded at enable time
  ([#3075](https://github.com/alibaba/anolisa/pull/3075)).

### Fixed

- `anolisa logs --limit N` now returns the most recent N matching records in
  append order, with the oldest record in that window first, instead of
  returning the earliest matches. The default limit of 50 therefore shows
  recent activity as logs grow, and scanning existing logs no longer blocks
  new records from being appended
  ([#2618](https://github.com/alibaba/anolisa/pull/2618)).

## [0.3.9] - 2026-09-02

### Added

- Applied and failed `anolisa update self` operations now enter the central
  operation log with their version and apply-mode context. Administrators can
  audit them through `anolisa logs`, while generated bug reports include only
  failed records and redact credentials from handled update URLs; previews and
  already-current no-ops remain log-free
  ([#2994](https://github.com/alibaba/anolisa/pull/2994)).

### Changed

- Newly shipped Raw repository configurations no longer advertise the unused
  `cache_ttl_secs` and `offline_fallback` settings. Existing configurations
  containing either field remain compatible, while Raw resolution continues
  to fetch the current distribution index and fail when it is unavailable
  ([#3002](https://github.com/alibaba/anolisa/pull/3002)).

### Fixed

- Global and command-local sandbox uninstall dry-runs now show the planned
  package changes without applying them, while system-mode telemetry mutation
  previews return before privilege checks or side effects
  ([#2922](https://github.com/alibaba/anolisa/pull/2922),
  [#2926](https://github.com/alibaba/anolisa/pull/2926)).
- Commands without a preview implementation now reject global `--dry-run`
  before dispatch. Sandbox remove plus kernel and security install reject both
  dry-run forms with `INVALID_ARGUMENT` and confirm that no action was taken,
  instead of reaching mutation, privilege, or not-implemented paths
  ([#2952](https://github.com/alibaba/anolisa/pull/2952),
  [#2957](https://github.com/alibaba/anolisa/pull/2957),
  [#2961](https://github.com/alibaba/anolisa/pull/2961)).
- The standalone installer now detects when PATH still resolves `anolisa` to
  another installation, reports both paths and versions, and provides a
  directly runnable PATH fix or matching npm/Homebrew removal command instead
  of printing an unqualified success message
  ([#2944](https://github.com/alibaba/anolisa/pull/2944)).

## [0.3.8] - 2026-08-26

### Added

- Tagged ANOLISA releases now include verified prebuilt CLI archives for Linux
  x64, Linux arm64, and macOS arm64. Users can download a standalone binary
  archive for each supported target directly from the GitHub Release
  ([#2883](https://github.com/alibaba/anolisa/pull/2883)).

### Fixed

- Raw installs now preserve `${VAR}` references in rendered file content for
  shell and systemd consumers, while continuing to expand nested ANOLISA layout
  placeholders and reject environment references in destination paths.
  `anolisa install cosh-ng --backend raw` can now install the gateway service
  template instead of rejecting its `EnvironmentFile=`-backed workspace
  reference as an unknown placeholder
  ([#2903](https://github.com/alibaba/anolisa/pull/2903)).

## [0.3.7] - 2026-08-25

### Changed

- Source and RPM builds now require Rust 1.93, with the rustup toolchain pinned
  to 1.93.1 and Cargo dependency resolution constrained by the declared MSRV.
  Builders can use the newest compiler packaged by Alibaba Cloud Linux 4
  without Cargo selecting dependencies that exceed the supported compiler;
  older Rust toolchains must be upgraded
  ([#2810](https://github.com/alibaba/anolisa/pull/2810)).

### Fixed

- `anolisa --dry-run restart <component>` now lists the units that would be
  restarted without invoking `systemctl daemon-reload` or `systemctl restart`.
  System-mode previews read recorded state without taking the exclusive install
  lock, so they no longer require write access to the state root
  ([#2774](https://github.com/alibaba/anolisa/pull/2774)).

## [0.3.6] - 2026-08-22

### Fixed

- `anolisa --quiet adapter scan` and `anolisa --quiet adapter status` now
  suppress all non-error human output, including empty-state messages and
  result tables, while `--json` continues to emit the standard envelope.
  Agents can rely on quiet adapter inspection producing no human output
  ([#2752](https://github.com/alibaba/anolisa/pull/2752)).
- `anolisa --dry-run forget <component>` now refuses a component that still
  has enabled adapters with the same `INVALID_ARGUMENT`, exit code 2, and
  `adapter disable` guidance as the real operation. Previews no longer report
  that an impossible forget would succeed, while unrelated adapter receipts
  remain ignored
  ([#2762](https://github.com/alibaba/anolisa/pull/2762)).

## [0.3.5] - 2026-08-20

### Fixed

- Uninstalling a component with a bare systemd service template now stops
  every loaded instance through `name@*.service` before disabling the declared
  `name@.service` template. Template-backed services no longer remain running
  after `anolisa uninstall`, while individual stop failures continue to surface
  as warnings without blocking cleanup
  ([#2603](https://github.com/alibaba/anolisa/pull/2603)).

## [0.3.4] - 2026-08-19

### Changed

- Component-targeting commands now use the repository component index as the
  sole authority for names absent from local state. Installed and recovery
  identities remain usable offline, while unsupported names return
  `INVALID_ARGUMENT`, an unavailable index returns `EXECUTION_FAILED`, and
  `NOT_INSTALLED` now reliably means a supported component is absent. A
  `--repo` override also governs identity and package selection for the whole
  invocation, so site-local package mappings and RPM `Provides` metadata can
  no longer establish unrecognized component names
  ([#2637](https://github.com/alibaba/anolisa/pull/2637)).

### Fixed

- Missing local Raw repository index errors now identify whether the active
  repository came from the exact `repo.toml` path or a one-off `--repo`
  override and provide matching recovery guidance. Users no longer need to
  guess which source configured the missing repository
  ([#2650](https://github.com/alibaba/anolisa/pull/2650)).

## [0.3.3] - 2026-08-18

### Added

- Telemetry instance snapshots now include the detected container runtime as
  `instance.container` for Docker, Podman, containerd, Kubernetes cgroups, and
  LXC, while bare-metal hosts omit the field. This gives downstream deployment
  statistics and troubleshooting a container-aware signal without collecting
  container or pod identities
  ([#2642](https://github.com/alibaba/anolisa/pull/2642)).

### Changed

- `anolisa status <component>` now validates new targets against the component
  index, resolves package aliases, rejects unsupported names with `anolisa list`
  guidance, and directs telemetry service targets to
  `anolisa telemetry status`. Exact installed identities remain inspectable
  when repository metadata is unavailable
  ([#2626](https://github.com/alibaba/anolisa/pull/2626)).

### Fixed

- Raw adapter bundle installs now preserve each archive file's mode and record
  the effective mode for integrity checks. Framework hooks and scripts retain
  their executable bit instead of being installed uniformly as data files
  ([#2619](https://github.com/alibaba/anolisa/pull/2619)).

## [0.3.2] - 2026-08-17

### Added

- ANOLISA now provides a native DSH adapter driver for plugin bundles.
  `anolisa adapter enable <component> dsh --profile <name>` accepts repeatable
  profiles, validates the bundle identity, delegates profile changes to DSH,
  and remembers the enable-time DSH home so status, disable, and re-enable keep
  targeting the same profiles even if `DSH_HOME` or the working directory
  changes. Disable DSH adapters before downgrading to an earlier ANOLISA release
  ([#2580](https://github.com/alibaba/anolisa/pull/2580)).
- `anolisa logs --level <LEVEL>` is now a visible alias for the existing
  `--severity` option, with the same validation and filtering behavior while
  `severity` remains the canonical JSON field
  ([#2558](https://github.com/alibaba/anolisa/pull/2558)).

### Changed

- `anolisa list` and `anolisa install --all` now evaluate component
  availability against an exact OS and architecture target from schema v2
  `components-v2.toml`. JSON output replaces `platforms` and
  `platform_available` with `targets` and `target_available`; repository
  publishers must deploy the v2 index beside the unchanged v1 index
  ([#2533](https://github.com/alibaba/anolisa/pull/2533)).

### Fixed

- `anolisa --dry-run install` now reads `meta.toml` beside the resolved Raw
  artifact before falling back to version-level metadata only when the sibling
  file is absent. Previews now validate the selected target's contract, reject
  corrupt published metadata instead of masking it, and still avoid downloading
  the artifact
  ([#2551](https://github.com/alibaba/anolisa/pull/2551)).
- System-helper status now reports `unknown` when `systemctl` cannot be started
  and reports `failed` only for a unit whose actual state is failed
  ([#2604](https://github.com/alibaba/anolisa/pull/2604)).

## [0.3.1] - 2026-08-13

### Fixed

- Adapter discovery now excludes undeclared shared resource directories unless
  a contract, receipt, or built-in framework driver identifies them as real
  adapters. Shared assets such as Tokenless common hooks no longer appear as
  unsupported frameworks in adapter scan and status output
  ([#2502](https://github.com/alibaba/anolisa/pull/2502)).

## [0.3.0] - 2026-08-12

### Fixed

- Adapter enable, status, and update now derive adapter revisions from
  ANOLISA-owned Raw files or native package metadata instead of hashing whole
  resource trees. Runtime caches and other unowned files no longer cause false
  drift or get copied into frameworks, while changed package-owned inputs block
  enable before framework mutation and unavailable metadata is reported with
  an `unknown` status
  ([#2419](https://github.com/alibaba/anolisa/pull/2419)).
- Re-enabling adapters now removes only stale materialized files recorded by the
  previous receipt, preserves runtime-created files, previews the cleanup with
  `--dry-run`, and retains the old receipt when a directory-to-file replacement
  would discard runtime data
  ([#2438](https://github.com/alibaba/anolisa/pull/2438)).

## [0.2.20] - 2026-08-11

### Changed

- `anolisa list` now announces the detected host platform and reports component
  availability instead of backend and ownership columns in human-readable
  output. Components unsupported on the host remain visible with their
  supported platform and no install action, while JSON adds `platforms` and
  `platform_available` without removing backend or ownership metadata
  ([#2367](https://github.com/alibaba/anolisa/pull/2367)).

### Fixed

- npm installs now keep `@anolisa/cli` as the sole owner of the public
  `anolisa` executable. With npm 10, local installs reliably create
  `node_modules/.bin/anolisa` instead of losing the command when platform
  packages are linked
  ([#2345](https://github.com/alibaba/anolisa/pull/2345)).

## [0.2.19] - 2026-08-10

### Fixed

- Raw installs that provision system dependencies now map resolver-provided
  `rpm` and `deb` package-family hints directly to the matching package
  manager backend. Minimal supported hosts no longer report an unsupported
  package base merely because the optional `which` command is absent, while
  distro-specific hints remain compatible
  ([#2314](https://github.com/alibaba/anolisa/pull/2314)).
- `anolisa --json osbase sandbox list` and
  `anolisa --json register status` now use the standard success envelope with
  `ok`, `schema_version`, and `command` metadata and nest business fields
  under `data`. Scripts can parse these legacy commands the same way as other
  JSON surfaces
  ([#2319](https://github.com/alibaba/anolisa/pull/2319)).
- OpenClaw adapters now honor `OPENCLAW_STATE_DIR` with OpenClaw-compatible
  whitespace, tilde, and absolute-path handling, keeping plugins, skills,
  receipts, status, and disable operations on the configured state.
  Re-enabling safely migrates resources recorded under the legacy fallback,
  preserves the old receipt when cleanup must be retried, and previews the
  migration during `--dry-run`. If an older receipt used an `OPENCLAW_HOME`
  that is no longer present in the environment, temporarily restore it before
  migration or cleanup
  ([#2337](https://github.com/alibaba/anolisa/pull/2337)).

## [0.2.18] - 2026-08-06

### Changed

- Telemetry upload now treats `SLS_PROJECT_PREFIX` as an SLS project prefix
  and appends the detected region, for example `anolisa-cn-hangzhou`.
  Deployments that set the legacy `SLS_PROJECT` must migrate to
  `SLS_PROJECT_PREFIX` so uploads reach their region-specific project
  ([#2260](https://github.com/alibaba/anolisa/pull/2260)).

### Fixed

- Raw installs now stream only selected archive payloads through private,
  disk-backed staging instead of retaining uncompressed contents in memory.
  Large packages can install with bounded payload memory while preserving
  atomic placement, rollback, cleanup, and digest verification
  ([#2250](https://github.com/alibaba/anolisa/pull/2250)).
- `anolisa status` and `anolisa doctor` now hash ANOLISA-owned files up to
  2 GiB and treat larger files as unchecked and degraded instead of failed.
  Intact components with large artifacts no longer appear damaged or trigger
  spurious repair, while recovery still fails closed when verification is
  required
  ([#2271](https://github.com/alibaba/anolisa/pull/2271)).
- Enabling a Codex adapter that declares hooks now discovers the installed
  plugin's hook identities and atomically persists their trusted hashes, so
  non-interactive `codex exec` sessions can run them. Missing hooks or
  overridden trust settings stop enablement with actionable diagnostics
  ([#2281](https://github.com/alibaba/anolisa/pull/2281)).

## [0.2.17] - 2026-08-05

### Added

- Raw installs can now render layout placeholders such as `{bindir}` and
  `{datadir}` inside declared text files before placement, so shared package
  templates follow the selected install scope and prefix. Integrity checks and
  repair use the rendered bytes
  ([#2222](https://github.com/alibaba/anolisa/pull/2222)).

### Changed

- Raw repository resolution now prefers the generation-2 index when published
  and enforces each component's minimum CLI version. Incompatible entries fail
  with an `anolisa update self` hint instead of silently installing an older or
  malformed result, while generation-1 repositories remain compatible
  ([#2222](https://github.com/alibaba/anolisa/pull/2222)).
- RPM-backed adapter scan, status, and enable operations now use a declared
  package-owned resource root and report a missing or invalid root instead of
  falling back to stale raw files. Codex adapters that target an external RPM
  root record a trust anchor; disable them before downgrading to `0.2.16`
  ([#2222](https://github.com/alibaba/anolisa/pull/2222)).

### Fixed

- Qoder native plugin bundles now use Qoder's own plugin lifecycle instead of
  being copied or rewritten as legacy hook bundles. Existing same-ID plugins
  across user and project scopes are protected, and unverified installs or
  removals retain a retryable receipt rather than claiming or deleting user
  state
  ([#2221](https://github.com/alibaba/anolisa/pull/2221)).

## [0.2.16] - 2026-08-03

### Added

- Successful `anolisa update <component>` and `anolisa update all` operations
  now report adapters whose resource bundles changed, with the exact
  `anolisa adapter enable ...` or `anolisa adapter status ...` follow-up
  command. JSON responses expose the same information through stable
  `adapter_actions` arrays
  ([#2018](https://github.com/alibaba/anolisa/pull/2018)).

### Fixed

- Raw system-scope installs on Debian-family hosts no longer fail with
  `rpm not found on PATH` when both RPM tooling and an RPM database are absent.
  An existing or newly appearing RPM database still stops the raw install
  before files change
  ([#2061](https://github.com/alibaba/anolisa/pull/2061)).

## [0.2.15] - 2026-07-30

### Added

- Interactive `anolisa install`, `anolisa install --all`, and
  `anolisa uninstall` now display phase-based activity during long-running
  planning and execution. ANSI-capable terminals animate the current phase,
  while limited interactive terminals print static phase lines
  ([#2036](https://github.com/alibaba/anolisa/pull/2036)).

### Changed

- Human-readable failures now use conventional `error:` and `hint:` labels
  without exposing machine codes. `--json` retains structured error codes, and
  exit statuses remain unchanged.
- Update notifications now quote the recommended `sudo anolisa upgrade` and
  `anolisa update --check` commands so their boundaries are clear.

## [0.2.14] - 2026-07-29

### Fixed

- `anolisa status` and `anolisa doctor` now detect Unix mode and Linux file
  capability drift for raw-managed files, including installations recorded by
  earlier releases, and recommend `anolisa repair` for recovery.
- `anolisa repair` now replays raw-managed components when only file metadata
  has drifted, restoring declared modes and confirmed capabilities. Failed
  updates restore only capabilities known to have been active before the
  operation, avoiding optional grants that never applied
  ([#1987](https://github.com/alibaba/anolisa/pull/1987)).

## [0.2.13] - 2026-07-28

### Added

- The `@anolisa/cli` npm package now supports macOS arm64 and selects the
  matching native binary during installation.
- Tokenless adapters now support Qwencode while keeping the Cosh extension
  independent from shared hook assets.

### Fixed

- Raw installs now refuse to provision a system package reserved by another
  pending RPM install and direct the user to `anolisa repair`, preventing
  components from claiming or later removing each other's dependencies.
- `cosh-ng` RPM installations now retain the `cosh-ng` component identity.
  Unambiguous legacy records and recovery journals stored as `cosh` are
  repaired so lifecycle commands target the correct component.
- Failed raw updates and repairs now restore file permissions and capabilities
  during rollback, keeping restored binaries executable.
- Enabling the Tokenless Qoder adapter now resolves shared hook paths in the
  cached plugin, preventing matching tool calls from failing because of broken
  hook commands.

## [0.2.12] - 2026-07-27

### Changed

- Commands that act on an installed component now report an absent target as
  `NOT_INSTALLED` instead of `INVALID_ARGUMENT`, so a caller can tell "there was
  nothing to act on" from "the invocation was wrong" without parsing the error
  message. The code reports state absence only, and does not indicate whether
  the name was valid. Affects `uninstall`, `update`, `repair`, `forget`,
  `restart`, and `adapter`; the exit code stays 2
  ([#1915](https://github.com/alibaba/anolisa/pull/1915)).

### Fixed

- Adapter status now ignores empty or incomplete stale source directories and
  reports missing bundles as degraded; raw uninstalls prune empty directories
  so another installation scope cannot be shadowed
  ([#1850](https://github.com/alibaba/anolisa/pull/1850)).
- Raw install dry-runs now validate component conflicts before execution,
  keeping preview results aligned with real installs. Repositories without
  lightweight sidecar metadata warn that conflict validation was skipped
  ([#1898](https://github.com/alibaba/anolisa/pull/1898)).

## [0.2.11] - 2026-07-24

### Added

- Raw `anolisa install --version` now installs the exact published component version.
- Raw `anolisa install --version` output now reports requested and resolved versions, artifact URL, and source repository.

### Changed

- Raw `anolisa install --version` now lists published alternatives when the requested version is unavailable.

### Fixed

- Raw `anolisa install --version` no longer installs another version when the requested version is unavailable.
- Raw component uninstalls with many files now complete faster and write substantially less data.
- Recovery now preserves installed state when operation recovery data is missing or corrupted.

## [0.2.10] - 2026-07-23

### Added

- `anolisa telemetry` now lets administrators enable or disable data collection.
- `anolisa telemetry` now lets administrators link or unlink named reporting.
- `anolisa telemetry status` now reports collection and named-reporting states in text or JSON.
- Adapter enable and disable commands now display component-provided follow-up notices.
- Adapter JSON output now includes structured component-provided notices.
- `anolisa install --version` JSON output now includes requested and resolved versions, source repository, and exact RPM.

### Changed

- Fresh ANOLISA RPM installations now enable anonymous telemetry by default.
- RPM installation output now explains how to disable telemetry.
- Enabled telemetry now resumes automatically after restarts on supported hosts.
- `anolisa register` now warns that the command is deprecated.
- `anolisa register` now enables telemetry without prompting.
- `anolisa register status` now directs users to `anolisa telemetry status`.
- `anolisa unregister` now disables telemetry while preserving local logs.
- `anolisa install --version` now selects the exact host-compatible RPM matching the requested version.
- `anolisa install --dry-run --version` now validates availability and displays the resolved RPM details.
- Adapter dry-runs now preview component-provided notices without changing the host.
- Adapter quiet output now suppresses component-provided notices.

### Fixed

- `anolisa register` now prevents duplicate uploads from earlier telemetry configurations.
- `anolisa unregister` no longer leaves earlier telemetry configurations reporting.
- `anolisa install --version` no longer changes the host when the requested RPM is unavailable or incompatible.
- `anolisa install --version` no longer records success when a different RPM version is installed.
- `anolisa repair` now rejects interrupted RPM installs whose installed version differs from the original request.
- `anolisa repair` now reports when an interrupted RPM install's architecture cannot be verified.
- `anolisa adapter disable` now shows saved follow-up notices even when component files are unavailable.
- Adapter notices can no longer inject terminal formatting into human-readable output.

## [0.2.9] - 2026-07-22

### Added

- `anolisa update all` now updates every tracked raw and RPM component while leaving the CLI unchanged.
- `anolisa repair` now restores damaged raw installations from their recorded versions.
- `anolisa repair` now supports user-scope installations without root privileges.
- `anolisa repair` now recovers interrupted install, update, adopt, uninstall, and batch operations.
- `anolisa repair` now reinstalls missing managed RPM packages.
- `anolisa status` now reports unclassified legacy records as needs-attention with scope-aware repair and forget guidance.
- `anolisa status` and `anolisa doctor` now run health checks from each installation's saved component manifest.
- User-mode adapter commands can now target visible system installations.
- `anolisa repair` can now restore unclassified legacy records from installed packages or intact files.
- `anolisa forget` can now remove unclassified legacy records without touching installed files or packages.

### Changed

- `anolisa install` now refuses unmanaged system RPMs and directs users to `anolisa adopt`.
- `anolisa install` now succeeds without changes when the component is already tracked.
- `anolisa adopt` now makes existing RPMs updatable while keeping package removal opt-in.
- `anolisa adopt` now succeeds without changes for already adopted packages.
- `anolisa update` now requires observed-only RPMs to be adopted first.
- `anolisa install --all` now applies new RPM packages in one package transaction.
- `anolisa upgrade` now applies planned RPM updates in one package transaction.
- `anolisa upgrade` now applies planned RPM installs in one package transaction.
- `anolisa list` and `anolisa status` now show separate user and system rows for shadowed components.
- `anolisa list` now labels tracked installations as owned, managed, adopted, or observed.
- `anolisa --install-mode user install` can now create a user installation beside a visible system installation.
- Lifecycle mutations now remain within the selected scope, including package aliases.
- The first modifying command now upgrades legacy state and preserves `installed.toml.v4.bak`.
- Newer state formats now produce an error instead of appearing empty.
- `install-anolisa.sh` now leaves distribution index retrieval to the CLI, keeping mirror data current.
- `install-anolisa.sh` now stages only OS-base manifests because component manifests are fetched when needed.
- `install-anolisa.sh --strict` now validates only binary and manifest bundle checksums.
- `ANOLISA_INDEX_URL` and `ANOLISA_INDEX_SHA256` no longer affect `install-anolisa.sh`.
- Lifecycle JSON output now includes explicit plans across install, adopt, update, repair, and uninstall.
- Uninstall JSON output now uses one schema for raw and RPM components, including package removal and plans.
- `anolisa uninstall --dry-run` now reports missing components as errors instead of empty successful plans.
- `anolisa forget` and `anolisa restart` now stop while an earlier component operation needs recovery.
- `anolisa doctor` now reports incomplete operations even without an active component record.

### Fixed

- RPM-managed components no longer fail `status` or `doctor` because of raw-install health checks.
- RPM component updates now refresh saved component manifests before reporting success.
- Incomplete RPM manifest refreshes now appear in `anolisa logs --severity warn`.
- Interrupted RPM updates now remain repairable instead of appearing successful with stale settings.
- `anolisa doctor` no longer recommends lifecycle commands when recovery data is unreadable or ambiguous.
- `anolisa doctor` no longer duplicates recovery findings across components sharing one state location.
- `anolisa doctor --help` now states that `--fix` remains unavailable.
- Batch RPM failures now preserve repairable state for packages that changed.
- Component aliases no longer redirect lifecycle changes into a different installation scope.
- Health checks for user services now use the correct service manager across installation scopes.
- Failed batch RPM operations now retry unaffected components individually.

## [0.2.8] - 2026-07-21

### Added

- `anolisa adapter enable` now supports `--allow-unsafe-plugin-install` for explicitly authorized OpenClaw plugin installation.
- OpenClaw adapter settings can now target specific OpenClaw versions.

### Changed

- `anolisa adapter enable` now checks OpenClaw compatibility before making changes.
- `anolisa adapter enable` now verifies OpenClaw plugins are loaded before reporting success.
- When OpenClaw blocks an unsafe plugin, ANOLISA now shows the reported findings.
- When supported, OpenClaw safety errors now suggest retrying with explicit unsafe authorization.

### Fixed

- Failed OpenClaw setting updates can now be retried without losing track of affected settings.
- Re-enabling an OpenClaw adapter no longer loses track of settings applied by an earlier successful enable.
- `anolisa adapter disable` now warns when OpenClaw settings may remain after an uncertain update.

## [0.2.7] - 2026-07-18

### Added

- `anolisa adapter` now manages Qwen Code extensions through the `qwen` CLI for Qwen Code 0.17 and newer.

### Changed

- `anolisa upgrade` and `anolisa repair` now explain component manifest reconciliation in human and JSON output.

### Fixed

- `anolisa upgrade` now refreshes component manifests after RPM package upgrades.
- `anolisa upgrade` now reconciles same-version RPM component manifest changes.
- `anolisa repair` now refreshes stale component manifests from the installed RPM.
- Failed component manifest refreshes now keep RPM components repairable and report the affected component.

## [0.2.6] - 2026-07-16

### Fixed

- `anolisa status` no longer reports healthy RPM-managed components as failed.

## [0.2.5] - 2026-07-14

### Added

- `anolisa repair` now recovers interrupted fresh RPM installs after the package has been installed.
- `anolisa update --check` now reports RPM components whose saved state requires reconciliation.

### Changed

- Install, adopt, and upgrade commands now require interrupted RPM installs to be repaired before continuing.

### Fixed

- Concurrent RPM installs now fail safely instead of overwriting another operation's component state.
- Reinstalling a missing ANOLISA-managed RPM now preserves the component's settings and history.
- `anolisa uninstall --dry-run --json` now includes `dry_run: true` and omits removal phases for components that are not installed.
- `anolisa upgrade` now refreshes saved RPM versions and package details after upgrades.
- `anolisa upgrade` now reconciles older RPM records that lack package details.

## [0.2.4] - 2026-07-13

### Added

- `anolisa update --check` now shows progress while checking for updates in interactive terminals.
- `anolisa upgrade` now shows progress while planning and applying upgrades in interactive terminals.

### Fixed

- Raw component installs and updates now choose installable archives even when binary releases are also listed.

## [0.2.3] - 2026-07-12

### Changed

- Package installation and removal progress now uses stderr, keeping command output safe for redirection.

### Fixed

- ANOLISA commands now exit cleanly when a downstream pipeline closes standard output early.
- ANOLISA commands now report standard output write failures instead of silently succeeding.

## [0.2.2] - 2026-07-09

### Added

- `anolisa update --check` now reports RPM upgrade opportunities without changing state.
- `anolisa update --check --motd` now prints a short login-friendly upgrade summary.
- `anolisa upgrade` now applies RPM image upgrades for RPM-managed toolchains.
- `anolisa upgrade` now installs missing default components from the selected target profile.
- `anolisa adapter scan` now marks enabled receipts with missing sources as orphaned.
- `anolisa adapter status` now reports missing adapter sources as degraded receipts.

### Changed

- `anolisa list` now shows component scope for visible user and system records.
- `anolisa status` now shows scope, mutability, shadowing, and state path metadata.
- `anolisa doctor` now includes readable system components in user-mode diagnostics.
- `anolisa doctor` now suggests system-mode commands for read-only system records.
- `anolisa update --check` now uses the latest target profile when `--target` is omitted.
- `anolisa update --check --motd` now points users to `sudo anolisa upgrade` when action is needed.

### Fixed

- `anolisa uninstall`, `forget`, and `update` now reject read-only system targets with system-mode guidance.
- `anolisa upgrade` now reports unresolved target defaults as check errors.
- `anolisa upgrade` now warns when refreshed RPM details are unavailable after an upgrade.

## [0.2.1] - 2026-07-08

### Added

- `anolisa adapter enable` now supports Tokenless adapters for cosh, Codex, and Claude Code.
- `anolisa adapter enable` now supports Tokenless adapters for Qoder.

### Changed

- Claude Code adapters now use per-component marketplaces to avoid affecting other ANOLISA plugins.
- `anolisa adapter enable` now rejects invalid framework and `adapter_type` combinations before changing settings.

### Fixed

- Codex adapters now work when component resources come from packaged data directories.
- Qoder adapter enable now keeps malformed `settings.json` unchanged instead of replacing it.
- Qoder adapter disable now removes only hook entries previously added by ANOLISA.
- Qoder adapters now prefer stable qodercli releases over matching prereleases.

## [0.2.0] - 2026-07-07

### Added

- Raw components can now declare `conflicts` to block incompatible raw installs.

### Fixed

- `anolisa install` now rejects raw component conflicts before changing the host.
- `anolisa install --dry-run` now reports raw component conflicts instead of showing an invalid plan.

## [0.1.20] - 2026-07-03

### Added

- ANOLISA can now be distributed as `@anolisa/cli` with Linux x64 and arm64 binaries.
- `repo.toml` now enables the npm backend for component distribution.

### Changed

- `anolisa list` now shows local state, ownership, and next action for each component.
- `anolisa list --json` now includes RPM package, version, architecture, and source repository details.

### Fixed

- RPM installs and updates now keep system repositories available for dependencies.
- Adapter commands now distinguish missing component manifests from invalid manifests.

## [0.1.19] - 2026-07-02

### Fixed

- `anolisa adapter disable --dry-run` now previews cleanup without removing adapter receipts or resources.
- Read-only commands now use downloaded repo config when saving `repo.toml` fails.
- Component commands now accept package aliases consistently when targeting installed components.
- Ambiguous package aliases no longer choose an arbitrary installed component.
- Unknown component names now report no match without querying packages.

## [0.1.18] - 2026-07-01

### Added

- `anolisa install` now auto-installs missing system packages for raw components in system mode.
- `anolisa install --dry-run` now labels unresolved dependencies as auto-install or manual.
- `anolisa install` now reports packages auto-installed during raw component installs.
- `anolisa status --verbose` now shows packages auto-installed for each component.

### Changed

- Commands that need repo access now download and validate `repo.toml` on first use.
- Repo config dry-runs now fetch and validate without writing `repo.toml`.
- RPM install and update now use only the `repo.toml` RPM repository.
- User-mode raw installs now report missing dependencies with install commands before changing files.
- Failed raw installs now list any auto-installed packages left on the system.
- `anolisa update self` no longer fetches repo config before checking CLI updates.

### Fixed

- `anolisa list --installed` now includes adopted RPM components.
- `anolisa list` now shows adopted, failed, and disabled component statuses.
- Adapter commands now prefer resources from the datadir that supplied the component contract.
- RPM installs now fail before `dnf` when `[backends.rpm]` is missing.
- RPM updates now explain missing `[backends.rpm]` instead of using host repositories.

## [0.1.17] - 2026-06-30

### Added

- Repository `components.toml` can now define component names, package aliases, and raw/RPM package mappings.
- `anolisa list --installed` now filters installed components.

### Changed

- `anolisa list` and `install --all` now read components from `components.toml` instead of `catalog.json`.
- `anolisa list` now shows `NAME`, `SUMMARY`, `BACKENDS`, and `STATUS`.
- `anolisa list --enabled` is now a hidden alias for `--installed`.
- `ANOLISA_CATALOG_URL` no longer changes list sources; configure `repo.toml` instead.
- `anolisa install`, `status`, `adopt`, and `repair` now resolve RPM package aliases from `components.toml`.
- `anolisa status` now suggests `sudo anolisa adopt <component>` for untracked RPM components.

### Fixed

- `anolisa status <RPM package>` now reports the canonical component row when an alias is installed.
- `anolisa repair <RPM package>` now refreshes the canonical component row when an alias is used.
- Non-root `anolisa osbase` mutations now reach the system helper instead of failing install-mode checks.
- Commands now reject root `--install-mode user` before writing ambiguous user-mode state.
- System-mode write commands now fail before changes when sudo is missing.
- Existing installs with managed symlinks no longer show false symlink integrity failures after upgrade.
- `anolisa status` now reports `referent_mismatch` when managed symlinks point elsewhere.

## [0.1.16] - 2026-06-29

### Added

- `anolisa osbase sandbox install runc` now installs runc, containerd, Docker, and Docker client.
- `anolisa osbase sandbox install` now enables services declared by sandbox scenarios.
- `anolisa osbase sandbox install` now runs scenario verification commands after installation.
- `anolisa osbase sandbox install` now records sandbox scenarios in `installed.toml`.
- `anolisa osbase sandbox install` now reports optional scenario packages as hints.
- `rund`, `firecracker`, and `gvisor` sandbox scenarios now define post-install checks.
- `anolisa adapter enable` now supports `adapter_type = "skill_bundle"` for OpenClaw and Hermes skills.
- The RPM package now installs default `repo.toml` to `/etc/anolisa/repo.toml`.
- ANOLISA telemetry setup now installs log rotation for ops `.jsonl` files.

### Changed

- `anolisa osbase sandbox install --dry-run` now shows preflight, package, service, verify, and state phases.
- `anolisa osbase sandbox install runc` now requires Linux kernel 4.18 or newer.
- `anolisa osbase sandbox install` now reports verification failures as warnings when other phases succeed.
- `repo.toml` now points RPM installs to the agentic-os repository path.
- `anolisa update self --json` now reports apply mode, RPM package, and RPM version observations.
- `anolisa adapter status` now treats skill bundles as healthy without plugin registration.
- `anolisa adapter enable` now rejects skill bundles that declare framework config entries.

### Fixed

- RPM-backed commands now use the component name as the default package name.
- `anolisa update self` now delegates RPM-owned CLI updates to `dnf`.
- Non-root sandbox installs now show package, service, verify, and state phases.
- Telemetry setup now runs the ilogtail installer with bash-compatible script handling.
- `anolisa adapter disable` now cleans skill bundles without plugin unregister errors.

## [0.1.15] - 2026-06-25

### Added

- `anolisa doctor` now reports component health, dependency status, and suggested fixes.
- Raw components can now declare runtime dependencies for install and update preflight checks.
- `anolisa install --dry-run` now previews runtime dependency status for raw components.

### Changed

- `anolisa install` and `update <component>` now refuse raw components with missing runtime dependencies before changing files.
- `anolisa restart <component>` now restarts service units shipped by RPM-backed components.
- `anolisa restart <component>` now shows guidance for RPM-backed template services instead of failing.

### Fixed

- `anolisa adapter enable` now expands `{datadir}` from the package that provided the adapter metadata.
- After `anolisa uninstall` or `forget`, adapter commands no longer see stale component metadata.

## [0.1.14] - 2026-06-24

### Added

- Raw components can now place systemd unit files with `{unitdir}`.
- Raw components can now place user service unit files with `{userunitdir}`.
- User-mode `anolisa install` now activates declared user-scope services.

### Changed

- User-mode `anolisa install` now resolves `%u` service templates to the current user.
- System-mode `anolisa install` now preserves `%u` user service templates for later per-user activation.
- `anolisa uninstall` now reloads systemd after removing declared service unit files.
- `anolisa restart <component>` now restarts user-scope services from user-mode installs.

### Fixed

- `anolisa install` now starts freshly installed service units without a manual systemd reload.
- `anolisa uninstall` now deactivates user-scope services from user-mode installs.
- `anolisa adapter enable` now finds `{datadir}` skills from the package directory that provides the adapter.

## [0.1.13] - 2026-06-23

### Added

- `anolisa adapter enable` now supports Hermes plugins.
- `anolisa adapter enable` now installs declared OpenClaw skills.
- `anolisa adapter enable` now applies declared OpenClaw config values.
- `anolisa install` now starts declared services for raw components.
- `anolisa install` now applies declared file capabilities for raw components.
- `anolisa install` now runs declared hooks for raw components.
- `anolisa update <component>` now restarts declared services for raw components.
- `anolisa update <component>` now reapplies declared file capabilities for raw components.
- `anolisa uninstall` now runs declared hooks for raw components.
- `anolisa uninstall` now disables declared services after stopping them.

### Changed

- `anolisa adapter scan` now honors declared adapter resource locations.
- `anolisa adapter enable` now reads package-installed adapter resources.
- `anolisa install --dry-run` now previews declared capabilities for raw components.
- `anolisa register status` now reports the latest registration after repeated changes.
- Cancelled `anolisa register` and `unregister` prompts now exit successfully.

### Fixed

- `anolisa adapter status` now detects OpenClaw plugins from wrapped table output.
- `anolisa adapter status` now ignores bundled Hermes plugins during checks.
- `anolisa adapter` commands now find metadata shipped by RPM-installed components.
- `anolisa register status` now reports sysom console registrations as active.

## [0.1.12] - 2026-06-22

### Added

- `anolisa update <component>` can update raw-managed components from the raw backend.
- `anolisa osbase sandbox list` shows scenarios from `sandbox.toml`.
- `anolisa osbase sandbox uninstall <scenario>` can remove packages for a sandbox scenario.
- `anolisa system setup` can install the helper service for non-root osbase commands.
- `anolisa system status` can show helper health, version, uptime, and last operation.
- `anolisa system teardown` can remove the helper service and sandbox config.
- `anolisa env --json` includes distro identity fields.

### Changed

- `anolisa osbase sandbox install <scenario>` now installs scenarios defined in `sandbox.toml`.
- Omitting `--install-mode` now selects `system` for root and `user` otherwise.
- `anolisa update <component> --dry-run` now lists raw backend candidate versions.

### Fixed

- Legacy `yum` backend names in `repo.toml` and `--backend` now resolve to `rpm`.
- Raw components installed with `--package` now update from the same package name.
- `anolisa update <component>` now refuses raw updates that would downgrade a component.
- `anolisa update <component>` now refuses raw updates when versions cannot be safely compared.

## [0.1.11] - 2026-06-18

### Added

- `anolisa adopt <component>` can track a pre-installed system RPM without installing it.
- `anolisa repair <component>` can refresh RPM component state after package details change.
- `anolisa forget <component>` can stop tracking a component without removing packages or files.

### Changed

- `anolisa status <component>` now reports drifted RPM components when system package details change.
- `anolisa uninstall` now keeps observed system RPMs unless `--remove-system-package` is used.
- `anolisa install` now preserves adapter package resources when adopting RPM components.

## [0.1.10] - 2026-06-17

### Added

- `anolisa install --backend rpm` can install missing RPM components through `dnf` and track them as managed.
- `anolisa install` can adopt matching pre-installed system RPMs without downloading a raw package.
- `anolisa update <component>` can update RPM-managed and RPM-observed components through `dnf`.
- `anolisa status` now shows package, version, architecture, and source repo for RPM-backed components.
- `anolisa status <component>` now reports matching untracked system RPMs as observed.

### Changed

- `anolisa update runtime <component>` is now `anolisa update <component>`; `self` and `all` stay subcommands.
- `repo.toml` now uses `[backends.rpm]` instead of `[backends.yum]`.
- `anolisa install --all` now lists adopted RPM components in the batch summary.

### Fixed

- `anolisa install --all` now prints the reason for each failed component in human output.
- `anolisa install` now refuses automatic RPM detection when `rpm` or `dnf` is missing, with a `--backend raw` hint.
- `anolisa install` no longer replaces a raw install if another install finishes first.

## [0.1.9] - 2026-06-16

### Added

- `anolisa install --all` can install every available component from the catalog.
- `anolisa install --all --fail-fast` can stop after the first failed component.
- `anolisa install --all --json` returns one batch summary with per-component results.
- `anolisa status` now shows adapter summaries for installed components.

### Changed

- `installed.toml` now distinguishes ANOLISA-managed packages from observed system RPMs.

## [0.1.8] - 2026-06-15

### Added

- `anolisa adapter enable` can now register installed adapters with OpenClaw.
- `anolisa adapter disable` can now remove OpenClaw adapter registrations.
- `anolisa adapter status` can now report OpenClaw adapter health.
- `anolisa adapter scan` can now show installed adapter resources.

### Changed

- `anolisa install` now places adapter resources needed by later enablement.
- `anolisa uninstall` now blocks components that still have enabled adapters.

## [0.1.7] - 2026-06-13

### Changed

- User-mode library paths now resolve to `~/.local/lib/anolisa`; other directories continue to follow `XDG_*` overrides.

### Fixed

- `anolisa install` no longer requires a local catalog entry before downloading from the remote repository.
- `anolisa install --dry-run` can preview files and services without downloading the full package.

## [0.1.6] - 2026-06-12

### Added

- `anolisa osbase sandbox install gvisor` now supports standalone, containerd, and substrate deployments. (#851)
- `anolisa list` can derive the component catalog from `repo.toml` configuration. (#854)

### Changed

- Replaced the legacy "capability" model with a unified component lifecycle; old state auto-migrates on next write. (#876)

### Fixed

- `anolisa list --enabled` now correctly shows installed components instead of an empty list. (#872)
- `anolisa list` no longer requires a separate local catalog file when `repo.toml` is configured. (#854)

## [0.1.5] - 2026-06-11

### Added

- `anolisa list` reads from a remote or local component catalog and returns structured JSON. (#850)
- `anolisa install <component>` downloads, verifies, and installs components from the remote repository. (#852)
- `anolisa uninstall` supports the new component model while preserving legacy fallback. (#852)

### Changed

- Simplified CLI help around `list`, `install`, `uninstall`, `status`, `doctor`, `logs`, `restart`, `update`. (#850)

### Fixed

- `anolisa list` returns an empty list with a config hint when no catalog is configured. (#850)
- Failed installs now automatically roll back partially-written files. (#852)

## [0.1.4] - 2026-06-10

### Added

- `anolisa adapter scan` detects available framework integrations. (#808)
- `anolisa adapter install` downloads verified packages and registers adapters with the target framework.
- `anolisa adapter remove` safely removes only ANOLISA-managed files, with dry-run and JSON preview support.
- `anolisa adapter install tokenless openclaw` wires up the tokenless adapter via the OpenClaw CLI.
- `anolisa enable` fetches component metadata from the remote repository, with offline fallback.
- `anolisa status` now includes component health check results.

### Changed

- Renamed subscription commands to top-level `anolisa register` / `unregister`.

### Fixed

- Adapter install/remove failures now roll back or preserve state for retry.

## [0.1.3] - 2026-06-09

### Added

- `anolisa --help` now groups commands by category (everyday vs. management).
- `list` command shows its `ls` alias in help output.
- `anolisa update self` prints a changelog link on success.

### Changed

- Corrected package license metadata to Apache-2.0.

## [0.1.2] - 2026-06-08

### Added

- `anolisa bug` generates a local diagnostic report with environment info and recent error logs.
- `anolisa self update` added as an alias for `anolisa update self`.

### Fixed

- Restored the bug report issue template.

## [0.1.1] - 2026-06-07

### Added

- `anolisa osbase sandbox install` provisions sandbox environments (firecracker and e2b backends).
- `anolisa register` / `unregister` manages data-upload consent with 30-day deferral.
- `anolisa enable` can configure log upload (ilogtail) with automatic region detection.
- `anolisa update self` downloads and applies CLI updates with integrity verification and rollback.
- Real dnf/apt package manager backends replacing placeholder stubs.
- GitHub Actions CI for the anolisa workspace.

### Fixed

- Install script uses portable bash expansion instead of `sed`.

## [0.1.0] - 2026-06-04

Initial alpha release of the ANOLISA CLI.

### Added

- CLI commands: `env`, `list`, `status`, `logs`, `enable`, `disable`, `uninstall`, `restart`, `update`, `info`, `doctor`.
- Environment detection: OS, arch, kernel, distro, container runtime, user identity (graceful degradation).
- Component lifecycle engine with preview-then-execute, integrity checks, and audit logging.
- Configuration-driven feature gates for shipping new capabilities without code changes.
- Declarative TOML component manifests with multi-architecture support.
- `install-anolisa.sh` installer with three modes (local, checkout, URL), checksum verification, and `--dry-run`.
- End-to-end smoke tests for agent-observability and token-optimization.

### Capabilities shipped

| Capability | Status |
|-----------|--------|
| agent-observability | `enable` fully wired (dry-run + real-execute) |
| Others (9 total) | Manifest-only; `enable` returns NOT_IMPLEMENTED |

### Known limitations

- Real-execute paths are Linux-only (darwin hosts can `--dry-run` only).
- No signature verification or rpm/deb backend yet.
- `update` command returns NOT_IMPLEMENTED.

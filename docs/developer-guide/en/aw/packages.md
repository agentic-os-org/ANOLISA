# AW package and configuration reference

[中文版](../../zh/aw/packages.md)

This reference is for integration and maintenance. For installation and daily use,
see the [user guide](../../../user-guide/en/aw/installation.md).

Use `aw install`, `aw uninstall`, `aw config init`, and `aw config` editing
commands for local package operations. The standalone `aw-package` executable
has been removed; its Rust library remains. Every bundle includes `aw` at its
root for first installation and requires an explicit absolute `--bundle` path.
`aw config validate` uses `aw validate` semantics, including relative paths
and `--against`. The removed editor-specific validation command has no CLI
replacement with the same ownership checks; the Rust editor API retains them.
Native Hook registration is `aw agent connect`; legacy native `aw install
--config ... --agent ...` remains compatible and cannot mix package options.
Installation still accepts only local unpacked Preview bundles, not URLs or
arbitrary named components.

Install AW core to run your existing Qoder or OpenClaw with no Provider. Add sec-core or your own boolean policy command when needed, using one AW configuration. The AW and sec-core components in this Preview ship as native Rust binaries; building, installing and running them do not require Python. Custom Providers or hook scripts have their own runtime dependencies. OpenClaw still needs Node.js, and both Agents need their own model accounts.

This Preview is not yet published through `anolisa install` or RPM. Download
`aw-preview-linux-x86_64-<candidate_sha>` from a successful **CI / AW Integration**
run. The workflow runs nightly on `main` at 03:23 Asia/Shanghai; for a reviewed
source commit or release candidate, run it manually with that commit's full
40-character `candidate_sha`. Wait for the entire workflow, including
**AW / Integration result**, to pass before using its packages. Daily PR checks
do not produce Preview packages. See the
[integration validation instructions](../../../../src/aw/CONTRIBUTING.md#daily-ci-and-nightly-integration)
for manual dispatch and native acceptance boundaries.

For source-to-integration steps, including a backend-free policy, see the
[component walkthrough](integration.md). To use
QwenPaw or Hermes with the same Providers, copy the
[four-framework configuration](native-agents.md#use-one-configuration-for-four-frameworks)
and set the installed binary paths. `aw config init` below generates
Qoder/OpenClaw entries only; it does not initialize native Agent profiles.
Container state and volume requirements are covered in the
[runtime guide](native-agents.md#containers-and-persistent-data).

## 1. Install the package

Use Linux with glibc 2.39 or newer (Ubuntu 24.04), on the matching CPU architecture. CI builds x86_64; the same native build supports aarch64. Extract the downloaded Actions artifact ZIP first.

```bash
sha256sum -c SHA256SUMS
AW_PREVIEW_VERSION=0.1.0-preview.1
AW_PREVIEW_BUNDLE="aw-core-$AW_PREVIEW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_PREVIEW_BUNDLE.tar.gz"
sudo install -d -m 755 /opt/aw-preview
export AW_PREVIEW_PREFIX="/opt/aw-preview/$AW_PREVIEW_VERSION"
sudo "./$AW_PREVIEW_BUNDLE/aw" install --scope system --bundle "$PWD/$AW_PREVIEW_BUNDLE" --prefix "$AW_PREVIEW_PREFIX"
```

These commands use system scope and record packages in `/var/lib/aw/packages`.
Keep `--scope system` on every extension install and uninstall. For a prefix in a
directory you own, omit `sudo` and use `--scope user` throughout instead. The
default is user scope, even under `sudo`; it otherwise writes to the invoking
process's XDG/HOME database. Ordinary-user `aw config init` follows the prefix's
registration and can read a system installation without using root's home.

The default build produces core only; `--component all` produces four packages. `aw-core` installs `bin/aw`; `aw-provider-sec-core` contains the Provider and Rust V2 sec-core CLI/daemon; `aw-provider-tokenless` contains its bridge, Tokenless CLI and pinned patched RTK; `aw-all-in-one` contains exactly the same three components. Every archive also carries a root-level `aw` for bootstrap, including Provider-only archives. For split installation, extract core and Provider and run each bundle's `aw install --scope system --bundle ABS_BUNDLE --prefix ABS_PREFIX` with `sudo` against the same new prefix, **core first**. Use the same scope for both packages. Do not install all-in-one into that prefix afterward.

The installer verifies hashes, modes, architecture, version and source commit, refuses replacements and records ownership in `.aw-packages`. Ctrl-C (SIGINT) or SIGTERM during installation rolls back this invocation’s writes so installation can be retried; forced termination (SIGKILL) is outside automatic rollback. Checksums detect corruption; they are not publisher signatures. Install parents must belong to the installer user and must not be writable by other users. Root owns the system backend installation; when connecting to an existing backend, a user-owned prefix is also supported.

## Use core without a Provider

Install only the Agent you need: Qoder CLI >=1.1.64 retaining the [native command-Hook contract](native-agents.md#build-and-start-qoder), or OpenClaw **2026.9.6** with Node.js. For Qoder, generate a base configuration as your regular user:

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
export AW_QODER=/absolute/path/to/qodercli
export AW_DEMO="$HOME/aw-preview"
install -d -m 700 "$AW_DEMO" "$AW_DEMO/workspace"
"$AW_PREVIEW_PREFIX/bin/aw" config init --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw-core.yaml" --state-dir "$AW_DEMO/core-state" \
  --qoder "$AW_QODER"
"$AW_PREVIEW_PREFIX/bin/aw" validate --config "$AW_DEMO/aw-core.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw-core.yaml" --agent qoder
```

For OpenClaw alone, replace `--qoder "$AW_QODER"` with `--node "$AW_NODE" --openclaw "$AW_OPENCLAW"`; supply the native settings and state options described in step 5 when launching. You can also provide both Agent entrypoints. No sec-core socket or Provider package is required. Empty `providers` and `events` mean no AW policy checks or per-tool AW audit; native permissions and existing Hooks/plugins still apply. AW still creates an instance and starts/reuses its shared service. Exit the Agent and stop that service with `aw stop --config "$AW_DEMO/aw-core.yaml"`.

`aw config init` is the initialization command: after core installation, it creates
a complete `aw.yaml` with Agent, daemon, execution and audit settings. Omitting
`--provider` (or passing `--provider none`) leaves `providers` and `events` empty.
It creates a new file and refuses to overwrite an existing one. To initialize a
policy in one command, select `--provider command` as shown in the
[boolean script guide](native-agents.md#boolean-policy-commands). To add rules later, follow
`aw config init` → `aw config add-provider` → `aw config add-event` →
`aw config add-hook` → `aw config validate`. Viewing, editing and validation
need an existing file; only `init` creates one. Only an absent event needs `aw config add-event`.
For QwenPaw/Hermes or all four targets, copy the
[shared template](integration.md#2-prepare-one-configuration-for-four-frameworks)
and fill its paths instead; `aw config init` currently generates Qoder/OpenClaw
entries only and never initializes framework profiles.

## Edit existing Provider and Hook configuration

Use `aw config show` and the editing commands as the configuration owner to view and change
an existing YAML or JSON file. These commands are offline: they do not execute
Providers or Hooks, install native callbacks, start an Agent or reload a service.
Exit active Agent sessions and stop the service using the original configuration
before changing it, then run the Agent again with the updated file. The new
configuration bytes select a new revision; an existing service does not adopt
edits automatically.

Every action requires `--config` with an absolute, symlink-free file path. Use an
owned regular file without hard links, special permission bits, extended access
ACLs or group/world write permissions; its parent must also be owned and not
group/world writable. Native NFSv4/rich/CIFS access ACLs are unsupported and rejected
even when they are mode-equivalent; use a configuration and parent directory
without these attributes. Inherited native ACLs on staging directories are rejected.
Linux procfs must be mounted. An opened editor stays bound to its parent
directory if an ancestor is renamed; replacing a path cannot redirect its edits.
Unknown, duplicate or incomplete options are errors. Definition files can use
absolute paths or paths relative to the current directory; each must be a
regular file containing exactly one complete YAML/JSON object. Standard input is
not supported. Input and expanded JSON are limited to 4 MiB and depth 32.
Duplicate keys, non-string keys, YAML merge keys/custom tags, non-finite numbers
and multiple documents are rejected. Unknown public fields are rejected while
Provider-owned `config` keys remain opaque.

| Command | Behavior |
| --- | --- |
| `aw config show --config ABS_FILE` | Print the complete working configuration as pretty JSON |
| `aw config show --config ABS_FILE --provider NAME` | Print one Provider |
| `aw config show --config ABS_FILE --event EVENT [--id ID]` | Print one event, or a step with its event-scoped ID |
| `aw config add-provider --config ABS_FILE --name NAME --definition FILE` | Add a complete Provider definition |
| `aw config remove-provider --config ABS_FILE --name NAME` | Remove an unreferenced Provider |
| `aw config add-event --config ABS_FILE --event EVENT --definition FILE` | Add an explicitly configured event |
| `aw config add-hook --config ABS_FILE --event EVENT --definition FILE` | Append a complete step, whose `id` is in the definition |
| `aw config remove-hook --config ABS_FILE --event EVENT --id ID` | Remove one event-scoped step |

`aw config validate --config FILE` separately validates fields and references,
printing `configuration valid` on success. It accepts relative paths and
`--against`; it does not open `ConfigurationEditor` or enforce the file-ownership
checks described above. The former standalone editor validator has been removed.

`show` intentionally includes private Provider values; choose where its output is
displayed or saved. Provider and event selectors are mutually exclusive, and
`--id` requires `--event`. A selected identity that does not exist is an error.
`show` and `aw config validate` leave the source file unchanged.

Add operations preserve the file when an identical definition already exists;
a different definition under the same name or event-scoped step ID is a conflict.
Remove operations preserve the file when the identity is already absent. Successful
mutations print `Updated configuration` or `Configuration unchanged`. Remove all
referencing steps, including disabled steps, before removing their Provider.
Removing an event's last step retains its event options and empty `steps` array.
`aw config add-hook` requires an existing event: `aw config add-event` makes enablement explicit and
never overrides an existing event's options.

Each mutating command publishes one operation atomically. Several commands are
separate publications, so earlier successful commands remain applied if a later
command fails.

For example, add a default boolean before-tool Hook to the empty core
configuration created above. Complete initialization first, then exit any Agent
and stop its service before editing. The demonstration script reads a normalized
event and returns JSON true when input contains `AW_DENY_TEST`; it does not
execute the input. Python 3 is this optional script's dependency. The shell
expands `AW_DEMO` while writing the Provider definition, so the saved argv has an
absolute script path; AW does not interpolate variables in YAML.

```bash
cat > "$AW_DEMO/check-tool.py" <<'PYTHON'
import json
import sys

event = json.load(sys.stdin)
print(json.dumps("AW_DENY_TEST" in json.dumps(event["tool"]["input"], ensure_ascii=False)))
PYTHON
cat > "$AW_DEMO/provider.yaml" <<YAML
transport:
  type: stdio
  location: agent
  argv: [/usr/bin/python3, "$AW_DEMO/check-tool.py"]
timeout_ms: 1000
max_output_bytes: 32
YAML
cat > "$AW_DEMO/event.yaml" <<'YAML'
enabled: true
required: true
steps: []
YAML
cat > "$AW_DEMO/hook.yaml" <<'YAML'
id: check-marker
provider: company
effects: [block]
reason_code: demo_marker_denied
on_error: block
YAML
chmod 600 "$AW_DEMO/check-tool.py" "$AW_DEMO/provider.yaml" "$AW_DEMO/event.yaml" "$AW_DEMO/hook.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" config add-provider --config "$AW_DEMO/aw-core.yaml" \
  --name company --definition "$AW_DEMO/provider.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" config add-event --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/event.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" config add-hook --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --definition "$AW_DEMO/hook.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" config show --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --id check-marker
"$AW_PREVIEW_PREFIX/bin/aw" config validate --config "$AW_DEMO/aw-core.yaml"
```

The omitted Provider `protocol` selects `aw-hook/v1alpha1`, and the omitted step
`operation` defaults to `invoke`. AW calls the script only for admitted events;
it needs no method dispatcher or response object. True requests the step's
`block` effect and reason code; false requests no effect. Errors follow
`on_error: block`. Replace the script with your own boolean rule. Existing
native scripts use the explicit [native Hook form](native-agents.md#connect-your-programs).
Use the `aw run` command above with the edited configuration to test native
behavior; offline validation does not execute the script.

To remove this demonstration policy later, stop its Agent/service first, remove
the referencing step, then its Provider. The empty event is retained. The script
remains on disk until you finish using it or clean up the demo directory.

```bash
"$AW_PREVIEW_PREFIX/bin/aw" config remove-hook --config "$AW_DEMO/aw-core.yaml" \
  --event tool.before --id check-marker
"$AW_PREVIEW_PREFIX/bin/aw" config remove-provider --config "$AW_DEMO/aw-core.yaml" \
  --name company
rm "$AW_DEMO/provider.yaml" "$AW_DEMO/event.yaml" "$AW_DEMO/hook.yaml"
```

A real change atomically replaces the file with serialized YAML, retaining
permission bits, group ownership and unrelated values, but rewriting comments, formatting and key
order. Extended attributes and ACLs are not preserved. Real changes require file
write access and an exclusive lock; no-ops use a read-only descriptor and shared
lock, retaining exact bytes and inode. Validation errors, conflicts, outstanding references, lock contention
and staging failures preserve the existing file. Concurrent changes cause a stale
snapshot error rather than an automatic merge or retry. Other writers must honor
the same file lock to avoid a race at publication. Save a copy before editing if
you need the previous text for rollback; this is atomic publication rather than a
power-loss durability guarantee. Passing validation establishes static references
and field shape, not Provider admission or native effect adoption.

## 2. Optionally install and start sec-core

Skip steps 2–5 for core-only use. For the security demo, install the matching extension into the core prefix first:

```bash
AW_PREVIEW_VERSION=0.1.0-preview.1
AW_PREVIEW_PROVIDER="aw-provider-sec-core-$AW_PREVIEW_VERSION-linux-$(uname -m)"
export AW_PREVIEW_PREFIX="/opt/aw-preview/$AW_PREVIEW_VERSION"
tar -xzf "$AW_PREVIEW_PROVIDER.tar.gz"
sudo "./$AW_PREVIEW_PROVIDER/aw" install --scope system --bundle "$PWD/$AW_PREVIEW_PROVIDER" --prefix "$AW_PREVIEW_PREFIX"
```

In a separate terminal, start the installed backend as root. Keep it in the foreground; Ctrl-C stops it. Configuration and data stay outside the immutable package prefix.

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
sudo install -d -m 755 /run/aw-preview-sec /etc/aw-preview
sudo install -d -m 700 /var/lib/aw-preview
printf '{"stateDir":"/var/lib/aw-preview/skillsec"}\n' | sudo tee /etc/aw-preview/skillsec.json
sudo chmod 600 /etc/aw-preview/skillsec.json
sudo env AGENT_SEC_DATA_DIR=/var/lib/aw-preview/sec-data OTEL_SDK_DISABLED=true \
  "$AW_PREVIEW_PREFIX/libexec/aw/providers/sec-core/agent-sec-daemon" serve \
  --socket /run/aw-preview-sec/daemon.sock --skillsec-config /etc/aw-preview/skillsec.json
```

The package also contains the authenticated `agent-sec-daemon-health` helper and
official service/configuration reference assets listed in its installation record. For a persistent
backend, reuse the existing root system service or install the independently
managed sec-core system package. AW install/uninstall does not change that
service or `/etc`; do not register a root service against a user-writable prefix.
See [backend ownership and health diagnostics](sec-core.md#packaged-backend-and-explicit-health-diagnostics).

## 3. Create one AW configuration

Install Qoder CLI >=1.1.64, OpenClaw **2026.9.6** and a compatible Node.js runtime separately.
Qoder must retain `--settings`, exec-form command Hooks with literal `args`,
`PreToolUse`/`PostToolUse`, callback IDs/input and exit 2 for blocking, as described
in the [Qoder support contract](native-agents.md#build-and-start-qoder). AW records its successfully
probed, nonempty version (at most 128 UTF-8 bytes) with 1.1.64 as the minimum semantic version; the
version probe does not certify native effect adoption. OpenClaw retains its version
check. Set the following absolute paths to your actual installations. Run the
remaining steps as your regular user.

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
export AW_QODER=/absolute/path/to/qodercli
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw/openclaw.mjs
export AW_DEMO="$HOME/aw-preview"
install -d -m 700 "$AW_DEMO" "$AW_DEMO/workspace" "$AW_DEMO/openclaw"
"$AW_PREVIEW_PREFIX/bin/aw" config init --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw.yaml" --state-dir "$AW_DEMO/state" \
  --provider sec-core --socket /run/aw-preview-sec/daemon.sock --qoder "$AW_QODER" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
```

Qoder and Node entrypoints must be executable by the current user; the OpenClaw `.mjs` file must be readable and does not need execute permission. Keep the configuration and socket paths short enough for Unix sockets (less than 108 bytes). The configuration file must not be the state directory or one of its ancestors; an existing state path must be a directory owned by the current user with mode `0700`. `aw config init` holds a shared package lock through validation and publication, including for a regular user reading a root-owned prefix. An active installation or uninstall causes a retryable lock error. It rechecks core and the explicitly selected Provider file hashes and permission modes, refusing missing or modified required payloads. A Provider installed but not selected adds no initialization dependency. Without `--provider`, configuration has no policy; `--socket` requires explicit `--provider sec-core` so old security commands cannot silently generate an unprotected configuration. It writes a private temporary file before exclusively publishing the configuration; a failed write leaves no partial configuration and can be retried. Both Agent bindings share the same Provider and before/after steps: Qoder `Bash` and OpenClaw `exec` both map their `/command` input to the sec-core Bash scanner. A risky command or a failed before-check blocks execution; after-checks observe the results reported by each framework (OpenClaw also reports blocked tool results). Other tools are not scanned. This provides native Hook enforcement, not an OS sandbox or cross-framework approval service.

## 4. Test Qoder

Log in through Qoder first if needed. In the new workspace, run two separate sessions. Each prompt requests exactly one tool call and prohibits retrying or substituting another command.

```bash
cd "$AW_DEMO/workspace"
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw.yaml" --agent qoder -- \
  -p 'Use Bash exactly once to execute `printf AW_QODER_OK > qoder-allowed.marker`. Report the tool result; do not retry or use another tool.' \
  --dangerously-skip-permissions --no-session-persistence
cat qoder-allowed.marker
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw.yaml" --agent qoder -- \
  -p 'Use Bash exactly once to execute `git -c http.sslVerify=false --version > qoder-blocked.marker`. Report the tool result; do not retry or use another tool.' \
  --dangerously-skip-permissions --no-session-persistence
test ! -e qoder-blocked.marker
```

The first command writes `AW_QODER_OK`. The second must report `Tool blocked by AW policy` and create no blocked marker. The Git command only prints a version: it performs no network request and changes no Git configuration, even if interception fails. Permission bypass is confined to these controlled demo sessions so native approval does not obscure the AW result; it is not required for everyday use. A model refusal, account error or lack of a tool invocation is not a successful block.

## 5. Test OpenClaw with the same file

In both the Gateway and client terminals, repeat the exports from step 3 using the same paths, then set the following profile variables. Use an isolated native profile so existing Agent settings are preserved. Run OpenClaw onboarding once to configure a real model account; skip daemon installation, channels and skills. This native file holds model/Gateway settings, not a second AW policy.

```bash
export OPENCLAW_STATE_DIR="$AW_DEMO/openclaw"
export OPENCLAW_CONFIG_PATH="$AW_DEMO/openclaw/openclaw.json"
"$AW_NODE" "$AW_OPENCLAW" onboard --classic --mode local \
  --gateway-bind loopback --gateway-port 18799 --no-install-daemon \
  --skip-channels --skip-skills --skip-health --skip-ui \
  --workspace "$AW_DEMO/workspace"
"$AW_NODE" "$AW_OPENCLAW" config set tools.allow '["exec"]' --strict-json
"$AW_NODE" "$AW_OPENCLAW" config set tools.exec.mode full
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw.yaml" --agent openclaw \
  --native-settings "$OPENCLAW_CONFIG_PATH" --native-state-dir "$OPENCLAW_STATE_DIR"
```

Keep this terminal open until it reports `AW openclaw native hooks ready`. In another terminal with the same variables, send the two turns through this Gateway (do not use `--local`, which bypasses the Gateway).

```bash
"$AW_NODE" "$AW_OPENCLAW" agent --session-id aw-allow-$(date +%s) --json \
  --message 'Use exec exactly once to execute `printf AW_OPENCLAW_OK > openclaw-allowed.marker`. Report the tool result; do not retry or use another tool.'
cat "$AW_DEMO/workspace/openclaw-allowed.marker"
"$AW_NODE" "$AW_OPENCLAW" agent --session-id aw-block-$(date +%s) --json \
  --message 'Use exec exactly once to execute `git -c http.sslVerify=false --version > openclaw-blocked.marker`. Report the tool result; do not retry or use another tool.'
test ! -e "$AW_DEMO/workspace/openclaw-blocked.marker"
"$AW_PREVIEW_PREFIX/bin/aw" status --config "$AW_DEMO/aw.yaml"
```

The first turn writes `AW_OPENCLAW_OK`; the second must report an AW policy block without creating its marker. Together with Qoder, this demonstrates the same `aw.yaml` and sec-core backend enforcing both native tool paths. AW keeps metadata-only audit records in the external state directory, including across restarts. The Agent profiles remain separate because their model credentials and sessions belong to their respective frameworks.

## 6. Stop, upgrade or uninstall

Exit Qoder and stop the foreground OpenClaw Gateway with Ctrl-C, then stop AW. Stop the sec-core terminal with Ctrl-C as well.

```bash
"$AW_PREVIEW_PREFIX/bin/aw" stop --config "$AW_DEMO/aw.yaml"
sudo "$AW_PREVIEW_PREFIX/bin/aw" uninstall --scope system --prefix "$AW_PREVIEW_PREFIX" --yes
```

Uninstall preflights all owned files and refuses modified or missing payloads before deleting anything. If deletion fails or SIGINT/SIGTERM arrives before completion, it restores the files and installation records already removed so you can retry. SIGKILL is outside automatic restoration. If restoration also fails, diagnostics identify the retained backup and unrestored paths. Once all payloads and installation records are removed, uninstall succeeds even if backup cleanup fails; a warning identifies the remaining backup for manual removal. Unknown files, the prefix/lock, external configuration, credentials and audit data remain. Remove those only after checking that you no longer need them.

For upgrades, install into a new prefix and generate a new configuration. Keep the old package and configuration for rollback. This Preview has no hot reload or audit format migration; back up state and use a new state directory if a later Preview changes its format.

## Build artifacts (developers)

From a clean committed checkout, install Rust 1.97.1, a C compiler, pkg-config and OpenSSL development headers. No Python is needed by these commands. Use an empty absolute output directory.

```bash
cargo test --locked --manifest-path src/aw/Cargo.toml -p aw-package
cargo run --locked --release --manifest-path src/aw/Cargo.toml \
  -p aw-package --bin aw-build -- --version 0.1.0-preview.1 \
  --output "$PWD/target/aw-preview-packages"
```

The Rust builder compiles locked release binaries, checks native ELF architecture, and generates only the selected archives and `SHA256SUMS`. The default is `--component core`, which never builds or reads sec-core; use `--component sec-core` for the extension or `--component all` for all four packages. The extension still requires core from the same source commit and version at installation. Failed or cancelled publication removes only the links created by that run, allowing retry; cleanup failures report exact remaining paths. The installed-package CI test exercises the real sec-core scanner, both tool mappings and audit recovery with synthetic events; it does not claim native Agent coverage. Native model turns remain explicit acceptance as above. The general AW development gate retains Python test tooling outside the delivery path.

CI / AW Integration runs nightly and can be dispatched manually for an exact source commit; PRs do not automatically run package acceptance. Package artifacts are retained for three days for both triggers. Distribute packages only after the full integration result passes; failed jobs retain diagnostic evidence. Archives are rebuildable but not guaranteed byte-identical; a unified release channel is future work.

## Optional Tokenless PostTool package

`--component tokenless` builds `aw-provider-tokenless`; `--component all` also
includes it in `aw-all-in-one`. The extension contains its bridge, Tokenless CLI and pinned patched RTK,
requires matching core, and follows the same installation-record and integrity rules as sec-core.
Use `aw config init --provider tokenless` or `aw config init --provider sec-core+tokenless --socket ABS_FILE`
to enable compression explicitly. See the [Tokenless guide](../../../user-guide/en/aw/tokenless.md) for
four-framework configuration, native result slots and failure behavior.

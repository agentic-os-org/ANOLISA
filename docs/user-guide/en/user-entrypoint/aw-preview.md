# Install and use AW Preview

[中文版](../../zh/user-entrypoint/aw-preview.md)

Install AW core to run your existing Qoder or OpenClaw with no Provider. Add sec-core or your own boolean policy command when needed, using one AW configuration. The AW and sec-core components in this Preview ship as native Rust binaries; building, installing and running them do not require Python. Custom Providers or hook scripts have their own runtime dependencies. OpenClaw still needs Node.js, and both Agents need their own model accounts.

This Preview is not yet published through `anolisa install` or RPM. Download artifacts from a successful manual **CI / AW Packages** run of the reviewed source commit. PR artifacts ending in `-validation` are merge-candidate checks, not distributable releases.

## 1. Install the package

Use Linux with glibc 2.39 or newer (Ubuntu 24.04), on the matching CPU architecture. CI builds x86_64; the same native build supports aarch64. Extract the downloaded Actions artifact ZIP first.

```bash
sha256sum -c SHA256SUMS
AW_PREVIEW_VERSION=0.1.0-preview.1
AW_PREVIEW_BUNDLE="aw-core-$AW_PREVIEW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_PREVIEW_BUNDLE.tar.gz"
sudo install -d -m 755 /opt/aw-preview
export AW_PREVIEW_PREFIX="/opt/aw-preview/$AW_PREVIEW_VERSION"
sudo "./$AW_PREVIEW_BUNDLE/aw-package" install --prefix "$AW_PREVIEW_PREFIX"
```

The default build produces core only; `--component all` produces three packages. `aw-core` contains `aw` and `aw-package`; `aw-provider-sec-core` contains the Provider and Rust V2 sec-core CLI/daemon; `aw-all-in-one` contains exactly the same two components. For split installation, extract core and Provider and run each bundle’s `aw-package install` against the same new prefix, **core first**. Do not install all-in-one into that prefix afterward.

The installer verifies hashes, modes, architecture, version and source commit, refuses replacements and records ownership in `.aw-packages`. Ctrl-C (SIGINT) or SIGTERM during installation rolls back this invocation’s writes so installation can be retried; forced termination (SIGKILL) is outside automatic rollback. Checksums detect corruption; they are not publisher signatures. Install parents must belong to the installer user and must not be writable by other users. Root owns the system backend installation; when connecting to an existing backend, a user-owned prefix is also supported.

## Use core without a Provider

Install only the Agent you need: Qoder CLI **1.1.64**, or OpenClaw **2026.9.6** with Node.js. For Qoder, generate a base configuration as your regular user:

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
export AW_QODER=/absolute/path/to/qodercli
export AW_DEMO="$HOME/aw-preview"
install -d -m 700 "$AW_DEMO" "$AW_DEMO/workspace"
"$AW_PREVIEW_PREFIX/bin/aw-package" configure --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw-core.yaml" --state-dir "$AW_DEMO/core-state" \
  --qoder "$AW_QODER"
"$AW_PREVIEW_PREFIX/bin/aw" validate --config "$AW_DEMO/aw-core.yaml"
"$AW_PREVIEW_PREFIX/bin/aw" run --config "$AW_DEMO/aw-core.yaml" --agent qoder
```

For OpenClaw alone, replace `--qoder "$AW_QODER"` with `--node "$AW_NODE" --openclaw "$AW_OPENCLAW"`; supply the native settings and state options described in step 5 when launching. You can also provide both Agent entrypoints. No sec-core socket or Provider package is required. Empty `providers` and `events` mean no AW policy checks or per-tool AW audit; native permissions and existing Hooks/plugins still apply. AW still creates an instance and starts/reuses its shared service. Exit the Agent and stop that service with `aw stop --config "$AW_DEMO/aw-core.yaml"`.

## 2. Optionally install and start sec-core

Skip steps 2–5 for core-only use. For the security demo, install the matching extension into the core prefix first:

```bash
AW_PREVIEW_VERSION=0.1.0-preview.1
AW_PREVIEW_PROVIDER="aw-provider-sec-core-$AW_PREVIEW_VERSION-linux-$(uname -m)"
export AW_PREVIEW_PREFIX="/opt/aw-preview/$AW_PREVIEW_VERSION"
tar -xzf "$AW_PREVIEW_PROVIDER.tar.gz"
sudo "./$AW_PREVIEW_PROVIDER/aw-package" install --prefix "$AW_PREVIEW_PREFIX"
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

## 3. Create one AW configuration

Install Qoder CLI **1.1.64**, OpenClaw **2026.9.6** and a compatible Node.js runtime separately; the launcher verifies the pinned Agent versions. Set the following absolute paths to your actual installations. Run the remaining steps as your regular user.

```bash
export AW_PREVIEW_PREFIX="/opt/aw-preview/0.1.0-preview.1"
export AW_QODER=/absolute/path/to/qodercli
export AW_NODE=/absolute/path/to/node
export AW_OPENCLAW=/absolute/path/to/openclaw/openclaw.mjs
export AW_DEMO="$HOME/aw-preview"
install -d -m 700 "$AW_DEMO" "$AW_DEMO/workspace" "$AW_DEMO/openclaw"
"$AW_PREVIEW_PREFIX/bin/aw-package" configure --prefix "$AW_PREVIEW_PREFIX" \
  --config "$AW_DEMO/aw.yaml" --state-dir "$AW_DEMO/state" \
  --provider sec-core --socket /run/aw-preview-sec/daemon.sock --qoder "$AW_QODER" \
  --node "$AW_NODE" --openclaw "$AW_OPENCLAW"
```

Qoder and Node entrypoints must be executable by the current user; the OpenClaw `.mjs` file must be readable and does not need execute permission. Keep the configuration and socket paths short enough for Unix sockets (less than 108 bytes). The configuration file must not be the state directory or one of its ancestors; an existing state path must be a directory owned by the current user with mode `0700`. `configure` holds a shared package lock through validation and publication, including for a regular user reading a root-owned prefix. An active installation or uninstall causes a retryable lock error. It rechecks core and the explicitly selected Provider file hashes and permission modes, refusing missing or modified required payloads. A Provider installed but not selected adds no initialization dependency. Without `--provider`, configuration has no policy; `--socket` requires explicit `--provider sec-core` so old security commands cannot silently generate an unprotected configuration. It writes a private temporary file before exclusively publishing the configuration; a failed write leaves no partial configuration and can be retried. Both Agent bindings share the same Provider and before/after steps: Qoder `Bash` and OpenClaw `exec` both map their `/command` input to the sec-core Bash scanner. A risky command or a failed before-check blocks execution; after-checks observe the results reported by each framework (OpenClaw also reports blocked tool results). Other tools are not scanned. This provides native Hook enforcement, not an OS sandbox or cross-framework approval service.

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
sudo "$AW_PREVIEW_PREFIX/bin/aw-package" uninstall --prefix "$AW_PREVIEW_PREFIX"
```

Uninstall preflights all owned files and refuses modified or missing payloads before deleting anything. If deletion fails or SIGINT/SIGTERM arrives before completion, it restores the files and receipts already removed so you can retry. SIGKILL is outside automatic restoration. If restoration also fails, diagnostics identify the retained backup and unrestored paths. Once all payloads and receipts are removed, uninstall succeeds even if backup cleanup fails; a warning identifies the remaining backup for manual removal. Unknown files, the prefix/lock, external configuration, credentials and audit data remain. Remove those only after checking that you no longer need them.

For upgrades, install into a new prefix and generate a new configuration. Keep the old package and configuration for rollback. This Preview has no hot reload or audit format migration; back up state and use a new state directory if a later Preview changes its format.

## Build artifacts (developers)

From a clean committed checkout, install Rust 1.97.1, a C compiler, pkg-config and OpenSSL development headers. No Python is needed by these commands. Use an empty absolute output directory.

```bash
cargo test --locked --manifest-path src/aw/Cargo.toml -p aw-package
cargo run --locked --release --manifest-path src/aw/Cargo.toml \
  -p aw-package --bin aw-build -- --version 0.1.0-preview.1 \
  --output "$PWD/target/aw-preview-packages"
```

The Rust builder compiles locked release binaries, checks native ELF architecture, and generates only the selected archives and `SHA256SUMS`. The default is `--component core`, which never builds or reads sec-core; use `--component sec-core` for the extension or `--component all` for all three packages. The extension still requires core from the same source commit and version at installation. Failed or cancelled publication removes only the links created by that run, allowing retry; cleanup failures report exact remaining paths. The installed-package CI test exercises the real sec-core scanner, both tool mappings and audit recovery with synthetic events; it does not claim native Agent coverage. Native model turns remain explicit acceptance as above. The general AW development gate retains Python test tooling outside the delivery path.

Package CI runs for changes to the package crate, AW manifests, this guide and its workflow. Changes elsewhere in AW/sec-core require a manual run on the exact source commit before distribution. Successful PR artifacts are retained for three days; manual artifacts for fourteen. Failed runs retain diagnostics but do not publish packages. Archives are rebuildable but not guaranteed byte-identical; a unified release channel is future work.

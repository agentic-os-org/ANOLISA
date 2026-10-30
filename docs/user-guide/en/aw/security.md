# Use sec-core

[中文版](../../zh/aw/security.md)

sec-core checks commands before tool execution. AW can block a call when it finds a risk or a
required check fails. The distribution template selects Qoder `Bash` and
OpenClaw `exec` with `native:qoder:Bash` and `native:openclaw:exec`. Scanning other tools requires explicit
selectors and input mappings. This uses native Hooks and does not provide an OS sandbox.

## Prepare programs and backend

Install the backend using the [sec-core guide](../agent-sec-core/index.md), including
its CLI, daemon and service lifecycle. Reuse its actual socket, normally
`/run/agent-sec-core/daemon.sock`. The AW Provider does not contain or start the
backend. Root daemons must run from root-controlled backend installations.

After [installing the AW CLI](installation.md), add the Provider bridge:

```bash
export AW_VERSION=0.1.0
export AW_SEC_BUNDLE="aw-provider-sec-core-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_SEC_BUNDLE.tar.gz"
export AW_EXTENSION_PREFIX="$HOME/.local/lib/aw/extensions"
install -d -m 700 "$HOME/.local/lib/aw"
aw install --prefix "$AW_EXTENSION_PREFIX" --bundle "$PWD/$AW_SEC_BUNDLE"
export AW_TEMPLATE_DIR="$PWD/$AW_SEC_BUNDLE/templates"
```

These commands use a user-owned extension prefix. A root-managed extension prefix
requires explicit `--scope system` and root privileges for writes.

## Generate configuration and run

Keep `AW_HOME` and `AW_QODER` from installation. Set the CLI and socket to the
backend's actual locations; keep the unpacked template available:

```bash
export AW_SEC_CLI="$(command -v agent-sec-cli)"
export AW_SEC_SOCKET=/run/agent-sec-core/daemon.sock
export AW_PROVIDERS_DIR="$AW_EXTENSION_PREFIX/libexec/aw/providers"
aw config init --prefix "$AW_EXTENSION_PREFIX" \
  --config "$AW_HOME/security.yaml" --state-dir "$AW_HOME/security-state" \
  --template "$AW_TEMPLATE_DIR/sec-core.json" \
  --template-var "PROVIDERS_DIR=$AW_PROVIDERS_DIR" \
  --template-var "SEC_CORE_CLI=$AW_SEC_CLI" --template-var "SOCKET=$AW_SEC_SOCKET" \
  --qoder "$AW_QODER"
aw validate --config "$AW_HOME/security.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/security.yaml" --agent qoder
```

The template requires `PROVIDERS_DIR`, `SEC_CORE_CLI` and `SOCKET`.
For native or Makefile installations, set `PROVIDERS_DIR` to the actual installed
Provider root, such as `/usr/libexec/aw/providers` or `/usr/local/libexec/aw/providers`. For compression too, install
the Tokenless backend and bridge, then select `sec-core-tokenless.json` with the
three additional backend values described in [Tokenless](tokenless.md).
OpenClaw uses its [corresponding launch arguments](configuration.md#openclaw).

A Provider delivered by RPM or another owner can instead be referenced directly
in YAML through its absolute `transport.argv`.

## Verify checks

Ask the native Agent to execute these commands in separate requests, keeping native permission
confirmation enabled.

```bash
printf AW_SECURITY_OK
git -c http.sslVerify=false --version
```

The first should return normally. The second only prints Git's version without accessing the network
or changing configuration, but its TLS-verification disabling option should match the rule and
produce an AW policy denial. A model declining or never calling the tool is not a successful AW
check; inspect native prompts when the check did not run.

If checks fail, verify the sec-core backend and socket using the
[backend diagnostics](../../../developer-guide/en/aw/providers/sec-core.md#external-backend-and-explicit-health-diagnostics).
Generated configuration blocks selected tools on input or scan errors; unselected tools skip scanning.

## Stop and extend

After exiting the Agent, run `aw stop --config "$AW_HOME/security.yaml"`.
Backend services remain managed through the sec-core installation guide.
Uninstalling AW preserves external backend configuration and data.

To scan other tools, update both the [step selector](configuration.md#select-tools-for-a-hook-step) and [Provider input mapping](../../../developer-guide/en/aw/providers/sec-core.md#configure-a-provider).

# Use sec-core

[中文版](../../zh/aw/security.md)

sec-core checks commands before tool execution. AW can block a call when it finds a risk or a
required check fails. Generated configuration checks Qoder `Bash` and OpenClaw `exec`;
other tools need explicit mappings. This uses native Hooks and does not provide an OS sandbox.

## Prepare programs and backend

After [installing core](installation.md), install the sec-core extension from the extracted
artifact directory. Skip this step if all-in-one is installed.

```bash
export AW_VERSION=0.1.0-preview.1
export AW_SEC_BUNDLE="aw-provider-sec-core-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_SEC_BUNDLE.tar.gz"
sudo "./$AW_SEC_BUNDLE/aw" install --scope system --bundle "$PWD/$AW_SEC_BUNDLE" --prefix "$AW_PREFIX"
```

This continues the installation page's system scope. For a user-owned core prefix,
omit `sudo` and use `--scope user` for the extension too. Keep the same scope for
core, every extension and uninstall; `sudo` does not select system scope.

AW does not start sec-core automatically.
Reuse an existing managed backend socket, such as `/run/agent-sec-core/daemon.sock`.
For sustained use, follow the [sec-core system service guide](../agent-sec-core/index.md).

For a first local trial, you can start the packaged backend in a separate terminal. These commands
require a root-managed AW installation under `/opt/aw-preview`. The configuration file must be
new for this trial; preserve a backup if it already exists.

```bash
export AW_PREFIX=/opt/aw-preview/0.1.0-preview.1
sudo install -d -m 755 /run/aw-preview-sec /etc/aw-preview
sudo install -d -m 700 /var/lib/aw-preview
printf '{"stateDir":"/var/lib/aw-preview/skillsec"}\n' | sudo tee /etc/aw-preview/skillsec.json
sudo chmod 600 /etc/aw-preview/skillsec.json
sudo env AGENT_SEC_DATA_DIR=/var/lib/aw-preview/sec-data OTEL_SDK_DISABLED=true \
  "$AW_PREFIX/libexec/aw/providers/sec-core/agent-sec-daemon" serve \
  --socket /run/aw-preview-sec/daemon.sock --skillsec-config /etc/aw-preview/skillsec.json
```

Keep the terminal running and use Ctrl-C to stop it after the trial. A root backend must not execute
programs from a user-writable prefix.

## Generate configuration and run

Return to the ordinary-user terminal with `AW_PREFIX`, `AW_HOME` and `AW_QODER` from installation.
Set `AW_SEC_SOCKET` to the actual backend; this example uses the socket started above.

```bash
export AW_SEC_SOCKET=/run/aw-preview-sec/daemon.sock
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/security.yaml" --state-dir "$AW_HOME/security-state" \
  --provider sec-core --socket "$AW_SEC_SOCKET" --qoder "$AW_QODER"
aw validate --config "$AW_HOME/security.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/security.yaml" --agent qoder
```

For compression too, install Tokenless and select `--provider sec-core+tokenless`.
OpenClaw uses its [corresponding launch arguments](configuration.md#openclaw).

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

The generated safety configuration blocks calls when the backend connection fails.
You can explicitly check backend connectivity.

```bash
export AW_SEC_PROVIDER="$AW_PREFIX/libexec/aw/providers/sec-core"
"$AW_SEC_PROVIDER/aw-provider-sec-core" --health \
  --helper "$AW_SEC_PROVIDER/agent-sec-daemon-health" --socket "$AW_SEC_SOCKET"
```

`backend: authenticated_reachable` means the backend can be reached; it does not replace the
result of checking an actual tool call.

## Stop and extend

After exiting the Agent, run `aw stop --config "$AW_HOME/security.yaml"`.
Stop the foreground trial backend with Ctrl-C; shared system services remain managed by their owner.
Uninstalling AW preserves external backend configuration and data.

For other tools or observation without blocking, see
[tool mappings and scan modes](../../../developer-guide/en/aw/sec-core.md#configure-a-provider).
Unmapped tools produce observation records without being scanned.

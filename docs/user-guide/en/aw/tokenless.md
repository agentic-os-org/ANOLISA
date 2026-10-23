# Use Tokenless

[中文版](../../zh/aw/tokenless.md)

Tokenless compresses text returned by tools to reduce subsequent model context.
It can also suggest commands that produce more concise output. Safety checks and native Agent
permissions remain in effect.

## Install and enable

Install AW core and `aw-provider-tokenless` from the same build, or install all-in-one.
Prepare `AW_PREFIX`, `AW_HOME` and `AW_QODER` using the [installation page](installation.md).
For split packages, add Tokenless from the extracted artifact directory.

```bash
export AW_VERSION=0.1.0-preview.1
export AW_TOKENLESS_BUNDLE="aw-provider-tokenless-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_TOKENLESS_BUNDLE.tar.gz"
sudo "./$AW_TOKENLESS_BUNDLE/aw" install --scope system --bundle "$PWD/$AW_TOKENLESS_BUNDLE" --prefix "$AW_PREFIX"
```

This continues the installation page's system scope. For a user-owned core prefix,
omit `sudo` and use `--scope user` for the extension too. Keep the same scope for
core, every extension and uninstall; `sudo` does not select system scope.

Skip this step if all-in-one is installed. The package includes Tokenless CLI and the required RTK.
There is no separate RTK, TOON, Python wheel or native Tokenless plugin to install.
Avoid enabling an overlapping Tokenless plugin in the same session to prevent duplicate processing.

```bash
aw config init --prefix "$AW_PREFIX" \
  --config "$AW_HOME/tokenless.yaml" --state-dir "$AW_HOME/tokenless-state" \
  --provider tokenless --qoder "$AW_QODER"
aw validate --config "$AW_HOME/tokenless.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/tokenless.yaml" --agent qoder
```

For OpenClaw, use its [Agent arguments](configuration.md#openclaw). To also check safety,
install and start [sec-core](security.md) and select
`--provider sec-core+tokenless --socket /run/agent-sec-core/daemon.sock`. Use the socket path of your running backend.
For other frameworks and SDKs, see the [integration reference](../../../developer-guide/en/aw/tokenless.md).

## Runtime behavior

- Before execution, AW may block the original call with a command retry suggestion. The Agent's new call passes through safety and native permission checks again.
- After execution, supported successful text results can be compressed. Failed results, images and unsupported content stay unchanged.
- Compression errors, timeouts or no savings preserve the original result. Compression cannot override a safety denial.

Generated configuration uses the packaged RTK. Missing or invalid files produce an error;
AW does not substitute a user-installed RTK. The current AW integration has no recovery entrypoint
for compressed results.

## Check results

Ask the Agent to run a tool that returns text, such as reading a file or checking Git status.
In another terminal with the same `AW_PREFIX`, `PATH` and `AW_HOME`, inspect the summary.

```bash
aw inspect --config "$AW_HOME/tokenless.yaml" --json
```

Check the Tokenless card and recent step records. `no_data` means no statistics are available;
`unsupported` means the configuration has no queryable metrics. On errors, check the Provider
and data directory first. Newly generated Tokenless configuration includes query settings;
for older files, see the [query reference](../../../developer-guide/en/aw/tokenless.md#read-only-panel-queries).

Tokenless statistics identify their source using the Agent name configured in AW,
the name selected by `aw run --agent`. For example, `qoder-work` and `qoder-test`
are identified separately even if both use Qoder. Reusing the same name across
configurations can still aggregate their records.

Statistics estimate candidate compression savings over recent records and may include other
integrations. They are not the current session's billing savings. Check the Agent's actual results
to confirm adoption into model context.

## Data and privacy

AW audit stores metadata only; Tokenless uses a separate data directory. Its default statistics
may retain complete before/after tool text. Local SLS logs contain metrics and attribution;
an existing telemetry collector may collect those logs separately.

Agent names may appear in statistics and logs. Avoid sensitive information in these names,
and check the names and content before sharing statistics or logs.

If recording is unnecessary, set these variables before launching AW. If a service is already
running, exit its Agents and stop it first.

```bash
export TOKENLESS_STATS_ENABLED=0
export TOKENLESS_SLS_ENABLED=0
```

Disabling recording does not delete history or disable compression. See
[Tokenless configuration and privacy](../tokenless/configuration-and-privacy.md) for more settings.

## Disable

Exit the Agent, then stop the service.

```bash
aw stop --config "$AW_HOME/tokenless.yaml"
```

Remove Tokenless steps and any unused Provider from the configuration, validate and relaunch.
Keep the data directory as needed; disabling the feature does not delete statistics.

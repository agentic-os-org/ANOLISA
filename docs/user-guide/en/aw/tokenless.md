# Use Tokenless

[中文版](../../zh/aw/tokenless.md)

Tokenless compresses tool text to reduce subsequent model context and can suggest
commands that produce more concise output. Safety checks and native permissions
remain in effect.

## Install and enable

Install the Tokenless backend CLI and its compatible RTK using the
[Tokenless guide](../tokenless/index.md). This Provider requires patched RTK 0.49.0;
use the backend owner's matching release. The AW Provider bundle contains only
the bridge, examples and template, not Tokenless CLI or RTK.

Prepare the AW CLI, `AW_HOME` and `AW_QODER` using [installation](installation.md),
then install the bridge into its own extension prefix:

```bash
export AW_VERSION=0.1.0
export AW_TOKENLESS_BUNDLE="aw-provider-tokenless-$AW_VERSION-linux-$(uname -m)"
tar -xzf "$AW_TOKENLESS_BUNDLE.tar.gz"
export AW_EXTENSION_PREFIX="$HOME/.local/lib/aw/extensions"
install -d -m 700 "$HOME/.local/lib/aw"
aw install --prefix "$AW_EXTENSION_PREFIX" --bundle "$PWD/$AW_TOKENLESS_BUNDLE"
export AW_TEMPLATE_DIR="$PWD/$AW_TOKENLESS_BUNDLE/templates"
```

These examples use user scope. Root-managed extension writes require explicit
`--scope system` and root privileges.
Avoid enabling an overlapping native Tokenless plugin in the same session.

Select the actual backend programs and compute the checksum of the selected RTK.
Resolve any symlink so `RTK_BINARY` identifies the regular executable itself:

```bash
export AW_TOKENLESS_CLI="$(command -v tokenless)"
export AW_PROVIDERS_DIR="$AW_EXTENSION_PREFIX/libexec/aw/providers"
export AW_RTK_BINARY="$(readlink -f "$(command -v rtk)")"
export AW_RTK_SHA256="$(sha256sum "$AW_RTK_BINARY" | awk '{print $1}')"
"$AW_RTK_BINARY" --version
aw config init --prefix "$AW_EXTENSION_PREFIX" \
  --config "$AW_HOME/tokenless.yaml" --state-dir "$AW_HOME/tokenless-state" \
  --template "$AW_TEMPLATE_DIR/tokenless.json" \
  --template-var "PROVIDERS_DIR=$AW_PROVIDERS_DIR" \
  --template-var "TOKENLESS_CLI=$AW_TOKENLESS_CLI" \
  --template-var "RTK_BINARY=$AW_RTK_BINARY" --template-var "RTK_SHA256=$AW_RTK_SHA256" \
  --qoder "$AW_QODER"
aw validate --config "$AW_HOME/tokenless.yaml"
cd "$AW_HOME/workspace"
aw run --config "$AW_HOME/tokenless.yaml" --agent qoder
```

The version command must report `rtk 0.49.0`. Template variables are
`PROVIDERS_DIR`, `TOKENLESS_CLI`, `RTK_BINARY` and `RTK_SHA256`.
For native or Makefile installations, use the actual Provider root for `PROVIDERS_DIR`. For security too, install and start
[sec-core](security.md) and select `sec-core-tokenless.json`, adding
`SEC_CORE_CLI` and `SOCKET` from that backend. OpenClaw uses its
[Agent arguments](configuration.md#openclaw); other runtimes use the
[integration reference](../../../developer-guide/en/aw/providers/tokenless.md).

Providers already installed by RPM or another owner may instead be configured
through absolute `transport.argv` in YAML.

## Runtime behavior

- Before execution, AW may block the original call with a command retry suggestion. The Agent's new call passes through safety and native permission checks again.
- After execution, supported successful text results can be compressed. Failed results, images and unsupported content stay unchanged.
- Compression errors, timeouts or no savings preserve the original result. Compression cannot override a safety denial.

Generated configuration uses the explicitly selected RTK path and checksum. Missing or invalid files produce an error;
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
for older files, see the [query reference](../../../developer-guide/en/aw/providers/tokenless.md#read-only-panel-queries).

Tokenless statistics identify their source using the Agent name configured in AW,
the name selected by `aw run --agent`. For example, `qoder-work` and `qoder-test`
are identified separately even if both use Qoder. Reusing the same name across
configurations can still aggregate their records.

Statistics estimate candidate compression savings over recent records and may include other
integrations. Confirm adoption into model context from the Agent's actual results; use the model
provider's billed usage to determine session billing savings.

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

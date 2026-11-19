# ws-ckpt Provider for AW

[中文版](README_zh.md)

This Provider calls the installed `ws-ckpt` CLI on AW's `input.submit` Hook.
Automatic snapshots default to off. Install AW and a compatible ws-ckpt CLI
and daemon separately. Install the ws-ckpt Skill with the backend if you want
to manage snapshot bindings and switches in conversation.

## Build and install

Build on Linux from the repository root.

```bash
./scripts/build-all.sh --component aw-provider-ws-ckpt --no-install
make -C providers/ws-ckpt install PREFIX=/usr/local DESTDIR=/absolute/path/to/staging-root
```

Native installation places the Provider under
`<prefix>/libexec/aw/providers/ws-ckpt/` and the full example under
`<prefix>/share/aw/providers/ws-ckpt/examples/`. If you set `AWPRJ`, use the
paths under that namespace. Native installation copies the Provider files only.

For an AW package archive, extract it and pass its absolute directory to
`aw install --bundle` with your Provider prefix.

## Configure and launch

The archive's `templates/ws-ckpt.json` configures Qoder's `input.submit` Hook
through `aw config init --template`. Set `PROVIDERS_DIR` to the absolute parent
of the installed `ws-ckpt` Provider directory and `WS_CKPT_CLI` to the absolute
backend executable using `--template-var`. The full `examples/aw.yaml` also
shows how to configure the Skill source and destination. Use the same
`ws-ckpt` executable in the Agent's `PATH` and the Provider's `--cli`.

Register the workspace with ws-ckpt and run the following command from
outside it. Use the same `PROVIDERS_DIR` and `WS_CKPT_CLI` paths you used to
generate the configuration. Set `WS_CKPT_AW_TARGET` to the target passed to
`--agent` and `WS_CKPT_AW_PROVIDER` to the installed Provider executable.

```bash
PROVIDERS_DIR=/absolute/provider/prefix/libexec/aw/providers
WS_CKPT_CLI=/opt/bin/ws-ckpt

WS_CKPT_AW_TARGET=qoder \
  WS_CKPT_AW_PROVIDER="$PROVIDERS_DIR/ws-ckpt/aw-provider-ws-ckpt" \
  PATH="$(dirname "$WS_CKPT_CLI"):$PATH" \
  aw run --config /absolute/path/to/aw.yaml --agent qoder
```

In the Agent session, use the ws-ckpt Skill to bind the workspace and enable
automatic snapshots. For unattended binding, run
`aw-provider-ws-ckpt bind --agent <target> --workspace <absolute-path>` with the
installed Provider executable. The Provider stores bindings and switches in
`.local/state/ws-ckpt/aw-policy.json` under the account's system home.
To remove the AW-managed Provider, run
`aw uninstall --prefix /absolute/provider/prefix aw-provider-ws-ckpt`.

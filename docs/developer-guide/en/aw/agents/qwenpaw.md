# QwenPaw

[中文版](../../../zh/aw/agents/qwenpaw.md)

Run source examples from `src/aw`; build AW with the [integration tutorial](../integration.md) first.

## Start QwenPaw

The verified baseline is QwenPaw 2.2.2.post1 with AgentScope 2.0.10. Install QwenPaw
with its compatible dependencies and initialize its model configuration through
QwenPaw. AW records the installed version and checks the plugin, middleware and
native task lifecycle interfaces for other releases. The example uses `argv: [qwenpaw, app]`. Set
`QWENPAW_WORKING_DIR` to the existing initialized absolute directory. AW adds only
its owned temporary plugin, preserves existing plugins and native configuration,
and removes its plugin after the App exits. Each installation has its own
`aw-native-<uuid>` directory and manifest ID, preserving user-owned `aw-native`
plugins and settings. AW leaves `config.json` loading and native MCP updates to
QwenPaw. Native factory arguments are forwarded unchanged, and tool generators
finish cleanup before their terminal result reaches the Coordinator.

Ordinary launches verify resident middleware on each constructed Agent and open
an acknowledged task scope. Before Hooks run forward; after Hooks unwind in
reverse. The host controls native tool concurrency. Offloaded tools keep their
scope until native generators, Hooks and background completion callbacks finish.
Background HTTP output and next-request hints become visible only after all required
native callbacks succeed. Denials retain policy text and Tokenless retry arguments.
Completed invocations retire their state, allowing long sessions to continue
within the bounded active-call limit.

After Hooks receive redacted terminal `ToolResponse` objects, including AW-denied
results. The application credential extension supplies one private execution
lease per Agent reply, shared by that reply's tools, and owns its cleanup; see
[Private execution leases](../../../../../src/aw/docs/design/en/adapters/private-execution.md).
Redactors must be idempotent across repeated delivery/Hook checks.
Authorization-bearing arguments remain unchanged. `on_error: report`
preserves callback error reporting; before `block` denies execution. For structured
Providers and boolean Hooks, after `withhold_result` prevents unverified result
delivery and later dispatch. This action is admitted only on ordinary resident
launches. Strict after processing withholds streaming chunks for matching tools.
Task-private execution also withholds intermediate chunks until terminal redaction.
Native Hook or lifecycle failures fence the Agent even when AW steps use `report`;
restart the failed Agent after correcting the cause. Cleanup timeout or uncertain finish/release acknowledgement fences
new requests process-wide and requires a worker restart. Credentials remain owned
until native work is confirmed drained. Shutdown shares one ten-second budget
with nested cleanup, including cancellation. Event budgets must be
1..55,000 ms;
ask/approve directives remain unsupported. Native permissions run before these
callbacks; external execution and earlier permission denials have separate native
paths.

The startup receipt confirms plugin loading; each Agent additionally passes the
actual installation guard before its request starts. Use App/API after the native
Hooks ready message. ACP/TUI and multi-worker launches remain rejected.
`--reloadable` retains its separate request-idle update contract. Ordinary
launches provide resident child scopes, long-session retirement and explicit
updates between fully drained replies. A reused Agent reads the active policy
for each new reply after reservation. App startup confirms update readiness before
the first reply; startup policy remains active until an update succeeds.
Each acknowledged reply scope owns bounded native callback observation and drains
it before close. Verified native identity can query the configured evidence source;
a missing or unavailable source leaves explicit gaps and unknown coverage.

```bash
QWENPAW_WORKING_DIR=/absolute/qwenpaw-home target/debug/aw run \
  --config crates/aw-service/examples/aw.qwenpaw.yaml --agent qwenpaw \
  -- --host 127.0.0.1 --port 8096
```

The [neutral QwenPaw example](https://github.com/agentic-os-org/ANOLISA/blob/main/src/aw/crates/aw-service/examples/aw.qwenpaw.yaml)
runs commands before and after tools; it installs no security policy. Use the
same `aw-provider/v1alpha1` policy configuration across supported adapters.
The native Hook output dialect and event coverage remain framework-specific.

An ordinary configuration with no Providers or event steps still installs
resident admission and cleanup middleware.
The shared sec-core Provider also supports [Skill validation before loading](../providers/sec-core.md#validate-skills-before-loading). Configure `check_skill` on `tool.before` for the native `Skill` tool and authorize its canonical source directories in the daemon.

## Keep the native App startup command

For an initialized QwenPaw installation in a dedicated Python environment, install
AW's startup module once and restart the App. Configure the `qwenpaw` target's
`argv` to match the executable and arguments you normally use, including host and
port. Start the AW service with the same `aw.yaml` first.

Run setup from the directory containing `aw.yaml`. Replace the Python path with
the interpreter in the dedicated QwenPaw environment:

```bash
QWENPAW_PYTHON=/absolute/qwenpaw-venv/bin/python
AW_STARTUP_DIR="$("$QWENPAW_PYTHON" -c 'import sysconfig; print(sysconfig.get_path("purelib"))')"
aw instance setup --agent qwenpaw --directory "$AW_STARTUP_DIR" > aw-startup.json
export AW_INSTANCE_BOOTSTRAP="$(python3 -c 'import json; print(json.load(open("aw-startup.json"))["environment"]["AW_INSTANCE_BOOTSTRAP"])')"
export QWENPAW_WORKING_DIR=/absolute/qwenpaw-home
qwenpaw app --host 127.0.0.1 --port 8096
```

Keep the returned startup environment in the application's service or shell
configuration. Setup creates a `.pth` activation file in the explicitly selected
environment and preserves any existing `sitecustomize`. The selected directory
must belong to the current user and disallow group/other writes. Setup refuses
to overwrite existing activation files. The App prepares its own
instance, loads the ordinary resident plugin from AW's private assets, and confirms
readiness after native plugin startup. Requests arriving before that confirmation
cannot build an Agent. A different native command fails before App startup.

Edit `aw.yaml`, pause dispatch and update from another terminal:

```bash
aw status --config aw.yaml
aw instance update --agent qwenpaw
```

The ordinary input, tool, strict-after and reply cleanup boundaries remain active;
the native App, user plugins and MCP configuration keep their existing ownership.
Policy activation waits for replies and cleanup to drain. Graceful shutdown releases the
instance and its temporary plugin assets. Stop the App before removing the exact
setup files listed in `aw-startup.json`, then unset `AW_INSTANCE_BOOTSTRAP` and
restart to remove this integration. This startup route supports one native App
process; ACP, TUI and multiprocessing require separate integration.

Stock QwenPaw 2.2.2.post1 can publish stale MCP permissions after a policy update
has been acknowledged. Native permission hot updates require a version containing
the [driver publication fix](https://github.com/kongche-jbw/QwenPaw/commit/6e7fb8a19ce61eca3b8dbc86d1818fc94b7252a8).
The verified [2.2.2.post1 backport](https://github.com/kongche-jbw/QwenPaw/commit/099f645819218256364e7d63ae35dce21e383bda)
is an unreleased Preview: both native and AW-managed Apps retain deny during
concurrent reload, with no denied server-side tool effect, and subsequent normal
reload recovers. New calls after policy acknowledgement use the latest policy;
already-authorized in-flight calls may complete.

Stop the App, install a complete wheel built from that fixed revision in its
dedicated environment, then restart. Use QwenPaw's
[wheel build script](https://github.com/kongche-jbw/QwenPaw/blob/099f645819218256364e7d63ae35dce21e383bda/scripts/wheel_build.sh)
to retain Console assets. With an unpatched version, keep native MCP configuration
and permissions unchanged while the App runs; make changes after exit, verify
persisted configuration, then restart.

`aw instance update` changes AW policy at the reply idle boundary. AW
`tool.before` denial protects subsequent calls that pass through its Hooks;
the QwenPaw fix supplies native driver concurrency control.

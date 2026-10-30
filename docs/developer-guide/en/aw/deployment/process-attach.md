# Attach external processes

[中文版](../../../zh/aw/deployment/process-attach.md)

A container or process controller can start a native process and attach AW before its first request. The controller retains ownership of the PID, terminal, dispatch and shutdown.

## Supported entrypoints

| Entrypoint | Support |
| --- | --- |
| Qoder 1.1.73 TUI | ConfigChange confirmation before input; native idle-prompt after requests |
| QwenPaw 2.2.2.post1 / AgentScope 2.0.10 App | Initialized profile, loopback HTTP; direct API use supported |
| Qoder print, headless or SDK; QwenPaw ACP/TUI/desktop | No PID attachment; SDK uses [managed factories](../agents/qoder-sdk.md) |

The ordinary Qoder CLI minimum version range does not apply to this mode.

## Prepare and attach

1. Give each process an exclusive profile. AW and Agent share user, PID and mount namespaces; QwenPaw also shares the network namespace.
2. Use a canonical absolute profile path owned by the runtime user with mode `0700`.
3. Start the native process, record `NATIVE_PID` and pause first-request dispatch.
4. Prepare `current.yaml` with the [Agent guide](../native-agents.md), then connect. Dispatch only after success returns instance, policy revision and process identity.

```bash
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID"
aw update --config current.yaml --agent TARGET
aw agent connect --config current.yaml --agent TARGET --pid "$NATIVE_PID" --remove
```

The three commands cover attachment, [idle update](policy-updates.md) and removal; run each at its appropriate stage.
Pause dispatch and wait for request completion before removal. `busy` retains the attachment for explicit retry. Successful removal retains the native process, unrelated Hooks and user configuration.

## Native preparation

- Qoder uses `$HOME/.qoder/settings.json`; omit `--settings`, custom configuration roots and Hook-disabling options. Existing settings must be a regular file owned by the same user, without group/other write permissions.
- QwenPaw must set absolute `QWENPAW_WORKING_DIR` in its initial process environment and listen on `127.0.0.1`. Other Apps in that environment also need explicit profiles; stop or reconfigure a reported peer whose startup directory cannot be verified.

The controller maintains profile exclusivity. Remove AW before stopping the external process.

## Cleanup failures

When native removal cannot be confirmed, AW retains ownership and bridge files. Restore native callbacks or the plugin API, then retry the same `--remove` command.
Restore a deleted plugin directory if it remains loaded. Restore Qoder's original AW configuration group if it was moved.

Forced termination during installation can leave staging files or unrecorded bindings. Use the owning supervisor to stop affected services, then inspect and clean their private state; automatic crash collection is not implemented.
See [policy update design](../../../../../src/aw/docs/design/en/runtime/policy-reload.md) for ownership and removal contracts.

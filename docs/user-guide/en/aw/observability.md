# Connect AgentSight

[中文版](../../zh/aw/observability.md)

AW associates Agent runs with AgentSight records to inspect activity and observation gaps.
Automatic observation supports Qoder, OpenClaw and Hermes. QwenPaw requires restricted `--reloadable` mode; ordinary resident mode requires `evidence.mode: off`.
AgentSight is installed and managed independently; connecting AW does not start a collector.

## Configure a source

Prepare a query service using the [AgentSight deployment guide](../agentsight/deployment.md).
Exit Agents and stop the old AW service, then add this block under `spec.agents.<target>`.

```yaml
evidence:
  mode: auto
  source_id: local-sight
  endpoint: http://127.0.0.1:7396
  query_timeout_ms: 1000
  query_bytes: 1048576
  max_items: 32
  max_bytes: 262144
```

Use the actual service port. Local loopback HTTP needs no token. A remote service must use HTTPS
with `token_file` pointing to a regular file owned by the current user with mode `0600`.
Do not put tokens on the command line. Use the service origin as the endpoint, without `/api`.

## Run and inspect

```bash
aw validate --config "$HOME/aw/aw.yaml"
aw run --config "$HOME/aw/aw.yaml" --agent qoder
```

Check the same configuration from another terminal.

```bash
aw status --config "$HOME/aw/aw.yaml"
```

Startup output includes source query state and an audit key. After the Agent makes a tool call,
inspect native association, source results and gaps in status. Query `ready` means the interface
is reachable; check AgentSight to establish whether it collected the target data and whether eBPF
is available. A just-completed call may not have been collected yet.

In `auto` mode, an unavailable source reports a reason while Agent execution and local AW audit
continue. `required` mode is not supported yet.

## Disable

Exit the Agent and stop AW. Set `evidence.mode` to `off` or remove the block, then relaunch.
This does not stop the independent AgentSight service.

For identity association, query limits and embedding interfaces, see the
[developer reference](../../../developer-guide/en/aw/agentsight.md).

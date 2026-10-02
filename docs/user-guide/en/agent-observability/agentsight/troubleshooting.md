# AgentSight Troubleshooting

[中文版](../../../zh/agent-observability/agentsight/troubleshooting.md)

Work top to bottom: most "AgentSight shows nothing" reports come from the first three sections.

## Quick check

```bash
systemctl is-active agentsight.service                 # is it running?
ps -eo pid,cmd | grep -E "agentsight (trace|serve)"    # both workers alive?
sudo agentsight discover                               # is your Agent recognised?
sudo agentsight summary --last 1                       # did anything land?
journalctl -u agentsight.service -n 50 --no-pager      # what does it complain about?
```

## No data at all

**1. The tracer is not running as root.** eBPF attachment fails silently for an unprivileged user
and you simply see no events. Run `sudo agentsight trace`, or use the packaged service. If the
environment cannot grant privileges at all, `agentsight trace --no-ebpf` still collects trajectories
(see [CLI reference](cli-reference.md#running-without-privileges---no-ebpf)); token, audit, and
interruption data remain unavailable.

**2. The kernel has no BTF.** `ls /sys/kernel/btf/vmlinux` must succeed. Without it the CO-RE probes
cannot load; `journalctl` shows a probe load error.

**3. Your Agent is not matched by any rule.** This is the most common cause after privileges. Check
whether a rule covers it:

```bash
sudo agentsight discover --list-known | grep -i <your-agent>
```

`discover` reads the same `config.json` the tracer uses, so `--list-known` reflects your custom
rules (add `--config <path>` for a non-default file). If the file is missing or unparseable it warns
and falls back to the built-in rules. See
[Agent discovery rules](configuration.md#agent-discovery-rules).

Remember that a user-provided `config.json` **replaces** the built-in rules instead of extending
them — a partially customised file silently drops discovery for every Agent it does not mention.
Start from `src/agentsight/agentsight.json` and add to it.

**4. The provider domain is missing.** TLS capture only applies to domains listed under `https`. Add
your gateway or provider domain and reload.

**5. Two tracers are competing.** A foreground `agentsight trace` plus the service means both
attach to the same uprobes. Stop one:

```bash
sudo systemctl stop agentsight.service     # before running a foreground tracer
```

**6. You are querying as the wrong user.** The service writes root-only data, so `agentsight token`
without `sudo` reads an empty or non-existent database.

## Some Agents appear, one does not

- A compiled Agent (Rust, Go) is not matched by `node*`-style rules — add a rule for its binary name.
- Wrapper processes matter: cosh-ng runs `cosh-shell` and `cosh-core`, and both need rules.
- Codex CLI needs an offset entry for brand-new releases; regenerate it with
  `src/agentsight/scripts/extract-codex-offsets.py`.
- `[attach_process] pid=… no SSL libraries found in maps` in the log means the process was matched
  but exposes no TLS library to hook — expected for short-lived helper processes.

## Dashboard problems

**401 or the login screen from a remote browser.** Remote access requires the token:

```bash
sudo agentsight dashboard --no-open        # prints URL + token
```

Then use `http://<host>:7396/?token=<TOKEN>`, or paste the token into the login form. Loopback
access never asks. To disable authentication on a trusted network, set
`server.auth.enabled` to `false` and reload.

**The page does not load at all.** Check the listener and the network path:

```bash
ss -ltnp | grep 7396
curl -s -o /dev/null -w '%{http_code}\n' http://127.0.0.1:7396/
```

If localhost answers `200` but your browser times out, the port is blocked. `serve` must bind
`0.0.0.0` (the packaged unit does), and the firewall or cloud security group must allow TCP 7396.
`agentsight dashboard` prints the ECS console link for exactly this.

**Some pages are missing.** The navigation only lists pages the host can serve: Token Savings needs
Tokenless, Security Observability and System Audit need agent-sec-core, Risk Enforcement needs the
enforcer. See [page availability](dashboard.md#navigation-and-page-availability).

**A page is empty although data exists.** Token Savings, Skill Metrics, and System Audit wait for
you to press **Query** after choosing a time range. Also check the Agent filter and that the range
covers when the sessions happened.

**"No agents discovered" on the Agent Dashboard.** That panel shows *live* processes. Historical
sessions remain on the other pages.

## `AgentSight enforcement unavailable` on every start

The enforcer daemon is not running:

```bash
sudo systemctl status agentsight-enforcer.service
sudo systemctl start agentsight-enforcer.service
```

Source builds need `make build-all`; plain `make build` skips the enforcer binary. Capture and
analysis are unaffected — only the Risk Enforcement page disappears.

## Data grows too fast

```bash
sudo du -sh /var/log/sysak/.agentsight
```

`genai_events.db` is capped at 200 MB (pruning starts at 90%) and `interruption_events.db` at
30 days / 100 MB. Raise or lower them as described in
[Retention and size limits](data-and-storage.md#retention-and-size-limits). Large `-wal` files are
normal while the service runs; they are checkpointed on clean shutdown.

## Memory or CPU concerns

The packaged unit caps the service at `CPUQuota=30%` and `MemoryMax=350M`, which matches the default
`runtime_limits`. On very busy hosts, prefer lowering the buffers over raising the cap:

```json
"runtime_limits": {
  "event_channel_capacity": 5000,
  "event_channel_policy": "drop_newest",
  "event_channel_max_bytes_mb": 32,
  "pending_genai_max_bytes_mb": 32,
  "max_connection_body_mb": 4
}
```

`drop_newest` and `sample` trade completeness for a hard bound on memory.

`event_channel_max_bytes_mb` bounds what the slot count cannot: one captured SSL record can be up to
4 MiB, so a full channel is not the same as a full buffer. Set it to `0` to disable the byte gate.
When the budget rejects events, the service logs a watermark line once a minute:

```bash
sudo journalctl -u agentsight | grep "Buffer watermarks"
```

A rising `dropped over budget` count means the pipeline cannot keep up — events are being shed to
stay inside `MemoryMax`. A count that stays at zero while the cgroup still approaches its cap means
the memory is held downstream (HTTP connection buffers) rather than in the channel, so lower
`max_connection_body_mb` instead:

```bash
sudo cat /sys/fs/cgroup/system.slice/agentsight.service/memory.current
```

If the service is ever OOM-killed it restarts on its own; the packaged unit disables systemd's start
rate limit precisely so a repeated kill cannot leave the host permanently unobserved.

## Interruption events look wrong

- **Nothing detected although a task failed**: check `features.interruption_detection.enabled`, and
  remember detection is derived from captured traffic — an Agent that never reached the provider
  produces no LLM-side signal.
- **Unsure which `--type` values are valid**: `agentsight interruption list --help` prints the full
  set; every detected type is accepted.
- **Duplicate-looking events**: interruptions are de-duplicated per conversation, so the same
  underlying error in two conversations is intentionally two events.
- **`token_limit` on healthy sessions**: it fires at 95% of `max_tokens`. Raise the Agent's
  `max_tokens` if answers are being cut off.

## macOS limitations

Only `trace` (trajectory collector over local JSONL sessions) and `serve` exist. There is no eBPF,
so Token, audit, interruption, and discover commands are unavailable, and `--db` / `--config` are
Linux-only.

## Output language looks inconsistent

`summary`, `metrics`, and `interruption` print English; `discover` and `token` print Chinese
regardless of locale. Use `--json` where you need stable machine-readable output — `discover` has no
`--json`, so query `/api/agent-health` instead. The Dashboard
follows the browser language and has a manual switch.

## Frequently asked

**Why is there no Token data for OpenClaw?** AgentSight watches the `openclaw-gateway` daemon.
Verify the client reaches the gateway; a "pairing required" error means you need
`openclaw devices approve`.

**Why does the Token Savings page show 0?** Tokenless produced no savings for those sessions — check
that it is enabled for that Agent, and that its statistics database exists.

**Why do cumulative savings exceed the per-call difference?** Agents resend conversation history on
every turn, so savings accumulate across turns.

**Can I trace an Agent that already runs?** Yes. Discovery scans `/proc` at startup and watches
`execve`, so a running Agent is picked up without restarting it.

## Collect diagnostics before filing an issue

```bash
agentsight --version
uname -r
ls /sys/kernel/btf/vmlinux
systemctl status agentsight.service
journalctl -u agentsight.service -n 200 --no-pager
sudo agentsight discover --list-known | head -40
sudo agentsight summary --last 24 --json
sudo ls -la /var/log/sysak/.agentsight
```

# cosh-ng troubleshooting

[中文版](../../../zh/user-entrypoint/cosh-ng/troubleshooting.md)

Diagnose cosh-ng problems in three steps: run the doctor, read the runtime
evidence on disk, and export a redacted bundle when you hand the problem to
someone else. This guide covers the failure classes cosh-ng can suffer
silently: a shell that exits without an error, leftover cosh-core processes,
and input that stops routing to the Agent.

## Step 1 — run the doctor

Look at live facts first.

- Inside an affected session, run `/health`. It probes the live core (a
  response means the core is alive; no response means it is dead or
  degraded) and prints routing facts: the effective AI-enabled state,
  integration mode, `command_not_found_handler` ownership, and the most
  recent routing decision. A live core plus routing facts that explain a
  fallback is reported as a named finding — "routing compatibility
  fallback, not a provider failure".
- Outside a session, run `cosh-shell doctor`. The header summarizes runtime
  (live shell and core processes, paired or orphaned), logs (level, 24h
  ERROR counts per component), and crashes (24h count with timestamps). If
  the routing line says the live probe is unavailable, run `/health` inside
  the affected session.

Follow each finding's remediation line. Most remediations point at
`cosh-shell diagnostics export`; run it before asking for help. Exit codes:
0 healthy, 1 warning, 2 error.

## Step 2 — read the runtime evidence

All diagnostics state lives under `~/.copilot-shell/`:

| Path | Content |
|---|---|
| `logs/cosh-shell.log.<date>`, `logs/cosh-core.log.<date>` | Daily logs; check for WARN/ERROR lines around the incident time |
| `cosh-shell-crash.log`, `cosh-core-crash.log` | One JSON line per panic (timestamp, version, pid, panic message) |
| `run/shell-<pid>.json`, `run/core-<pid>.json` | One file per live session; entries left behind by a crash or SIGKILL are evidence of an abnormal exit. `cosh-shell doctor` reports them and suggests a kill command for orphaned cores; a new session cleans up entries older than a week |

Raise log verbosity temporarily when the defaults hide the problem:

- `COSH_LOG=debug cosh-shell` (highest priority), or `RUST_LOG=debug`
- `[logging] level = "debug"` in `~/.copilot-shell/config.toml`

Priority: `COSH_LOG` > `RUST_LOG` > config `[logging] level` > default `info`.

## Step 3 — export a diagnostic bundle

`cosh-shell diagnostics export` collects a redacted bundle: configuration,
logs, crash records, run-registry entries, and health facts. It covers the
last 24 hours by default; widen the window when the problem is older:

```
cosh-shell diagnostics export --since-hours 72
```

The bundle is self-describing: its evidence fields carry component versions,
collection time, and per-file meaning, so a maintainer or an Agent unfamiliar
with cosh can interpret it directly. Attach a one-line problem description:

> Symptom: … — When: … — Already tried: `cosh-shell doctor`, `/health`, …

## Input routing matrix (zsh, natural-language input stops routing)

In an Enhanced zsh session, natural-language input routes through the
`command_not_found_handler`. When it stops reaching the Agent, decide the
cause inside the session:

```
typeset -p _COSH_AI_ENABLED _COSH_HAS_USER_COMMAND_NOT_FOUND
whence -v command_not_found_handler
```

| Observation | Interpretation | Action |
|---|---|---|
| `??` reaches the Agent but bare natural-language input does not | Routing or hook degradation, not a provider failure | Run `/health`, then export a bundle |
| `_COSH_AI_ENABLED=0` | Expected fallback: AI is disabled | Re-enable AI, or accept the fallback |
| `_COSH_HAS_USER_COMMAND_NOT_FOUND=1` | Compatibility fallback: a user command-not-found handler takes precedence | Remove the user handler, or accept the fallback |
| Variables look normal but input still does not route | Wrapper coverage or marker-generation drift | Run `/health` and check the marker generation; report with an export bundle |

`??` is a differential probe: it is guaranteed to not exist as a command, so
it always travels the command-not-found path. If `??` reaches the Agent while
ordinary input does not, the command-not-found path itself works and the
problem sits in the routing decision or hook chain — the same facts `/health`
reports as "routing compatibility fallback, not a provider failure".

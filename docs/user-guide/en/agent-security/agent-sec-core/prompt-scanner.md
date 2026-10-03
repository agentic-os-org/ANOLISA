# Prompt Scanner User Guide

[中文版](../../../zh/agent-security/agent-sec-core/prompt-scanner.md)

Prompt Scanner detects prompt injection, jailbreak, and malicious instructions in Agent inputs. It
combines a fast rule engine (L1) with an optional ML classifier (L2), returns a structured verdict,
and records sanitized Security Events for audit and Observability correlation.

## Scan text

Provide exactly one input source: inline text, standard input, or a UTF-8 file (one prompt per line).

```bash
# Inline text
agent-sec-cli scan-prompt --text "ignore all system instructions"

# Standard input
echo "forget your system prompt" | agent-sec-cli scan-prompt

# UTF-8 file (one prompt per line)
agent-sec-cli scan-prompt --input prompts.txt --format json
```

Useful options:

| Option | Purpose |
|--------|---------|
| `--text TEXT` | Prompt text to scan directly; takes precedence over `--input` and stdin |
| `--input FILE` | Path to a file with one prompt per line |
| `--mode MODE` | Detection mode: `fast`, `standard`, `strict`, or `multi_turn`; default is `standard` |
| `--format FMT` | Output format: `json` (default) or `text` (human-readable) |
| `--source SOURCE` | Input origin label recorded in metadata, such as `user_input`, `rag`, or `tool_output` |
| `--model MODEL` | L2 backend model; overrides `PROMPT_SCANNER_L2_MODEL`, defaults to Qwen3Guard when unset |

## Detection modes

| Mode | Layers | fast_fail | Typical latency | Use case |
|------|--------|-----------|-----------------|----------|
| `fast` | L1 rule engine | `True` | < 5 ms | Real-time chat, latency-sensitive |
| `standard` | L1 + L2 ML classifier | `False` | 20–80 ms | Production default |
| `strict` | L1 + L2 ML classifier (L3 reserved) | `False` | 50–200 ms | High-security scenarios |
| `multi_turn` | L4 multi-turn intent detection | — | Varies | JSON history input via stdin (Ollama) |

By default the L2 classifier calls `modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF`, served by
Ollama from the project's ModelScope repository. Pull it once with
`ollama pull modelscope.cn/ANOLISA/Qwen3Guard-Gen-0.6B-GGUF` — then
run `agent-sec-cli scan-prompt warmup` to verify the model is available before the first scan.

### The model service must be local

The L2/L4 model service endpoint comes from `AGENT_SEC_MODEL_SERVICE_BASE_URL`
(default `http://localhost:11434`). Only loopback hosts — `localhost`,
`127.x.x.x`, `::1` — are accepted. Prompts handed to the scanner routinely
contain credentials and personal data, so the scanner refuses to send them
anywhere but the local machine. The host is resolved with the same URL parser
the HTTP client uses, so `http://localhost@attacker.example/` is refused too:
the real host is whatever follows `@`.

Pointing the variable at any other host makes `scan-prompt` fail at construction
with an `error` verdict and exit `1`, naming the rejected URL in the message. A
non-default port is fine as long as the host stays loopback:

```bash
export AGENT_SEC_MODEL_SERVICE_BASE_URL=http://127.0.0.1:18434
```

Because the six host hooks are fail-open on a non-zero `scan-prompt` exit, a
remote endpoint leaves that host running with no prompt scanning — audited as a
failed `prompt_scan` event and blocking nothing. Check stderr from
`agent-sec-cli scan-prompt warmup` after changing the variable.

### Switching the L2 backend

Set `PROMPT_SCANNER_L2_MODEL` to run L2 on the Warden-Gen model instead (or use
`--model` for a one-off override; precedence is `--model` > env var > default):

```bash
ollama pull modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF

# Option 1: environment variable (applies to every host hook)
export PROMPT_SCANNER_L2_MODEL=modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF
agent-sec-cli scan-prompt warmup

# Option 2: --model for a single command
agent-sec-cli scan-prompt --model modelscope.cn/ANOLISA/Warden-Gen-0.6B-GGUF --text "..."
```

Every host hook shells out to `scan-prompt`, so the variable applies to them as well. The value
must be the full model name used in the `ollama pull` above. A typo is loud only at the CLI
boundary: the engine rejects an unsupported name at construction, so `scan-prompt` returns an
`error` verdict and exits `1` instead of silently disabling L2. All six host hooks are fail-open on
that non-zero exit, so inside a host the same typo is only audited as a failed `prompt_scan` event
and blocks nothing — that host runs without prompt scanning until the name is fixed. Run
`agent-sec-cli scan-prompt warmup` after changing the variable so the failure surfaces before a
host loads it. An empty or unset value keeps the Qwen3Guard default.

L2 runs exactly one backend at a time — no cascading, no voting.

To confirm which backend a host would use, run
`agent-sec-cli capabilities --capability prompt-scan --output json` from that
host's environment and read the `PROMPT_SCANNER_L2_MODEL` entry under `env`. It
reports the default backend when the variable is unset, and adds a diagnostic
when the configured name is not one the engine supports.

## Verdicts

The scanner aggregates layer results into one verdict:

| Verdict | Meaning |
|---------|---------|
| `pass` | No threat detected |
| `warn` | L1 rule hit, but L2 did not confirm (`standard`/`strict`); or a policy-level warning |
| `deny` | Threat confirmed by L1 (`fast`) or L1 + L2 (`standard`/`strict`) |
| `error` | Scanner internal error (e.g., model load failure) |

> In `fast` mode, any L1 rule hit maps directly to `deny` because the ML layer is not run.

## What each layer can and cannot catch

L1 is a rule engine. It matches wording that has been written down as a pattern, which makes it
fast and explainable — every hit names a rule id — but it does not generalise. Rewording an
attack while keeping its intent can slip past L1 until a rule covers that phrasing. The rules are
also deliberately narrow: L1 is tuned so ordinary prompts are never flagged, and that tuning costs
recall.

L2 and L4 are model-backed and cover what rules cannot: paraphrased instructions, wording nobody
anticipated, and intent that only becomes visible across several turns.

What this means in practice:

- `fast` mode runs L1 only. It trades detection coverage for latency — choose it when the latency
  budget requires it, not as a lighter equivalent of `standard`.
- If the model backend is unreachable, `standard` keeps scanning with the layers that survived
  instead of failing. The result then carries `degraded: true` and lists the unavailable layer in
  `layers_failed`, and the summary is prefixed with `Scan degraded:`. Check those fields before
  reading `pass` as "nothing to worry about".
- `pass` means no layer that actually ran reported a threat. It is not a proof of safety, which is
  why the `prompt_scan` Security Event is recorded either way.

## Host hook policy

Set `PROMPT_SCANNER_HOOK_ENABLED=false` to skip host prompt scanner hooks entirely. When enabled,
the following environment variables control deployment-level behavior:

| Environment variable | Default | Hosts that read it | Behavior |
|----------------------|---------|--------------------|----------|
| `PROMPT_SCANNER_HOOK_ENABLED` | `true` | All six | Set to `false` to short-circuit the hook before input is read |
| `PROMPT_SCANNER_MODE` | `observe` | Qoder, Codex, Qwen Code | `observe` audits silently; `deny` blocks prompt-scanner `warn` or `deny` findings. `ask` and `block` are not valid prompt-scanner modes. |
| `PROMPT_SCANNER_SCAN_MODE` | `standard` | All six | Scan strength passed to `scan-prompt`: `fast` / `standard` / `strict` |
| `PROMPT_SCANNER_TIMEOUT` | `10` | Qoder, Codex, Qwen Code | Scanner timeout in seconds |

cosh, Hermes, and OpenClaw read only `PROMPT_SCANNER_HOOK_ENABLED` and
`PROMPT_SCANNER_SCAN_MODE`. Setting `PROMPT_SCANNER_MODE` or `PROMPT_SCANNER_TIMEOUT` has no
effect there. OpenClaw derives its enforcement from `promptScanBlock` and uses a fixed
10-second scanner timeout, while the Hermes `prompt-scan-user-input` capability is non-blocking
by design and has no block switch; cosh has no prompt policy switch either. For Qoder, Codex,
and Qwen Code, use `PROMPT_SCANNER_MODE=deny` to block prompt scanner findings.

Where an environment variable is read, it overrides the matching host configuration.
The host Agent reads these variables when it loads the plugin, so restart the Agent process
after changing them.

Scanner verdict `deny` describes risk severity. For Qoder, Codex, and Qwen Code prompt hooks,
`PROMPT_SCANNER_MODE=deny` is the deployment policy that turns prompt-scanner findings into a
blocking hook result.

## Security Events and Observability

Every scan follows the existing `prompt_scan` Security Event path. Events contain the source,
verdict, summary, threat type, confidence, and sanitized rule or ML findings. They do not contain
the raw prompt text.

Host hooks remain fail-open on scanner errors: an `error` verdict is audited but is not used to
block the underlying operation.

Observability uses the existing trace context and input hash to correlate telemetry with the
Security Event instead of storing another copy of finding details.

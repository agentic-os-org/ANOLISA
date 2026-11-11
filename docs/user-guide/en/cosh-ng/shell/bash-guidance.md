# Bash guidance and login-shell controls

[中文版](../../../zh/cosh-ng/shell/bash-guidance.md)

This integration targets ALinux4 with Bash 5.2; the same Hook supports verified Bash 5.1 behavior. The image keeps Bash as the default login shell and preinstalls Cosh. Run
`cosh` when you want an AI Shell. A temporary session returns to its parent
Bash when you exit; exiting a login Cosh session ends that login instead.

The Cosh RPM does not install `profile.d` entries or edit `/etc/bashrc`; a
separate system package owns those optional integration points. Automatic
failure guidance checks the installed module and actual Shell configuration,
not hashes of user rc files or an exact Cosh release version. Existing user
Hooks take priority; conflicts skip failure guidance while the login display
remains independent.

## Enter with a question

```bash
cosh --ask 'Help investigate why this service will not start.'
```

Cosh shows the question after the Shell prompt and starts an Agent turn.
It skips the automatic health scan and unrelated startup metadata. Effective
tool-approval information remains visible. After the answer, the same session
accepts Shell commands and natural-language questions.

The initial question goes directly to Core, which selects available ECS trial
access or requests authentication through the existing flow. A missing local
credential does not force `/auth` before trial eligibility is checked. Cancelling
authentication cancels that request; later `/auth` does not replay it.

`--ask` explicitly requests AI for this initial question, even with `COSH_SHELL_AI=off`. Authentication and tool approvals still apply. The existing AI-disabled state remains after the response; later input does not automatically enable AI.

## Control guidance and the resource display

| Command | Effect |
|---|---|
| `cosh guide off` | Persistently stop AI tips; an active external Bash Hook retires at its next callback. |
| `cosh guide on` | Allow invitations again, without resetting suppression history or forcing Hook installation. |
| `cosh guide status` | Show the current guidance preference. |
| `cosh health banner` | Request the compact local health display from a terminal. |
| `cosh health banner off` | Persistently turn off automatic login resource displays. |
| `cosh health banner on` | Restore automatic login resource displays where integrated. |
| `cosh health banner status` | Show the login resource-display preference. |

The controls are independent: turning off AI tips keeps resource facts
available. Neither control disables Cosh's existing natural-language input
or changes tool-approval policy. Invitations are throttled and deduplicated;
ordinary command failures remain quiet.
After an external Bash Hook retires because of `off` or a conflict, `cosh guide on`
restores the preference; a new Bash must pass admission again. It does not
reinstall a Hook removed from an existing session.

An explicit `cosh health banner --disable kernel.oom` omits that check for the
invocation. `--disable` is repeatable; it cannot expand the passive check set.
The login health path has a 500 ms budget and discards late results. Automatic
enhancements skip noninteractive invocations. Hook conflicts or incompatible module
protocols stop failure guidance; the login display checks its own conditions
independently.

The same health configuration applies to Cosh and the login projection. For
example, in the existing `~/.copilot-shell/config.toml`:

```toml
[health.checks]
"resource.swap" = false

[health.policies.login-banner]
disable = ["kernel.oom"]
```

The first setting disables a check everywhere; the second only narrows the
login context. Other context names are `session-startup` and `on-demand`.
These settings cannot enable a check excluded by a context's cost or safety
policy. Keep unrelated provider settings in the file intact.

External Bash invitations do not capture stdout/stderr or automatically rerun commands.
A qualifying failure offers a short command with a concrete event ID:

```bash
cosh --ask 'Analyze this failure' --context <event-id>
```

Executing it passes that exact event's original directory, observed command fragment, literal
arguments, exit status and timestamp to the Agent. Output and expanded argv are explicitly
unavailable. The current directory and original question remain unchanged; Shell history is
never used to guess the latest failure.

The initial allowlist covers `cargo build` / `cargo test` with status 101 and `pytest` with
status 1, including bounded literal arguments and closed quotes. Expansions, substitutions,
globs, redirections, pipelines, truncated or sensitive input, and non-execution
options such as help or collection-only remain silent. The observed tool must resolve
to an external executable; a function overriding that tool is skipped. After a wrapper
function returns, ERR may still report its last external command. Guidance is best-effort:
the fragment does not prove a top-level call, complete input line or reproducible argv.

Private local records retain at most 16 entries and are usable for one hour. Missing,
expired, evicted or unsafe records fail before an Agent request, without substituting
another event. Manual copying of error output is not a prerequisite; the Agent should
explain evidence gaps and follow existing approval rules if reproduction is needed.
AI analysis starts only after the user runs the invitation command.

## Choose the default login shell

| Command | Target and effect |
|---|---|
| `cosh login status` | Inspect the current effective user's login shell and managed recovery state. |
| `cosh login set` | Confirm setting that user's next login to Cosh. |
| `cosh login restore` | Confirm restoring that user's recorded original Bash. |
| `cosh login set --user alice` | Target an explicitly named local account; administrator authorization is required. |
| `cosh login restore --user alice` | Restore the named account's recorded Bash with administrator authorization. |

Changes affect new logins; existing sessions continue. Without `--user`,
`sudo cosh login set` targets root, not the invoking user's account.
`--yes` skips the interactive confirmation, not authorization or safety checks.
Changing another account requires an explicit name.

Login changes require a qualified system package with the lifecycle capability.
An unqualified build refuses `set` without changing the account. Conflicting
external shell changes, untrusted records, and unavailable authorization do not
trigger a forced overwrite. Restoring an unmanaged account does not invent an
original Shell.

Run `cosh guide` for interactive entry, initial questions, independent controls and
default-login commands. No-argument help accesses no configuration or state.
The guide switch covers startup AI suggestions, passive failure help and
default-login offers; it does not disable direct questions.

## Display language

The default is `auto`, independent of region. A valid `COSH_SHELL_LANG` overrides
Cosh's `[ui].language`. `auto` uses the first non-empty value among `LC_ALL`,
`LC_MESSAGES` and `LANG`. Chinese locales use Chinese; English, `C`/`POSIX` and
unsupported locales use English without falling through to lower-priority variables.
Explicit `zh-CN` / `en-US` fixes the language. Bash locale is unchanged; no region lookup occurs.

The management feedback for `guide`, `health banner` and `login` uses the language
environment override and locale without reading the config file, so language
configuration cannot block these commands. Resource displays and Cosh sessions
retain the configuration precedence above.

Passive guidance reads regular config files only, up to 256 KiB. Symlinks, special files
and read failures cause a quiet retreat rather than a guessed language. A valid explicit
language environment override needs no config read. Native Cosh config loading is unchanged.

## State failures and partial collection

Display controls do not wait on a blocking language config. Cosh health suggestions retain the four-hour warning and thirty-minute critical cooldowns, with worsening conditions eligible again. A suggestion is claimed under a nonblocking lock and saved atomically before display. Corrupt state, lock contention or timeout leaves resource facts visible without a suggestion.

Existing history is retained. The default Cosh private directory and history permissions are safely tightened to 0700/0600; a custom history path requires a safe private directory. Partial filesystem collection keeps valid facts while marking failed scope. Service suggestions name the known service and ask to verify its current state.

## ECS automatic access

The first explicit question reuses [Core's ECS automatic-access flow](../core/providers.md)
when eligible. Core owns eligibility, activation, suppression and the `ecs_identity`
model fixed to `auto`. Bash startup does not write a provider or model into configuration.
Existing explicit provider choices remain authoritative and are not migrated or replaced.
Fresh-user real-model and target-image qualification remain separate acceptance scopes.

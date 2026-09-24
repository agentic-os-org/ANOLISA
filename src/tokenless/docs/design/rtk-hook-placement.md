# RTK Hook Placement

[中文版](rtk-hook-placement_zh.md)

## Purpose

Record the evaluation of moving RTK from the PreTool command rewrite to a
PostTool output filter, and publish the wrapper contract that host risk
classifiers consume for as long as the rewrite stays the delivery mechanism.

Tokenless injects RTK in a PreTool hook: Core asks `rtk rewrite` for the RTK
form of the model's shell command, then anchors an attributed wrapper around
it (`pre_tool_with_optional_rtk` and `anchor_rtk_prefix` in
`crates/tokenless-runtime/src/entry.rs`). A host that classifies command risk
after its PreTool hooks have run therefore sees

```
env TOKENLESS_AGENT_ID=… TOKENLESS_SESSION_ID=… TOKENLESS_TOOL_USE_ID=… TOKENLESS_DATA_DIR=… /usr/bin/rtk <payload>
```

instead of the command the model asked for. That coupling already cost one
host its read-only auto-approval (#3415), was patched inside that host with a
private recognition table (#3436), and was narrowed by making rewriting opt-in
(#3432). None of the three removes the coupling: every host with a risk,
approval, or audit path still has to recognize a tokenless-internal command
shape and still has to decide which `TOKENLESS_*` assignments it may ignore.

## Question

Can RTK move to PostTool — the original command executes untouched and the
hook compresses its output before the model sees it — so that no host ever has
to recognize a wrapper?

**No, not as a replacement.** Post-tool filtering loses all RTK savings on the
hosts that cannot replace live tool output, covers roughly a quarter of the
commands RTK rewrites today, filters without knowing what the command asked
for, records no statistics, and duplicates the native PostTool pipeline on the
families it does cover. The wrapper contract below is adopted instead; a
post-approval rewrite seam remains the only structurally complete answer and
is tracked separately.

## How RTK is wired today

Three facts frame the evaluation.

1. **PreTool owns the command.** `rtk rewrite` exit codes 0 and 3 mean
   "rewrite", 1 and 2 mean "leave it alone". On a rewrite, Core replaces the
   tool arguments, so the wrapper is both what the shell runs and what any
   later classifier sees.
2. **PostTool owns the output, and RTK output bypasses it.** The adapter
   records `output_optimization: "rtk"` per tool call (`mark_rtk_optimized`)
   and the PostTool hook consumes that mark (`consume_output_optimization`),
   so an RTK-produced result is never compressed twice.
3. **Build and test commands are already reserved for PostTool.**
   `is_build_log_owned_command` returns passthrough for cargo
   build/check/clippy/install/test, pytest, npm and pnpm test/build/jest,
   npx jest, go build/test/vet, and make, so the native `BuildLogCompressor`
   owns that output rather than RTK.

## Route A — move RTK to PostTool (`rtk pipe`)

RTK does have a post-hoc mode: `rtk pipe [-f <filter>]` reads stdin, applies
one named filter, and prints the result. Six findings decide the route. Code
facts below are from RTK v0.49.0, the revision `scripts/setup-rtk.sh` pins.

### A1. Only some hosts can replace live tool output

PreTool rewriting needs one capability — argument replacement — and every host
with a `rewrite` hook has it. PostTool compression needs the host to replace
the model-visible result:

| Host | Post-tool live replacement | Mechanism |
|------|---------------------------|-----------|
| Cosh-NG | yes | `hookSpecificOutput.updatedToolResponse` |
| Claude Code ≥ 2.1.121 | yes | `hookSpecificOutput.updatedToolOutput` |
| Claude Code < 2.1.121 | no | version gate fails open, compression disabled |
| Qoder CLI | yes | `updatedToolOutput` (string slot) |
| OpenCode | yes | adapter maps to `tool.execute.after` |
| Hermes | yes | `transform_tool_result` |
| QwenPaw | yes | plugin sets `replace_output` |
| DSH | partially | `tools/post-execute`, one replaceable content shape |
| Codex | **no** | `PostToolUse` rejects suppression and replacement |
| OpenClaw | **no** | `tool_result_persist` rewrites the transcript only |
| Qwen Code | **no** | shared hook: `additionalContext`-only branch |

Codex states the constraint explicitly (`adapters/tokenless/codex/README.md`):
its PostToolUse cannot suppress or replace output, so additive injection would
leave the original in place and grow the prompt — which is why that adapter
ships `rewrite` plus additive `response-diagnostics` and nothing else.
OpenClaw's persist hook rewrites what OpenClaw stores, not what the model
already received ([response compression](../response-compression.md), path 1).
Qwen Code reaches the shared hook, whose capability branch leaves
`additionalContext`-only hosts at `can_replace = False`
(`adapters/tokenless/common/hooks/compress_response_hook.py`).

On those hosts a post-tool RTK saves exactly zero tokens while the pre-tool
rewrite still saves them. Route A is not a migration; it is a per-host feature
loss.

### A2. `rtk pipe` covers a fraction of what `rtk rewrite` claims

RTK v0.49.0 declares 86 `Commands` variants. Eighteen are RTK's own utilities,
hook plumbing, or deliberate pass-throughs (`init`, `gain`, `config`,
`telemetry`, `rewrite`, `hook`, `recall`, `pipe`, `run`, `proxy`, …), leaving
68 output-optimizing proxies. `pipe_cmd::resolve_filter` accepts 27 aliases covering 23 output
families: cargo-test, pytest, go-test, go-build, ctest, tsc, vitest, grep/rg,
find/fd, git-log, git-diff, git-status, log, mypy, ruff-check, ruff-format,
sqlfluff-lint, prettier, phpunit, pest/paratest/php-test, ecs, phpstan, pint.

A 37-command probe of `rtk rewrite` (appendix) found 32 commands with an RTK
equivalent. Twelve of those 32 have a pipe filter that could serve them
post-hoc, and three of the twelve are build/test commands that PreTool already
reserves for the native pipeline — so nine commands, about 28% of the rewritten
surface, could survive Route A. The other 23 (72%) would lose all RTK savings,
including the largest outputs in the probe: `ps aux` (105 638 B raw, 2 616 B
through `rtk ps aux`) has no filter, and neither do `ls`, `tree`, `wc`,
`git branch`, `docker`, `kubectl`, `curl`, `gh`, `jest`, or `read`.

### A3. Pipe filters are not request-aware, and some expect RTK-instrumented input

The exec path re-runs the command with RTK's own arguments. The pipe path sees
only text, so it cannot honour what the command asked for:

- `git log`: the exec path injects
  `--pretty=format:%h %s (%ar) <%an>%n%b%n---END---`. `git_log_wrapper` calls
  `filter_log_output(input, 50, false, false)` — `user_format = false` — so
  the filter splits on the `---END---` markers that only RTK's own format
  produces. Native `git log` output has none, and the whole log collapses into
  one commit block. Measured on `git log -20`: exec mode returns all 20
  commits in 5 952 B (72% below raw); pipe mode returns a single commit plus
  `[+319 lines omitted]` in 208 B. That 99% "saving" is a discarded answer,
  not compression.
- `git status`: the exec path runs `git status --porcelain -b`, and
  `format_status_output` parses porcelain (`## branch` header, XY codes). Fed
  native human-readable status, the same filter returns it essentially
  untouched: 286 B raw, 283 B through `rtk pipe -f git-status`, against 101 B
  from `rtk git status`.
- `git diff`: `compact_diff(input, 200)` applies a fixed cap regardless of
  `-U`, `--stat`, pathspecs, or how much of the diff the model asked for.

Post-hoc filtering can therefore be more lossy than the exec path while
looking more efficient, and the loss is invisible: the native pipeline records
a stash entry and leaves a retrieve marker, `rtk pipe` leaves nothing behind.

### A4. Pipe mode records no statistics

The tokenless stats integration is a patch on RTK's execution tracking:
`third_party/patches/rtk-tokenless-stats.patch` inserts
`record_to_tokenless_stats(original_cmd, rtk_cmd, input, output)` inside
`impl TimedExecution`. `Commands::Pipe` calls `pipe_cmd::run` directly, never
constructs a `TimedExecution`, and `pipe_cmd.rs` contains no tracking call at
all. Measured with the patched v0.49.0 build and `TOKENLESS_STATS_ENABLED=1`:
`rtk grep -rn 'fn ' crates` writes one `stats` row
(`operation = rewrite-command`, with agent, session and tool-use attribution
and before/after character and token counts); the same output through
`rtk pipe -f grep` creates no stats database at all.

Under Route A, RTK savings would therefore disappear from both `rtk gain` and
`tokenless stats` unless PostTool re-derived them — and PostTool cannot: it
sees the original command and, at best, a filter name, so the
`original_cmd`/`rtk_cmd` pair that stats records today has no source.

### A5. Buffering, caps, and timing

`pipe_cmd::run` reads all of stdin into a `String`, hard-capped at
`RAW_CAP = 10 MiB`, and bails with an error above the cap. A hook can only
turn that into fail-open passthrough, after paying for a subprocess and a full
copy of the output; the exec path filters while the child streams. Hosts also
bound tool output before any hook sees it: tokenless's own Grep documentation
distinguishes "all received rows" from "all possible matches before host
truncation" ([runtime design](runtime-library.md)), so post-hoc filtering can
only compress whatever survived the host's limit, while the exec path keeps
the large output from ever existing. Finally, the PostTool hook budget
is 8 s shared with the native pipeline, and Route A spends part of it on a
second compressor per shell call.

### A6. Where a pipe filter exists, the native pipeline already covers it

The 23 pipe families are build/test logs, grep/find listings, diffs, and linter
output — exactly the domains the Rust PostTool pipeline implements in-process
(`BuildLogCompressor`, `SearchResultsCompressor`, `post_tool/diff.rs`) with
dialect state machines, stack-trace protection, stash-backed reduction, and
retrieve markers. Running `rtk pipe` over the same output would rewrite it
twice, which is the situation `is_build_log_owned_command` exists to prevent.

### Route A verdict

Rejected as a replacement for the PreTool rewrite. Nothing in the evaluation
supports moving RTK off the execution path, and the narrow families where
post-hoc filtering would add value are already served — better, and
recoverably — by the native pipeline.

## Route B — keep the rewrite, publish the wrapper contract (adopted)

Route B accepts that the wrapper is visible to hosts and removes the reason
each host has to reverse-engineer it: the form becomes a published contract
with a documented grammar, key semantics, unwrap algorithm, conformance
vectors, and version rule (next section). A host that follows the contract
classifies the effective command instead of the wrapper, and stops maintaining
a private table.

Route B does not make tokenless invisible. It makes the wrapper's shape
somebody else's promise rather than each consumer's guess, which is what #3436
had to build for one host.

## Route C — rewrite after approval, at a host execution seam (future)

The only design that satisfies "the classifier sees the original command" and
"RTK keeps its exec-path savings" at the same time is to move the rewrite
later: the host classifies and approves the model's command, then substitutes
the RTK form at the point where it actually spawns the process. That needs a
post-approval execution seam per host, so it is a host-by-host integration
rather than a tokenless-side change, and it is not attempted here. Where a
host exposes such a seam, Route C is strictly better than both A and B; where
it does not, Route B is the floor.

## RTK wrapper contract v1

**Producer.** Tokenless Core, `anchor_rtk_prefix`. Emitted only in PreTool
responses carrying `action: replace_arguments` or `block_and_suggest` together
with `output_optimization: "rtk"`.

**Grammar.**

```
wrapped_segment  := [transparent_prefix] wrapper payload
transparent_prefix := user tokens RTK config preserves (for example `sudo`)
wrapper          := "env" SP assignment SP assignment SP assignment SP assignment SP rtk_path
assignment       := key "=" quoted_value
key              := TOKENLESS_AGENT_ID | TOKENLESS_SESSION_ID
                  | TOKENLESS_TOOL_USE_ID | TOKENLESS_DATA_DIR
```

- The four keys always appear, in the order shown.
- `quoted_value` is the value verbatim when every byte is ASCII alphanumeric
  or one of `/ _ - .`; otherwise it is single-quoted with each embedded `'`
  replaced by `'\''` (`shell_quote`).
- `rtk_path` is an absolute path quoted by the same rule. Treat it as opaque:
  it is `/usr/bin/rtk` for package installs and the resolved local build
  otherwise, and it may contain spaces.
- `payload` is RTK's own form of the original segment with its bare `rtk`
  token replaced by `wrapper`. It starts with an RTK subcommand, not with
  `rtk`.
- **Every rewritten segment carries its own wrapper.** `a && b` becomes
  `wrapper a && wrapper b`; wrappers also appear inside `$(…)` subshells,
  after `sudo`, and after the user's own environment assignments. Backtick and
  double-quoted command substitutions are left untouched because they need the
  host parser. "The command starts with `env TOKENLESS_`" is therefore **not**
  a sufficient recognition rule.

**Key semantics for consumers.**

| Key | Meaning | Consumer rule |
|-----|---------|---------------|
| `TOKENLESS_AGENT_ID` | stats attribution | ignore when classifying risk |
| `TOKENLESS_SESSION_ID` | stats attribution, may be empty | ignore when classifying risk |
| `TOKENLESS_TOOL_USE_ID` | stats attribution, may be empty | ignore when classifying risk |
| `TOKENLESS_DATA_DIR` | absolute tokenless state directory RTK writes stats to | ignore when classifying risk; never treat as user input |

**Unwrap algorithm.**

1. Split the command into segments at `&&`, `||`, `;`, `|`, newlines, and
   `$(…)` boundaries — the same segmentation `bare_rtk_offsets_by_segment`
   uses.
2. In each segment, skip leading environment assignments and a leading `env`
   token. The segment is wrapped when the next four tokens are exactly the
   contract keys in order, each as `KEY=value`, and the token after them is a
   path whose basename is `rtk`.
3. The segment's **effective command** is every token after that `rtk` path.
   Classify it together with any `transparent_prefix` that preceded the
   wrapper: `sudo` plus a payload is still privileged.
4. A command is tokenless-wrapped when at least one segment is. Segments that
   do not match stay as they are.

**What the contract does not promise.**

- The model's original text is not recoverable from the wrapper. RTK's
  rewrite is part of the payload (`cat README.md` → `read README.md`,
  `head -50 main.rs` → `read main.rs --max-lines 50`). A host that wants to
  audit or display what the model asked for must capture it at PreTool time.
- The wrapper is not proof of provenance. A model or a user can type the same
  string. Unwrapping is safe for *classification* — the payload is what
  actually runs — but a host must never grant trust, skip approval, or widen
  an allowlist merely because a command matches the wrapper shape. Provenance
  comes from the host's own hook pipeline, which knows whether it invoked the
  tokenless rewrite hook for this call.

**Versioning.** v1 is the form produced by `anchor_rtk_prefix` today and is
pinned by tests in `crates/tokenless-runtime/src/entry.rs`
(`pre_tool_rtk_wrapper_matches_published_contract_v1` and
`pre_tool_anchor_preserves_quoted_arguments_and_handles_subshells`). Any change
to the key set, key order, quoting rule, or segmentation bumps the contract to
v2, updates this document, and keeps emitting v1 for one release so consumers
can migrate. A consumer that meets an unknown form must treat the whole
command as opaque and classify it as-is — fail closed, never guess.

**Conformance vectors.**

| Wrapped command | Effective command(s) |
|-----------------|----------------------|
| `env TOKENLESS_AGENT_ID=a TOKENLESS_SESSION_ID=s TOKENLESS_TOOL_USE_ID=t TOKENLESS_DATA_DIR=/d /usr/bin/rtk git status` | `git status` |
| `env … /usr/bin/rtk grep -E 'foo \| rtk bar' src && env … /usr/bin/rtk git status` | `grep -E 'foo \| rtk bar' src`, `git status` |
| `echo $(env … /usr/bin/rtk git status)` | `echo $(…)` with inner effective command `git status` |
| `sudo env … /usr/bin/rtk git status` | `sudo git status` (privileged) |
| `RUST_BACKTRACE=1 env … /usr/bin/rtk cargo test` | `RUST_BACKTRACE=1 cargo test` |
| `env TOKENLESS_AGENT_ID='a b' … /opt/my tools/rtk ps aux` | `ps aux` (quoted values and spaced paths) |
| `env TOKENLESS_AGENT_ID=a … /usr/bin/rtk read README.md` | `read README.md` — original `cat README.md` is **not** recoverable |

## Follow-ups

1. Publish this contract to host owners and retire private recognition tables
   in favour of it; the Cosh-NG table added by #3436 is the first candidate
   (separate change, outside this component).
2. Capture the model's original command at PreTool time in host integrations
   that want audit fidelity, since the wrapper cannot carry it.
3. Investigate Route C per host that exposes a post-approval execution seam.
4. Re-run this evaluation if RTK ever ships request-aware post-hoc filters
   (filters that receive the original command line, not just its output) or if
   Codex and OpenClaw gain live output replacement.

## Appendix — reproduction

Coverage and fidelity claims come from the pinned RTK source
(`bash scripts/setup-rtk.sh`, then read `src/cmds/system/pipe_cmd.rs`,
`src/cmds/git/git.rs`, `src/hooks/rewrite_cmd.rs`, `src/core/stream.rs`) and
from the tokenless stats patch (`third_party/patches/rtk-tokenless-stats.patch`).

Size measurements were taken in this component's source tree with the pinned
RTK v0.49.0 built by `scripts/setup-rtk.sh`, the same tree as input, and byte
counts from `wc -c`:

```bash
raw=$(git log -20 2>&1 | wc -c)
exec=$(third_party/rtk/target/release/rtk git log -20 2>&1 | wc -c)
pipe=$(git log -20 2>&1 | third_party/rtk/target/release/rtk pipe -f git-log | wc -c)
```

| Command | raw B | `rtk <cmd>` B | `rtk pipe -f` B | filter |
|---------|-------|---------------|-----------------|--------|
| `git log -20` | 21 212 | 5 952 | 208 | `git-log` |
| `git diff HEAD~3` | 64 649 | 23 186 | 8 413 | `git-diff` |
| `git status` | 286 | 101 | 283 | `git-status` |
| `git branch -a` | 6 022 | 401 | — | none |
| `grep -rn 'fn ' crates` | 154 280 | 19 337 | 37 306 | `grep` |
| `find . -name '*.rs'` | 12 989 | 1 314 | 1 986 | `find` |
| `ls -al crates` | 574 | 162 | — | none |
| `wc -l entry.rs lib.rs` | 101 | 34 | — | none |
| `cat entry.rs` | 71 090 | 71 090 | — | none |
| `ps aux` | 105 638 | 2 616 | — | none |

`—` means `rtk pipe` has no filter for that command, so the post-tool route
returns the raw output unchanged. `cat entry.rs` shows the exec path passing
source through as well: RTK savings are command-specific, not universal.

The 37-command `rtk rewrite` probe ran each command through `rtk rewrite` and
recorded the exit code and rewritten form: 32 commands were rewritten (exit 3)
and five had no RTK equivalent (exit 1: `git diff`, `git show HEAD`,
`npm test`, `env`, `jq . package.json`). Of the 32, twelve map to an existing
pipe filter and 20 do not.

Fidelity check on the losing case: `rtk git log -20` returns all 20 commits,
while `git log -20 | rtk pipe -f git-log` returns one commit followed by
`[+319 lines omitted]`.

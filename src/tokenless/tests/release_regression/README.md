# Installed-package release regression

[中文版](README_zh.md)

This opt-in Linux x86-64 suite installs local Tokenless npm tarballs in existing
Claude Code and OpenCode images, and local Python wheels in an AgentScope 2.x
image, then exercises the installed Core and integrations.
Images contain the Agent and its dependencies, but no Tokenless or RTK. Nothing
is published, and the Tokenless source tree is not mounted in the container.

## Prepare inputs

Use the existing images `tokenless-test-agent-claude-code:2.1.259` and
`tokenless-test-agent-opencode:1.18.27`. Both need Node.js, npm, Python 3.13,
Bash, and a glibc version compatible with the supplied binaries. Image IDs are
recorded; these tags are local test assets, not public registry images.
AgentScope uses `tokenless-test-agent-agentscope2:2.0.7.post1`, with Python 3.13
and the Agent's dependencies already installed. It needs no Node.js or shell tool.

Build the current native binaries and package them using the normal packer:

```bash
make build
make npm-package
make python-wheel agentscope-wheel
```

Prepare a clean checkout of `pillarjs/path-to-regexp` at
`8877f41873e37a30258d3935feaf1d2679321735`, the same real project used during
BuildLog development. Capture its dependency lock once with
`npm install --package-lock-only --ignore-scripts`, or reuse the lock from a
previous recorded run. The suite archives tracked files, copies that lock,
and runs `npm ci` inside each container. It records the lock's SHA-256; a
different lock is a different workload environment.

## Run

From the Tokenless component root:

```bash
# Package installation and deterministic Core checks; no model calls.
python3 tests/release_regression/run.py --project /path/to/path-to-regexp

# Also run live Agent tasks in each selected image.
python3 tests/release_regression/run.py \
  --project /path/to/path-to-regexp \
  --api-key-file /tmp/tokenless-openclaw-api-key
```

Use `--agents claude-code`, `--agents opencode`, or `--agents agentscope2` for a focused iteration.
`--model` defaults to `deepseek-v4-flash-0731`. Live tasks use the
[Bailian TokenPlan endpoints](https://help.aliyun.com/en/model-studio/base-url).
The key is mounted read-only and passed in process environment or the in-memory AgentScope credential;
it is never copied into a package, configuration file, or report. Live runs
consume the supplied account's model quota.

The runner creates a new `/tmp/tokenless-release-regression.*` directory and
prints its path. Each Agent has its own installation, project, state, and
report. Nonzero exit means a check failed. Without a key, live checks are
explicitly `not_run`; that result is not a complete release sign-off.

## Evidence and acceptance

- This opt-in suite does not replace the build gates. Recovery API changes also require
  checking the standalone `benchmark/l1-compressor` and `benchmark/l2-module` workspaces:
  main-workspace `cargo test --workspace` does not compile either. Run each workspace's
  `cargo fmt --all -- --check`, `cargo clippy --all-targets --locked -- -D warnings`, and
  `RUST_MIN_STACK=16777216 cargo test --release --locked`, as the Tokenless CI job does.
- Installation uses both local npm tarballs offline. The resolved executable
  must match `target/release/tokenless` by SHA-256. Plugins are enabled through
  their installed scripts, not a test-supplied replacement hook.
- AgentScope installs the two supplied wheels with `--no-index --no-deps` in an isolated
  environment over the image's existing Agent dependencies. Wheel SHA-256 values are recorded.
- Real workload: unmodified `npm test` must report 484 passing tests. A
  deliberately missing Vitest config supplies the separate failure case;
  the suite does not claim this injected configuration error occurred naturally.
- Core checks cover BuildLog reduction, full-data TOON preference, record
  reduction, Tool Error and RTK bypass, file and plain-text passthrough,
  no savings, dry-run, unauthorized Retrieve, and byte-exact CLI recovery.
- The existing L1 records fixture checks full-data TOON. A derived fixture
  doubles its messages so TOON alone falls below the lossless selection gate.
  That fixture checks recovery contracts only; its savings are not evidence
  about real workload performance. The target record is selected from records
  actually omitted by Core before asking the model to retrieve it.
- Each live shell task must show one applied compression, one emitted Stash entry,
  one model-issued standalone Retrieve command, and one CLI Retrieve hit.
  The recovered output must bypass compression. The final answer must cite
  an omitted test or the omitted record, as appropriate.
- AgentScope runs the synthetic record task with a custom static Retrieve Tool and no shell tool.
  It records actual model inputs and responses, verifies an unchanged tool list, BeforeModel
  visibility authorization, byte-identical recovered data, one `embedded` Retrieve hit, and no
  recompression. `embedded` and CLI events are reported separately. A capture failure is a test
  harness failure, not evidence that the product's retrieval failed.
- New hints must contain the complete `If needed` instruction and a bare 24-hex hash, never a
  newly generated angle-bracket marker. Logs include failed attempts and partial output on timeouts.
- Reports retain provider usage and tool-output token estimates separately.
  `saved_minus_retrieved_tokens` subtracts recovered payload tokens from
  single-output savings; it is not whole-session savings or a billing estimate.
  Host trailing-newline normalization is reported separately from exact CLI
  payload recovery. The suite does not pretend to be a randomized Agent A/B test.

Reports, dependency locks, logs, and SQLite evidence remain outside Git.
The suite does not test macOS, old-glibc compatibility, other installers, or
every Agent. Provider failures, host delivery limits, and Core failures must
be assessed from their recorded evidence before release.

# anolisa-tokenless

LLM token optimization toolkit — content-aware compression, command rewriting, and diagnostics.

Tool Ready's legacy pre-call checks remain registered but are hard-disabled. Post-tool environment
failure diagnostics are independent and remain active where the host supports additive context.

> **Release status:** These npm packages are private and are not currently
> published to the public registry. This document describes the intended
> package layout and post-publication workflow; its platform rows are build
> targets, not a list of packages that users can install today.

## Install after publication

```bash
npm install -g anolisa-tokenless
```

This automatically installs the correct prebuilt binary for your platform.

## Binaries

| Binary | Description |
|--------|-------------|
| `tokenless` | Main CLI — content-aware protocol, direct compression, retrieval, and stats |
| `rtk` | Command rewriting engine (filters CLI output noise) |

TOON (Token-Oriented Object Notation) encoding is built into `tokenless`
via the `toon-format` library — see `tokenless compress-toon` /
`tokenless decompress-toon` below.

## Platform Support

| Platform | Architecture | Package |
|----------|-------------|---------|
| Linux (glibc) | x86_64 | `@anolisa/tokenless-linux-x64` |
| Linux (glibc) | aarch64 | `@anolisa/tokenless-linux-arm64` |
| macOS | x86_64 (Intel) | `@anolisa/tokenless-darwin-x64` |
| macOS | aarch64 (Apple Silicon) | `@anolisa/tokenless-darwin-arm64` |

The correct platform-specific binaries are automatically installed via `optionalDependencies`.

> **glibc only:** the Linux binaries target `*-unknown-linux-gnu` with a
> pinned minimum baseline of **GLIBC 2.17**, and the Linux platform packages
> declare `"libc": ["glibc"]`. musl-based distributions (e.g. Alpine) are not
> supported — build from source there instead.

## Agent Adapters

The root package bundles the Tokenless adapters for Agent products (cosh,
OpenClaw, Hermes, Qoder, Claude Code, Codex, OpenCode, Qwen Code, and DeepSeek Harness). The
adapter hooks are plain bash/python scripts — OS and architecture independent —
so they work on both Linux and macOS.

DeepSeek Harness uses its dedicated `tools/post-execute` path for JSON response compression and
environment-error attribution. It does not use the Protocol v2 PostTool Pipeline.

On install, they are copied to the user-level data directory searched by the
hook dispatcher:

```
~/.local/share/anolisa/adapters/tokenless/
```

To register an adapter with an Agent product, run its install script,
e.g. for Claude Code:

```bash
bash ~/.local/share/anolisa/adapters/tokenless/claude-code/scripts/install.sh
```

## Usage

```bash
# Run the Protocol v2 PostTool operation used by shared Agent hooks
jq -n --rawfile content build.log \
  '{protocol_version: 2, operation: "post_tool",
    attribution: {agent_id: "manual"},
    input: {result_kind: "tool", tool_name: "Bash", content: $content,
      status: "success", content_origin: "command_output",
      output_optimization: "none",
      capabilities: {replace_output: true, recovery: {kind: "none"},
                     replace_with_text: true}}}' \
  | tokenless compress

# Compress an API response
tokenless compress-response -f response.json

# Compress tool schemas
tokenless compress-schema -f tools.json

# Encode JSON to TOON format (payloads under 500 characters pass through
# unchanged by default; use --min-toon-chars 0 to encode them anyway)
tokenless compress-toon -f data.json

# Decode TOON back to JSON
tokenless decompress-toon -f data.toon

# Command rewriting (filters CLI output noise)
rtk ls -la
# Or use rewrite subcommand
rtk rewrite "ls -la"

# Report the current hard-disabled Tool Ready status
tokenless env-check --all
```

## Build from Source

Source builds are **Linux-only**. macOS users should install the prebuilt
binaries via `npm install -g anolisa-tokenless`; the macOS CLI binaries are
cross-compiled from Linux and published as npm platform packages.

If no prebuilt binary is available for your platform, or you want to build on
Linux from source:

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/src/tokenless
make build
make install
```

### Prerequisites

- **Linux** host (glibc-based distribution)
- **Rust** toolchain >= 1.91 (required by rtk v0.49.0)
- **just** — build runner for rtk setup
- **Git** — for rtk source download

## Packaging for npm

The npm packer reads prebuilt native executables from this fixed layout:

```text
target/npm-prebuilt/
├── linux-x64/{tokenless,rtk}
├── linux-arm64/{tokenless,rtk}
├── darwin-x64/{tokenless,rtk}
└── darwin-arm64/{tokenless,rtk}
```

Validating Linux packages requires GNU `readelf` from binutils. The packer
rejects binaries that require GLIBC symbols newer than the supported 2.17
baseline.

Package one target:

```bash
node npm/scripts/package-npm.js --target linux-arm64
```

For the current host, the Make entry point can build the native executables
before invoking the same packer:

```bash
make npm-package
```

Package the complete matrix:

```bash
node npm/scripts/package-npm.js --all
# or
make npm-package-all
```

Before copying anything, the packer checks that all three files exist and that
their ELF or Mach-O architecture matches the selected npm target; it never
executes cross-target binaries. Prebuilt Linux inputs must retain the documented
GLIBC 2.17 compatibility baseline.

### Publishing

```bash
make npm-publish
```

This packages all targets, then publishes the four platform packages first
and the root package last. The registry is pinned to
`https://registry.npmjs.org/` both in the generated manifests
(`publishConfig`) and on the publish command line, and already-published
versions are skipped so a partially failed run can be safely re-executed.

## License

Apache License 2.0

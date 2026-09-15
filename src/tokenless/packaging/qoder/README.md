# Standalone Qoder plugin packaging

[中文版](README_zh.md)

Build a Qoder plugin that carries its own Tokenless runtime and shared hooks.
This developer packaging entry point does not install software or publish a package.
Normal component installation remains `anolisa install tokenless` followed by
`anolisa adapter enable tokenless qoder`.

## Build

Use trusted, checksum-verified prebuilt binaries from the same Tokenless release
as the source checkout. Each selected directory contains `tokenless`, `rtk`, and
`version.txt` (the Tokenless release version). Binary architecture is checked;
`version.txt` is caller-supplied provenance, not proof of binary identity.
The publisher must pin source commits and archive checksums. Do not build on macOS.

```text
prebuilt/
  linux-x64/{tokenless,rtk,version.txt}
  linux-arm64/{tokenless,rtk,version.txt}
  darwin-arm64/{tokenless,rtk,version.txt}
```

```bash
python3 packaging/qoder/package.py --prebuilt-root /path/to/prebuilt \
  --target linux-x64 --target linux-arm64 --target darwin-arm64 \
  --output /path/to/new-plugin-directory
qodercli plugins validate /path/to/new-plugin-directory
qodercli plugins install /path/to/new-plugin-directory --scope user
```

The output must not exist. Select only platforms for which verified binaries are
available. `darwin-x64` is accepted for separately built assets; this does not
imply a published Intel Mac release. Output includes a stamped native manifest,
shared hooks, native binaries, a platform launcher, licenses and SHA-256 inventory.
No npm install lifecycle script or model-invoked Skill is required.

## Runtime and limitations

Restart Qoder CLI or run `/plugins reload` after installation. Bash and Python
3.10+ are required. Missing dependencies emit a diagnostic to stderr and pass
through the original tool result. The plugin never downloads code on tool calls
or changes the user's shell configuration. It uses its own binary version even
when another Tokenless is on PATH. The legacy Tool Ready hook remains disabled.

The standalone bundle sets `TOKENLESS_DISABLE_SHELL_RECOVERY=1` only for its
hooks. Its private PATH is not inherited by Qoder's shell, so Marker-based
compression requiring `tokenless retrieve` is disabled, including table sampling
that needs recovery. Command rewriting and compression that does not require
recovery remain available. Existing ANOLISA-installed adapters retain recovery.
The standalone bundle omits the global `tokenless-stats` slash command; run
`/path/to/new-plugin-directory/bin/tokenless stats` explicitly instead.

For rollback, disable or uninstall the plugin with Qoder's native plugin manager.
Do not enable a standalone copy and an ANOLISA-installed copy simultaneously.
Validate real model-visible output separately: structural validation and mock
contract tests alone do not establish end-to-end savings.

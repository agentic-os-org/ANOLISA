# ws-ckpt packaging for maintainers

[中文版](../../zh/ws-ckpt/packaging.md)

## Local packaging and tests

Run from the repository root on Linux:

```bash
make -C src/ws-ckpt build
make -C src/ws-ckpt package-raw
make -C src/ws-ckpt test-package-raw
```

## Preview

Use the branch to validate and its workspace version:

```bash
gh workflow run release-preview.yaml --repo agentic-os-org/ANOLISA \
  --ref <branch> -f component=ws-ckpt -f version=<workspace-version>
```

Download the Linux x86_64 and aarch64 build artifacts from that workflow run.

## Release tag

After updating the release version, push `ckpt/v<version>` to trigger the
Release workflow. The tag version must match the workspace version.
The workflow attaches both Linux architectures' prebuilt assets to the GitHub
release. Use a new version tag for each release.

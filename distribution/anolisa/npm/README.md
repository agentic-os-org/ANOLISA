# @anolisa/cli

ANOLISA CLI — Agentic OS component lifecycle manager.

## Install

```bash
npm install -g @anolisa/cli
```

## Usage

```bash
# Install a component
anolisa install tokenless

# Check component status
anolisa status tokenless

# Manage adapters
anolisa adapter scan
anolisa adapter enable tokenless openclaw
```

## Platform Support

| Platform | Architecture | Package |
|----------|-------------|---------|
| Linux | x86_64 | `@anolisa/cli-linux-x64` |
| Linux | aarch64 | `@anolisa/cli-linux-arm64` |
| macOS | arm64 | `@anolisa/cli-darwin-arm64` |

The correct platform-specific binary is automatically installed via `optionalDependencies`.

## Build from Source

If no prebuilt binary is available for your platform:

```bash
git clone https://github.com/alibaba/anolisa.git
cd anolisa/distribution/anolisa
cargo build --release -p anolisa-cli
```

## License

Apache License 2.0

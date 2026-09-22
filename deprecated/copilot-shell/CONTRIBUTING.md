# Contributing Guide

[中文版](CONTRIBUTING_zh.md)

This document explains how to participate in Copilot Shell development.

## Environment Setup

### System Requirements

- Node.js ≥ 20
- npm ≥ 9
- Git

### Get the Code

```bash
git clone https://github.com/alibaba/anolisa.git
cd deprecated/copilot-shell
```

### Install Dependencies

```bash
make deps
```

This runs `npm install` and initializes Husky Git hooks.

## Development Workflow

### Build

```bash
make build
```

Build uses esbuild; output goes to each package's `dist/` directory.

### Run

```bash
# Run the build output directly
node packages/cli/dist/index.js

# Or use npm script
cd packages/cli && npm start
```

### Lint

```bash
make lint
```

Includes ESLint and type checking.

### Format

The project uses Prettier for code formatting. Save-on-format in your editor,
or run manually:

```bash
npx prettier --write .
```

### Test

```bash
make test
```

Testing framework is vitest. Test files are co-located with source code,
named `*.test.ts`.

## Code Organization

### Package Structure

| Package                     | Path                   | Responsibility    |
| --------------------------- | ---------------------- | ----------------- |
| `@copilot-shell/cli`        | `packages/cli/`        | CLI entry and TUI |
| `@copilot-shell/core`       | `packages/core/`       | Core engine       |
| `@copilot-shell/test-utils` | `packages/test-utils/` | Test utilities    |

### Module Conventions

- Uses ESM (`"type": "module"`)
- Exports unified through each package's `src/index.ts`
- Type declarations separated from implementation (`types.ts`)

## Commit Conventions

Follow the project root's commit conventions with scope `cosh`:

```
feat(cosh): add --json flag to config command
fix(cosh): handle empty model response gracefully
```

### Pre-commit Checks

Ensure the following pass before each commit:

```bash
make lint
make test
```

Husky pre-commit hook automatically runs lint checks.

## Adding a New Slash Command

1. Create a command file under `packages/cli/src/commands/`
2. Implement the `Command` interface
3. Import and register at the command registry
4. Add corresponding unit tests

## Adding a New Tool

1. Create a tool definition under `packages/core/src/tools/`
2. Implement the tool's `execute` method
3. Register in the tool registry
4. Add approval classification (which modes require confirmation)
5. Write integration tests

## Adding a New Hook Event

1. Define the event type in `packages/core/src/hooks/`
2. Implement the event's input/output schema
3. Trigger the event at the appropriate point in the agent loop
4. Write unit tests
5. Update Hook development documentation

## Integration Tests

Integration tests are located in the `integration-tests/` directory:

```bash
# Run all integration tests
cd integration-tests && npm test

# Run specific tests
npx vitest run integration-tests/hooks/
```

## Release Process

Releases are automated via the `/cosh-dev release` skill:

1. Update version number
2. Generate CHANGELOG
3. Build and verify
4. Create Git tag
5. Push to remote

# Adding CLI Commands

[中文版](../../zh/cosh-ng/adding-commands.md)

## Overview

cosh-cli uses clap to build its command tree, with each subsystem corresponding to a `cmd/<subsystem>.rs` module. Adding a new command requires modifications across three layers: type definitions (cosh-types) → platform implementation (cosh-platform) → CLI entry (cosh-cli).

## Steps

### 1. Define Response Types (cosh-types)

Add or extend data types in `crates/cosh-types/src/`:

```rust
// crates/cosh-types/src/my_subsystem.rs
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MyResult {
    pub field: String,
    pub success: bool,
}
```

Export in `lib.rs`.

### 2. Implement Platform Logic (cosh-platform)

Implement the actual operation in `crates/cosh-platform/src/`:

```rust
// crates/cosh-platform/src/my_subsystem.rs
use cosh_types::error::CoshError;
use cosh_types::my_subsystem::MyResult;

use crate::detect::Distro;

pub fn my_action(distro: &Distro, param: &str, dry_run: bool) -> Result<MyResult, CoshError> {
    if dry_run {
        return Ok(MyResult { field: param.to_string(), success: true });
    }
    // Actual execution logic...
    Ok(MyResult { field: param.to_string(), success: true })
}
```

### 3. Register CLI Command (cosh-cli)

Create `crates/cosh-cli/src/cmd/my_subsystem.rs`:

```rust
use std::time::Instant;

use clap::Subcommand;
use cosh_platform::detect::Distro;
use cosh_platform::my_subsystem;

use crate::{build_meta, print_failure, print_success};

#[derive(Subcommand)]
pub enum MyCommands {
    /// Do something
    DoSomething {
        /// Target parameter
        target: String,
        /// Preview without executing
        #[arg(long)]
        dry_run: bool,
    },
}

pub fn run(action: MyCommands, distro: &Distro, start: Instant) -> i32 {
    match action {
        MyCommands::DoSomething { target, dry_run } => {
            match my_subsystem::my_action(distro, &target, dry_run) {
                Ok(result) => print_success(result, build_meta("my", distro, start, dry_run)),
                Err(e) => print_failure(e, build_meta("my", distro, start, dry_run)),
            }
        }
    }
}
```

Register in `cmd/mod.rs`:

```rust
pub mod my_subsystem;
```

Add the subcommand in `main.rs`:

```rust
#[derive(Subcommand)]
enum Commands {
    // ...existing...
    /// My new subsystem
    My {
        #[command(subcommand)]
        action: cmd::my_subsystem::MyCommands,
    },
}
```

And add the branch in `match cli.command`:

```rust
Commands::My { action } => cmd::my_subsystem::run(action, &distro, start),
```

### 4. Add Integration Tests

Add tests in `crates/cosh-cli/tests/cli_integration.rs`:

```rust
#[test]
fn test_my_command_json_envelope() {
    let output = run_cli(&["my", "do-something", "target", "--dry-run"]);
    let resp: serde_json::Value = serde_json::from_str(&output).unwrap();
    assert_eq!(resp["ok"], true);
    assert_eq!(resp["meta"]["subsystem"], "my");
    assert_eq!(resp["meta"]["dry_run"], true);
}
```

## Design Constraints

| Rule | Description |
|------|-------------|
| JSON output | Always use `CoshResponse<T>` envelope |
| Exit codes | Success = 0, Failure = 1 |
| `--dry-run` | Add it only when the backend can provide a genuinely non-mutating preview; it is an action flag, not a global CLI promise |
| Input validation | Use `validate_*` to check parameters before execution |
| subsystem field | `meta.subsystem` must match the command name |
| Distribution routing | Logic that needs to distinguish distributions goes in `cosh-platform` |

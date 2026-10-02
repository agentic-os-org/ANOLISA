# 新增 CLI 命令

[English](../../en/cosh-ng/adding-commands.md)

## 概述

cosh-cli 使用 clap 构建命令树，每个子系统对应一个 `cmd/<subsystem>.rs` 模块。新增命令
需要依次修改 cosh-types 中的类型、cosh-platform 中的平台实现和 cosh-cli 入口。

## 步骤

### 1. 定义响应类型（cosh-types）

在 `crates/cosh-types/src/` 中新增或扩展数据类型。

```rust
// crates/cosh-types/src/my_subsystem.rs
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct MyResult {
    pub field: String,
    pub success: bool,
}
```

在 `lib.rs` 中导出。

### 2. 实现平台逻辑（cosh-platform）

在 `crates/cosh-platform/src/` 中实现实际操作。

```rust
// crates/cosh-platform/src/my_subsystem.rs
use cosh_types::error::CoshError;
use cosh_types::my_subsystem::MyResult;

use crate::detect::Distro;

pub fn my_action(distro: &Distro, param: &str, dry_run: bool) -> Result<MyResult, CoshError> {
    if dry_run {
        return Ok(MyResult { field: param.to_string(), success: true });
    }
    // 实际执行逻辑...
    Ok(MyResult { field: param.to_string(), success: true })
}
```

### 3. 注册 CLI 命令（cosh-cli）

创建 `crates/cosh-cli/src/cmd/my_subsystem.rs`。

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

在 `cmd/mod.rs` 中注册模块。

```rust
pub mod my_subsystem;
```

在 `main.rs` 中添加子命令。

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

随后在 `match cli.command` 中添加分支。

```rust
Commands::My { action } => cmd::my_subsystem::run(action, &distro, start),
```

### 4. 添加集成测试

在 `crates/cosh-cli/tests/cli_integration.rs` 中添加测试。

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

## 设计约束

| 规则 | 说明 |
|------|------|
| JSON 输出 | 始终使用 `CoshResponse<T>` 信封 |
| 退出码 | 成功 = 0，失败 = 1 |
| `--dry-run` | 仅在后端能提供真正无副作用的预览时添加；它是 action flag，不是 CLI 全局承诺 |
| 输入验证 | 在执行前使用 `validate_*` 检查参数 |
| subsystem 字段 | `meta.subsystem` 必须与命令名一致 |
| 发行版路由 | 需要区分发行版的逻辑放在 `cosh-platform` |

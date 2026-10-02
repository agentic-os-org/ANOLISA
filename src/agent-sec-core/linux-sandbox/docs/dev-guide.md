# 开发者指南

本文档面向 linux-sandbox 的开发者，包含开发环境搭建、代码规范和贡献指南。

## 目录

- [开发环境](#开发环境)
- [代码质量检查](#代码质量检查)
- [项目结构](#项目结构)
- [测试](#测试)
- [贡献指南](#贡献指南)

## 开发环境

### 依赖要求

- Linux 系统（内核 3.8+）
- Rust 工具链（1.93.0+，由 `rust-toolchain.toml` 指定）
- bubblewrap (`bwrap` 命令)

### 安装依赖

```bash
# 检查 bubblewrap
which bwrap && bwrap --version

# 安装 bubblewrap
sudo yum install bubblewrap   # CentOS/RHEL/Alinux
sudo apt install bubblewrap   # Ubuntu/Debian
```

### 克隆和构建

```bash
git clone <repository-url>
cd linux-sandbox
cargo build --release
```

## 代码质量检查

在提交代码前，请运行以下命令确保代码质量：

```bash
# 代码格式化
cargo fmt

# Clippy 静态检查（包含所有目标和特性）
cargo clippy --all-targets --all-features

# 运行所有测试
cargo test --all-targets --all-features
```

### 推荐的开发工作流

```bash
# 1. 格式化代码
cargo fmt

# 2. 检查并修复 Clippy 警告
cargo clippy --all-targets --all-features --fix

# 3. 运行测试
cargo test --all-targets --all-features

# 4. 构建发布版本
cargo build --release
```

## 测试

### Rust 单元测试 + 集成测试

```bash
cargo test

# 包含所有特性
cargo test --all-features
```

### Python 端到端测试

需要先安装沙箱二进制文件：

```bash
sudo cp target/release/linux-sandbox /usr/local/bin/
python3 tests/integration_test.py
```

### 测试覆盖要求

- 新功能必须包含单元测试
- CLI 命令必须包含集成测试
- 安全相关功能必须包含端到端测试

## 贡献指南

### 代码规范

1. **格式化** - 使用 `cargo fmt` 自动格式化
2. **Clippy** - 保持零警告（`cargo clippy --all-targets --all-features`）
3. **文档** - 公共 API 必须包含文档注释
4. **错误处理** - 使用 `thiserror` 定义错误类型，避免 `unwrap()`

### 提交规范

- 使用清晰的提交信息
- 一个提交只包含一个逻辑变更
- 提交前确保所有测试通过

### 策略变更

修改策略相关代码时：

1. 更新 `src/policy.rs` 中的类型定义
2. 更新 `docs/user-guide.md` 中的文档
3. 添加或更新相关测试用例
4. 确保 JSON Schema 兼容性（使用 `schemars` 派生）

## 架构说明

### 核心组件

1. **CLI** (`cli.rs`) - 解析命令行参数，协调各组件
2. **Policy** (`policy.rs`) - 定义沙箱策略类型
3. **Bwrap** (`bwrap_args.rs`, `bwrap_exec.rs`) - 构建和执行 bubblewrap
4. **Seccomp** (`seccomp.rs`) - 配置系统调用过滤

### 执行流程

```
CLI 解析参数
    ↓
构建 FileSystemPolicy + NetworkPolicy
    ↓
生成 bwrap 参数
    ↓
配置 seccomp 过滤器
    ↓
执行 bubblewrap
```

### 安全模型

- **文件系统隔离** - 通过 bubblewrap 的 bind mount 实现
- **网络隔离** - 通过 seccomp 阻止 socket 系统调用
- **系统调用过滤** - 通过 seccomp BPF 程序实现
- **进程隔离** - 通过 PID/用户命名空间实现

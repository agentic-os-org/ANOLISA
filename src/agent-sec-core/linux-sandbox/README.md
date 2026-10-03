# linux-sandbox

Linux 进程沙箱隔离工具，用于安全地执行不受信任的命令。

## 功能特性

- **文件系统隔离** - 基于 bubblewrap 的文件系统隔离，默认只读，可配置可写目录
- **网络隔离** - 可选的网络访问控制，支持代理路由模式
- **系统调用过滤** - 基于 seccomp 阻止危险系统调用（ptrace、io_uring 等）
- **进程隔离** - PID/用户命名空间隔离，NO_NEW_PRIVS 防提权
- **敏感目录保护** - 自动保护 `.git`、`.copilot-shell`、`.agents` 等目录

## 依赖要求

- Linux 系统（内核 3.8+）
- Rust 工具链（构建时需要）
- 系统已安装 bubblewrap (`bwrap` 命令)

```bash
# 检查 bubblewrap
which bwrap && bwrap --version

# 安装 bubblewrap
sudo yum install bubblewrap   # CentOS/RHEL/Alinux
sudo apt install bubblewrap   # Ubuntu/Debian
```

## 安装

### 从源码构建

```bash
git clone <repository-url>
cd linux-sandbox
cargo build --release
sudo cp target/release/linux-sandbox /usr/local/bin/
```

### 验证安装

```bash
linux-sandbox --help
```

## 快速开始

### 只读模式

```bash
# 读取成功
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"}]}' \
    --network-sandbox-policy '"restricted"' \
    -- cat /etc/hostname
# ✅ 输出主机名

# 写入失败（只读模式下禁止写入）
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"}]}' \
    --network-sandbox-policy '"restricted"' \
    -- bash -c 'echo test > /tmp/test.txt'
# ❌ bash: /tmp/test.txt: Read-only file system
```

### 可写工作区模式

```bash
mkdir -p /tmp/workspace

# 工作目录写入成功
linux-sandbox \
    --sandbox-policy-cwd "/tmp/workspace" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"},{"path":{"type":"special","value":{"kind":"current_working_directory"}},"access":"write"}]}' \
    --network-sandbox-policy '"restricted"' \
    -- bash -c 'echo hello > /tmp/workspace/test.txt'
# ✅ 文件创建成功

# 非工作目录写入失败
linux-sandbox \
    --sandbox-policy-cwd "/tmp/workspace" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"},{"path":{"type":"special","value":{"kind":"current_working_directory"}},"access":"write"}]}' \
    --network-sandbox-policy '"restricted"' \
    -- bash -c 'echo hack > /etc/passwd'
# ❌ bash: /etc/passwd: Read-only file system
```

### 网络隔离 (seccomp)

```bash
# 网络受限时阻止连接（seccomp 阻止 socket/connect 系统调用）
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"}]}' \
    --network-sandbox-policy '"restricted"' \
    -- python3 -c "import socket; s=socket.socket(); s.connect(('8.8.8.8',53))"
# ❌ PermissionError: [Errno 1] Operation not permitted

# 网络启用时允许 socket
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"}]}' \
    --network-sandbox-policy '"enabled"' \
    -- python3 -c "import socket; s=socket.socket(); s.close(); print('socket ok')"
# ✅ socket ok
```

### 危险系统调用拦截 (seccomp)

```bash
# ptrace 被阻止（防止进程调试/注入攻击）
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"}]}' \
    --network-sandbox-policy '"restricted"' \
    -- python3 -c "
import ctypes
libc = ctypes.CDLL(None)
r = libc.ptrace(0, 0, 0, 0)  # PTRACE_TRACEME
print('ptrace allowed' if r == 0 else 'ptrace blocked')
"
# ❌ 进程被 SIGKILL 终止（seccomp 强制阻止）

# io_uring 被阻止（防止异步 I/O 绕过沙箱）
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{"kind":"restricted","entries":[{"path":{"type":"special","value":{"kind":"root"}},"access":"read"}]}' \
    --network-sandbox-policy '"restricted"' \
    -- python3 -c "
import ctypes
libc = ctypes.CDLL(None)
r = libc.syscall(425, 1, 0)  # io_uring_setup
print('io_uring allowed' if r >= 0 else 'io_uring blocked')
"
# ❌ io_uring blocked
```

## 文档

- [用户指南](docs/user-guide.md) - 详细的策略配置和使用示例
- [开发者指南](docs/dev-guide.md) - 开发环境搭建和贡献指南

## 测试

```bash
# Rust 单元测试 + 集成测试
cargo test

# Python 端到端测试（需先安装沙箱）
sudo cp target/release/linux-sandbox /usr/local/bin/
python3 tests/integration_test.py
```

## 项目结构

```
linux-sandbox/
├── src/
│   ├── lib.rs        # 库入口
│   ├── cli.rs        # CLI 主逻辑
│   ├── seccomp.rs    # seccomp 过滤
│   ├── bwrap_args.rs # bubblewrap 参数构建
│   ├── bwrap_exec.rs # bubblewrap 执行
│   ├── proxy.rs      # 代理路由
│   ├── policy.rs     # 策略定义
│   ├── path.rs       # 路径处理
│   └── error.rs      # 错误类型
├── tests/
│   ├── all.rs        # Rust 集成测试入口
│   ├── integration_test.py  # Python 端到端测试
│   └── suite/        # Rust 集成测试
├── docs/
│   ├── user-guide.md # 用户指南
│   └── dev-guide.md  # 开发者指南
└── README.md
```

## 许可证

Apache-2.0

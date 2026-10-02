# 用户指南

本文档详细介绍 linux-sandbox 的策略配置和使用方法。

## 目录

- [策略概述](#策略概述)
- [文件系统策略](#文件系统策略)
- [网络策略](#网络策略)
- [配置示例](#配置示例)
- [高级用法](#高级用法)

## 策略概述

linux-sandbox 通过两种策略控制沙箱行为：

1. **文件系统策略** (`--file-system-sandbox-policy`) - 控制文件系统访问权限
2. **网络策略** (`--network-sandbox-policy`) - 控制网络访问权限

策略使用 JSON 格式通过命令行参数传递。

## 文件系统策略

### 策略类型 (`kind`)

| 类型 | 说明 |
|------|------|
| `restricted` | 受限模式，按 entries 配置访问权限 |
| `unrestricted` | 不受限模式，完整文件系统访问 |
| `external_sandbox` | 外部沙箱模式（由外部工具管理） |

### 访问权限 (`access`)

| 权限 | 说明 |
|------|------|
| `read` | 只读访问 |
| `write` | 读写访问 |
| `none` | 禁止访问 |

### 路径类型

文件系统策略支持两种路径类型：

#### 1. 特殊路径 (`special`)

预定义的路径别名，便于跨平台使用：

| 路径 | 说明 |
|------|------|
| `root` | 文件系统根目录 `/` |
| `minimal` | 最小化环境（仅包含必要系统目录） |
| `current_working_directory` | 当前工作目录 |
| `project_roots` | 项目根目录列表（用于 Agent 场景） |
| `tmpdir` | 临时目录（沙箱内隔离） |
| `slash_tmp` | 系统 `/tmp` 目录 |

#### 2. 绝对路径 (`path`)

指定具体的绝对路径：

```json
{
  "type": "path",
  "path": "/home/user/data"
}
```

### 策略条目 (`entries`)

每个条目定义一个路径及其访问权限：

```json
{
  "path": {
    "type": "special",
    "value": {
      "kind": "root"
    }
  },
  "access": "read"
}
```

## 网络策略

| 值 | 说明 |
|------|------|
| `"restricted"` | 禁止网络访问（seccomp 阻止 socket/connect） |
| `"enabled"` | 允许网络访问 |

## 配置示例

### 示例 1：只读模式

只允许读取根目录，禁止写入和网络：

```bash
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{
        "kind": "restricted",
        "entries": [
            {
                "path": {
                    "type": "special",
                    "value": {"kind": "root"}
                },
                "access": "read"
            }
        ]
    }' \
    --network-sandbox-policy '"restricted"' \
    -- cat /etc/hostname
```

### 示例 2：可写工作区

允许读取根目录，但只允许写入当前工作目录：

```bash
linux-sandbox \
    --sandbox-policy-cwd "/tmp/workspace" \
    --file-system-sandbox-policy '{
        "kind": "restricted",
        "entries": [
            {
                "path": {
                    "type": "special",
                    "value": {"kind": "root"}
                },
                "access": "read"
            },
            {
                "path": {
                    "type": "special",
                    "value": {"kind": "current_working_directory"}
                },
                "access": "write"
            }
        ]
    }' \
    --network-sandbox-policy '"restricted"' \
    -- bash -c 'echo hello > /tmp/workspace/test.txt'
```

### 示例 3：多目录访问

允许访问多个特定目录：

```bash
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{
        "kind": "restricted",
        "entries": [
            {
                "path": {
                    "type": "special",
                    "value": {"kind": "root"}
                },
                "access": "read"
            },
            {
                "path": {
                    "type": "path",
                    "path": "/data"
                },
                "access": "write"
            },
            {
                "path": {
                    "type": "path",
                    "path": "/tmp"
                },
                "access": "write"
            }
        ]
    }' \
    --network-sandbox-policy '"restricted"' \
    -- my-app
```

### 示例 4：允许网络访问

在文件系统隔离的同时允许网络：

```bash
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{
        "kind": "restricted",
        "entries": [
            {
                "path": {
                    "type": "special",
                    "value": {"kind": "root"}
                },
                "access": "read"
            }
        ]
    }' \
    --network-sandbox-policy '"enabled"' \
    -- curl https://api.example.com
```

## 高级用法

### 默认策略

如果未指定策略，文件系统策略默认为：

```json
{
  "kind": "restricted",
  "entries": [
    {
      "path": {
        "type": "special",
        "value": {"kind": "root"}
      },
      "access": "read"
    }
  ]
}
```

### 平台默认保护

在 `restricted` 模式下，沙箱会自动应用平台默认保护：

- 阻止对敏感目录的写入（如 `/etc`, `/usr`, `/bin` 等）
- 保护版本控制目录（`.git`, `.svn` 等）
- 保护 Agent 配置目录（`.copilot-shell`, `.agents` 等）

### 完整磁盘访问

使用 `unrestricted` 类型可禁用文件系统隔离（不推荐）：

```bash
linux-sandbox \
    --sandbox-policy-cwd "$(pwd)" \
    --file-system-sandbox-policy '{"kind": "unrestricted"}' \
    --network-sandbox-policy '"restricted"' \
    -- my-app
```

### 危险系统调用拦截

当网络策略为 `restricted` 时，seccomp 会自动拦截以下危险系统调用：

- `ptrace` - 防止进程调试和注入攻击
- `io_uring_setup` - 防止异步 I/O 绕过沙箱
- `socket`, `connect` 等 - 阻止网络访问

被拦截的进程将收到 `SIGKILL` 信号终止。

# Alinux YUM 仓库体系参考

## Alinux YUM 仓库 URL 命名规范

Alinux YUM 仓库遵循以下 URL 结构：

### Alinux 4 URL 结构

```
{base_url}/{version}/{repo}/{arch}/{type}/
```

- **base_url**:  `https://mirrors.aliyun.com/alinux/`
- **version**: `4`
- **repo**: 仓库名称（os, updates, plus, devel 等）
- **arch**: 架构（x86_64, aarch64, source 等）
- **type**:
  - `os` - 普通二进制包
  - `debug` - 调试包
  - `source` - 源码包

**完整示例**：

```
http://mirrors.aliyun.com/alinux/4/os/x86_64/os/
http://mirrors.aliyun.com/alinux/4/os/x86_64/debug/
http://mirrors.aliyun.com/alinux/4/os/source/
```

## Alinux YUM 仓库名称(repo)规则

**Tier1 基础仓库:**

- `os` - 系统默认初始仓库，通常与正式发布时的 ISO 镜像清单匹配 (开源, 仅包含最新包)
- `updates` - 系统更新仓库，os 仓库的更新包或后引入的新包存放在这里 (开源, 包含历史包)
- `plus` - 系统自研仓库，包含阿里云自研及深度定制开发的组件 (开源, 包含历史包)

**Tier2 开发运行支持仓库:**

- `devel` - 仅在 Alinux 4 上存在，为开发准备的额外构建依赖和额外工具 (开源, 包含历史包) [Alinux4专属]

### YUM 仓库数据分组原则

YUM 仓库数据**按仓库 URL 自然分组**，每个仓库 URL 对应一个独立分组。分组标识从 URL 路径中提取。

### Alinux 4 分组规则

| URL 路径模式       | 分组标识                            | 说明             |
| ------------------ | ----------------------------------- | ---------------- |
| `/{arch}/os/`    | `x86_64`, `aarch64`             | 普通二进制包仓库 |
| `/{arch}/debug/` | `x86_64_debug`, `aarch64_debug` | Debug 包仓库     |
| `/source/`       | `src`                             | 源码包仓库       |

**示例**：

```
http://mirrors.aliyun.com/alinux/4/os/x86_64/os/     → 分组: x86_64
http://mirrors.aliyun.com/alinux/4/os/x86_64/debug/  → 分组: x86_64_debug
http://mirrors.aliyun.com/alinux/4/os/source/        → 分组: src
```

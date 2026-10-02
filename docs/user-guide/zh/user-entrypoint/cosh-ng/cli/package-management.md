# 软件包管理

[English](../../../../en/user-entrypoint/cosh-ng/cli/package-management.md)

`cosh-cli pkg` 提供结构化的软件包操作，会根据平台检测结果选择 dnf、apt、zypper 或 Homebrew，并返回统一的 JSON 响应。

## 命令

| 命令 | 用途 |
|---|---|
| `cosh-cli pkg install <package>` | 安装软件包 |
| `cosh-cli pkg remove <package>` | 删除软件包 |
| `cosh-cli pkg search <query>` | 搜索软件包名称 |
| `cosh-cli pkg list --installed` | 列出已安装的软件包 |

## 安装或删除

执行变更前先预览：

```bash
cosh-cli pkg install nginx --dry-run
cosh-cli pkg remove nginx --dry-run
```

去掉 `--dry-run` 才会真正执行。软件包操作通常需要 root 权限；安装时如果软件包已经存在，命令仍会成功，并在响应中将 `already_installed` 标记为 `true`。

`--dry-run` 会预览该操作，不会安装或删除任何内容、不会下载软件包、也不会写入软件包数据库。它校验的内容取决于后端：在 dnf、apt 与 zypper 上，它确认的是依赖求解在**当前元数据下**能够成功；它不覆盖只在真实事务中才会暂现的失败，例如下载或签名错误、文件冲突、scriptlet 执行失败，以及并发的软件包操作改变了系统状态。这些后端为了求解会读取仓库元数据，本地元数据缺失或已过期时会联网获取。Homebrew 没有模拟模式，其 `--dry-run` 仅确认 formula 是否存在（安装）或是否已安装（删除）—— 它完全不做依赖与冲突求解。

## 搜索

查询会作为一个参数传递，并使用可移植的软件包名称模式。允许的软件包名称字符之外，还可以使用 `*`、`?`、`[` 和 `]`：

```bash
cosh-cli pkg search 'libssl*'
cosh-cli pkg search 'python3-?'
cosh-cli pkg search 'lib[0-9]*'
```

特定后端的正则表达式、Shell 元字符、空查询以及以 `-` 开头的查询都会被拒绝。`cosh-cli` 会在支持的后端之间保持完整软件包名称的匹配语义一致。

## 列表和错误

`list --installed` 返回软件包名称和版本。搜索结果还会报告每个软件包是否已安装；后端无法提供时，搜索结果中的版本以及软件包列表中的架构或仓库字段可能省略。

常见错误包括 `PkgNotFound`、`PkgBackendError`、`UnsupportedDistro` 和 `PermissionDenied`。请根据响应中的 `error.hint` 采取下一步操作。路由详情见[支持的平台](../supported-distros.md)，响应封装见[输出格式](../output-format.md)。

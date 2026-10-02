# 扩展管理

扩展（Extensions）是 Copilot Shell 的能力扩展机制。外部组件（如
agent-sec-core、tokenless）通过声明式配置集成到 Copilot Shell 中，
无需修改核心代码。

## 查看已加载的扩展

```
/extensions
```

此命令列出当前会话中所有已发现和加载的扩展。

## 扩展加载路径

Copilot Shell 按以下顺序搜索扩展：

1. **系统级目录**：`/usr/share/copilot-shell/extensions/`
2. **用户级目录**：`~/.copilot-shell/extensions/`
3. **项目级目录**：`.copilot-shell/extensions/`
4. **CLI 参数指定**：`--extensions` 标志

每个扩展目录下应包含一个 `cosh-extension.json` 声明文件。

## 扩展声明格式

扩展通过 `cosh-extension.json` 文件声明自身能力：

```json
{
  "name": "my-extension",
  "version": "1.0.0",
  "hooks": {
    "PreToolUse": [
      {
        "command": "${EXTENSION_DIR}/hooks/pre-tool.sh",
        "matcher": "Shell"
      }
    ]
  },
  "tools": [
    {
      "name": "my-custom-tool",
      "command": "${EXTENSION_DIR}/tools/my-tool.sh"
    }
  ]
}
```

### 变量替换

扩展配置中支持以下变量：

| 变量 | 含义 |
|------|------|
| `${EXTENSION_DIR}` | 当前扩展所在目录的绝对路径 |

## 已知扩展

ANOLISA 生态中以下组件通过扩展机制集成：

| 扩展 | 功能 |
|------|------|
| `agent-sec-core` | 安全沙箱、命令审查、Hooks 注入 |
| `tokenless` | LLM Token 压缩优化 |

这些扩展随 ANOLISA 安装时自动部署到系统级扩展目录。

## 启用和禁用扩展

### 通过 CLI 参数

```bash
# 只加载指定的扩展
cosh --extensions my-extension,another-extension

# 列出已加载的扩展
cosh --list-extensions
```

### 通过配置文件

```json
{
  "extensions": ["my-extension"]
}
```

## 扩展与 Hooks 的关系

扩展可以注册 hooks 到 Copilot Shell 的事件系统中。扩展注册的 hooks
在执行优先级中排在用户自定义 hooks 之后：

1. User hooks（用户设置）
2. Extension hooks（扩展注入）
3. Remote hooks（远程加载）

## 相关文档

- [Hook 开发指南](../../../../developer-guide/zh/copilot-shell/hooks/index.md) — 了解扩展如何注册 hooks

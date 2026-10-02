# Extensions

[English](../../../../en/user-entrypoint/cosh-ng/core/extensions.md)

Extension 可以打包 Skills、Hooks、MCP server、设置、上下文或 Agent 定义。Extension 可能加入可执行命令和外部工具，只应安装可信来源的 Extension。

## 安装或链接 Extension

在 `cosh` 提示符中运行：

```text
/extensions list
/extensions info <name>
/extensions doctor [name]

/extensions install ./extension
/extensions install https://example.com/extension.git --ref main
/extensions link ./extension
/extensions update <name>
/extensions update --all
/extensions uninstall <name>
```

`install` 会把软件包复制到托管的用户目录；`link` 保持使用本地目录，适合开发。HTTPS Git 源可以使用 `--ref`。运行 `/extensions help` 查看当前版本的语法。

## 审查并启用变更

添加或改变可执行能力的操作可能需要先确认：

```text
/extensions operation <operation-id>
/extensions consent <operation-id>
/extensions cancel <operation-id>
```

确认前检查来源和能力差异。使用 `/extensions enable <name>`、`/extensions disable <name>` 和 `/extensions reload` 控制已安装的软件包。如果系统目录和用户目录同时找到同一个 Extension，请明确选择来源：

```text
/extensions select-source <name> user
/extensions select-source <name> system
```

## Extension 设置

```text
/extensions settings list <name> [--scope user|workspace]
/extensions settings get <name> <key> [--scope user|workspace]
/extensions settings set <name> <key> <value> --scope user
/extensions settings unset <name> <key> --scope workspace
```

敏感设置使用操作系统密钥存储，显示为 `[redacted]`，不能使用 workspace 作用域。Workspace 设置要求项目已受信任。

## 创建脚手架

Extension 作者可以创建并检查一个起始软件包：

```text
/extensions new <path> --template minimal
/extensions doctor <name>
```

模板包括 `minimal`、`skill`、`hook`、`mcp`、`context` 和 `agent`。Extension Hook 和工具使用与配置文件 Hook、MCP server 相同的审批规则。

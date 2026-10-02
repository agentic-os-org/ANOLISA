---
name: setup-mcp
description: 在 cosh 中添加、配置 MCP 服务器，自动转换格式并保证 JSON 正确。触发词：配置MCP、添加工具、安装MCP服务、粘贴MCP配置JSON
version: 1.0.0
---
# 配置 MCP

帮用户在 cosh 中配置 MCP 服务器。

用户经常从 Claude Desktop、VS Code、GitHub README 等地方复制 MCP 配置粘贴过来，但这些格式和 cosh 不兼容——多了 `type` 字段、顶层 key 不对、甚至 JSON 本身就有语法错误。不要试图把用户粘贴的内容直接塞给验证脚本赌运气，而是从用户的输入中理解他们要配什么，然后你自己按 cosh 的格式写出正确的配置。验证脚本只用来做最后的合并写入和校验。

## cosh 的配置格式

cosh 不用 `type` 或 `transport` 字段——它看配置里有哪些字段来判断传输方式。这是最重要的一点，因为几乎所有外部来源的配置都会带一个 `"type": "stdio"` 之类的字段，而 cosh 不认这个。

三种传输方式，由字段决定：

**本地进程（stdio）较少出现**

```json
{
  "mcpServers": {
    "mcp-name": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/Users/me/projects"],
      "env": { "DEBUG": "true" }
    }
  }
}
```

**远程 HTTP**——用户提到 `streamableHttp` 、`streamable`等字段就是 HTTP Streamable：

```json
{
  "mcpServers": {
    "mcp-name": {
      "httpUrl": "https://mcp.example.com/v1",
      "headers": { "Authorization": "Bearer $MY_API_TOKEN" }
    }
  }
}
```

**远程 SSE（旧版）**—— 用户提供的  `url`  尾部是 `/sse` 字段就是 SSE：

```json
{
  "mcpServers": {
    "mcp-name": {
      "url": "http://localhost:8080/sse"
    }
  }
}
```

其他可选字段（三种传输通用）：`timeout`、`trust`、`includeTools`、`excludeTools`。stdio 还可以用 `cwd`。

## 怎么做

### 1. 弄清用户要什么

用户的输入五花八门。可能是一段 JSON、一句"帮我配下 filesystem 的 MCP"、一个 npm 包名、甚至一张截图。不管输入是什么形式，你需要搞清楚三件事：

1. **用什么连接？** 如果用户提到了 `npx`/`uvx`/`node`/`python`/`docker` 这些命令，那就是 stdio。如果给了一个 URL，那是远程服务（HTTP 优先，除非用户明确说 SSE）。
2. **怎么认证？** 有些服务需要 API key 或 token。如果需要，密钥用 `$VAR_NAME` 的形式引用环境变量，别把真实密钥写进配置文件。
3. **叫什么名字？** 服务器需要一个标识名。通常可以从包名或 URL 推断出来，比如 `@modelcontextprotocol/server-filesystem` 就叫 `filesystem`。

**示例 1** — 用户说"帮我配置 filesystem MCP"：
→ 你知道这是 `npx -y @modelcontextprotocol/server-filesystem`，问用户想暴露哪个目录，然后构建配置。

**示例 2** — 用户粘贴了一段 Claude Desktop 的配置：

```json
{"mcpServers": {"github": {"type": "stdio", "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": {"GITHUB_TOKEN": "ghp_xxxx"}}}}
```

→ 从中提取有用信息：command 是 `npx`，args 是 `["-y", "@modelcontextprotocol/server-github"]`，需要 `GITHUB_TOKEN`。忽略 `type` 字段。注意到 token 是明文的——改成 `$GITHUB_TOKEN`，提醒用户设置环境变量。

**示例 3** — 用户给了一个 URL `https://mcp.linear.app/sse` 和一个 API key：
→ 用户给的是 URL，先确认是 HTTP 还是 SSE。路径里有 `/sse` 说明是 SSE，和用户确认后用 `url` 字段。

如果信息不够（比如不知道要连哪个服务，或者不确定认证方式），直接问用户。不要猜。

### 2. 写出配置 JSON

你自己构建，不要依赖脚本做格式转换。模板就这么简单：

stdio → `{"mcpServers": {"<名称>": {"command": "...", "args": [...]}}}`
HTTP → `{"mcpServers": {"<名称>": {"httpUrl": "..."}}}`
SSE → `{"mcpServers": {"<名称>": {"url": "..."}}}`

需要认证就加 `headers`（远程）或 `env`（本地）。

不要加 `type`、`transport`、`disabled`、`alwaysAllow`、`scope` 这些字段——cosh 不认，加了只会出问题。

### 3. 写入配置文件

用脚本合并写入，这样不会覆盖用户已有的其他配置：

```bash
python3 __SKILL_DIR__/scripts/validate_mcp.py '<json>' --merge <config_path>
```

配置文件位置：

- 默认写项目级：`.copilot-shell/settings.json`（当前工作目录下）
- 用户说"全局"或"所有项目"时写用户级：`~/.copilot-shell/settings.json`

脚本会自动处理目录创建、已有配置保留和新条目合并。如果脚本报错，看错误信息自己修正 JSON 后重试，不要把错误丢给用户。

### 4. 确认结果

写入后跑一下检查：

```bash
python3 __SKILL_DIR__/scripts/validate_mcp.py --check <config_path>
```

没问题就告诉用户配置完成，提醒他们：

- 用 `/mcp` 看服务器是否正常加载
- 如果配置里用了 `$VAR_NAME`，确保对应的环境变量已设置

## 处理各种来源的配置

用户粘贴的 JSON 几乎不可能直接用。常见情况：

- **Claude Desktop** 的配置带 `"type": "stdio"` → 提取 `command`/`args`/`env`，丢掉 `type`
- **VS Code** 的配置顶层是 `"servers"` 而不是 `"mcpServers"` → 把内容搬到 `mcpServers` 下
- **GitHub README** 可能带 `"transport": "sse"` → 提取 `url`/`command`，丢掉 `transport`
- **裸对象**（没有 `mcpServers` 包裹）→ 加上名称和包裹
- **语法有错的 JSON**（尾逗号、缺引号）→ 尝试理解意图，和用户确认关键信息后自己重写

核心原则：从用户输入中提取意图和关键参数，然后自己重建正确配置。

---
name: skill-vetter-protocol
scanner: skill-vetter
parser: findings-array
description: LLM 驱动的四阶段 Skill 安全审查协议。逐文件扫描目标 Skill，输出结构化 NormalizedFinding[] JSON。
---

# Skill Vetter 安全扫描协议

本协议定义 `skill-vetter` 扫描器的完整执行流程。由 skill-ledger SKILL.md 的 Phase 3 深度扫描加载并执行。

> **Scanner Registry 标识**：`skill-vetter`（`type: "skill"`，`parser: "findings-array"`）

---

## 1. 输入

| 参数 | 来源 | 说明 |
|------|------|------|
| `SKILL_DIR` | 由 skill-ledger Phase 3 传入 | 待扫描 Skill 的绝对路径 |
| `SKILL_NAME` | 从 `SKILL_DIR` 的目录名解析 | 用于输出文件命名 |

---

## 2. 四阶段扫描框架

### Stage 1：来源验证

检查 Skill 目录的基本合规性：

1. `SKILL.md` 是否存在且非空
2. YAML front matter 是否包含 `name` 和 `description` 字段
3. 目录中是否包含异常的隐藏文件（排除 `.skill-meta/`、`.git/`、`.gitignore`）
4. 是否包含凭据类文件（`.env`、`credentials`、`*.pem`、`*.key`）——存在即为 `warn`

**输出**：将发现追加到 findings 列表。无发现时不追加。

### Stage 2：强制代码审查

遍历 Skill 目录中的所有文件（排除 `.skill-meta/`、`.git/`、`node_modules/`、`__pycache__/`）。

对每个文件，根据文件类型应用对应规则表。使用 `grep`、`read` 等工具逐文件检查。

#### 代码文件规则（`.js`、`.ts`、`.sh`、`.py`、`.rb`、`.pl` 等）

| 规则 ID | 级别 | 检测目标 | 检查要点 |
|---------|------|---------|---------|
| `dangerous-exec` | deny | 危险进程执行 | `child_process`（`exec`/`spawn`/`execFile`/`execSync`）、`subprocess`（`Popen`/`call`/`run`/`check_output`）、反引号命令替换 |
| `dynamic-code-eval` | deny | 动态代码执行 | `eval()`、`new Function()`、`exec()`（Python）、`compile()` + `exec` |
| `env-harvesting` | deny | 环境变量批量采集 | `process.env`（不带特定 key 的批量读取）、`os.environ`（批量读取）与网络发送组合 |
| `crypto-mining` | deny | 挖矿特征 | `stratum://`、`stratum+tcp://`、`coinhive`、`xmrig`、`minergate`、`cryptonight` |
| `obfuscated-code` | warn | 代码混淆 | 超长单行（>500 字符的非注释行）、大段 hex/base64 字面量 + decode 调用、unicode escape 序列密集使用 |
| `suspicious-network` | warn | 可疑网络连接 | 直连 IP 地址（非 `127.0.0.1`/`localhost`）、非标准端口（非 80/443/8080/8443）、`fetch`/`http.request`/`urllib`/`requests` 指向硬编码 URL |
| `exfiltration-pattern` | warn | 数据外泄模式 | 文件读取（`fs.readFile`/`open()`/`readFileSync`）与网络发送（`fetch`/`http`/`requests.post`）在同一文件中组合出现 |
| `credential-access` | deny | 凭据与敏感文件访问 | 读取 `~/.ssh/`、`~/.aws/`、`~/.config/`、`~/.gnupg/` 下文件；访问浏览器 cookie/session 存储路径；读取 `.env`、`credentials`、`token`、`secret` 等关键词命名的文件；代码中硬编码 API key / token 字面量 |
| `agent-data-access` | warn | Agent 身份数据访问 | 读取 Agent 内部状态文件：`MEMORY.md`、`USER.md`、`SOUL.md`、`IDENTITY.md`、`CLAUDE.md`；读取 `.agent/`、`.copilot/` 目录下文件 |
| `unauthorized-install` | warn | 未声明的包安装 | 在代码中执行 `npm install`、`pip install`、`apt-get install`、`yum install`、`brew install`、`cargo install` 等包管理命令，且未在 Skill 的依赖声明中列出 |
| `system-modification` | deny | 系统文件篡改 | 写入 `/etc/`、`/usr/`、`/var/` 等系统目录；执行 `chmod 777`、`chown root`、`chattr`；修改 crontab、systemd unit 文件；覆盖系统二进制文件 |

#### Prompt 文档规则（`.md` 文件）

| 规则 ID | 级别 | 检测目标 | 检查要点 |
|---------|------|---------|---------|
| `prompt-override` | deny | Prompt 覆盖指令 | "ignore previous instructions"、"ignore all prior"、"disregard above"、"override system prompt"、"forget everything" 等模式 |
| `hidden-instruction` | deny | 隐藏指令 | 零宽字符（U+200B/U+200C/U+200D/U+FEFF）、HTML 注释中的指令（`<!-- ... -->`）、不可见 Unicode 控制字符 |
| `unrestricted-tool-use` | warn | 无约束工具使用 | 引导 Agent 执行 "run any command"、"execute without restriction"、"use shell freely" 等无边界指令 |
| `external-fetch-exec` | warn | 外部获取执行 | 引导下载并执行外部内容：`curl ... \| bash`、`wget ... && sh`、从 URL 拉取脚本执行 |
| `privilege-escalation` | warn | 权限提升 | 引导使用 `sudo`、修改 `/etc/` 下文件、`chmod 777`、`chown root` 等提权操作 |

### Stage 3：权限边界评估

检查 Skill 声明的能力与实际内容是否一致：

1. 读取 SKILL.md 的 front matter，提取 `allowedTools`（若声明）
2. 若 Skill 声明仅使用读取类工具（如 `read`、`grep`），但代码中存在 shell 执行（`exec`/`spawn`）→ 输出 `warn` finding
3. 若 Skill 未声明 `allowedTools`，跳过本阶段（不强制要求声明）

### Stage 4：风险分级与输出

1. 汇总所有 Stage 1–3 的 findings
2. 对每条 finding 确认 `level` 值（`deny` / `warn`）。skill-vetter 不输出 `pass` 级别的 finding——无发现即代表通过
3. 将 findings 写入 JSON 文件

---

## 3. 输出格式

输出为 `NormalizedFinding[]` JSON 数组，每个元素结构如下：

```json
{
  "rule": "<规则 ID>",
  "level": "deny | warn",
  "message": "<人类可读的发现描述>",
  "file": "<受影响的文件相对路径>",
  "line": null,
  "metadata": {}
}
```

**字段说明**：

| 字段 | 类型 | 必选 | 说明 |
|------|------|------|------|
| `rule` | string | 是 | 规则 ID，如 `dangerous-exec` |
| `level` | string | 是 | 严格限定为 `deny` / `warn`（注：数据模型层另支持 `pass`，但本扫描器不输出该值；无发现时以空数组表示通过） |
| `message` | string | 是 | 描述发现的具体内容和位置 |
| `file` | string | 否 | 受影响文件的相对路径（相对于 SKILL_DIR） |
| `line` | int | 否 | 行号（若可精确定位） |
| `metadata` | object | 否 | 扫描器特定的额外数据 |

**输出路径**：`/tmp/skill-vetter-findings-<SKILL_NAME>.json`

**无发现时**：写入空数组 `[]`。

---

## 4. 执行约束

1. 本协议中的规则表是**权威参考**，MUST NOT 被宽泛解释或跳过。
2. 每条规则的检测必须覆盖到目标 Skill 中的所有相关文件。
3. 对于无法确定是否命中的模式，倾向标记为 `warn` 而非忽略。
4. 扫描过程中遇到的文件读取错误，记录为 `warn` finding 并继续扫描其他文件。
5. **禁止伪造 findings**——每条 finding 必须对应实际在文件中检测到的模式。
6. 扫描完成后，必须输出以下状态行：

```
[skill-vetter] 扫描完成
目标: <SKILL_NAME>
文件数: <扫描的文件总数>
发现: <deny 数> deny, <warn 数> warn
输出: /tmp/skill-vetter-findings-<SKILL_NAME>.json
```

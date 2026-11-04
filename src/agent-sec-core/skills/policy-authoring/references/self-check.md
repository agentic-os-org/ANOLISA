# 自检清单

本 Skill 的校验路径：Agent 自己生成 Cedar，自己过这份清单，不调用任何命令。

它只覆盖官方解析器和 schema **确定会拒**的写法，属于必要条件检查，不是充分条件：全部通过也
不证明语义正确，语义靠回译确认。

逐条过，命中就改到不命中为止。每条后面的"拒绝方"指出哪道检查会拒它，方便对得上报错。

## 1. 结构

| 检查项 | 拒绝方 |
|---|---|
| 每条规则以 `;` 结尾，`when` / `unless` 后只有一对大括号 | parser |
| 效果只有 `permit` / `forbid` | parser |
| 作用域只有 `principal, action, resource`，没有 `context` | parser |
| 至少一条静态规则（空文本不算交付物） | admission |
| 没有注解：`@id`、`@advice`、`@description` 全部不写 | admission |
| 没有模板、没有 `?principal`、没有 `link` | 子集检查 |

## 2. 主体与资源

| 检查项 | 拒绝方 |
|---|---|
| `principal` 裸写，不写 `principal == ...`、`principal is ...`、`principal in ...` | admission |
| 条件里不出现 `principal` 这个词 | 子集检查 |
| 不出现 `SecCore::File::"..."`、`SecCore::Subject::"..."` 这类实体 UID | 子集检查 |
| `file.read` / `file.write` / `file.exec` 的规则用 `resource is SecCore::File` | schema 校验 |
| `network.use` 的规则用 `resource == SecCore::Network::"all"` | schema 校验 |

## 3. 动作与字段

| 检查项 | 拒绝方 |
|---|---|
| `action` 只取 `file.read`、`file.write`、`file.exec`、`network.use` | schema 校验 |
| `file.read` 只读 `context.input.program` | schema 校验 |
| `file.write` 只读 `context.input.program`、`operation`、`mode`、`ownerUid`、`ownerGid` | schema 校验 |
| `file.exec` 只读 `context.input.cwd`、`arg_count`、`args` | schema 校验 |
| `network.use` 的条件里不读任何 `context.input` 字段 | schema 校验 |
| 读 `mode` / `ownerUid` / `ownerGid` 前先写 `context.input has <字段>` | schema 校验 |
| `operation` 只与 `SecCore::WriteOperation::"write"`、`"create"`、`"delete"`、`"rename"`、`"setattr"` 比较 | schema 校验 |
| 不出现 schema 未声明的字段名 | schema 校验 |

## 4. 路径

| 检查项 | 拒绝方 |
|---|---|
| `resource.path`、`context.input.program`、`context.input.cwd` 只与字面量比较，不与另一个属性比较 | 路径检查 |
| 字面量是规范绝对路径：`/` 开头，无 `..`、无空段、无重复 `/`、无尾部 `/`、无反斜杠、无控制字符 | 路径检查 |
| `like` 只有"字面量前缀 + 末尾 `/*`"，前缀以 `/` 结束 | 路径检查 |
| 子树写成两条：`== "/dir"` 或 `like "/dir/*"` | 语义 |
| 文件名里真实的 `*` 写成 `\*`，不在 `like` 里当中段通配 | 路径检查 |
| 路径不是相对路径、不是变量拼接、不是运行时才能确定的值 | 路径检查 |

## 5. 语义

| 检查项 | 拒绝方 |
|---|---|
| "只允许 X"同时写了同动作的补集 `forbid`；补集保留同一 `operation` 条件 | 语义 |
| 保留写权限、只封删除时，`forbid` 带 `operation == "delete"` 条件 | 语义 |
| 纯禁止不额外补 `permit` | 语义 |
| 没有把"禁止删除"写成禁止全部写操作，没有把 host 级限制写成全网开放 | 语义 |
| 每条规则能在表格里说清"允许/拒绝谁在什么条件下做什么" | 自检 |

## 6. 交付标注

交付必须包含：

1. 一句话意图复述；
2. 假设（路径、`operation` 集合、子树还是单文件）；
3. **校验状态：已按清单自检**，列出过了哪几段（结构、主体与资源、动作与字段、路径、语义）；
4. Cedar 原文；
5. 每条规则的中文说明与仍然被拒绝的操作；
6. 无法表达的要求（unresolved）；
7. Agent 自身运行所需的权限（模型端点、凭据、工具）是否被这份 policy 或现有
   Binding 覆盖，没覆盖就说清缺哪一条。

不能把自检写成"已校验"、"已验证"或"可以生效"；自检只证明写法不被拒，不证明语义正确。

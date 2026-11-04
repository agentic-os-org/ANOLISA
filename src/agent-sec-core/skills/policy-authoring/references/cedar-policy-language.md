# SecCore Cedar 语言参考

本文件是本 Skill 写作范围的速查，不是完整 Cedar 手册。类型定义以
[`seccore.cedarschema`](seccore.cedarschema) 为准，逐项检查见
[`self-check.md`](self-check.md)。写作约束不等于策略入口的拒绝条件。

## 规则结构

```cedar
<effect> ( <scope> )
<when | unless> { <表达式> };
```

- `<effect>`：`permit` 或 `forbid`，没有其他效果；审批、二次确认只能进 unresolved。
- `<scope>`：只写 `principal, action, resource` 三项；`action` 与文件资源用 `==`、`is`
  收窄，但 `context` 绝不能出现在作用域里。
- 条件只有 `when`（满足时生效）和 `unless`（不满足时生效）；`unless { A }` 等价于 `when { !A }`。
- 每条规则以 `;` 结尾；`action` 必须指名单个动作，一条规则只写一个动作。

## 作用域与字面量

| 位置 | 写法 |
|---|---|
| 动作 | `action == SecCore::Action::"file.read"`，只能写单个动作 |
| 文件资源 | `resource is SecCore::File`；路径在条件里读 `resource.path` |
| 网络资源 | `resource == SecCore::Network::"all"` |
| 写操作 | `context.input.operation == SecCore::WriteOperation::"delete"` |

`WriteOperation` 的取值是实体枚举：`"write"`、`"create"`、`"delete"`、`"rename"`、`"setattr"`，
比较时写 `SecCore::WriteOperation::"..."`，不要写成字符串字面量、也不要用 `is`。

## 表达式语言

条件里可用的取值来源只有三类：`resource.path`、`context.input.<字段>`，以及字面量。

| 能力 | 写法 |
|---|---|
| 逻辑 | `&&`、`\|\|`、`!` |
| 整数比较 | `==`、`!=`、`<`、`<=`、`>`、`>=`（`mode`、`ownerUid`、`ownerGid`、`arg_count`） |
| 字符串相等 | `==`、`!=` |
| 模式匹配 | `like "前缀/*"`，只允许在末尾出现 `*` |
| 字段存在 | `context.input has mode` |
| 集合相等 | `context.input.args == [{index: 0, value: "status"}, {index: 1, value: "--short"}]` |

本 Skill 的限制：

- 没有算术运算，没有 `let ... in`，没有 `count`、`sum`、`min`、`max`。
- 集合无序，参数位置必须靠 `index` 表达；`index` 从 `0` 开始，对应 `argv[1]`。
- 可选字段（`mode`、`ownerUid`、`ownerGid`）不 `has` 就读取会被拒；`has` 之后再用 `==` 比较。
- 空策略集一律被拒：至少要有一条静态策略。

## 路径与字符串

路径只在 `resource.path`、`context.input.program`、`context.input.cwd` 三处出现。以下是本 Skill 的
写作约束，不代表策略入口一定拒绝其他路径写法：

- 是规范绝对路径：`/` 开头，无 `..`、无空段、无重复 `/`、无尾部 `/`、无反斜杠、无控制字符；
- 与**字面量**比较，不和另一个属性比较（不写 `resource.path == context.input.program`）；
- 子树写成两个子条件：`resource.path == "/dir"` 或 `resource.path like "/dir/*"`，
  因为 `like "/dir/*"` 不匹配 `/dir` 本身，`== "/dir"` 也不匹配 `/dir/child`；
- 根目录子树写成 `resource.path like "/*"`（再配 `== "/"`）；
- 等式中的真实星号原样写：`resource.path == "/a*b"`；写成 `"/a\*b"` 会报 invalid escape；
- 仅在 `like` 模式中把真实星号转义为 `\*`：`resource.path like "/a\*b/*"`，末尾 `*` 才是通配；
- 反斜杠和引号按 Cedar 字符串转义写 `\\`、`\"`。

本 Skill 的 `like` 形态只有"字面量前缀 + 末尾 `/*`"，以下违反写作约束，不代表策略入口会拒绝：

```text
resource.path like "/a*"      // 前缀不以 / 结尾
resource.path like "/a/*/b"   // 通配符不在末尾
resource.path == "/a/../b"    // 非规范路径
resource.path == "/a//b"      // 重复分隔符
resource.path == "/a\\b"    // 反斜杠
```

## 能解析但会被拒

这些写法语法合法，但会被子集检查或 schema 校验拒绝：

| 写法 | 拒绝原因 |
|---|---|
| `permit(principal == SecCore::Subject::"A", ...)` | 主体由 Scope/Binding 选择，策略不绑定身份 |
| `when { principal == ... }` | 条件中不允许引用 `principal` |
| `@id("x")` / `@advice("x")` | 任何注解都不写 |
| 模板与 `?principal` | 不支持模板链接 |
| 空策略集 | 至少要有一条静态策略 |
| `context.input.nonexistent` | 未在 schema 声明的字段 |
| `forbid(principal, action, resource) when { resource.path == "..." }` | 不指名动作，条件字段无法对全部动作的资源类型成立，schema 校验拒 |

## 决策语义

- 默认拒绝：没有匹配的 permit 即拒绝。
- forbid 优先：任何匹配的 forbid 都压过 permit。
- permit 只增加权限，不能收窄另一条 permit；排他要求必须写成同动作的补集 forbid。
- 补集写 `unless { 完整许可条件 }`，连同路径、operation 或执行条件一起否定。
- 同一份 policy set 内多条规则按"默认拒绝 + forbid 覆盖"组合；纯 forbid 不授予许可，排他要求仍需补集。
- `file.read` 只是内容读取；`file.exec` 只是程序启动；`network.use` 只有全网络开关。

## 下游提交格式（仅供参考）

本 Skill 只交付 Cedar 源码，不负责包装或提交。下游提交时将源码放入 JSON 策略模板的
`content.text`，声明语言与固定 schema 引用，不直接提交裸 `.cedar` 文件。例如：

```json
{
  "language": "cedar",
  "content": {
    "schema": { "schemaId": "seccore.system", "revision": 1 },
    "text": "forbid(principal, action == SecCore::Action::\"network.use\", resource == SecCore::Network::\"all\");"
  }
}
```

区分三个阶段：
1. Cedar 源码：实际解析语法、检查支持的写法与字段类型才算源码校验；离线自检不算。
2. 策略模板：JSON 包装与源码校验通过后可保存，不表示已经批准、绑定或下发。
3. 执行目标：是否支持 Cedar 执行须另行确认，模板保存成功不代表策略已经生效或实际拦截。

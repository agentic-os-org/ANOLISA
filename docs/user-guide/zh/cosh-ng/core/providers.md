# 模型 Provider 与认证

[English](../../../en/cosh-ng/core/providers.md)

交互式终端可使用 `/auth` 配置模型。托管或 headless 环境请在系统或用户配置文件中定义 Provider；项目配置不能添加凭据或 Provider 定义。

## ECS 免配置接入

在支持此能力的 Cosh Shell/Core 中，ECS 用户尚未选择 Provider、设置端点或提供凭据时，自然语言触发的模型请求会尝试默认免配置接入，无需先执行 `/auth`。已经配置的模型继续使用原配置。默认使用 `qwen3.7-plus`，显式指定的模型保持不变。该路径需要服务端支持 ECS 实例身份交换。

免配置仅指无需手动填写模型凭据，不代表永久免费。当前处于体验阶段，后续将提供每月免费额度；用尽后停止调用，用户可自行购买加油包或开通按量付费。可用额度和付费方式以控制台实际提供的内容为准，Cosh 不会自动开通付费。

首次使用若需要开通，终端会直接显示二维码和[操作系统控制台链接](https://console.aliyun.com/alinux/welcome?source=cosh)。使用当前 ECS 所属账号开通后，Cosh 自动检查并继续当前问题，无需回到终端确认。临时凭据只保存在内存，接近过期时在下次请求前自动换取；不会写入模型配置或要求保存 AK/SK。

- `Esc` 取消当前等待；取消的问题不会自动重发。
- `n` 打开“不再提示”设置，可选本次 Shell 会话或以后。关闭后同时停止自动接入操作系统控制台模型服务。
- 本次会话的选择随 Shell 退出而结束；长期选择保存在 `~/.copilot-shell/cosh/preferences.toml`，不随 `/new` 或 Core 重启重置。
- `/auth` 可配置其他模型，也可显式重新启用免配置接入；重新启用不会重发已取消的问题。

只有服务端明确返回额度不足时才显示额度不足提示，不自动重试；可前往操作系统控制台查看，或通过 `/auth` 切换模型服务。网络、权限和身份交换失败分别报告，不当作未开通。

## 在交互式终端选择 Provider

```text
/auth
```

认证菜单提供 Aliyun AK/SK、DashScope、OpenAI-compatible、Coding Plan 和 Token Plan。内置 plan endpoint 默认使用中国站。需要国际站时，在启动 `cosh` 前设置：

```bash
COSH_SERVICE_SITE=international cosh
```

可用值还包括 `china`/`cn` 和 `international`/`intl`/`global`。该设置只改变内置 plan endpoint，不会改写已保存的自定义 URL。

## 配置 Provider

将下面示例写入 `~/.copilot-shell/config.toml`（管理员也可以写入 `/etc/copilot-shell/config.toml`），并在环境变量中提供 key：

```toml
[ai]
active_provider = "dashscope"
active_model = "qwen3.7-plus"

[ai.providers.dashscope]
type = "dashscope"
base_url = "https://dashscope.aliyuncs.com/compatible-mode/v1"
api_key = "${DASHSCOPE_API_KEY}"
model = "qwen3.7-plus"
```

其他常用 profile 使用相同结构：

```toml
[ai.providers.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
api_key = "${OPENAI_API_KEY}"
model = "gpt-4o"

[ai.providers.deepseek]
type = "deepseek"
base_url = "https://api.deepseek.com/v1"
api_key = "${DEEPSEEK_API_KEY}"
model = "deepseek-chat"

[ai.providers.aliyun]
type = "aliyun"
access_key_id = "${ALIBABA_CLOUD_ACCESS_KEY_ID}"
access_key_secret = "${ALIBABA_CLOUD_ACCESS_KEY_SECRET}"
security_token = "${ALIBABA_CLOUD_SECURITY_TOKEN}"
model = "qwen3.7-plus"
```

使用 ECS RAM role 时，设置 `type = "aliyun"` 和 `auth_source = "ecs_ram_role"`，无需保存静态 AK/SK。

`aliyun` provider 会自动选择 SysOM API 主机：在能连通 VPC 内网代理的 ECS 上走内网，因此不通公网的实例也能使用；否则走公网主机。只有需要固定到某个主机时才设置 `sysom_endpoint`：

```toml
[ai.providers.aliyun]
type = "aliyun"
auth_source = "ecs_ram_role"
sysom_endpoint = "https://sysom.cn-shanghai.aliyuncs.com"
model = "qwen3.7-plus"
```

`sysom_endpoint` 与 `base_url` 是刻意分开的两个设置：`base_url` 属于 OpenAI 兼容语义，`aliyun` provider 不读取它。

优先级依次为 `COSH_SYSOM_ENDPOINT`、`sysom_endpoint`、VPC 探测、公网端点。覆盖值支持裸主机名（可带端口）或 HTTP(S) URL；URL 路径会被丢弃，非法覆盖值按未设置处理。仅自动选中的 VPC 路由直连，与探针保持一致；显式覆盖（包括显式配置的 VPC 主机）和公网回退仍遵守系统代理。需要让显式端点绕过代理时，请设置 `NO_PROXY`。

推理请求的建连超时为 3 秒，读取空闲超时为 120 秒；成功读取会重置空闲计时，持续正常输出的流没有总时长限制。

| `type` | 用途 |
|---|---|
| `dashscope` | 支持 Qwen reasoning 的 DashScope OpenAI-compatible endpoint |
| `openai` | 使用 OpenAI 请求约定，包括 `max_completion_tokens` |
| `deepseek` | 支持 reasoning-content 的 OpenAI-compatible endpoint |
| `aliyun` | 使用 AK/SK 或 ECS RAM role 的 Alibaba Cloud SysOM |
| 其他值 | 通用 OpenAI-compatible 行为 |

只有需要显式 cache marker 时才为 DashScope 设置 `explicit_cache = true`；默认行为请省略或设为 `false`。

## 优先级与凭据缺失

活动 Provider 和模型按以下顺序解析：配置层、`COSH_AI_PROVIDER`/`COSH_MODEL`/`COSH_OUTPUT_LANGUAGE`，再到 Provider 字段及其环境变量回退。`--model <name>` 只覆盖模型；切换 Provider 请使用 `COSH_AI_PROVIDER` 或 `active_provider`。

| 变量 | 回退内容 |
|---|---|
| `OPENAI_BASE_URL` | OpenAI-compatible base URL |
| `DASHSCOPE_API_KEY`，然后 `OPENAI_API_KEY` | API-key Provider |
| `ALIBABA_CLOUD_ACCESS_KEY_ID`、`ALIBABA_CLOUD_ACCESS_KEY_SECRET`、`ALIBABA_CLOUD_SECURITY_TOKEN` | Aliyun 凭据 |

缺少 key 时，交互式 Core 会请求认证。独立 headless client 必须回复该 control request，或在启动前配置凭据。配置层规则见[配置](../configuration.md)，control protocol 见 [Headless 模式](headless-mode.md)。

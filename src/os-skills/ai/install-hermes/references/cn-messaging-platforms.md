# Hermes Agent 中国消息平台接入指南

本文档覆盖钉钉、飞书/Lark、微信、企业微信四大平台的完整接入流程。

---

## 钉钉（DingTalk）

### 行为模型

| 场景 | 行为 |
|------|------|
| 私聊（1:1） | 自动回复所有消息，无需 @mention |
| 群聊 | 仅在被 @mention 时回复 |
| 多人群聊 | 默认按用户隔离会话历史 |

会话隔离控制（`config.yaml`）：

```yaml
group_sessions_per_user: true   # false = 群内共享一个对话
```

### 前置依赖

默认**最小安装**内已包含核心依赖。若使用 `--full` 安装或需要单独补装：

```bash
uv pip install --no-config -e ".[dingtalk]" -i https://mirrors.aliyun.com/pypi/simple/
# 或单独安装依赖：
uv pip install --no-config dingtalk-stream httpx alibabacloud-dingtalk -i https://mirrors.aliyun.com/pypi/simple/
```

### Step 1: 创建钉钉应用

1. 登录 [钉钉开发者后台](https://open-dev.dingtalk.com/)
2. **应用开发 → 自建应用 → H5 微应用**（或机器人）
3. 获取 **Client ID (AppKey)** 和 **Client Secret (AppSecret)**

> Client Secret 仅显示一次，丢失需重新生成。

### Step 2: 启用机器人能力

1. 应用设置 → **添加能力 → 机器人**
2. 消息接收模式选择 **Stream Mode**（推荐，无需公网 URL）

### Step 3: 获取钉钉 User ID

> ❗员工工号 ≠ 钉钉 User ID，两者格式完全不同。

**推荐方法 — 通过网关日志获取**：

1. **先不设置** `DINGTALK_ALLOWED_USERS`，启动网关：`hermes gateway run &`
2. 从钉钉给 Bot 发一条消息
3. 查看日志 `~/.hermes/logs/agent.log`，找到这一行：
   ```
   WARNING gateway.run: Unauthorized user: $:LWCP_v1:$xxxx... (姓名) on dingtalk
   ```
4. 把 `$:LWCP_v1:$xxxx...` 这个完整字符串填入配置

**备选方法**：找组织管理员在钉钉通讯录查询成员的系统 ID。

### Step 4: 配置

**方式 A: 交互式配置（推荐）**

```bash
hermes gateway setup    # 选择 DingTalk
```

支持两种授权方式：
- **扫码授权**（推荐）：终端显示二维码，钉钉扫码自动写入凭证
- **手动粘贴**：直接输入 Client ID / Secret / 用户 ID

**方式 B: 手动配置**

`~/.hermes/.env`：

```bash
DINGTALK_CLIENT_ID=your-app-key
DINGTALK_CLIENT_SECRET=your-app-secret
DINGTALK_ALLOWED_USERS=user-id-1,user-id-2
```

### 启动

```bash
hermes gateway
```

### AI 卡片（可选）

在 `config.yaml` 中配置卡片模板 ID 可启用富文本流式卡片：

```yaml
platforms:
  dingtalk:
    enabled: true
    extra:
      card_template_id: "your-card-template-id"
```

### 显示设置

```yaml
display:
  platforms:
    dingtalk:
      show_reasoning: false
      streaming: true
      tool_progress: all        # all | new | off
      interim_assistant_messages: true
```

### 故障排查

| 问题 | 修复 |
|------|------|
| Bot 不回复 | 检查机器人能力已启用 + Stream Mode + ALLOWED_USERS 包含你的 ID |
| `dingtalk-stream not installed` | `pip install dingtalk-stream httpx` |
| 凭证缺失错误 | 检查 `.env` 中 CLIENT_ID 和 CLIENT_SECRET |
| Stream 断连重连循环 | 检查网络和凭证有效性，自动指数退避重连 |
| `No session_webhook` | 发新消息即可（钉钉 webhook 有时效限制） |

---

## 飞书 / Lark（Feishu）

### 行为模型

| 场景 | 行为 |
|------|------|
| 私聊 | 自动回复所有消息 |
| 群聊 | 仅在被 @mention 时回复 |
| 多人群聊 | 默认按用户隔离会话 |

### 连接模式

| 模式 | 说明 | 适用场景 |
|------|------|---------|
| `websocket`（推荐） | SDK 维护的长连接，自动重连 | 无公网环境 |
| `webhook` | HTTP 端点接收推送 | 已有公网 HTTP 服务 |

### Step 1: 创建飞书应用

**扫码创建（推荐）**：

```bash
hermes gateway setup    # 选择 Feishu / Lark，扫码自动创建
```

**手动创建**：
1. 飞书：https://open.feishu.cn/ ｜ Lark：https://open.larksuite.com/
2. 创建应用 → 获取 **App ID** 和 **App Secret**
3. 启用 Bot 能力

### Step 2: 配置

**交互式**：`hermes gateway setup` → 选 Feishu / Lark

**手动配置** `~/.hermes/.env`：

```bash
FEISHU_APP_ID=cli_xxx
FEISHU_APP_SECRET=secret_xxx
FEISHU_DOMAIN=feishu              # feishu | lark
FEISHU_CONNECTION_MODE=websocket  # websocket | webhook
FEISHU_ALLOWED_USERS=ou_xxx,ou_yyy
FEISHU_HOME_CHANNEL=oc_xxx        # 可选：cron/通知输出频道
```

Webhook 模式额外配置：

```bash
FEISHU_WEBHOOK_HOST=127.0.0.1
FEISHU_WEBHOOK_PORT=8765
FEISHU_WEBHOOK_PATH=/feishu/webhook
FEISHU_ENCRYPT_KEY=your-encrypt-key        # Webhook 签名验证
FEISHU_VERIFICATION_TOKEN=your-token       # 额外认证层
```

### 启动

```bash
hermes gateway
```

### 群消息策略

```bash
FEISHU_GROUP_POLICY=allowlist   # open | allowlist | disabled
```

### 交互卡片

支持按钮点击回调，用于命令审批（Allow/Deny）。需在开发者后台配置：
1. 订阅 `card.action.trigger` 事件
2. 启用 Bot 的交互卡片能力
3. Webhook 模式需配置卡片请求 URL

### 文档评论智能回复

在飞书文档中 @mention Bot，Hermes 会读取文档内容和评论上下文，直接在评论区回复。

---

## 微信（Weixin / WeChat）

> 适用于个人微信账号。企业微信请看下方 WeCom 部分。

### 前置依赖

默认**最小安装**内已包含核心依赖。若需要单独补装：

```bash
uv pip install --no-config aiohttp cryptography -i https://mirrors.aliyun.com/pypi/simple/
```

### 配置流程

```bash
hermes gateway setup    # 选择 Weixin
```

向导会：
1. 请求二维码 → 终端显示
2. 用微信扫码确认登录
3. 自动保存凭证到 `~/.hermes/weixin/accounts/`

手动配置 `~/.hermes/.env`：

```bash
WEIXIN_ACCOUNT_ID=your-account-id
WEIXIN_DM_POLICY=open                    # open | allowlist | disabled | pairing
WEIXIN_ALLOWED_USERS=user_id_1,user_id_2
WEIXIN_HOME_CHANNEL=chat_id
```

### 连接方式

HTTP 长轮询（非 WebSocket），无需公网端点。

### 特性

- AES-128-ECB 加密 CDN 媒体自动加解密
- Markdown 原生渲染
- 智能消息分块（4000 字符限制，逻辑边界拆分）
- 输入中指示器（"正在输入..."）
- 消息去重（5 分钟滑动窗口）
- 上下文 Token 持久化（重启后恢复回复连续性）

### 访问策略

| DM 策略 | 行为 |
|---------|------|
| `open` | 任何人可私聊（默认） |
| `allowlist` | 仅 allow_from 列表中用户 |
| `disabled` | 忽略所有私聊 |

群策略默认 `disabled`（个人微信可能在很多群中）。

---

## 企业微信（WeCom）

### 前置依赖

默认**最小安装**内已包含核心依赖。若需要单独补装：

```bash
uv pip install --no-config aiohttp httpx cryptography -i https://mirrors.aliyun.com/pypi/simple/
```

### 配置流程

**扫码创建（推荐）**：

```bash
hermes gateway setup    # 选择 WeCom，扫码自动创建 AI Bot
```

**手动配置** `~/.hermes/.env`：

```bash
WECOM_BOT_ID=your-bot-id
WECOM_SECRET=your-secret
WECOM_ALLOWED_USERS=user_id_1,user_id_2
WECOM_HOME_CHANNEL=chat_id
```

### 连接方式

WebSocket 长连接（`wss://openws.work.weixin.qq.com`），无需公网端点。自动心跳 + 指数退避重连。

### 访问策略

```bash
WECOM_DM_POLICY=open        # open | allowlist | disabled | pairing
WECOM_GROUP_POLICY=open     # open | allowlist | disabled
```

支持**按群精细控制**发言者白名单：

```yaml
platforms:
  wecom:
    enabled: true
    extra:
      bot_id: "your-bot-id"
      secret: "your-secret"
      group_policy: "allowlist"
      group_allow_from: ["group_id_1", "group_id_2"]
      groups:
        group_id_1:
          allow_from: ["user_alice", "user_bob"]
        "*":
          allow_from: ["user_admin"]
```

### 媒体支持

| 类型 | 入站 | 出站限制 |
|------|------|---------|
| 图片 | URL / base64，自动缓存 | 10 MB |
| 文件 | 自动缓存，保留文件名 | 20 MB |
| 语音 | 文字转写提取 | 2 MB（AMR） |
| 视频 | 下载缓存 | 10 MB |

超限自动降级为通用文件附件发送。入站 AES-256-CBC 加密媒体自动解密。

---

## 四平台对比速查

| 特性 | 钉钉 | 飞书 | 微信 | 企微 |
|------|------|------|------|------|
| 连接方式 | Stream (WebSocket) | WebSocket / Webhook | 长轮询 | WebSocket |
| 需要公网 | 否 | Webhook 模式需要 | 否 | 否 |
| 扫码配置 | 支持 | 支持 | 支持 | 支持 |
| AI 卡片 | 支持 | 支持（交互按钮） | 不支持 | 不支持 |
| 媒体加密 | 无 | 无 | AES-128-ECB | AES-256-CBC |
| 群策略默认 | @mention | @mention | disabled | open |
| 文档评论回复 | 不支持 | 支持 | 不支持 | 不支持 |

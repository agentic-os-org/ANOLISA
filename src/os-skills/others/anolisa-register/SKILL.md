---
name: anolisa-register
version: 1.0.0
description: 管理 Agentic OS 注册状态，支持查询注册状态、注册加入共建计划、注销退出。当用户询问anolisa注册状态、想加入共建、开启数据上传、关闭anolisa数据上传、查看anolisa注册隐私策略，或提到 anolisa register 相关操作时使用。
---

# Anolisa Subscription 管理

## 核心命令

| 操作 | 命令 | 说明 |
|------|------|------|
| 查询状态 | `anolisa register status` | 显示当前anolisa注册状态 |
| 查询状态（JSON） | `anolisa register status --json` | 返回机器可读格式 |
| 注册 | `sudo anolisa register` | 加入 Agentic OS 共建计划 |
| 注销 | `sudo anolisa unregister` | 退出共建，停止数据上传 |

## 自然语言意图映射

| 用户表达 | 识别意图 | 映射命令 |
|----------|----------|----------|
| "我现在的anolisa注册状态是什么" / "我注册了吗" | query_register_status | `anolisa register status` |
| "我想开启anolisa数据上传" / "注册到控制台" / "启用 Token 上报" | enable_upload | `sudo anolisa register`（需二次确认） |
| "我想关闭anolisa数据上传" / "撤销注册" / "取消注册" | disable_upload | `sudo anolisa unregister`（需二次确认） |

## 执行流程

### 查询注册状态

```bash
anolisa register status
```

输出示例：
```
Product: ECS
Consent State: REGISTERED
Data Upload: active
Registered: 2026-06-04 05:46 (via cli)
Operator: root
```

### 注册（加入共建计划）

```bash
sudo anolisa register
```

- 若已注册：输出提示 `Already registered.`，无需重复操作
- 若未注册：展示共建计划说明，交互确认 `[Y/N]`，用户确认后完成注册

**注意**：在调用此命令前应向用户展示注册前的二次确认，避免误操作。

**非交互环境处理**：如果 agent 不支持交互式 session（无法响应 `[Y/N]` 提示），则：
1. 先向用户展示下方「共建计划说明」的完整内容
2. 明确告知用户注册后会上传哪些数据、不会上传哪些数据
3. 获得用户明确同意后，使用 `sudo anolisa register --yes` 跳过交互确认完成注册

### 注销（退出共建）

```bash
sudo anolisa unregister
```

- 交互确认 `Unregister? [y/N]:`，输入 `y` 后注销
- 注销后数据上传停止，可随时重新注册

## 状态字段说明（JSON 格式）

```json
{
  "consent_state": "registered",   // registered | unregistered
  "operator": "root",              // 执行注册的用户
  "product_type": "ECS",           // 产品类型
  "registration_time": "2026-06-04T05:46:53Z",
  "source": "cli",                 // 注册来源
  "upload_active": true            // 数据上传是否活跃
}
```

## 共建计划说明

加入后用户获得：
- 更智能的 agent（从真实场景训练，更精准）
- 跨实例 Token 洞察（在控制台查看所有实例的费用与趋势）
- 个性化优化（模型选择、Token 节省、Skill 推荐）
- 新功能优先体验

承诺：
- **只上传**脱敏聚合统计（Token 数、模型 ID、请求次数、时间窗口）
- **绝不上传** Prompt、对话、密钥、文件
- 走阿里云内网，零公网流量，零额外配置
- 随时可通过 `sudo anolisa unregister` 退出

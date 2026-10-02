# selective-claw

[OpenClaw](https://github.com/openclaw/openclaw) 的选择性上下文管理插件。将所有消息完整保存在 SQLite 中，通过摘要压缩旧轮次 + 保留最近轮次原文的方式管理上下文窗口，并提供 `expand_turn` 工具让 LLM 按需展开被压缩的历史轮次。

## 目录

- [功能概述](#功能概述)
- [工作原理](#工作原理)
- [快速上手](#快速上手)
- [配置项](#配置项)
- [项目架构](#项目架构)
- [开发指南](#开发指南)
- [许可证](#许可证)

## 功能概述

当对话变长时，Agent 通常会丢失早期的决策、约束和上下文。selective-claw 通过以下方式解决这个问题：

1. **持久化所有消息** — 存入 SQLite 数据库，永不删除
2. **摘要压缩旧轮次** — 超出 freshTailTurns 的旧轮次自动生成一句话摘要
3. **保留最近轮次原文** — 最近 N 个 turn 的消息始终原样传递给 LLM
4. **按需展开** — LLM 可通过 `expand_turn` 工具，按 turn_seq 展开被压缩轮次的完整原始消息

## 工作原理

```
Gateway 调用链：assemble() → LLM 推理 → afterTurn()

assemble() 组装上下文：
┌─────────────────────────────────────────┐
│  [summary] Earlier conversation context: │  旧轮次的一句话摘要
│  Turn 1: 讨论了部署方案                    │
│  Turn 3: 确定了数据库选型                   │
├─────────────────────────────────────────┤
│  最近 3 个 turn 的原始消息                  │  原样保留
│  (user/assistant/tool 消息)               │
└─────────────────────────────────────────┘

LLM 推理时：
  - 看到摘要，如果需要某个旧 turn 的细节
  - 调用 expand_turn({ turn_ids: [1] })
  - 工具返回该 turn 的完整原始消息

afterTurn() 生成摘要：
  - 为 freshTail 之外且尚无摘要的 turn 并行调用 LLM
  - 每个 turn 生成一句话摘要，存入 DB
```

## 快速上手

### 前置条件

- 支持插件上下文引擎的 OpenClaw（`>=2026.5.22`）
- Node.js 22+（需要 `node:sqlite` 内置模块）

### 安装插件

通过本地路径安装（源码方式）：

```bash
openclaw plugins install /path/to/selective-claw
```

### 验证生效

安装后，selective-claw 会自动注册为上下文引擎并注册 `expand_turn` 工具。默认配置即可工作，无需额外设置。

## 配置项

所有配置均为可选。通过 OpenClaw 的插件配置进行设置：

```json
{
  "selective-claw": {
    "enabled": true,
    "freshTailTurns": 3,
    "dbPath": "~/.openclaw/selective-claw.db"
  }
}
```

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `enabled` | `true` | 启用或禁用插件 |
| `freshTailTurns` | `3` | 始终保留原文的最近轮次数 |
| `dbPath` | `~/.openclaw/selective-claw.db` | SQLite 数据库文件路径 |

## 项目架构

```
selective-claw/
├── src/
│   ├── plugin/index.ts      # OpenClaw 插件入口，注册引擎和 expand_turn 工具
│   ├── engine.ts             # SelectiveContextEngine（ContextEngine 实现）
│   ├── assembler.ts          # 摘要 + fresh tail 组装器
│   ├── recall-tool.ts        # expand_turn 工具实现
│   ├── summarize.ts          # LLM 摘要生成
│   ├── estimate-tokens.ts    # Token 估算
│   ├── fts5-sanitize.ts      # FTS5 查询清理
│   ├── openclaw-bridge.ts    # OpenClaw 类型定义
│   ├── types.ts              # 配置类型
│   ├── db/
│   │   ├── connection.ts     # SQLite 连接（WAL 模式）
│   │   └── migration.ts      # 数据表：conversations、messages、FTS5
│   └── store/
│       ├── message-store.ts  # 消息 CRUD + 全文搜索
│       └── index.ts          # 重导出
└── test/                     # Vitest 测试套件（64 个测试）
```

### 核心设计决策

- **`ownsCompaction: true`** — 告知 OpenClaw 不要触发自带的自动压缩，由 selective-claw 管理上下文
- **`compact()` 返回成功** — 实际裁剪在 assemble 中完成，compact 只是告诉 gateway "已处理"
- **消息通过 reconcile 入库** — Gateway 不调用 ingest()，消息通过 assemble/afterTurn 的 params.messages 增量导入
- **内容对齐** — reconcileMessages 使用 rawMessage JSON 匹配做增量导入，处理 gateway 替换消息的情况
- **FTS5 + Porter 词干提取** — `tokenize='porter unicode61'` 提供词干化和 Unicode 支持

## 开发指南

```bash
# 安装依赖
npm install

# 运行测试（需要 Node 22+，使用 node:sqlite）
npm test

# 构建
npm run build
```

### 运行测试

测试使用内存数据库，无需文件系统配置：

```bash
npm test                              # 全部 64 个测试
npx vitest run test/assembler.test.ts # 运行单个测试套件
```

## 许可证

Apache License 2.0

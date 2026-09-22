# 贡献指南

[English](CONTRIBUTING.md)

本文档介绍如何参与 Copilot Shell 的开发。

## 环境准备

### 系统要求

- Node.js ≥ 20
- npm ≥ 9
- Git

### 获取代码

```bash
git clone https://github.com/alibaba/anolisa.git
cd deprecated/copilot-shell
```

### 安装依赖

```bash
make deps
```

此命令会执行 `npm install` 并初始化 Husky Git hooks。

## 开发工作流

### 构建

```bash
make build
```

构建使用 esbuild，产物输出到各包的 `dist/` 目录。

### 运行

```bash
# 直接运行构建产物
node packages/cli/dist/index.js

# 或使用 npm script
cd packages/cli && npm start
```

### 代码检查

```bash
make lint
```

包含 ESLint 和类型检查。

### 格式化

项目使用 Prettier 进行代码格式化。编辑器保存时自动格式化，
或手动运行：

```bash
npx prettier --write .
```

### 测试

```bash
make test
```

测试框架为 vitest，测试文件与源码同目录，命名为 `*.test.ts`。

## 代码组织

### 包结构

| 包                          | 路径                   | 职责             |
| --------------------------- | ---------------------- | ---------------- |
| `@copilot-shell/cli`        | `packages/cli/`        | 命令行入口和 TUI |
| `@copilot-shell/core`       | `packages/core/`       | 核心引擎         |
| `@copilot-shell/test-utils` | `packages/test-utils/` | 测试工具         |

### 模块约定

- 使用 ESM（`"type": "module"`）
- 导出统一通过各包的 `src/index.ts`
- 类型声明与实现分离（`types.ts`）

## 提交规范

遵循项目根目录的提交规范，scope 为 `cosh`：

```
feat(cosh): add --json flag to config command
fix(cosh): handle empty model response gracefully
```

### 提交前检查

每次提交前确保通过：

```bash
make lint
make test
```

Husky pre-commit hook 会自动执行 lint 检查。

## 添加新的 Slash 命令

1. 在 `packages/cli/src/commands/` 下创建命令文件
2. 实现 `Command` 接口
3. 在命令注册处导入并注册
4. 添加对应的单元测试

## 添加新工具

1. 在 `packages/core/src/tools/` 下创建工具定义
2. 实现工具的 `execute` 方法
3. 在工具注册表中注册
4. 添加审批分类（哪些审批模式下需要确认）
5. 编写集成测试

## 添加新的 Hook 事件

1. 在 `packages/core/src/hooks/` 中定义事件类型
2. 实现事件的输入/输出 schema
3. 在 agent loop 的对应位置触发事件
4. 编写单元测试
5. 更新 Hook 开发文档

## 集成测试

集成测试位于 `integration-tests/` 目录：

```bash
# 运行所有集成测试
cd integration-tests && npm test

# 运行特定测试
npx vitest run integration-tests/hooks/
```

## 发布流程

发布通过 `/cosh-dev release` 技能自动完成：

1. 更新版本号
2. 生成 CHANGELOG
3. 构建并验证
4. 创建 Git tag
5. 推送到远程

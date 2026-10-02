# AgentSight Develop Skills

AgentSight 为开发者提供的 AI coding skill 集合，帮助 AI agent 遵循项目规范。

## 安装

将以下提示词发送给你使用的 AI coding agent（Claude Code、Qoder、Codex、Cursor 等），即可一键安装全部 skill：

```
读取 develop-skills/README.md 中的安装指令，将 develop-skills/ 下所有 agentsight-* 子目录复制到你的 skill 目录中。
```

### 安装指令（供 AI agent 执行）

1. 找到当前 agent 的 skill 目录：
   - Claude Code: `.claude/skills/`
   - Qoder: `.qoder/skills/`
   - Codex: `.codex/skills/`
   - 其他 agent: 参考对应文档确认 skill 目录位置

2. 将 `develop-skills/` 下每个 `agentsight-*` 子目录及其内容复制到 skill 目录中：
   ```
   develop-skills/agentsight-pr-body/  →  .<agent>/skills/agentsight-pr-body/
   ```

3. 确认每个子目录下的 `SKILL.md` 文件已正确复制。

## 可用 Skill

| Skill | 说明 |
|-------|------|
| `agentsight-pr-body` | 分析分支变更，按 anolisa 规范生成/更新 PR 标题和正文 |
| `agentsight-code-review` | 6 维度代码审查：硬性规则、eBPF 安全、FFI 边界、Footprint Ladder、流水线测试、文档同步 |
| `agentsight-auto-format` | 编辑代码后自动运行 rustfmt/ruff/prettier，保持代码风格一致 |
| `agentsight-bugfix` | Fix issues 流程：测试环境复现、数据流断点分析、构建部署验证的标准化调试迭代循环 |

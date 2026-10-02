---
name: skillfs-mount
description: >
  Help users configure, mount, and unmount a skillfs FUSE virtual filesystem
  on their local machine (e.g. for openclaw). Covers analyzing existing skill
  usage to suggest view configuration, generating skillfs-views.toml, mounting,
  and graceful unmount.
  Use this skill when the user asks to: set up skillfs, configure skill views,
  mount or unmount skillfs, check if skillfs is running, or get help organizing
  their skills directory (especially for openclaw users).
---

# skillfs 配置与挂载指南

## 前提条件

- `skillfs` 已安装（RPM 包），运行 `skillfs --version` 确认
- 系统已安装 `fuse3`（若缺失：`yum install -y fuse3`）
- 确认 skills 源目录路径（openclaw 用户通常为 `~/.openclaw/skills`）

---

## 操作流程

### STEP 1：了解用户类型，确定主视图

**有使用历史的用户（openclaw）：**

运行脚本分析历史调用频次，按调用次数决定哪些 skill 进主视图：

```bash
# 分析 openclaw session 日志中的 skill 使用频次（默认读 ~/.openclaw）
python3 <skill_dir>/scripts/skill_usage_from_session_logs.py

# 分析 copilot-shell (cosh) chat 日志中的 skill 调用统计（默认读 ~/.copilot-shell）
python3 <skill_dir>/scripts/skill_usage_from_chat_logs.py
```

两个脚本都支持 `--logs-dir` 指向自定义日志目录。

输出示例：
```
=== Skill Invocation Statistics ===
  github: 42
  slack: 28
  tmux: 17
  weather: 3
```

调用频次高的 skill 建议放入主视图，频次低或从未使用的放入次级视图。

**纯新用户（无历史）：**

列出 skills 目录内容，按用途给出分类建议：

```bash
ls ~/.openclaw/skills/
```

常见分类建议：
- 主视图（高频核心）：代码工具、版本管理、常用 API 集成
- 次级视图：不常用工具、实验性 skill、特定场景用途

---

### STEP 2：生成 skillfs-views.toml

根据分析结果，运行 classify 命令生成初始配置：

```bash
skillfs classify ~/.openclaw/skills --primary-count 8
```

生成的 `~/.openclaw/skills/skillfs-views.toml` 结构（视图名称固定为 `major` / `other`）：

```toml
[[view]]
name = "major"
default = true
description = "Core skills shown at mount time"
skills = ["github", "slack", "tmux", ...]

[[view]]
name = "other"
default = false
description = "Additional skills accessible via skill-discover"
skills = ["weather", "notion", ...]
```

> 挂载行为由 `default = true/false` 决定，视图名称仅作为 skill-discover 中的分组标题显示。

**按分析结果手动调整**：直接编辑 views.toml，在两个 skills 列表之间移动 skill 名称。

> ⚠️ views.toml 中的字符串必须与 skill 的**目录名**完全一致，否则该技能将从视图中
> 消失。目录名是权威的 skill key；`SKILL.md` frontmatter 里的 `name:` 只是展示元数据，
> 不参与视图匹配，重命名目录后也不会覆盖目录 key。

---

### STEP 3：挂载

```bash
# 原地挂载（推荐）：Agent 访问同一路径，FUSE 透明过滤内容
skillfs mount ~/.openclaw/skills ~/.openclaw/skills \
  --pid-file /tmp/skillfs.pid \
  --log-file /tmp/skillfs-{pid}.log &
sleep 1

# 确认挂载成功
ls ~/.openclaw/skills/    # 应只显示主视图 skill + skill-discover
```

挂载时自动将未出现在 views.toml 中的新 skill 追加到默认视图。

需要长期保持挂载时改用 managed 模式：它启动一个 detached supervisor，worker 意外退出后
会自动重挂，不需要自己管理 pid file。

```bash
skillfs mount ~/.openclaw/skills ~/.openclaw/skills --managed
```

---

### STEP 4：验证挂载状态

```bash
# 检查进程是否存活
kill -0 $(cat /tmp/skillfs.pid) 2>/dev/null && echo "mounted" || echo "not mounted"

# 查看实时日志
tail -f /tmp/skillfs-$(cat /tmp/skillfs.pid).log
```

---

### STEP 5：卸载

```bash
# managed 挂载：用 stop 停止 supervisor 并卸载
# 非 managed 或残留挂载：stop 也会直接卸载，可作为通用兜底
skillfs stop ~/.openclaw/skills

# 前台挂载也可以直接向进程发信号
kill -TERM $(cat /tmp/skillfs.pid)
sleep 1

# 卸载后目录恢复完整内容
ls ~/.openclaw/skills/

# 若进程意外退出，手动清理残留挂载：
fusermount3 -u ~/.openclaw/skills
```

PID 文件在卸载成功后自动删除。

---

## 关键说明

- SkillFS **不是只读**文件系统：挂载期间对 skill 的写入（含 install / update / remove）
  会透传到底层 source 树，写 `SKILL.md` 还会触发重新解析
- 但 views.toml 是例外：挂载期间它对 FUSE 不可见，需修改时先卸载
- 不生成 views.toml 也可直接挂载，此时所有 skill 均在主视图中可见
- 该工作区不能同时作为 ws-ckpt 工作区根初始化 —— in-place 挂载会让 `rename(2)` 返回
  `EBUSY`。先 `init` 再挂载，或先卸载再 `init`

---

## 故障排查

| 错误 | 解决方案 |
|------|----------|
| `Package fuse3 was not found` | `yum install -y fuse3 fuse3-devel` |
| `Transport endpoint is not connected` | `fusermount3 -u <mountpoint>` 后重新挂载 |
| skill 消失于列表 | 统一 skill 目录名与 views.toml 字符串（不是 SKILL.md 的 `name:`） |
| skill-discover 不完整 | 重新运行 `skillfs classify` 或手动编辑 views.toml |
| 原地挂载后无法写 views.toml | 先卸载，编辑，再重新挂载 |

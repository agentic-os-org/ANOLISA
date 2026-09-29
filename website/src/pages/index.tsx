import Head from '@docusaurus/Head';
import useBaseUrl from '@docusaurus/useBaseUrl';
import useDocusaurusContext from '@docusaurus/useDocusaurusContext';
import Layout from '@theme/Layout';
import ThemedImage from '@theme/ThemedImage';
import {useEffect, useRef, useState, type CSSProperties, type KeyboardEvent} from 'react';
import CopyCommand from '../components/CopyCommand';
import SiteLink from '../components/SiteLink';
import {installCommand, type Locale} from '../../content.config';

const content = {
  en: {
    badge: 'Agentic OS 1.0',
    lead: 'An operating system layer built for agents.',
    hook: 'Cut 30–70% of your agent’s tool-output tokens with one command.',
    systemScope: 'Just one thing the OS does for your agent — it also runs, recovers, and secures it.',
    statement:
      'The users of operating systems have changed. ANOLISA makes agents first-class participants at the system layer.',
    installLabel: 'One entry point. Enable what you need.',
    agentLabel: 'Bring your Agent in',
    agentPrompt:
      'Read https://agentic-os.sh/agents/ to learn how to use ANOLISA, then help me install it for this environment.',
    startTokenless: 'Start saving with Tokenless',
    exploreAnolisa: 'Explore ANOLISA',
    copy: 'Copy',
    copied: 'Copied',
    surfaceLabel: 'ANOLISA system surface',
    surfaceStatus: 'online',
    surfaceFooter: 'one entry · capabilities on demand',
    scenariosTitle: 'Solve the critical problems in Agent operations.',
    scenariosIntro:
      'Everyday Agent operations depend on terminal collaboration, Token efficiency, and execution environments. Each capability can be installed independently and integrated on demand.',
    openGuide: 'Open the guide',
    exploreTitle: 'Documentation and project resources',
    exploreIntro:
      'Find user guides, developer documentation, release history, and the machine-readable Agent entry point.',
  },
  zh: {
    badge: 'Agentic OS 1.0',
    lead: 'Agent 原生的操作系统层。',
    hook: '一条命令，让 Agent 少烧 30～70% 的工具输出 token。',
    systemScope: '这只是操作系统为 Agent 做的一件事——它还让 Agent 跑得起、退得回、守得住。',
    statement: '操作系统的使用者已经改变。ANOLISA 让 Agent 成为系统中的一等公民。',
    installLabel: '一个入口，按需启用',
    agentLabel: '让你的 Agent 接入',
    agentPrompt:
      '阅读 https://agentic-os.sh/agents/，了解如何使用 ANOLISA，并根据当前环境帮我完成安装。',
    startTokenless: '开始使用 Tokenless',
    exploreAnolisa: '了解 ANOLISA',
    copy: '复制',
    copied: '已复制',
    surfaceLabel: 'ANOLISA 系统能力视图',
    surfaceStatus: '在线',
    surfaceFooter: '一个入口 · 能力按需接入',
    scenariosTitle: '解决 Agent 运行中的关键问题。',
    scenariosIntro:
      'Agent 的日常运行，绕不开终端协作、Token 开销和执行环境。每项能力都可独立安装、按需接入。',
    openGuide: '打开指南',
    exploreTitle: '文档与项目资源',
    exploreIntro: '查阅用户指南、开发文档、版本记录和面向 Agent 的机器入口。',
  },
} as const;

type LocalizedText = Record<Locale, string>;
type CodeTone = 'meta' | 'keep' | 'noise' | 'remove' | 'add';

type CapabilityDemo = {
  id: string;
  icon: string;
  name: LocalizedText;
  metricLabel: LocalizedText;
  metricValue: string;
  sourceLabel: LocalizedText;
  resultLabel: LocalizedText;
  resultStatus: LocalizedText;
  action: LocalizedText;
  copy: LocalizedText;
  stages: readonly string[];
  raw: readonly {tone: CodeTone; text: string}[];
  optimized: readonly {tone: CodeTone; text: string}[];
};

type CapabilityShowcase = {
  id: string;
  componentLabel: string;
  accent: 'cyan' | 'lime' | 'amber';
  processor: string;
  eyebrow: LocalizedText;
  title: LocalizedText;
  intro: LocalizedText;
  note: LocalizedText;
  noteLink?: LocalizedText;
  noteHref?: string;
  views: readonly CapabilityDemo[];
};

const featureUiContent = {
  en: {
    railLabel: 'ANOLISA capability areas',
    demoTabs: 'Capability examples',
    activeStages: 'Active stages',
  },
  zh: {
    railLabel: 'ANOLISA 能力分组',
    demoTabs: '能力演示',
    activeStages: '当前处理阶段',
  },
} as const;

const tokenDemos = {
  json: {
    icon: '{}',
    name: {en: 'JSON / API', zh: 'JSON / API'},
    metricLabel: {en: 'Response compression', zh: 'Response 压缩'},
    metricValue: '26–78%',
    copy: {
      en: 'When an array is long, Tokenless keeps its first 32 items and moves the rest to Stash. IDs and paths remain in the current context.',
      zh: '数组过长时，Tokenless 保留前 32 项，并把其余内容存入 Stash。当前上下文仍会保留 ID 和 Path。',
    },
    stages: ['Response', 'TOON', 'Stash'],
    raw: [
      {tone: 'meta', text: '{ "tool": "search_code",'},
      {tone: 'keep', text: '  "status": "ok",'},
      {tone: 'noise', text: '  "debug": { "latency_ms": 14,'},
      {tone: 'noise', text: '    "trace": "gw-84f19", "cache": "hit" },'},
      {tone: 'meta', text: '  "results": ['},
      {tone: 'keep', text: '    { "id": 0, "name": "item-0",'},
      {tone: 'keep', text: '      "path": "src/module_0/file_0.rs" },'},
      {tone: 'keep', text: '    { "id": 1, "name": "item-1", … },'},
      {tone: 'noise', text: '    … 98 more result records …'},
      {tone: 'meta', text: '  ],'},
      {tone: 'keep', text: '  "request_id": "req-8f3a91" }'},
    ],
    optimized: [
      {tone: 'meta', text: 'tool: search_code'},
      {tone: 'keep', text: 'status: ok'},
      {tone: 'meta', text: 'results[32]{id,name,path}:'},
      {tone: 'keep', text: '  0,item-0,src/module_0/file_0.rs'},
      {tone: 'keep', text: '  1,item-1,src/module_1/file_1.rs'},
      {tone: 'noise', text: '  … 30 more retained rows …'},
      {tone: 'meta', text: '<<tokenless:7f3a…>> · 68 retrievable'},
      {tone: 'keep', text: 'request_id: req-8f3a91'},
    ],
  },
  logs: {
    icon: '>_',
    name: {en: 'Build logs', zh: '构建日志'},
    metricLabel: {en: 'Command output', zh: '命令输出'},
    metricValue: '60–90%',
    copy: {
      en: 'Build progress and passing checks collapse. Warnings, failures, and nearby context remain.',
      zh: '编译进度和已通过的项目会被收起，Warning、Failure 及其附近内容继续保留。',
    },
    stages: ['RTK rewrite', 'Errors kept', 'Bounded context'],
    raw: [
      {tone: 'noise', text: 'Compiling tokenless-schema v0.7.6'},
      {tone: 'noise', text: 'Compiling tokenless-cli v0.7.6'},
      {tone: 'keep', text: 'warning: unused import: `Duration`'},
      {tone: 'noise', text: 'Running unittests src/lib.rs'},
      {tone: 'noise', text: 'test response::empty_values ... ok'},
      {tone: 'noise', text: 'test response::keeps_ids ... ok'},
      {tone: 'keep', text: 'test response::retains_paths ... FAILED'},
      {tone: 'keep', text: "thread 'response::retains_paths' panicked:"},
      {tone: 'keep', text: 'assertion failed: retained >= expected'},
      {tone: 'noise', text: 'note: run with RUST_BACKTRACE=1'},
      {tone: 'keep', text: 'test result: FAILED. 95 passed; 1 failed'},
    ],
    optimized: [
      {tone: 'meta', text: 'cargo test · 1 failure'},
      {tone: 'keep', text: 'warning: unused import: `Duration`'},
      {tone: 'keep', text: 'response::retains_paths ... FAILED'},
      {tone: 'keep', text: "thread 'response::retains_paths' panicked:"},
      {tone: 'keep', text: 'assertion failed: retained >= expected'},
      {tone: 'noise', text: 'context: RUST_BACKTRACE=1 available'},
      {tone: 'meta', text: 'FAILED · 95 passed · 1 failed'},
    ],
  },
  diff: {
    icon: '±',
    name: {en: 'Git diff', zh: 'Git Diff'},
    metricLabel: {en: 'Command output', zh: '命令输出'},
    metricValue: '60–90%',
    copy: {
      en: 'Changed lines keep the context needed to understand them. Large unchanged blocks stay out of the prompt.',
      zh: '改动行和必要的上下文会被保留，大段未变化的内容不会进入 Prompt。',
    },
    stages: ['RTK rewrite', 'Changed lines', 'Bounded context'],
    raw: [
      {tone: 'meta', text: 'diff --git a/src/config.rs b/src/config.rs'},
      {tone: 'noise', text: 'index 24d81ad..83af920 100644'},
      {tone: 'meta', text: '@@ -42,9 +42,9 @@ impl Defaults {'},
      {tone: 'noise', text: '     Self {'},
      {tone: 'noise', text: '       compression: true,'},
      {tone: 'remove', text: '-      truncate_arrays_at: 16,'},
      {tone: 'add', text: '+      truncate_arrays_at: 32,'},
      {tone: 'noise', text: '       stash: true,'},
      {tone: 'noise', text: '       stats: Stats::default(),'},
      {tone: 'noise', text: '       timeout_ms: 5000,'},
      {tone: 'noise', text: '     }'},
    ],
    optimized: [
      {tone: 'meta', text: 'src/config.rs · 1 line changed'},
      {tone: 'meta', text: '@@ -42,3 +42,3 @@ Defaults'},
      {tone: 'noise', text: '  compression: true,'},
      {tone: 'remove', text: '- truncate_arrays_at: 16,'},
      {tone: 'add', text: '+ truncate_arrays_at: 32,'},
      {tone: 'noise', text: '  stash: true'},
    ],
  },
} as const;

type TokenDemoId = keyof typeof tokenDemos;

const tokenlessViews: readonly CapabilityDemo[] = (
  Object.entries(tokenDemos) as [TokenDemoId, (typeof tokenDemos)[TokenDemoId]][]
).map(([id, demo]) => ({
  id,
  ...demo,
  sourceLabel: {en: 'Original tool output', zh: '原始工具输出'},
  resultLabel: {en: 'Context-ready output', zh: '上下文就绪输出'},
  resultStatus: {en: 'signal preserved', zh: '关键信号已保留'},
  action: {en: 'compress', zh: '压缩'},
}));

const capabilityShowcases: readonly CapabilityShowcase[] = [
  {
    id: 'cosh-ng',
    componentLabel: 'cosh-ng',
    accent: 'cyan',
    processor: 'cosh-ng',
    eyebrow: {en: 'COSH-NG · TERMINAL COLLABORATION', zh: 'COSH-NG · 终端协作'},
    title: {
      en: 'Keep your terminal. Keep your session.',
      zh: '不换终端，不搬会话。',
    },
    intro: {
      en: 'Cosh-ng layers AI onto the bash or zsh you already use. Commands, aliases, scripts, and interactive programs keep working while the Agent takes over only when you ask.',
      zh: 'Cosh-ng 把 AI 叠在原来的 Shell 会话上。命令、别名、脚本和交互程序照常工作，需要时让 Agent 接手。',
    },
    note: {
      en: 'Ask directly, accept a failure insight, or start with a login health check. Tab\u00a0and\u00a0Enter submit; typing on ignores it.',
      zh: '直接交代任务，也可以采纳失败洞察或登录健康检查。按 Tab\u00a0和\u00a0Enter 提交，继续输入即可忽略。',
    },
    views: [
      {
        id: 'terminal',
        icon: '>_',
        name: {en: 'Terminal session', zh: '终端会话'},
        metricLabel: {en: 'Interaction', zh: '协作方式'},
        metricValue: 'SHELL + AGENT',
        sourceLabel: {en: 'Task in the shell', zh: 'Shell 中的任务'},
        resultLabel: {en: 'Agent workspace', zh: 'Agent 工作现场'},
        resultStatus: {en: 'control preserved', zh: '控制权始终保留'},
        action: {en: 'collaborate', zh: '协作'},
        copy: {
          en: 'Natural-language tasks, tool output, and approval steps share one terminal. Interrupt now and resume the same conversation later.',
          zh: '自然语言任务、工具输出和确认步骤都在一个终端里。你可以随时中断，也可以稍后继续同一段会话。',
        },
        stages: ['Agent pane', 'Approval', 'Resume'],
        raw: [
          {tone: 'meta', text: '$ cosh-ng'},
          {tone: 'keep', text: 'you › fix the failing checkout tests'},
          {tone: 'noise', text: 'agent › inspecting the workspace'},
          {tone: 'meta', text: 'tool › cargo test checkout'},
          {tone: 'keep', text: 'checkout::expired_session ... FAILED'},
          {tone: 'noise', text: 'agent › tracing the failing branch'},
          {tone: 'keep', text: 'proposal › edit src/checkout.rs'},
          {tone: 'meta', text: 'approval › waiting for your decision'},
        ],
        optimized: [
          {tone: 'meta', text: 'session task-4c21 · resumable'},
          {tone: 'keep', text: 'agent pane › plan + live output'},
          {tone: 'keep', text: 'approval card › edit src/checkout.rs'},
          {tone: 'add', text: '✓ approved for this step'},
          {tone: 'meta', text: '$ cargo test checkout'},
          {tone: 'add', text: 'test result: ok · 8 passed'},
          {tone: 'noise', text: 'Ctrl+C › return to the shell anytime'},
        ],
      },
    ],
  },
  {
    id: 'tokenless',
    componentLabel: 'Tokenless',
    accent: 'lime',
    processor: 'tokenless',
    eyebrow: {en: 'TOKENLESS · TOOL OUTPUT COMPRESSION', zh: 'TOKENLESS · 工具输出压缩'},
    title: {en: 'Keep what matters. Send fewer tokens.', zh: '保留关键信息，少用一些 Token。'},
    intro: {
      en: 'Tokenless compresses tool output before it reaches the model. Errors, changes, and useful fields stay in context. Repeated records and progress noise take up less space.',
      zh: 'Tokenless 会在工具输出进入模型前先做一次压缩。错误、变更和关键字段留下，重复内容和过程噪声少占上下文。',
    },
    note: {
      en: 'These ranges come from repository benchmarks. Results vary by payload and cannot be translated directly into provider billing savings.',
      zh: '这里展示仓库中的 Benchmark 区间。实际结果会随 Payload 变化，也不能直接换算成 Provider 账单的节省比例。',
    },
    noteLink: {en: 'Read the benchmark methodology', zh: '查看 Benchmark 方法'},
    noteHref: 'https://github.com/agentic-os-org/ANOLISA/tree/main/src/tokenless/benchmark',
    views: tokenlessViews,
  },
  {
    id: 'agentsight',
    componentLabel: 'AgentSight',
    accent: 'lime',
    processor: 'agentsight',
    eyebrow: {en: 'AGENTSIGHT · TRAJECTORY & TOKEN VISIBILITY', zh: 'AGENTSIGHT · 轨迹与 TOKEN 观测'},
    title: {en: 'Lay an Agent run out in front of you.', zh: '把一次 Agent 运行摊开来看。'},
    intro: {
      en: 'Model responses, tool calls, observations, and token use return to one structured trajectory, so you can see what happened inside a run.',
      zh: '模型调用了什么，工具返回了什么，上下文花在哪里，都会回到同一份运行轨迹里。',
    },
    note: {
      en: 'Linux adds eBPF visibility without modifying Agent code. macOS can collect local sessions and display their trajectories.',
      zh: 'Linux 上无需修改 Agent 代码即可获得 eBPF 深度观测。macOS 可以采集本地会话并查看运行轨迹。',
    },
    views: [
      {
        id: 'trajectory',
        icon: '◎',
        name: {en: 'Run trajectory', zh: '运行轨迹'},
        metricLabel: {en: 'Visibility', zh: '观测范围'},
        metricValue: 'TRACE + TOKENS',
        sourceLabel: {en: 'Live agent events', zh: '实时 Agent 事件'},
        resultLabel: {en: 'Structured trajectory', zh: '结构化运行轨迹'},
        resultStatus: {en: 'run made inspectable', zh: '运行过程可检查'},
        action: {en: 'observe', zh: '观测'},
        copy: {
          en: 'Model calls and tool activity become one ordered trajectory. On Linux, token events show where context is being spent.',
          zh: '模型调用和工具活动会按顺序汇成一条轨迹。在 Linux 上，Token 事件还能说明上下文主要花在了哪里。',
        },
        stages: ['Collect', 'Correlate', 'Inspect'],
        raw: [
          {tone: 'meta', text: 'trace.start › run-84f1'},
          {tone: 'keep', text: 'model.request › plan checkout fix'},
          {tone: 'noise', text: 'process.exec › git status'},
          {tone: 'keep', text: 'tool.call › search_code'},
          {tone: 'noise', text: 'file.open › src/checkout.rs'},
          {tone: 'keep', text: 'model.response › propose patch'},
          {tone: 'meta', text: 'token.event › prompt + completion'},
          {tone: 'noise', text: 'trace.end › success'},
        ],
        optimized: [
          {tone: 'meta', text: 'run-84f1 · success · 2.4s'},
          {tone: 'keep', text: '01 model › formed a plan'},
          {tone: 'keep', text: '02 tool › searched repository'},
          {tone: 'keep', text: '03 file › inspected checkout.rs'},
          {tone: 'keep', text: '04 model › proposed the patch'},
          {tone: 'add', text: 'tokens › prompt · completion · tools'},
          {tone: 'noise', text: 'viewer › replay any event in context'},
        ],
      },
    ],
  },
  {
    id: 'ws-ckpt',
    componentLabel: 'ws-ckpt',
    accent: 'amber',
    processor: 'ws-ckpt',
    eyebrow: {en: 'WS-CKPT · WORKSPACE RECOVERY', zh: 'WS-CKPT · 工作区恢复'},
    title: {en: 'Restore a known-good workspace when changes go wrong.', zh: '工作区出了问题，退\u2060回上一个可用状态。'},
    intro: {
      en: 'With automatic checkpoints enabled, ws-ckpt keeps a baseline and end-of-turn recovery points. Preview affected files first, then restore the whole workspace when you are ready.',
      zh: '显式开启自动检查点后，ws-ckpt 会在会话开始和每轮结束时留下恢复点。回滚前先看哪些文件会变化，确认后再恢复整个工作区。',
    },
    note: {
      en: 'Automatic checkpoints are opt-in. The CLI runs without root while privileged snapshot operations stay inside the Linux daemon.',
      zh: '自动检查点需要显式开启。CLI 无需 root，特权快照操作只在 Linux Daemon 内完成。',
    },
    views: [
      {
        id: 'recovery',
        icon: '↶',
        name: {en: 'Checkpoint & rollback', zh: '检查点与回滚'},
        metricLabel: {en: 'Snapshot creation', zh: '快照创建'},
        metricValue: 'CoW SNAPSHOT',
        sourceLabel: {en: 'Changing workspace', zh: '正在变化的工作区'},
        resultLabel: {en: 'Recovery point', zh: '可恢复检查点'},
        resultStatus: {en: 'rollback ready', zh: '已可回滚'},
        action: {en: 'checkpoint', zh: '创建检查点'},
        copy: {
          en: 'Each saved turn becomes a recovery point. Preview the affected files, then roll back along the snapshot history when a change goes wrong.',
          zh: '每个已保存的回合都会成为恢复点。改动出错时，可以先预览受影响文件，再沿快照历史快速回退。',
        },
        stages: ['Checkpoint', 'Diff', 'Rollback'],
        raw: [
          {tone: 'meta', text: '$ ws-ckpt checkpoint -s before-refactor'},
          {tone: 'keep', text: 'workspace › ~/project'},
          {tone: 'noise', text: 'agent edits › src/checkout.rs'},
          {tone: 'noise', text: 'agent edits › src/session.rs'},
          {tone: 'remove', text: 'tests › checkout regression detected'},
          {tone: 'meta', text: '$ ws-ckpt rollback -s before-refactor --preview'},
          {tone: 'keep', text: 'preview › 2 files will be restored'},
        ],
        optimized: [
          {tone: 'add', text: 'checkpoint › before-refactor'},
          {tone: 'meta', text: 'snapshot › ckpt-20260814-01'},
          {tone: 'keep', text: 'diff › src/checkout.rs modified'},
          {tone: 'keep', text: 'diff › src/session.rs modified'},
          {tone: 'noise', text: 'preview › no files changed yet'},
          {tone: 'add', text: 'rollback ready › confirm to restore'},
        ],
      },
    ],
  },
  {
    id: 'skillfs',
    componentLabel: 'SkillFS',
    accent: 'amber',
    processor: 'skillfs',
    eyebrow: {en: 'SKILLFS · SKILLS ON DEMAND', zh: 'SKILLFS · SKILLS 按需挂载'},
    title: {en: 'Put only task-relevant Skills in view.', zh: '只把当前任务需要的 Skills 放到眼前。'},
    intro: {
      en: 'SkillFS organizes one Skill repository into focused views. Common Skills stay in front of the Agent; the rest remain discoverable when needed.',
      zh: 'SkillFS 把同一座技能仓库整理成不同视图。常用 Skills 直接出现在 Agent 面前，其余能力留在次级视图，需要时再打开。',
    },
    note: {
      en: 'SkillFS requires Linux and FUSE. It controls how Skills are exposed; scanning, signing, and risk decisions belong to the security layer.',
      zh: 'SkillFS 需要 Linux 和 FUSE。它负责如何向 Agent 提供 Skills，扫描、签名和风险判断由安全层负责。',
    },
    views: [
      {
        id: 'mount',
        icon: '▤',
        name: {en: 'Mounted skill view', zh: 'Skill 挂载视图'},
        metricLabel: {en: 'Exposure', zh: '提供方式'},
        metricValue: 'ON DEMAND',
        sourceLabel: {en: 'Local skill catalog', zh: '本地 Skill 目录'},
        resultLabel: {en: 'Agent-facing view', zh: '面向 Agent 的视图'},
        resultStatus: {en: 'skills mounted', zh: 'Skills 已挂载'},
        action: {en: 'mount', zh: '挂载'},
        copy: {
          en: 'Choose a view, mount it, and let the Agent read the compiled instructions it needs. Secondary Skills remain discoverable without crowding the default view.',
          zh: '选择视图并完成挂载，Agent 就能读取当前需要的编译后说明。其他 Skills 仍可发现，但不会挤满默认视图。',
        },
        stages: ['Select view', 'Compile', 'Mount'],
        raw: [
          {tone: 'meta', text: '/skills-source'},
          {tone: 'keep', text: '├── coding/SKILL.md'},
          {tone: 'keep', text: '├── review/SKILL.md'},
          {tone: 'noise', text: '├── operations/SKILL.md'},
          {tone: 'noise', text: '├── research/SKILL.md'},
          {tone: 'meta', text: '└── skillfs-views.toml'},
          {tone: 'keep', text: 'default = ["coding", "review"]'},
        ],
        optimized: [
          {tone: 'meta', text: '/agent-skills · mounted'},
          {tone: 'add', text: '├── coding/SKILL.md · compiled'},
          {tone: 'add', text: '├── review/SKILL.md · compiled'},
          {tone: 'keep', text: '└── skill-discover/SKILL.md'},
          {tone: 'noise', text: 'secondary › operations, research'},
          {tone: 'meta', text: 'ordinary files › source-backed'},
        ],
      },
    ],
  },
  {
    id: 'sec-core',
    componentLabel: 'Agent Sec Core',
    accent: 'amber',
    processor: 'sec-core',
    eyebrow: {en: 'AGENT SEC CORE · EXECUTION POLICY', zh: 'AGENT SEC CORE · 执行策略'},
    title: {en: 'Give every security layer one clear job.', zh: '每个安全组件，各守住一层边界。'},
    intro: {
      en: 'Sandbox execution, Skill integrity, code scanning, and host hardening cover different risks. Use them independently or combine them into defense in depth.',
      zh: '沙箱执行、Skill 完整性、代码扫描和主机加固分别处理不同风险。它们可以独立启用，也可以组合成纵深防护。',
    },
    note: {
      en: 'Linux enforces the sandbox boundary. System hardening, asset verification, and host approval policies remain separate security layers.',
      zh: 'Linux 负责落实沙箱边界。系统加固、Skill 资产校验和宿主审批策略是彼此独立的安全层。',
    },
    views: [
      {
        id: 'policy',
        icon: '◇',
        name: {en: 'Command policy', zh: '命令策略'},
        metricLabel: {en: 'Security capabilities', zh: '安全能力'},
        metricValue: '4',
        sourceLabel: {en: 'Requested command', zh: '待执行命令'},
        resultLabel: {en: 'Sandbox decision', zh: '沙箱执行策略'},
        resultStatus: {en: 'policy applied', zh: '策略已应用'},
        action: {en: 'classify', zh: '判断风险'},
        copy: {
          en: 'Each component handles a distinct security question, from the command boundary to Skill drift and host baseline findings.',
          zh: '每个组件负责一个明确问题，从命令权限边界，到 Skill 漂移和主机基线风险，都能单独查看。',
        },
        stages: ['Classify', 'Policy', 'Sandbox'],
        raw: [
          {tone: 'meta', text: 'agent requests › npm install package-x'},
          {tone: 'keep', text: 'operation › package install'},
          {tone: 'noise', text: 'filesystem › workspace writes requested'},
          {tone: 'noise', text: 'network › registry access requested'},
          {tone: 'keep', text: 'risk signal › code execution + network'},
          {tone: 'meta', text: 'decision pending'},
        ],
        optimized: [
          {tone: 'keep', text: 'risk › medium'},
          {tone: 'meta', text: 'decision › sandbox + confirmation'},
          {tone: 'add', text: 'filesystem › workspace + /tmp only'},
          {tone: 'remove', text: 'network › denied until approved'},
          {tone: 'keep', text: 'approval › required from user'},
          {tone: 'meta', text: 'audit › decision recorded'},
        ],
      },
    ],
  },
] as const satisfies readonly CapabilityShowcase[];

type CapabilityId = (typeof capabilityShowcases)[number]['id'];

const capabilityById = Object.fromEntries(
  capabilityShowcases.map((capability) => [capability.id, capability]),
) as Record<CapabilityId, CapabilityShowcase>;

const componentCapabilityIds: Record<string, CapabilityId> = Object.fromEntries(
  capabilityShowcases.map((capability) => [capability.componentLabel, capability.id]),
) as Record<string, CapabilityId>;

const scenarios = {
  en: [
    {
      role: 'TERMINAL COLLABORATION',
      name: 'Cosh-ng',
      title: 'Run and take control of Agents in the terminal.',
      surfaceTitle: 'Let Agents work directly in your terminal.',
      proof: 'No separate chat window required',
      surfaceBody: 'Follow live output and take control whenever you need to.',
      body:
        'Cosh-ng runs in the Shell you already use. Give it tasks in natural language, call tools and Skills, and connect existing workflows through Hooks or MCP.',
      promise:
        'The terminal remains your workspace, and you can interrupt or resume the Agent at any time.',
      cta: 'Start with Cosh-ng',
      components: [
        {
          label: 'cosh-ng',
          description: 'Run Agents in the terminal and take control at any time.',
          href: '/docs/user-guide/user-entrypoint/cosh-ng/quickstart',
        },
      ],
      href: '/docs/user-guide/user-entrypoint/cosh-ng/quickstart',
      accent: 'cyan',
    },
    {
      role: 'TOKEN & CONTEXT',
      name: 'Token Flow',
      title: 'See where Tokens go and reduce tool-output cost.',
      surfaceTitle: 'Compress tool output without changing Agent code.',
      proof: '30–70% tool-output compression*',
      surfaceBody:
        'Works with Claude Code, Codex, Qoder, and more. Original content stays retrievable.',
      body:
        'Tokenless compresses JSON, logs, and diffs before tool output reaches the model. AgentSight records trajectories and tracks Token usage on Linux so you can see where the context is going.',
      promise: 'Review the compression result and retrieve original content when needed.',
      cta: 'Start with Tokenless',
      components: [
        {
          label: 'Tokenless',
          description: 'Compress tool output before it enters the model context.',
          href: '/docs/user-guide/token-saving/tokenless/quickstart',
        },
        {
          label: 'AgentSight',
          description: 'Inspect trajectories and track Token usage on Linux.',
          href: '/docs/user-guide/agent-observability/agentsight',
        },
      ],
      href: '/docs/user-guide/token-saving/tokenless/quickstart',
      accent: 'lime',
    },
    {
      role: 'RUNTIME & SECURITY',
      name: 'Agent Runtime',
      title: 'Run Agent tasks with isolation and workspace recovery.',
      surfaceTitle: 'Isolate risky commands and keep workspace recovery points.',
      proof: 'Isolation · snapshots · Skills on demand',
      surfaceBody:
        'Agent Sec Core limits risky commands. ws-ckpt handles recovery, while SkillFS mounts Skills on demand.',
      body:
        'Agent Sec Core classifies commands and applies sandbox policies. ws-ckpt creates fast workspace checkpoints and rolls back file changes. SkillFS exposes selected Skills through a mounted filesystem when they are needed.',
      promise:
        'These Linux components work independently and can be combined for tasks that need stronger controls.',
      cta: 'Start with ws-ckpt',
      components: [
        {
          label: 'ws-ckpt',
          description: 'Create workspace checkpoints and roll back file changes.',
          href: '/docs/user-guide/runtime/ws-ckpt',
        },
        {
          label: 'SkillFS',
          description: 'Expose selected Skills through a mounted filesystem.',
          href: '/docs/user-guide/runtime/skillfs',
        },
        {
          label: 'Agent Sec Core',
          description: 'Classify risky commands and apply sandbox policies.',
          href: '/docs/user-guide/agent-security/agent-sec-core/quickstart',
        },
      ],
      href: '/docs/user-guide/runtime/ws-ckpt',
      accent: 'amber',
    },
  ],
  zh: [
    {
      role: '终端协作',
      name: 'Cosh-ng',
      title: '在终端里运行 Agent，也能随时接管',
      surfaceTitle: '让 Agent 直接在你的终端里工作',
      proof: '无需切换到单独的聊天窗口',
      surfaceBody: '跟随实时输出，需要时随时接管。',
      body:
        'Cosh-ng 运行在你熟悉的 Shell 里。你可以用自然语言分配任务，调用工具和 Skills，并通过 Hooks 或 MCP 接入现有工作流。',
      promise: '终端仍是工作现场，你可以随时中断或恢复 Agent。',
      cta: '开始使用 Cosh-ng',
      components: [
        {
          label: 'cosh-ng',
          description: '在终端中运行 Agent，需要时随时接管。',
          href: '/docs/user-guide/user-entrypoint/cosh-ng/quickstart',
        },
      ],
      href: '/docs/user-guide/user-entrypoint/cosh-ng/quickstart',
      accent: 'cyan',
    },
    {
      role: 'TOKEN 与上下文',
      name: 'Token Flow',
      title: '看清 Token 花在哪里，减少工具输出开销',
      surfaceTitle: '无需修改 Agent 代码，直接压缩工具输出',
      proof: '30～70% 工具输出压缩*',
      surfaceBody: '支持 Claude Code、Codex、Qoder 等，压缩后仍可取回原文。',
      body:
        'Tokenless 会在工具输出进入模型前压缩 JSON、日志和 Diff。AgentSight 记录运行轨迹，并在 Linux 上统计 Token 用量，让你知道上下文花在了哪里。',
      promise: '压缩结果可以查看，需要时仍能取回原始内容。',
      cta: '从 Tokenless 开始',
      components: [
        {
          label: 'Tokenless',
          description: '在工具输出进入模型上下文前完成压缩。',
          href: '/docs/user-guide/token-saving/tokenless/quickstart',
        },
        {
          label: 'AgentSight',
          description: '查看运行轨迹，并在 Linux 上统计 Token 用量。',
          href: '/docs/user-guide/agent-observability/agentsight',
        },
      ],
      href: '/docs/user-guide/token-saving/tokenless/quickstart',
      accent: 'lime',
    },
    {
      role: '运行与安全',
      name: 'Agent Runtime',
      title: '隔离高风险操作，也为工作区保留恢复点',
      surfaceTitle: '隔离高风险命令，并为工作区保留恢复点',
      proof: '隔离执行 · 工作区快照 · Skills 按需挂载',
      surfaceBody: 'Agent Sec Core 限制高风险命令，ws-ckpt 负责恢复，SkillFS 按需挂载 Skills。',
      body:
        'Agent Sec Core 会判断命令风险并应用沙箱策略。ws-ckpt 可以创建工作区检查点，也能回滚文件变更。SkillFS 在需要时通过挂载文件系统提供指定 Skills。',
      promise: '这些组件面向 Linux，可以独立启用，也可以按任务需要组合使用。',
      cta: '从 ws-ckpt 开始',
      components: [
        {
          label: 'ws-ckpt',
          description: '创建工作区检查点，出现问题时回滚文件变更。',
          href: '/docs/user-guide/runtime/ws-ckpt',
        },
        {
          label: 'SkillFS',
          description: '通过挂载文件系统向 Agent 提供指定 Skills。',
          href: '/docs/user-guide/runtime/skillfs',
        },
        {
          label: 'Agent Sec Core',
          description: '判断命令风险，并为执行过程应用沙箱策略。',
          href: '/docs/user-guide/agent-security/agent-sec-core/quickstart',
        },
      ],
      href: '/docs/user-guide/runtime/ws-ckpt',
      accent: 'amber',
    },
  ],
} as const;

const scenarioRailIcons = {
  cyan: '>_',
  lime: '{}',
  amber: '[]',
} as const;

const componentRailIcons: Record<string, string> = {
  'cosh-ng': '>_',
  Tokenless: 'T/',
  AgentSight: '◎',
  'Agent Sec Core': '◇',
  'ws-ckpt': '↶',
  SkillFS: '▤',
};

const routes = {
  en: [
    {
      label: 'USER GUIDE',
      title: 'Use ANOLISA',
      body: 'Install, configure, operate, and troubleshoot each capability.',
      href: '/docs/user-guide',
    },
    {
      label: 'DEVELOPER GUIDE',
      title: 'Build with ANOLISA',
      body: 'Read architecture, protocols, extension points, and test guidance.',
      href: '/docs/developer-guide',
    },
    {
      label: 'CHANGELOG',
      title: 'Follow releases',
      href: '/changelog',
    },
    {
      label: 'FOR AGENTS',
      title: 'Read the machine entry point',
      body: 'Let your Agent read and understand ANOLISA.',
      href: '/agents/',
    },
  ],
  zh: [
    {
      label: '用户指南',
      title: '使用 ANOLISA',
      body: '查找各项能力的安装、配置、运行和故障排查说明。',
      href: '/docs/user-guide',
    },
    {
      label: '开发者指南',
      title: '参与 ANOLISA 开发',
      body: '阅读架构、协议、扩展点与测试说明。',
      href: '/docs/developer-guide',
    },
    {
      label: 'CHANGELOG',
      title: '了解版本变化',
      href: '/changelog',
    },
    {
      label: 'FOR AGENTS',
      title: '打开 Agent 入口',
      body: '让你的 Agent 读取并了解 ANOLISA。',
      href: '/agents/',
    },
  ],
} as const;

function CoshVisual({locale}: {locale: Locale}) {
  const zh = locale === 'zh';
  const [activeView, setActiveView] = useState<'handoff' | 'insight' | 'health'>('handoff');
  const viewLabels = [
    ['handoff', zh ? '一句话接手' : 'Ask in plain language', zh ? '自然语言、审批与原生交互' : 'Intent, approval, and native interaction'],
    ['insight', zh ? '失败后补位' : 'Help after failure', zh ? '命令挂了，Tab 采纳分析' : 'Accept an insight with Tab'],
    ['health', zh ? '登录健康检查' : 'Login health check', zh ? '风险和排查建议直接出现在首屏' : 'See risks and next steps as you log in'],
  ] as const;

  return (
    <div className="capabilityCanvas coshCanvas">
      <nav className="capabilityFeaturePicker" aria-label={zh ? 'Cosh-ng 共驾方式' : 'Cosh-ng copilot modes'}>
        {viewLabels.map(([id, label, description]) => (
          <button className={activeView === id ? 'is-active' : ''} type="button" aria-pressed={activeView === id} onClick={() => setActiveView(id)} key={id}>
            <span>{label}</span><small>{description}</small>
          </button>
        ))}
      </nav>

      <section className={`coshStage coshStage--${activeView}`} key={activeView}>
        <header className="featureStageHeader"><span><i /> cosh-ng · bash/zsh</span><b>{zh ? '人机共驾' : 'COPILOT SHELL'}</b></header>

        {activeView === 'handoff' && (
          <div className="coshHandoffScene">
            <div className="coshPromptLine"><b>❯</b><span>{zh ? '新建一个 Vite 项目，交互问题我来回答' : 'Create a Vite project; I will answer interactive prompts'}</span><i /></div>
            <article className="coshPlanCard">
              <header><span>{zh ? '副驾准备执行' : 'COPILOT IS READY'}</span><b>Bash · {zh ? '中风险' : 'MEDIUM RISK'}</b></header>
              <code>npm create vite@latest admin-demo</code>
              <footer><span>{zh ? '查看完整命令' : 'review full command'}</span><strong>{zh ? '允许一次 ↵' : 'allow once ↵'}</strong></footer>
            </article>
            <div className="coshTtyHandoff">
              <div><span>?</span><strong>Install with npm and start now?</strong><b>Yes / No</b></div>
              <p><i /> {zh ? '命令正在等待你的输入' : 'The command is waiting for your input'}</p>
            </div>
            <strong className="coshSceneClaim">{zh ? 'AI 接住意图，键盘仍在你手里' : 'AI takes the intent. You keep the keyboard.'}</strong>
          </div>
        )}

        {activeView === 'insight' && (
          <div className="coshInsightScene">
            <div className="coshCommandRun"><p><span>❯</span> cargo test -q</p><code>error[E0308]: mismatched types<br />&nbsp;--&gt; src/config.rs:9:20<br />&nbsp;expected <b>u64</b>, found <b>String</b></code></div>
            <article className="coshInsightCard">
              <header><i>!</i><strong>{zh ? '洞察：构建或测试失败' : 'Insight: build or test failed'}</strong></header>
              <p>{zh ? '分析这次构建或测试失败，定位首个可行动错误' : 'Analyze this failure and find the first actionable error'}</p>
              <footer><kbd>Tab</kbd><span>{zh ? '填入' : 'fill'}</span><kbd>Enter</kbd><span>{zh ? '提交' : 'submit'}</span><small>{zh ? '继续输入可忽略' : 'keep typing to ignore'}</small></footer>
            </article>
            <div className="coshDiagnosis"><span>src/config.rs:9</span><strong>{zh ? 'timeout_ms 需要 u64' : 'timeout_ms expects u64'}</strong><small>test result: ok · 1 passed</small></div>
            <strong className="coshSceneClaim">{zh ? '它会补一句，但不会抢走提示符' : 'It offers a next step without taking the prompt.'}</strong>
          </div>
        )}

        {activeView === 'health' && (
          <div className="coshHealthScene">
            <div className="coshLoginPrompt"><span>Last login 09:41 · tty1</span><strong>❯ <i /></strong></div>
            <article className="coshHealthCard">
              <header><span>{zh ? '登录健康概览' : 'LOGIN HEALTH'}</span><b>{zh ? '1 个风险项' : '1 RISK'}</b></header>
              <div className="coshVitals"><span><i style={{'--health-level': '28%'} as CSSProperties} />CPU <b>28%</b></span><span><i style={{'--health-level': '63%'} as CSSProperties} />MEM <b>63%</b></span><span><i style={{'--health-level': '41%'} as CSSProperties} />DISK <b>41%</b></span></div>
              <section><i>!</i><div><strong>{zh ? '近期发现 OOM 信号' : 'Recent OOM signal found'}</strong><small>cgroup memory limit · killed worker</small></div></section>
            </article>
            <div className="coshHealthSuggestion"><span>{zh ? '可以试试' : 'TRY THIS'}</span><p>{zh ? '帮我分析最近一次 OOM 的原因，重点看被杀进程、cgroup 和当时内存水位。' : 'Analyze the latest OOM, focusing on the killed process, cgroup, and memory level.'}</p><small><kbd>Tab</kbd> {zh ? '填入后按 Enter 提交' : 'then Enter to submit'}</small></div>
            <strong className="coshSceneClaim">{zh ? '一登录，就知道先查哪里' : 'Log in and know where to start.'}</strong>
          </div>
        )}
      </section>
    </div>
  );
}

function AgentSightVisual({locale}: {locale: Locale}) {
  const zh = locale === 'zh';
  const [activeView, setActiveView] = useState<'trace' | 'tokens' | 'agents' | 'issues'>('trace');
  const viewLabels = [
    ['trace', zh ? '调用轨迹' : 'Call trace', zh ? '看输入如何走到模型与工具' : 'Follow input into model and tool calls'],
    ['tokens', zh ? 'Token 构成' : 'Token X-ray', zh ? '找到推高上下文的内容' : 'Find what drives context growth'],
    ['agents', zh ? '子代理拓扑' : 'Subagents', zh ? '顺着高亮路径切换代理' : 'Follow the highlighted agent path'],
    ['issues', zh ? '异常定位' : 'Interruptions', zh ? '让重复、截断和失败显形' : 'Expose loops, truncation, and failure'],
  ] as const;

  return (
    <div className="capabilityCanvas sightCanvas">
      <nav className="capabilityFeaturePicker" aria-label={zh ? 'AgentSight 特性' : 'AgentSight features'}>
        {viewLabels.map(([id, label, description]) => (
          <button
            className={activeView === id ? 'is-active' : ''}
            type="button"
            aria-pressed={activeView === id}
            onClick={() => setActiveView(id)}
            key={id}>
            <span>{label}</span>
            <small>{description}</small>
          </button>
        ))}
      </nav>

      <section className={`sightStage sightStage--${activeView}`} key={activeView}>
        <header className="sightStageHeader">
          <span><i /> {zh ? '示例运行 run-84f1' : 'SAMPLE RUN run-84f1'}</span>
          <b>ATIF v1.7</b>
        </header>

        {activeView === 'trace' && (
          <div
            className="sightInvocationFlow"
            role="img"
            aria-label={zh ? '用户输入经 Agent 组织提示词后调用模型与工具' : 'User input becomes an Agent prompt, model call, and tool call'}>
            <article className="sightInvocationStep sightInvocationStep--user">
              <span>01</span>
              <i aria-hidden="true">⌨</i>
              <small>{zh ? '用户输入' : 'USER INPUT'}</small>
              <strong>{zh ? '修复 checkout 测试' : 'Fix the checkout tests'}</strong>
            </article>
            <article className="sightInvocationStep sightInvocationStep--agent">
              <span>02</span>
              <i aria-hidden="true">◎</i>
              <small>{zh ? 'Agent 提示词' : 'AGENT PROMPT'}</small>
              <strong>{zh ? '指令 + 对话 + 工具定义' : 'Instructions + history + tools'}</strong>
            </article>
            <article className="sightInvocationStep sightInvocationStep--llm">
              <span>03</span>
              <i aria-hidden="true">◉</i>
              <small>{zh ? 'LLM 调用' : 'LLM CALL'}</small>
              <strong>8.7K in · 412 out</strong>
            </article>
            <article className="sightInvocationStep sightInvocationStep--tool">
              <span>04</span>
              <i aria-hidden="true">⌕</i>
              <small>TOOL CALL</small>
              <strong>search_code(&quot;checkout&quot;)</strong>
              <em>{zh ? '返回 18 条匹配' : '18 matches returned'}</em>
            </article>
            <div className="sightInvocationRunner" aria-hidden="true" />
            <p>{zh ? '从一句输入到每次模型与工具调用，都能按轮展开查看' : 'Inspect every model and tool call from the user input that started the turn'}</p>
          </div>
        )}

        {activeView === 'tokens' && (
          <div className="sightTokenChart">
            <div className="sightTokenLegend"><span>STATIC</span><span>USER</span><span>ASSISTANT</span><span>TOOLS</span><span>INJECTED</span></div>
            <div className="sightTokenColumns">
              {[48, 58, 66, 73, 96, 84].map((height, index) => (
                <div className={`sightTokenColumn${index === 4 ? ' is-peak' : ''}`} style={{'--column-height': `${height}%`} as CSSProperties} key={height}>
                  <i /><i /><i /><i /><i />
                  <small>STEP {index + 1}</small>
                </div>
              ))}
            </div>
            <svg className="sightOutputCurve" viewBox="0 0 620 130" preserveAspectRatio="none" aria-hidden="true">
              <path d="M8 106 C72 100 110 89 154 92 S246 66 304 72 S405 30 482 34 S558 58 612 22" />
            </svg>
            <p><strong>{zh ? '工具输出推高了第 5 步上下文' : 'Tool output drove the step 5 context peak'}</strong><span>PEAK CONTEXT</span></p>
          </div>
        )}

        {activeView === 'agents' && (
          <div
            className="sightTopologyFlow"
            role="img"
            aria-label={zh ? 'Root Agent 派生 Research、Review 和 Test 子代理' : 'Root Agent spawning Research, Review, and Test subagents'}>
            <article className="sightInvocationStep sightTopologyNode sightTopologyNode--root">
              <span>ROOT</span>
              <i aria-hidden="true">◎</i>
              <small>ROOT AGENT</small>
              <strong>{zh ? '修复 checkout 测试' : 'Fix checkout tests'}</strong>
              <em>12 steps · 8.7K in</em>
            </article>
            <div className="sightTopologyLink"><span>{zh ? '派生' : 'SPAWNS'}</span></div>
            <article className="sightInvocationStep sightTopologyNode sightTopologyNode--research">
              <span>01</span>
              <i aria-hidden="true">⌕</i>
              <small>RESEARCH</small>
              <strong>{zh ? '定位失败路径' : 'Locate the failing path'}</strong>
              <em>4 steps · 4.1K in</em>
            </article>
            <div className="sightTopologyChildren">
              <article className="sightInvocationStep sightTopologyNode sightTopologyNode--review">
                <span>02</span>
                <i aria-hidden="true">◇</i>
                <small>REVIEW</small>
                <strong>{zh ? '检查补丁' : 'Review patch'}</strong>
              </article>
              <article className="sightInvocationStep sightTopologyNode sightTopologyNode--test is-active">
                <span>03</span>
                <i aria-hidden="true">✓</i>
                <small>TEST</small>
                <strong>{zh ? '当前查看的子代理' : 'Active subagent trace'}</strong>
              </article>
            </div>
            <div className="sightTopologyPath"><span>root</span><b>→</b><span>research</span><b>→</b><strong>test</strong></div>
            <p>{zh ? '父子关系和当前查看路径放在同一张拓扑里' : 'See parent-child relationships and the active trace path together'}</p>
          </div>
        )}

        {activeView === 'issues' && (
          <div
            className="sightInvocationFlow sightInterruptionFlow"
            role="img"
            aria-label={zh ? '重复的模型与工具调用被识别为异常循环' : 'Repeated model and tool calls identified as an interruption loop'}>
            <article className="sightInvocationStep sightInterruptionStep">
              <span>01</span>
              <i aria-hidden="true">◉</i>
              <small>{zh ? 'LLM 调用' : 'LLM CALL'}</small>
              <strong>{zh ? '继续查找 checkout' : 'Search checkout again'}</strong>
            </article>
            <article className="sightInvocationStep sightInterruptionStep">
              <span>02</span>
              <i aria-hidden="true">⌕</i>
              <small>TOOL CALL</small>
              <strong>search_code(&quot;checkout&quot;)</strong>
            </article>
            <article className="sightInvocationStep sightInterruptionStep">
              <span>03</span>
              <i aria-hidden="true">↻</i>
              <small>{zh ? '相同调用' : 'SAME CALL'}</small>
              <strong>{zh ? '参数和结果没有变化' : 'Same arguments and result'}</strong>
            </article>
            <article className="sightInvocationStep sightInterruptionStep sightInterruptionStep--alert">
              <span>04</span>
              <i aria-hidden="true">!</i>
              <small>{zh ? '异常定位' : 'INTERRUPTION'}</small>
              <strong>{zh ? '重复工具序列 ×4' : 'Repeated tool sequence ×4'}</strong>
              <em>DEAD LOOP · HIGH</em>
            </article>
            <div className="sightInterruptionLoop" aria-hidden="true">
              <span>🤖</span>
              <b>REPEAT ×4</b>
            </div>
            <p>{zh ? '重复调用、截断与失败都会落到具体事件，不再只剩一段报错' : 'Loops, truncation, and failures resolve to the exact event that caused them'}</p>
          </div>
        )}
      </section>
    </div>
  );
}

function CheckpointVisual({locale}: {locale: Locale}) {
  const zh = locale === 'zh';
  const [activeView, setActiveView] = useState<'rollback' | 'automatic' | 'preview' | 'branch'>('rollback');
  const viewLabels = [
    ['rollback', zh ? '回滚路线' : 'Rollback path', zh ? '整个工作区退回可用状态' : 'Restore the whole workspace'],
    ['automatic', zh ? '自动检查点' : 'Auto checkpoints', zh ? '会话开始和每轮结束自动留存' : 'Save session start and turn end'],
    ['preview', zh ? '先看再退' : 'Preview first', zh ? '确认前查看受影响文件' : 'Inspect affected files before restore'],
    ['branch', zh ? '分支历史' : 'Branch history', zh ? '旧尝试保留，新路线继续' : 'Keep old attempts and continue'],
  ] as const;

  return (
    <div className="capabilityCanvas checkpointCanvas">
      <nav className="capabilityFeaturePicker" aria-label={zh ? 'ws-ckpt 特性' : 'ws-ckpt features'}>
        {viewLabels.map(([id, label, description]) => (
          <button className={activeView === id ? 'is-active' : ''} type="button" aria-pressed={activeView === id} onClick={() => setActiveView(id)} key={id}>
            <span>{label}</span><small>{description}</small>
          </button>
        ))}
      </nav>

      <section className={`checkpointStage checkpointStage--${activeView}`} key={activeView}>
        <header className="featureStageHeader"><span><i /> ws-ckpt · ~/agent-workspace</span><b>CoW</b></header>

        {activeView === 'rollback' && (
          <div className="checkpointHistory">
            <svg viewBox="0 0 760 270" preserveAspectRatio="none" aria-hidden="true">
              <defs><marker id="rollback-arrow" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0,0 L8,4 L0,8 Z" /></marker></defs>
              <path className="checkpointMainPath" d="M72 142 C166 142 184 142 270 142 S400 80 492 80 S590 80 680 80" />
              <path className="checkpointRollbackPath" markerEnd="url(#rollback-arrow)" d="M680 62 C586 8 354 4 274 118" />
            </svg>
            <div className="checkpointHistoryNode checkpointHistoryNode--baseline"><span>✓</span><strong>Baseline</strong><small>{zh ? '会话开始' : 'session start'}</small></div>
            <div className="checkpointHistoryNode checkpointHistoryNode--turn1"><span>✓</span><strong>Turn 1</strong><small>{zh ? '可用状态' : 'known good'}</small></div>
            <div className="checkpointHistoryNode checkpointHistoryNode--turn2"><span>✓</span><strong>Turn 2</strong><small>{zh ? '重构会话' : 'refactor'}</small></div>
            <div className="checkpointHistoryNode checkpointHistoryNode--turn3"><span>!</span><strong>Turn 3</strong><small>{zh ? '测试回归' : 'regression'}</small><b>HEAD</b></div>
            <div className="checkpointRollbackLabel"><strong>Turn 3 → Turn 1</strong><span>{zh ? '恢复到可用检查点' : 'RESTORE A KNOWN-GOOD CHECKPOINT'}</span></div>
          </div>
        )}

        {activeView === 'automatic' && (
          <div className="checkpointAutoScene">
            <div className="checkpointAutoLine" aria-hidden="true" />
            {[
              ['Baseline', zh ? '会话开始' : 'session start'],
              ['Turn 1', zh ? '回合结束' : 'turn end'],
              ['Turn 2', zh ? '回合结束' : 'turn end'],
              ['Turn 3', zh ? '回合结束' : 'turn end'],
            ].map(([name, detail], index) => <div style={{'--checkpoint-delay': `${index * 180}ms`} as CSSProperties} key={name}><span>✓</span><strong>{name}</strong><small>{detail}</small></div>)}
            <p><strong>{zh ? '每轮结束，自动留一个恢复点' : 'Keep a recovery point after every turn'}</strong><span>{zh ? '需要显式开启 · 回合内仍可手动保存' : 'Opt-in · save important mid-turn states manually'}</span></p>
          </div>
        )}

        {activeView === 'preview' && (
          <div className="checkpointPreviewScene">
            <div className="checkpointWorkspaceState"><span>HEAD</span><strong>Turn 3</strong><small>{zh ? '当前工作区保持不变' : 'workspace remains unchanged'}</small></div>
            <div className="checkpointPreviewArrow"><span>◉</span><b>{zh ? '只计算差异' : 'CALCULATE DIFF ONLY'}</b></div>
            <article className="checkpointDiffCard">
              <header><strong>{zh ? '回滚到 Turn 1 的预览' : 'Preview restore to Turn 1'}</strong><b>0 {zh ? '项已执行' : 'APPLIED'}</b></header>
              <p><span>↶</span><code>src/checkout.rs</code><b>{zh ? '恢复' : 'restore'}</b></p>
              <p><span>↶</span><code>src/session.rs</code><b>{zh ? '恢复' : 'restore'}</b></p>
              <footer>{zh ? '确认前，Head 和文件都不会改变' : 'Head and files stay untouched until confirmation'}</footer>
            </article>
          </div>
        )}

        {activeView === 'branch' && (
          <div className="checkpointBranchScene">
            <svg viewBox="0 0 720 260" preserveAspectRatio="none" aria-hidden="true">
              <path d="M60 132 C155 132 192 132 278 132" />
              <path className="checkpointOldBranch" d="M278 132 C380 132 420 58 548 58 S626 58 674 58" />
              <path className="checkpointNewBranch" d="M278 132 C380 132 420 208 548 208 S626 208 674 208" />
            </svg>
            <div className="checkpointBranchNode checkpointBranchNode--base">Baseline</div>
            <div className="checkpointBranchNode checkpointBranchNode--turn1">Turn 1</div>
            <div className="checkpointBranchNode checkpointBranchNode--old">Turn 3 <small>{zh ? '旧尝试保留' : 'kept'}</small></div>
            <div className="checkpointBranchNode checkpointBranchNode--new">New Head <small>{zh ? '从 Turn 1 继续' : 'continue from Turn 1'}</small></div>
            <p>{zh ? '回退不会删除旧快照，新尝试会自然长成一条分支' : 'Rollback keeps old snapshots and lets a new branch grow'}</p>
          </div>
        )}
      </section>
    </div>
  );
}

function SkillFsVisual({locale}: {locale: Locale}) {
  const zh = locale === 'zh';
  const [activeView, setActiveView] = useState<'views' | 'discover' | 'transform' | 'activation'>('views');
  const viewLabels = [
    ['views', zh ? '视图隔离' : 'Focused views', zh ? '默认只展示当前工作集' : 'Show only the current working set'],
    ['discover', zh ? '按需发现' : 'Discover', zh ? '从次级视图打开更多能力' : 'Open more from secondary views'],
    ['transform', zh ? '读取时适配' : 'Read-time adapt', zh ? '把安装命令适配到当前系统' : 'Adapt install commands to this system'],
    ['activation', zh ? '可信版本' : 'Trusted version', zh ? '落实外部安全提供方的决定' : 'Apply an external security decision'],
  ] as const;

  return (
    <div className="capabilityCanvas skillFsCanvas">
      <nav className="capabilityFeaturePicker" aria-label={zh ? 'SkillFS 特性' : 'SkillFS features'}>
        {viewLabels.map(([id, label, description]) => (
          <button className={activeView === id ? 'is-active' : ''} type="button" aria-pressed={activeView === id} onClick={() => setActiveView(id)} key={id}>
            <span>{label}</span><small>{description}</small>
          </button>
        ))}
      </nav>

      <section className={`skillStage skillStage--${activeView}`} key={activeView}>
        <header className="featureStageHeader"><span><i /> SkillFS · /skills</span><b>FUSE</b></header>

        {activeView === 'views' && (
          <div className="skillUniverse">
            <header><span>{zh ? '同一座技能仓库' : 'ONE SKILL REPOSITORY'}</span><b>16 {zh ? '个已安装' : 'INSTALLED'}</b></header>
            <div className="skillCloud" aria-hidden="true">
              {['browser', 'research', 'slides', 'github', 'terminal', 'deploy', 'security', 'docs', 'pdf', 'data', 'review', 'ops'].map((skill) => <span key={skill}>{skill}</span>)}
            </div>
            <div className="skillViewLens">
              <header><span>AGENT /skills</span><b>{zh ? '当前视图' : 'CURRENT VIEW'}</b></header>
              <div className="skillVisibilityMetric"><strong>4</strong><span>/ 16<br />{zh ? '当前可见' : 'VISIBLE NOW'}</span></div>
              <p>{zh ? '只把当前任务需要的 Skills 放到眼前' : 'Put only task-relevant Skills in view'}</p>
              <div className="skillVisibleSet"><span>github</span><span>terminal</span><span>deploy</span><span>review</span></div>
            </div>
          </div>
        )}

        {activeView === 'discover' && (
          <div className="skillDiscoverScene">
            <div className="skillCurrentShelf"><header><span>/skills</span><b>4 {zh ? '个常用 Skills' : 'PRIMARY SKILLS'}</b></header><p>github</p><p>terminal</p><p>deploy</p><p>review</p></div>
            <aside className="skillDiscoverDrawer skillDiscoverDrawer--large">
              <header><span>⌕</span><strong>skill-discover</strong></header>
              <p><b>research</b><span>4 skills</span></p><p><b>media</b><span>3 skills</span></p><p><b>operations</b><span>5 skills</span></p>
              <small>{zh ? '次级视图仍然可发现和读取' : 'Secondary views remain discoverable and readable'}</small>
            </aside>
            <article className="skillDiscoverPreview"><span>research</span><strong>browser/SKILL.md</strong><small>{zh ? '打开说明，不会把它加入默认视图' : 'Open the instructions without adding it to the default view'}</small></article>
          </div>
        )}

        {activeView === 'transform' && (
          <div className="skillTransformScene">
            <article><span>{zh ? 'Skill 原始说明' : 'ORIGINAL SKILL'}</span><strong>deploy/SKILL.md</strong><code>sudo <mark>apt-get</mark> install -y <mark>libssl-dev</mark></code><small>{zh ? '源文件保持不变' : 'source stays unchanged'}</small></article>
            <div className="skillReadLens"><span>↻</span><strong>OS Adapter</strong><small>{zh ? '目标 Alinux / Anolis' : 'target Alinux / Anolis'}</small></div>
            <article className="is-result"><span>{zh ? 'Agent 在当前系统读到' : 'AGENT READS ON THIS SYSTEM'}</span><strong>deploy/SKILL.md</strong><code>sudo <mark>dnf</mark> install -y <mark>openssl-devel</mark></code><small>{zh ? '包管理器和软件包名一起适配' : 'package manager and package name adapted'}</small></article>
            <p>{zh ? '读取时只改写 Agent 看到的 SKILL.md，源文件保持不变。' : 'Only the SKILL.md view is adapted at read time. The source file stays unchanged.'}</p>
          </div>
        )}

        {activeView === 'activation' && (
          <div className="skillActivationScene">
            <header><span>{zh ? '外部安全提供方给出决定' : 'EXTERNAL SECURITY PROVIDER DECIDES'}</span><b>{zh ? 'SkillFS 只负责落实' : 'SKILLFS APPLIES IT'}</b></header>
            <div className="skillActivationChoices"><span className="is-current"><i>●</i><b>current</b><small>{zh ? '读取当前版本' : 'read live version'}</small></span><span className="is-fallback"><i>◐</i><b>fallback</b><small>{zh ? '读取可信快照' : 'read trusted snapshot'}</small></span><span className="is-hidden"><i>○</i><b>hidden</b><small>{zh ? '暂不出现在视图' : 'remove from view'}</small></span></div>
            <div className="skillActivationResult"><span>github/SKILL.md</span><strong>fallback</strong><small>{zh ? '当前文件发生漂移，使用可信快照' : 'Current files drifted; trusted snapshot selected'}</small></div>
            <p>{zh ? '扫描、签名和风险判断由安全层完成，SkillFS 不冒充安全扫描器' : 'Scanning, signing, and risk decisions remain in the security layer'}</p>
          </div>
        )}
      </section>
    </div>
  );
}

function SecurityVisual({locale}: {locale: Locale}) {
  const zh = locale === 'zh';
  const [activeView, setActiveView] = useState<'sandbox' | 'ledger' | 'scanner' | 'hardening'>('sandbox');
  const [policy, setPolicy] = useState<'read' | 'write' | 'deny'>('write');
  const viewLabels = [
    ['sandbox', 'Linux Sandbox', zh ? '给命令刚好够用的系统权限' : 'Give commands just enough OS access'],
    ['ledger', 'Skill Ledger', zh ? '让每次 Skill 变更都有迹可循' : 'Keep every Skill change traceable'],
    ['scanner', 'Code Scanner', zh ? '在执行前发现可疑代码' : 'Catch suspicious code before execution'],
    ['hardening', zh ? '系统加固' : 'System Hardening', zh ? '扫描并预览主机基线修复' : 'Scan and preview baseline remediation'],
  ] as const;
  const policies = {
    read: {
      command: 'git status',
      label: zh ? '只读 · 无网络' : 'READ-ONLY · NO NETWORK',
      detail: zh ? '整个文件系统只读' : 'The filesystem stays read-only',
    },
    write: {
      command: 'npm install package-x',
      label: zh ? '2 个可写目录' : '2 WRITABLE ROOTS',
      detail: zh ? '只开放 Workspace 与 /tmp' : 'Only Workspace and /tmp are writable',
    },
    deny: {
      command: 'rm -rf /',
      label: zh ? '执行前拒绝' : 'REJECTED BEFORE EXECUTION',
      detail: zh ? '破坏性命令不会进入沙箱' : 'The destructive command never enters the sandbox',
    },
  } as const;
  const activePolicy = policies[policy];

  return (
    <div className="capabilityCanvas securityCanvas">
      <nav className="capabilityFeaturePicker" aria-label={zh ? 'Agent Sec Core 组件' : 'Agent Sec Core components'}>
        {viewLabels.map(([id, label, description]) => (
          <button className={activeView === id ? 'is-active' : ''} type="button" aria-pressed={activeView === id} onClick={() => setActiveView(id)} key={id}>
            <span>{label}</span><small>{description}</small>
          </button>
        ))}
      </nav>

      <section className={`securityStage securityStage--${activeView}`} key={activeView}>
        <header className="featureStageHeader"><span><i /> Agent Sec Core · Linux</span><b>{activeView === 'sandbox' ? 'RUNTIME' : activeView === 'ledger' ? 'INTEGRITY' : activeView === 'scanner' ? 'PRE-EXEC' : 'HOST'}</b></header>

        {activeView === 'sandbox' && (
          <div className="securitySandboxScene">
            <nav className="securityCommandPicker" aria-label={zh ? '命令权限示例' : 'Command policy examples'}>
              {(Object.keys(policies) as Array<keyof typeof policies>).map((id) => (
                <button type="button" className={policy === id ? 'is-active' : ''} aria-pressed={policy === id} onClick={() => setPolicy(id)} key={id}>
                  <code>{policies[id].command}</code>
                  <span>{id === 'read' ? (zh ? '只读任务' : 'Read task') : id === 'write' ? (zh ? '构建任务' : 'Build task') : (zh ? '破坏性命令' : 'Destructive')}</span>
                </button>
              ))}
            </nav>
            <div className={`permissionField permissionField--${policy}`} key={policy}>
              <div className="permissionOuterAsset permissionOuterAsset--etc"><span>🔒</span><b>/etc</b></div>
              <div className="permissionOuterAsset permissionOuterAsset--ssh"><span>🔒</span><b>~/.ssh</b></div>
              <div className="permissionOuterAsset permissionOuterAsset--network"><span>◎</span><b>Network</b></div>
              <div className="permissionMembrane">
                <header><span>⬡</span><strong>linux-sandbox</strong><b>{policy === 'read' ? 'read-only' : policy === 'write' ? 'workspace-write' : 'deny'}</b></header>
                <code>&gt; {activePolicy.command}</code>
                <div className="permissionWorkspace">
                  <span className="permissionArea permissionArea--workspace"><b>Workspace</b><small>{policy === 'write' ? (zh ? '可写' : 'WRITE') : (zh ? '只读' : 'READ')}</small></span>
                  <span className="permissionArea permissionArea--tmp"><b>/tmp</b><small>{policy === 'write' ? (zh ? '可写' : 'WRITE') : (zh ? '只读' : 'READ')}</small></span>
                  <span className="permissionArea permissionArea--git"><b>.git</b><small>🔒 {zh ? '只读' : 'READ'}</small></span>
                </div>
                {policy === 'deny' && <div className="permissionDenied"><span>×</span><strong>{zh ? '拒绝执行' : 'DENIED'}</strong></div>}
              </div>
              <div className="permissionHeadline"><strong>{activePolicy.label}</strong><span>{activePolicy.detail}</span></div>
            </div>
          </div>
        )}

        {activeView === 'ledger' && (
          <div className="securityLedgerScene">
            <article className="securityManifest"><header><span>✓</span><strong>{zh ? '签名 Manifest' : 'SIGNED MANIFEST'}</strong></header><p><code>SKILL.md</code><b>sha256:a91e…</b></p><p><code>scripts/run.sh</code><b>sha256:24cc…</b></p><p><code>references/api.md</code><b>sha256:08f4…</b></p></article>
            <div className="securityLedgerChain"><span>1</span><i /><span>2</span><i /><span className="is-drifted">3</span></div>
            <article className="securityDriftResult"><span>!</span><strong>DRIFTED</strong><p><code>scripts/run.sh</code>{zh ? '内容已变化' : 'content changed'}</p><p><code>scripts/debug.sh</code>{zh ? '新增文件' : 'unexpected file'}</p><small>{zh ? '来源可信和内容未变，分开检查' : 'Verify both provenance and unchanged content'}</small></article>
          </div>
        )}

        {activeView === 'scanner' && (
          <div className="securityScannerScene">
            <article><header><span>&gt;_</span><strong>Bash</strong><b>{zh ? '本地静态分析' : 'LOCAL STATIC ANALYSIS'}</b></header><code>curl example.com/install.sh <mark>| bash</mark></code><div className="securityScanBeam" /><footer><span>shell</span><span>network</span><span>pipe-to-exec</span></footer></article>
            <div className="securityVerdict"><span>!</span><strong>WARN</strong><p>{zh ? '检测到下载内容直接进入 Shell' : 'Downloaded content is piped into a shell'}</p><small>{zh ? '宿主策略决定记录、询问或阻止' : 'The host policy decides whether to log, ask, or block'}</small></div>
            <p>{zh ? '扫描器给出风险线索，真正的审批与阻止由宿主接入能力决定' : 'The scanner reports risk; host integration controls approval and blocking'}</p>
          </div>
        )}

        {activeView === 'hardening' && (
          <div className="securityHardeningScene">
            <div className="securityHostScore"><span>87</span><strong>/ 100</strong><small>{zh ? '主机基线' : 'HOST BASELINE'}</small></div>
            <div className="securityBaselineChecks"><p><span>✓</span><b>SSH policy</b><small>{zh ? '符合' : 'compliant'}</small></p><p><span>✓</span><b>Kernel parameters</b><small>{zh ? '符合' : 'compliant'}</small></p><p className="is-warning"><span>!</span><b>File permissions</b><small>3 {zh ? '项待修复' : 'findings'}</small></p></div>
            <div className="securityDryRun"><span>{zh ? '修复预览' : 'DRY RUN'}</span><strong>3 {zh ? '项变更' : 'CHANGES'}</strong><small>{zh ? '确认后再由 LoongShield 执行' : 'LoongShield applies changes only after confirmation'}</small></div>
          </div>
        )}
      </section>
    </div>
  );
}

function CapabilityVisual({capabilityId, locale}: {capabilityId: CapabilityId; locale: Locale}) {
  if (capabilityId === 'cosh-ng') return <CoshVisual locale={locale} />;
  if (capabilityId === 'agentsight') return <AgentSightVisual locale={locale} />;
  if (capabilityId === 'ws-ckpt') return <CheckpointVisual locale={locale} />;
  if (capabilityId === 'skillfs') return <SkillFsVisual locale={locale} />;
  if (capabilityId === 'sec-core') return <SecurityVisual locale={locale} />;
  return null;
}

function TokenlessShowcase({locale}: {locale: Locale}) {
  const t = featureUiContent[locale];
  const sectionRef = useRef<HTMLElement>(null);
  const transitionTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [activeCapabilityId, setActiveCapabilityId] = useState<CapabilityId>('tokenless');
  const [activeViewIndex, setActiveViewIndex] = useState(0);
  const [animationRun, setAnimationRun] = useState(0);
  const [isTransitioning, setIsTransitioning] = useState(false);
  const capability = capabilityById[activeCapabilityId];
  const demo = capability.views[activeViewIndex];
  const isTokenless = activeCapabilityId === 'tokenless';

  const replay = () => setAnimationRun((run) => run + 1);
  const transitionTo = (capabilityId: CapabilityId, viewIndex = 0) => {
    if (transitionTimer.current) clearTimeout(transitionTimer.current);
    setIsTransitioning(true);
    const delay = window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : 180;
    transitionTimer.current = setTimeout(() => {
      setActiveCapabilityId(capabilityId);
      setActiveViewIndex(viewIndex);
      setAnimationRun((run) => run + 1);
      requestAnimationFrame(() => requestAnimationFrame(() => setIsTransitioning(false)));
    }, delay);
  };

  const handleTabKeyDown = (event: KeyboardEvent<HTMLButtonElement>) => {
    if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
    event.preventDefault();
    const direction = event.key === 'ArrowRight' ? 1 : -1;
    const nextIndex =
      (activeViewIndex + direction + capability.views.length) % capability.views.length;
    const buttons = event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>(
      '.tokenDemoTab',
    );
    buttons?.[nextIndex]?.focus();
    transitionTo(activeCapabilityId, nextIndex);
  };

  useEffect(() => {
    const section = sectionRef.current;
    if (!section) return;
    const observer = new IntersectionObserver(
      ([entry]) => {
        if (!entry.isIntersecting) return;
        replay();
        observer.disconnect();
      },
      {threshold: 0.32},
    );
    observer.observe(section);
    return () => {
      observer.disconnect();
      if (transitionTimer.current) clearTimeout(transitionTimer.current);
    };
  }, []);

  return (
    <section
      className={`tokenFeatureSection tokenFeatureSection--${capability.accent} homeSnapPoint`}
      ref={sectionRef}>
      <div className="tokenFeatureInner siteContainer">
        <header
          className={`tokenFeatureHeading${
            isTransitioning ? ' tokenFeatureHeading--leaving' : ''
          }`}
          key={`${activeCapabilityId}-heading`}>
          <div>
            <p className="tokenFeatureEyebrow">{capability.eyebrow[locale]}</p>
            <h2>{capability.title[locale]}</h2>
            <p className="tokenFeatureIntro">{capability.intro[locale]}</p>
          </div>
          <p className="tokenFeatureBenchmark">
            <span>{capability.note[locale]}</span>
            {capability.noteHref && capability.noteLink && (
              <a href={capability.noteHref} target="_blank" rel="noreferrer">
                {capability.noteLink[locale]} ↗
              </a>
            )}
          </p>
        </header>

        <div className="tokenFeatureLayout">
          <div className="tokenWorkbench">
            <header className="tokenWorkbenchHeader">
              {isTokenless ? (
                <div className="tokenDemoTabs" role="tablist" aria-label={t.demoTabs}>
                  {capability.views.map((item, index) => {
                    const active = index === activeViewIndex;
                    return (
                      <button
                        className={`tokenDemoTab${active ? ' tokenDemoTab--active' : ''}`}
                        type="button"
                        role="tab"
                        aria-selected={active}
                        tabIndex={active ? 0 : -1}
                        key={item.id}
                        onClick={() => transitionTo(activeCapabilityId, index)}
                        onKeyDown={handleTabKeyDown}>
                        <span aria-hidden="true">{item.icon}</span>
                        {item.name[locale]}
                      </button>
                    );
                  })}
                </div>
              ) : (
                <div className="capabilityModeLabel">
                  <span aria-hidden="true">{demo.icon}</span>
                  <strong>{demo.name[locale]}</strong>
                  <small>{capability.processor}</small>
                </div>
              )}
              <div className="tokenMetric" aria-live="polite">
                <span>{demo.metricLabel[locale]}</span>
                <strong>{demo.metricValue}</strong>
              </div>
            </header>

            {isTokenless ? (
              <div
                className={`tokenPipeline${isTransitioning ? ' tokenPipeline--leaving' : ''}`}
                key={`${activeCapabilityId}-${demo.id}-${animationRun}`}>
                <article className="tokenCodeCard">
                  <header>
                    <span>{demo.sourceLabel[locale]}</span>
                    <b aria-hidden="true">{demo.icon}</b>
                  </header>
                  <code>
                    {demo.raw.map((line, index) => (
                      <span
                        className={`tokenCodeLine tokenCodeLine--${line.tone}`}
                        style={{animationDelay: `${index * 45}ms`}}
                        key={`${line.text}-${index}`}>
                        {line.text}
                      </span>
                    ))}
                  </code>
                </article>

                <div className="tokenProcessor" aria-hidden="true">
                  <span>{capability.processor}</span>
                  <small>{demo.action[locale]}</small>
                </div>

                <article className="tokenCodeCard tokenCodeCard--optimized">
                  <header>
                    <span>{demo.resultLabel[locale]}</span>
                    <b>✓ {demo.resultStatus[locale]}</b>
                  </header>
                  <code>
                    {demo.optimized.map((line, index) => (
                      <span
                        className={`tokenCodeLine tokenCodeLine--${line.tone}`}
                        style={{animationDelay: `${260 + index * 55}ms`}}
                        key={`${line.text}-${index}`}>
                        {line.text}
                      </span>
                    ))}
                  </code>
                </article>
              </div>
            ) : (
              <div
                className={`capabilityVisualShell${
                  isTransitioning ? ' capabilityVisualShell--leaving' : ''
                }`}
                key={`${activeCapabilityId}-${animationRun}`}>
                <CapabilityVisual capabilityId={activeCapabilityId} locale={locale} />
              </div>
            )}

            <footer className="tokenWorkbenchFooter">
              <p>{demo.copy[locale]}</p>
              <div aria-label={t.activeStages}>
                {demo.stages.map((stage) => (
                  <span key={stage}>{stage}</span>
                ))}
              </div>
            </footer>
          </div>

          <aside className="tokenFeatureRail" aria-label={t.railLabel}>
            {scenarios[locale].map((scenario) => (
              <section
                className={`tokenRailGroup tokenRailGroup--${scenario.accent}`}
                key={scenario.name}>
                <header>
                  <span aria-hidden="true">{scenarioRailIcons[scenario.accent]}</span>
                  <strong>{scenario.role}</strong>
                </header>
                <div>
                  {scenario.components.map((component) => {
                    const capabilityId = componentCapabilityIds[component.label];
                    const active = capabilityId === activeCapabilityId;
                    return (
                      <button
                        className={active ? 'tokenRailItem--active' : undefined}
                        type="button"
                        aria-pressed={active}
                        onClick={() => transitionTo(capabilityId)}
                        key={component.label}>
                        <span className="tokenRailItemIcon" aria-hidden="true">
                          {componentRailIcons[component.label]}
                        </span>
                        <strong>{component.label}</strong>
                        <small>{component.description}</small>
                        <b aria-hidden="true">→</b>
                      </button>
                    );
                  })}
                </div>
              </section>
            ))}
          </aside>
        </div>
      </div>
    </section>
  );
}

export default function Home() {
  const {i18n} = useDocusaurusContext();
  const locale: Locale = i18n.currentLocale === 'zh' ? 'zh' : 'en';
  const t = content[locale];
  const scenarioItems = scenarios[locale];
  const routeItems = routes[locale];
  const lockupLight = useBaseUrl('/img/brand/anolisa-lockup-light.svg');
  const lockupDark = useBaseUrl('/img/brand/anolisa-lockup-dark.svg');

  return (
    <Layout title={t.lead} description={`${t.hook} ${t.systemScope}`}>
      <Head>
        <meta property="og:type" content="website" />
      </Head>
      <main className="homePage">
        <section className="heroSection homeSnapPoint">
          <div className="heroGrid siteContainer">
            <div className="heroCopy">
              <div className="releaseBadge">
                <span aria-hidden="true" />
                {t.badge}
              </div>
              <h1 className="heroWordmark">
                <ThemedImage
                  alt="ANOLISA"
                  sources={{light: lockupLight, dark: lockupDark}}
                />
              </h1>
              <div className="heroPositioning">
                <p>{t.lead}</p>
                <p>{t.hook}</p>
                <p>{t.systemScope}</p>
              </div>
              <p className="heroStatement">{t.statement}</p>

              <div className="buttonRow">
                <SiteLink
                  locale={locale}
                  to="/docs/user-guide/token-saving/tokenless/quickstart"
                  className="primaryButton">
                  {t.startTokenless} →
                </SiteLink>
                <SiteLink locale={locale} to="/docs/quickstart" className="secondaryButton">
                  {t.exploreAnolisa}
                </SiteLink>
              </div>

              <div className="heroCommand">
                <p>{t.installLabel}</p>
                <CopyCommand command={installCommand} label={t.copy} copiedLabel={t.copied} />
              </div>

              <div className="agentCommand">
                <p><span aria-hidden="true">&gt;</span> {t.agentLabel}</p>
                <CopyCommand command={t.agentPrompt} label={t.copy} copiedLabel={t.copied} />
              </div>
            </div>

            <aside className="systemSurface" aria-label={t.surfaceLabel}>
              <header>
                <span>anolisa://system-surface</span>
                <span className="surfaceStatus">{t.surfaceStatus}</span>
              </header>
              <div className="surfaceCore">
                <span className="surfaceCoreLabel">AGENT WORKLOAD</span>
                <strong>ANOLISA</strong>
                <span>OBSERVE · CONTROL · RECOVER</span>
              </div>
              {scenarioItems.map((scenario) => (
                <SiteLink
                  locale={locale}
                  to={scenario.href}
                  className={`surfaceLayer surfaceLayer--${scenario.accent}`}
                  key={scenario.name}>
                  <div className="surfaceLayerMeta">
                    <span>{scenario.role}</span>
                    {'proof' in scenario && <small>{scenario.proof}</small>}
                  </div>
                  <strong>{scenario.surfaceTitle}</strong>
                  <em>{scenario.surfaceBody}</em>
                  <b aria-hidden="true">→</b>
                </SiteLink>
              ))}
              <footer>
                <span>{t.surfaceFooter}</span>
                <span>source: main</span>
              </footer>
            </aside>
          </div>
        </section>

        <TokenlessShowcase locale={locale} />

        <section className="scenarioSection homeSnapPoint" id="scenarios">
          <div className="siteContainer">
            <div className="scenarioHeading">
              <div>
                <h2>{t.scenariosTitle}</h2>
                <p>{t.scenariosIntro}</p>
              </div>
              <SiteLink locale={locale} to="/docs/quickstart" className="textLink">
                {t.openGuide} →
              </SiteLink>
            </div>

            <div className="scenarioGrid">
              {scenarioItems.map((scenario) => (
                <article
                  className={`scenarioCard scenarioCard--${scenario.accent}`}
                  key={scenario.name}>
                  <header>
                    <span>{scenario.role}</span>
                    <b aria-hidden="true">↗</b>
                  </header>
                  <p className="scenarioName">{scenario.name}</p>
                  <h3>
                    <SiteLink locale={locale} to={scenario.href} className="scenarioTitleLink">
                      {scenario.title}
                    </SiteLink>
                  </h3>
                  <p className="scenarioBody">{scenario.body}</p>
                  <p className="scenarioPromise">{scenario.promise}</p>
                  <SiteLink locale={locale} to={scenario.href} className="scenarioCta">
                    {scenario.cta} →
                  </SiteLink>
                  <div className="componentTags">
                    {scenario.components.map((component) => (
                      <SiteLink locale={locale} to={component.href} key={component.label}>
                        {component.label}
                      </SiteLink>
                    ))}
                  </div>
                </article>
              ))}
            </div>

            <div className="routeSection">
              <div className="routeIntro">
                <h2>{t.exploreTitle}</h2>
                <p>{t.exploreIntro}</p>
              </div>
              <div className="routeGrid">
                {routeItems.map((route) => (
                  <SiteLink locale={locale} to={route.href} className="routeCard" key={route.label}>
                    <span>{route.label}</span>
                    <strong>{route.title}</strong>
                    {'body' in route && <p>{route.body}</p>}
                    <b aria-hidden="true">→</b>
                  </SiteLink>
                ))}
              </div>
            </div>
          </div>
        </section>
      </main>
    </Layout>
  );
}

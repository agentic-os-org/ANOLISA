use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::HealthBannerTitle => "健康检查",
        MessageId::HealthStartupLabel => "健康",
        MessageId::HealthBannerTryLabel => "可输入",
        MessageId::HealthBannerFindingLabel => "发现",
        MessageId::HealthBannerEvidenceLabel => "证据",
        MessageId::HealthBannerMoreFindingsLabel => "另有 {count} 个问题",
        MessageId::HealthBannerUnavailableLabel => "不可用",
        MessageId::HealthBannerFindingsSection => "重点关注",
        MessageId::HealthBannerSuggestedPromptSection => "建议下一步",
        MessageId::HealthBannerSuggestedPromptIntro => "以下提示词可直接输入给 Agent：",
        MessageId::HealthSeverityOk => "正常",
        MessageId::HealthSeverityWarning => "警告",
        MessageId::HealthSeverityCritical => "严重",
        MessageId::HealthSeverityDegraded => "降级",
        MessageId::HealthSeverityUnavailable => "不可用",
        MessageId::HealthMetricCpu => "CPU",
        MessageId::HealthMetricCpuLoadPerCore => "1分钟负载",
        MessageId::HealthMetricLoad1mShort => "1分钟",
        MessageId::HealthMetricLoad5mShort => "5分钟",
        MessageId::HealthMetricLoadValue => "{load} / {cores}核（{ratio}倍）",
        MessageId::HealthMetricCpuPerCoreUnit => "倍",
        MessageId::HealthMetricCpuUsed => "CPU 已用",
        MessageId::HealthMetricHost => "主机",
        MessageId::HealthMetricUsed => "已用",
        MessageId::HealthMetricMemory => "内存",
        MessageId::HealthMetricMemoryAvailable => "内存可用",
        MessageId::HealthMetricMemoryUsed => "内存已用",
        MessageId::HealthMetricSwap => "Swap",
        MessageId::HealthMetricSwapUsed => "Swap 已用",
        MessageId::HealthMetricDisk => "磁盘",
        MessageId::HealthMetricDiskUsed => "磁盘已用",
        MessageId::HealthMetricDiskMountUsed => "磁盘 {mount} 已用",
        MessageId::HealthMetricPressure => "负载",
        MessageId::HealthMetricLevels => "资源",
        MessageId::HealthMetricSignal => "信号",
        MessageId::HealthMetricService => "服务",
        MessageId::HealthEvidenceDiskAvailable => "可用 {gib} GiB",
        MessageId::HealthEvidenceMount => "挂载点 {mount}",
        MessageId::HealthEvidenceOomKilledProcess => "杀掉 {process}",
        MessageId::HealthEvidenceOomCgroup => "cgroup {cgroup}",
        MessageId::HealthEvidenceOomOneHourCount => "1h OOM {count}",
        MessageId::HealthEvidenceOomTwentyFourHourCount => "24h OOM {count}",
        MessageId::HealthEvidenceOomVictimKilledAgo => "{age}前杀掉 {subject}",
        MessageId::HealthEvidenceOomVictimKilled => "杀掉 {subject}",
        MessageId::HealthEvidenceOomAge => "OOM 发生于 {age}前",
        MessageId::HealthOomScopeMemcg => "cgroup 内存限制触发",
        MessageId::HealthOomScopeHost => "整机内存不足触发",
        MessageId::HealthOomScopeCpuset => "cpuset/NUMA 范围内存不足",
        MessageId::HealthOomScopeMemoryPolicy => "内存策略范围不足",
        MessageId::HealthOomScopeUnknown => "OOM 触发范围未识别",
        MessageId::HealthTryAnalyzeMemoryPressure => "分析内存压力",
        MessageId::HealthTryCheckSwapPressure => "检查换页压力",
        MessageId::HealthTryCheckRecentOom => "分析最近一次 OOM 原因",
        MessageId::HealthTryInspectDiskUsage => "检查磁盘占用",
        MessageId::HealthTryInspectServiceStatus => "检查 {service} 状态",
        MessageId::HealthTryInspectHighLoad => "分析高负载",
        MessageId::HealthTryInspectProcessMemory => "检查 {process} 内存",
        MessageId::HealthTryReviewUnavailableChecks => "查看缺失检查",
        MessageId::HealthPromptAnalyzeMemoryPressure => {
            "请核实当前内存压力、主要占用及适用的 cgroup 限制，给出处理建议。"
        }
        MessageId::HealthPromptCheckSwapPressure => "请核实是否存在换页压力，检查内存余量及相关进程。",
        MessageId::HealthPromptCheckRecentOom => {
            "请核对近期 OOM 的发生时间、作用域和受影响进程，再排查相关内存限制。"
        }
        MessageId::HealthPromptInspectDiskUsage => "请核实当前磁盘占用与主要来源，给出处理建议。",
        MessageId::HealthPromptInspectServiceStatus => {
            "请先核实服务 {service} 的当前状态，再排查可能原因并给出处理建议。"
        }
        MessageId::HealthPromptInspectHighLoad => "请核实当前负载，区分 CPU 繁忙与 I/O 等待等可能来源。",
        MessageId::HealthPromptInspectProcessMemory => {
            "请核对近期 OOM 中 {process} 的受影响情况、事件时间及相关内存限制。"
        }
        MessageId::HealthPromptReviewUnavailableChecks => {
            "说明启动健康检查为什么不可用，以及如何恢复这些检查。"
        }
        MessageId::HealthInsightMemoryAvailableLow => {
            "可用内存偏低"
        }
        MessageId::HealthInsightDiskHigh => "挂载点磁盘使用率偏高",
        MessageId::HealthInsightRecentOom => {
            "近期 OOM 事件"
        }
        MessageId::HealthInsightCpuLoadHigh => "最近多个窗口负载偏高",
        MessageId::HealthInsightSwapPressure => {
            "Swap 使用偏高"
        }
        MessageId::HealthInsightServiceState => {
            "服务单元 {service} 当前 {observed}，预期 {expected}"
        }
        MessageId::HealthInsightGeneric => "启动健康检查发现了值得排查的信号",
        MessageId::HealthUnavailablePermissionDenied => "权限不足",
        MessageId::HealthUnavailableCommandMissing => "命令缺失",
        MessageId::HealthUnavailableTimeout => "检查超时",
        MessageId::HealthUnavailableUnsupported => "平台不支持",
        MessageId::HealthUnavailableParseError => "解析失败",
        MessageId::HealthFindingPlatformUnsupported => "平台不支持",
        MessageId::HealthFindingCoreCollectorUnavailable => "核心检查不可用",
        MessageId::HealthFindingCpuLoadHigh => "系统负载偏高",
        MessageId::HealthFindingMemoryAvailableLow => "可用内存偏低",
        MessageId::HealthFindingSwapPressure => "换页压力有上下文",
        MessageId::HealthFindingDiskHigh => "磁盘水位偏高",
        MessageId::HealthFindingRecentOom => "近期 OOM 信号",
        MessageId::HealthFindingKernelPanic => "近期内核 panic",
        MessageId::HealthFindingServiceFailed => "服务失败",
        MessageId::HealthFindingServiceInactive => "服务未运行",
        MessageId::HealthCollectorProvider => "Provider",
        MessageId::HealthCollectorConfig => "配置",
        MessageId::HealthCollectorHooks => "Hooks",
        MessageId::HealthCollectorPty => "PTY",
        MessageId::HealthCollectorPermissions => "权限",
        MessageId::DoctorTitle => "cosh-shell 体检",
        MessageId::DoctorStatusLabel => "状态",
        MessageId::DoctorChecksLabel => "检查项",
        MessageId::DoctorRemediationLabel => "补救",
        MessageId::DoctorAllHealthy => "全部检查通过",
        MessageId::HealthFindingProviderUnconfigured => "AI provider 未就绪",
        MessageId::HealthFindingConfigUnavailable => "配置不可用",
        MessageId::HealthFindingHooksUntrusted => "项目 hooks 未信任",
        MessageId::HealthFindingPtyUnavailable => "PTY 支持不可用",
        MessageId::HealthFindingPermissionsUnwritable => "配置目录不可写",
        MessageId::HealthRemediationProvider => {
            "为 adapter '{adapter}' 配置凭据（环境变量或 config.toml），或运行 /auth"
        }
        MessageId::HealthRemediationUnknownAdapter => {
            "'{adapter}' 不是受支持的 adapter；请将 adapter_default 设为以下之一：fake, claude-code, qwen-cli, cosh-core"
        }
        MessageId::HealthRemediationConfig => {
            "设置 HOME，以便 cosh-shell 解析 ~/.copilot-shell 并加载配置"
        }
        MessageId::HealthRemediationConfigUnreadable => {
            "请将 ~/.copilot-shell/config.toml 设为可读文件（而非目录）并修正其权限"
        }
        MessageId::HealthRemediationConfigInvalid => {
            "修复 ~/.copilot-shell/config.toml，使 cosh-shell 能加载（合法 TOML 或可识别的 key=value 条目）"
        }
        MessageId::HealthRemediationHooks => "检查并信任 {path} 下的项目 hooks 后它们才会运行",
        MessageId::HealthRemediationPty => {
            "请在真实终端中运行 cosh-shell；交互式 shell 需要 PTY（/dev/ptmx）"
        }
        MessageId::HealthRemediationPermissions => {
            "修正 {path} 的权限，使 cosh-shell 能写入配置、日志与状态"
        }
        MessageId::HealthTryReasonMemoryLow => "可用内存偏低",
        MessageId::HealthTryReasonSwapWithContext => "swap 偏高且有压力上下文",
        MessageId::HealthTryReasonRecentOom => "近期 OOM 值得回溯原因",
        MessageId::HealthTryReasonDiskHigh => "磁盘空间紧张",
        MessageId::HealthTryReasonServiceState => "配置服务状态异常",
        MessageId::HealthTryReasonHighLoad => "最近负载持续偏高",
        MessageId::HealthTryReasonMissingCoreCheck => "核心健康检查缺失",
        _ => return None,
    })
}

use super::MessageId;

pub(super) fn message(id: MessageId) -> Option<&'static str> {
    Some(match id {
        MessageId::ConfigInvalidLanguageBody => "无效语言: {language}",
        MessageId::ConfigSupportedLanguagesFooter => "支持: auto, en-US, zh-CN。",
        MessageId::ConfigUnknownKeyBody => "未知配置项: {key}",
        MessageId::ConfigHomeMissingBody => "HOME 未设置，无法持久化配置。",
        MessageId::ConfigHomeMissingFooter => "设置 HOME 或手动编辑配置。",
        MessageId::ConfigUnchangedTitle => "配置未变更",
        MessageId::ConfigNoFileChangedBody => "未修改配置文件。",
        MessageId::ConfigSavedTitle => "配置已保存",
        MessageId::ConfigSavedValueLine => "已保存 ui.{setting} = \"{value}\"。",
        MessageId::ConfigCurrentSessionLanguageLine => "当前会话语言: {language}。",
        MessageId::ConfigSavedFooter => "保存的设置立即生效；Agent 回复跟随你的提问语言。",
        MessageId::ConfigSaveFailedTitle => "配置保存失败",
        MessageId::ConfigSaveFailedBody => "配置保存失败: {error}",
        MessageId::ConfigSavePromptTitle => "保存配置？",
        MessageId::ConfigFileLine => "文件: {path}",
        MessageId::ConfigPendingChangeLine => "ui.{setting}: {before} -> {after}",
        MessageId::ConfigSaveButton => "保存",
        MessageId::ConfigCancelButton => "取消",
        MessageId::ConfigApplyKeysFooter => "按键: Left/Right 选择 | Enter 应用 | Esc 取消",
        MessageId::ConfigLanguageTitle => "语言",
        MessageId::ConfigLanguageAutoLine => "auto    跟随 LC_ALL/LC_MESSAGES/LANG",
        MessageId::ConfigLanguageEnLine => "en-US   英语",
        MessageId::ConfigLanguageZhLine => "zh-CN   简体中文",
        MessageId::ConfigLanguageKeysFooter => "按键: Left/Right 选择 | Enter 确认 | Esc 取消",
        MessageId::CaptureInputRejectedTitle => "输入未投递",
        MessageId::CaptureInputRejectedBody => {
            "卡片操作处理期间键入的内容无法安全投递，请重新输入。"
        }
        MessageId::GuideUsage => concat!(
            "Cosh 配置与入门\n",
            "\n",
            "用法:\n",
            "  cosh guide                 在正常终端打开配置向导\n",
            "  cosh guide --plain         打印入门与配置入口，不交互\n",
            "  cosh guide --help          显示本帮助\n",
            "  cosh guide on              开启使用引导提示\n",
            "  cosh guide off             关闭使用引导提示\n",
            "  cosh guide status          查看使用引导提示\n",
        ),
        MessageId::GuidePreferenceSaved => "AI 使用提示：{state}（已保存）。",
        MessageId::GuidePreferenceDefault => "AI 使用提示：开启（默认设置）。",
        MessageId::GuideSaved => "AI 使用提示已保存为{state}。",
        MessageId::GuideDisabledSaved => "AI 使用提示已关闭并保存。",
        MessageId::GuideStateOn => "开启",
        MessageId::GuideStateOff => "关闭",
        MessageId::GuideAvailableConditional => "自动引导仅在支持的 Shell 中显示。",
        MessageId::GuideUnconfirmed => "当前会话的自动引导状态尚未确认。",
        MessageId::GuideRuntimeUnconfirmed => "当前引导状态尚未确认：{error}。",
        MessageId::GuideClosed => "当前自动引导已关闭。",
        MessageId::GuideIndependentHint => {
            "AI 使用提示包括首屏建议、失败求助和登录推广；仍可手动运行 cosh。关闭登录资源摘要：cosh health banner off。"
        }
        MessageId::GuideReadFailed => "无法读取引导设置：{error}。请检查后重试。",
        MessageId::GuideSaveFailed => "引导设置持久保存失败：{error}。请重试原命令。",
        MessageId::GuideRuntimeSaveFailed => {
            "当前运行态已设为{state}，但持久保存失败：{error}。请重试原命令。"
        }
        MessageId::GuideRuntimePublishFailed => {
            "运行态更新失败，持久设置未保存：{error}。请重试原命令。"
        }
        MessageId::GuideRuntimePublishPartial => {
            "当前关闭标记已生效，但运行态发布失败，持久设置未保存：{error}。请重试原命令。"
        }
        MessageId::GuideRuntimeUnavailable => "没有已验证的本地运行目录",
        MessageId::GuideErrorCorrupt => "已保存的状态已损坏",
        MessageId::GuideErrorVersion => "不支持的状态版本",
        MessageId::GuideErrorBusy => "引导状态正被其他进程使用",
        MessageId::GuideErrorUnsafe => "状态路径的私有性或属主不符合要求",
        MessageId::GuideErrorTooLarge => "引导状态超过大小或数量限制",
        MessageId::GuideErrorIo => "存储读写失败（{error}）",
        MessageId::LoginUser => "用户：",
        MessageId::LoginCurrentShell => "当前登录 Shell：",
        MessageId::LoginOriginalBash => "原 Bash：",
        MessageId::LoginTransaction => "事务状态：",
        MessageId::LoginManaged => "受管状态：",
        MessageId::LoginNoManagedRecord => "无受管记录",
        MessageId::LoginManagedYes => "是",
        MessageId::LoginManagedNo => "否",
        MessageId::LoginTransactionUnmanaged => "未受管",
        MessageId::LoginTransactionPrepared => "已准备",
        MessageId::LoginTransactionApplied => "已应用",
        MessageId::LoginTransactionRestored => "已恢复",
        MessageId::LoginTransactionConflict => "冲突",
        MessageId::LoginNewLoginsOnly => "仅影响新登录，已有会话保持不变。",
        MessageId::LoginChangePreview => "用户：{user}（UID {uid}）：{before} -> {after}。仅影响新登录。",
        MessageId::LoginConfirm => "是否继续？[y/N] ",
        MessageId::LoginShellChanged => "登录 Shell：{shell}。仅影响新登录。",
        MessageId::LoginChangeComplete => "登录 Shell 操作已完成。仅影响新登录。",
        MessageId::LoginAuthorizationRequired => "授权不可用或未成功，请联系管理员执行：{command}",
        MessageId::LoginCommandFailed => "cosh login：操作未完成：{error}",
        MessageId::LoginUsage => "用法：cosh login status|set|restore [--user USER] [--yes]",
        MessageId::LoginIdentityMismatch => "提权账号身份与已确认的 UID 不一致。",
        MessageId::LoginStateConflict => "账号 Shell 或登录记录存在冲突，请联系管理员核查。",
        MessageId::LoginRecoveryMissing => "没有可信的登录恢复记录，未修改账号。",
        MessageId::GuideWizardTitle => "Cosh 配置与入门",
        MessageId::GuideWizardStart => concat!(
            "### 在 Bash 或 zsh 提问\n",
            "\n",
            "在终端运行：\n",
            "\n",
            "`cosh --ask '磁盘满了怎么查？'`\n",
            "\n",
            "### 已在 Cosh 中提问\n",
            "\n",
            "直接输入「磁盘满了怎么查？」并按 Enter。支持免配置接入的 ECS 可直接提问，首次使用时验证接入。其他环境如需配置，Cosh 会给出提示。\n",
            "\n",
            "### 在 Cosh 中继续用\n",
            "\n",
            "在 Bash 或 zsh 中输入 `cosh` 进入会话。输入 `df -h` 查看磁盘用量，或输入「帮我解释磁盘用量」向 AI 提问。用 `/help` 查看会话内帮助。\n",
            "\n",
            "### 返回原会话\n",
            "\n",
            "回答后会留在 Cosh。从 Bash 或 zsh 启动时，输入 `exit` 返回原会话。直接以 Cosh 登录时，`exit` 会结束登录会话。可在首页的「默认登录 Shell」修改后续登录的 Shell。\n",
        ),
        MessageId::GuideWizardEntryLanguage => "语言",
        MessageId::GuideWizardEntryAi => "AI 服务",
        MessageId::GuideWizardEntryTips => "更多设置",
        MessageId::GuideWizardEntryHealth => "登录健康摘要",
        MessageId::GuideWizardEntryLogin => "默认登录 Shell",
        MessageId::GuideWizardEntryStart => "快速使用指南",
        MessageId::GuideWizardStartSummary => "提问与常用命令",
        MessageId::GuideWizardLanguageTitle => "语言 / Language",
        MessageId::GuideWizardGuideTipLabel => "使用引导提示",
        MessageId::GuideWizardLanguageBody => "界面语言。会话内也可运行 `/config language`。",
        MessageId::GuideWizardAiBody => "在原终端提问，首次使用时验证接入。",
        MessageId::GuideWizardTipsBody => concat!(
            "命令分析方式与个性化推荐数据。\n",
            "\n",
            "- `/mode analysis` 修改当前会话的命令分析方式。\n",
            "- `/recommendations` 管理个性化提问推荐与本地数据。\n",
        ),
        MessageId::GuideWizardHealthBody => concat!(
            "登录健康摘要，包含健康与资源状态。\n",
            "\n",
            "- `cosh health banner off` 关闭登录健康摘要，不影响 AI 使用提示。\n",
            "- `cosh health banner on` 开启；`cosh health banner status` 查看。\n",
        ),
        MessageId::GuideWizardLoginBody => concat!(
            "默认登录 Shell，仅支持登录切换的系统可用。\n",
            "\n",
            "- `cosh login status` 查看登录 Shell。\n",
            "- `cosh login set` 新登录进入 Cosh（已有会话保持不变）。\n",
            "- `cosh login restore` 恢复记录的原 Bash，下次登录生效。\n",
            "- `cosh login set --user alice` 管理其他用户（仅管理员）。\n",
            "- `restore` 和 `status` 同样支持 `--user alice`。\n",
        ),
        MessageId::GuideWizardPlainFooter => {
            "在正常终端运行 `cosh guide` 可交互完成以上配置。"
        }
        MessageId::GuideWizardHomeHint => "Enter 打开  Esc/q 退出指南\n↑↓ 选择",
        MessageId::GuideWizardChoiceHint => "Enter 保存  Esc 返回\nF2/h 首页  ↑↓ 选择  q 退出",
        MessageId::GuideWizardOpenHint => "Enter 打开  Esc 返回\nF2/h 首页  ↑↓ 选择  q 退出",
        MessageId::GuideWizardConfirmHint => "Enter {action}  Esc 返回\nF2/h 首页  ↑↓/PgDn  q 退出",
        MessageId::GuideWizardScrollHint => "Esc 返回  q 退出\nF2/h 首页  ↑↓/PgUp/PgDn 滚动",
        MessageId::GuideWizardAskHint => "Enter 回终端去提问  Esc 返回\nF2/h 首页  ↑↓ 滚动  q 退出",
        MessageId::GuideWizardAskReceipt => "在 Bash 或 zsh 中运行：cosh --ask '磁盘满了怎么查？'\n已在 Cosh 中？直接输入：磁盘满了怎么查？",
        MessageId::GuideWizardReviewHint => "向下滚动查看后可确认",
        MessageId::GuideWizardConfirmSet => "设为 Cosh",
        MessageId::GuideWizardConfirmRestore => "恢复 Shell",
        MessageId::GuideWizardConfirmReset => "重置数据",
        MessageId::GuideWizardConfirmClear => "清理数据",
        MessageId::GuideWizardConfirmDisable => "关闭并清理",
        MessageId::GuideWizardConfirmImport => "允许导入",
        MessageId::GuideWizardCancelKey => "取消",
        MessageId::GuideWizardSavedLine => "已保存：{value}",
        MessageId::GuideWizardSavedNextLogin => "，下次登录生效。",
        MessageId::GuideWizardSaveFailed => "保存失败：{error}",
        MessageId::GuideWizardReadFailed => "无法读取",
        MessageId::GuideWizardLanguageAuto => "跟随系统",
        MessageId::GuideWizardLanguageEn => "English",
        MessageId::GuideWizardLanguageZh => "简体中文",
        MessageId::GuideWizardLanguageEffectiveLine => "当前界面：{value}",
        MessageId::GuideWizardLanguagePrefLine => "已保存偏好：{value}",
        MessageId::GuideWizardSavedMarker => "已保存",
        MessageId::GuideWizardLanguageSaved => "界面语言已切换为 {effective}。",
        MessageId::GuideWizardLanguageFollowed => "已保存为跟随系统，当前界面为 {effective}。",
        MessageId::GuideWizardLanguageOverride => "已保存 {saved}。当前界面仍使用 {effective}，由 COSH_SHELL_LANG 指定。",
        MessageId::GuideWizardLanguageSourceLine => "来源：{value}",
        MessageId::GuideWizardSourceConfig => "用户配置",
        MessageId::GuideWizardSourceEnv => "环境变量",
        MessageId::GuideWizardSourceDefault => "默认",
        MessageId::GuideWizardLanguageEnvNote => {
            "COSH_SHELL_LANG 指定了当前界面语言，覆盖已保存的偏好。"
        }
        MessageId::GuideWizardLanguageScopeNote => {
            "此设置只改变界面语言。提问时可自行选择语言。"
        }
        MessageId::GuideWizardGuideTipBody => {
            "显示 Cosh 使用提示，不是 AI 总开关。关闭后仍可手动运行 cosh。"
        }
        MessageId::GuideWizardHealthDetailBody => {
            "登录时显示系统健康和资源用量摘要。"
        }
        MessageId::GuideWizardHealthSaved => "登录健康摘要已设为{value}，下次登录生效。",
        MessageId::GuideWizardHealthReadFailed => "无法读取登录健康摘要设置。",
        MessageId::GuideWizardHealthSaveFailed => "无法保存登录健康摘要设置：{error}",
        MessageId::GuideWizardAiReadonly => {
            "在原终端提问，首次使用时验证接入。"
        }
        MessageId::GuideWizardLoginReadonly => {
            "在终端运行 cosh login set 或 cosh login restore 修改。"
        }
        MessageId::GuideWizardLoginFailed => "无法读取登录 Shell：{error}",
        MessageId::GuideWizardTooSmall => "终端过小，无法显示向导。",
        MessageId::GuideWizardTooSmallHint => {
            "放大终端可继续，Esc 或 q 退出。文本版：cosh guide --plain。"
        }
        MessageId::GuideWizardCurrent => "当前",
        MessageId::GuideWizardAiLoading => "读取中…",
        MessageId::GuideWizardAiAsk => "回终端去提问",
        MessageId::GuideWizardAiConfigured => "已配置 AI 接入",
        MessageId::GuideWizardAiConfiguredHome => "已配置 · 连接未验证",
        MessageId::GuideWizardAiPendingHome => "免配置自动接入，可直接提问",
        MessageId::GuideWizardAiUnverified => "连接未验证",
        MessageId::GuideWizardAiIncomplete => "配置不完整",
        MessageId::GuideWizardAiNoFallback => "不会自动切换试用接入。",
        MessageId::GuideWizardAiUnknown => "暂无法读取 AI 接入状态",
        MessageId::GuideWizardAiDisabled => "ECS 自动接入已关闭",
        MessageId::GuideWizardAiDisabledNext => "可在管理中重新开启 ECS 自动接入，或使用手工配置的服务。",
        MessageId::GuideWizardAiEcs => "ECS 免配置接入 · 首次提问时验证",
        MessageId::GuideWizardAiPending => "免配置自动接入，可直接提问",
        MessageId::GuideWizardAiIdentityFailed => "暂无法确认 ECS 接入，可重试提问",
        MessageId::GuideWizardAiSourceRuntime => "运行时配置",
        MessageId::GuideWizardAiUnconfigured => "需要配置 AI 服务",
        MessageId::GuideWizardAiServiceLine => "当前 AI 服务：{value}",
        MessageId::GuideWizardAiSourceLine => "配置来源：{value}",
        MessageId::GuideWizardAiSourceUser => "用户配置",
        MessageId::GuideWizardAiSourceSystem => "系统配置",
        MessageId::GuideWizardAiAutoUseLine => "零配置自动使用：{value}",
        MessageId::GuideWizardAiAutoUseOn => "开启",
        MessageId::GuideWizardAiAutoUseOff => "关闭",
        MessageId::GuideWizardAiManage => "管理 AI 服务",
        MessageId::GuideWizardAiConfigure => "配置 AI 服务",
        MessageId::GuideWizardAiCheck => "检查配置",
        MessageId::GuideWizardTipsAnalysis => "命令分析方式",
        MessageId::GuideWizardTipsPersonal => "个性化提问推荐",
        MessageId::GuideWizardTipsData => "推荐数据与隐私",
        MessageId::GuideWizardAnalysisSmart => "先给建议",
        MessageId::GuideWizardAnalysisAuto => "自动分析",
        MessageId::GuideWizardAnalysisManual => "仅手动分析",
        MessageId::GuideWizardEffectiveLine => "实际：{value}",
        MessageId::GuideWizardSourceLine => "来源：{value}",
        MessageId::GuideWizardSourceUser => "用户偏好",
        MessageId::GuideWizardNewSessionNote => "保存后新会话生效。",
        MessageId::GuideWizardOverriddenNote => "已保存，但被环境变量覆盖。",
        MessageId::GuideWizardForcedOffNote => {
            "COSH_RECOMMENDATIONS_ENABLED=0 强制关闭，开启请求不会被保存。"
        }
        MessageId::GuideWizardManualPausedNote => "manual 会暂停新会话的推荐，但保留开启偏好。",
        MessageId::GuideWizardDataImportLine => "Bash 历史导入：{value}",
        MessageId::GuideWizardDataAllowImport => "允许导入 Bash 历史",
        MessageId::GuideWizardDataStopImport => "停止后续导入",
        MessageId::GuideWizardDataSaveOnPref => "保存开启偏好",
        MessageId::GuideWizardDataSaveOffPref => "保存关闭偏好",
        MessageId::GuideWizardDataEnvNote => "实际状态仍受环境变量控制。",
        MessageId::GuideWizardDataStopKeepsNote => "停止只影响后续导入，已有数据仍保留。",
        MessageId::GuideWizardDataClear => "清理本地推荐数据",
        MessageId::GuideWizardDataReset => "重置推荐数据并关闭推荐",
        MessageId::GuideWizardPersonalEnable => "开启个性化推荐",
        MessageId::GuideWizardPersonalDisable => "关闭推荐并清理本地数据",
        MessageId::GuideWizardPersonalDisabled => "已保存关闭偏好并清理本地推荐数据。已有会话仍按已加载的设置运行。",
        MessageId::GuideWizardDataImportOverride => "当前由 COSH_RECOMMENDATIONS_BASH_HISTORY 指定为{effective}。",
        MessageId::GuideWizardAnalysisOverride => "已保存 {saved}。COSH_SHELL_ANALYSIS_MODE 仍指定使用 {effective}。",
        MessageId::GuideWizardAnalysisChoiceHelp => "先给建议：对有价值的失败或诊断输出先展示洞察。\n自动分析：部分高置信故障可能自动发起 AI 分析，其他情况先提示。\n仅手动分析：暂停主动建议、自动分析和个性化推荐，仍可手动提问或分析。\n用 /mode analysis 可调整当前会话。",
        MessageId::GuideWizardDataDamaged => "无法读取推荐数据。",
        MessageId::GuideWizardDataPrivacy => {
            "## 隐私\n\n- 默认不导入 Bash 历史；开启需单独同意。首次同步只读取历史末尾建立基线。\n- 活动保留 7 天、近期画像 14 天、长期模式与证据快照 90 天，并受容量限制。\n- 少量近期活动可能发送给当前 AI 服务，脱敏尽力而为；清理本地不代表清理服务方记录。"
        }
        MessageId::GuideWizardDataCleared => "已清理本地推荐数据，保留开关设置。",
        MessageId::GuideWizardDataResetDone => "已重置损坏数据并关闭推荐。",
        MessageId::GuideWizardImportConsent => "允许导入 Bash 历史？少量近期活动可能发送给当前 AI 服务，脱敏不能保证覆盖全部敏感内容。首次同步只建立读取基线，本次保存不导入历史。",
        MessageId::GuideWizardImportScope => "导入偏好供新 Cosh 会话使用；已有会话未同步，已有数据保留。",
        MessageId::GuideWizardPreferenceDefault => "偏好：{value}（默认）",
        MessageId::GuideWizardPreferenceSaved => "偏好：{value}（已保存）",
        MessageId::GuideWizardConfirm => "确认",
        MessageId::GuideWizardCancel => "取消",
        MessageId::GuideWizardLoginSet => "设为 Cosh 登录 Shell",
        MessageId::GuideWizardLoginRestore => "恢复原登录 Shell",
        MessageId::GuideWizardLoginOther => "查看其他用户管理命令",
        MessageId::GuideWizardLoginVerified => "已将默认登录 Shell 恢复为 {value}。",
        MessageId::GuideWizardLoginSetDone => "已将默认登录 Shell 设为 Cosh。",
        MessageId::GuideWizardLoginSetTitle => "将 Cosh 设为默认登录 Shell",
        MessageId::GuideWizardLoginTargetLine => "改为：{target}",
        MessageId::GuideWizardLoginRestoreAction => "恢复为 {target}",
        MessageId::GuideWizardLoginUnconfirmed => "变更结果未确认。可读取当前状态或返回，本次向导中请勿重复提交。",
        MessageId::GuideWizardLoginSudoNote => "需要管理员授权。",
        MessageId::GuideWizardConfigUnreadable => "无法读取配置文件。请手动编辑后重新打开向导。",
        MessageId::GuideWizardDataClearConfirm => "确认清除本地推荐数据？开关偏好将保留。",
        MessageId::GuideWizardLoginOtherGuidance => "其它用户管理命令（需管理员权限）：",
        MessageId::GuideWizardLoginReread => "重新读取登录状态",
        MessageId::GuideWizardLoginRereadDone => "已重新读取登录状态。",
        MessageId::GuideWizardDetailHint => "Enter 应用  Esc 返回\nF2/h 首页  ↑↓/PgDn  q 退出",
        MessageId::GuideWizardResultSaved => "已保存：{actions}。",
        MessageId::GuideWizardResultPartial => "部分完成：{actions}。",
        MessageId::GuideWizardResultUnconfirmed => "结果未确认：{actions}。请先核对当前状态；本次指南内相关写入仍暂停。",
        MessageId::GuideWizardResultNone => "未更改配置。",
        MessageId::GuideWizardResultCancelledNone => "已取消，未更改配置。",
        MessageId::GuideWizardResultCancelled => "已取消本次操作，已保存的更改保留。",
        MessageId::GuideWizardResultFailed => "未完成：{error}",
        MessageId::GuideWizardResultInterrupted => "操作被中断。",
        MessageId::GuideWizardResultScope => "首次提问时会尝试连接。",
        MessageId::GuideWizardActionConfigure => "新增或编辑 AI 服务",
        MessageId::GuideWizardActionActivate => "切换 AI 服务",
        MessageId::GuideWizardActionDelete => "删除 AI 服务",
        MessageId::GuideWizardActionTrial => "免配置接入设置",
        MessageId::GuideWizardActionLogin => "登录 Shell",
        MessageId::GuideWizardAiExit => "完成，回 AI 服务",
        MessageId::GuideWizardAiSummarySaved => "已保存 AI 服务设置。",
        MessageId::GuideWizardAiSummaryPartial => "AI 服务操作部分完成。",
        MessageId::GuideWizardAiSummaryUnconfirmed => "AI 服务操作结果未确认。",
        MessageId::GuideWizardAiSummaryFailed => "AI 服务操作未完成。",
        MessageId::GuideWizardAiAddService => "添加 AI 服务",
        MessageId::GuideWizardAiUnavailable => "无法打开 AI 服务管理：{error}",
        MessageId::GuideWizardAiCoreUntrusted => {
            "cosh-core 未通过身份校验，已拒绝打开 AI 服务管理。请使用受信任的 cosh-core。"
        }
        MessageId::GuideWizardAiCoreCapability => {
            "当前 cosh-core 不支持安全配置写入，已拒绝修改。请升级 cosh-core 后重试。"
        }
        MessageId::GuideWizardInputLost => "终端输入已结束，向导退出。",
        MessageId::GuideWizardPersonalOffConfirm => {
            "关闭个性化推荐，并清理本地推荐派生活动、画像与缓存？"
        }
        MessageId::GuideWizardPersonalOffScope => {
            "已有 Cosh 会话未同步，重新打开后按关闭偏好运行。保留源 Bash 历史、认证配置与服务端记录。"
        }
        MessageId::GuideWizardDataClearScope => {
            "清理本地推荐派生活动、画像与缓存，保留推荐与导入开关。不删除源 Bash 历史，不修改认证配置，不清理服务端记录。"
        }
        MessageId::GuideWizardDataResetScope => {
            "隔离损坏的推荐载荷，重建推荐状态并关闭推荐。旧载荷可能作为隔离副本保留，不等于彻底擦除。源 Bash 历史、认证配置与服务端记录不变。"
        }
        MessageId::GuideWizardPersonalOffUnconfirmed => {
            "关闭偏好已保存；本地推荐数据的清理结果尚未确认。"
        }
        MessageId::GuideWizardLoginOtherNote => {
            "USER 是占位符，请替换为目标用户名；退出向导后在终端自行执行，向导不会代为执行。"
        }
        MessageId::GuideWizardLoginCancelled => "已取消管理员授权，未修改登录 Shell。",
        MessageId::GuideWizardLoginCommitPending => {
            "登录 Shell 变更可能已提交，结果未确认。可读取当前状态或返回，本次向导中暂停继续变更登录 Shell。"
        }
        MessageId::GuideWizardPartialSessionRecord => {
            "模型配置已保存，会话记录未保存。"
        }
        MessageId::GuideWizardTipsSummary => "分析方式与推荐数据",
        MessageId::GuideWizardCleanupUnconfirmed => "本地推荐数据清理结果尚未确认。",
        MessageId::GuideWizardLoginSummarySet => "Cosh · 已设为默认",
        MessageId::GuideWizardLoginSummaryBenefit => "当前 {shell}",
        MessageId::GuideWizardLoginSummaryPending => "变更结果未确认",
        MessageId::GuideWizardLoginSummaryConflict => "登录 Shell 与记录冲突",
        MessageId::GuideWizardLoginSummaryRecoveryMissing => "Cosh · 缺少可信恢复记录",
        MessageId::GuideWizardLoginBenefit => {
            "让每次登录直接进入 Cosh。照常执行命令，需要帮助时向 AI 提问。"
        }
        MessageId::GuideWizardLoginSudoConfirmNote => {
            "确认后在当前终端按 sudo 提示验证权限。"
        }
        MessageId::GuideWizardAiHowTo => "查看提问方式",
        MessageId::GuideWizardSudoHandoffTitle => "默认登录 Shell 变更",
        MessageId::GuideWizardSudoHandoffTarget => "目标账号：{user} · UID {uid}",
        MessageId::GuideWizardSudoHandoffSet => "将默认登录 Shell 设为 {target}",
        MessageId::GuideWizardSudoHandoffRestore => "将默认登录 Shell 恢复为 {target}",
        MessageId::GuideWizardSudoHandoffPrompt => "如 sudo 要求密码，请在当前终端输入。",
        MessageId::GuideWizardSudoHandoffEnter => "输入后按 Enter 继续。",
        MessageId::GuideWizardSudoHandoffEcho => "输入通常不显示字符或星号。",
        MessageId::GuideWizardSudoHandoffCancel => "Ctrl-C 请求取消。",
        MessageId::GuideWizardSudoHandoffNoPassword => "无需密码时直接继续。",
        MessageId::GuideWizardAiWriteBlocked => {
            "上一次{action}的结果尚未确认，已暂停该类写入。可查看当前状态或结束管理后核查。"
        }
        MessageId::GuideWizardAiWritesHeld => "上一次变更结果未确认，相关写入已暂停。",
        _ => return None,
    })
}

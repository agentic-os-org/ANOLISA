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
            "## 进入 AI Shell\n",
            "\n",
            "- 在原终端运行 `cosh`，照常执行 `df -h` 等命令，也可直接提问。\n",
            "- `cosh --ask '你的问题'` 带着首问进入交互会话。\n",
            "- 会话中用 `/agent` 打开提问编辑器；首次需要 AI 时沿用现有认证流程。\n",
            "- `/help` 查看会话命令，`cosh --help` 查看启动参数。\n",
            "- `exit` 退出；从 Bash 启动时回到原 Bash。\n",
            "- 作为登录 Shell 时，`exit` 可能结束本次登录。\n",
        ),
        MessageId::GuideWizardEntryLanguage => "语言",
        MessageId::GuideWizardEntryAi => "AI 服务",
        MessageId::GuideWizardEntryTips => "提示与推荐",
        MessageId::GuideWizardEntryHealth => "登录健康摘要",
        MessageId::GuideWizardEntryLogin => "默认登录 Shell",
        MessageId::GuideWizardEntryStart => "使用入门",
        MessageId::GuideWizardStartSummary => "命令、提问与退出",
        MessageId::GuideWizardLanguageTitle => "语言 / Language",
        MessageId::GuideWizardGuideTipLabel => "使用引导提示",
        MessageId::GuideWizardLanguageBody => "界面语言。会话内也可运行 `/config language`。",
        MessageId::GuideWizardAiBody => "配置与管理 AI 服务。会话内运行 `/auth`。",
        MessageId::GuideWizardTipsBody => concat!(
            "使用引导提示、命令分析方式与个性化提问推荐。\n",
            "\n",
            "- `cosh guide off` 关闭使用引导提示，不影响手动使用 Cosh。\n",
            "- `cosh guide on` 开启；`cosh guide status` 查看。\n",
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
        MessageId::GuideWizardHomeHint => "↑↓ 选择  Enter 打开  Esc 退出",
        MessageId::GuideWizardChoiceHint => "↑↓ 选择  Enter 保存  Esc 返回",
        MessageId::GuideWizardScrollHint => "↑↓ 滚动  PgUp/PgDn 翻页  Esc 返回",
        MessageId::GuideWizardSavedLine => "已保存：{value}",
        MessageId::GuideWizardSavedNextLogin => "已保存，下次登录生效。",
        MessageId::GuideWizardSaveFailed => "保存失败：{error}",
        MessageId::GuideWizardReadFailed => "无法读取",
        MessageId::GuideWizardLanguageAuto => "跟随系统",
        MessageId::GuideWizardLanguageEn => "English",
        MessageId::GuideWizardLanguageZh => "简体中文",
        MessageId::GuideWizardLanguageEffectiveLine => "实际语言：{value}",
        MessageId::GuideWizardLanguageSourceLine => "来源：{value}",
        MessageId::GuideWizardSourceConfig => "用户配置",
        MessageId::GuideWizardSourceEnv => "环境变量",
        MessageId::GuideWizardSourceDefault => "默认",
        MessageId::GuideWizardLanguageEnvNote => {
            "COSH_SHELL_LANG 覆盖保存值；修改保存值只影响新会话。"
        }
        MessageId::GuideWizardLanguageScopeNote => {
            "只改变界面语言，AI 回复语言跟随你的提问。"
        }
        MessageId::GuideWizardGuideTipBody => {
            "显示 Cosh 使用提示，不是 AI 总开关。关闭后仍可手动运行 cosh。"
        }
        MessageId::GuideWizardHealthDetailBody => {
            "登录时显示健康与资源状态，影响后续登录。"
        }
        MessageId::GuideWizardAiReadonly => {
            "在 Cosh 会话中运行 /auth 配置与管理 AI 服务。配置完成不等于已连接。"
        }
        MessageId::GuideWizardLoginReadonly => {
            "在终端运行 cosh login set 或 cosh login restore 修改。"
        }
        MessageId::GuideWizardLoginFailed => "无法读取登录 Shell：{error}",
        MessageId::GuideWizardTooSmall => "终端过小，无法显示向导。",
        MessageId::GuideWizardTooSmallHint => {
            "放大终端后重试，或运行 cosh guide --plain。Esc 退出。"
        }
        MessageId::GuideWizardCurrent => "当前",
        MessageId::GuideWizardAiLoading => "读取中…",
        MessageId::GuideWizardAiUnconfigured => "未配置",
        MessageId::GuideWizardAiServiceLine => "当前 AI 服务：{value}",
        MessageId::GuideWizardAiSourceLine => "配置来源：{value}",
        MessageId::GuideWizardAiSourceUser => "用户配置",
        MessageId::GuideWizardAiSourceSystem => "系统配置",
        MessageId::GuideWizardAiAutoUseLine => "零配置自动使用：{value}",
        MessageId::GuideWizardAiAutoUseOn => "开启",
        MessageId::GuideWizardAiAutoUseOff => "关闭",
        MessageId::GuideWizardAiManage => "配置 AI 服务（打开 /auth）",
        MessageId::GuideWizardAiHandoff => {
            "正在打开 Cosh 会话进行认证，完成后输入 exit 返回向导。"
        }
        MessageId::GuideWizardAiHandoffFailed => "无法打开 Cosh 会话：{error}",
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
        MessageId::GuideWizardDataAllowImport => "允许导入并保存",
        MessageId::GuideWizardDataStopImport => "停止导入并保存",
        MessageId::GuideWizardDataSaveOnPref => "保存开启偏好",
        MessageId::GuideWizardDataSaveOffPref => "保存关闭偏好",
        MessageId::GuideWizardDataEnvNote => "实际状态仍受环境变量控制。",
        MessageId::GuideWizardDataStopKeepsNote => "停止只影响后续导入，已有数据仍保留。",
        MessageId::GuideWizardDataClear => "仅清理数据",
        MessageId::GuideWizardDataReset => "重置数据并关闭推荐",
        MessageId::GuideWizardDataDamaged => "无法读取推荐数据。",
        MessageId::GuideWizardDataPrivacy => {
            "## 隐私\n\n- 默认不导入 Bash 历史；开启需单独同意。首次同步只读取历史末尾建立基线。\n- 活动保留 7 天、近期画像 14 天、长期模式与证据快照 90 天，并受容量限制。\n- 少量近期活动可能发送给当前 AI 服务，脱敏尽力而为；清理本地不代表清理服务方记录。"
        }
        MessageId::GuideWizardDataCleared => "已清理本地推荐数据，保留开关设置。",
        MessageId::GuideWizardDataResetDone => "已重置损坏数据并关闭推荐。",
        MessageId::GuideWizardConfirm => "确认",
        MessageId::GuideWizardCancel => "取消",
        MessageId::GuideWizardLoginSet => "设为 Cosh 登录 Shell",
        MessageId::GuideWizardLoginRestore => "恢复原登录 Shell",
        MessageId::GuideWizardLoginOther => "其它用户（打开 cosh login）",
        MessageId::GuideWizardLoginVerified => "已验证：{value}",
        MessageId::GuideWizardLoginUnconfirmed => "未确认变更，请重查后再试。",
        MessageId::GuideWizardLoginHandoff => "正在打开登录管理，完成后返回向导。",
        MessageId::GuideWizardLoginSudoNote => "需要 sudo 权限；密码在终端输入，不进入向导。",
        MessageId::GuideWizardConfigUnreadable => "无法读取配置文件。请手动编辑后重新打开向导。",
        MessageId::GuideWizardDataClearConfirm => "确认清除本地推荐数据？开关偏好将保留。",
        MessageId::GuideWizardLoginHandoffFailed => "打开登录管理失败：{error}",
        MessageId::GuideWizardAiStatusConfiguredGeneric => "已配置 AI 接入",
        MessageId::GuideWizardAiStatusUnverified => "连接尚未验证。",
        MessageId::GuideWizardAiStatusAutoReady => "ECS 免配置接入 · 首次提问时验证",
        MessageId::GuideWizardAiStatusAutoPending => "自动接入待确认 · 可直接尝试提问",
        MessageId::GuideWizardAiStatusAutoProbeFailed => "自动接入待确认 · 身份检测失败（{value}）",
        MessageId::GuideWizardAiStatusAutoDisabled => "自动使用 ECS 试用已关闭",
        MessageId::GuideWizardAiStatusNeedsConfig => "需要配置 AI 服务",
        MessageId::GuideWizardAiStatusExplicitIncomplete => "已选择显式接入，但配置未完成",
        MessageId::GuideWizardAiStatusUnknown => "暂时无法确认 AI 接入状态",
        MessageId::GuideWizardAiShortConfigured => "已配置",
        MessageId::GuideWizardAiShortAutoReady => "ECS 自动接入",
        MessageId::GuideWizardAiShortAutoPending => "自动接入待确认",
        MessageId::GuideWizardAiShortAutoDisabled => "自动接入已关闭",
        MessageId::GuideWizardAiShortUnknown => "状态未知",
        MessageId::GuideWizardAiActionAsk => "开始提问",
        MessageId::GuideWizardAiActionCheck => "检测自动接入",
        MessageId::GuideWizardAiActionRetry => "重试",
        MessageId::GuideWizardAiActionEnable => "重新开启自动使用（打开 /auth）",
        MessageId::GuideWizardAiSourceRuntime => "运行时配置",
        _ => return None,
    })
}

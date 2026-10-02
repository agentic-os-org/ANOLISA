/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { Box, Text } from 'ink';
import { IdeIntegrationNudge } from '../IdeIntegrationNudge.js';
import { CommandFormatMigrationNudge } from '../CommandFormatMigrationNudge.js';
import { LoopDetectionConfirmation } from './LoopDetectionConfirmation.js';
import { FolderTrustDialog } from './FolderTrustDialog.js';
import { ShellConfirmationDialog } from './ShellConfirmationDialog.js';
import { ConsentPrompt } from './ConsentPrompt.js';
import { SettingInputPrompt } from './SettingInputPrompt.js';
import { PluginChoicePrompt } from './PluginChoicePrompt.js';
import { HookMultiSelectPrompt } from './HookMultiSelectPrompt.js';
import { ThemeDialog } from './ThemeDialog.js';
import { SettingsDialog } from './SettingsDialog.js';
import { AuthDialog } from '../auth/AuthDialog.js';
import { OpenAIKeyPrompt } from './OpenAIKeyPrompt.js';
import { CustomAgentKeyImportPrompt } from './CustomAgentKeyImportPrompt.js';
import {
  CustomAgentKeySharePrompt,
  type AgentChoice,
} from './CustomAgentKeySharePrompt.js';
import { CustomAgentKeyDetectFailedPrompt } from './CustomAgentKeyDetectFailedPrompt.js';
import { AliyunAuthPrompt } from './AliyunAuthPrompt.js';
import { EditorSettingsDialog } from './EditorSettingsDialog.js';
import { PermissionsModifyTrustDialog } from './PermissionsModifyTrustDialog.js';
import { ModelDialog } from './ModelDialog.js';
import { ApprovalModeDialog } from './ApprovalModeDialog.js';
import { theme } from '../semantic-colors.js';
import { useUIState } from '../contexts/UIStateContext.js';
import { useUIActions } from '../contexts/UIActionsContext.js';
import { useConfig } from '../contexts/ConfigContext.js';
import { useSettings } from '../contexts/SettingsContext.js';
import { AuthState } from '../types.js';
import {
  AuthType,
  decryptCredential,
  type AliyunAuthMethod,
  type STSCredentials,
  type AliyunCredentials,
  ALIYUN_DEFAULT_MODEL,
} from '@copilot-shell/core';
import process from 'node:process';
import { useState, useEffect, useMemo } from 'react';
import { type UseHistoryManagerReturn } from '../hooks/useHistoryManager.js';
import { IdeTrustChangeDialog } from './IdeTrustChangeDialog.js';
import { WelcomeBackDialog } from './WelcomeBackDialog.js';
import { ModelSwitchDialog } from './ModelSwitchDialog.js';
import { AgentCreationWizard } from './subagents/create/AgentCreationWizard.js';
import { AgentsManagerDialog } from './subagents/manage/AgentsManagerDialog.js';
import { SkillsDialog } from './SkillsDialog.js';
import { SessionPicker } from './SessionPicker.js';
import { t } from '../../i18n/index.js';
import {
  readOpenClawConfig,
  readQwenCodeConfig,
  hasOpenClawConfigDir,
  hasQwenCodeConfigDir,
  type AgentKeyConfig,
} from '../../utils/customAgentKeyConfig.js';

interface DialogManagerProps {
  addItem: UseHistoryManagerReturn['addItem'];
  terminalWidth: number;
}

// Props for DialogManager
export const DialogManager = ({
  addItem,
  terminalWidth,
}: DialogManagerProps) => {
  const config = useConfig();
  const settings = useSettings();

  const uiState = useUIState();
  const uiActions = useUIActions();
  const { constrainHeight, terminalHeight, staticExtraHeight, mainAreaWidth } =
    uiState;

  const getDefaultOpenAIConfig = () => {
    const fromSettings = settings.merged.security?.auth;
    const modelSettings = settings.merged.model;
    return {
      apiKey:
        decryptCredential(fromSettings?.apiKey ?? '') ||
        process.env['OPENAI_API_KEY'] ||
        '',
      baseUrl: fromSettings?.baseUrl || process.env['OPENAI_BASE_URL'] || '',
      // 优先使用 openaiModel（按认证方式隔离），避免显示其他认证方式的模型名称
      model:
        fromSettings?.openaiModel ||
        modelSettings?.name ||
        process.env['OPENAI_MODEL'] ||
        '',
    };
  };

  /**
   * Agent Key 共享流程状态机（当前认证流程内有效）:
   *   'idle'        - 流程一：尚未展示 Agent 选择列表
   *   'openclaw'    - 流程二：正在处理 OpenClaw Key 探测
   *   'qwencode'    - 流程二：正在处理 Qwen Code Key 探测
   *   'done'        - 用户选择不需要，进入手动配置
   */
  const [agentShareState, setAgentShareState] = useState<
    'idle' | 'openclaw' | 'qwencode' | 'done'
  >('idle');

  // 记录探测失败的 Agent，返回流程一时过滤掉
  const [failedAgents, setFailedAgents] = useState<
    Set<'openclaw' | 'qwencode'>
  >(new Set());

  // 认证流程结束后重置状态
  useEffect(() => {
    if (!uiState.isAuthenticating) {
      setAgentShareState('idle');
      setFailedAgents(new Set());
    }
  }, [uiState.isAuthenticating]);

  // 在顶层缓存文件探测结果（只在对应状态激活时读取文件，避免每次重渲染触发 IO）
  const openclawConfig = useMemo(
    () => (agentShareState === 'openclaw' ? readOpenClawConfig() : null),
    [agentShareState],
  );
  const qwenCodeConfig = useMemo(
    () => (agentShareState === 'qwencode' ? readQwenCodeConfig() : null),
    [agentShareState],
  );

  // 静默分别探测每个 Agent 的配置目录是否存在，用于过滤 Agent Key Sharing 列表项
  // （只检查目录，不读取 Key）。仅在进入 USE_OPENAI 认证流程时执行一次。
  const hasOpenclawDir = useMemo(
    () =>
      uiState.isAuthenticating &&
      uiState.pendingAuthType === AuthType.USE_OPENAI
        ? hasOpenClawConfigDir()
        : false,
    [uiState.isAuthenticating, uiState.pendingAuthType],
  );
  const hasQwenCodeDir = useMemo(
    () =>
      uiState.isAuthenticating &&
      uiState.pendingAuthType === AuthType.USE_OPENAI
        ? hasQwenCodeConfigDir()
        : false,
    [uiState.isAuthenticating, uiState.pendingAuthType],
  );
  const hasAnyAgentConfigDir = hasOpenclawDir || hasQwenCodeDir;

  // 当 idle 状态下所有"可探测到的" Agent 均已失败时，自动跳转到手动配置
  // （避免在渲染期间 setState）。不可探测到的 Agent 不应计入"全部失败"的判断。
  useEffect(() => {
    if (agentShareState !== 'idle' || !hasAnyAgentConfigDir) return;
    const hasRemaining =
      (hasOpenclawDir && !failedAgents.has('openclaw')) ||
      (hasQwenCodeDir && !failedAgents.has('qwencode'));
    if (!hasRemaining) {
      setAgentShareState('done');
    }
  }, [
    agentShareState,
    failedAgents,
    hasOpenclawDir,
    hasQwenCodeDir,
    hasAnyAgentConfigDir,
  ]);

  if (uiState.showWelcomeBackDialog && uiState.welcomeBackInfo?.hasHistory) {
    return (
      <WelcomeBackDialog
        welcomeBackInfo={uiState.welcomeBackInfo}
        onSelect={uiActions.handleWelcomeBackSelection}
        onClose={uiActions.handleWelcomeBackClose}
      />
    );
  }
  if (uiState.showIdeRestartPrompt) {
    return <IdeTrustChangeDialog reason={uiState.ideTrustRestartReason} />;
  }
  if (uiState.shouldShowIdePrompt) {
    return (
      <IdeIntegrationNudge
        ide={uiState.currentIDE!}
        onComplete={uiActions.handleIdePromptComplete}
      />
    );
  }
  if (uiState.shouldShowCommandMigrationNudge) {
    return (
      <CommandFormatMigrationNudge
        tomlFiles={uiState.commandMigrationTomlFiles}
        onComplete={uiActions.handleCommandMigrationComplete}
      />
    );
  }
  if (uiState.isFolderTrustDialogOpen) {
    return (
      <FolderTrustDialog
        onSelect={uiActions.handleFolderTrustSelect}
        isRestarting={uiState.isRestarting}
      />
    );
  }
  if (uiState.shellConfirmationRequest) {
    return (
      <ShellConfirmationDialog
        request={uiState.shellConfirmationRequest}
        contentWidth={mainAreaWidth}
      />
    );
  }
  if (uiState.loopDetectionConfirmationRequest) {
    return (
      <LoopDetectionConfirmation
        onComplete={uiState.loopDetectionConfirmationRequest.onComplete}
      />
    );
  }
  if (uiState.userPromptConfirmationRequest) {
    const { reason, resolve } = uiState.userPromptConfirmationRequest;
    return (
      <ConsentPrompt
        prompt={`${t('⚠️  **Hook Safety Check**')}\n\n${reason}\n\n${t('Send this prompt or cancel?')}`}
        onConfirm={resolve}
        terminalWidth={terminalWidth}
      />
    );
  }
  if (uiState.sandboxBypassRequest) {
    const { original_command, reason, onComplete } =
      uiState.sandboxBypassRequest;
    return (
      <ConsentPrompt
        prompt={
          `**沙箱执行失败 — 是否允许直接运行？**\n\n` +
          `命令：\`${original_command}\`\n\n` +
          `原因：${reason}\n\n` +
          `确认后将临时禁用沙箱防护，执行完毕后自动恢复。`
        }
        onConfirm={onComplete}
        terminalWidth={terminalWidth}
      />
    );
  }
  if (uiState.confirmationRequest) {
    return (
      <ConsentPrompt
        prompt={uiState.confirmationRequest.prompt}
        onConfirm={uiState.confirmationRequest.onConfirm}
        terminalWidth={terminalWidth}
      />
    );
  }
  if (uiState.hookMultiSelectRequest) {
    return (
      <HookMultiSelectPrompt
        hookNames={uiState.hookMultiSelectRequest.hookNames}
        title={uiState.hookMultiSelectRequest.title}
        onSelect={uiState.hookMultiSelectRequest.onSelect}
        onCancel={uiState.hookMultiSelectRequest.onCancel}
        terminalWidth={terminalWidth}
      />
    );
  }
  if (uiState.confirmUpdateExtensionRequests.length > 0) {
    const request = uiState.confirmUpdateExtensionRequests[0];
    return (
      <ConsentPrompt
        prompt={request.prompt}
        onConfirm={request.onConfirm}
        terminalWidth={terminalWidth}
      />
    );
  }
  if (uiState.settingInputRequests.length > 0) {
    const request = uiState.settingInputRequests[0];
    // Use settingName as key to force re-mount when switching between different settings
    return (
      <SettingInputPrompt
        key={request.settingName}
        settingName={request.settingName}
        settingDescription={request.settingDescription}
        sensitive={request.sensitive}
        onSubmit={request.onSubmit}
        onCancel={request.onCancel}
        terminalWidth={terminalWidth}
      />
    );
  }
  if (uiState.pluginChoiceRequests.length > 0) {
    const request = uiState.pluginChoiceRequests[0];
    return (
      <PluginChoicePrompt
        key={request.marketplaceName}
        marketplaceName={request.marketplaceName}
        plugins={request.plugins}
        onSelect={request.onSelect}
        onCancel={request.onCancel}
        terminalWidth={terminalWidth}
      />
    );
  }
  if (uiState.isThemeDialogOpen) {
    return (
      <Box flexDirection="column">
        {uiState.themeError && (
          <Box marginBottom={1}>
            <Text color={theme.status.error}>{uiState.themeError}</Text>
          </Box>
        )}
        <ThemeDialog
          onSelect={uiActions.handleThemeSelect}
          onHighlight={uiActions.handleThemeHighlight}
          settings={settings}
          availableTerminalHeight={
            constrainHeight ? terminalHeight - staticExtraHeight : undefined
          }
          terminalWidth={mainAreaWidth}
        />
      </Box>
    );
  }
  if (uiState.isEditorDialogOpen) {
    return (
      <Box flexDirection="column">
        {uiState.editorError && (
          <Box marginBottom={1}>
            <Text color={theme.status.error}>{uiState.editorError}</Text>
          </Box>
        )}
        <EditorSettingsDialog
          onSelect={uiActions.handleEditorSelect}
          settings={settings}
          onExit={uiActions.exitEditorDialog}
        />
      </Box>
    );
  }
  if (uiState.isSettingsDialogOpen) {
    return (
      <Box flexDirection="column">
        <SettingsDialog
          settings={settings}
          onSelect={(settingName) => {
            if (settingName === 'ui.theme') {
              uiActions.openThemeDialog();
              return;
            }
            if (settingName === 'general.preferredEditor') {
              uiActions.openEditorDialog();
              return;
            }
            uiActions.closeSettingsDialog();
          }}
          onRestartRequest={() => process.exit(0)}
          availableTerminalHeight={terminalHeight - staticExtraHeight}
          config={config}
        />
      </Box>
    );
  }
  if (uiState.isApprovalModeDialogOpen) {
    const currentMode = config.getApprovalMode();
    return (
      <Box flexDirection="column">
        <ApprovalModeDialog
          settings={settings}
          currentMode={currentMode}
          onSelect={uiActions.handleApprovalModeSelect}
          availableTerminalHeight={
            constrainHeight ? terminalHeight - staticExtraHeight : undefined
          }
        />
      </Box>
    );
  }
  if (uiState.isModelDialogOpen) {
    return <ModelDialog onClose={uiActions.closeModelDialog} />;
  }
  if (uiState.isVisionSwitchDialogOpen) {
    return <ModelSwitchDialog onSelect={uiActions.handleVisionSwitchSelect} />;
  }

  // For OpenAI authentication, show errors in OpenAIKeyPrompt instead of AuthDialog
  // So we check if user is authenticating with OpenAI before rendering AuthDialog
  const isOpenAIAuthenticating =
    uiState.isAuthenticating && uiState.pendingAuthType === AuthType.USE_OPENAI;

  if (
    uiState.isAuthDialogOpen ||
    (uiState.authError && !isOpenAIAuthenticating)
  ) {
    return (
      <Box flexDirection="column">
        <AuthDialog />
      </Box>
    );
  }

  if (uiState.isAuthenticating) {
    if (uiState.pendingAuthType === AuthType.USE_OPENAI) {
      const defaults = getDefaultOpenAIConfig();

      // 已有 apiKey 则跳过 Agent 共享流程直接展示输入页
      // 无 apiKey 且检测到任一 Agent 配置目录存在，才进入流程一
      if (!defaults.apiKey && hasAnyAgentConfigDir) {
        // 流程一：展示 Agent 选择列表
        if (agentShareState === 'idle') {
          // 排除：1) 已探测失败的 Agent；2) 配置目录不存在的 Agent
          const excludedChoices: AgentChoice[] = [...failedAgents];
          if (!hasOpenclawDir) excludedChoices.push('openclaw');
          if (!hasQwenCodeDir) excludedChoices.push('qwencode');
          // 计算剩余可用选项（仅包含配置目录存在且未失败的 Agent）
          const availableAgents = (['openclaw', 'qwencode'] as const).filter(
            (a) =>
              (a === 'openclaw' ? hasOpenclawDir : hasQwenCodeDir) &&
              !failedAgents.has(a),
          );
          // 全部已失败：useEffect 会处理并跳转到 done，此处暂返回 null 等待下一次渲染
          if (availableAgents.length === 0) {
            return null;
          }
          return (
            <CustomAgentKeySharePrompt
              excludedChoices={excludedChoices}
              onSelect={(choice: AgentChoice) => {
                if (choice === 'none') {
                  setAgentShareState('done');
                } else {
                  setAgentShareState(choice);
                }
              }}
              onCancel={() => {
                uiActions.cancelAuthentication();
                uiActions.setAuthState(AuthState.Updating);
              }}
            />
          );
        }

        // 流程二：根据选中的 Agent 探测 Key，成功则自动导入，失败则返回流程一（agentDetectMap 查表）
        const agentDetectMap: Record<
          'openclaw' | 'qwencode',
          { name: string; config: AgentKeyConfig | null }
        > = {
          openclaw: { name: 'OpenClaw', config: openclawConfig },
          qwencode: { name: 'Qwen Code', config: qwenCodeConfig },
        };
        const detecting =
          agentDetectMap[agentShareState as 'openclaw' | 'qwencode'];
        if (detecting) {
          if (detecting.config) {
            return (
              <CustomAgentKeyImportPrompt
                agentKeyConfig={detecting.config}
                agentName={detecting.name}
                onAccept={(cfg) => {
                  uiActions.handleAuthSelect(AuthType.USE_OPENAI, {
                    apiKey: cfg.apiKey,
                    baseUrl: cfg.baseUrl,
                    model: cfg.model,
                  });
                }}
              />
            );
          }
          return (
            <CustomAgentKeyDetectFailedPrompt
              agentName={detecting.name}
              onContinue={() => {
                const agent = agentShareState as 'openclaw' | 'qwencode';
                const nextFailed = new Set(failedAgents).add(agent);
                setFailedAgents(nextFailed);
                // 还有其他可用 Agent 则返回流程一，否则 useEffect 会自动跳到 done
                setAgentShareState('idle');
              }}
            />
          );
        }
      }

      // agentShareState === 'done'，或已有 apiKey：展示 OpenAI Key 输入页
      return (
        <OpenAIKeyPrompt
          onSubmit={(apiKey, baseUrl, model) => {
            // Clear previous auth error before submitting new credentials
            uiActions.onAuthError(null);
            uiActions.handleAuthSelect(AuthType.USE_OPENAI, {
              apiKey,
              baseUrl,
              model,
            });
          }}
          onCancel={() => {
            uiActions.cancelAuthentication();
            uiActions.setAuthState(AuthState.Updating);
          }}
          defaultApiKey={defaults.apiKey}
          defaultBaseUrl={defaults.baseUrl}
          defaultModel={defaults.model}
          authError={uiState.authError}
        />
      );
    }

    // 阿里云认证：使用稳定的 key 避免重新挂载
    // 即使 pendingAuthType 变为 undefined，只要 isAuthenticating 为 true 就保持渲染
    const isAliyunAuth =
      uiState.pendingAuthType === AuthType.USE_ALIYUN ||
      (uiState.isAuthenticating && !uiState.pendingAuthType);

    const handleAliyunAuthSubmit = (
      method: AliyunAuthMethod,
      credentials: STSCredentials | AliyunCredentials,
      model: string,
    ) => {
      // 区分 STS 凭证和 AK/SK 凭证
      if ('securityToken' in credentials) {
        // STS 凭证（ECS RAM Role）
        uiActions.handleAuthSelect(AuthType.USE_ALIYUN, {
          accessKeyId: credentials.accessKeyId,
          accessKeySecret: credentials.accessKeySecret,
          securityToken: credentials.securityToken,
          expiration: credentials.expiration,
          method: method as string,
          model,
        });
      } else {
        // AK/SK 凭证
        uiActions.handleAuthSelect(AuthType.USE_ALIYUN, {
          accessKeyId: credentials.accessKeyId,
          accessKeySecret: credentials.accessKeySecret,
          method: method as string,
          model,
        });
      }
    };

    if (isAliyunAuth) {
      return (
        <AliyunAuthPrompt
          key="aliyun-auth"
          isAuthenticating={uiState.isAuthenticating}
          onSubmit={handleAliyunAuthSubmit}
          onCancel={() => {
            uiActions.cancelAuthentication();
            uiActions.setAuthState(AuthState.Updating);
          }}
          defaultModel={
            settings.merged.security?.auth?.aliyunModel || ALIYUN_DEFAULT_MODEL
          }
        />
      );
    }

    // isAuthenticating 为 true 但 pendingAuthType 不匹配任何已知类型
    // 返回 null 避免渲染其他内容导致闪烁
    return null;
  }
  if (uiState.isPermissionsDialogOpen) {
    return (
      <PermissionsModifyTrustDialog
        onExit={uiActions.closePermissionsDialog}
        addItem={addItem}
      />
    );
  }

  if (uiState.isSubagentCreateDialogOpen) {
    return (
      <AgentCreationWizard
        onClose={uiActions.closeSubagentCreateDialog}
        config={config}
      />
    );
  }

  if (uiState.isAgentsManagerDialogOpen) {
    return (
      <AgentsManagerDialog
        onClose={uiActions.closeAgentsManagerDialog}
        config={config}
      />
    );
  }

  if (uiState.isResumeDialogOpen) {
    return (
      <SessionPicker
        sessionService={config.getSessionService()}
        currentBranch={uiState.branchName}
        onSelect={uiActions.handleResume}
        onCancel={uiActions.closeResumeDialog}
      />
    );
  }

  if (uiState.isSkillsDialogOpen) {
    return (
      <SkillsDialog
        skillsByLevel={uiState.skillsByLevel}
        onToggle={uiActions.toggleSkillDisabled}
        onInvoke={(skillName) => {
          uiActions.closeSkillsDialog();
          uiActions.handleFinalSubmit(`/skills ${skillName}`);
        }}
        onClose={uiActions.closeSkillsDialog}
        isLoading={uiState.isSkillsLoading}
      />
    );
  }

  return null;
};

/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { createContext, useContext } from 'react';
import type {
  HistoryItem,
  ThoughtSummary,
  ConsoleMessageItem,
  ShellConfirmationRequest,
  ConfirmationRequest,
  HookMultiSelectRequest,
  LoopDetectionConfirmationRequest,
  UserPromptConfirmationRequest,
  SandboxBypassRequest,
  HistoryItemWithoutId,
  StreamingState,
  SettingInputRequest,
  PluginChoiceRequest,
  SkillDefinition,
} from '../types.js';
import type { CommandContext, SlashCommand } from '../commands/types.js';
import type { TextBuffer } from '../components/shared/text-buffer.js';
import type {
  AuthType,
  IdeContext,
  ApprovalMode,
  IdeInfo,
  SkillLevel,
} from '@copilot-shell/core';
import type { DOMElement } from 'ink';
import type { SessionStatsState } from '../contexts/SessionContext.js';
import type { ExtensionUpdateState } from '../state/extensions.js';
import type { UpdateObject } from '../utils/updateCheck.js';

import { type UseHistoryManagerReturn } from '../hooks/useHistoryManager.js';
import { type RestartReason } from '../hooks/useIdeTrustListener.js';

export interface UIState {
  history: HistoryItem[];
  historyManager: UseHistoryManagerReturn;
  isThemeDialogOpen: boolean;
  themeError: string | null;
  isAuthenticating: boolean;
  isConfigInitialized: boolean;
  authError: string | null;
  isAuthDialogOpen: boolean;
  showBashOptionInAuthDialog: boolean;
  pendingAuthType: AuthType | undefined;
  editorError: string | null;
  isEditorDialogOpen: boolean;
  debugMessage: string;
  quittingMessages: HistoryItem[] | null;
  isSettingsDialogOpen: boolean;
  isModelDialogOpen: boolean;
  isPermissionsDialogOpen: boolean;
  isApprovalModeDialogOpen: boolean;
  isResumeDialogOpen: boolean;
  slashCommands: readonly SlashCommand[];
  pendingSlashCommandHistoryItems: HistoryItemWithoutId[];
  commandContext: CommandContext;
  shellConfirmationRequest: ShellConfirmationRequest | null;
  confirmationRequest: ConfirmationRequest | null;
  hookMultiSelectRequest: HookMultiSelectRequest | null;
  confirmUpdateExtensionRequests: ConfirmationRequest[];
  settingInputRequests: SettingInputRequest[];
  pluginChoiceRequests: PluginChoiceRequest[];
  loopDetectionConfirmationRequest: LoopDetectionConfirmationRequest | null;
  userPromptConfirmationRequest: UserPromptConfirmationRequest | null;
  sandboxBypassRequest: SandboxBypassRequest | null;
  geminiMdFileCount: number;
  streamingState: StreamingState;
  initError: string | null;
  pendingGeminiHistoryItems: HistoryItemWithoutId[];
  thought: ThoughtSummary | null;
  shellModeActive: boolean;
  reverseSearchActive: boolean;
  commandSearchActive: boolean;
  completionShowSuggestions: boolean;
  shellCompletionShowSuggestions: boolean;
  userMessages: string[];
  buffer: TextBuffer;
  inputWidth: number;
  suggestionsWidth: number;
  isInputActive: boolean;
  shouldShowIdePrompt: boolean;
  shouldShowCommandMigrationNudge: boolean;
  commandMigrationTomlFiles: string[];
  isFolderTrustDialogOpen: boolean;
  isTrustedFolder: boolean | undefined;
  constrainHeight: boolean;
  showErrorDetails: boolean;
  filteredConsoleMessages: ConsoleMessageItem[];
  ideContextState: IdeContext | undefined;
  showToolDescriptions: boolean;
  ctrlCPressedOnce: boolean;
  ctrlDPressedOnce: boolean;
  showEscapePrompt: boolean;
  elapsedTime: number;
  currentLoadingPhrase: string;
  historyRemountKey: number;
  messageQueue: string[];
  showAutoAcceptIndicator: ApprovalMode;
  // Quota-related state
  currentModel: string;
  contextFileNames: string[];
  errorCount: number;
  availableTerminalHeight: number | undefined;
  mainAreaWidth: number;
  staticAreaMaxItemHeight: number;
  staticExtraHeight: number;
  dialogsVisible: boolean;
  pendingHistoryItems: HistoryItemWithoutId[];
  nightly: boolean;
  branchName: string | undefined;
  sessionStats: SessionStatsState;
  terminalWidth: number;
  terminalHeight: number;
  mainControlsRef: React.MutableRefObject<DOMElement | null>;
  currentIDE: IdeInfo | null;
  updateInfo: UpdateObject | null;
  showIdeRestartPrompt: boolean;
  ideTrustRestartReason: RestartReason;
  isRestarting: boolean;
  extensionsUpdateState: Map<string, ExtensionUpdateState>;
  activePtyId: number | undefined;
  embeddedShellFocused: boolean;
  // Vision switch dialog
  isVisionSwitchDialogOpen: boolean;
  // Welcome back dialog
  showWelcomeBackDialog: boolean;
  welcomeBackInfo: {
    hasHistory: boolean;
    lastPrompt?: string;
  } | null;
  welcomeBackChoice: 'continue' | 'restart' | null;
  // Subagent dialogs
  isSubagentCreateDialogOpen: boolean;
  isAgentsManagerDialogOpen: boolean;
  // Skills dialog
  isSkillsDialogOpen: boolean;
  skillsByLevel: Record<SkillLevel, SkillDefinition[]>;
  isSkillsLoading: boolean;
  // Feedback dialog
  isFeedbackDialogOpen: boolean;
}

export const UIStateContext = createContext<UIState | null>(null);

export const useUIState = () => {
  const context = useContext(UIStateContext);
  if (!context) {
    throw new Error('useUIState must be used within a UIStateProvider');
  }
  return context;
};

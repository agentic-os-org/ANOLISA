/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { render } from 'ink-testing-library';
import type React from 'react';
import type { Config } from '@copilot-shell/core';
import { LoadedSettings } from '../config/settings.js';
import { KeypressProvider } from '../ui/contexts/KeypressContext.js';
import { SettingsContext } from '../ui/contexts/SettingsContext.js';
import { ShellFocusContext } from '../ui/contexts/ShellFocusContext.js';
import { ConfigContext } from '../ui/contexts/ConfigContext.js';
import { UIStateContext, type UIState } from '../ui/contexts/UIStateContext.js';
import {
  UIActionsContext,
  type UIActions,
} from '../ui/contexts/UIActionsContext.js';

const mockSettings = new LoadedSettings(
  { path: '', settings: {}, originalSettings: {} },
  { path: '', settings: {}, originalSettings: {} },
  { path: '', settings: {}, originalSettings: {} },
  { path: '', settings: {}, originalSettings: {} },
  true,
  new Set(),
);

// Create minimal mock UIState and UIActions for InputPrompt testing
const createMockUIState = (): UIState => ({
  // ... minimal required fields for InputPrompt
  reverseSearchActive: false,
  commandSearchActive: false,
  completionShowSuggestions: false,
  shellCompletionShowSuggestions: false,
  shellModeActive: false,
  // Add other required fields with default values
  history: [],
  historyManager: {} as never,
  isThemeDialogOpen: false,
  themeError: null,
  isAuthenticating: false,
  isConfigInitialized: true,
  authError: null,
  isAuthDialogOpen: false,
  showBashOptionInAuthDialog: false,
  pendingAuthType: undefined,
  editorError: null,
  isEditorDialogOpen: false,
  debugMessage: '',
  quittingMessages: null,
  isSettingsDialogOpen: false,
  isModelDialogOpen: false,
  isPermissionsDialogOpen: false,
  isApprovalModeDialogOpen: false,
  isResumeDialogOpen: false,
  slashCommands: [],
  pendingSlashCommandHistoryItems: [],
  commandContext: {} as never,
  shellConfirmationRequest: null,
  confirmationRequest: null,
  hookMultiSelectRequest: null,
  confirmUpdateExtensionRequests: [],
  settingInputRequests: [],
  pluginChoiceRequests: [],
  loopDetectionConfirmationRequest: null,
  userPromptConfirmationRequest: null,
  sandboxBypassRequest: null,
  geminiMdFileCount: 0,
  streamingState: 'Idle' as never,
  initError: null,
  pendingGeminiHistoryItems: [],
  thought: null,
  userMessages: [],
  buffer: {} as never,
  inputWidth: 80,
  suggestionsWidth: 80,
  isInputActive: true,
  shouldShowIdePrompt: false,
  shouldShowCommandMigrationNudge: false,
  commandMigrationTomlFiles: [],
  isFolderTrustDialogOpen: false,
  isTrustedFolder: undefined,
  constrainHeight: true,
  showErrorDetails: false,
  filteredConsoleMessages: [],
  ideContextState: undefined,
  showToolDescriptions: false,
  ctrlCPressedOnce: false,
  ctrlDPressedOnce: false,
  showEscapePrompt: false,
  elapsedTime: 0,
  currentLoadingPhrase: '',
  historyRemountKey: 0,
  messageQueue: [],
  showAutoAcceptIndicator: 'auto' as never,
  currentModel: '',
  contextFileNames: [],
  errorCount: 0,
  availableTerminalHeight: undefined,
  mainAreaWidth: 80,
  staticAreaMaxItemHeight: 10,
  staticExtraHeight: 0,
  dialogsVisible: false,
  pendingHistoryItems: [],
  nightly: false,
  branchName: undefined,
  sessionStats: {} as never,
  terminalWidth: 80,
  terminalHeight: 24,
  mainControlsRef: { current: null },
  currentIDE: null,
  updateInfo: null,
  showIdeRestartPrompt: false,
  ideTrustRestartReason: 'trusted' as never,
  isRestarting: false,
  extensionsUpdateState: new Map(),
  activePtyId: undefined,
  embeddedShellFocused: false,
  isVisionSwitchDialogOpen: false,
  showWelcomeBackDialog: false,
  welcomeBackInfo: null,
  welcomeBackChoice: null,
  isSubagentCreateDialogOpen: false,
  isAgentsManagerDialogOpen: false,
  isSkillsDialogOpen: false,
  skillsByLevel: {} as never,
  isSkillsLoading: false,
  isFeedbackDialogOpen: false,
});

const createMockUIActions = (): UIActions => ({
  setReverseSearchActive: () => {},
  setCommandSearchActive: () => {},
  cancelReverseSearch: () => {},
  cancelCommandSearch: () => {},
  resetCompletion: () => {},
  resetShellCompletion: () => {},
  clearInput: () => {},
  registerResetCompletion: () => {},
  registerResetShellCompletion: () => {},
  registerCancelReverseSearch: () => {},
  registerCancelCommandSearch: () => {},
  registerClearInput: () => {},
  setCompletionShowSuggestions: () => {},
  setShellCompletionShowSuggestions: () => {},
  openThemeDialog: () => {},
  openEditorDialog: () => {},
  handleThemeSelect: () => {},
  handleThemeHighlight: () => {},
  handleApprovalModeSelect: () => {},
  handleAuthSelect: async () => {},
  handleContinueToBash: () => {},
  setAuthState: () => {},
  onAuthError: () => {},
  cancelAuthentication: () => {},
  handleEditorSelect: () => {},
  exitEditorDialog: () => {},
  closeSettingsDialog: () => {},
  closeModelDialog: () => {},
  closePermissionsDialog: () => {},
  setShellModeActive: () => {},
  vimHandleInput: () => false,
  handleIdePromptComplete: () => {},
  handleCommandMigrationComplete: () => {},
  handleFolderTrustSelect: () => {},
  setConstrainHeight: () => {},
  refreshStatic: () => {},
  handleFinalSubmit: () => {},
  handleClearScreen: () => {},
  handleVisionSwitchSelect: () => {},
  handleWelcomeBackSelection: () => {},
  handleWelcomeBackClose: () => {},
  closeSubagentCreateDialog: () => {},
  closeAgentsManagerDialog: () => {},
  openSkillsDialog: () => {},
  closeSkillsDialog: () => {},
  toggleSkillDisabled: async () => {},
  openResumeDialog: () => {},
  closeResumeDialog: () => {},
  handleResume: () => {},
  openFeedbackDialog: () => {},
  closeFeedbackDialog: () => {},
  temporaryCloseFeedbackDialog: () => {},
  submitFeedback: () => {},
});

export const renderWithProviders = (
  component: React.ReactElement,
  {
    shellFocus = true,
    settings = mockSettings,
    config = undefined,
    uiState = createMockUIState(),
    uiActions = createMockUIActions(),
    pasteWorkaround = false,
  }: {
    shellFocus?: boolean;
    settings?: LoadedSettings;
    config?: Config;
    uiState?: UIState;
    uiActions?: UIActions;
    pasteWorkaround?: boolean;
  } = {},
): ReturnType<typeof render> =>
  render(
    <SettingsContext.Provider value={settings}>
      <ConfigContext.Provider value={config}>
        <ShellFocusContext.Provider value={shellFocus}>
          <UIStateContext.Provider value={uiState}>
            <UIActionsContext.Provider value={uiActions}>
              <KeypressProvider
                kittyProtocolEnabled={true}
                pasteWorkaround={pasteWorkaround}
              >
                {component}
              </KeypressProvider>
            </UIActionsContext.Provider>
          </UIStateContext.Provider>
        </ShellFocusContext.Provider>
      </ConfigContext.Provider>
    </SettingsContext.Provider>,
  );

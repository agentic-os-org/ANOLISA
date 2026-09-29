/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { createContext, useContext } from 'react';
import { type Key } from '../hooks/useKeypress.js';
import { type IdeIntegrationNudgeResult } from '../IdeIntegrationNudge.js';
import { type CommandMigrationNudgeResult } from '../CommandFormatMigrationNudge.js';
import { type FolderTrustChoice } from '../components/FolderTrustDialog.js';
import {
  type AuthType,
  type EditorType,
  type ApprovalMode,
  type AliyunCredentialsExtended,
  type SkillLevel,
} from '@copilot-shell/core';
import { type SettingScope } from '../../config/settings.js';
import type { AuthState } from '../types.js';
import { type VisionSwitchOutcome } from '../components/ModelSwitchDialog.js';
import { type OpenAICredentials } from '../components/OpenAIKeyPrompt.js';

export interface UIActions {
  openThemeDialog: () => void;
  openEditorDialog: () => void;
  handleThemeSelect: (
    themeName: string | undefined,
    scope: SettingScope,
  ) => void;
  handleThemeHighlight: (themeName: string | undefined) => void;
  handleApprovalModeSelect: (
    mode: ApprovalMode | undefined,
    scope: SettingScope,
  ) => void;
  handleAuthSelect: (
    authType: AuthType | undefined,
    credentials?: OpenAICredentials | AliyunCredentialsExtended,
  ) => Promise<void>;
  handleContinueToBash: () => void;
  setAuthState: (state: AuthState) => void;
  onAuthError: (error: string | null) => void;
  cancelAuthentication: () => void;
  handleEditorSelect: (
    editorType: EditorType | undefined,
    scope: SettingScope,
  ) => void;
  exitEditorDialog: () => void;
  closeSettingsDialog: () => void;
  closeModelDialog: () => void;
  closePermissionsDialog: () => void;
  setShellModeActive: (value: boolean) => void;
  setReverseSearchActive: (value: boolean) => void;
  setCommandSearchActive: (value: boolean) => void;
  cancelReverseSearch: () => void;
  cancelCommandSearch: () => void;
  resetCompletion: () => void;
  resetShellCompletion: () => void;
  clearInput: () => void;
  registerResetCompletion: (resetFn: () => void) => void;
  registerResetShellCompletion: (resetFn: () => void) => void;
  registerCancelReverseSearch: (cancelFn: () => void) => void;
  registerCancelCommandSearch: (cancelFn: () => void) => void;
  registerClearInput: (clearFn: () => void) => void;
  setCompletionShowSuggestions: (value: boolean) => void;
  setShellCompletionShowSuggestions: (value: boolean) => void;
  vimHandleInput: (key: Key) => boolean;
  handleIdePromptComplete: (result: IdeIntegrationNudgeResult) => void;
  handleCommandMigrationComplete: (result: CommandMigrationNudgeResult) => void;
  handleFolderTrustSelect: (choice: FolderTrustChoice) => void;
  setConstrainHeight: (value: boolean) => void;
  refreshStatic: () => void;
  handleFinalSubmit: (value: string) => void;
  handleClearScreen: () => void;
  // Vision switch dialog
  handleVisionSwitchSelect: (outcome: VisionSwitchOutcome) => void;
  // Welcome back dialog
  handleWelcomeBackSelection: (choice: 'continue' | 'restart') => void;
  handleWelcomeBackClose: () => void;
  // Subagent dialogs
  closeSubagentCreateDialog: () => void;
  closeAgentsManagerDialog: () => void;
  // Skills dialog
  openSkillsDialog: () => void;
  closeSkillsDialog: () => void;
  toggleSkillDisabled: (skillName: string, level: SkillLevel) => Promise<void>;
  // Resume session dialog
  openResumeDialog: () => void;
  closeResumeDialog: () => void;
  handleResume: (sessionId: string) => void;
  // Feedback dialog
  openFeedbackDialog: () => void;
  closeFeedbackDialog: () => void;
  temporaryCloseFeedbackDialog: () => void;
  submitFeedback: (rating: number) => void;
}

export const UIActionsContext = createContext<UIActions | null>(null);

export const useUIActions = () => {
  const context = useContext(UIActionsContext);
  if (!context) {
    throw new Error('useUIActions must be used within a UIActionsProvider');
  }
  return context;
};

/**
 * @license
 * Copyright 2025 Qwen Code
 * SPDX-License-Identifier: Apache-2.0
 */

import { useState, useCallback } from 'react';
import {
  SessionService,
  logSessionSummary,
  type Config,
} from '@copilot-shell/core';
import { buildResumedHistoryItems } from '../utils/resumeHistoryUtils.js';
import { getPromptCountFromSessionData } from '../contexts/SessionContext.js';
import type { UseHistoryManagerReturn } from './useHistoryManager.js';

export interface UseResumeCommandOptions {
  config: Config | null;
  historyManager: Pick<UseHistoryManagerReturn, 'clearItems' | 'loadHistory'>;
  startNewSession: (sessionId: string, initialPromptCount?: number) => void;
  remount?: () => void;
}

export interface UseResumeCommandResult {
  isResumeDialogOpen: boolean;
  openResumeDialog: () => void;
  closeResumeDialog: () => void;
  handleResume: (sessionId: string) => void;
}

export function useResumeCommand(
  options?: UseResumeCommandOptions,
): UseResumeCommandResult {
  const [isResumeDialogOpen, setIsResumeDialogOpen] = useState(false);

  const openResumeDialog = useCallback(() => {
    setIsResumeDialogOpen(true);
  }, []);

  const closeResumeDialog = useCallback(() => {
    setIsResumeDialogOpen(false);
  }, []);

  const { config, historyManager, startNewSession, remount } = options ?? {};

  const handleResume = useCallback(
    async (sessionId: string) => {
      if (!config || !historyManager || !startNewSession) {
        return;
      }

      // Close dialog immediately to prevent input capture during async operations.
      closeResumeDialog();

      const cwd = config.getTargetDir();
      const sessionService = new SessionService(cwd);
      const sessionData = await sessionService.loadSession(sessionId);

      if (!sessionData) {
        return;
      }

      // Start new session in UI context, preserving the resumed prompt count so
      // run ids keep increasing for the same session.
      startNewSession(sessionId, getPromptCountFromSessionData(sessionData));

      // Reset UI history.
      const uiHistoryItems = buildResumedHistoryItems(sessionData, config);
      historyManager.clearItems();
      historyManager.loadHistory(uiHistoryItems);

      // Update session history core.
      // Flush current session summary before switching to the resumed session
      logSessionSummary(config);
      config.startNewSession(sessionId, sessionData);
      await config.getGeminiClient()?.initialize?.();

      // Refresh terminal UI.
      remount?.();
    },
    [closeResumeDialog, config, historyManager, startNewSession, remount],
  );

  return {
    isResumeDialogOpen,
    openResumeDialog,
    closeResumeDialog,
    handleResume,
  };
}

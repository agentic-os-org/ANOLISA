/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { Box, Text } from 'ink';
import { theme } from '../semantic-colors.js';
import { ConsoleSummaryDisplay } from './ConsoleSummaryDisplay.js';
import { ContextUsageDisplay } from './ContextUsageDisplay.js';
import { useTerminalSize } from '../hooks/useTerminalSize.js';
import { AutoAcceptIndicator } from './AutoAcceptIndicator.js';
import { ShellModeIndicator } from './ShellModeIndicator.js';
import { isNarrowWidth } from '../utils/isNarrowWidth.js';

import { useStatusLine } from '../hooks/useStatusLine.js';
import { useUIState } from '../contexts/UIStateContext.js';
import { useConfig } from '../contexts/ConfigContext.js';
import { useVimMode } from '../contexts/VimModeContext.js';
import { useCompactMode } from '../contexts/CompactModeContext.js';
import { ApprovalMode } from '@copilot-shell/core';
import { StreamingState } from '../types.js';
import { t } from '../../i18n/index.js';

export const Footer: React.FC = () => {
  const uiState = useUIState();
  const config = useConfig();
  const { vimEnabled, vimMode } = useVimMode();
  const { compactMode } = useCompactMode();
  const { lines: statusLineLines } = useStatusLine();

  const {
    errorCount,
    showErrorDetails,
    promptTokenCount,
    showAutoAcceptIndicator,
  } = {
    errorCount: uiState.errorCount,
    showErrorDetails: uiState.showErrorDetails,
    promptTokenCount: uiState.sessionStats.lastPromptTokenCount,
    showAutoAcceptIndicator: uiState.showAutoAcceptIndicator,
  };

  const showErrorIndicator = !showErrorDetails && errorCount > 0;

  const { columns: terminalWidth } = useTerminalSize();
  const isNarrow = isNarrowWidth(terminalWidth);

  // Check if debug mode is enabled
  const debugMode = config.getDebugMode();

  const contextWindowSize =
    config.getContentGeneratorConfig()?.contextWindowSize;

  // Hide "? for shortcuts" when a custom status line is active (it already
  // occupies the top row, so the hint is redundant). Matches upstream behavior.
  // Also hide when there's text in the input box (original behavior)
  const suppressHint =
    !!statusLineLines || (uiState.buffer?.text?.length ?? 0) > 0;

  // Left bottom row: high-priority messages > approval mode > hint.
  const leftBottomContent = uiState.ctrlCPressedOnce ? (
    <Text color={theme.status.warning}>{t('Press Ctrl+C again to exit.')}</Text>
  ) : uiState.ctrlDPressedOnce ? (
    <Text color={theme.status.warning}>{t('Press Ctrl+D again to exit.')}</Text>
  ) : uiState.showEscapePrompt ? (
    <Text color={theme.text.secondary}>{t('Press Esc again to clear.')}</Text>
  ) : vimEnabled && vimMode === 'INSERT' ? (
    <Text color={theme.text.secondary}>-- INSERT --</Text>
  ) : uiState.shellModeActive ? (
    <ShellModeIndicator />
  ) : showAutoAcceptIndicator !== undefined &&
    showAutoAcceptIndicator !== ApprovalMode.DEFAULT ? (
    <AutoAcceptIndicator approvalMode={showAutoAcceptIndicator} />
  ) : suppressHint ? null : (
    <Text color={theme.text.secondary}>
      {t('Ctrl+C to exit')}
      {' · '}
      {t('/bash for shell')}
      {' · '}
      {t('? for shortcuts')}
      {uiState.streamingState === StreamingState.WaitingForConfirmation &&
        ` · ${t('Press Esc to interrupt')}`}
    </Text>
  );

  const rightItems: Array<{ key: string; node: React.ReactNode }> = [];
  if (debugMode) {
    rightItems.push({
      key: 'debug',
      node: <Text color={theme.status.warning}>Debug Mode</Text>,
    });
  }
  if (promptTokenCount >= 0 && contextWindowSize) {
    rightItems.push({
      key: 'context',
      node: (
        <ContextUsageDisplay
          promptTokenCount={promptTokenCount}
          terminalWidth={terminalWidth}
          contextWindowSize={contextWindowSize}
        />
      ),
    });
  }
  if (showErrorIndicator) {
    rightItems.push({
      key: 'errors',
      node: <ConsoleSummaryDisplay errorCount={errorCount} />,
    });
  }

  // Add mode indicator to right items
  if (vimEnabled) {
    rightItems.push({
      key: 'vim-mode',
      node: (
        <Text color={theme.text.accent}>
          {t(`vim ${vimMode.toLowerCase()}`)}
        </Text>
      ),
    });
  }

  if (uiState.shellModeActive) {
    rightItems.push({
      key: 'shell-mode',
      node: <Text color={theme.text.accent}>{t('shell mode')}</Text>,
    });
  }

  if (
    showAutoAcceptIndicator !== undefined &&
    showAutoAcceptIndicator !== ApprovalMode.DEFAULT
  ) {
    rightItems.push({
      key: 'approval-mode',
      node: (
        <Text color={theme.text.accent}>
          {showAutoAcceptIndicator === ApprovalMode.AUTO_EDIT
            ? t('auto')
            : t('manual')}
        </Text>
      ),
    });
  }

  if (compactMode) {
    rightItems.push({
      key: 'compact',
      node: <Text color={theme.text.accent}>{t('compact')}</Text>,
    });
  } else {
    rightItems.push({
      key: 'verbose',
      node: <Text color={theme.text.accent}>{t('verbose')}</Text>,
    });
  }

  // Layout matches upstream: left column has status line (top) + hints/mode
  // (bottom), right section has indicators. Status line and hints coexist.
  return (
    <Box
      flexDirection={isNarrow ? 'column' : 'row'}
      justifyContent={isNarrow ? 'flex-start' : 'space-between'}
      width="100%"
      paddingX={2}
      gap={isNarrow ? 0 : 1}
    >
      {/* Left column — status line on top, hints/mode on bottom */}
      <Box flexDirection="column" flexShrink={isNarrow ? 0 : 1}>
        {statusLineLines &&
          !uiState.ctrlCPressedOnce &&
          !uiState.ctrlDPressedOnce && (
            <>
              {statusLineLines.map((line, i) => (
                <Text key={`status-line-${i}`} dimColor wrap="truncate">
                  {line}
                </Text>
              ))}
            </>
          )}
        {leftBottomContent && <Box>{leftBottomContent}</Box>}
      </Box>

      {/* Right Section — never compressed */}
      <Box flexShrink={0} gap={1}>
        {rightItems.map(({ key, node }, index) => (
          <Box key={key} alignItems="center">
            {index > 0 && <Text color={theme.text.secondary}> | </Text>}
            {node}
          </Box>
        ))}
      </Box>
    </Box>
  );
};

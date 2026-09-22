/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import { ToolConfirmationOutcome } from '@copilot-shell/core';
import { Box, Text } from 'ink';
import type React from 'react';
import { theme } from '../semantic-colors.js';
import { ExecCommandPreview } from './messages/ExecCommandPreview.js';
import type { RadioSelectItem } from './shared/RadioButtonSelect.js';
import { RadioButtonSelect } from './shared/RadioButtonSelect.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { t } from '../../i18n/index.js';

export interface ShellConfirmationRequest {
  commands: string[];
  onConfirm: (
    outcome: ToolConfirmationOutcome,
    approvedCommands?: string[],
  ) => void;
}

export interface ShellConfirmationDialogProps {
  request: ShellConfirmationRequest;
  /**
   * The available width of the container that holds this dialog.
   * Used to correctly size the command preview boxes so they don't overflow
   * or truncate in narrow terminals. When omitted, a safe default is used.
   */
  contentWidth?: number;
}

// Safe fallback when the container width is not injected by a parent layout.
const DEFAULT_CONTENT_WIDTH = 80;
// marginLeft(1) + border(2) + padding(2) = 5
const DIALOG_OVERHEAD = 5;
const MIN_PREVIEW_WIDTH = 20;

export const ShellConfirmationDialog: React.FC<
  ShellConfirmationDialogProps
> = ({ request, contentWidth }) => {
  const { commands, onConfirm } = request;
  // contentWidth is the available width of the container (e.g. mainAreaWidth)
  // passed from the layout. Fallback to a safe default when not provided.
  const commandPreviewWidth = Math.max(
    (contentWidth ?? DEFAULT_CONTENT_WIDTH) - DIALOG_OVERHEAD,
    MIN_PREVIEW_WIDTH,
  );

  useKeypress(
    (key) => {
      if (key.name === 'escape') {
        onConfirm(ToolConfirmationOutcome.Cancel);
      }
    },
    { isActive: true },
  );

  const handleSelect = (item: ToolConfirmationOutcome) => {
    if (item === ToolConfirmationOutcome.Cancel) {
      onConfirm(item);
    } else {
      // For both ProceedOnce and ProceedAlways, we approve all the
      // commands that were requested.
      onConfirm(item, commands);
    }
  };

  const options: Array<RadioSelectItem<ToolConfirmationOutcome>> = [
    {
      label: t('Yes, allow once'),
      value: ToolConfirmationOutcome.ProceedOnce,
      key: 'Yes, allow once',
    },
    {
      label: t('Yes, allow always for this session'),
      value: ToolConfirmationOutcome.ProceedAlways,
      key: 'Yes, allow always for this session',
    },
    {
      label: t('No (esc)'),
      value: ToolConfirmationOutcome.Cancel,
      key: 'No (esc)',
    },
  ];

  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={theme.status.warning}
      padding={1}
      width="100%"
      marginLeft={1}
    >
      <Box flexDirection="column" marginBottom={1}>
        <Text bold color={theme.text.primary}>
          {t('Shell Command Execution')}
        </Text>
        <Text color={theme.text.primary}>
          {t('A custom command wants to run the following shell commands:')}
        </Text>
        <Box flexDirection="column" marginTop={1}>
          {commands.map((cmd) => (
            <ExecCommandPreview
              key={cmd}
              command={cmd}
              contentWidth={commandPreviewWidth}
            />
          ))}
        </Box>
      </Box>

      <Box marginBottom={1}>
        <Text color={theme.text.primary}>{t('Do you want to proceed?')}</Text>
      </Box>

      <RadioButtonSelect items={options} onSelect={handleSelect} isFocused />
    </Box>
  );
};

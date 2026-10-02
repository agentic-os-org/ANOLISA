/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { Text, Box } from 'ink';
import { theme } from '../../semantic-colors.js';
import { SCREEN_READER_USER_PREFIX } from '../../textConstants.js';
import { isSlashCommand } from '../../utils/commandUtils.js';
import type { SlashCommand } from '../../commands/types.js';

interface UserMessageProps {
  text: string;
  commands?: readonly SlashCommand[];
}

export const UserMessage: React.FC<UserMessageProps> = ({ text, commands }) => {
  const prefix = '> ';
  const prefixWidth = prefix.length;

  const textColor = isSlashCommand(text, commands)
    ? theme.text.accent
    : theme.text.primary;

  return (
    <Box flexDirection="row" paddingY={0} marginY={1} alignSelf="flex-start">
      <Box width={prefixWidth}>
        <Text color={theme.text.accent} aria-label={SCREEN_READER_USER_PREFIX}>
          {prefix}
        </Text>
      </Box>
      <Box flexGrow={1}>
        <Text wrap="wrap" color={textColor}>
          {text}
        </Text>
      </Box>
    </Box>
  );
};

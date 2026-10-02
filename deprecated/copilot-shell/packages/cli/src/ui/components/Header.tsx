/**
 * @license
 * Copyright 2025 Google LLC
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { Box, Text } from 'ink';
import { AuthType, shortenPath, tildeifyPath } from '@copilot-shell/core';
import { theme } from '../semantic-colors.js';
import { getCachedStringWidth } from '../utils/textUtils.js';
import { useTerminalSize } from '../hooks/useTerminalSize.js';

interface HeaderProps {
  customAsciiArt?: string; // For user-defined ASCII art
  version: string;
  authType?: AuthType;
  model: string;
  workingDirectory: string;
}

function titleizeAuthType(value: string): string {
  return value
    .split(/[-_]/g)
    .filter(Boolean)
    .map((part) => {
      if (part.toLowerCase() === 'ai') {
        return 'AI';
      }
      return part.charAt(0).toUpperCase() + part.slice(1);
    })
    .join(' ');
}

// Format auth type for display
function formatAuthType(authType?: AuthType): string {
  if (!authType) {
    return 'Unknown';
  }

  switch (authType) {
    case AuthType.USE_OPENAI:
      return 'OpenAI';
    case AuthType.USE_GEMINI:
      return 'Gemini';
    case AuthType.USE_VERTEX_AI:
      return 'Vertex AI';
    case AuthType.USE_ANTHROPIC:
      return 'Anthropic';
    case AuthType.USE_ALIYUN:
      return 'Aliyun Authentication';
    default:
      return titleizeAuthType(String(authType));
  }
}

export const Header: React.FC<HeaderProps> = ({
  version,
  authType,
  model,
  workingDirectory,
}) => {
  const { columns: terminalWidth } = useTerminalSize();

  const formattedAuthType = formatAuthType(authType);

  const containerMarginX = 2; // marginLeft + marginRight on the outer container
  const infoPanelPaddingX = 1;
  const infoPanelBorderWidth = 2; // left + right border
  const infoPanelChromeWidth = infoPanelBorderWidth + infoPanelPaddingX * 2;

  const availableTerminalWidth = Math.max(
    0,
    terminalWidth - containerMarginX * 2,
  );

  // Calculate max path length (subtract padding/borders from available space)
  const maxPathLength = Math.max(
    0,
    availableTerminalWidth - infoPanelChromeWidth,
  );

  const infoPanelContentWidth = Math.max(
    0,
    availableTerminalWidth - infoPanelChromeWidth,
  );
  const authModelText = `${formattedAuthType} | ${model}`;
  const authHintText = ' (/auth to change)';
  const showAuthHint =
    infoPanelContentWidth > 0 &&
    getCachedStringWidth(authModelText + authHintText) <= infoPanelContentWidth;

  // Now shorten the path to fit the available space
  const tildeifiedPath = tildeifyPath(workingDirectory);
  const shortenedPath = shortenPath(tildeifiedPath, Math.max(3, maxPathLength));
  const displayPath =
    maxPathLength <= 0
      ? ''
      : shortenedPath.length > maxPathLength
        ? shortenedPath.slice(0, maxPathLength)
        : shortenedPath;

  return (
    <Box
      flexDirection="row"
      alignItems="center"
      marginX={containerMarginX}
      width={availableTerminalWidth}
    >
      {/* Info panel */}
      <Box
        flexDirection="column"
        borderStyle="round"
        borderColor={theme.border.default}
        paddingX={infoPanelPaddingX}
        flexGrow={1}
      >
        {/* Title line: >_ Copilot Shell (v{version}) */}
        <Text>
          <Text bold color={theme.text.accent}>
            &gt;_ Copilot Shell
          </Text>
          <Text color={theme.text.secondary}> (v{version})</Text>
        </Text>
        {/* Empty line for spacing */}
        <Text> </Text>
        {/* Auth and Model line */}
        <Text>
          <Text color={theme.text.secondary}>{authModelText}</Text>
          {showAuthHint && (
            <Text color={theme.text.secondary}>{authHintText}</Text>
          )}
        </Text>
        {/* Directory line */}
        <Text color={theme.text.secondary}>{displayPath}</Text>
      </Box>
    </Box>
  );
};

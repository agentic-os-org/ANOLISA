/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useEffect, useCallback } from 'react';
import { Box, Text } from 'ink';
import { Colors } from '../colors.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { t } from '../../i18n/index.js';
import { AGENT_KEY_AUTO_REDIRECT_MS } from '../../utils/customAgentKeyConfig.js';

interface CustomAgentKeyDetectFailedPromptProps {
  /** 探测失败的 Agent 名称，用于提示文案 */
  agentName: string;
  /** Called when the user presses Enter or after the auto-redirect timeout. */
  onContinue: () => void;
}

/**
 * 流程二（失败）：用户选择了某个 Agent，但未检测到其 Key。
 * 2s 后或用户按 Enter 后自动返回流程一。
 */
export function CustomAgentKeyDetectFailedPrompt({
  agentName,
  onContinue,
}: CustomAgentKeyDetectFailedPromptProps): React.JSX.Element {
  useEffect(() => {
    const timer = setTimeout(onContinue, AGENT_KEY_AUTO_REDIRECT_MS);
    return () => clearTimeout(timer);
  }, [onContinue]);

  const handleKeypress = useCallback(
    (key: { name?: string }) => {
      if (key.name === 'return') {
        onContinue();
      }
    },
    [onContinue],
  );

  useKeypress(handleKeypress, { isActive: true });

  return (
    <Box
      borderStyle="round"
      borderColor={Colors.Gray}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      <Text bold color={Colors.Gray}>
        {t('Agent Key Sharing')}
      </Text>
      <Box marginTop={1}>
        <Text color={Colors.AccentRed}>
          {t('No API Key found for {{agentName}}.', { agentName })}
        </Text>
      </Box>
      <Box marginTop={1} flexDirection="column">
        <Text color={Colors.Gray}>{t('Returning to agent selection...')}</Text>
        <Text color={Colors.Gray} dimColor>
          {t('Press Enter or wait 2s to continue')}
        </Text>
      </Box>
    </Box>
  );
}

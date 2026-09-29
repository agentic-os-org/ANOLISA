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
import type { AgentKeyConfig } from '../../utils/customAgentKeyConfig.js';
import { AGENT_KEY_AUTO_REDIRECT_MS } from '../../utils/customAgentKeyConfig.js';

interface CustomAgentKeyImportPromptProps {
  agentKeyConfig: AgentKeyConfig;
  /** Agent 名称，用于展示标题，如 "OpenClaw" 或 "Qwen Code" */
  agentName: string;
  onAccept: (config: AgentKeyConfig) => void;
}

function maskApiKey(key: string): string {
  if (key.length <= 3) return '*'.repeat(key.length);
  return key.slice(0, 3) + '*'.repeat(Math.min(key.length - 3, 20));
}

export function CustomAgentKeyImportPrompt({
  agentKeyConfig,
  agentName,
  onAccept,
}: CustomAgentKeyImportPromptProps): React.JSX.Element {
  // 2s 后自动完成认证
  useEffect(() => {
    const timer = setTimeout(
      () => onAccept(agentKeyConfig),
      AGENT_KEY_AUTO_REDIRECT_MS,
    );
    return () => clearTimeout(timer);
  }, [agentKeyConfig, onAccept]);

  const handleKeypress = useCallback(
    (key: { name?: string }) => {
      if (key.name === 'return') {
        onAccept(agentKeyConfig);
      }
    },
    [agentKeyConfig, onAccept],
  );

  useKeypress(handleKeypress, { isActive: true });

  const title = t('{{agentName}} configuration detected', { agentName });
  const subtitle = t(
    'The following configuration from {{agentName}} will be imported',
    { agentName },
  );

  return (
    <Box
      borderStyle="round"
      borderColor={Colors.AccentBlue}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      <Text bold color={Colors.AccentBlue}>
        {title}
      </Text>
      <Box marginTop={1}>
        <Text>{subtitle}:</Text>
      </Box>
      <Box marginTop={1} flexDirection="column" paddingLeft={2}>
        <Box flexDirection="row">
          <Box width={16}>
            <Text color={Colors.Gray}>{t('Provider:')}</Text>
          </Box>
          <Text>{agentKeyConfig.providerName}</Text>
        </Box>
        <Box flexDirection="row">
          <Box width={16}>
            <Text color={Colors.Gray}>{t('API Key:')}</Text>
          </Box>
          <Text>{maskApiKey(agentKeyConfig.apiKey)}</Text>
        </Box>
        <Box flexDirection="row">
          <Box width={16}>
            <Text color={Colors.Gray}>{t('Base URL:')}</Text>
          </Box>
          <Text>{agentKeyConfig.baseUrl}</Text>
        </Box>
        {agentKeyConfig.model ? (
          <Box flexDirection="row">
            <Box width={16}>
              <Text color={Colors.Gray}>{t('Model:')}</Text>
            </Box>
            <Text>{agentKeyConfig.model}</Text>
          </Box>
        ) : null}
      </Box>
      <Box marginTop={1}>
        <Text color={Colors.Gray}>
          {t('Press Enter or wait 2s to complete authentication')}
        </Text>
      </Box>
    </Box>
  );
}

/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { useCallback } from 'react';
import { Box, Text } from 'ink';
import { Colors } from '../colors.js';
import { RadioButtonSelect } from './shared/RadioButtonSelect.js';
import { useKeypress } from '../hooks/useKeypress.js';
import { t } from '../../i18n/index.js';

export type AgentChoice = 'openclaw' | 'qwencode' | 'none';

interface CustomAgentKeySharePromptProps {
  /** Called when the user confirms a choice. */
  onSelect: (choice: AgentChoice) => void;
  /** Called when the user presses ESC or Ctrl+C to cancel. */
  onCancel: () => void;
  /** Choices to exclude from the list (e.g. already-failed agents). */
  excludedChoices?: AgentChoice[];
}

const ALL_AGENT_ITEMS: Array<{
  key: string;
  value: AgentChoice;
  label: string;
}> = [
  { key: 'openclaw', value: 'openclaw', label: 'OpenClaw' },
  { key: 'qwencode', value: 'qwencode', label: 'Qwen Code' },
  { key: 'none', value: 'none', label: t('No, configure manually') },
];

/**
 * 流程一：询问用户是否将已安装 Agent 的 Key 共享给 cosh。
 * 支持：
 *   - 上下方向键导航 + 回车确认
 *   - 数字 1/2/3 快速选择
 *   - ESC / Ctrl+C 取消，返回 AuthDialog
 */
export function CustomAgentKeySharePrompt({
  onSelect,
  onCancel,
  excludedChoices = [],
}: CustomAgentKeySharePromptProps): React.JSX.Element {
  const AGENT_ITEMS = ALL_AGENT_ITEMS.filter(
    (item) => !excludedChoices.includes(item.value),
  );
  // 只处理 ESC / Ctrl+C；数字选择和方向键由 RadioButtonSelect 内部的 useSelectionList 处理
  const handleKeypress = useCallback(
    (key: { name?: string; ctrl?: boolean }) => {
      if (key.name === 'escape' || (key.ctrl && key.name === 'c')) {
        onCancel();
      }
    },
    [onCancel],
  );

  useKeypress(handleKeypress, { isActive: true });

  return (
    <Box
      borderStyle="round"
      borderColor={Colors.AccentBlue}
      flexDirection="column"
      padding={1}
      width="100%"
    >
      <Text bold color={Colors.AccentBlue}>
        {t('Agent Key Sharing')}
      </Text>
      <Box marginTop={1}>
        <Text>
          {t(
            'Authorize importing API keys from the configuration file of installed agents for automatic configuration? Choose an Agent or manually configure it.',
          )}
        </Text>
      </Box>
      <Box marginTop={1}>
        <RadioButtonSelect<AgentChoice>
          items={AGENT_ITEMS}
          showNumbers={true}
          onSelect={onSelect}
          key={excludedChoices.join(',')}
        />
      </Box>
      <Box marginTop={1}>
        <Text color={Colors.Gray}>
          {t(
            '↑↓ or j/k to navigate · 1/2/3 select · Enter confirm · Esc cancel',
          )}
        </Text>
      </Box>
    </Box>
  );
}

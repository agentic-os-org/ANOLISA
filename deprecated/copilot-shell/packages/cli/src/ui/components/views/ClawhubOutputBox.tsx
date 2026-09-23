/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import type React from 'react';
import { Box, Text } from 'ink';
import { theme } from '../../semantic-colors.js';
import type { ClawhubResultItem } from '../../types.js';

interface ClawhubOutputBoxProps {
  title: string;
  items?: ClawhubResultItem[];
  text?: string;
  isError?: boolean;
  width?: number;
}

export const ClawhubOutputBox: React.FC<ClawhubOutputBoxProps> = ({
  title,
  items,
  text,
  isError,
  width,
}) => (
  <Box
    borderStyle="round"
    borderColor={isError ? theme.status.error : theme.border.default}
    flexDirection="column"
    padding={1}
    width={width}
  >
    <Box marginBottom={1}>
      <Text bold color={isError ? theme.status.error : theme.text.accent}>
        {title}
      </Text>
    </Box>

    {items && items.length > 0 && (
      <Box flexDirection="column">
        {items.map((item, idx) => (
          <Box key={idx} flexDirection="row">
            <Text bold color={theme.text.link}>
              {item.slug}
            </Text>
            {item.description && (
              <Text color={theme.text.primary}>
                {'  '}
                {item.description}
              </Text>
            )}
            {item.score && (
              <Text color={theme.status.warning}>
                {'  '}({item.score})
              </Text>
            )}
          </Box>
        ))}
      </Box>
    )}

    {text && (
      <Box>
        <Text
          wrap="wrap"
          color={isError ? theme.status.error : theme.text.primary}
        >
          {text}
        </Text>
      </Box>
    )}

    {!items?.length && !text && (
      <Text color={theme.text.primary}>No results.</Text>
    )}
  </Box>
);

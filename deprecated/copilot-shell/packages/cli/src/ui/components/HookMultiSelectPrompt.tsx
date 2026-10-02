/**
 * @license
 * Copyright 2026 Alibaba Cloud
 * SPDX-License-Identifier: Apache-2.0
 */

import { Box, Text } from 'ink';
import { useState, useCallback, useMemo } from 'react';
import { theme } from '../semantic-colors.js';
import { t } from '../../i18n/index.js';
import { useKeypress, type Key } from '../hooks/useKeypress.js';

type HookMultiSelectPromptProps = {
  hookNames: string[];
  title: string;
  onSelect: (selectedNames: string[]) => void;
  onCancel: () => void;
  terminalWidth: number;
};

const MAX_VISIBLE_ITEMS = 10;

export const HookMultiSelectPrompt = (props: HookMultiSelectPromptProps) => {
  const { hookNames, title, onSelect, onCancel } = props;

  const [cursorIndex, setCursorIndex] = useState(0);
  const [selected, setSelected] = useState<Set<string>>(new Set());

  const handleKeypress = useCallback(
    (key: Key) => {
      const { name, sequence } = key;

      if (name === 'escape') {
        onCancel();
        return;
      }

      if (name === 'return') {
        if (selected.size === 0) return;
        onSelect([...selected]);
        return;
      }

      // Toggle selection
      if (sequence === ' ') {
        const hookName = hookNames[cursorIndex];
        if (hookName) {
          setSelected((prev) => {
            const next = new Set(prev);
            if (next.has(hookName)) {
              next.delete(hookName);
            } else {
              next.add(hookName);
            }
            return next;
          });
        }
        return;
      }

      // Select / deselect all
      if (sequence === 'a') {
        setSelected((prev) =>
          prev.size === hookNames.length ? new Set() : new Set(hookNames),
        );
        return;
      }

      if (name === 'up' || sequence === 'k') {
        setCursorIndex((prev) => (prev > 0 ? prev - 1 : hookNames.length - 1));
        return;
      }

      if (name === 'down' || sequence === 'j') {
        setCursorIndex((prev) => (prev < hookNames.length - 1 ? prev + 1 : 0));
        return;
      }
    },
    [hookNames, cursorIndex, selected, onSelect, onCancel],
  );

  useKeypress(handleKeypress, { isActive: true });

  const { visibleHooks, startIndex, hasMore, hasLess } = useMemo(() => {
    const total = hookNames.length;
    if (total <= MAX_VISIBLE_ITEMS) {
      return {
        visibleHooks: hookNames,
        startIndex: 0,
        hasMore: false,
        hasLess: false,
      };
    }

    let start = 0;
    const halfWindow = Math.floor(MAX_VISIBLE_ITEMS / 2);

    if (cursorIndex <= halfWindow) {
      start = 0;
    } else if (cursorIndex >= total - halfWindow) {
      start = total - MAX_VISIBLE_ITEMS;
    } else {
      start = cursorIndex - halfWindow;
    }

    const end = Math.min(start + MAX_VISIBLE_ITEMS, total);

    return {
      visibleHooks: hookNames.slice(start, end),
      startIndex: start,
      hasLess: start > 0,
      hasMore: end < total,
    };
  }, [hookNames, cursorIndex]);

  return (
    <Box
      borderStyle="round"
      borderColor={theme.border.default}
      flexDirection="column"
      paddingY={1}
      paddingX={2}
      width="100%"
    >
      <Text bold color={theme.text.accent}>
        {title}
      </Text>

      <Box marginTop={1} flexDirection="column">
        {hasLess && (
          <Box>
            <Text dimColor>
              {'  '}↑ {t('{{count}} more above', { count: String(startIndex) })}
            </Text>
          </Box>
        )}

        {visibleHooks.map((hookName, visibleIndex) => {
          const actualIndex = startIndex + visibleIndex;
          const isCursor = actualIndex === cursorIndex;
          const isSelected = selected.has(hookName);
          const checkbox = isSelected ? '[✓]' : '[ ]';
          const prefix = isCursor ? '❯ ' : '  ';

          return (
            <Box key={hookName} flexDirection="row">
              <Text color={isCursor ? theme.text.accent : undefined}>
                {prefix}
              </Text>
              <Text
                color={
                  isSelected
                    ? theme.text.accent
                    : isCursor
                      ? theme.text.accent
                      : undefined
                }
                bold={isCursor}
              >
                {checkbox} {hookName}
              </Text>
            </Box>
          );
        })}

        {hasMore && (
          <Box>
            <Text dimColor>
              {'  '}↓{' '}
              {t('{{count}} more below', {
                count: String(
                  hookNames.length - startIndex - MAX_VISIBLE_ITEMS,
                ),
              })}
            </Text>
          </Box>
        )}
      </Box>

      <Box marginTop={1} flexDirection="column">
        <Text dimColor>
          {t(
            '↑↓/jk navigate, Space toggle, a select all, Enter confirm, Esc cancel',
          )}
        </Text>
        {selected.size > 0 && (
          <Text color={theme.text.accent}>
            {t('{{count}} selected', { count: String(selected.size) })}
          </Text>
        )}
      </Box>
    </Box>
  );
};
